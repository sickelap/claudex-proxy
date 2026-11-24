use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
use std::sync::Arc;

use serde_json::Value;

use crate::AppError;
use crate::application::{InferenceProvider, InferenceService, SequentialSessionIds};
use crate::auth::LocalCredentialProvider;
use crate::config;
use crate::domain::{
    ClientProtocol, InferenceError, InferenceEvent, InferenceRequest, InferenceResult,
};
use crate::providers::{AnthropicProvider, CodexProvider};

/// Interactive REPL that streams replies from the Responses API.
pub fn run() -> Result<(), AppError> {
    let registry = config::load_registry(None)?;
    let codex: Arc<dyn InferenceProvider> = Arc::new(CodexProvider::default());
    let anthropic: Arc<dyn InferenceProvider> = Arc::new(AnthropicProvider::default());
    let service = InferenceService::new(
        Arc::new(registry),
        Arc::new(LocalCredentialProvider),
        [codex, anthropic],
        Arc::new(SequentialSessionIds::default()),
    );
    run_with_inference(&|request| service.infer(request))
}

fn run_with_inference(
    inference: &impl Fn(InferenceRequest) -> Result<InferenceResult, InferenceError>,
) -> Result<(), AppError> {
    let mut messages: Vec<Value> = Vec::new();
    println!("claudex-proxy: /exit or Ctrl-D to quit");

    let stdin = io::stdin();
    loop {
        print!("> ");
        io::stdout().flush().ok();

        let mut line = String::new();
        let bytes_read = stdin
            .lock()
            .read_line(&mut line)
            .map_err(|e| AppError(e.to_string()))?;
        if bytes_read == 0 {
            println!();
            return Ok(());
        }

        let content = line.trim_end_matches(['\n', '\r']).to_string();
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "/exit" || trimmed == "/quit" {
            return Ok(());
        }

        messages.push(serde_json::json!({"role": "user", "content": content}));

        match ask(inference, &messages) {
            Ok(answer) => {
                let answer = if answer.is_empty() {
                    "[no text output]".to_string()
                } else {
                    answer
                };
                println!("{answer}");
                messages.push(serde_json::json!({"role": "assistant", "content": answer}));
            }
            Err(error) => {
                messages.pop();
                eprintln!("error: {error}");
            }
        }
    }
}

fn build_payload(messages: &[Value]) -> Value {
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.5".to_string());
    serde_json::json!({
        "model": model,
        "input": messages,
        "store": false,
        "stream": true,
    })
}

fn ask(
    inference: &impl Fn(InferenceRequest) -> Result<InferenceResult, InferenceError>,
    messages: &[Value],
) -> Result<String, AppError> {
    let payload = build_payload(messages);
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("gpt-5.5")
        .to_string();
    let request = InferenceRequest {
        requested_model: model,
        protocol: ClientProtocol::Responses,
        system: vec![],
        messages: vec![],
        tools: vec![],
        tool_choice: None,
        reasoning: None,
        max_output_tokens: None,
        stream: true,
        provider_options: BTreeMap::new(),
        proxy: None,
        native_body: payload,
    };
    let result = inference(request).map_err(AppError::from)?;
    if let Some(InferenceEvent::NativeResponse { status, body, .. }) = result.events.first() {
        if !(200..300).contains(status) {
            return Err(AppError(format!(
                "HTTP {status}: {}",
                String::from_utf8_lossy(body)
            )));
        }
        return parse_stream_text(Cursor::new(body));
    }
    let mut answer = String::new();
    for event in result.events {
        match event {
            InferenceEvent::TextDelta { delta, .. }
            | InferenceEvent::ReasoningDelta { delta, .. } => answer.push_str(&delta),
            InferenceEvent::ContentBlockStart {
                block: crate::domain::ContentBlock::Text { text },
                ..
            } => answer.push_str(&text),
            InferenceEvent::Error { error } => return Err(AppError::from(error)),
            _ => {}
        }
    }
    Ok(answer.trim().to_string())
}

fn parse_stream_text(reader: impl Read) -> Result<String, AppError> {
    let buf_reader = BufReader::new(reader);
    let mut parts = String::new();
    let mut event_name: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();
    let mut final_response: Option<Value> = None;

    for line in buf_reader.lines() {
        let line = line.map_err(|e| AppError(e.to_string()))?;
        if line.is_empty() {
            if let Some(resp) = handle_stream_event(event_name.take(), &data_lines, &mut parts)? {
                final_response = Some(resp);
            }
            data_lines.clear();
        } else if let Some(rest) = line.strip_prefix("event:") {
            event_name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start().to_string());
        }
    }
    if let Some(resp) = handle_stream_event(event_name.take(), &data_lines, &mut parts)? {
        final_response = Some(resp);
    }

    let trimmed = parts.trim();
    if !trimmed.is_empty() {
        return Ok(trimmed.to_string());
    }
    Ok(output_text(
        &final_response.unwrap_or_else(|| Value::Object(Default::default())),
    ))
}

