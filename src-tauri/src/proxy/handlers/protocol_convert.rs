//! Pure-function module for Anthropic ↔ OpenAI protocol conversion.
//!
//! All functions here are stateless — no network, no IO, no shared state.
//! This makes them trivial to unit-test.

use serde_json::{json, Value};

// ============================================================================
// Request conversion: Anthropic → OpenAI
// ============================================================================

/// Convert an Anthropic `/v1/messages` request body to an OpenAI `/v1/chat/completions` body.
///
/// Full conversion following the reference claude-code-router project:
/// - `system` (top-level) → system message
/// - user messages: tool_result → role:"tool"; image → image_url; text pass-through
/// - assistant messages: tool_use → tool_calls; thinking → thinking extension
/// - `tools[].input_schema` → `tools[].function.parameters`
/// - `tool_choice` → format mapping
/// - `thinking.budget_tokens` → `reasoning` extension
pub fn anthropic_to_openai_request(anthropic_body: &Value) -> Value {
    let mut openai = json!({});

    // Model — pass through
    if let Some(model) = anthropic_body.get("model") {
        openai["model"] = model.clone();
    }

    // Build messages array
    let mut messages: Vec<Value> = Vec::new();

    // System prompt: Anthropic uses a top-level `system` field
    if let Some(system) = anthropic_body.get("system") {
        match system {
            Value::String(s) => {
                messages.push(json!({"role": "system", "content": s}));
            }
            Value::Array(parts) => {
                let content_parts: Vec<Value> = parts
                    .iter()
                    .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|p| p.get("text").cloned().map(|t| {
                        let mut part = json!({"type": "text", "text": t});
                        if let Some(cc) = p.get("cache_control") {
                            part["cache_control"] = cc.clone();
                        }
                        part
                    }))
                    .collect();
                if !content_parts.is_empty() {
                    messages.push(json!({"role": "system", "content": content_parts}));
                }
            }
            _ => {}
        }
    }

    // Messages — role-aware conversion
    if let Some(Value::Array(msgs)) = anthropic_body.get("messages") {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");

            match msg.get("content") {
                // String content — pass through for any role
                Some(Value::String(s)) => {
                    messages.push(json!({"role": role, "content": s}));
                }
                // Array content — role-specific handling
                Some(Value::Array(blocks)) => {
                    if role == "user" {
                        convert_user_message(blocks, &mut messages);
                    } else if role == "assistant" {
                        convert_assistant_message(blocks, &mut messages);
                    } else {
                        // Other roles: just concatenate text
                        let text = extract_text(blocks);
                        messages.push(json!({"role": role, "content": text}));
                    }
                }
                // Null or missing content
                _ => {
                    messages.push(json!({"role": role, "content": ""}));
                }
            }
        }
    }

    openai["messages"] = Value::Array(messages);

    // Direct mappings
    for key in &["max_tokens", "stream", "temperature", "top_p"] {
        if let Some(v) = anthropic_body.get(*key) {
            openai[*key] = v.clone();
        }
    }

    // stop_sequences → stop
    if let Some(stops) = anthropic_body.get("stop_sequences") {
        openai["stop"] = stops.clone();
    }

    // tools[].input_schema → tools[].function.parameters
    if let Some(Value::Array(tools)) = anthropic_body.get("tools") {
        let openai_tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                        "description": tool.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                        "parameters": tool.get("input_schema").cloned().unwrap_or(json!({}))
                    }
                })
            })
            .collect();
        if !openai_tools.is_empty() {
            openai["tools"] = Value::Array(openai_tools);
        }
    }

    // tool_choice mapping
    if let Some(tc) = anthropic_body.get("tool_choice") {
        let tc_type = tc.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match tc_type {
            "tool" => {
                // Specific tool → function choice
                let name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                openai["tool_choice"] = json!({
                    "type": "function",
                    "function": {"name": name}
                });
            }
            "any" => {
                openai["tool_choice"] = json!("required");
            }
            "auto" => {
                openai["tool_choice"] = json!("auto");
            }
            "none" => {
                openai["tool_choice"] = json!("none");
            }
            _ => {
                openai["tool_choice"] = tc.clone();
            }
        }
    }

    // thinking → reasoning extension (for providers that support it)
    if let Some(thinking) = anthropic_body.get("thinking") {
        if thinking.get("type").and_then(|t| t.as_str()) == Some("enabled") {
            let budget = thinking
                .get("budget_tokens")
                .and_then(|b| b.as_u64())
                .unwrap_or(0);
            let effort = budget_to_effort(budget);
            openai["reasoning"] = json!({
                "effort": effort,
                "enabled": true
            });
        }
    }

    openai
}

