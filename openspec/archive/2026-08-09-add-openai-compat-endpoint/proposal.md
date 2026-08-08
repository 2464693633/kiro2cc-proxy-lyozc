# 变更提案：add-openai-compat-endpoint

## 背景

当前代理只暴露 Anthropic 协议端点（`/v1/messages`、`/cc/v1/messages`）。大量客户端只会说 OpenAI Chat Completions 协议（Cherry Studio / LobeChat / one-api / 各语言 OpenAI SDK），要接入必须额外部署一层翻译代理进程。

`/v1/models` 现有响应已是 OpenAI 形状（`{object:"list", data:[{id, object:"model", created, owned_by, ...}]}`，见 `handlers.rs:215` / `:297`），只缺 `POST /v1/chat/completions` 这一个端点。

参考实现：`ZyphrZero/kiro.rs` 的 `src/anthropic/openai.rs`（该实现内部强制非流式后合成 SSE，本提案改为真流式转码）。

## 目标范围

**在范围内：**
- 新增 `src/anthropic/openai.rs`：OpenAI ↔ Anthropic 双向协议翻译
- 新增路由 `POST /v1/chat/completions`，挂在既有 `v1_routes`（自动继承 `auth_middleware`）
- **真流式转码**：内部走 `stream: true`，Anthropic SSE 逐帧转 OpenAI `chat.completion.chunk`，不缓冲全量
- 请求翻译：system/developer 抽取、tool_calls↔tool_use、tool↔tool_result、data-URL 图片、tools/tool_choice、`max_completion_tokens` 别名
- 响应翻译：文本 / tool_calls / thinking（`reasoning_content`）/ finish_reason / usage（含 `prompt_tokens_details.cached_tokens`）
- 单元测试覆盖三段：请求翻译、非流式响应翻译、SSE 转码状态机

**不在范围内：**
- `/v1/responses`（OpenAI Responses API）—— Codex CLI 默认协议，工作量约翻倍，后续 issue
- `/v1/embeddings`、`/v1/completions`（legacy）—— Kiro 上游无对应能力
- 远程 URL 图片下载 —— 仅支持 `data:` URL，远程拉取属额外出网行为
- `temperature` / `top_p` / `n` / penalties 的语义透传 —— Kiro 上游不支持，serde 静默忽略（不报错）
- admin-ui 前端改动
- `/cc/v1/chat/completions` —— `/cc/v1` 的缓冲语义是为 Claude Code 的 `input_tokens` 精度设计的，对 OpenAI 客户端无意义

## 技术方案

**核心思路：不碰任何既有链路，新端点做纯协议翻译后复用 `post_messages`。**

```
POST /v1/chat/completions
  ├─ openai_to_anthropic()      OpenAI JSON → Anthropic JSON (serde_json::Value)
  │                              ↓ to_vec → Bytes
  ├─ post_messages(...)          ★ 完全复用：模型映射、多账号故障转移、RPM 计数、
  │                                用量记账、prompt cache 四层降级链、多端点 LB、
  │                                thinking、tool schema 规范化
  │                              ↓ Response
  └─ 流式 → OpenAiSseTranscoder  Anthropic SSE → OpenAI chunk（逐帧）
     非流 → parse + build        Anthropic JSON → chat.completion
```

**为什么翻译产物是 `serde_json::Value` 而非 `MessagesRequest`**：
1. `MessagesRequest` 只派生 `Deserialize`（`types.rs:129`），构造后无法序列化
2. `post_messages` 接收裸 `Bytes` 并在内部 `parse_messages_request` 反序列化（`handlers.rs:709`、`:171`）

构造 Value → `to_vec` → `Bytes` 既避免为 5 个类型加 `Serialize`，又让请求走与真实 Anthropic 请求**完全相同**的解析与校验路径。

**签名适配**（与 kiro.rs 参考实现的差异）：

| | kiro.rs | 本项目 |
|---|---|---|
| 请求提取 | `Json<MessagesRequest>` | 裸 `Bytes` |
| 认证上下文 | `Extension<KeyContext>` | `Option<Extension<ApiKeyContext>>` |
| 额外 extractor | 无 | `ConnectInfo<SocketAddr>` + `HeaderMap` |

新 handler 声明相同 extractor 后原样透传。

**转码器两个关键约束**：

1. **独立 tool index**：OpenAI 的 `tool_calls[].index` 必须仅在工具调用间从 0 递增，而 Anthropic 的 block index 把 text/thinking 块也计入同一序列（`stream.rs` 的 `next_block_index`）。转码器维护独立 ordinal 计数器 + `HashMap<block_index, tool_index>` 映射。
2. **字节级缓冲**：TCP chunk 边界可能落在多字节字符中间，按 chunk 做 `from_utf8_lossy` 会把被切断的字节替换成 U+FFFD 使 CJK 文本永久损坏。故 `buf: Vec<u8>` 只缓冲原始字节，切出完整 SSE 帧后才解码。

## 预期影响

| 模块 | 改动 | 兼容性 |
|---|---|---|
| `src/anthropic/openai.rs`（新增） | 协议翻译 + SSE 转码器 | 无影响（新模块） |
| `src/anthropic/mod.rs` | `mod openai;` + 文档注释 | 无影响 |
| `src/anthropic/router.rs` | `v1_routes` 加一条 route | 无影响（新增路由，既有路由不变） |
| `converter.rs` / `stream.rs` / `provider.rs` / `token_manager.rs` / `middleware.rs` / `types.rs` | **零改动** | — |

用量记账、RPM 计数、prompt cache、多端点 LB、故障转移均由复用的 `post_messages` 自动生效，**不会重复计数**（新端点自身不做任何记账）。

`/v1/models` 无需改动即兼容 OpenAI 客户端。

## 风险

| 风险 | 级别 | 缓解 |
|---|---|---|
| tool_calls index 语义错位致客户端解析失败 | high | 独立 ordinal 计数器；专项回归测试（先文本后双工具场景） |
| TCP chunk 切断多字节字符致 CJK 乱码 | high | 字节级缓冲；专项回归测试（在 "中" 的 3 字节序列正中间切断，已验证该测试能捕获旧实现的 `"���文内容"`） |
| 上游异常断流客户端永久等待 | medium | `eof()` 无条件补发结束帧 + `[DONE]`；专项测试 |
| `reasoning_content` 非 OpenAI 官方字段 | low | 社区事实标准（DeepSeek / vLLM / OpenRouter 一致）；不识别的客户端忽略未知字段 |
| 大响应体内存占用 | low | 真流式转码不缓冲全量，仅保留未成帧的尾部字节 |

## 验收标准

- [x] `cargo check` 通过
- [x] `cargo clippy -- -D warnings`：本 change 触及文件零诊断（预存在 `cache/fingerprint.rs:203`、`kiro/token_manager.rs:394` 不在范围内）
- [x] `cargo fmt --check` 通过（触及的 3 个文件）
- [x] `cargo test` 全绿，既有测试零回归（基线 385 → 413，新增 28）
- [x] 不引入新外部 crate（`Cargo.toml` 无改动）
- [x] 既有 `/v1/messages`、`/cc/v1/messages` 行为完全不变
- [x] `docs/代码速查表.md` + `README.md` 同步
