use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::application::InferenceProvider;
use crate::auth::{self, CodexCredential};
use crate::codecs::{decode_responses_sse, messages_to_responses, responses_to_messages};
use crate::domain::{
    ContentBlock, InferenceError, InferenceEvent, InferenceRequest, ModelMapping,
    ProviderCredential, ProviderKind, Usage,
};

const CODEX_USER_AGENT: &str = "codex_vscode/0.94.0-alpha.7 (claudex-proxy)";
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderHttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub stream: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderHttpResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

pub trait HttpTransport: Send + Sync {
    fn send(&self, request: ProviderHttpRequest) -> Result<ProviderHttpResponse, String>;
}

pub struct UreqTransport;

impl HttpTransport for UreqTransport {
    fn send(&self, input: ProviderHttpRequest) -> Result<ProviderHttpResponse, String> {
        let mut request = ureq::post(&input.url).timeout(Duration::from_secs(120));
        for (name, value) in &input.headers {
            request = request.set(name, value);
        }
        let response = match request.send_bytes(&input.body) {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => return Err(error.to_string()),
        };
        let status = response.status();
        let content_type = response.content_type().to_string();
        let mut body = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut body)
            .map_err(|error| error.to_string())?;
        Ok(ProviderHttpResponse {
            status,
            content_type,
            body,
        })
    }
}

pub trait CodexTokenRefresher: Send + Sync {
    fn refresh(&self, credential: &mut CodexCredential) -> bool;
}

pub struct RealCodexTokenRefresher;
impl CodexTokenRefresher for RealCodexTokenRefresher {
    fn refresh(&self, credential: &mut CodexCredential) -> bool {
        auth::refresh_auth(credential)
    }
}

pub struct CodexProvider {
    transport: Arc<dyn HttpTransport>,
    refresher: Arc<dyn CodexTokenRefresher>,
}

pub struct AnthropicProvider {
    transport: Arc<dyn HttpTransport>,
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self {
            transport: Arc::new(UreqTransport),
        }
    }
}

impl AnthropicProvider {
    #[cfg(test)]
    pub fn new(transport: Arc<dyn HttpTransport>) -> Self {
        Self { transport }
    }

    pub fn execute_native(
        &self,
        credential: &ProviderCredential,
        mapping: &ModelMapping,
        request: &InferenceRequest,
    ) -> Result<ProviderHttpResponse, InferenceError> {
        let translated = if request.protocol == crate::domain::ClientProtocol::Responses {
            responses_to_messages(
                &request.native_body,
                &mapping.upstream_model,
                mapping.default_max_output_tokens.unwrap_or(4096),
            )?
        } else {
            request.native_body.clone()
        };
        let mut body = translated
            .as_object()
            .cloned()
            .ok_or_else(|| InferenceError::validation("Messages request must be an object"))?;
        body.insert(
            "model".into(),
            Value::String(mapping.upstream_model.clone()),
        );
        if !body.contains_key("max_tokens") {
            body.insert(
                "max_tokens".into(),
                Value::from(
                    request
                        .max_output_tokens
                        .or(mapping.default_max_output_tokens)
                        .unwrap_or(4096),
                ),
            );
        }
        body.remove("provider_options");
        body.remove("claudex");
        body.remove("authorization");
        body.remove("api_key");
        body.remove("x-api-key");
        let body = serde_json::to_vec(&Value::Object(body))
            .map_err(|error| InferenceError::validation(error.to_string()))?;
        self.transport
            .send(ProviderHttpRequest {
                url: format!("{}/messages", auth::ANTHROPIC_API_BASE_URL),
                headers: anthropic_headers(credential, request.stream)?,
                body,
                stream: request.stream,
            })
            .map_err(|error| InferenceError::upstream(error, None))
    }
}

