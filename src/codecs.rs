use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::domain::{ContentBlock, InferenceError, InferenceEvent, Usage};

pub fn responses_to_messages(
    input: &Value,
    upstream_model: &str,
    default_max: u64,
) -> Result<Value, InferenceError> {
    let source = input
        .as_object()
        .ok_or_else(|| InferenceError::validation("Responses request must be an object"))?;
    reject_keys(source, &["audio", "modalities", "parallel_tool_calls"])?;
    let mut out = Map::new();
    out.insert("model".into(), Value::String(upstream_model.into()));
    out.insert(
        "max_tokens".into(),
        Value::from(
            source
                .get("max_output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(default_max),
        ),
    );
    out.insert(
        "messages".into(),
        responses_input_to_messages(source.get("input"))?,
    );
    if let Some(instructions) = source.get("instructions") {
        out.insert("system".into(), instructions.clone());
    }
    if let Some(tools) = source.get("tools") {
        out.insert("tools".into(), responses_tools_to_messages(tools)?);
    }
    if let Some(choice) = source.get("tool_choice") {
        out.insert(
            "tool_choice".into(),
            responses_tool_choice_to_messages(choice)?,
        );
    }
    if let Some(reasoning) = source.get("reasoning") {
        let budget = reasoning
            .get("budget_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(1024);
        out.insert(
            "thinking".into(),
            serde_json::json!({"type":"enabled","budget_tokens":budget}),
        );
    }
    apply_provider_options(source.get("provider_options"), "anthropic.", &mut out)?;
    if source
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        out.insert("stream".into(), Value::Bool(true));
    }
    Ok(Value::Object(out))
}

fn responses_input_to_messages(input: Option<&Value>) -> Result<Value, InferenceError> {
    match input {
        None => Ok(Value::Array(vec![])),
        Some(Value::String(text)) => Ok(serde_json::json!([{"role":"user","content":text}])),
        Some(Value::Array(items)) => {
            let mut messages = Vec::new();
            for item in items {
                let kind = item
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("message");
                match kind {
                    "message" => messages.push(serde_json::json!({
                        "role": item.get("role").and_then(Value::as_str).unwrap_or("user"),
                        "content": responses_content_to_messages(item.get("content"))?,
                    })),
                    "function_call" => messages.push(serde_json::json!({"role":"assistant","content":[{"type":"tool_use","id":item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(Value::String("call".into())),"name":item.get("name").cloned().unwrap_or(Value::String("tool".into())),"input":parse_json_value(item.get("arguments"))?}]})),
                    "function_call_output" => messages.push(serde_json::json!({"role":"user","content":[{"type":"tool_result","tool_use_id":item.get("call_id").cloned().unwrap_or(Value::String("call".into())),"content":item.get("output").cloned().unwrap_or(Value::Null)}]})),
                    other => return Err(InferenceError::validation(format!("Responses input type {other} cannot be translated safely"))),
                }
            }
            Ok(Value::Array(messages))
        }
        _ => Err(InferenceError::validation(
            "Responses input must be text or an array",
        )),
    }
}

fn responses_content_to_messages(content: Option<&Value>) -> Result<Value, InferenceError> {
    let Some(content) = content else {
        return Ok(Value::Array(vec![]));
    };
    if let Value::String(text) = content {
        return Ok(Value::String(text.clone()));
    }
    let items = content.as_array().ok_or_else(|| {
        InferenceError::validation("Responses message content must be text or blocks")
    })?;
    items.iter().map(|item| match item.get("type").and_then(Value::as_str) {
        Some("input_text" | "output_text") => Ok(serde_json::json!({"type":"text","text":item.get("text").cloned().unwrap_or(Value::String(String::new()))})),
        Some("input_image") => Ok(serde_json::json!({"type":"image","source":item.get("source").or_else(|| item.get("image_url")).cloned().unwrap_or(Value::Null)})),
        Some(other) => Err(InferenceError::validation(format!("Responses content block {other} cannot be translated safely"))),
        None => Err(InferenceError::validation("Responses content block requires type")),
    }).collect::<Result<Vec<_>, _>>().map(Value::Array)
}

fn responses_tools_to_messages(tools: &Value) -> Result<Value, InferenceError> {
    let tools = tools
        .as_array()
        .ok_or_else(|| InferenceError::validation("tools must be an array"))?;
    tools.iter().map(|tool| {
        if tool.get("type").and_then(Value::as_str) != Some("function") { return Err(InferenceError::validation("only function tools can be translated")); }
        Ok(serde_json::json!({"name":tool.get("name").cloned().unwrap_or(Value::Null),"description":tool.get("description").cloned().unwrap_or(Value::Null),"input_schema":tool.get("parameters").cloned().unwrap_or_else(|| serde_json::json!({"type":"object"}))}))
    }).collect::<Result<Vec<_>, _>>().map(Value::Array)
}

fn responses_tool_choice_to_messages(choice: &Value) -> Result<Value, InferenceError> {
    match choice.as_str() {
        Some("auto") => Ok(serde_json::json!({"type":"auto"})),
        Some("required") => Ok(serde_json::json!({"type":"any"})),
        Some("none") => Ok(serde_json::json!({"type":"none"})),
        _ if choice.get("type").and_then(Value::as_str) == Some("function") => Ok(
            serde_json::json!({"type":"tool","name":choice.get("name").cloned().unwrap_or(Value::Null)}),
        ),
        _ => Err(InferenceError::validation(
            "unsupported Responses tool_choice",
        )),
    }
}

pub fn messages_to_responses(input: &Value, upstream_model: &str) -> Result<Value, InferenceError> {
    let source = input
        .as_object()
        .ok_or_else(|| InferenceError::validation("Messages request must be an object"))?;
    reject_keys(source, &["metadata", "service_tier"])?;
    let mut out = Map::new();
    out.insert("model".into(), Value::String(upstream_model.into()));
    out.insert("store".into(), Value::Bool(false));
    out.insert(
        "input".into(),
        messages_input_to_responses(source.get("messages"))?,
    );
    if let Some(system) = source.get("system") {
        out.insert("instructions".into(), system.clone());
    }
    if let Some(tools) = source.get("tools") {
        out.insert("tools".into(), messages_tools_to_responses(tools)?);
    }
    if let Some(choice) = source.get("tool_choice") {
        out.insert(
            "tool_choice".into(),
            messages_tool_choice_to_responses(choice)?,
        );
    }
    if let Some(thinking) = source.get("thinking") {
        out.insert("reasoning".into(), serde_json::json!({"budget_tokens":thinking.get("budget_tokens").cloned().unwrap_or(Value::from(1024))}));
    }
    if let Some(limit) = source.get("max_tokens") {
        out.insert("max_output_tokens".into(), limit.clone());
    }
    apply_provider_options(source.get("provider_options"), "codex.", &mut out)?;
    if source
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        out.insert("stream".into(), Value::Bool(true));
    }
    Ok(Value::Object(out))
}

