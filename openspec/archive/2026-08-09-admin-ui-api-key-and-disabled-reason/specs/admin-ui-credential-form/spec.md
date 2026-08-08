# 规范增量：admin-ui-credential-form

## 新增需求

### 需求：禁用原因通过 Admin API 暴露

`GET /api/admin/credentials` 的每个凭据条目在被禁用时携带 `disabledReason`，让调用方能区分四种禁用语义。

#### 场景：禁用账号给出原因
- **WHEN** 某账号 `disabled == true` 且内存中 `disabled_reason` 为 `QuotaExceeded`
- **THEN** 该条目返回 `"disabledReason": "quota_exceeded"`（serde `snake_case`）

#### 场景：未禁用账号不给原因
- **WHEN** 某账号 `disabled == false`，但内存中 `disabled_reason` 仍残留历史值（如自愈重新启用后未清理）
- **THEN** 该条目**不含** `disabledReason` 字段，避免调用方把历史原因当成当前状态

#### 场景：四种原因各自可区分
- **WHEN** 账号分别因手动禁用 / 连续失败 / 额度耗尽 / 配置不完整被禁用
- **THEN** 依次返回 `manual` / `too_many_failures` / `quota_exceeded` / `invalid_config`

#### 场景：字段缺省不破坏旧调用方
- **WHEN** 账号未被禁用
- **THEN** 响应中该字段被 `skip_serializing_if` 跳过，旧前端按缺省处理，行为与现状一致

### 需求：Admin UI 支持添加 API Key 凭据

添加账号对话框的认证方式包含 "Kiro API Key"，选中后表单按 API Key 凭据的字段要求切换。

#### 场景：选中 API Key 后替换凭据输入框
- **WHEN** 用户在认证方式下拉框选择 "Kiro API Key"
- **THEN** `refreshToken` 输入框隐藏，`kiroApiKey` 输入框显示（标记必填），并给出"不需要 Refresh Token、不会过期"的说明

#### 场景：API Key 凭据校验 kiroApiKey 而非 refreshToken
- **WHEN** 认证方式为 API Key 且 `kiroApiKey` 为空，用户提交
- **THEN** 前端拦截并提示需要填写 Kiro API Key，不发起请求

#### 场景：API Key 凭据不要求 refreshToken
- **WHEN** 认证方式为 API Key，`kiroApiKey` 已填、`refreshToken` 为空，用户提交
- **THEN** 请求正常发出，payload 含 `kiroApiKey` 且**不含** `refreshToken`

#### 场景：social 凭据仍强制 refreshToken
- **WHEN** 认证方式为 Social 且 `refreshToken` 为空，用户提交
- **THEN** 前端仍拦截并提示需要填写 Refresh Token（与现状一致）

#### 场景：切换认证方式后不残留另一类字段
- **WHEN** 用户填了 `kiroApiKey` 后把认证方式切回 Social 并提交
- **THEN** payload 不含 `kiroApiKey`（按当前认证方式只发对应字段）

### 需求：Admin UI 支持修改 API Key 凭据的 key

编辑对话框对 API Key 凭据提供 `kiroApiKey` 字段，沿用其它敏感字段的"留空不修改"语义。

#### 场景：API Key 凭据显示 key 字段
- **WHEN** 打开一个 `authMethod == "api_key"` 账号的编辑对话框
- **THEN** 显示 `kiroApiKey` 输入框，占位文案提示"已配置，留空不修改"

#### 场景：留空不修改
- **WHEN** 用户未填 `kiroApiKey` 直接提交
- **THEN** payload 不含该字段，后端保留原值

#### 场景：非 API Key 凭据不显示该字段
- **WHEN** 打开 social / idc 账号的编辑对话框
- **THEN** 不显示 `kiroApiKey` 输入框

### 需求：Admin UI 展示禁用原因

被禁用账号在卡片与详情页展示可读的禁用原因，而非统一的"已禁用"。

#### 场景：卡片通过悬浮提示给出原因
- **WHEN** 某账号被禁用且响应含 `disabledReason`
- **THEN** 卡片的"已禁用"徽章带 `title` 悬浮文案显示对应原因，卡片布局不变

#### 场景：详情页作为可见文本给出原因
- **WHEN** 打开被禁用账号的详情页
- **THEN** 状态徽章旁以可见文本显示禁用原因

#### 场景：无原因时退化到现状
- **WHEN** 账号被禁用但响应不含 `disabledReason`（如后端为旧版本）
- **THEN** 仅显示"已禁用"，不显示原因，不报错

#### 场景：配置不完整的原因指向可执行的修复
- **WHEN** 账号因 `authMethod=api_key` 但缺少 `kiroApiKey` 被启动校验禁用
- **THEN** 展示的原因明确指出配置缺失（而非"连续认证失败"这类会诱导用户反复重试的描述）

### 需求：双语文案完整

#### 场景：中英键集合一致
- **WHEN** 对比 `zh.json` 与 `en.json` 的 `credentials` 段
- **THEN** 两侧键集合完全相同，无任一侧缺键（缺键会导致界面显示 i18n key 原文）
