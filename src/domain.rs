use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Codex,
    Anthropic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderCredential {
    Codex {
        token: String,
        refresh_token: Option<String>,
        account_id: Option<String>,
        client_id: Option<String>,
        base_url: String,
    },
    AnthropicApiKey(String),
    AnthropicOAuth(String),
}

impl ProviderCredential {
    pub fn provider(&self) -> ProviderKind {
        match self {
            Self::Codex { .. } => ProviderKind::Codex,
            Self::AnthropicApiKey(_) | Self::AnthropicOAuth(_) => ProviderKind::Anthropic,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientProtocol {
    Responses,
    Messages,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source: Value,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(default)]
        is_error: bool,
    },
    Reasoning {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    Opaque {
        provider: ProviderKind,
        value: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyMetadataOptions {
    #[serde(default)]
    pub include_metadata: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceRequest {
    pub requested_model: String,
    pub protocol: ClientProtocol,
    #[serde(default)]
    pub system: Vec<ContentBlock>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub provider_options: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyMetadataOptions>,
    pub native_body: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceError {
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

impl InferenceError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self {
            kind: "validation_error".into(),
            message: message.into(),
            status: Some(400),
        }
    }

    pub fn upstream(message: impl Into<String>, status: Option<u16>) -> Self {
        Self {
            kind: "upstream_error".into(),
            message: message.into(),
            status,
        }
    }
}

impl From<InferenceError> for AppError {
    fn from(error: InferenceError) -> Self {
        AppError(error.message)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InferenceEvent {
    NativeResponse {
        protocol: ClientProtocol,
        status: u16,
        content_type: String,
        body: Vec<u8>,
    },
    ResponseStart {
        upstream_request_id: Option<String>,
    },
    ContentBlockStart {
        index: usize,
        block: ContentBlock,
    },
    TextDelta {
        index: usize,
        delta: String,
    },
    ReasoningDelta {
        index: usize,
        delta: String,
    },
    SignatureDelta {
        index: usize,
        delta: String,
    },
    ToolArgumentDelta {
        index: usize,
        delta: String,
    },
    ContentBlockDone {
        index: usize,
    },
    Usage {
        usage: Usage,
    },
    Complete {
        stop_reason: Option<String>,
    },
    Ping,
    Unknown {
        event_type: String,
        data: Value,
    },
    Error {
        error: InferenceError,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingMetadata {
    pub session_id: String,
    pub provider: ProviderKind,
    pub requested_model: String,
    pub upstream_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceResult {
    pub events: Vec<InferenceEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<RoutingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMapping {
    pub id: String,
    pub provider: ProviderKind,
    pub upstream_model: String,
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
    #[serde(default)]
    pub supported_options: BTreeSet<String>,
    #[serde(default)]
    pub default_max_output_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_content_round_trips_without_flattening() {
        let request = InferenceRequest {
            requested_model: "claude".into(),
            protocol: ClientProtocol::Messages,
            system: vec![ContentBlock::Text {
                text: "be precise".into(),
            }],
            messages: vec![Message {
                role: "assistant".into(),
                content: vec![
                    ContentBlock::Reasoning {
                        text: "think".into(),
                        signature: Some("sig".into()),
                    },
                    ContentBlock::ToolUse {
                        id: "tool-1".into(),
                        name: "read".into(),
                        input: serde_json::json!({"path":"a"}),
                    },
                ],
            }],
            tools: vec![],
            tool_choice: None,
            reasoning: None,
            max_output_tokens: Some(42),
            stream: true,
            provider_options: BTreeMap::new(),
            proxy: None,
            native_body: serde_json::json!({"vendor_extension": true}),
        };
        let json = serde_json::to_value(&request).unwrap();
        let decoded: InferenceRequest = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(decoded, request);
        assert!(json["messages"][0]["content"].is_array());
        assert_eq!(json["messages"][0]["content"][1]["type"], "tool_use");
    }

    #[test]
    fn ordered_events_serialize_in_order() {
        let result = InferenceResult {
            events: vec![
                InferenceEvent::ResponseStart {
                    upstream_request_id: Some("req".into()),
                },
                InferenceEvent::TextDelta {
                    index: 0,
                    delta: "hello".into(),
                },
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
            ],
            metadata: None,
        };
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["events"][0]["type"], "response_start");
        assert_eq!(value["events"][1]["type"], "text_delta");
        assert_eq!(value["events"][3]["type"], "complete");
    }
}
