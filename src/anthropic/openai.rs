// Copyright (c) 2026 Harllan He. Licensed under MIT.
//! OpenAI Chat Completions 兼容端点
//!
//! 把 OpenAI `POST /v1/chat/completions` 请求翻译成 Anthropic 请求体，交给
//! [`super::handlers::post_messages`] 走完整链路（模型映射、多账号故障转移、
//! RPM 计数、用量记账、prompt cache、多端点负载均衡、tool schema 规范化），
//! 再把响应翻回 OpenAI 格式。
//!
//! 这样只会说 OpenAI 协议的客户端（Cherry Studio / LobeChat / one-api /
//! OpenAI SDK）无需额外翻译进程即可直连 Kiro 后端。
//!
//! # 流式实现
//!
//! 内部走 `stream: true`，[`OpenAiSseTranscoder`] 把 Anthropic SSE **逐帧**转成
//! OpenAI `chat.completion.chunk`，客户端能看到逐 token 输出，工具调用的
//! `arguments` 也是增量的。不缓冲全量响应。
//!
//! # 请求体构造方式
//!
//! 翻译产物是 `serde_json::Value` 而非 [`super::types::MessagesRequest`]：后者
//! 只派生了 `Deserialize`，且 `post_messages` 接收裸 `Bytes` 并在内部自行
//! 反序列化。构造 Value → `to_vec` → `Bytes` 既不必为一串类型加 `Serialize`，
//! 又让请求走与真实 Anthropic 请求完全相同的解析与校验路径。

use std::collections::HashMap;
use std::convert::Infallible;

use axum::{
    body::{Body, Bytes},
    extract::{Extension, State},
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use futures::{Stream, StreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::handlers::post_messages;
use super::middleware::{ApiKeyContext, AppState};

/// 未显式给出 max_tokens 时的默认输出上限
const DEFAULT_MAX_TOKENS: i64 = 32000;

// ============================ 请求类型 ============================

/// OpenAI Chat Completions 请求体
///
/// 只声明需要翻译的字段；`temperature` / `top_p` / `n` / `presence_penalty`
/// 等 Kiro 上游不支持的参数由 serde 默认忽略，不报错（保持客户端兼容）。
#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub max_tokens: Option<i64>,
    #[serde(default)]
    pub max_completion_tokens: Option<i64>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

// ============================ 请求翻译 ============================

/// OpenAI 请求 → Anthropic 请求体（JSON）
///
/// `want_stream` 决定注入的 `stream` 字段：真流式转码下与客户端诉求一致。
fn openai_to_anthropic(req: &ChatCompletionRequest, want_stream: bool) -> Result<Value, String> {
    let max_tokens = req
        .max_tokens
        .or(req.max_completion_tokens)
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS);

    let mut system: Vec<Value> = Vec::new();
    // 合并后的对话消息：(role, content blocks)
    let mut merged: Vec<(String, Vec<Value>)> = Vec::new();

    for m in &req.messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        match role {
            "system" | "developer" => {
                for text in collect_text_strings(m.get("content")) {
                    system.push(json!({ "type": "text", "text": text }));
                }
            }
            "user" => {
                let blocks = content_blocks(m.get("content"));
                push_merged(&mut merged, "user", blocks);
            }
            "assistant" => {
                let mut blocks = content_blocks(m.get("content"));
                if let Some(calls) = m.get("tool_calls").and_then(|v| v.as_array()) {
                    for call in calls {
                        let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let func = call.get("function");
                        let name = func
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        // arguments 是 JSON 字符串；解析失败退化为空对象，
                        // 避免整轮请求因客户端序列化瑕疵被拒。
                        let args_str = func
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        let input: Value =
                            serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input,
                        }));
                    }
                }
                push_merged(&mut merged, "assistant", blocks);
            }
            "tool" => {
                let tool_use_id = m.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("");
                let content = collect_text_strings(m.get("content")).join("\n");
                // Anthropic 协议里 tool_result 属于 user 轮
                push_merged(
                    &mut merged,
                    "user",
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                    })],
                );
            }
            _ => {}
        }
    }

    // 丢弃空内容轮：Anthropic 不接受空 content
    let messages: Vec<Value> = merged
        .into_iter()
        .filter(|(_, blocks)| !blocks.is_empty())
        .map(|(role, blocks)| json!({ "role": role, "content": blocks }))
        .collect();

    if messages.is_empty() {
        return Err("messages must contain at least one user/assistant message".to_string());
    }

    let mut out = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": want_stream,
    });

    if !system.is_empty() {
        out["system"] = Value::Array(system);
    }
    if let Some(tools) = req.tools.as_ref().map(|ts| convert_tools(ts))
        && !tools.is_empty()
    {
        out["tools"] = Value::Array(tools);
    }
    if let Some(tc) = req.tool_choice.as_ref().and_then(convert_tool_choice) {
        out["tool_choice"] = tc;
    }
    if let Some(effort) = req
        .reasoning_effort
        .as_ref()
        .filter(|e| !e.trim().is_empty())
    {
        out["output_config"] = json!({ "effort": effort });
    }

    Ok(out)
}

/// 追加到 merged；与上一轮 role 相同则合并 content blocks
///
/// Anthropic 不接受连续同角色消息，OpenAI 客户端却常发（如连续多条 tool 结果）。
fn push_merged(merged: &mut Vec<(String, Vec<Value>)>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = merged.last_mut()
        && last.0 == role
    {
        last.1.extend(blocks);
        return;
    }
    merged.push((role.to_string(), blocks));
}

