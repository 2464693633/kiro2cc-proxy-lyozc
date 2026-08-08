# 任务清单：add-kiro-api-key-credential

## 状态：ARCHIVED

## 任务

### 1. 凭据模型
- [x] `credentials.rs` 新增 `kiro_api_key: Option<String>`（camelCase + `skip_serializing_if`）
- [x] `Debug` impl 加 `kiro_api_key` 并走 `redact`（禁止明文进日志）
- [x] `canonicalize_auth_method_value` 加 `api_key` / `apikey` 分支
- [x] 新增 `is_api_key_credential()` / `is_external_idp_credential()` / `token_type_header()`
- [x] **单测** 5 个：字段或 authMethod 双路识别 / 别名规范化 / `token_type_header` 优先级（含两者同时命中）/ 序列化往返 + 未设置不序列化 / Debug 脱敏
- 验收：`cargo check` 通过

### 2. Token 获取短路
- [x] `try_ensure_token` 开头短路，在过期判断之前返回 `CallContext`
- [x] `refresh_token()` 加拒绝守卫（防止把 `kiroApiKey` 当 refreshToken 发出）
- [x] `validate_refresh_token` 对 API Key 凭据改校验 `kiroApiKey` 非空后放行
- [x] **单测** 3 个：短路返回 token 即 key / refresh 明确拒绝 / 校验放行且缺 key 与空白串均报错
- 验收：`cargo test` 通过

### 3. TokenType 请求头
- [x] `build_headers` 硬编码 external_idp 分支换成 `token_type_header()`
- [x] `build_mcp_headers` **补上** `TokenType`（预存在缺陷：此前从不设置，external_idp 的 MCP 调用同样受影响）
- [x] **单测** 3 个：API Key 注入 `API_KEY` + Bearer 为 key / social 不带该头 / MCP 路径注入
- 验收：`cargo test provider::tests::` 通过

### 4. machineId 派生
- [x] 末段派生拆成互斥两路：`KiroAPIKey/` vs `KotlinNativeAPI/`
- [x] **单测** 2 个：独立种子（同值 key 与 refreshToken 派生结果不同 + 稳定性）/ 互斥不回落
- 验收：`cargo test machine_id::` 通过

### 5. 启动校验 + InvalidConfig
- [x] `DisabledReason` 新增 `InvalidConfig` 变体 + `describe()` match 臂
- [x] `MultiTokenManager::new` 加校验循环：声明 api_key 但缺 key → 禁用 + 标 `InvalidConfig`
- [x] `load_stats()` 的覆盖守卫把 `InvalidConfig` 提升到与 `Manual` 同级保护
- [x] 已核对三处行为点：自愈只管 `TooManyFailures`（配置错误不自愈）、`persist_credentials` 只写 `Manual`、stats 分类过滤排除 `InvalidConfig`（每次启动重新推导）
- [x] **单测** 2 个：缺 key 自动禁用 + 完整配置不禁用 / `describe()` 与 `Manual` 可区分且含 `kiroApiKey`
- 验收：`cargo test` 通过

### 6. Admin API
- [x] `AddCredentialRequest.refresh_token`：`String` → `Option<String>`；新增 `kiro_api_key`
- [x] `UpdateCredentialRequest` 新增 `kiro_api_key`（空串清除）
- [x] `admin/service.rs::add_credential` 构造时接入两字段（空白串归一为 `None`）
- [x] `token_manager::add_credential` 去重哈希按凭据类型分流；API Key 跳过刷新验证
- [x] `apply_update_fields` 接入 `kiro_api_key`
- [x] `update_credential` 的 `needs_revalidation` 收窄：仅当更新后仍是 OAuth 凭据才触发
- [x] **单测** 2 个：按 `kiroApiKey` 去重且错误信息指明字段 / 添加成功且不发上游请求
- 验收：`cargo test` 通过

### 7. 验证与文档
- [x] `cargo fmt --check` 通过
- [x] `cargo clippy -- -D warnings` 通过（提交门槛，plain 模式）
- [x] `cargo test --bin kiro2cc-proxy` **430/430 全绿**（基线 413 + 新增 17）
- [x] `docs/代码速查表.md` 同步
- [x] `README.md` 同步（credentials.json 格式章节 + 配置详解）

## 验收标准（全局）

- [x] `cargo check` 通过
- [x] `cargo clippy -- -D warnings` 通过
- [x] `cargo fmt --check` 通过
- [x] `cargo test` 全绿（既有零回归 413 → 430）
- [x] 旧 `credentials.json`（无 `kiroApiKey`）行为完全不变
- [x] 不引入新外部 crate（`Cargo.toml` 无改动）
- [x] 文档同步完成

## 已知偏差

`cargo clippy --all-targets -- -D warnings` 的 `field_reassign_with_default` 计数由 61 升至 88（+27），均来自本变更新增测试沿用仓库既有的 `let mut c = Default::default(); c.field = ...` 写法（基线那 61 个同款）。提交门槛沿用上一变更的口径（plain `cargo clippy`），该模式不在门槛内。若后续统一整改，应连同基线 61 处一并改为结构体字面量 + `..Default::default()`。

## 依赖与约束

- 不引入新 crate
- 不改动 OAuth 凭据的任何既有路径（social / idc / external_idp 刷新逻辑零改动）
- 端点选择、多端点 LB、账号选择策略均不涉及
- `InvalidConfig` 不写入 stats 持久化（配置态每次启动重新推导）

## 已知 follow-up（不在本 change 范围）

- admin-ui 前端表单加 `kiroApiKey` 输入框（后端 API 已就绪）
- `KIRO_API_KEY` 环境变量自动建凭据（与"凭据只从 credentials.json 加载"的现有模型冲突，需先定方案）
- API Key 凭据的余额/额度查询（`get_usage_limits_for` 走 OAuth 专用端点，API Key 是否支持未实测）
- API Key 有效性的启动期探测
