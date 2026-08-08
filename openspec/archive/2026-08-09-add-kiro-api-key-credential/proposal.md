# 变更提案：add-kiro-api-key-credential

## 背景

当前代理只支持 OAuth 类凭据（social / idc / builder-id / iam / external_idp），全部依赖 `refreshToken` 走刷新流程换取 access token。Kiro 另有一种 **API Key** 凭据形式（`ksk_` 前缀），本身不过期、无需刷新，直接作为 Bearer Token 使用。

把带 `kiroApiKey` 的 `credentials.json` 丢给本代理，serde 会静默忽略该字段，账号因缺 `refreshToken` 无法刷新，直接不可用。参考实现 `ZyphrZero/kiro.rs` 已支持该凭据类型。

## 目标范围

**在范围内：**
- `KiroCredentials` 新增 `kiroApiKey: Option<String>` + 三个判定 helper
- `authMethod: "api_key"`（含 `apikey` 别名）纳入规范化
- token 获取短路：API Key 凭据直接用 `kiroApiKey` 作 Bearer Token，不进刷新流程
- 请求头：携带 `TokenType: API_KEY`
- machineId 派生：改用 `sha256("KiroAPIKey/<key>")` 种子，与 refreshToken 路径**互斥不回落**
- 启动校验：声明 `api_key` 但缺 key → 自动禁用，新增 `DisabledReason::InvalidConfig`
- Admin API：`refreshToken` 改为可选、新增 `kiroApiKey` 字段、去重与验证按凭据类型分流
- 顺带修一处预存在缺陷：`build_mcp_headers` 从未设置 `TokenType`

**不在范围内：**
- `KIRO_API_KEY` 环境变量自动建凭据（kiro.rs 有；本项目 Admin 面板已可添加，且环境变量注入凭据与现有"凭据只从 credentials.json 加载"的模型冲突）
- admin-ui 前端表单字段（后端 API 已就绪，前端可后续单独加；当前可通过 API 或手写 `credentials.json` 接入）
- API Key 的余额/额度查询（`get_usage_limits_for` 走的是 OAuth 专用端点，API Key 是否支持未实测）
- API Key 有效性的启动期探测（无刷新流程可复用，有效性由首次真实调用判定）

## 技术方案

### 1. 凭据模型（`credentials.rs`）

```rust
pub kiro_api_key: Option<String>,   // camelCase, skip_serializing_if

pub fn is_api_key_credential(&self) -> bool   // 带 key 字段 或 authMethod=api_key
pub fn is_external_idp_credential(&self) -> bool
pub fn token_type_header(&self) -> Option<&'static str>  // API_KEY / EXTERNAL_IDP / None
```

`is_api_key_credential` 对"只填 `kiroApiKey` 没写 `authMethod`"和"只写 `authMethod` 没填 key"都返回 true：前者让用户少填一个字段，后者让启动校验能捕获配置残缺。

`token_type_header` 中 **API Key 判定优先于 external_idp**：两者同时命中时实际发出的 Bearer Token 来自 `kiroApiKey`，头必须与之匹配。

### 2. Token 获取（`token_manager.rs`）

`try_ensure_token` 开头短路，在任何过期判断之前返回：

```rust
if credentials.is_api_key_credential() {
    let api_key = /* 非空校验 */;
    return Ok(CallContext { id, token: api_key.to_string(), credentials: credentials.clone() });
}
```

同时给 `refresh_token()` 加拒绝守卫。这不是防御性冗余：若某路径把 API Key 凭据误当 OAuth 凭据，`auth_method` 落到 `else` 分支会走 social 刷新，**把 `kiroApiKey` 当 refreshToken 发给上游**。明确报错优于静默泄露。

`validate_refresh_token` 对 API Key 凭据改校验 `kiroApiKey` 非空后放行（沿用 `external_idp` 已有的特例先例）。

### 3. 请求头（`provider.rs`）

`build_headers` 里原本硬编码的 external_idp 分支换成 `token_type_header()`。

