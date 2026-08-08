# 规范增量：api-key-credential

## 新增需求

### 需求：API Key 凭据类型

代理支持以 Kiro API Key 作为凭据，直接把 `kiroApiKey` 作为 Bearer Token 使用，不依赖 `refreshToken`、不参与 token 刷新。

#### 场景：仅填 kiroApiKey 即被识别
- **WHEN** `credentials.json` 某账号带 `kiroApiKey` 字段但未声明 `authMethod`
- **THEN** 该账号被识别为 API Key 凭据，无需 `refreshToken` 即可通过校验

#### 场景：authMethod 别名规范化
- **WHEN** `authMethod` 为 `api_key` / `apikey` / `API_KEY`（大小写不敏感）
- **THEN** 统一规范化为 `api_key`，均被识别为 API Key 凭据

#### 场景：旧配置零影响
- **WHEN** `credentials.json` 不含 `kiroApiKey` 字段
- **THEN** 所有 OAuth 凭据（social / idc / builder-id / iam / external_idp）行为与本变更前完全一致

#### 场景：未设置时不写入配置文件
- **WHEN** 一个无 `kiroApiKey` 的账号被序列化回 `credentials.json`（如 token 刷新后回写）
- **THEN** 输出不含 `kiroApiKey` 字段，旧配置文件不会凭空多出该键

#### 场景：API Key 不出现在日志
- **WHEN** `KiroCredentials` 被 `Debug` 格式化（如 `tracing::debug!` 打印主凭证）
- **THEN** `kiroApiKey` 显示为 `[REDACTED]`，明文不得出现

### 需求：Token 获取跳过刷新

#### 场景：直接以 kiroApiKey 作为 Bearer Token
- **WHEN** 对 API Key 凭据调用 `try_ensure_token`
- **THEN** 在任何过期判断之前短路返回，`CallContext.token` 即 `kiroApiKey` 原值，不发起刷新请求

#### 场景：缺 key 时报错而非静默失败
- **WHEN** 凭据声明 `authMethod: api_key` 但 `kiroApiKey` 缺失或为空白串
- **THEN** `try_ensure_token` 返回明确错误，错误信息含账号 ID 与缺失字段名

#### 场景：拒绝对 API Key 凭据执行刷新
- **WHEN** 任何路径对 API Key 凭据调用 `refresh_token()`
- **THEN** 立即返回错误（信息含"不支持 token 刷新"），**不得**退化为 social 刷新把 `kiroApiKey` 当 `refreshToken` 发给上游

#### 场景：refreshToken 长度校验对 API Key 凭据放行
- **WHEN** 对 API Key 凭据调用 `validate_refresh_token`
- **THEN** 改为校验 `kiroApiKey` 非空后放行，不触发 100 字符下限与截断检测（与 `external_idp` 的既有特例同构）

### 需求：TokenType 请求头

#### 场景：API Key 凭据携带 API_KEY
- **WHEN** 使用 API Key 凭据调用上游数据面接口
- **THEN** 请求头含 `TokenType: API_KEY`，且 `Authorization` 为 `Bearer <kiroApiKey>`

#### 场景：企业 SSO 凭据行为不变
- **WHEN** 使用 `external_idp` 凭据调用上游
- **THEN** 请求头含 `TokenType: EXTERNAL_IDP`（与本变更前一致）

#### 场景：social / idc 不携带该头
- **WHEN** 使用 social 或 idc 凭据调用上游
- **THEN** 请求头不含 `TokenType`

#### 场景：两类标记同时命中时以 API Key 为准
- **WHEN** 凭据同时声明 `authMethod: external_idp` 且带 `kiroApiKey`
- **THEN** 头值为 `API_KEY`（实际发出的 Bearer Token 来自 `kiroApiKey`，头必须与之匹配）

#### 场景：MCP 调用同样携带 TokenType
- **WHEN** 使用 API Key 或 `external_idp` 凭据发起 MCP（WebSearch）调用
- **THEN** MCP 请求头同样含对应 `TokenType` 值
- **注**：修正预存在缺陷 —— `build_mcp_headers` 此前从不设置该头，导致这两类凭据的 MCP 调用被上游按 social 凭据校验而失败