/// OpenAI `message.content`（字符串或数组）→ Anthropic content blocks
fn content_blocks(content: Option<&Value>) -> Vec<Value> {
    let mut out = Vec::new();
    match content {
        Some(Value::String(s)) => {
            if !s.is_empty() {
                out.push(json!({ "type": "text", "text": s }));
            }
        }
        Some(Value::Array(parts)) => {
            for part in parts {
                match part.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                    "text" | "input_text" => {
                        if let Some(t) = part.get("text").and_then(|v| v.as_str())
                            && !t.is_empty()
                        {
                            out.push(json!({ "type": "text", "text": t }));
                        }
                    }
                    "image_url" => {
                        if let Some(block) = image_block(part) {
                            out.push(block);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

/// 仅收集纯文本（system / tool 内容用）
fn collect_text_strings(content: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    match content {
        Some(Value::String(s)) => {
            if !s.is_empty() {
                out.push(s.clone());
            }
        }
        Some(Value::Array(parts)) => {
            for part in parts {
                if let Some(t) = part.get("text").and_then(|v| v.as_str())
                    && !t.is_empty()
                {
                    out.push(t.to_string());
                }
            }
        }
        _ => {}
    }
    out
}

/// OpenAI `image_url` → Anthropic image block
///
/// 仅支持 `data:` URL：远程 URL 需要代理侧下载，属于额外的出网行为，不在本端点范围内。
fn image_block(part: &Value) -> Option<Value> {
    let url = part
        .get("image_url")
        .and_then(|iu| iu.get("url"))
        .and_then(|v| v.as_str())?;
    let rest = url.strip_prefix("data:")?;
    let (media_type, data) = rest.split_once(";base64,")?;
    Some(json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": media_type,
            "data": data,
        }
    }))
}

/// OpenAI tools → Anthropic tools
fn convert_tools(tools: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for t in tools {
        // OpenAI: {type:"function", function:{name, description, parameters}}
        // 少数客户端直接平铺 {name, description, parameters}，故 fallback 到 t 本身
        let func = t.get("function").unwrap_or(t);
        let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let description = func
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let input_schema = func
            .get("parameters")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        out.push(json!({
            "name": name,
            "description": description,
            "input_schema": input_schema,
        }));
    }
    out
}

/// OpenAI tool_choice → Anthropic tool_choice
fn convert_tool_choice(tc: &Value) -> Option<Value> {
    match tc {
        Value::String(s) => match s.as_str() {
            "required" => Some(json!({ "type": "any" })),
            // none 表示禁用工具：省略字段即为默认放行，故返回 None
            "none" => None,
            _ => Some(json!({ "type": "auto" })),
        },
        Value::Object(_) => tc
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str())
            .map(|n| json!({ "type": "tool", "name": n })),
        _ => None,
    }
}

// ============================ 响应翻译（非流式） ============================

/// 从 Anthropic 响应中提取的、构造 OpenAI 响应所需的全部信息
#[derive(Debug, Default)]
struct ParsedResponse {
    text: String,
    /// OpenAI 形状的 tool_calls
    tool_calls: Vec<Value>,
    /// thinking 块聚合文本，渲染为 `reasoning_content`
    reasoning: String,
    finish_reason: String,
    prompt_tokens: i64,
    completion_tokens: i64,
    /// prompt cache 命中量，映射到 `prompt_tokens_details.cached_tokens`
    cached_tokens: Option<i64>,
}

/// Anthropic message JSON → [`ParsedResponse`]
fn parse_anthropic_message(anthropic: &Value) -> ParsedResponse {
    let mut p = ParsedResponse::default();

    if let Some(blocks) = anthropic.get("content").and_then(|v| v.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        p.text.push_str(t);
                    }
                }
                Some("thinking") => {
                    if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                        p.reasoning.push_str(t);
                    }
                }
                Some("tool_use") => {
                    p.tool_calls.push(json!({
                        "id": block.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "type": "function",
                        "function": {
                            "name": block.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                            "arguments": block
                                .get("input")
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "{}".to_string()),
                        },
                    }));
                }
                // web_search_tool_result 等块对 OpenAI 客户端无意义，忽略
                _ => {}
            }
        }
    }

    let stop_reason = anthropic
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("end_turn");
    p.finish_reason = map_finish_reason(stop_reason, !p.tool_calls.is_empty()).to_string();

    let usage = anthropic.get("usage");
    p.prompt_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    p.completion_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    p.cached_tokens = usage
        .and_then(|u| u.get("cache_read_input_tokens"))
        .and_then(|v| v.as_i64());

    p
}

/// Anthropic `stop_reason` → OpenAI `finish_reason`
fn map_finish_reason(stop_reason: &str, has_tool_calls: bool) -> &'static str {
    match stop_reason {
        "tool_use" => "tool_calls",
        "max_tokens" | "model_context_window_exceeded" => "length",
        _ if has_tool_calls => "tool_calls",
        _ => "stop",
    }
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn new_id() -> String {
    format!("chatcmpl-{}", Uuid::new_v4().simple())
}

/// 构造 OpenAI usage 对象
fn build_usage_json(prompt_tokens: i64, completion_tokens: i64, cached: Option<i64>) -> Value {
    let mut usage = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens,
    });
    // prompt_tokens_details.cached_tokens 是 OpenAI 官方字段，本项目 prompt cache
    // 四层降级链的产出正好填这里
    if let Some(c) = cached {
        usage["prompt_tokens_details"] = json!({ "cached_tokens": c });
    }
    usage
}

/// 构造非流式 `chat.completion` 响应体
fn build_completion_json(p: &ParsedResponse, model: &str) -> Value {
    // 纯工具调用轮：OpenAI 约定 content 为 null
    let content: Value = if p.text.is_empty() && !p.tool_calls.is_empty() {
        Value::Null
    } else {
        Value::String(p.text.clone())
    };

    let mut message = json!({ "role": "assistant", "content": content });
    if !p.tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(p.tool_calls.clone());
    }
    if !p.reasoning.is_empty() {
        message["reasoning_content"] = Value::String(p.reasoning.clone());
    }

    json!({
        "id": new_id(),
        "object": "chat.completion",
        "created": now_ts(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": p.finish_reason,
        }],
        "usage": build_usage_json(p.prompt_tokens, p.completion_tokens, p.cached_tokens),
    })
}

