# 规范增量：openai-compat

## 新增需求

### 需求：POST /v1/chat/completions 端点

代理暴露 OpenAI Chat Completions 兼容端点，把请求翻译为 Anthropic 格式后复用 `/v1/messages` 全链路，再把响应翻回 OpenAI 格式。

#### 场景：认证沿用子 API Key
- **WHEN** 客户端以 `Authorization: Bearer <子 API Key>` 或 `x-api-key` 调用 `POST /v1/chat/completions`
- **THEN** 由 `v1_routes` 上既有的 `auth_middleware` 校验（启用/过期/额度），与 `/v1/messages` 完全一致；未认证返回 401

#### 场景：请求体无法解析
- **WHEN** 请求体不是合法 JSON 或缺少必需字段
- **THEN** 返回 HTTP 400，body 为 OpenAI 错误形状 `{"error":{"message":...,"type":"invalid_request_error"}}`

#### 场景：无有效对话消息
- **WHEN** `messages` 为空数组，或仅含 `system`/`developer` 角色（无任何 user/assistant 轮）
- **THEN** 返回 HTTP 400，`type` 为 `invalid_request_error`

#### 场景：上游错误原样透传
- **WHEN** 内部 `post_messages` 返回非 2xx（如 429 限流、400 上下文超限、503 provider 未配置）
- **THEN** 原样透传该状态码与响应体（Anthropic 错误体 `{error:{type,message}}` 与 OpenAI 同名同构，无需转换）

#### 场景：不重复计量
- **WHEN** 任一 `/v1/chat/completions` 请求完成
- **THEN** RPM 计数、用量记账、prompt cache 追踪均只由内部 `post_messages` 执行一次；本端点自身不做任何记账

### 需求：OpenAI → Anthropic 请求翻译

#### 场景：system 与 developer 角色抽取到顶层
- **WHEN** `messages` 含 `role: "system"` 或 `role: "developer"` 的消息
- **THEN** 其文本按出现顺序抽取到 Anthropic 顶层 `system: [{type:"text", text}]`，且不出现在 `messages` 数组中

#### 场景：assistant.tool_calls 转 tool_use 块
- **WHEN** assistant 消息含 `tool_calls[]`
- **THEN** 每项转为 `{type:"tool_use", id, name, input}`，其中 `function.arguments`（JSON 字符串）被解析为 `input` 对象

#### 场景：非法 arguments 降级为空对象
- **WHEN** `function.arguments` 不是合法 JSON
- **THEN** `input` 取 `{}`，不使整轮请求失败（容忍客户端序列化瑕疵）

#### 场景：tool 角色转 tool_result 并归入 user 轮
- **WHEN** `messages` 含 `role: "tool"` 且带 `tool_call_id`
- **THEN** 转为 `{type:"tool_result", tool_use_id, content}` 并归入 **user** 轮（Anthropic 协议要求 tool_result 属 user）

#### 场景：相邻同角色消息合并
- **WHEN** 翻译后出现连续同 role 的消息轮（如连续两条 user、或多条 tool 结果）
- **THEN** 合并为单轮，content blocks 按序拼接（Anthropic 不接受连续同角色）

#### 场景：data-URL 图片转 base64 块
- **WHEN** user 消息的 content 数组含 `{type:"image_url", image_url:{url:"data:<media_type>;base64,<data>"}}`
- **THEN** 转为 `{type:"image", source:{type:"base64", media_type, data}}`

#### 场景：远程 URL 图片被丢弃
- **WHEN** `image_url.url` 不是 `data:` URL（如 `https://...`）
- **THEN** 该 block 被丢弃，不阻断请求（远程拉取属额外出网行为，不在范围内）

#### 场景：max_tokens 优先级与缺省
- **WHEN** 请求给出 `max_tokens` 与 `max_completion_tokens`
- **THEN** `max_tokens` 优先；仅给后者时后者生效；两者皆缺或 ≤ 0 时取默认 32000

#### 场景：tool_choice 四种取值映射
- **WHEN** `tool_choice` 为 `"auto"` / `"required"` / `"none"` / `{type:"function",function:{name}}`
- **THEN** 分别映射为 `{type:"auto"}` / `{type:"any"}` / **省略该字段** / `{type:"tool",name}`

#### 场景：reasoning_effort 映射
- **WHEN** 请求含非空 `reasoning_effort`
- **THEN** 写入 Anthropic `output_config: {effort}`

#### 场景：不支持的参数静默忽略
- **WHEN** 请求含 `temperature` / `top_p` / `n` / `presence_penalty` / `frequency_penalty` / `stream_options` 等 Kiro 上游不支持的参数
- **THEN** 静默忽略，**不返回错误**（保持客户端兼容）

### 需求：Anthropic → OpenAI 非流式响应翻译

#### 场景：纯文本响应
- **WHEN** Anthropic 响应 content 仅含 text 块且 `stop_reason: "end_turn"`
- **THEN** 返回 `object: "chat.completion"`、`choices[0].message.content` 为拼接文本、`finish_reason: "stop"`、`id` 以 `chatcmpl-` 开头

#### 场景：纯工具调用响应 content 为 null
- **WHEN** Anthropic 响应仅含 tool_use 块（无 text）
- **THEN** `choices[0].message.content` 为 `null`，`tool_calls[]` 每项含 `id` / `type:"function"` / `function.name` / `function.arguments`（**JSON 字符串**而非对象），`finish_reason: "tool_calls"`