fn messages_input_to_responses(messages: Option<&Value>) -> Result<Value, InferenceError> {
    let messages = messages
        .and_then(Value::as_array)
        .ok_or_else(|| InferenceError::validation("Messages request requires messages array"))?;
    let mut output = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        match message.get("content") {
            Some(Value::String(text)) => output.push(serde_json::json!({"type":"message","role":role,"content":[{"type":"input_text","text":text}]})),
            Some(Value::Array(blocks)) => {
                let mut content = Vec::new();
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => content.push(serde_json::json!({"type":"input_text","text":block.get("text").cloned().unwrap_or(Value::String(String::new()))})),
                        Some("image") => content.push(serde_json::json!({"type":"input_image","source":block.get("source").cloned().unwrap_or(Value::Null)})),
                        Some("tool_use") => output.push(serde_json::json!({"type":"function_call","call_id":block.get("id").cloned().unwrap_or(Value::String("call".into())),"name":block.get("name").cloned().unwrap_or(Value::String("tool".into())),"arguments":serde_json::to_string(block.get("input").unwrap_or(&Value::Null)).unwrap()})),
                        Some("tool_result") => output.push(serde_json::json!({"type":"function_call_output","call_id":block.get("tool_use_id").cloned().unwrap_or(Value::String("call".into())),"output":block.get("content").cloned().unwrap_or(Value::Null)})),
                        Some("thinking") => return Err(InferenceError::validation("signed thinking blocks cannot be replayed safely to Codex")),
                        Some(other) => return Err(InferenceError::validation(format!("Messages content block {other} cannot be translated safely"))),
                        None => return Err(InferenceError::validation("Messages content block requires type")),
                    }
                }
                if !content.is_empty() { output.push(serde_json::json!({"type":"message","role":role,"content":content})); }
            }
            _ => return Err(InferenceError::validation("Messages content must be text or blocks")),
        }
    }
    Ok(Value::Array(output))
}

