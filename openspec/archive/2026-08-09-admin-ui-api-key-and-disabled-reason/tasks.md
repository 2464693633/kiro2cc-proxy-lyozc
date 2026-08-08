# 任务清单：admin-ui-api-key-and-disabled-reason

## 状态：ARCHIVED

## 任务

### 1. 后端暴露 disabledReason（三层穿透）
- [x] `DisabledReason` 由私有 enum 改 `pub`（已有 `#[serde(rename_all = "snake_case")]`，序列化形态不变）
- [x] `CredentialEntrySnapshot` 新增 `pub disabled_reason: Option<DisabledReason>`（`skip_serializing_if`）
- [x] `snapshot()` 填值时加守卫：`if e.disabled { e.disabled_reason } else { None }`
- [x] `CredentialStatusItem` 新增同名字段，`AdminService::get_all_credentials` 透传
- [x] **单测**（2 个）：未禁用时字段为 `None`（守卫生效）/ 四个变体序列化为 snake_case
- 验收：`cargo test` 432 全绿（基线 430）

### 2. 前端类型对齐
- [x] `api.ts` 新增 `DisabledReason` 联合类型（4 个 snake_case 字面量）
- [x] `CredentialStatusItem` 加 `disabledReason?: DisabledReason`
- [x] `AddCredentialRequest`：`refreshToken` 改可选、加 `kiroApiKey`、`authMethod` 联合类型补 `'api_key'` 与 `'external_idp'`
- [x] `UpdateCredentialRequest` 加 `kiroApiKey`
- 验收：`tsc -b` exit 0

### 3. 添加对话框支持 API Key
- [x] `AuthMethod` 类型补 `'api_key'`，下拉框加选项
- [x] 新增 `kiroApiKey` state + `resetForm` 清理 + `isApiKey` 派生标志
- [x] 凭据输入框按 `isApiKey` 二选一渲染（`refreshToken` ↔ `kiroApiKey`）
- [x] 校验分流：API Key 校验 `kiroApiKey` 非空，其它仍校验 `refreshToken`
- [x] payload 按认证方式只发对应字段，避免切换后残留
- 验收：`tsc -b` exit 0

### 4. 编辑对话框支持改 key
- [x] 新增 `kiroApiKey` state + `useEffect` 重置 + `isApiKey` 判定（依据 `credential.authMethod`）
- [x] 仅 API Key 凭据渲染该字段，沿用"留空不修改"语义
- [x] 非空才进 payload
- 验收：`tsc -b` exit 0

### 5. 禁用原因展示
- [x] `lib/utils.ts` 新增 `disabledReasonI18nKey()`，集中映射避免两处组件重复
- [x] 卡片：禁用徽章加 `title` 悬浮文案（不改布局）
- [x] 详情页：状态徽章旁可见文本
- [x] 两处都对 `undefined` 退化到仅显示"已禁用"
- 验收：`tsc -b` exit 0

### 6. 双语文案
- [x] `zh.json` / `en.json` 各新增 9 键（4 个 API Key 相关 + 1 toast + 4 个禁用原因）
- [x] 脚本核对两侧 `credentials` 段键集合完全一致（162 = 162）
- 验收：两文件 `ConvertFrom-Json` 通过，键集合 diff 为空

### 7. 构建与文档
- [x] `npm run build`（`tsc -b` + `vite build`）exit 0
- [x] `cargo fmt --check` / `cargo clippy -- -D warnings` / `cargo test` 全绿
- [x] `docs/代码速查表.md` 同步

## 验收标准（全局）

- [x] `cargo clippy -- -D warnings` 通过
- [x] `cargo fmt --check` 通过
- [x] `cargo test` 432 全绿（基线 430，新增 2，零回归）
- [x] `npm run build` 通过（tsc 类型检查 + vite 产物）
- [x] 中英文案键集合一致
- [x] 不引入新外部 crate、不新增 npm 依赖
- [x] 旧后端 / 旧前端组合下行为退化到现状，不报错

## 依赖与约束

- 复用既有 `skip_serializing_if` 缺省语义，不引入版本协商
- 不改 `DisabledReason` 的 serde 形态（已持久化到 `stats.json`，改名会读不回历史数据）
- 不动 `disabled: bool` 字段，`disabledReason` 是附加信息而非替代
- 前端不新增依赖，沿用既有 `Badge` / `Input` / `select` 组件与 i18n 机制

## 已知 follow-up（不在本变更范围）

- 卡片悬浮提示改用正式 `Tooltip` 组件（当前用原生 `title`，移动端无悬浮态）
- `external_idp` 的专属表单字段（`tokenEndpoint` / `provider` / `scopes` 至今没有前端入口，只能手写 `credentials.json` 或走 API）
- 按禁用原因筛选账号列表