fn openai_error(status: StatusCode, err_type: &str, message: &str) -> Response {
    let body = json!({
        "error": {
            "message": message,
            "type": err_type,
        }
    });
    (status, Json(body)).into_response()
}

// ============================ 流式转码 ============================

/// Anthropic SSE → OpenAI `chat.completion.chunk` 转码器
///
/// # 为什么需要独立的 tool index
///
/// OpenAI 的 `tool_calls[].index` 必须是**仅在工具调用之间**从 0 递增的序号，
/// 而 Anthropic 的 block index 把 text / thinking 块也计入同一序列
/// （见 `stream.rs` 的 `next_block_index`）。若直接复用 block index，
/// 「先文本后工具」的响应会得到 `index: 1` 起头的 tool_calls，
/// 部分客户端按数组下标对齐时会解析失败。故这里维护独立的 ordinal 计数器
/// 与 `block_to_tool_idx` 映射。
///
/// # 跨 chunk 拼接
///
/// 一个 TCP chunk 可能含多个完整 SSE 帧、也可能只含半帧，因此 `buf` 保留
/// 尚未成帧的尾部字节，等下一个 chunk 到达后再拼接解析。
struct OpenAiSseTranscoder {
    id: String,
    created: i64,
    model: String,
    /// 未成帧的尾部**字节**
    ///
    /// 必须是 `Vec<u8>` 而非 `String`：TCP chunk 边界可能落在一个多字节字符
    /// 中间（CJK 文本尤其常见），若按 chunk 做 `from_utf8_lossy` 会把被切断的
    /// 字节序列替换成 U+FFFD，文本永久损坏。这里只缓冲原始字节，等切出完整
    /// SSE 帧后再解码——完整帧一定是合法 UTF-8。
    buf: Vec<u8>,
    /// role 帧是否已发送（OpenAI 约定只在首个 chunk 出现）
    role_sent: bool,
    /// 下一个工具调用的 OpenAI index
    tool_ordinal: usize,
    /// Anthropic block index → OpenAI tool_calls index
    block_to_tool_idx: HashMap<i64, usize>,
    /// 来自 message_delta 的 stop_reason，等 message_stop 时才落到结束帧
    pending_finish: Option<String>,
    /// 来自 message_delta 的 usage
    pending_usage: Option<(i64, i64, Option<i64>)>,
    /// 结束帧是否已发送，避免重复收尾
    finished: bool,
}

impl OpenAiSseTranscoder {
    fn new(model: &str) -> Self {
        Self {
            id: new_id(),
            created: now_ts(),
            model: model.to_string(),
            buf: Vec::new(),
            role_sent: false,
            tool_ordinal: 0,
            block_to_tool_idx: HashMap::new(),
            pending_finish: None,
            pending_usage: None,
            finished: false,
        }
    }

    /// 序列化一个 delta chunk
    fn chunk(&self, delta: Value) -> String {
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": Value::Null,
            }],
        });
        format!("data: {chunk}\n\n")
    }

    /// 结束帧（带 finish_reason 与 usage）+ `[DONE]`
    fn finish(&mut self) -> String {
        if self.finished {
            return String::new();
        }
        self.finished = true;

        let finish_reason = self.pending_finish.take().unwrap_or_else(|| "stop".into());
        let mut chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": finish_reason,
            }],
        });
        if let Some((prompt, completion, cached)) = self.pending_usage.take() {
            chunk["usage"] = build_usage_json(prompt, completion, cached);
        }
        format!("data: {chunk}\n\ndata: [DONE]\n\n")
    }

    /// 喂入一段字节，返回应发给客户端的 OpenAI SSE 文本
    fn feed(&mut self, chunk: &[u8]) -> String {
        self.buf.extend_from_slice(chunk);
        let mut out = String::new();

        // 按空行切帧；最后一段可能不完整，留在 buf 里等下一个 chunk
        while let Some(pos) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let frame: Vec<u8> = self.buf.drain(..pos + 2).collect();
            // 完整帧必为合法 UTF-8（event 名 + JSON），此处解码安全
            let frame = String::from_utf8_lossy(&frame[..pos]);
            out.push_str(&self.handle_frame(&frame));
        }
        out
    }

    /// 上游流结束时收尾（若上游未给出 message_stop 也要保证 `[DONE]`）
    fn eof(&mut self) -> String {
        let mut out = String::new();
        // 残留未成帧的字节尝试最后解析一次（上游可能省略末尾空行）
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let rest = String::from_utf8_lossy(&rest);
            if !rest.trim().is_empty() {
                out.push_str(&self.handle_frame(&rest));
            }
        }
        out.push_str(&self.finish());
        out
    }

    /// 解析单个 SSE 帧（`event: X\ndata: {...}`）并映射为 OpenAI chunk
    fn handle_frame(&mut self, frame: &str) -> String {
        let mut event_name = "";
        let mut data_line = "";
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                event_name = rest.trim();
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_line = rest.trim();
            }
        }
        if data_line.is_empty() {
            return String::new();
        }
        let Ok(data) = serde_json::from_str::<Value>(data_line) else {
            return String::new();
        };

        match event_name {
            "message_start" => {
                if self.role_sent {
                    String::new()
                } else {
                    self.role_sent = true;
                    self.chunk(json!({ "role": "assistant", "content": "" }))
                }
            }
            "content_block_start" => self.handle_block_start(&data),
            "content_block_delta" => self.handle_block_delta(&data),
            "message_delta" => {
                if let Some(sr) = data.pointer("/delta/stop_reason").and_then(|v| v.as_str()) {
                    // tool_calls 的判定依赖是否真的产生过工具块
                    let has_tools = self.tool_ordinal > 0;
                    self.pending_finish = Some(map_finish_reason(sr, has_tools).to_string());
                }
                if let Some(u) = data.get("usage") {
                    let prompt = u.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                    let completion = u.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                    let cached = u.get("cache_read_input_tokens").and_then(|v| v.as_i64());
                    self.pending_usage = Some((prompt, completion, cached));
                }
                String::new()
            }
            "message_stop" => self.finish(),
            "error" => {
                // 上游中途报错：透出错误 chunk 后立即收尾，避免客户端把
                // 截断的响应当成正常完成
                let message = data
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("upstream stream error");
                self.pending_finish = Some("stop".to_string());
                let err = json!({
                    "id": self.id,
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": "stop",
                    }],
                    "error": { "message": message, "type": "api_error" },
                });
                self.finished = true;
                format!("data: {err}\n\ndata: [DONE]\n\n")
            }
            // ping 在 OpenAI 协议无对应语义，直接丢弃
            _ => String::new(),
        }
    }

    fn handle_block_start(&mut self, data: &Value) -> String {
        let Some(block) = data.get("content_block") else {
            return String::new();
        };
        if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
            return String::new();
        }
        let block_index = data.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
        let tool_idx = self.tool_ordinal;
        self.tool_ordinal += 1;
        self.block_to_tool_idx.insert(block_index, tool_idx);

        let mut out = String::new();
        // 工具块可能是响应的第一个块，此时 role 帧还没发过
        if !self.role_sent {
            self.role_sent = true;
            out.push_str(&self.chunk(json!({ "role": "assistant", "content": "" })));
        }
        out.push_str(&self.chunk(json!({
            "tool_calls": [{
                "index": tool_idx,
                "id": block.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "type": "function",
                "function": {
                    "name": block.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "arguments": "",
                },
            }]
        })));
        out
    }

    fn handle_block_delta(&mut self, data: &Value) -> String {
        let Some(delta) = data.get("delta") else {
            return String::new();
        };
        match delta.get("type").and_then(|v| v.as_str()) {
            Some("text_delta") => {
                let text = delta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if text.is_empty() {
                    return String::new();
                }
                self.chunk(json!({ "content": text }))
            }
            Some("thinking_delta") => {
                let t = delta.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                if t.is_empty() {
                    return String::new();
                }
                // reasoning_content 是社区事实标准（DeepSeek / vLLM / OpenRouter）；
                // 不识别的客户端会忽略未知字段
                self.chunk(json!({ "reasoning_content": t }))
            }
            Some("input_json_delta") => {
                let partial = delta
                    .get("partial_json")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if partial.is_empty() {
                    return String::new();
                }
                let block_index = data.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                // 未见过 content_block_start 的块不该出现 delta；保守跳过
                let Some(&tool_idx) = self.block_to_tool_idx.get(&block_index) else {
                    return String::new();
                };
                self.chunk(json!({
                    "tool_calls": [{
                        "index": tool_idx,
                        "function": { "arguments": partial },
                    }]
                }))
            }
            _ => String::new(),
        }
    }
}

