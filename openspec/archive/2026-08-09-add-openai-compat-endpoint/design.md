# 设计：add-openai-compat-endpoint

## 架构决策

### 决策 1：复用 `post_messages` 而非新建链路

**选择**：新端点只做协议翻译，把 Anthropic 请求体交给既有 `post_messages`。

**替代方案**：直接调 `convert_request` + `provider.call_api_stream`，自建一条平行链路。

**理由**：`post_messages` 里串了 8 项与协议无关的横切逻辑——RPM 计数、账号绑定过滤、WebSearch 分流、thinking 后缀覆写、prefix token 估算、fingerprint profile 构建、prompt cache 模拟、用量记账。自建链路要么重复实现（后续必然漂移），要么漏掉（静默丢失计量）。翻译层复用则这些全部自动生效，且**不会重复计数**（新端点自身不记账）。

代价是多一次 JSON 序列化/反序列化往返。相对一次上游 LLM 调用的耗时可以忽略。

### 决策 2：翻译产物用 `serde_json::Value`，不构造 `MessagesRequest`

**约束**：
- `MessagesRequest` 只派生 `Deserialize`（`types.rs:129`），构造出来也没法序列化
- `post_messages` 的入参是裸 `Bytes`，内部自行 `parse_messages_request`（`handlers.rs:709`、`:171`）

**选择**：构造 `Value` → `serde_json::to_vec` → `Bytes`。

**替代方案**：给 `MessagesRequest` 及嵌套的 `SystemMessage` / `Tool` / `Thinking` / `OutputConfig` 都加 `Serialize`。

**理由**：加 `Serialize` 会污染 5 个类型的公开派生（且 `Thinking` 的 `deserialize_budget_tokens` 自定义逻辑没有对称的序列化实现，容易埋坑）。走 Value 路径的额外收益：请求经过与真实 Anthropic 请求**完全相同**的解析与校验路径，包括 `parse_messages_request` 的诊断日志——翻译产物若有结构问题，报错位置和真实请求一致。

### 决策 3：真流式转码，不用「内部非流式 + 合成 SSE」

**选择**：内部 `stream: true`，逐帧转码。

**替代方案**（kiro.rs 参考实现的做法）：内部强制 `stream: false`，拿到完整结果后一次性合成 chunk 序列。

**理由**：参考实现的注释承认「对 Codex 这类拿到结果再展示的客户端语义一致」——但对话式客户端（Cherry Studio / LobeChat）会表现为长时间空白后整段文本突然出现。本项目的 `stream.rs` 已经产出规范的 Anthropic SSE，转码是纯映射，多出的复杂度集中在一个状态机里且可完整单测。

代价：多约 200 行 + 一个转码状态机。换来逐 token 输出与增量 `arguments`。

## 转码状态机

### 事件映射表

| Anthropic 事件 | OpenAI chunk |
|---|---|
| `message_start` | `delta:{role:"assistant",content:""}`（仅一次） |
| `content_block_delta` / `text_delta` | `delta:{content}` |
| `content_block_delta` / `thinking_delta` | `delta:{reasoning_content}` |
| `content_block_start` / `tool_use` | `delta:{tool_calls:[{index,id,type,function:{name,arguments:""}}]}` |
| `content_block_delta` / `input_json_delta` | `delta:{tool_calls:[{index,function:{arguments}}]}` |
| `content_block_stop` | 无（OpenAI 无块级结束语义） |
| `message_delta` | 无（暂存 stop_reason + usage） |
| `message_stop` | 结束帧（finish_reason + usage）+ `[DONE]` |
| `ping` | 丢弃 |
| `error` | 错误 chunk + `[DONE]`，置 finished |

### 陷阱 1：tool index 语义不同

Anthropic 的 block index 是**所有** content 块的统一序号（`stream.rs` 的 `next_block_index`，text / thinking / tool_use 共用一个计数器）。OpenAI 的 `tool_calls[].index` 是**仅工具之间**的数组下标，必须从 0 起连续。