fn handle_stream_event(
    event_name: Option<String>,
    data_lines: &[String],
    parts: &mut String,
) -> Result<Option<Value>, AppError> {
    if data_lines.is_empty() {
        return Ok(None);
    }

    let data = data_lines.join("\n");
    if data == "[DONE]" {
        return Ok(None);
    }

    let event: Value = match serde_json::from_str(&data) {
        Ok(event) => event,
        Err(_) => return Ok(None),
    };

    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(event_name);

    match event_type.as_deref() {
        Some("response.output_text.delta") | Some("response.refusal.delta") => {
            if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                parts.push_str(delta);
            }
            Ok(None)
        }
        Some("response.completed") => Ok(Some(event.get("response").cloned().unwrap_or(event))),
        Some("error") | Some("response.failed") => Err(AppError(stream_error_message(&event))),
        _ => Ok(None),
    }
}

fn stream_error_message(event: &Value) -> String {
    let error = event
        .get("error")
        .or_else(|| event.get("response").and_then(|r| r.get("error")))
        .cloned()
        .unwrap_or_else(|| event.clone());

    match &error {
        Value::Object(_) => error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.get("detail").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string()),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn output_text(response: &Value) -> String {
    if let Some(text) = response
        .get("output_text")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        return text.to_string();
    }

    let mut parts = String::new();
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for item in output {
            if item.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let Some(content) = item.get("content").and_then(Value::as_array) else {
                continue;
            };
            for entry in content {
                match entry.get("type").and_then(Value::as_str) {
                    Some("output_text") => {
                        if let Some(text) = entry.get("text").and_then(Value::as_str) {
                            parts.push_str(text);
                        }
                    }
                    Some("refusal") => {
                        if let Some(text) = entry.get("refusal").and_then(Value::as_str) {
                            parts.push_str(text);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    parts.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_output_text_deltas() {
        let sse = "event: response.output_text.delta\ndata: {\"delta\":\"Hel\"}\n\nevent: response.output_text.delta\ndata: {\"delta\":\"lo\"}\n\n";
        let text = parse_stream_text(sse.as_bytes()).unwrap();
        assert_eq!(text, "Hello");
    }

    #[test]
    fn falls_back_to_completed_response_when_no_deltas() {
        let sse = "event: response.completed\ndata: {\"response\":{\"output_text\":\"done\"}}\n\n";
        let text = parse_stream_text(sse.as_bytes()).unwrap();
        assert_eq!(text, "done");
    }

    #[test]
    fn raises_on_stream_error_event() {
        let sse = "event: error\ndata: {\"error\":{\"message\":\"boom\"}}\n\n";
        let error = parse_stream_text(sse.as_bytes()).unwrap_err();
        assert_eq!(error.0, "boom");
    }

    #[test]
    fn raises_on_response_failed_event() {
        let sse =
            "event: response.failed\ndata: {\"response\":{\"error\":{\"message\":\"failed\"}}}\n\n";
        let error = parse_stream_text(sse.as_bytes()).unwrap_err();
        assert_eq!(error.0, "failed");
    }

    #[test]
    fn chat_uses_application_boundary_with_complete_history() {
        let seen = std::sync::Mutex::new(None);
        let inference = |request: InferenceRequest| {
            *seen.lock().unwrap() = Some(request);
            Ok(InferenceResult {
                events: vec![InferenceEvent::NativeResponse {
                    protocol: ClientProtocol::Responses,
                    status: 200,
                    content_type: "text/event-stream".into(),
                    body: b"event: response.output_text.delta\ndata: {\"delta\":\"ok\"}\n\n"
                        .to_vec(),
                }],
                metadata: None,
            })
        };
        let messages = vec![
            serde_json::json!({"role":"user","content":"first"}),
            serde_json::json!({"role":"assistant","content":"answer"}),
            serde_json::json!({"role":"user","content":"second"}),
        ];
        assert_eq!(ask(&inference, &messages).unwrap(), "ok");
        let request = seen.lock().unwrap();
        let request = request.as_ref().unwrap();
        assert_eq!(request.native_body["input"], Value::Array(messages));
        assert!(request.stream);
    }
}