fn messages_tools_to_responses(tools: &Value) -> Result<Value, InferenceError> {
    tools.as_array().ok_or_else(|| InferenceError::validation("tools must be an array"))?.iter().map(|tool| Ok(serde_json::json!({"type":"function","name":tool.get("name").cloned().unwrap_or(Value::Null),"description":tool.get("description").cloned().unwrap_or(Value::Null),"parameters":tool.get("input_schema").cloned().unwrap_or_else(|| serde_json::json!({"type":"object"}))}))).collect::<Result<Vec<_>, InferenceError>>().map(Value::Array)
}

fn messages_tool_choice_to_responses(choice: &Value) -> Result<Value, InferenceError> {
    match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Ok(Value::String("auto".into())),
        Some("any") => Ok(Value::String("required".into())),
        Some("none") => Ok(Value::String("none".into())),
        Some("tool") => Ok(
            serde_json::json!({"type":"function","name":choice.get("name").cloned().unwrap_or(Value::Null)}),
        ),
        _ => Err(InferenceError::validation(
            "unsupported Messages tool_choice",
        )),
    }
}

fn reject_keys(map: &Map<String, Value>, keys: &[&str]) -> Result<(), InferenceError> {
    if let Some(key) = keys.iter().find(|key| map.contains_key(**key)) {
        return Err(InferenceError::validation(format!(
            "field {key} cannot be translated safely"
        )));
    }
    Ok(())
}

fn apply_provider_options(
    value: Option<&Value>,
    prefix: &str,
    output: &mut Map<String, Value>,
) -> Result<(), InferenceError> {
    let Some(value) = value else {
        return Ok(());
    };
    let options = value
        .as_object()
        .ok_or_else(|| InferenceError::validation("provider_options must be an object"))?;
    for (name, value) in options {
        let stripped = name.strip_prefix(prefix).ok_or_else(|| {
            InferenceError::validation(format!("provider option {name} targets another provider"))
        })?;
        output.insert(stripped.to_string(), value.clone());
    }
    Ok(())
}

fn parse_json_value(value: Option<&Value>) -> Result<Value, InferenceError> {
    match value {
        Some(Value::String(text)) => serde_json::from_str(text)
            .map_err(|_| InferenceError::validation("tool arguments must contain valid JSON")),
        Some(value) => Ok(value.clone()),
        None => Ok(serde_json::json!({})),
    }
}

#[derive(Default, Debug, PartialEq)]
struct Accumulated {
    id: Option<String>,
    blocks: BTreeMap<usize, ContentBlock>,
    usage: Usage,
    stop_reason: Option<String>,
    error: Option<InferenceError>,
    started: bool,
    completed: bool,
}