「先输出一段文本、再调两个工具」这种极常见的响应下，block index 是 0/1/2，而 OpenAI 期望的 tool index 是 0/1。直接复用会让客户端按数组下标对齐时错位或越界。

**解法**：`tool_ordinal` 独立计数 + `block_to_tool_idx: HashMap<i64, usize>` 反查映射。`input_json_delta` 只认映射表里已登记的 block（未见过 `content_block_start` 的块保守跳过）。

### 陷阱 2：TCP chunk 可能切断多字节字符

初版把缓冲区写成 `String`，每个 chunk 先 `String::from_utf8_lossy` 再追加。这在 ASCII 下没问题，遇到 CJK 就坏：chunk 边界落在「中」（`E4 B8 AD`）中间时，前半的 `E4` 被替换成 U+FFFD，后半的 `B8 AD` 也各自变成 U+FFFD，文本永久损坏且无法恢复。

已实测确认：临时改回按 chunk 解码后，`chunk_split_mid_utf8_character_does_not_corrupt_text` 断言失败，实际得到 `"���文内容"`。

**解法**：`buf: Vec<u8>` 只缓冲原始字节，用 `windows(2).position(|w| w == b"\n\n")` 在字节层面切帧，仅对**完整帧**做 `from_utf8_lossy`（完整帧是 event 名 + JSON，必为合法 UTF-8）。

### 陷阱 3：上游断流客户端永久等待

OpenAI SDK 靠 `data: [DONE]` 判定流结束。若上游异常断流而未发 `message_stop`，不补发就会让客户端挂死。

**解法**：`eof()` 无条件调用 `finish()`；`finish()` 用 `finished` 标志幂等，避免 `error` 事件已收尾后重复发送。

## 数据流

```
Bytes (OpenAI JSON)
  │ serde_json::from_slice → ChatCompletionRequest
  │ openai_to_anthropic()  → Value
  │ serde_json::to_vec     → Bytes
  ▼
post_messages(State, Option<Extension<ApiKeyContext>>, ConnectInfo, HeaderMap, Bytes)
  │
  ├─ 非 2xx → 原样 return（错误体同构）
  │
  ├─ stream=false: to_bytes → parse_anthropic_message → build_completion_json
  │
  └─ stream=true:  into_body().into_data_stream()
                     │ stream::unfold + OpenAiSseTranscoder
                     ▼ Body::from_stream
```

## 测试策略

28 个单测，与 `openai.rs` 同文件 `#[cfg(test)] mod tests`（仓库惯例是就近同文件测试，见 `endpoint.rs` / `cache/*`；`src/test.rs` 是未挂进模块树的遗留 CLI 调试文件）。

| 分组 | 数量 | 覆盖 |
|---|---|---|
| 请求翻译 | 11 | system/developer 抽取、tool_calls→tool_use、非法 arguments 降级、tool→tool_result 归 user、相邻同 role 合并、data-URL 图片 + 远程 URL 丢弃、max_tokens 三态、tool_choice 四态、未知参数不报错、空 messages 拒绝、tools + stream 透传 |
| 非流式响应 | 4 | 纯文本、纯工具（content null + arguments 为字符串）、finish_reason 四态 + thinking、usage + cached_tokens 有无 |
| 流式转码 | 13 | role 帧幂等、text 逐帧、**tool index 跳过 text 块**、arguments 增量拼接、结束帧 usage + `[DONE]`、**跨 chunk 半帧**、**切断多字节字符**、单 chunk 多帧、thinking→reasoning_content、ping 丢弃、断流补终止、error 透出、工具块首发补 role |

粗体为针对上述三个陷阱的回归测试。转码测试通过 `frame()` 辅助函数复刻 `stream.rs::to_sse_string` 的输出格式，直接喂字节给 `feed()`，不需要起 HTTP 服务。