/// Convert a user message's content blocks.
/// - tool_result blocks → independent role:"tool" messages
/// - text + image blocks → single user message with content array
fn convert_user_message(blocks: &[Value], messages: &mut Vec<Value>) {
    // First: extract tool_result blocks as independent messages
    for block in blocks {
        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
            if let Some(tool_use_id) = block.get("tool_use_id").and_then(|id| id.as_str()) {
                let content = match block.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(parts)) => {
                        // tool_result content can be an array of content blocks
                        serde_json::to_string(parts).unwrap_or_default()
                    }
                    Some(other) => serde_json::to_string(other).unwrap_or_default(),
                    None => String::new(),
                };
                let mut tool_msg = json!({
                    "role": "tool",
                    "content": content,
                    "tool_call_id": tool_use_id
                });
                if let Some(cc) = block.get("cache_control") {
                    tool_msg["cache_control"] = cc.clone();
                }
                messages.push(tool_msg);
            }
        }
    }

    // Second: collect text + image blocks as user message content
    let text_and_media: Vec<Value> = blocks
        .iter()
        .filter_map(|block| {
            let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match btype {
                "text" => {
                    if block.get("text").and_then(|t| t.as_str()).is_some() {
                        Some(block.clone())
                    } else {
                        None
                    }
                }
                "image" => {
                    // Convert Anthropic image format to OpenAI image_url format
                    if let Some(source) = block.get("source") {
                        let source_type =
                            source.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        let url = if source_type == "base64" {
                            let data =
                                source.get("data").and_then(|d| d.as_str()).unwrap_or("");
                            let media_type = source
                                .get("media_type")
                                .and_then(|m| m.as_str())
                                .unwrap_or("image/png");
                            format!("data:{};base64,{}", media_type, data)
                        } else {
                            source
                                .get("url")
                                .and_then(|u| u.as_str())
                                .unwrap_or("")
                                .to_string()
                        };
                        Some(json!({
                            "type": "image_url",
                            "image_url": {"url": url}
                        }))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        })
        .collect();

    if !text_and_media.is_empty() {
        messages.push(json!({"role": "user", "content": text_and_media}));
    }
}

/// Convert an assistant message's content blocks.
/// - text blocks → content string
/// - tool_use blocks → tool_calls array
/// - thinking blocks → thinking extension field
fn convert_assistant_message(blocks: &[Value], messages: &mut Vec<Value>) {
    let mut assistant_msg = json!({"role": "assistant", "content": ""});

    // Text content
    let text_parts: Vec<&str> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect();
    if !text_parts.is_empty() {
        assistant_msg["content"] = Value::String(text_parts.join("\n"));
    }

    // tool_use → tool_calls
    let tool_calls: Vec<Value> = blocks
        .iter()
        .filter(|b| {
            b.get("type").and_then(|t| t.as_str()) == Some("tool_use") && b.get("id").is_some()
        })
        .map(|b| {
            let input = b.get("input").cloned().unwrap_or(json!({}));
            let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
            json!({
                "id": b.get("id").unwrap(),
                "type": "function",
                "function": {
                    "name": b.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                    "arguments": arguments
                }
            })
        })
        .collect();
    if !tool_calls.is_empty() {
        assistant_msg["tool_calls"] = Value::Array(tool_calls);
    }

    // thinking → thinking extension
    if let Some(thinking_part) = blocks.iter().find(|b| {
        b.get("type").and_then(|t| t.as_str()) == Some("thinking") && b.get("signature").is_some()
    }) {
        assistant_msg["thinking"] = json!({
            "content": thinking_part.get("thinking").and_then(|t| t.as_str()).unwrap_or(""),
            "signature": thinking_part.get("signature").and_then(|s| s.as_str()).unwrap_or("")
        });
    }

    messages.push(assistant_msg);
}

/// Extract and concatenate text from content blocks.
fn extract_text(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("")
}

/// Map thinking budget_tokens to effort level string.
fn budget_to_effort(budget: u64) -> &'static str {
    if budget == 0 {
        "none"
    } else if budget < 4000 {
        "low"
    } else if budget < 16000 {
        "medium"
    } else {
        "high"
    }
}

// ============================================================================
// Response conversion: OpenAI → Anthropic (non-streaming)
// ============================================================================

/// Convert an OpenAI `/v1/chat/completions` response to Anthropic `/v1/messages` format.
///
/// Handles:
/// - text content → content[].type="text"
/// - tool_calls → content[].type="tool_use"
/// - thinking → content[].type="thinking"
/// - finish_reason mapping
/// - usage with cache_read_input_tokens
pub fn openai_to_anthropic_response(openai_body: &Value, model: &str) -> Value {
    let id = openai_body
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| format!("msg_{}", s))
        .unwrap_or_else(|| format!("msg_{}", uuid_simple()));

    let choice = openai_body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());

    let message = choice.and_then(|c| c.get("message"));

    // Build content array — order: thinking, text, tool_use (matching reference project)
    let mut content: Vec<Value> = Vec::new();

    // thinking → content[].type="thinking"
    if let Some(thinking) = message.and_then(|m| m.get("thinking")) {
        if let Some(thinking_content) = thinking.get("content").and_then(|c| c.as_str()) {
            content.push(json!({
                "type": "thinking",
                "thinking": thinking_content,
                "signature": thinking.get("signature").and_then(|s| s.as_str()).unwrap_or("")
            }));
        }
    }

    // text content
    if let Some(text) = message.and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
    }

    // tool_calls → content[].type="tool_use"
    if let Some(Value::Array(tool_calls)) = message.and_then(|m| m.get("tool_calls")) {
        for tc in tool_calls {
            let func = tc.get("function");
            let arguments_str = func
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("{}");

            // Parse arguments JSON string to object
            let parsed_input: Value = serde_json::from_str(arguments_str).unwrap_or_else(|_| {
                json!({"text": arguments_str})
            });

            content.push(json!({
                "type": "tool_use",
                "id": tc.get("id").and_then(|id| id.as_str()).unwrap_or(""),
                "name": func.and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or(""),
                "input": parsed_input
            }));
        }
    }

    // If no content at all, add empty text block
    if content.is_empty() {
        content.push(json!({"type": "text", "text": ""}));
    }

    let stop_reason = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|r| r.as_str())
        .map(map_finish_reason)
        .unwrap_or("end_turn");

    let usage = openai_body.get("usage");
    let prompt_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cached_tokens = usage
        .and_then(|u| u.get("prompt_tokens_details"))
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": prompt_tokens.saturating_sub(cached_tokens),
            "output_tokens": output_tokens,
            "cache_read_input_tokens": cached_tokens
        }
    })
}