fn accumulate(events: &[InferenceEvent]) -> Result<Accumulated, InferenceError> {
    let mut state = Accumulated::default();
    for event in events {
        match event {
            InferenceEvent::ResponseStart {
                upstream_request_id,
            } if !state.started => {
                state.started = true;
                state.id = upstream_request_id.clone();
            }
            InferenceEvent::ContentBlockStart { index, block }
                if state.started && !state.blocks.contains_key(index) =>
            {
                state.blocks.insert(*index, block.clone());
            }
            InferenceEvent::TextDelta { index, delta } => match state.blocks.get_mut(index) {
                Some(ContentBlock::Text { text }) => text.push_str(delta),
                _ => {
                    return Err(InferenceError::upstream(
                        "text delta without matching text block",
                        Some(502),
                    ));
                }
            },
            InferenceEvent::ReasoningDelta { index, delta } => match state.blocks.get_mut(index) {
                Some(ContentBlock::Reasoning { text, .. }) => text.push_str(delta),
                _ => {
                    return Err(InferenceError::upstream(
                        "reasoning delta without matching block",
                        Some(502),
                    ));
                }
            },
            InferenceEvent::SignatureDelta { index, delta } => match state.blocks.get_mut(index) {
                Some(ContentBlock::Reasoning { signature, .. }) => {
                    signature.get_or_insert_with(String::new).push_str(delta)
                }
                _ => {
                    return Err(InferenceError::upstream(
                        "signature delta without matching block",
                        Some(502),
                    ));
                }
            },
            InferenceEvent::ToolArgumentDelta { index, delta } => match state.blocks.get_mut(index)
            {
                Some(ContentBlock::ToolUse { input, .. }) => {
                    let mut text = input.as_str().unwrap_or_default().to_string();
                    text.push_str(delta);
                    *input = Value::String(text);
                }
                _ => {
                    return Err(InferenceError::upstream(
                        "tool delta without matching block",
                        Some(502),
                    ));
                }
            },
            InferenceEvent::Usage { usage } => state.usage = usage.clone(),
            InferenceEvent::Complete { stop_reason } => {
                state.completed = true;
                state.stop_reason = stop_reason.clone();
            }
            InferenceEvent::Error { error } => state.error = Some(error.clone()),
            InferenceEvent::Ping
            | InferenceEvent::Unknown { .. }
            | InferenceEvent::ContentBlockDone { .. } => {}
            InferenceEvent::NativeResponse { .. } => {
                return Err(InferenceError::upstream(
                    "native response cannot be translated",
                    Some(502),
                ));
            }
            _ => {
                return Err(InferenceError::upstream(
                    "invalid canonical event order",
                    Some(502),
                ));
            }
        }
    }
    if !state.started {
        return Err(InferenceError::upstream(
            "canonical event stream did not start",
            Some(502),
        ));
    }
    Ok(state)
}

pub fn encode_responses_json(
    events: &[InferenceEvent],
    model: &str,
) -> Result<Value, InferenceError> {
    let state = accumulate(events)?;
    if let Some(error) = state.error {
        return Err(error);
    }
    let mut output = Vec::new();
    let mut content = Vec::new();
    for block in state.blocks.values() {
        match block {
        ContentBlock::Text { text } => content.push(serde_json::json!({"type":"output_text","text":text})),
        ContentBlock::Reasoning { text, signature } => content.push(serde_json::json!({"type":"reasoning","text":text,"signature":signature})),
        ContentBlock::ToolUse { id, name, input } => output.push(serde_json::json!({"type":"function_call","call_id":id,"name":name,"arguments": if input.is_string() { input.clone() } else { Value::String(serde_json::to_string(input).unwrap()) }})),
        ContentBlock::Opaque { .. } => return Err(InferenceError::upstream("opaque provider block cannot cross protocols", Some(502))),
        _ => {}
    }
    }
    if !content.is_empty() {
        output.insert(
            0,
            serde_json::json!({"type":"message","role":"assistant","content":content}),
        );
    }
    Ok(
        serde_json::json!({"id":state.id.unwrap_or_else(||"resp_proxy".into()),"object":"response","status":if state.completed{"completed"}else{"in_progress"},"model":model,"output":output,"usage":{"input_tokens":state.usage.input_tokens,"output_tokens":state.usage.output_tokens,"total_tokens":state.usage.input_tokens+state.usage.output_tokens}}),
    )
}

pub fn encode_messages_json(
    events: &[InferenceEvent],
    model: &str,
) -> Result<Value, InferenceError> {
    let state = accumulate(events)?;
    if let Some(error) = state.error {
        return Err(error);
    }
    let content = state.blocks.values().map(|block| match block {
        ContentBlock::Text { text } => Ok(serde_json::json!({"type":"text","text":text})),
        ContentBlock::Reasoning { text, signature } => Ok(serde_json::json!({"type":"thinking","thinking":text,"signature":signature})),
        ContentBlock::ToolUse { id, name, input } => Ok(serde_json::json!({"type":"tool_use","id":id,"name":name,"input": if input.is_string() { serde_json::from_str(input.as_str().unwrap()).unwrap_or(Value::Null) } else { input.clone() }})),
        ContentBlock::Opaque { .. } => Err(InferenceError::upstream("opaque provider block cannot cross protocols", Some(502))),
        _ => Err(InferenceError::upstream("unsupported output block", Some(502))),
    }).collect::<Result<Vec<_>, _>>()?;
    Ok(
        serde_json::json!({"id":state.id.unwrap_or_else(||"msg_proxy".into()),"type":"message","role":"assistant","model":model,"content":content,"stop_reason":state.stop_reason,"usage":{"input_tokens":state.usage.input_tokens,"output_tokens":state.usage.output_tokens}}),
    )
}