impl InferenceProvider for AnthropicProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn infer(
        &self,
        mapping: &ModelMapping,
        credential: ProviderCredential,
        request: &InferenceRequest,
    ) -> Result<Vec<InferenceEvent>, InferenceError> {
        let response = self.execute_native(&credential, mapping, request)?;
        if !(200..300).contains(&response.status) {
            return Err(InferenceError::upstream(
                safe_error_detail(&response.body),
                Some(response.status),
            ));
        }
        if request.protocol == crate::domain::ClientProtocol::Messages {
            return Ok(vec![InferenceEvent::NativeResponse {
                protocol: request.protocol,
                status: response.status,
                content_type: response.content_type,
                body: response.body,
            }]);
        }
        if request.stream {
            return decode_anthropic_sse(&response.body);
        }
        let value: Value = serde_json::from_slice(&response.body).map_err(|error| {
            InferenceError::upstream(format!("invalid Anthropic response: {error}"), Some(502))
        })?;
        Ok(messages_json_events(&value))
    }
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self {
            transport: Arc::new(UreqTransport),
            refresher: Arc::new(RealCodexTokenRefresher),
        }
    }
}

impl CodexProvider {
    #[cfg(test)]
    pub fn new(transport: Arc<dyn HttpTransport>, refresher: Arc<dyn CodexTokenRefresher>) -> Self {
        Self {
            transport,
            refresher,
        }
    }

    pub fn execute_native(
        &self,
        credential: &mut CodexCredential,
        mapping: &ModelMapping,
        request: &InferenceRequest,
    ) -> Result<ProviderHttpResponse, InferenceError> {
        let body = codex_body(request, &mapping.upstream_model)?;
        let first = self
            .transport
            .send(codex_http_request(credential, body.clone(), request.stream))
            .map_err(|error| InferenceError::upstream(error, None))?;
        if (first.status == 401 || first.status == 403) && self.refresher.refresh(credential) {
            return self
                .transport
                .send(codex_http_request(credential, body, request.stream))
                .map_err(|error| InferenceError::upstream(error, None));
        }
        Ok(first)
    }
}

fn codex_http_request(
    credential: &CodexCredential,
    body: Vec<u8>,
    stream: bool,
) -> ProviderHttpRequest {
    let mut headers = vec![
        (
            "Authorization".into(),
            format!("Bearer {}", credential.token),
        ),
        ("Content-Type".into(), "application/json".into()),
        (
            "Accept".into(),
            if stream {
                "text/event-stream".into()
            } else {
                "application/json".into()
            },
        ),
        ("User-Agent".into(), CODEX_USER_AGENT.into()),
    ];
    if let Some(account_id) = &credential.account_id {
        headers.push(("ChatGPT-Account-ID".into(), account_id.clone()));
    }
    ProviderHttpRequest {
        url: format!("{}/responses", credential.base_url),
        headers,
        body,
        stream,
    }
}

fn anthropic_headers(
    credential: &ProviderCredential,
    stream: bool,
) -> Result<Vec<(String, String)>, InferenceError> {
    let mut headers = vec![
        ("Content-Type".into(), "application/json".into()),
        (
            "Accept".into(),
            if stream {
                "text/event-stream".into()
            } else {
                "application/json".into()
            },
        ),
        ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
    ];
    match credential {
        ProviderCredential::AnthropicApiKey(value) => {
            headers.push(("x-api-key".into(), value.clone()))
        }
        ProviderCredential::AnthropicOAuth(value) => {
            headers.push(("Authorization".into(), format!("Bearer {value}")))
        }
        ProviderCredential::Codex { .. } => {
            return Err(InferenceError::upstream(
                "Anthropic provider received a Codex credential",
                None,
            ));
        }
    }
    Ok(headers)
}