/// Map OpenAI `finish_reason` to Anthropic `stop_reason`.
fn map_finish_reason(reason: &str) -> &'static str {
    match reason {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "content_filter" => "end_turn",
        "tool_calls" | "function_call" => "tool_use",
        _ => "end_turn",
    }
}

// ============================================================================
// Streaming conversion: OpenAI SSE → Anthropic SSE
// ============================================================================

/// State machine for converting OpenAI streaming chunks to Anthropic SSE events.
///
/// Tracks content block indices, tool call mappings, and thinking state
/// to produce correct Anthropic SSE event sequences.
pub struct StreamConverter {
    model: String,
    started: bool,
    finished: bool,
    /// Monotonically increasing content block index allocator
    content_index: usize,
    /// Index of the currently open content block (-1 = none open)
    current_content_block: i64,
    /// Whether a text content block has been started
    text_block_started: bool,
    /// Whether a thinking content block has been started
    thinking_started: bool,
    /// Map: OpenAI tool_call index → Anthropic content block index
    tool_call_index_map: std::collections::HashMap<u64, usize>,
    /// Deferred message_delta to send at close (carries usage + stop_reason)
    stop_reason_delta: Option<Value>,
}

impl StreamConverter {
    pub fn new(model: String) -> Self {
        Self {
            model,
            started: false,
            finished: false,
            content_index: 0,
            current_content_block: -1,
            text_block_started: false,
            thinking_started: false,
            tool_call_index_map: std::collections::HashMap::new(),
            stop_reason_delta: None,
        }
    }