pub fn encode_responses_sse(
    events: &[InferenceEvent],
    model: &str,
) -> Result<Vec<u8>, InferenceError> {
    let final_value = encode_responses_json(events, model)?;
    let mut out = String::new();
    for event in events {
        let (name, data) = match event {
            InferenceEvent::ResponseStart {
                upstream_request_id,
            } => (
                "response.created",
                serde_json::json!({"type":"response.created","response":{"id":upstream_request_id,"model":model}}),
            ),
            InferenceEvent::TextDelta { index, delta } => (
                "response.output_text.delta",
                serde_json::json!({"type":"response.output_text.delta","output_index":index,"delta":delta}),
            ),
            InferenceEvent::ReasoningDelta { index, delta } => (
                "response.reasoning_summary_text.delta",
                serde_json::json!({"type":"response.reasoning_summary_text.delta","output_index":index,"delta":delta}),
            ),
            InferenceEvent::ToolArgumentDelta { index, delta } => (
                "response.function_call_arguments.delta",
                serde_json::json!({"type":"response.function_call_arguments.delta","output_index":index,"delta":delta}),
            ),
            InferenceEvent::Error { error } => {
                ("error", serde_json::json!({"type":"error","error":error}))
            }
            _ => continue,
        };
        out.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    }
    out.push_str(&format!(
        "event: response.completed\ndata: {}\n\n",
        serde_json::json!({"type":"response.completed","response":final_value})
    ));
    Ok(out.into_bytes())
}

pub fn encode_messages_sse(
    events: &[InferenceEvent],
    model: &str,
) -> Result<Vec<u8>, InferenceError> {
    let state = accumulate(events)?;
    let mut out = String::new();
    out.push_str(&format!("event: message_start\ndata: {}\n\n", serde_json::json!({"type":"message_start","message":{"id":state.id,"type":"message","role":"assistant","model":model,"content":[],"usage":{"input_tokens":state.usage.input_tokens,"output_tokens":0}}})));
    for (index, block) in &state.blocks {
        let start = match block {
            ContentBlock::Text { .. } => serde_json::json!({"type":"text","text":""}),
            ContentBlock::Reasoning { .. } => {
                serde_json::json!({"type":"thinking","thinking":"","signature":""})
            }
            ContentBlock::ToolUse { id, name, .. } => {
                serde_json::json!({"type":"tool_use","id":id,"name":name,"input":{}})
            }
            _ => {
                return Err(InferenceError::upstream(
                    "unsupported Messages stream block",
                    Some(502),
                ));
            }
        };
        out.push_str(&format!(
            "event: content_block_start\ndata: {}\n\n",
            serde_json::json!({"type":"content_block_start","index":index,"content_block":start})
        ));
        let delta = match block {
            ContentBlock::Text { text } => serde_json::json!({"type":"text_delta","text":text}),
            ContentBlock::Reasoning { text, .. } => {
                serde_json::json!({"type":"thinking_delta","thinking":text})
            }
            ContentBlock::ToolUse { input, .. } => {
                serde_json::json!({"type":"input_json_delta","partial_json":if input.is_string(){input.as_str().unwrap().to_string()}else{serde_json::to_string(input).unwrap()}})
            }
            _ => unreachable!(),
        };
        out.push_str(&format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::json!({"type":"content_block_delta","index":index,"delta":delta})
        ));
        if let ContentBlock::Reasoning {
            signature: Some(signature),
            ..
        } = block
        {
            out.push_str(&format!("event: content_block_delta\ndata: {}\n\n", serde_json::json!({"type":"content_block_delta","index":index,"delta":{"type":"signature_delta","signature":signature}})));
        }
        out.push_str(&format!(
            "event: content_block_stop\ndata: {}\n\n",
            serde_json::json!({"type":"content_block_stop","index":index})
        ));
    }
    out.push_str(&format!("event: message_delta\ndata: {}\n\n", serde_json::json!({"type":"message_delta","delta":{"stop_reason":state.stop_reason},"usage":{"input_tokens":state.usage.input_tokens,"output_tokens":state.usage.output_tokens,"cache_read_input_tokens":state.usage.cached_input_tokens}})));
    out.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    Ok(out.into_bytes())
}