`build_mcp_headers` **补上同一段** —— 它此前从未设置 `TokenType`，这是预存在缺陷：external_idp 凭据的 MCP（WebSearch）调用同样受影响，上游会按 social 凭据校验该 Bearer Token 而失败。

### 4. machineId（`machine_id.rs`）

优先级链不变（账号级 → 全局 → 派生），只把末段的派生拆成互斥两路：

```
is_api_key_credential() → sha256("KiroAPIKey/<kiroApiKey>")
否则                    → sha256("KotlinNativeAPI/<refreshToken>")
```

**不允许互相回落**：若 API Key 凭据在 key 缺失时回落到 refreshToken 派生，同一账号在配置修好前后会得到不同 machineId，上游可能视为换设备。

### 5. 启动校验（`token_manager.rs::new`）

声明 `api_key` 但 key 缺失/空白 → `disabled = true` + `DisabledReason::InvalidConfig`，避免每次请求都到 `try_ensure_token` 失败一遍。

`InvalidConfig` 的三个行为约束：
- **不参与自愈** —— 自愈只处理 `TooManyFailures`，配置错误自愈不了
- **不写入 stats 持久化** —— 每次启动按配置重新推导，改对配置后自然恢复
- **不被 `load_stats()` 覆盖** —— 与 `Manual` 同级保护，否则真因会被历史的 `QuotaExceeded` 顶掉，面板上看不到

### 6. Admin API

- `AddCredentialRequest.refresh_token`：`String` → `Option<String>`，新增 `kiro_api_key`
- `UpdateCredentialRequest`：新增 `kiro_api_key`（空串表示清除）
- `add_credential`：去重哈希按凭据类型分流（API Key 比 `kiroApiKey`，OAuth 比 `refreshToken`）；API Key 凭据跳过"刷新一次以验证"这一步
- `update_credential` 的 `needs_revalidation`：仅当更新后**仍是 OAuth 凭据**时才触发，否则会撞上 refresh 守卫

## 预期影响

| 模块 | 改动 | 兼容性 |
|---|---|---|
| `kiro/model/credentials.rs` | 新字段 + 3 helper + 别名规范化 | 向后兼容（Option，旧配置不受影响；未设置时不序列化） |
| `kiro/token_manager.rs` | 短路 + 守卫 + 校验放行 + 启动校验 + `InvalidConfig` | OAuth 路径逻辑不变 |
| `kiro/provider.rs` | `token_type_header()` 替换硬编码分支；MCP 补头 | external_idp 行为不变，MCP 是修缺陷 |
| `kiro/machine_id.rs` | 派生分支互斥 | OAuth 凭据派生结果不变 |
| `admin/types.rs` + `admin/service.rs` | DTO 字段 | `refreshToken` 由必填变可选——**API 层面放宽**，旧客户端不受影响 |

## 风险

| 风险 | 级别 | 缓解 |
|---|---|---|
| API Key 误走刷新流程，把 key 当 refreshToken 发给上游 | high | `refresh_token()` 拒绝守卫 + 专项测试；`update_credential` 的 revalidation 条件收窄 |
| machineId 在配置修复前后漂移 | medium | 两条派生路径互斥不回落 + 专项测试 |
| `InvalidConfig` 被 stats 覆盖导致真因丢失 | medium | 与 `Manual` 同级保护 + 已核对自愈/持久化/分类三处行为点 |
| `refreshToken` 改可选后，OAuth 凭据漏填时报错变模糊 | low | `validate_refresh_token` 仍对非 API Key 凭据要求 refreshToken，错误信息不变 |
| API Key 凭据无法在 add 时验证有效性 | low | 文档化：有效性由首次真实调用判定；启动校验只保证字段完整 |

## 验收标准

- [x] `cargo check` 通过
- [x] `cargo clippy -- -D warnings` 通过（提交门槛）
- [x] `cargo fmt --check` 通过
- [x] `cargo test` 全绿，既有测试零回归（413 → 430，新增 17）
- [x] 不引入新外部 crate
- [x] 旧 `credentials.json`（无 `kiroApiKey`）行为完全不变
- [x] `docs/代码速查表.md` + `README.md` 同步
