# 任务清单：add-openai-compat-endpoint

## 状态：ARCHIVED

## 任务

### 1. 请求翻译层
- [x] 新建 `src/anthropic/openai.rs`，定义 `ChatCompletionRequest`（只声明需翻译字段，其余由 serde 忽略）
- [x] `openai_to_anthropic(&req, want_stream) -> Result<Value, String>`：
  - system / developer → 顶层 `system[]`
  - user / assistant 文本 → text blocks
  - `assistant.tool_calls[]` → `tool_use` blocks（`arguments` 字符串解析为 `input`，非法则退 `{}`）
  - `tool` + `tool_call_id` → `tool_result` block 并归入 **user** 轮
  - `image_url` 仅取 `data:` URL → base64 image block
  - `max_tokens` \| `max_completion_tokens` \| 默认 32000
  - `reasoning_effort` → `output_config.effort`
- [x] `push_merged` 合并相邻同 role 轮；空 content 轮丢弃；全空返 Err
- [x] `convert_tools` / `convert_tool_choice`（auto / any / 省略 / tool）
- 验收：`cargo check` 通过

### 2. 非流式响应翻译
- [x] `ParsedResponse` 结构 + `parse_anthropic_message`：text / thinking / tool_use 块分拣
- [x] `map_finish_reason`：tool_use→tool_calls、max_tokens·model_context_window_exceeded→length、有工具→tool_calls、其余→stop
- [x] `build_completion_json`：纯工具轮 `content: null`；thinking → `reasoning_content`
- [x] `build_usage_json`：prompt/completion/total + `prompt_tokens_details.cached_tokens`（取 `cache_read_input_tokens`，无则省略）
- 验收：`cargo check` 通过

### 3. 真流式 SSE 转码器
- [x] **3a. 状态机骨架** `OpenAiSseTranscoder`：`buf` / `role_sent` / `tool_ordinal` / `block_to_tool_idx` / `pending_finish` / `pending_usage` / `finished`
- [x] **3b. 帧切分** `feed`：字节级缓冲 + `windows(2).position(|w| w == b"\n\n")` 切帧，仅对完整帧解码 UTF-8
- [x] **3c. 事件映射** `handle_frame` / `handle_block_start` / `handle_block_delta`：10 类 Anthropic 事件 → OpenAI chunk
- [x] **3d. 收尾** `finish`（幂等）+ `eof`（无条件补 `[DONE]`）
- [x] **3e. 流包装** `transcode_stream`：`stream::unfold` 包 `Body::into_data_stream()`
- 验收：`cargo test openai::` 全绿

### 4. Handler 与路由
- [x] `post_chat_completions`：extractor 与 `post_messages` 对齐（`State` + `Option<Extension<ApiKeyContext>>` + `ConnectInfo` + `HeaderMap` + `Bytes`）并原样透传
- [x] 非 2xx 原样 return（Anthropic 与 OpenAI 错误体同构）
- [x] `src/anthropic/mod.rs`：`mod openai;` + 文档注释补 OpenAI 端点
- [x] `src/anthropic/router.rs`：`v1_routes` 加 `POST /chat/completions`（自动继承 `auth_middleware`）
- 验收：`cargo check` 通过；既有路由不变

### 5. 单元测试（28 个，全部 PASS）
- [x] 请求翻译 11 个
- [x] 非流式响应 4 个
- [x] 流式转码 13 个，含三项陷阱回归：
  - `tool_call_index_starts_at_zero_skipping_text_blocks`
  - `sse_frame_split_across_chunks_is_reassembled`
  - `chunk_split_mid_utf8_character_does_not_corrupt_text`
- 验收：`cargo test --bin kiro2cc-proxy openai::` 28/28 全绿

### 6. 缺陷修正（clippy 暴露）
- [x] 4 处 collapsible-if 改用 let chain（与 `stream.rs:1221` 既有写法一致）
- [x] **UTF-8 边界 bug**：初版 `buf: String` + 按 chunk `from_utf8_lossy` 会把被 TCP 边界切断的多字节字符替换成 U+FFFD。已改为 `buf: Vec<u8>` 字节级缓冲。
  - 已实测验证该测试能捕获旧实现：临时改回后断言失败，实际得 `"���文内容"`
- 验收：`cargo clippy` 在本 change 触及文件零诊断

### 7. 文档与构建验证
- [x] `docs/代码速查表.md`：新增 `src/anthropic/openai.rs` 章节 + 快速定位表补行 + 请求链路总览补 OpenAI 支线
- [x] `README.md`：API 端点表补 OpenAI 兼容端点 + 接入示例 + 功能特性列表
- [x] `cargo fmt --check`：触及的 3 个文件干净
- [x] `cargo clippy -- -D warnings`：仅剩 2 个预存在 error（`cache/fingerprint.rs:203`、`kiro/token_manager.rs:394`），与本 change 前一致
- [x] `cargo test --bin kiro2cc-proxy` **413/413 全绿**（基线 385 + 新增 28，既有零回归）

## 验收标准（全局）

- [x] `cargo check` 通过
- [x] `cargo clippy -- -D warnings` 本 change 触及文件零诊断
- [x] `cargo fmt --check` 通过
- [x] `cargo test` 全绿，既有测试零回归（385 → 413）
- [x] 不引入新外部 crate（`Cargo.toml` 无改动）
- [x] `/v1/messages`、`/cc/v1/messages`、`/v1/models` 行为完全不变
- [x] 文档同步完成（代码速查表 + README）

## 依赖与约束

- 复用既有 `futures::stream::unfold`（与 `handlers.rs::create_sse_stream` 同一模式）
- 复用既有 `auth_middleware`（挂在 `v1_routes` 内即自动生效）
- 不动 `converter.rs` / `stream.rs` / `provider.rs` / `token_manager.rs` / `middleware.rs` / `types.rs`
- 不新增配置项（端点无条件启用，与 `/v1/messages` 同一认证门槛）
- 计量单一来源：本端点自身不记账，全部由复用的 `post_messages` 执行

## 已知 follow-up（不在本 change 范围）

- `/v1/responses`（OpenAI Responses API）—— Codex CLI 默认协议
- 远程 URL 图片下载（当前仅支持 `data:` URL）
- `stream_options.include_usage` 语义：当前无条件在结束帧带 usage，未按该参数开关
- `temperature` / `top_p` 等采样参数：Kiro 上游不支持，当前静默忽略；若上游后续开放需补映射