    /// Allocate the next content block index.
    fn next_block_index(&mut self) -> usize {
        let idx = self.content_index;
        self.content_index += 1;
        idx
    }

    /// Emit a content_block_stop for the currently open block, if any.
    fn close_current_block(&mut self, events: &mut Vec<String>) {
        if self.current_content_block >= 0 {
            events.push(format!(
                "event: content_block_stop\ndata: {}\n\n",
                json!({"type": "content_block_stop", "index": self.current_content_block})
            ));
            self.current_content_block = -1;
        }
    }

    /// Process a single OpenAI SSE data line (the part after "data: ").
    /// Returns zero or more Anthropic SSE event strings to send to the client.
    pub fn process_chunk(&mut self, data: &str) -> Vec<String> {
        let data = data.trim();

        if data == "[DONE]" {
            return self.finalize();
        }

        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        // Handle error chunks from upstream
        if chunk.get("error").is_some() {
            let err_event = format!(
                "event: error\ndata: {}\n\n",
                json!({
                    "type": "error",
                    "message": {
                        "type": "api_error",
                        "message": chunk.get("error").unwrap().to_string()
                    }
                })
            );
            return vec![err_event];
        }

        // Accumulate usage whenever we see it
        if let Some(usage) = chunk.get("usage") {
            let prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let cached_tokens = usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);

            let usage_val = json!({
                "input_tokens": prompt_tokens.saturating_sub(cached_tokens),
                "output_tokens": output_tokens,
                "cache_read_input_tokens": cached_tokens
            });

            if let Some(ref mut delta) = self.stop_reason_delta {
                delta["usage"] = usage_val;
            } else {
                self.stop_reason_delta = Some(json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                    "usage": usage_val
                }));
            }
        }

        let choice = match chunk.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) {
            Some(c) => c,
            None => return vec![],
        };

        let delta = match choice.get("delta") {
            Some(d) => d,
            None => {
                // No delta but might have finish_reason
                if choice.get("finish_reason").and_then(|r| r.as_str()).is_some() {
                    // Fall through to finish_reason handling below
                    &Value::Object(serde_json::Map::new())
                } else {
                    return vec![];
                }
            }
        };

        let mut events = Vec::new();

        // First chunk → send message_start
        if !self.started && !self.finished {
            self.started = true;
            let model_name = chunk
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or(&self.model);
            events.push(format!(
                "event: message_start\ndata: {}\n\n",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": format!("msg_{}", uuid_simple()),
                        "type": "message",
                        "role": "assistant",
                        "model": model_name,
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {"input_tokens": 0, "output_tokens": 0}
                    }
                })
            ));
        }

        if self.finished {
            return events;
        }

        // === Thinking delta ===
        if let Some(thinking) = delta.get("thinking") {
            if !self.thinking_started {
                let idx = self.next_block_index();
                events.push(format!(
                    "event: content_block_start\ndata: {}\n\n",
                    json!({
                        "type": "content_block_start",
                        "index": idx,
                        "content_block": {"type": "thinking", "thinking": ""}
                    })
                ));
                self.current_content_block = idx as i64;
                self.thinking_started = true;
            }

            if let Some(sig) = thinking.get("signature").and_then(|s| s.as_str()) {
                if !sig.is_empty() {
                    events.push(format!(
                        "event: content_block_delta\ndata: {}\n\n",
                        json!({
                            "type": "content_block_delta",
                            "index": self.current_content_block,
                            "delta": {"type": "signature_delta", "signature": sig}
                        })
                    ));
                    // Signature ends the thinking block
                    self.close_current_block(&mut events);
                }
            } else if let Some(content) = thinking.get("content").and_then(|c| c.as_str()) {
                if !content.is_empty() {
                    events.push(format!(
                        "event: content_block_delta\ndata: {}\n\n",
                        json!({
                            "type": "content_block_delta",
                            "index": self.current_content_block,
                            "delta": {"type": "thinking_delta", "thinking": content}
                        })
                    ));
                }
            }
        }

        // === Text content delta ===
        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            // Close non-text block if open (e.g. thinking just ended)
            if self.current_content_block >= 0 && !self.text_block_started {
                self.close_current_block(&mut events);
            }

            if !self.text_block_started {
                self.text_block_started = true;
                let idx = self.next_block_index();
                events.push(format!(
                    "event: content_block_start\ndata: {}\n\n",
                    json!({
                        "type": "content_block_start",
                        "index": idx,
                        "content_block": {"type": "text", "text": ""}
                    })
                ));
                self.current_content_block = idx as i64;
            }

            if !content.is_empty() {
                events.push(format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    json!({
                        "type": "content_block_delta",
                        "index": self.current_content_block,
                        "delta": {"type": "text_delta", "text": content}
                    })
                ));
            }
        }

        // === Tool calls delta ===
        if let Some(Value::Array(tool_calls)) = delta.get("tool_calls") {
            for tc in tool_calls {
                let tc_index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);

                if !self.tool_call_index_map.contains_key(&tc_index) {
                    // New tool call: close any open block, start a new tool_use block
                    self.close_current_block(&mut events);

                    let block_idx = self.next_block_index();
                    self.tool_call_index_map.insert(tc_index, block_idx);

                    let tc_id = tc
                        .get("id")
                        .and_then(|id| id.as_str())
                        .map(String::from)
                        .unwrap_or_else(|| format!("call_{}_{}", uuid_simple(), tc_index));
                    let tc_name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");

                    events.push(format!(
                        "event: content_block_start\ndata: {}\n\n",
                        json!({
                            "type": "content_block_start",
                            "index": block_idx,
                            "content_block": {
                                "type": "tool_use",
                                "id": tc_id,
                                "name": tc_name,
                                "input": {}
                            }
                        })
                    ));
                    self.current_content_block = block_idx as i64;
                }

                // Stream tool arguments as input_json_delta
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                {
                    if !args.is_empty() {
                        let block_idx = self.tool_call_index_map[&tc_index];
                        events.push(format!(
                            "event: content_block_delta\ndata: {}\n\n",
                            json!({
                                "type": "content_block_delta",
                                "index": block_idx,
                                "delta": {
                                    "type": "input_json_delta",
                                    "partial_json": args
                                }
                            })
                        ));
                    }
                }
            }
        }

        // === finish_reason ===
        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            // Close any remaining open content block
            self.close_current_block(&mut events);

            let anthropic_stop = map_finish_reason(reason);

            let output_tokens = chunk
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let prompt_tokens = chunk
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cached_tokens = chunk
                .get("usage")
                .and_then(|u| u.get("prompt_tokens_details"))
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            self.stop_reason_delta = Some(json!({
                "type": "message_delta",
                "delta": {"stop_reason": anthropic_stop, "stop_sequence": null},
                "usage": {
                    "input_tokens": prompt_tokens.saturating_sub(cached_tokens),
                    "output_tokens": output_tokens,
                    "cache_read_input_tokens": cached_tokens
                }
            }));

            self.finished = true;
        }

        events
    }

    /// Finalize the stream — produce closing events.
    fn finalize(&mut self) -> Vec<String> {
        let mut events = Vec::new();

        if !self.started {
            // Never got any data — send a minimal message
            events.push(format!(
                "event: message_start\ndata: {}\n\n",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": format!("msg_{}", uuid_simple()),
                        "type": "message",
                        "role": "assistant",
                        "model": self.model,
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {"input_tokens": 0, "output_tokens": 0}
                    }
                })
            ));
            self.started = true;
        }

        // Close any remaining open content block
        self.close_current_block(&mut events);

        // Send message_delta with stop_reason + usage
        if let Some(delta) = self.stop_reason_delta.take() {
            events.push(format!("event: message_delta\ndata: {}\n\n", delta));
        } else {
            events.push(format!(
                "event: message_delta\ndata: {}\n\n",
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                    "usage": {"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0}
                })
            ));
        }

        // Send message_stop
        events.push(format!(
            "event: message_stop\ndata: {}\n\n",
            json!({"type": "message_stop"})
        ));

        events
    }
}

