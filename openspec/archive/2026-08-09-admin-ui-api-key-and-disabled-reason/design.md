# 设计：admin-ui-api-key-and-disabled-reason

## 为什么需要后端改动

原提案把这项列为"纯前端 follow-up"，实测不成立：`disabled_reason` 在**三层**都不存在——`DisabledReason` 是私有 enum，`CredentialEntrySnapshot` 和 `CredentialStatusItem` 都只有 `disabled: bool`。前端渲染不出没有下发的字段，所以必须先穿透后端。

`kiroApiKey` 那一半确实只差前端（后端在 `add-kiro-api-key-credential` 里已就绪），但两半共用同一个 `credentials` i18n 段与同一批组件，拆成两个变更会让 `api.ts` 和两个 JSON 各改两遍，因此合并为一个变更。

## 关键决策

### 1. `snapshot()` 的守卫：未禁用时强制 `None`

```rust
disabled_reason: if e.disabled { e.disabled_reason } else { None },
```

`CredentialEntry.disabled_reason` 在重新启用时**不保证被清空**——`reset_failure_count` 等路径只清 `disabled`，历史原因可能残留。若直接透传，前端会在一个已启用的账号上读到 `quota_exceeded`，渲染成"额度已用尽"。守卫放在 snapshot 层而不是前端，让"原因仅在禁用时有意义"成为 API 契约的一部分，而不是每个消费方各自记得判断。

单测 `test_snapshot_exposes_disabled_reason_only_while_disabled` 锁这条。

### 2. 不改 `DisabledReason` 的 serde 形态

它已经以 snake_case 持久化在 `stats.json` 里（`QuotaExceeded` / `TooManyFailures` 会写入，见 `add-kiro-api-key-credential` 的白名单逻辑）。改 rename 规则或变体名会让历史数据读不回来，账号重启后丢失禁用态。所以本变更只改可见性（`enum` → `pub enum`），一个字节的序列化输出都不动。

### 3. 前端凭据字段"二选一"而非"都显示"

添加对话框按 `isApiKey` 在 `refreshToken` 和 `kiroApiKey` 之间切换渲染，不同时显示两个。理由是后端 `validate_refresh_token` 对 API Key 凭据走的是完全独立的校验分支（只看 `kiroApiKey`），同时给两个输入框会让用户以为可以都填、或者不确定该填哪个。

配套地，payload 也按认证方式只发对应字段：

```ts
...(isApiKey ? { kiroApiKey: ... } : { refreshToken: ... })
```

不这样做的话，用户先填了 `refreshToken` 再切到 API Key，那个值会一起发出去——后端虽然会忽略它（API Key 分支不读 `refreshToken`），但请求体里携带一个不该存在的凭据本身就不该发生。

### 4. `disabledReasonI18nKey()` 放 `lib/utils.ts`

卡片和详情页都要把 enum 值映射到文案。映射写在共享 helper 里而非各组件内联，是因为后端未来新增变体时，漏改一处会让那个变体在某个页面上静默显示为空白。集中一处后，新增变体只需改 helper + 两个 JSON。

helper 对 `undefined` 返回 `null`，两处调用点都据此退化到仅显示"已禁用"——这同时覆盖了「旧后端 + 新前端」的部署组合。

### 5. 卡片用原生 `title` 而非 `Tooltip` 组件

卡片列表的徽章行已经很挤（健康状态 + 禁用 + 订阅等级）。插入可见文本会挤掉昵称的显示宽度。原生 `title` 零布局成本，代价是移动端没有悬浮态——详情页有可见文本作为补偿路径。正式 `Tooltip` 留作 follow-up。

## 兼容性

| 组合 | 行为 |
|---|---|
| 新后端 + 新前端 | 完整功能 |
| 旧后端 + 新前端 | `disabledReason` 缺省 → helper 返回 `null` → 仅显示"已禁用"（即现状） |
| 新后端 + 旧前端 | 多下发一个字段，旧前端不读，无影响 |

`disabled: bool` 未改动，`disabledReason` 是纯附加信息。既有前端逻辑（`credential.disabled ? ... : ...`）全部不受影响。

## 验证

- `cargo test` 432（基线 430，新增 2）
- `npm run build`：`tsc -b` 类型检查 + `vite build` 产物均通过
- 脚本核对 `zh.json` / `en.json` 的 `credentials` 段键集合完全一致（162 = 162）——防止一侧漏加导致另一语言显示 raw key
