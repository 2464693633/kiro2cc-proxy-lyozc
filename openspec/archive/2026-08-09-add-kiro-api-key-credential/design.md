# 设计：add-kiro-api-key-credential

## 架构决策

### 决策 1：作为凭据类型而非独立认证通道

**选择**：把 API Key 建模为 `KiroCredentials` 的一种形态，与 social / idc / external_idp 并列，共用账号池、优先级、故障转移、多端点 LB、sticky 路由、用量记账。

**替代方案**：单独一条绕过 `MultiTokenManager` 的调用路径。

**理由**：API Key 与 OAuth 凭据的差异只在"如何得到 Bearer Token"这一步——之后的请求构造、重试、端点选择、限流完全相同。做成独立通道会让这些能力全部需要二次实现。代价是 `KiroCredentials` 多一个大部分账号用不到的字段，可接受。

### 决策 2：`is_api_key_credential()` 对两种残缺输入都返回 true

判定为 `kiro_api_key.is_some() || authMethod == api_key`，是**或**而非且。两个方向各有用途：

- **只填 `kiroApiKey`、不写 `authMethod`** → 仍被识别，用户少填一个字段
- **只写 `authMethod`、缺 `kiroApiKey`** → 仍被识别，于是启动校验能捕获它并明确报"缺 kiroApiKey"

若用**且**，第二种情况会被当成 OAuth 凭据，报错变成"缺 refreshToken"——对着一个明确写了 `api_key` 的配置说缺 refreshToken，会把用户带偏。

### 决策 3：`token_type_header()` 中 API Key 优先于 external_idp

两者同时命中在正常配置下不该出现，但一旦出现（如用户在 external_idp 账号上误填 `kiroApiKey`），`try_ensure_token` 的短路分支在前，实际发出的 Bearer Token **一定**来自 `kiroApiKey`。头必须与 token 的实际来源一致，否则上游按 EXTERNAL_IDP 规则校验一个 API Key，得到的错误信息会完全指错方向。

即：这个优先级不是偏好，是被短路顺序决定的。

### 决策 4：`refresh_token()` 加拒绝守卫

`try_ensure_token` 已经短路，守卫看起来冗余。但它防的是一类具体的静默故障：

`refresh_token()` 的分发逻辑是 `if idc {...} else if external_idp {...} else { social }`。`api_key` 不匹配前两个，**落到 `else`**，于是走 social 刷新——把 `kiroApiKey` 当作 `refreshToken` POST 给 OIDC 端点。这不是报错，是把凭据泄露给一个不该收到它的端点，且错误信息会是"刷新失败"，完全指不到真因。

守卫把它变成一句明确的错误。

### 决策 5：machineId 两条派生路径互斥

优先级链（账号级 → 全局 → 派生）保持不变，只把末段拆开。关键是**不允许回落**：

若 API Key 凭据在 `kiroApiKey` 缺失时回落到 `refreshToken` 派生，那么"配置残缺时"和"配置修好后"会得到两个不同的 machineId。machineId 是设备指纹，上游可能因此判定为换设备。返回 `None` 让调用方走既有的缺失处理，比给出一个会漂移的值安全。

### 决策 6：新增 `DisabledReason::InvalidConfig` 而非复用现有变体

复用 `Manual` 会让"用户主动禁用"和"配置写错"无法区分；复用 `TooManyFailures` 会把它拖进自愈循环——而配置错误自愈不了，每轮自愈都会重新启用它、再失败、再累计，白烧重试预算。

新变体需要满足三个约束，均已核对：

| 约束 | 位置 | 现状 |
|---|---|---|
| 不参与自愈 | 自愈逻辑 | 只匹配 `TooManyFailures`，自动跳过 |
| 不写入 `credentials.json` | `persist_credentials` | 只把 `Manual` 同步到配置文件 |
| 不写入 `stats.json` | stats 分类过滤 | 只保留 `QuotaExceeded`/`TooManyFailures` |
| **不被 stats 覆盖** | `load_stats()` 守卫 | **需要改** —— 原本只保护 `Manual` |