/// Generate a simple pseudo-unique ID (not cryptographic, just for message IDs).
fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", ts)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- Request conversion ---

    #[test]
    fn basic_request_conversion() {
        let anthropic = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let openai = anthropic_to_openai_request(&anthropic);

        assert_eq!(openai["model"], "claude-sonnet-4-20250514");
        assert_eq!(openai["max_tokens"], 1024);
        let msgs = openai["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "Hello");
    }

    #[test]
    fn system_prompt_becomes_system_message() {
        let anthropic = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "system": "You are a helpful assistant.",
            "messages": [
                {"role": "user", "content": "Hi"}
            ]
        });

        let openai = anthropic_to_openai_request(&anthropic);
        let msgs = openai["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are a helpful assistant.");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn user_text_blocks_become_content_array() {
        let anthropic = json!({
            "model": "test",
            "max_tokens": 10,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Hello "},
                        {"type": "text", "text": "world"}
                    ]
                }
            ]
        });

        let openai = anthropic_to_openai_request(&anthropic);
        let msgs = openai["messages"].as_array().unwrap();
        // User text blocks should produce a user message with content array
        assert_eq!(msgs[0]["role"], "user");
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
    }

    #[test]
    fn tool_result_becomes_tool_role_message() {
        let anthropic = json!({
            "model": "test",
            "max_tokens": 10,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_result", "tool_use_id": "call_123", "content": "result text"},
                        {"type": "text", "text": "and some text"}
                    ]
                }
            ]
        });

        let openai = anthropic_to_openai_request(&anthropic);
        let msgs = openai["messages"].as_array().unwrap();
        // Should produce: tool message + user message
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[0]["tool_call_id"], "call_123");
        assert_eq!(msgs[0]["content"], "result text");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn assistant_tool_use_becomes_tool_calls() {
        let anthropic = json!({
            "model": "test",
            "max_tokens": 10,
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Let me help"},
                        {
                            "type": "tool_use",
                            "id": "call_abc",
                            "name": "read_file",
                            "input": {"path": "/tmp/test.txt"}
                        }
                    ]
                }
            ]
        });

        let openai = anthropic_to_openai_request(&anthropic);
        let msgs = openai["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[0]["content"], "Let me help");
        let tool_calls = msgs[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_abc");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "read_file");
        // arguments should be JSON string
        let args: serde_json::Value =
            serde_json::from_str(tool_calls[0]["function"]["arguments"].as_str().unwrap())
                .unwrap();
        assert_eq!(args["path"], "/tmp/test.txt");
    }

    #[test]
    fn tools_definition_converted() {
        let anthropic = json!({
            "model": "test",
            "max_tokens": 10,
            "tools": [
                {
                    "name": "read_file",
                    "description": "Read a file",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"}
                        },
                        "required": ["path"]
                    }
                }
            ],
            "messages": [{"role": "user", "content": "Hi"}]
        });

        let openai = anthropic_to_openai_request(&anthropic);
        let tools = openai["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "read_file");
        assert_eq!(tools[0]["function"]["description"], "Read a file");
        assert!(tools[0]["function"]["parameters"]["properties"]["path"].is_object());
    }

    #[test]
    fn tool_choice_type_tool_mapped() {
        let anthropic = json!({
            "model": "test",
            "max_tokens": 10,
            "tool_choice": {"type": "tool", "name": "read_file"},
            "messages": [{"role": "user", "content": "Hi"}]
        });

        let openai = anthropic_to_openai_request(&anthropic);
        assert_eq!(openai["tool_choice"]["type"], "function");
        assert_eq!(openai["tool_choice"]["function"]["name"], "read_file");
    }

    #[test]
    fn stop_sequences_mapped_to_stop() {
        let anthropic = json!({
            "model": "test",
            "max_tokens": 10,
            "stop_sequences": ["END", "STOP"],
            "messages": [{"role": "user", "content": "Hi"}]
        });

        let openai = anthropic_to_openai_request(&anthropic);
        assert_eq!(openai["stop"], json!(["END", "STOP"]));
    }

    #[test]
    fn stream_flag_preserved() {
        let anthropic = json!({
            "model": "test",
            "max_tokens": 10,
            "stream": true,
            "messages": [{"role": "user", "content": "Hi"}]
        });

        let openai = anthropic_to_openai_request(&anthropic);
        assert_eq!(openai["stream"], true);
    }

    // --- Response conversion ---

    #[test]
    fn basic_response_conversion() {
        let openai = json!({
            "id": "chatcmpl-abc123",
            "choices": [{
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });

        let anthropic = openai_to_anthropic_response(&openai, "claude-sonnet-4-20250514");

        assert_eq!(anthropic["type"], "message");
        assert_eq!(anthropic["role"], "assistant");
        assert_eq!(anthropic["model"], "claude-sonnet-4-20250514");
        assert_eq!(anthropic["stop_reason"], "end_turn");

        let content = anthropic["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello!");

        assert_eq!(anthropic["usage"]["input_tokens"], 10);
        assert_eq!(anthropic["usage"]["output_tokens"], 5);
    }

    #[test]
    fn response_with_tool_calls() {
        let openai = json!({
            "id": "chatcmpl-xyz",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"/tmp/test.txt\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });

        let anthropic = openai_to_anthropic_response(&openai, "test");
        assert_eq!(anthropic["stop_reason"], "tool_use");

        let content = anthropic["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["id"], "call_abc");
        assert_eq!(content[0]["name"], "read_file");
        assert_eq!(content[0]["input"]["path"], "/tmp/test.txt");
    }

    #[test]
    fn length_finish_reason_mapped() {
        let openai = json!({
            "id": "x",
            "choices": [{
                "message": {"role": "assistant", "content": "..."},
                "finish_reason": "length"
            }],
            "usage": {"prompt_tokens": 0, "completion_tokens": 0}
        });

        let anthropic = openai_to_anthropic_response(&openai, "test");
        assert_eq!(anthropic["stop_reason"], "max_tokens");
    }

    // --- Streaming conversion ---

    #[test]
    fn stream_converter_basic_flow() {
        let mut conv = StreamConverter::new("claude-test".to_string());

        // First chunk: role
        let events = conv.process_chunk(r#"{"choices":[{"delta":{"role":"assistant"},"index":0}]}"#);
        assert!(!events.is_empty());
        assert!(events[0].contains("message_start"));

        // Content chunk
        let events = conv.process_chunk(r#"{"choices":[{"delta":{"content":"Hello"},"index":0}]}"#);
        let combined: String = events.join("");
        assert!(combined.contains("content_block_start"));
        assert!(combined.contains("content_block_delta"));
        assert!(combined.contains("Hello"));

        // More content
        let events = conv.process_chunk(r#"{"choices":[{"delta":{"content":"!"},"index":0}]}"#);
        let combined: String = events.join("");
        assert!(combined.contains("content_block_delta"));
        assert!(combined.contains("!"));
        assert!(!combined.contains("content_block_start"));

        // Finish
        let events = conv.process_chunk(r#"{"choices":[{"delta":{},"finish_reason":"stop","index":0}]}"#);
        let combined: String = events.join("");
        assert!(combined.contains("content_block_stop"));

        // [DONE]
        let events = conv.process_chunk("[DONE]");
        let combined: String = events.join("");
        assert!(combined.contains("message_delta"));
        assert!(combined.contains("end_turn"));
        assert!(combined.contains("message_stop"));
    }

    #[test]
    fn stream_converter_tool_calls() {
        let mut conv = StreamConverter::new("test".to_string());

        // First chunk
        conv.process_chunk(r#"{"choices":[{"delta":{"role":"assistant"},"index":0}]}"#);

        // Tool call start
        let events = conv.process_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"read_file","arguments":""}}]},"index":0}]}"#,
        );
        let combined: String = events.join("");
        assert!(combined.contains("content_block_start"));
        assert!(combined.contains("tool_use"));
        assert!(combined.contains("call_abc"));
        assert!(combined.contains("read_file"));

        // Tool arguments streaming
        let events = conv.process_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]},"index":0}]}"#,
        );
        let combined: String = events.join("");
        assert!(combined.contains("input_json_delta"));

        // Finish with tool_calls
        let events = conv.process_chunk(
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls","index":0}]}"#,
        );
        let combined: String = events.join("");
        assert!(combined.contains("content_block_stop"));

        // [DONE]
        let events = conv.process_chunk("[DONE]");
        let combined: String = events.join("");
        assert!(combined.contains("tool_use"));
        assert!(combined.contains("message_stop"));
    }

    #[test]
    fn stream_converter_thinking() {
        let mut conv = StreamConverter::new("test".to_string());

        // First chunk
        conv.process_chunk(r#"{"choices":[{"delta":{"role":"assistant"},"index":0}]}"#);

        // Thinking content
        let events = conv.process_chunk(
            r#"{"choices":[{"delta":{"thinking":{"content":"Let me think..."}},"index":0}]}"#,
        );
        let combined: String = events.join("");
        assert!(combined.contains("content_block_start"));
        assert!(combined.contains("\"type\":\"thinking\""));
        assert!(combined.contains("thinking_delta"));
        assert!(combined.contains("Let me think..."));

        // Thinking signature (ends thinking block)
        let events = conv.process_chunk(
            r#"{"choices":[{"delta":{"thinking":{"signature":"sig_abc"}},"index":0}]}"#,
        );
        let combined: String = events.join("");
        assert!(combined.contains("signature_delta"));
        assert!(combined.contains("sig_abc"));
        assert!(combined.contains("content_block_stop"));

        // Then text content
        let events = conv.process_chunk(r#"{"choices":[{"delta":{"content":"Here is my answer"},"index":0}]}"#);
        let combined: String = events.join("");
        assert!(combined.contains("content_block_start"));
        assert!(combined.contains("text_delta"));
    }
}