### 需求：machineId 派生

#### 场景：API Key 凭据使用独立种子
- **WHEN** API Key 凭据需要派生 machineId（未配置账号级与全局 `machineId`）
- **THEN** 取 `sha256("KiroAPIKey/<kiroApiKey>")`，输出 64 位十六进制

#### 场景：与 refreshToken 派生结果不同
- **WHEN** 一个 API Key 凭据的 `kiroApiKey` 与另一 OAuth 凭据的 `refreshToken` 取值相同
- **THEN** 两者派生出的 machineId 不同（种子前缀不同）

#### 场景：两条派生路径互斥不回落
- **WHEN** 凭据声明 `authMethod: api_key`、`kiroApiKey` 缺失，但**同时**带有 `refreshToken`
- **THEN** 返回 `None`，**不得**回落到 `refreshToken` 派生（否则同一账号在配置修复前后 machineId 漂移，上游可能视为换设备）

#### 场景：显式配置优先级不变
- **WHEN** API Key 凭据配置了账号级或全局 `machineId`
- **THEN** 优先使用显式配置值，与 OAuth 凭据的优先级链一致

### 需求：启动期配置校验

#### 场景：缺 key 的账号自动禁用
- **WHEN** 启动加载到声明 `authMethod: api_key` 但 `kiroApiKey` 缺失/空白的账号
- **THEN** 该账号被自动禁用、原因标为 `InvalidConfig`，并记录 warn 日志含账号 ID

#### 场景：配置完整的账号正常启用
- **WHEN** 启动加载到 `authMethod: api_key` 且 `kiroApiKey` 非空的账号
- **THEN** 该账号保持启用，参与正常的账号选择

#### 场景：InvalidConfig 不参与自愈
- **WHEN** 所有账号均被禁用而触发自愈（重置失败计数并重新启用）
- **THEN** 自愈只处理 `TooManyFailures`，`InvalidConfig` 账号保持禁用（配置错误无法通过重试恢复）

#### 场景：InvalidConfig 不被 stats 覆盖
- **WHEN** 某账号本次启动被判为 `InvalidConfig`，而 `stats.json` 中留有它此前的 `QuotaExceeded` / `TooManyFailures` 记录
- **THEN** 禁用原因保持 `InvalidConfig`（与 `Manual` 同级受保护），真因不被历史状态顶掉

#### 场景：改对配置后自然恢复
- **WHEN** 用户补上 `kiroApiKey` 并重启
- **THEN** 该账号恢复启用（`InvalidConfig` 不写入 stats 持久化，每次启动按配置重新推导）

### 需求：Admin API 支持

#### 场景：通过 API 添加 API Key 账号
- **WHEN** `POST /api/admin/credentials` 请求体带 `kiroApiKey` 与 `authMethod: "api_key"`，不带 `refreshToken`
- **THEN** 账号添加成功并启用；**不发起**上游刷新请求（API Key 无刷新流程可用于验证，有效性由首次真实调用判定）

#### 场景：OAuth 账号仍要求 refreshToken
- **WHEN** `POST /api/admin/credentials` 既不带 `refreshToken` 也不带 `kiroApiKey`
- **THEN** 返回错误（`refreshToken` 由必填改为可选，但非 API Key 凭据仍在校验层要求它）

#### 场景：按 kiroApiKey 去重
- **WHEN** 添加的 API Key 凭据其 `kiroApiKey` 与已有账号重复
- **THEN** 返回错误且信息指明 `kiroApiKey` 重复（而非拿 `refreshToken` 比对）

#### 场景：更新 API Key 不触发刷新验证
- **WHEN** `PUT /api/admin/credentials/{id}` 同时更新 `refreshToken` 与 `kiroApiKey`，或对已是 API Key 的账号更新 `refreshToken`
- **THEN** 跳过重新验证（否则会撞上 `refresh_token()` 的拒绝守卫直接报错）

#### 场景：清除 kiroApiKey
- **WHEN** `PUT /api/admin/credentials/{id}` 传 `kiroApiKey: ""`
- **THEN** 该字段被清除为 `None`；若该账号因此变回 OAuth 凭据，后续按 OAuth 规则校验