fn codex_body(request: &InferenceRequest, upstream_model: &str) -> Result<Vec<u8>, InferenceError> {
    let translated = if request.protocol == crate::domain::ClientProtocol::Messages {
        messages_to_responses(&request.native_body, upstream_model)?
    } else {
        request.native_body.clone()
    };
    let mut body = translated
        .as_object()
        .cloned()
        .ok_or_else(|| InferenceError::validation("Responses request must be an object"))?;
    body.insert("model".into(), Value::String(upstream_model.into()));
    body.remove("max_output_tokens");
    body.remove("provider_options");
    body.remove("claudex");
    serde_json::to_vec(&Value::Object(body))
        .map_err(|error| InferenceError::validation(error.to_string()))
}

impl InferenceProvider for CodexProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn infer(
        &self,
        mapping: &ModelMapping,
        credential: ProviderCredential,
        request: &InferenceRequest,
    ) -> Result<Vec<InferenceEvent>, InferenceError> {
        let ProviderCredential::Codex {
            token,
            refresh_token,
            account_id,
            client_id,
            base_url,
        } = credential
        else {
            return Err(InferenceError::upstream(
                "Codex provider received a non-Codex credential",
                None,
            ));
        };
        let mut credential = CodexCredential {
            token,
            refresh_token,
            account_id,
            client_id,
            base_url,
        };
        let response = self.execute_native(&mut credential, mapping, request)?;
        if !(200..300).contains(&response.status) {
            let detail = safe_error_detail(&response.body);
            return Err(InferenceError::upstream(detail, Some(response.status)));
        }
        if request.protocol == crate::domain::ClientProtocol::Responses {
            return Ok(vec![InferenceEvent::NativeResponse {
                protocol: request.protocol,
                status: response.status,
                content_type: response.content_type,
                body: response.body,
            }]);
        }
        if request.stream {
            return decode_responses_sse(&response.body);
        }
        let value: Value = serde_json::from_slice(&response.body).map_err(|error| {
            InferenceError::upstream(format!("invalid Codex response: {error}"), Some(502))
        })?;
        Ok(responses_json_events(&value))
    }
}

fn safe_error_detail(body: &[u8]) -> String {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("upstream request failed")
        .to_string()
}

pub fn responses_json_events(value: &Value) -> Vec<InferenceEvent> {
    let mut events = vec![InferenceEvent::ResponseStart {
        upstream_request_id: value.get("id").and_then(Value::as_str).map(str::to_string),
    }];
    for item in value
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for content in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = events.len();
            match content.get("type").and_then(Value::as_str) {
                Some("output_text") => events.push(InferenceEvent::ContentBlockStart {
                    index,
                    block: ContentBlock::Text {
                        text: content
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                    },
                }),
                Some("reasoning") => events.push(InferenceEvent::ContentBlockStart {
                    index,
                    block: ContentBlock::Reasoning {
                        text: content
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        signature: content
                            .get("signature")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    },
                }),
                _ => events.push(InferenceEvent::ContentBlockStart {
                    index,
                    block: ContentBlock::Opaque {
                        provider: ProviderKind::Codex,
                        value: content.clone(),
                    },
                }),
            }
            events.push(InferenceEvent::ContentBlockDone { index });
        }
    }
    if let Some(usage) = value.get("usage") {
        events.push(InferenceEvent::Usage {
            usage: Usage {
                input_tokens: usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                output_tokens: usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                cached_input_tokens: usage
                    .pointer("/input_tokens_details/cached_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            },
        });
    }
    events.push(InferenceEvent::Complete {
        stop_reason: value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string),
    });
    events
}