/// 把 Anthropic SSE 响应体包成 OpenAI SSE 流
fn transcode_stream(
    body: Body,
    model: &str,
) -> impl Stream<Item = Result<Bytes, Infallible>> + use<> {
    let transcoder = OpenAiSseTranscoder::new(model);
    let data_stream = body.into_data_stream();

    stream::unfold(
        (data_stream, transcoder, false),
        |(mut data_stream, mut transcoder, done)| async move {
            if done {
                return None;
            }
            match data_stream.next().await {
                Some(Ok(chunk)) => {
                    let out = transcoder.feed(&chunk);
                    Some((Ok(Bytes::from(out)), (data_stream, transcoder, false)))
                }
                Some(Err(e)) => {
                    tracing::error!(error = %e, "读取上游 SSE 流失败，转码提前收尾");
                    let out = transcoder.eof();
                    Some((Ok(Bytes::from(out)), (data_stream, transcoder, true)))
                }
                None => {
                    let out = transcoder.eof();
                    Some((Ok(Bytes::from(out)), (data_stream, transcoder, true)))
                }
            }
        },
    )
}

// ============================ Handler ============================

/// `POST /v1/chat/completions`
///
/// 认证由 `v1_routes` 上挂载的 `auth_middleware` 完成（OpenAI 客户端发送的
/// `Authorization: Bearer` 已被 `common::auth::extract_api_key` 支持）。
pub async fn post_chat_completions(
    State(state): State<AppState>,
    identity: Option<Extension<ApiKeyContext>>,
    connect_info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let req: ChatCompletionRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "/v1/chat/completions 请求体反序列化失败");
            return openai_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!("Request body could not be parsed: {e}"),
            );
        }
    };

    let want_stream = req.stream;
    let model = req.model.clone();

    tracing::info!(
        model = %model,
        stream = %want_stream,
        message_count = %req.messages.len(),
        "Received POST /v1/chat/completions request"
    );

    // 1. OpenAI → Anthropic 请求翻译
    let anthropic_req = match openai_to_anthropic(&req, want_stream) {
        Ok(v) => v,
        Err(msg) => {
            return openai_error(StatusCode::BAD_REQUEST, "invalid_request_error", &msg);
        }
    };
    let anthropic_body = match serde_json::to_vec(&anthropic_req) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            tracing::error!(error = %e, "序列化 Anthropic 请求体失败");
            return openai_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                &format!("failed to build upstream request: {e}"),
            );
        }
    };

    // 2. 复用 Anthropic 全链路
    let inner = post_messages(
        State(state),
        identity,
        connect_info,
        headers,
        anthropic_body,
    )
    .await;

    let status = inner.status();

    // 上游非 2xx：Anthropic 错误体 {error:{type,message}} 与 OpenAI 同名同构，原样透传
    if !status.is_success() {
        return inner;
    }

    // 3. Anthropic → OpenAI 响应翻译
    if want_stream {
        let stream = transcode_stream(inner.into_body(), &model);
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .body(Body::from_stream(stream))
            .unwrap()
    } else {
        let body_bytes = match axum::body::to_bytes(inner.into_body(), usize::MAX).await {
            Ok(b) => b,
            Err(e) => {
                return openai_error(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    &format!("failed to read upstream response: {e}"),
                );
            }
        };
        let anthropic: Value = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(e) => {
                return openai_error(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    &format!("failed to parse upstream response: {e}"),
                );
            }
        };
        let parsed = parse_anthropic_message(&anthropic);
        (StatusCode::OK, Json(build_completion_json(&parsed, &model))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_from(v: Value) -> ChatCompletionRequest {
        serde_json::from_value(v).expect("请求体应可反序列化")
    }

    // ==================== 请求翻译 ====================

    #[test]
    fn system_and_developer_roles_are_hoisted_to_top_level_system() {
        let req = req_from(json!({
            "model": "claude-sonnet-4-5",
            "messages": [
                {"role": "system", "content": "rule A"},
                {"role": "developer", "content": "rule B"},
                {"role": "user", "content": "hi"},
            ]
        }));
        let out = openai_to_anthropic(&req, false).unwrap();

        assert_eq!(out["system"][0]["text"], json!("rule A"));
        assert_eq!(out["system"][1]["text"], json!("rule B"));
        // system 不应出现在 messages 里
        assert_eq!(out["messages"].as_array().unwrap().len(), 1);
        assert_eq!(out["messages"][0]["role"], json!("user"));
    }

    #[test]
    fn assistant_tool_calls_become_tool_use_blocks() {
        let req = req_from(json!({
            "model": "claude-sonnet-4-5",
            "messages": [
                {"role": "user", "content": "查天气"},
                {"role": "assistant", "content": "好", "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"北京\"}"}
                }]},
            ]
        }));
        let out = openai_to_anthropic(&req, false).unwrap();

        let blocks = out["messages"][1]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], json!("text"));
        assert_eq!(blocks[1]["type"], json!("tool_use"));
        assert_eq!(blocks[1]["id"], json!("call_1"));
        assert_eq!(blocks[1]["name"], json!("get_weather"));
        // arguments 字符串被解析成 JSON 对象
        assert_eq!(blocks[1]["input"]["city"], json!("北京"));
    }

    #[test]
    fn malformed_tool_arguments_degrade_to_empty_object() {
        let req = req_from(json!({
            "model": "claude-sonnet-4-5",
            "messages": [
                {"role": "user", "content": "x"},
                {"role": "assistant", "tool_calls": [{
                    "id": "call_1",
                    "function": {"name": "f", "arguments": "not-json{{"}
                }]},
            ]
        }));
        let out = openai_to_anthropic(&req, false).unwrap();
        assert_eq!(out["messages"][1]["content"][0]["input"], json!({}));
    }

    #[test]
    fn tool_role_becomes_tool_result_in_user_turn() {
        let req = req_from(json!({
            "model": "claude-sonnet-4-5",
            "messages": [
                {"role": "user", "content": "查天气"},
                {"role": "assistant", "tool_calls": [{
                    "id": "call_1", "function": {"name": "f", "arguments": "{}"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "晴 25℃"},
            ]
        }));
        let out = openai_to_anthropic(&req, false).unwrap();

        // tool 结果归入 user 轮（Anthropic 协议要求）
        let last = &out["messages"][2];
        assert_eq!(last["role"], json!("user"));
        assert_eq!(last["content"][0]["type"], json!("tool_result"));
        assert_eq!(last["content"][0]["tool_use_id"], json!("call_1"));
        assert_eq!(last["content"][0]["content"], json!("晴 25℃"));
    }

    #[test]
    fn adjacent_same_role_messages_are_merged() {
        let req = req_from(json!({
            "model": "claude-sonnet-4-5",
            "messages": [
                {"role": "user", "content": "第一句"},
                {"role": "user", "content": "第二句"},
                {"role": "assistant", "content": "回应"},
                {"role": "tool", "tool_call_id": "c1", "content": "结果1"},
                {"role": "tool", "tool_call_id": "c2", "content": "结果2"},
            ]
        }));
        let out = openai_to_anthropic(&req, false).unwrap();
        let msgs = out["messages"].as_array().unwrap();

        // user+user 合并成一轮（2 个 block），两条 tool 合并成一个 user 轮
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[1]["role"], json!("assistant"));
        assert_eq!(msgs[2]["role"], json!("user"));
        assert_eq!(msgs[2]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn data_url_image_becomes_base64_image_block() {
        let req = req_from(json!({
            "model": "claude-sonnet-4-5",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "看图"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAB"}},
                // 远程 URL 不支持，应被丢弃
                {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}},
            ]}]
        }));
        let out = openai_to_anthropic(&req, false).unwrap();
        let blocks = out["messages"][0]["content"].as_array().unwrap();

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1]["type"], json!("image"));
        assert_eq!(blocks[1]["source"]["media_type"], json!("image/png"));
        assert_eq!(blocks[1]["source"]["data"], json!("AAAB"));
    }

    #[test]
    fn max_completion_tokens_aliases_max_tokens_with_default_fallback() {
        // max_completion_tokens 作为别名生效
        let req = req_from(json!({
            "model": "m", "max_completion_tokens": 512,
            "messages": [{"role": "user", "content": "x"}]
        }));
        assert_eq!(
            openai_to_anthropic(&req, false).unwrap()["max_tokens"],
            json!(512)
        );

        // max_tokens 优先于 max_completion_tokens
        let req = req_from(json!({
            "model": "m", "max_tokens": 100, "max_completion_tokens": 512,
            "messages": [{"role": "user", "content": "x"}]
        }));
        assert_eq!(
            openai_to_anthropic(&req, false).unwrap()["max_tokens"],
            json!(100)
        );

        // 都不给则用默认值
        let req = req_from(json!({
            "model": "m", "messages": [{"role": "user", "content": "x"}]
        }));
        assert_eq!(
            openai_to_anthropic(&req, false).unwrap()["max_tokens"],
            json!(DEFAULT_MAX_TOKENS)
        );
    }

    #[test]
    fn tool_choice_maps_all_four_forms() {
        assert_eq!(
            convert_tool_choice(&json!("auto")),
            Some(json!({"type": "auto"}))
        );
        assert_eq!(
            convert_tool_choice(&json!("required")),
            Some(json!({"type": "any"}))
        );
        // none = 不使用工具，省略字段
        assert_eq!(convert_tool_choice(&json!("none")), None);
        assert_eq!(
            convert_tool_choice(&json!({"type": "function", "function": {"name": "f"}})),
            Some(json!({"type": "tool", "name": "f"}))
        );
    }

    #[test]
    fn unsupported_openai_params_are_ignored_not_rejected() {
        // temperature / top_p / n / penalties 等 Kiro 不支持的参数不应导致 400
        let req = req_from(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "x"}],
            "temperature": 0.7, "top_p": 0.9, "n": 1,
            "presence_penalty": 0.1, "frequency_penalty": 0.2,
            "stream_options": {"include_usage": true}
        }));
        assert!(openai_to_anthropic(&req, false).is_ok());
    }

    #[test]
    fn empty_messages_are_rejected() {
        let req = req_from(json!({ "model": "m", "messages": [] }));
        assert!(openai_to_anthropic(&req, false).is_err());

        // 只有 system 也算空（无对话轮）
        let req = req_from(json!({
            "model": "m", "messages": [{"role": "system", "content": "only system"}]
        }));
        assert!(openai_to_anthropic(&req, false).is_err());
    }

    #[test]
    fn tools_convert_and_stream_flag_passthrough() {
        let req = req_from(json!({
            "model": "m", "stream": true,
            "messages": [{"role": "user", "content": "x"}],
            "tools": [{"type": "function", "function": {
                "name": "get_weather",
                "description": "查天气",
                "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
            }}]
        }));
        let out = openai_to_anthropic(&req, true).unwrap();

        assert_eq!(out["stream"], json!(true));
        assert_eq!(out["tools"][0]["name"], json!("get_weather"));
        assert_eq!(out["tools"][0]["description"], json!("查天气"));
        assert_eq!(
            out["tools"][0]["input_schema"]["properties"]["city"]["type"],
            json!("string")
        );
    }

    // ==================== 非流式响应翻译 ====================

    #[test]
    fn plain_text_response_maps_to_stop() {
        let anthropic = json!({
            "content": [{"type": "text", "text": "你好"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 3}
        });
        let p = parse_anthropic_message(&anthropic);
        let out = build_completion_json(&p, "claude-sonnet-4-5");

        assert_eq!(out["object"], json!("chat.completion"));
        assert_eq!(out["choices"][0]["message"]["content"], json!("你好"));
        assert_eq!(out["choices"][0]["message"]["role"], json!("assistant"));
        assert_eq!(out["choices"][0]["finish_reason"], json!("stop"));
        assert!(out["id"].as_str().unwrap().starts_with("chatcmpl-"));
    }

    #[test]
    fn tool_use_response_sets_tool_calls_and_null_content() {
        let anthropic = json!({
            "content": [{
                "type": "tool_use", "id": "toolu_1", "name": "get_weather",
                "input": {"city": "北京"}
            }],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 8}
        });
        let p = parse_anthropic_message(&anthropic);
        let out = build_completion_json(&p, "m");

        assert_eq!(out["choices"][0]["finish_reason"], json!("tool_calls"));
        // 纯工具调用轮 content 必须是 null
        assert_eq!(out["choices"][0]["message"]["content"], Value::Null);
        let tc = &out["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["id"], json!("toolu_1"));
        assert_eq!(tc["type"], json!("function"));
        assert_eq!(tc["function"]["name"], json!("get_weather"));
        // arguments 必须是 JSON 字符串而非对象
        let args = tc["function"]["arguments"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(args).unwrap()["city"],
            json!("北京")
        );
    }

    #[test]
    fn finish_reason_mapping_covers_length_and_thinking() {
        assert_eq!(map_finish_reason("max_tokens", false), "length");
        assert_eq!(
            map_finish_reason("model_context_window_exceeded", false),
            "length"
        );
        assert_eq!(map_finish_reason("tool_use", false), "tool_calls");
        assert_eq!(map_finish_reason("end_turn", false), "stop");
        // 有 tool_calls 时即便 stop_reason 是 end_turn 也判为 tool_calls
        assert_eq!(map_finish_reason("end_turn", true), "tool_calls");

        // thinking 块渲染为 reasoning_content
        let anthropic = json!({
            "content": [
                {"type": "thinking", "thinking": "让我想想"},
                {"type": "text", "text": "答案"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let out = build_completion_json(&parse_anthropic_message(&anthropic), "m");
        assert_eq!(
            out["choices"][0]["message"]["reasoning_content"],
            json!("让我想想")
        );
        assert_eq!(out["choices"][0]["message"]["content"], json!("答案"));
    }

    #[test]
    fn usage_includes_totals_and_cached_tokens() {
        let anthropic = json!({
            "content": [{"type": "text", "text": "x"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 100, "output_tokens": 20,
                "cache_read_input_tokens": 80
            }
        });
        let out = build_completion_json(&parse_anthropic_message(&anthropic), "m");
        let usage = &out["usage"];

        assert_eq!(usage["prompt_tokens"], json!(100));
        assert_eq!(usage["completion_tokens"], json!(20));
        assert_eq!(usage["total_tokens"], json!(120));
        assert_eq!(usage["prompt_tokens_details"]["cached_tokens"], json!(80));

        // 无 cache 命中时不输出 prompt_tokens_details
        let anthropic = json!({
            "content": [{"type": "text", "text": "x"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let out = build_completion_json(&parse_anthropic_message(&anthropic), "m");
        assert!(out["usage"].get("prompt_tokens_details").is_none());
    }

    // ==================== 流式转码 ====================

    /// 把 Anthropic SSE 帧拼成字节串（复刻 stream.rs::to_sse_string 格式）
    fn frame(event: &str, data: Value) -> String {
        format!("event: {}\ndata: {}\n\n", event, data)
    }

    /// 从转码输出里取出所有 `data:` 行解析成 JSON（跳过 [DONE]）
    fn chunks_of(out: &str) -> Vec<Value> {
        out.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|s| *s != "[DONE]")
            .map(|s| serde_json::from_str(s).expect("chunk 应是合法 JSON"))
            .collect()
    }

    #[test]
    fn message_start_emits_role_chunk_once() {
        let mut t = OpenAiSseTranscoder::new("m");
        let out = t.feed(frame("message_start", json!({"type": "message_start"})).as_bytes());
        let chunks = chunks_of(&out);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0]["object"], json!("chat.completion.chunk"));
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], json!("assistant"));

        // 重复的 message_start 不再发 role 帧
        let out2 = t.feed(frame("message_start", json!({"type": "message_start"})).as_bytes());
        assert!(chunks_of(&out2).is_empty());
    }

    #[test]
    fn text_delta_streams_incrementally() {
        let mut t = OpenAiSseTranscoder::new("m");
        t.feed(frame("message_start", json!({"type": "message_start"})).as_bytes());

        let mut got = String::new();
        for piece in ["你", "好", "世界"] {
            let out = t.feed(
                frame(
                    "content_block_delta",
                    json!({"index": 0, "delta": {"type": "text_delta", "text": piece}}),
                )
                .as_bytes(),
            );
            let chunks = chunks_of(&out);
            assert_eq!(chunks.len(), 1, "每个 text_delta 应产出恰好 1 个 chunk");
            got.push_str(
                chunks[0]["choices"][0]["delta"]["content"]
                    .as_str()
                    .unwrap(),
            );
        }
        // 逐帧转码而非缓冲全量
        assert_eq!(got, "你好世界");
    }

    #[test]
    fn tool_call_index_starts_at_zero_skipping_text_blocks() {
        // 回归重点：Anthropic block index 把 text 块也计入，OpenAI 的
        // tool_calls[].index 必须仅在工具间从 0 递增
        let mut t = OpenAiSseTranscoder::new("m");
        t.feed(frame("message_start", json!({"type": "message_start"})).as_bytes());

        // block 0 = text
        t.feed(
            frame(
                "content_block_delta",
                json!({"index": 0, "delta": {"type": "text_delta", "text": "先说话"}}),
            )
            .as_bytes(),
        );

        // block 1 = 第一个 tool_use → OpenAI index 必须是 0
        let out = t.feed(
            frame(
                "content_block_start",
                json!({"index": 1, "content_block": {
                    "type": "tool_use", "id": "toolu_a", "name": "fa", "input": {}
                }}),
            )
            .as_bytes(),
        );
        let tc = &chunks_of(&out)[0]["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(
            tc["index"],
            json!(0),
            "首个工具的 index 必须是 0，不能沿用 block index 1"
        );
        assert_eq!(tc["id"], json!("toolu_a"));
        assert_eq!(tc["function"]["name"], json!("fa"));

        // block 2 = 第二个 tool_use → OpenAI index 必须是 1
        let out = t.feed(
            frame(
                "content_block_start",
                json!({"index": 2, "content_block": {
                    "type": "tool_use", "id": "toolu_b", "name": "fb", "input": {}
                }}),
            )
            .as_bytes(),
        );
        let tc = &chunks_of(&out)[0]["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(tc["index"], json!(1));

        // 两个工具的 arguments 增量各自对齐到正确 index
        let out = t.feed(
            frame(
                "content_block_delta",
                json!({"index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"k\""}}),
            )
            .as_bytes(),
        );
        let tc = &chunks_of(&out)[0]["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(
            tc["index"],
            json!(1),
            "block 2 的 delta 应映射回工具 index 1"
        );
    }

    #[test]
    fn input_json_delta_accumulates_arguments_incrementally() {
        let mut t = OpenAiSseTranscoder::new("m");
        t.feed(
            frame(
                "content_block_start",
                json!({"index": 0, "content_block": {
                    "type": "tool_use", "id": "toolu_1", "name": "f", "input": {}
                }}),
            )
            .as_bytes(),
        );

        let mut args = String::new();
        for piece in ["{\"city\"", ":\"北京\"", "}"] {
            let out = t.feed(
                frame(
                    "content_block_delta",
                    json!({"index": 0, "delta": {
                        "type": "input_json_delta", "partial_json": piece
                    }}),
                )
                .as_bytes(),
            );
            let chunks = chunks_of(&out);
            assert_eq!(chunks.len(), 1);
            args.push_str(
                chunks[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]
                    .as_str()
                    .unwrap(),
            );
        }
        // 客户端拼接后应得到完整合法 JSON
        assert_eq!(
            serde_json::from_str::<Value>(&args).unwrap()["city"],
            json!("北京")
        );
    }

    #[test]
    fn final_chunk_carries_usage_and_terminates_with_done() {
        let mut t = OpenAiSseTranscoder::new("m");
        t.feed(frame("message_start", json!({"type": "message_start"})).as_bytes());
        t.feed(
            frame(
                "content_block_delta",
                json!({"index": 0, "delta": {"type": "text_delta", "text": "hi"}}),
            )
            .as_bytes(),
        );
        t.feed(
            frame(
                "message_delta",
                json!({
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                    "usage": {
                        "input_tokens": 42, "output_tokens": 7,
                        "cache_read_input_tokens": 30
                    }
                }),
            )
            .as_bytes(),
        );
        // message_delta 本身不产出 chunk，usage 挂到结束帧
        let out = t.feed(frame("message_stop", json!({"type": "message_stop"})).as_bytes());

        assert!(
            out.trim_end().ends_with("data: [DONE]"),
            "必须以 [DONE] 收尾"
        );
        let chunks = chunks_of(&out);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0]["choices"][0]["finish_reason"], json!("stop"));
        assert_eq!(chunks[0]["usage"]["prompt_tokens"], json!(42));
        assert_eq!(chunks[0]["usage"]["completion_tokens"], json!(7));
        assert_eq!(chunks[0]["usage"]["total_tokens"], json!(49));
        assert_eq!(
            chunks[0]["usage"]["prompt_tokens_details"]["cached_tokens"],
            json!(30)
        );
    }

    #[test]
    fn sse_frame_split_across_chunks_is_reassembled() {
        // 回归重点：一个 TCP chunk 可能只含半帧，必须跨 chunk 拼接
        let mut t = OpenAiSseTranscoder::new("m");
        let full = frame(
            "content_block_delta",
            json!({"index": 0, "delta": {"type": "text_delta", "text": "跨块文本"}}),
        );
        let bytes = full.as_bytes();
        let split_at = bytes.len() / 2;

        // 前半帧不应产出任何 chunk
        let out1 = t.feed(&bytes[..split_at]);
        assert!(chunks_of(&out1).is_empty(), "半帧不应产出 chunk");

        // 后半帧到达后完整解析
        let out2 = t.feed(&bytes[split_at..]);
        let chunks = chunks_of(&out2);
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0]["choices"][0]["delta"]["content"],
            json!("跨块文本")
        );
    }

    #[test]
    fn chunk_split_mid_utf8_character_does_not_corrupt_text() {
        // 回归重点：TCP chunk 边界可能落在多字节字符中间。若按 chunk 做
        // from_utf8_lossy，被切断的字节会变成 U+FFFD，CJK 文本永久损坏。
        // 转码器必须缓冲原始字节、只在切出完整帧后解码。
        let mut t = OpenAiSseTranscoder::new("m");
        let full = frame(
            "content_block_delta",
            json!({"index": 0, "delta": {"type": "text_delta", "text": "中文内容"}}),
        );
        let bytes = full.as_bytes();

        // 定位 "中" 的起始字节，在其 3 字节序列正中间切断
        let needle = "中".as_bytes();
        let pos = bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("帧内应含该字符");
        let split_at = pos + 1;

        let out1 = t.feed(&bytes[..split_at]);
        assert!(chunks_of(&out1).is_empty());

        let out2 = t.feed(&bytes[split_at..]);
        let chunks = chunks_of(&out2);
        assert_eq!(chunks.len(), 1);
        // 文本必须完好，不含替换字符
        let content = chunks[0]["choices"][0]["delta"]["content"]
            .as_str()
            .unwrap();
        assert_eq!(content, "中文内容");
        assert!(!content.contains('\u{FFFD}'), "不应出现 UTF-8 替换字符");
    }

    #[test]
    fn multiple_frames_in_one_chunk_all_emit() {
        let mut t = OpenAiSseTranscoder::new("m");
        let combined = format!(
            "{}{}{}",
            frame("message_start", json!({"type": "message_start"})),
            frame(
                "content_block_delta",
                json!({"index": 0, "delta": {"type": "text_delta", "text": "a"}})
            ),
            frame(
                "content_block_delta",
                json!({"index": 0, "delta": {"type": "text_delta", "text": "b"}})
            ),
        );
        let chunks = chunks_of(&t.feed(combined.as_bytes()));

        // role 帧 + 2 个文本帧
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], json!("assistant"));
        assert_eq!(chunks[1]["choices"][0]["delta"]["content"], json!("a"));
        assert_eq!(chunks[2]["choices"][0]["delta"]["content"], json!("b"));
    }

    #[test]
    fn thinking_delta_maps_to_reasoning_content() {
        let mut t = OpenAiSseTranscoder::new("m");
        let out = t.feed(
            frame(
                "content_block_delta",
                json!({"index": 0, "delta": {"type": "thinking_delta", "thinking": "推理中"}}),
            )
            .as_bytes(),
        );
        let chunks = chunks_of(&out);
        assert_eq!(
            chunks[0]["choices"][0]["delta"]["reasoning_content"],
            json!("推理中")
        );
    }

    #[test]
    fn ping_frames_are_dropped() {
        let mut t = OpenAiSseTranscoder::new("m");
        // ping 在 OpenAI 协议无对应语义，不应泄漏给客户端
        let out = t.feed(frame("ping", json!({"type": "ping"})).as_bytes());
        assert!(chunks_of(&out).is_empty());
        assert!(!out.contains("ping"));
    }

    #[test]
    fn eof_without_message_stop_still_terminates() {
        // 上游异常断流（无 message_stop）时也必须给客户端 [DONE]，
        // 否则 OpenAI SDK 会一直等待
        let mut t = OpenAiSseTranscoder::new("m");
        t.feed(frame("message_start", json!({"type": "message_start"})).as_bytes());
        let out = t.eof();

        assert!(out.trim_end().ends_with("data: [DONE]"));
        let chunks = chunks_of(&out);
        assert_eq!(chunks[0]["choices"][0]["finish_reason"], json!("stop"));
    }

    #[test]
    fn upstream_error_event_surfaces_and_terminates() {
        let mut t = OpenAiSseTranscoder::new("m");
        t.feed(frame("message_start", json!({"type": "message_start"})).as_bytes());
        let out = t.feed(
            frame(
                "error",
                json!({"type": "error", "error": {"type": "api_error", "message": "上游炸了"}}),
            )
            .as_bytes(),
        );

        assert!(out.contains("上游炸了"));
        assert!(out.trim_end().ends_with("data: [DONE]"));

        // 收尾后不再重复发结束帧
        assert!(t.finish().is_empty());
    }

    #[test]
    fn tool_use_first_block_still_emits_role_chunk() {
        // 工具块可能是响应首个块，此时 role 帧尚未发出，必须补发
        let mut t = OpenAiSseTranscoder::new("m");
        let out = t.feed(
            frame(
                "content_block_start",
                json!({"index": 0, "content_block": {
                    "type": "tool_use", "id": "toolu_1", "name": "f", "input": {}
                }}),
            )
            .as_bytes(),
        );
        let chunks = chunks_of(&out);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], json!("assistant"));
        assert_eq!(
            chunks[1]["choices"][0]["delta"]["tool_calls"][0]["index"],
            json!(0)
        );
    }
}