最后一条是唯一需要动的：不改的话，一个此前因额度耗尽被禁的账号，这次因缺 key 被判 `InvalidConfig`，随后 `load_stats()` 会把原因覆盖回 `QuotaExceeded`，面板上显示"额度用尽"而真因是配置写错。

前三条的天然满足恰好就是想要的语义：配置态不持久化，每次启动按当前配置重新推导，用户改对配置重启即恢复。

### 决策 7：`add_credential` 对 API Key 跳过验证性刷新

OAuth 凭据在添加时会先刷新一次，用来尽早发现无效 token。API Key 没有等价的轻量探测手段——`get_usage_limits_for` 走的是 OAuth 专用端点，是否接受 API Key 未实测。

选择直接接受配置，有效性由首次真实调用判定。这让 `add` 不发任何网络请求（也使该路径可单测）。代价是无效 key 要到第一次对话才暴露，已在提案中文档化。

### 决策 8：`update_credential` 的 revalidation 条件收窄

原条件是 `update.refresh_token.is_some()`。若同时更新 `refreshToken` 与 `kiroApiKey`，或对已是 API Key 的账号更新 `refreshToken`，就会对 API Key 凭据调 `refresh_token()`，撞上决策 4 的守卫直接报错。

新条件在"更新后仍是 OAuth 凭据"时才触发，需要同时看三件事：本次是否设了非空 key（`sets_api_key`）、本次是否清空了 key（`clears_api_key`）、账号原本是否是 API Key（`existing_is_api_key`）。清空是有意义的操作——它把 API Key 凭据变回 OAuth 凭据，此时应当重新验证。

## 数据流

```
credentials.json / Admin API
  │ serde → KiroCredentials { kiro_api_key: Some(...) }
  ▼
MultiTokenManager::new
  │ 启动校验：声明 api_key 但缺 key → disabled + InvalidConfig
  │ load_stats()：InvalidConfig 与 Manual 同级，不被覆盖
  ▼
acquire_context → select_next_credential → try_ensure_token
  │ ★ is_api_key_credential() → 短路，token = kiro_api_key
  │   （OAuth 路径：is_token_expired → refresh_token → 回写文件）
  ▼
CallContext { id, token, credentials }
  ▼
provider.build_headers / build_mcp_headers
  │ Authorization: Bearer <token>
  │ ★ TokenType: token_type_header()  → API_KEY
  │ machine_id: sha256("KiroAPIKey/<key>")
  ▼
上游 Kiro API
```

标 ★ 的两处是本变更的核心；其余全部复用。

## 测试策略

17 个单测，分布在 4 个文件的既有 `#[cfg(test)] mod tests` 内（仓库惯例是就近同文件测试）。

| 文件 | 数量 | 覆盖 |
|---|---|---|
| `credentials.rs` | 5 | 双路识别、别名规范化、`token_type_header` 四态 + 同时命中、序列化往返 + 未设置不序列化、Debug 脱敏 |
| `token_manager.rs` | 7 | 校验放行（含空白串）、refresh 拒绝、短路返回、启动禁用 + 完整配置不禁用、`describe()` 可区分、按 key 去重、add 不发网络请求 |
| `provider.rs` | 3 | API Key 注入 `API_KEY` + Bearer 为 key、social 不带头、MCP 路径注入 |
| `machine_id.rs` | 2 | 独立种子 + 稳定性、互斥不回落 |

三个针对最高风险决策的回归测试：`test_refresh_token_refuses_api_key_credential`（决策 4）、`test_api_key_credential_does_not_fall_back_to_refresh_token`（决策 5）、`test_build_mcp_headers_injects_token_type`（预存在缺陷）。

`add_credential` 的两个测试能跑在 CI 里不发网络请求，正是决策 7 的副产物。