pub fn messages_json_events(value: &Value) -> Vec<InferenceEvent> {
    let mut events = vec![InferenceEvent::ResponseStart {
        upstream_request_id: value.get("id").and_then(Value::as_str).map(str::to_string),
    }];
    for (index, block) in value
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let block = match block.get("type").and_then(Value::as_str) {
            Some("text") => ContentBlock::Text {
                text: block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
            },
            Some("tool_use") => ContentBlock::ToolUse {
                id: block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
            },
            Some("thinking") => ContentBlock::Reasoning {
                text: block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                signature: block
                    .get("signature")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            },
            _ => ContentBlock::Opaque {
                provider: ProviderKind::Anthropic,
                value: block.clone(),
            },
        };
        events.push(InferenceEvent::ContentBlockStart { index, block });
        events.push(InferenceEvent::ContentBlockDone { index });
    }
    if let Some(usage) = value.get("usage") {
        events.push(InferenceEvent::Usage {
            usage: Usage {
                input_tokens: usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                output_tokens: usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                cached_input_tokens: usage
                    .get("cache_read_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            },
        });
    }
    events.push(InferenceEvent::Complete {
        stop_reason: value
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    });
    events
}

pub(crate) fn decode_anthropic_sse(body: &[u8]) -> Result<Vec<InferenceEvent>, InferenceError> {
    let text = std::str::from_utf8(body)
        .map_err(|_| InferenceError::upstream("Anthropic stream is not UTF-8", Some(502)))?;
    let mut events = Vec::new();
    let mut event_name: Option<&str> = None;
    let mut data = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !data.is_empty() {
                let value: Value = serde_json::from_str(&data.join("\n")).map_err(|error| {
                    InferenceError::upstream(
                        format!("invalid Anthropic stream event: {error}"),
                        Some(502),
                    )
                })?;
                decode_anthropic_event(event_name, value, &mut events);
            }
            event_name = None;
            data.clear();
        } else if let Some(name) = line.strip_prefix("event:") {
            event_name = Some(name.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start());
        }
    }
    Ok(events)
}