#### 场景：finish_reason 映射
- **WHEN** Anthropic `stop_reason` 为 `tool_use` / `max_tokens` / `model_context_window_exceeded` / 其它
- **THEN** 分别映射为 `tool_calls` / `length` / `length` / `stop`；若响应实际含 tool_calls 则一律为 `tool_calls`

#### 场景：thinking 块渲染为 reasoning_content
- **WHEN** Anthropic 响应含 thinking 块
- **THEN** 聚合文本写入 `choices[0].message.reasoning_content`（社区事实标准字段），且不混入 `content`

#### 场景：usage 字段映射
- **WHEN** Anthropic 响应含 usage
- **THEN** `input_tokens`→`prompt_tokens`、`output_tokens`→`completion_tokens`，并计算 `total_tokens`

#### 场景：prompt cache 命中映射为 cached_tokens
- **WHEN** Anthropic usage 含 `cache_read_input_tokens`
- **THEN** 写入 `usage.prompt_tokens_details.cached_tokens`（OpenAI 官方字段）；无该字段时**不输出** `prompt_tokens_details`

### 需求：真流式 SSE 转码

`stream: true` 时内部同样以流式调用上游，Anthropic SSE 事件被**逐帧**转为 OpenAI `chat.completion.chunk`，不缓冲全量响应。

#### 场景：message_start 产出 role 帧且仅一次
- **WHEN** 收到 Anthropic `message_start` 事件
- **THEN** 产出一个 `delta: {role:"assistant", content:""}` 的 chunk；后续重复的 `message_start` 不再产出

#### 场景：text_delta 逐帧转发
- **WHEN** 收到 `content_block_delta` + `text_delta`
- **THEN** 每帧产出恰好一个 `delta: {content:"<增量文本>"}` 的 chunk（不聚合、不等待流结束）

#### 场景：thinking_delta 转 reasoning_content
- **WHEN** 收到 `content_block_delta` + `thinking_delta`
- **THEN** 产出 `delta: {reasoning_content:"<增量>"}` 的 chunk

#### 场景：tool_calls index 仅在工具间从 0 递增
- **WHEN** 响应先有 text 块（Anthropic block index 0），随后两个 tool_use 块（block index 1、2）
- **THEN** 两个工具的 OpenAI `tool_calls[].index` 分别为 **0** 和 **1**（不得沿用 Anthropic block index），且后续这些块的 `input_json_delta` 正确映射回对应工具 index

#### 场景：工具块为首个块时补发 role 帧
- **WHEN** `content_block_start`(tool_use) 在任何 `message_start` 之前到达
- **THEN** 先补发 role 帧再发 tool_calls chunk，保证客户端能建立 assistant 消息

#### 场景：input_json_delta 增量拼接
- **WHEN** 同一工具块连续收到多个 `input_json_delta`
- **THEN** 每帧产出携带同一 `index` 的 `function.arguments` 增量；客户端顺序拼接后得到完整合法 JSON

#### 场景：结束帧携带 usage 并以 [DONE] 收尾
- **WHEN** 收到 `message_delta`（含 stop_reason 与 usage）随后 `message_stop`
- **THEN** `message_delta` 自身不产出 chunk；`message_stop` 时产出带 `finish_reason` 与 `usage` 的结束帧，紧随 `data: [DONE]`

#### 场景：ping 帧不泄漏
- **WHEN** 收到 Anthropic `ping` 保活事件
- **THEN** 丢弃，不产出任何 OpenAI chunk（OpenAI 协议无对应语义）

#### 场景：上游 error 事件透出并立即收尾
- **WHEN** 流中途收到 Anthropic `error` 事件
- **THEN** 产出含 `error.message` 的 chunk 后立即 `[DONE]` 收尾，且不再重复发送结束帧（避免客户端把截断响应当作正常完成）

#### 场景：上游断流仍保证终止
- **WHEN** 上游流结束或读取出错，但从未收到 `message_stop`
- **THEN** 补发带 `finish_reason` 的结束帧 + `data: [DONE]`（否则 OpenAI SDK 会永久等待）

#### 场景：单 chunk 含多帧全部产出
- **WHEN** 一个 TCP chunk 内含多个完整 SSE 帧
- **THEN** 按序解析并产出全部对应 OpenAI chunk

#### 场景：跨 chunk 半帧拼接
- **WHEN** 一个 SSE 帧被 TCP chunk 边界切成两半
- **THEN** 前半不产出任何 chunk；后半到达后完整解析产出

#### 场景：切断多字节字符不损坏文本
- **WHEN** TCP chunk 边界落在一个多字节 UTF-8 字符（如 CJK）的字节序列中间
- **THEN** 拼接后文本与原文完全一致，**不得出现 U+FFFD 替换字符**（转码器须缓冲原始字节、仅在切出完整帧后解码）

## 兼容性

### 需求：既有端点行为不变

#### 场景：Anthropic 端点零回归
- **WHEN** 客户端调用 `/v1/messages`、`/cc/v1/messages`、`/v1/models`、`/v1/messages/count_tokens`
- **THEN** 行为与本变更前完全一致（`converter.rs` / `stream.rs` / `provider.rs` / `token_manager.rs` / `types.rs` 零改动）

#### 场景：/v1/models 已兼容 OpenAI 客户端
- **WHEN** OpenAI 客户端调用 `GET /v1/models` 做模型发现
- **THEN** 现有响应 `{object:"list", data:[{id, object:"model", created, owned_by, ...}]}` 直接可用，无需改动；多出的 `display_name` / `type` / `max_tokens` 字段对 OpenAI 客户端无害
