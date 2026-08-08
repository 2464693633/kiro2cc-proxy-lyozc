# 变更提案：admin-ui-api-key-and-disabled-reason

## 背景

上一个变更（`add-kiro-api-key-credential`）让后端支持了 Kiro API Key 凭据，但前端没有同步，留下两个断口：

**断口 1：前端无法添加 API Key 凭据。** 添加对话框的认证方式只有 `social` / `idc` 两个选项，且 `refreshToken` 是前端强制必填（`if (!refreshToken.trim())` 直接 toast 拦截）。API Key 凭据没有 `refreshToken`，会被挡在前端，只能靠手写 `credentials.json` 或直接调 API 接入。前端类型 `authMethod?: 'social' | 'idc'` 连此前就支持的 `external_idp` 也一直没补。

**断口 2：禁用原因不可见。** 后端有 4 种 `DisabledReason`（`Manual` / `TooManyFailures` / `QuotaExceeded` / `InvalidConfig`），但前端只能看到 `disabled: bool`，四种情况显示同一个"已禁用"。四者的处置方式完全不同：

| 原因 | 处置 |
|---|---|
| `Manual` | 人工重新启用 |
| `TooManyFailures` | 可重置失败计数重试 |
| `QuotaExceeded` | 等下个计费周期 |
| `InvalidConfig` | 必须改配置后重启（自愈不会救它） |

`InvalidConfig` 正是上个变更新增的：`authMethod=api_key` 但漏填 `kiroApiKey` 时账号会被启动校验自动禁用。若前端不显示真因，用户会把它当瞬态问题反复点"启用"，而它永远不会自愈。

**关键发现**：`disabled_reason` 在后端三层都没有暴露 —— `DisabledReason` 是私有 enum，`CredentialEntrySnapshot` 和 `CredentialStatusItem` 都只有 `disabled: bool`。所以这不是纯前端变更，必须先补后端暴露链路。

## 目标范围

**在范围内：**

后端（前端的前置条件）：
- `DisabledReason` 由私有改 `pub`（serde `snake_case` 表示不变，不影响既有持久化）
- `CredentialEntrySnapshot` 新增 `disabled_reason: Option<DisabledReason>`
- `CredentialStatusItem`（Admin DTO）新增同名字段
- 仅在 `disabled == true` 时给出原因

前端：
- 类型补齐：`refreshToken` 改可选、新增 `kiroApiKey`、`authMethod` 补 `external_idp` / `api_key`、`CredentialStatusItem` 增加 `disabledReason`
- 添加对话框：认证方式增加 "Kiro API Key"；选中后 `refreshToken` 输入框替换为 `kiroApiKey`，必填校验随之切换
- 编辑对话框：API Key 凭据显示 `kiroApiKey` 字段，沿用"留空不修改"语义
- 卡片 / 详情页展示禁用原因
- `zh` / `en` 双语文案

**不在范围内：**
- `Suspended` 禁用原因（属于未做的错误分类钩子变更，本变更只覆盖现存 4 种）
- 从 kiro.rs 移植错误分类钩子（独立变更）
- `/v1/responses`（独立变更）
- 卡片列表按禁用原因筛选 / 排序（后续 issue）
- `external_idp` 的专属字段表单（`provider` / `tokenEndpoint` / `scopes`）—— 本变更只补类型联合值，不加表单项

## 技术方案

1. **后端暴露链路**：enum 公开化 → snapshot 增字段 → DTO 增字段 → `AdminService::get_all_credentials` 透传。全链路 `Option`，`skip_serializing_if = "Option::is_none"`，旧前端读不到该字段时行为不变。

2. **原因仅在禁用时给出**：`disabled_reason: if e.disabled { e.disabled_reason } else { None }`。内存里 `disabled_reason` 可能残留上一次自动禁用的历史值（例如自愈把 `disabled` 置回 `false` 但没清 reason），直接透传会让前端把历史原因当成当前状态。

3. **前端必填校验按凭据类型分流**：`isApiKey = authMethod === 'api_key'`，校验分支从"`refreshToken` 必填"改为"API Key 凭据校验 `kiroApiKey`，其余校验 `refreshToken`"。提交时按类型只发��应字段，不把空串发给后端。

4. **i18n 映射抽到 `lib/utils.ts`**：`disabledReasonI18nKey(reason)` 返回 i18n key，卡片与详情页共用，避免两处各写一份 `switch`。

5. **展示位置分工**：卡片空间紧张，原因走徽章 `title` 悬浮（零布局改动）；详情页空间充足，作为可见文本展示。

## 预期影响

| 模块 | 改动 | 兼容性 |
|---|---|---|
| `src/kiro/token_manager.rs` | enum 公开化 + snapshot 字段 | 向后兼容（serde 表示不变；新字段 Option） |
| `src/admin/types.rs` | DTO 新增字段 | 向后兼容（Option + skip_if_none） |
| `src/admin/service.rs` | 透传字段 | 无行为变化 |
| `admin-ui/src/types/api.ts` | 类型补齐 | `refreshToken` 由必填改可选，是放宽 |
| `add-credential-dialog.tsx` | 新选项 + 校验分流 | social / idc 路径行为不变 |
| `edit-credential-dialog.tsx` | 新增条件字段 | 非 API Key 凭据不显示 |
| `credential-card.tsx` / `credential-detail-page.tsx` | 展示原因 | 无原因时与现状一致 |
| `lib/utils.ts` | 新增 helper | 纯新增 |
| `zh.json` / `en.json` | 各 +9 键 | 纯新增 |

## 风险

| 风险 | 级别 | 缓解 |
|---|---|---|
| 前端把历史 `disabled_reason` 当当前状态 | medium | 后端在 `disabled == false` 时强制返回 `None`，单测锁定 |
| `refreshToken` 改可选后，social 凭据漏填也能提交 | medium | 校验按类型分流，social 路径仍强制必填；后端 `validate_refresh_token` 是第二道防线 |
| i18n 键在一侧漏加导致显示 key 原文 | low | 提交前脚本对比 `zh` / `en` 键集合一致 |
| `authMethod` 联合类型新增值后 `switch` 未覆盖 | low | `disabledReasonI18nKey` 用 `Record` 全覆盖 + 兜底 |

## 验收标准

- [ ] `cargo fmt --check` / `cargo clippy -- -D warnings` / `cargo test` 全绿
- [ ] `admin-ui`：`tsc -b` 与 `vite build` 均通过
- [ ] `zh` / `en` 键集合完全一致
- [ ] 不引入新外部 crate、不引入新 npm 依赖
- [ ] 后端新增单测覆盖"未禁用时不给原因"