pub fn decode_responses_sse(body: &[u8]) -> Result<Vec<InferenceEvent>, InferenceError> {
    let text = std::str::from_utf8(body)
        .map_err(|_| InferenceError::upstream("Responses stream is not UTF-8", Some(502)))?;
    let mut out = Vec::new();
    let mut data = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !data.is_empty() {
                let value: Value = serde_json::from_str(&data.join("\n"))
                    .map_err(|error| InferenceError::upstream(error.to_string(), Some(502)))?;
                let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
                let index = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                match kind {
                    "response.created" => out.push(InferenceEvent::ResponseStart {
                        upstream_request_id: value
                            .pointer("/response/id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    }),
                    "response.output_text.delta" => out.push(InferenceEvent::TextDelta {
                        index,
                        delta: value
                            .get("delta")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                    }),
                    "response.reasoning_summary_text.delta" => {
                        out.push(InferenceEvent::ReasoningDelta {
                            index,
                            delta: value
                                .get("delta")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .into(),
                        })
                    }
                    "response.function_call_arguments.delta" => {
                        out.push(InferenceEvent::ToolArgumentDelta {
                            index,
                            delta: value
                                .get("delta")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .into(),
                        })
                    }
                    "response.completed" => {
                        if let Some(response) = value.get("response") {
                            return Ok(crate::providers::responses_json_events(response));
                        }
                    }
                    "error" => out.push(InferenceEvent::Error {
                        error: InferenceError::upstream(
                            value
                                .pointer("/error/message")
                                .and_then(Value::as_str)
                                .unwrap_or("Responses stream error"),
                            None,
                        ),
                    }),
                    _ => out.push(InferenceEvent::Unknown {
                        event_type: kind.into(),
                        data: value,
                    }),
                }
            }
            data.clear();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_responses_request_capabilities_and_rejects_lossy_input() {
        let input = serde_json::json!({"model":"alias","instructions":"system","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]},{"type":"function_call_output","call_id":"c1","output":{"ok":true}}],"tools":[{"type":"function","name":"read","parameters":{"type":"object"}}],"tool_choice":{"type":"function","name":"read"},"reasoning":{"budget_tokens":2048},"max_output_tokens":99,"provider_options":{"anthropic.cache_control":{"type":"ephemeral"}}});
        let out = responses_to_messages(&input, "claude", 4096).unwrap();
        assert_eq!(out["model"], "claude");
        assert_eq!(out["max_tokens"], 99);
        assert_eq!(out["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(out["messages"][1]["content"][0]["type"], "tool_result");
        assert_eq!(out["thinking"]["budget_tokens"], 2048);
        assert_eq!(out["cache_control"]["type"], "ephemeral");
        assert!(responses_to_messages(&serde_json::json!({"audio":{}}), "claude", 1).is_err());
    }

    #[test]
    fn translates_messages_request_capabilities_and_rejects_lossy_input() {
        let input = serde_json::json!({"model":"alias","system":"system","messages":[{"role":"assistant","content":[{"type":"tool_use","id":"c1","name":"read","input":{"path":"a"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"ok"}]}],"tools":[{"name":"read","input_schema":{"type":"object"}}],"tool_choice":{"type":"tool","name":"read"},"thinking":{"budget_tokens":2048},"max_tokens":99,"provider_options":{"codex.reasoning_effort":"high"}});
        let out = messages_to_responses(&input, "gpt").unwrap();
        assert_eq!(out["model"], "gpt");
        assert_eq!(out["input"][0]["type"], "function_call");
        assert_eq!(out["input"][1]["type"], "function_call_output");
        assert_eq!(out["tools"][0]["parameters"]["type"], "object");
        assert_eq!(out["reasoning_effort"], "high");
        let lossy = serde_json::json!({"messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"x","signature":"sig"}]}]});
        assert!(messages_to_responses(&lossy, "gpt").is_err());
    }

    fn canonical() -> Vec<InferenceEvent> {
        vec![
            InferenceEvent::ResponseStart {
                upstream_request_id: Some("id".into()),
            },
            InferenceEvent::ContentBlockStart {
                index: 0,
                block: ContentBlock::Text {
                    text: String::new(),
                },
            },
            InferenceEvent::TextDelta {
                index: 0,
                delta: "hello".into(),
            },
            InferenceEvent::ContentBlockDone { index: 0 },
            InferenceEvent::Usage {
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 2,
                    cached_input_tokens: 0,
                },
            },
            InferenceEvent::Complete {
                stop_reason: Some("end_turn".into()),
            },
        ]
    }

    #[test]
    fn non_streaming_encoders_preserve_content_usage_and_stop() {
        let events = canonical();
        let responses = encode_responses_json(&events, "gpt").unwrap();
        let messages = encode_messages_json(&events, "claude").unwrap();
        assert_eq!(responses["output"][0]["content"][0]["text"], "hello");
        assert_eq!(responses["usage"]["output_tokens"], 2);
        assert_eq!(messages["content"][0]["text"], "hello");
        assert_eq!(messages["stop_reason"], "end_turn");
    }

    #[test]
    fn golden_non_streaming_translation_preserves_tools_reasoning_usage_and_errors() {
        let events = vec![
            InferenceEvent::ResponseStart {
                upstream_request_id: Some("upstream-id".into()),
            },
            InferenceEvent::ContentBlockStart {
                index: 0,
                block: ContentBlock::Reasoning {
                    text: "why".into(),
                    signature: Some("sig".into()),
                },
            },
            InferenceEvent::ContentBlockDone { index: 0 },
            InferenceEvent::ContentBlockStart {
                index: 1,
                block: ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path":"a"}),
                },
            },
            InferenceEvent::ContentBlockDone { index: 1 },
            InferenceEvent::Usage {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 4,
                    cached_input_tokens: 2,
                },
            },
            InferenceEvent::Complete {
                stop_reason: Some("tool_use".into()),
            },
        ];
        let responses = encode_responses_json(&events, "gpt").unwrap();
        let messages = encode_messages_json(&events, "claude").unwrap();
        assert_eq!(responses["output"][0]["content"][0]["signature"], "sig");
        assert_eq!(responses["output"][1]["arguments"], "{\"path\":\"a\"}");
        assert_eq!(messages["content"][0]["type"], "thinking");
        assert_eq!(messages["content"][1]["input"]["path"], "a");
        assert_eq!(messages["usage"]["input_tokens"], 10);
        assert_eq!(messages["stop_reason"], "tool_use");

        let failure = vec![
            InferenceEvent::ResponseStart {
                upstream_request_id: None,
            },
            InferenceEvent::Error {
                error: InferenceError::upstream("safe failure", Some(429)),
            },
        ];
        let error = encode_messages_json(&failure, "x").unwrap_err();
        assert_eq!(error.message, "safe failure");
        assert_eq!(error.status, Some(429));
    }

    #[test]
    fn streaming_encoders_match_non_streaming_semantics_and_reject_bad_order() {
        let events = canonical();
        let responses = encode_responses_sse(&events, "gpt").unwrap();
        let messages = encode_messages_sse(&events, "claude").unwrap();
        let responses_text = String::from_utf8(responses).unwrap();
        let completed_line = responses_text
            .lines()
            .rev()
            .find(|line| line.starts_with("data: "))
            .unwrap()
            .trim_start_matches("data: ");
        let completed: Value = serde_json::from_str(completed_line).unwrap();
        assert_eq!(
            completed["response"],
            encode_responses_json(&events, "gpt").unwrap()
        );
        let decoded_messages = crate::providers::decode_anthropic_sse(&messages).unwrap();
        assert_eq!(
            encode_messages_json(&decoded_messages, "claude").unwrap(),
            encode_messages_json(&events, "claude").unwrap()
        );
        assert!(
            encode_messages_sse(
                &[InferenceEvent::TextDelta {
                    index: 0,
                    delta: "bad".into()
                }],
                "x"
            )
            .is_err()
        );
    }
}