fn decode_anthropic_event(name: Option<&str>, value: Value, events: &mut Vec<InferenceEvent>) {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .or(name)
        .unwrap_or("unknown");
    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    match event_type {
        "message_start" => {
            let message = value.get("message").unwrap_or(&value);
            events.push(InferenceEvent::ResponseStart {
                upstream_request_id: message
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
            if let Some(usage) = message.get("usage") {
                events.push(anthropic_usage_event(usage));
            }
        }
        "content_block_start" => {
            let raw = value.get("content_block").cloned().unwrap_or(Value::Null);
            let block = match raw.get("type").and_then(Value::as_str) {
                Some("text") => ContentBlock::Text {
                    text: raw
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                },
                Some("tool_use") => ContentBlock::ToolUse {
                    id: raw
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    name: raw
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    input: raw
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                },
                Some("thinking") => ContentBlock::Reasoning {
                    text: raw
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    signature: raw
                        .get("signature")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                },
                _ => ContentBlock::Opaque {
                    provider: ProviderKind::Anthropic,
                    value: raw,
                },
            };
            events.push(InferenceEvent::ContentBlockStart { index, block });
        }
        "content_block_delta" => {
            let delta = value.get("delta").unwrap_or(&Value::Null);
            let text = |field| {
                delta
                    .get(field)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => events.push(InferenceEvent::TextDelta {
                    index,
                    delta: text("text"),
                }),
                Some("input_json_delta") => events.push(InferenceEvent::ToolArgumentDelta {
                    index,
                    delta: text("partial_json"),
                }),
                Some("thinking_delta") => events.push(InferenceEvent::ReasoningDelta {
                    index,
                    delta: text("thinking"),
                }),
                Some("signature_delta") => events.push(InferenceEvent::SignatureDelta {
                    index,
                    delta: text("signature"),
                }),
                _ => events.push(InferenceEvent::Unknown {
                    event_type: "content_block_delta".into(),
                    data: value,
                }),
            }
        }
        "content_block_stop" => events.push(InferenceEvent::ContentBlockDone { index }),
        "message_delta" => {
            if let Some(usage) = value.get("usage") {
                events.push(anthropic_usage_event(usage));
            }
            if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                events.push(InferenceEvent::Complete {
                    stop_reason: Some(reason.into()),
                });
            }
        }
        "message_stop" => {}
        "ping" => events.push(InferenceEvent::Ping),
        "error" => {
            let error = value.get("error").unwrap_or(&value);
            events.push(InferenceEvent::Error {
                error: InferenceError::upstream(
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Anthropic stream error"),
                    error
                        .get("status")
                        .and_then(Value::as_u64)
                        .map(|status| status as u16),
                ),
            });
        }
        other => events.push(InferenceEvent::Unknown {
            event_type: other.into(),
            data: value,
        }),
    }
}

fn anthropic_usage_event(value: &Value) -> InferenceEvent {
    InferenceEvent::Usage {
        usage: Usage {
            input_tokens: value
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: value
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cached_input_tokens: value
                .get("cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ClientProtocol;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::sync::Mutex;

    struct FakeTransport {
        responses: Mutex<VecDeque<ProviderHttpResponse>>,
        requests: Mutex<Vec<ProviderHttpRequest>>,
    }
    impl HttpTransport for FakeTransport {
        fn send(&self, request: ProviderHttpRequest) -> Result<ProviderHttpResponse, String> {
            self.requests.lock().unwrap().push(request);
            Ok(self.responses.lock().unwrap().pop_front().unwrap())
        }
    }
    struct FakeRefresh(bool);
    impl CodexTokenRefresher for FakeRefresh {
        fn refresh(&self, credential: &mut CodexCredential) -> bool {
            if self.0 {
                credential.token = "new".into();
            }
            self.0
        }
    }
    fn response(status: u16) -> ProviderHttpResponse {
        ProviderHttpResponse {
            status,
            content_type: "application/json".into(),
            body: b"{}".to_vec(),
        }
    }
    fn mapping() -> ModelMapping {
        ModelMapping {
            id: "alias".into(),
            provider: ProviderKind::Codex,
            upstream_model: "upstream".into(),
            capabilities: BTreeSet::new(),
            supported_options: BTreeSet::new(),
            default_max_output_tokens: None,
        }
    }
    fn request() -> InferenceRequest {
        InferenceRequest {
            requested_model: "alias".into(),
            protocol: ClientProtocol::Responses,
            system: vec![],
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            reasoning: None,
            max_output_tokens: Some(99),
            stream: false,
            provider_options: BTreeMap::new(),
            proxy: None,
            native_body: serde_json::json!({"model":"alias","max_output_tokens":99}),
        }
    }

    #[test]
    fn normalizes_request_and_keeps_headers_provider_isolated() {
        let transport = Arc::new(FakeTransport {
            responses: Mutex::new([response(200)].into()),
            requests: Mutex::new(vec![]),
        });
        let provider = CodexProvider::new(transport.clone(), Arc::new(FakeRefresh(false)));
        let mut credential = CodexCredential::new("token", "http://codex");
        provider
            .execute_native(&mut credential, &mapping(), &request())
            .unwrap();
        let requests = transport.requests.lock().unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["model"], "upstream");
        assert!(body.get("max_output_tokens").is_none());
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(name, value)| name == "Authorization" && value == "Bearer token")
        );
        assert!(
            !requests[0]
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("x-api-key")
                    || name.starts_with("anthropic-"))
        );
    }

    #[test]
    fn retries_once_for_401_and_403_when_refreshable() {
        for status in [401, 403] {
            let transport = Arc::new(FakeTransport {
                responses: Mutex::new([response(status), response(200)].into()),
                requests: Mutex::new(vec![]),
            });
            let provider = CodexProvider::new(transport.clone(), Arc::new(FakeRefresh(true)));
            let mut credential = CodexCredential::new("old", "http://codex");
            assert_eq!(
                provider
                    .execute_native(&mut credential, &mapping(), &request())
                    .unwrap()
                    .status,
                200
            );
            let requests = transport.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(
                requests[1]
                    .headers
                    .iter()
                    .any(|(_, value)| value == "Bearer new")
            );
        }
    }

    #[test]
    fn does_not_replay_non_refreshable_errors() {
        let transport = Arc::new(FakeTransport {
            responses: Mutex::new([response(400)].into()),
            requests: Mutex::new(vec![]),
        });
        let provider = CodexProvider::new(transport.clone(), Arc::new(FakeRefresh(true)));
        let mut credential = CodexCredential::new("token", "http://codex");
        assert_eq!(
            provider
                .execute_native(&mut credential, &mapping(), &request())
                .unwrap()
                .status,
            400
        );
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn anthropic_headers_never_include_codex_credentials() {
        let headers = anthropic_headers(
            &ProviderCredential::AnthropicApiKey("anthropic".into()),
            false,
        )
        .unwrap();
        assert!(
            headers
                .iter()
                .any(|(name, value)| name == "x-api-key" && value == "anthropic")
        );
        assert!(
            !headers
                .iter()
                .any(|(name, value)| name == "ChatGPT-Account-ID" || value.contains("codex"))
        );
        let codex = ProviderCredential::Codex {
            token: "codex-secret".into(),
            refresh_token: None,
            account_id: Some("account".into()),
            client_id: None,
            base_url: "http://codex".into(),
        };
        assert!(anthropic_headers(&codex, false).is_err());
    }

    #[test]
    fn anthropic_non_streaming_uses_exact_headers_endpoint_and_upstream_model() {
        for credential in [
            ProviderCredential::AnthropicApiKey("key".into()),
            ProviderCredential::AnthropicOAuth("oauth".into()),
        ] {
            let transport = Arc::new(FakeTransport {
                responses: Mutex::new(
                    [ProviderHttpResponse {
                        status: 200,
                        content_type: "application/json".into(),
                        body: br#"{"content":[]}"#.to_vec(),
                    }]
                    .into(),
                ),
                requests: Mutex::new(vec![]),
            });
            let provider = AnthropicProvider::new(transport.clone());
            let mut mapping = mapping();
            mapping.provider = ProviderKind::Anthropic;
            mapping.upstream_model = "claude-upstream".into();
            mapping.default_max_output_tokens = Some(2048);
            let mut request = request();
            request.protocol = ClientProtocol::Messages;
            request.native_body = serde_json::json!({"model":"alias","messages":[]});
            request.stream = false;
            assert_eq!(
                provider
                    .execute_native(&credential, &mapping, &request)
                    .unwrap()
                    .status,
                200
            );
            let sent = transport.requests.lock().unwrap();
            assert_eq!(sent[0].url, "https://api.anthropic.com/v1/messages");
            assert_eq!(
                serde_json::from_slice::<Value>(&sent[0].body).unwrap()["model"],
                "claude-upstream"
            );
            assert!(
                sent[0]
                    .headers
                    .iter()
                    .any(|(name, value)| name == "anthropic-version" && value == ANTHROPIC_VERSION)
            );
            assert!(
                sent[0]
                    .headers
                    .iter()
                    .any(|(name, value)| (name == "x-api-key" && value == "key")
                        || (name == "Authorization" && value == "Bearer oauth"))
            );
        }
    }

    #[test]
    fn anthropic_errors_expose_safe_message_not_raw_body() {
        let transport = Arc::new(FakeTransport {
            responses: Mutex::new(
                [ProviderHttpResponse {
                    status: 400,
                    content_type: "application/json".into(),
                    body: br#"{"error":{"message":"safe"},"secret":"do-not-return"}"#.to_vec(),
                }]
                .into(),
            ),
            requests: Mutex::new(vec![]),
        });
        let provider = AnthropicProvider::new(transport);
        let mut mapping = mapping();
        mapping.provider = ProviderKind::Anthropic;
        let mut request = request();
        request.protocol = ClientProtocol::Messages;
        let error = provider
            .infer(
                &mapping,
                ProviderCredential::AnthropicApiKey("key".into()),
                &request,
            )
            .unwrap_err();
        assert_eq!(error.message, "safe");
        assert!(!error.message.contains("secret"));
        assert_eq!(error.status, Some(400));
    }

    #[test]
    fn anthropic_stream_golden_fixture_preserves_event_classes_and_order() {
        let fixture = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\n",
            "event: ping\ndata: {\"type\":\"ping\"}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"read\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"why\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
            "event: future_event\ndata: {\"type\":\"future_event\",\"value\":1}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"stream failed\"}}\n\n"
        );
        let events = decode_anthropic_sse(fixture.as_bytes()).unwrap();
        assert!(matches!(events[0], InferenceEvent::ResponseStart { .. }));
        assert!(matches!(events[1], InferenceEvent::Usage { .. }));
        assert!(matches!(events[2], InferenceEvent::Ping));
        assert!(events.iter().any(
            |event| matches!(event, InferenceEvent::TextDelta { delta, .. } if delta == "hi")
        ));
        assert!(events.iter().any(|event| matches!(event, InferenceEvent::ToolArgumentDelta { delta, .. } if delta == "{}")));
        assert!(events.iter().any(
            |event| matches!(event, InferenceEvent::ReasoningDelta { delta, .. } if delta == "why")
        ));
        assert!(events.iter().any(
            |event| matches!(event, InferenceEvent::SignatureDelta { delta, .. } if delta == "sig")
        ));
        assert!(events.iter().any(|event| matches!(event, InferenceEvent::Unknown { event_type, .. } if event_type == "future_event")));
        assert!(events.iter().any(|event| matches!(event, InferenceEvent::Complete { stop_reason: Some(reason) } if reason == "end_turn")));
        assert!(
            matches!(events.last(), Some(InferenceEvent::Error { error }) if error.message == "stream failed")
        );
    }

    #[test]
    fn native_messages_fast_path_strips_local_fields_and_preserves_structured_json() {
        for (stream, content_type, response_body) in [
            (false, "application/json", br#"{"type":"message","content":[{"type":"thinking","thinking":"x","signature":"sig"},{"type":"tool_use","id":"t","name":"read","input":{"path":"a"}}],"usage":{"input_tokens":1,"output_tokens":2},"stop_reason":"tool_use"}"#.as_slice()),
            (true, "text/event-stream", b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\"}\n\n".as_slice()),
        ] {
            let transport = Arc::new(FakeTransport { responses: Mutex::new([ProviderHttpResponse { status: 200, content_type: content_type.into(), body: response_body.to_vec() }].into()), requests: Mutex::new(vec![]) });
            let provider = AnthropicProvider::new(transport.clone());
            let mut mapping = mapping(); mapping.provider = ProviderKind::Anthropic;
            let mut request = request(); request.protocol = ClientProtocol::Messages; request.stream = stream;
            request.native_body = serde_json::json!({
                "model":"alias", "messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"ok"}]}]}],
                "tools":[{"name":"read","input_schema":{"type":"object"}}], "thinking":{"type":"enabled","budget_tokens":1024},
                "claudex":{"include_metadata":true}, "provider_options":{"anthropic.cache_control":true},
                "authorization":"local-secret", "api_key":"local-key", "stream":stream
            });
            let events = provider.infer(&mapping, ProviderCredential::AnthropicApiKey("upstream-key".into()), &request).unwrap();
            let InferenceEvent::NativeResponse { body, content_type: returned_type, .. } = &events[0] else { panic!("expected native response") };
            assert_eq!(body, response_body); assert_eq!(returned_type, content_type);
            let sent = transport.requests.lock().unwrap();
            let sent_body: Value = serde_json::from_slice(&sent[0].body).unwrap();
            assert!(sent_body.get("claudex").is_none() && sent_body.get("provider_options").is_none());
            assert!(sent_body.get("authorization").is_none() && sent_body.get("api_key").is_none());
            assert_eq!(sent_body["messages"][0]["content"][0]["type"], "tool_result");
            assert_eq!(sent_body["thinking"]["budget_tokens"], 1024);
            assert!(sent[0].headers.iter().any(|(name, value)| name == "x-api-key" && value == "upstream-key"));
        }
    }
}
