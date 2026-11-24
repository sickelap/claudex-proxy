use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;
use std::thread;

use serde_json::Value;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::AppError;
use crate::application::{
    InferenceProvider, InferenceService, SequentialSessionIds, StaticModelRegistry,
};
use crate::auth::LocalCredentialProvider;
use crate::codecs::{
    encode_messages_json, encode_messages_sse, encode_responses_json, encode_responses_sse,
};
use crate::domain::{
    ClientProtocol, InferenceError, InferenceEvent, InferenceRequest, InferenceResult,
    ProxyMetadataOptions, RoutingMetadata,
};
use crate::providers::{AnthropicProvider, CodexProvider};

const RESPONSES_PATH: &str = "/v1/responses";
const MESSAGES_PATH: &str = "/v1/messages";
type InferenceFn =
    Arc<dyn Fn(InferenceRequest) -> Result<InferenceResult, InferenceError> + Send + Sync>;

#[derive(Clone)]
struct ProxyConfig {
    inference: InferenceFn,
}

pub fn run(host: &str, port: u16, registry: StaticModelRegistry) -> Result<(), AppError> {
    let codex: Arc<dyn InferenceProvider> = Arc::new(CodexProvider::default());
    let anthropic: Arc<dyn InferenceProvider> = Arc::new(AnthropicProvider::default());
    let service = Arc::new(InferenceService::new(
        Arc::new(registry),
        Arc::new(LocalCredentialProvider),
        [codex, anthropic],
        Arc::new(SequentialSessionIds::default()),
    ));
    let config = ProxyConfig {
        inference: Arc::new(move |request| service.infer(request)),
    };
    run_with_config(host, port, config)
}

fn run_with_config(host: &str, port: u16, config: ProxyConfig) -> Result<(), AppError> {
    let server = Server::http((host, port))
        .map_err(|error| AppError(format!("failed to bind {host}:{port}: {error}")))?;
    eprintln!("claudex-proxy listening on http://{host}:{port}");
    serve(server, config);
    Ok(())
}

fn serve(server: Server, config: ProxyConfig) {
    let server = Arc::new(server);
    let config = Arc::new(config);
    for request in server.incoming_requests() {
        let config = Arc::clone(&config);
        thread::spawn(move || handle_request(request, &config));
    }
}

fn handle_request(request: Request, config: &ProxyConfig) {
    let protocol = match request.url() {
        RESPONSES_PATH => ClientProtocol::Responses,
        MESSAGES_PATH => ClientProtocol::Messages,
        _ => {
            respond_json_error(
                request,
                404,
                "unsupported path: use POST /v1/responses or POST /v1/messages",
            );
            return;
        }
    };
    if *request.method() != Method::Post {
        respond_protocol_error(
            request,
            protocol,
            405,
            "unsupported method: only POST is supported",
        );
        return;
    }
    let mut request = request;
    let mut body = Vec::new();
    if request.as_reader().read_to_end(&mut body).is_err() {
        respond_protocol_error(request, protocol, 400, "request body must be UTF-8 JSON");
        return;
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(_) => {
            respond_protocol_error(request, protocol, 400, "invalid JSON request body");
            return;
        }
    };
    let decoded = match protocol {
        ClientProtocol::Responses => decode_responses_request(payload),
        ClientProtocol::Messages => decode_messages_request(payload),
    };
    let inference_request = match decoded {
        Ok(request) => request,
        Err(error) => {
            respond_protocol_inference_error(request, protocol, error);
            return;
        }
    };
    let response_model = inference_request.requested_model.clone();
    let stream = inference_request.stream;
    match (config.inference)(inference_request) {
        Ok(result) => write_inference_result(request, result, protocol, stream, &response_model),
        Err(error) => respond_protocol_inference_error(request, protocol, error),
    }
}

fn decode_messages_request(payload: Value) -> Result<InferenceRequest, InferenceError> {
    let mut object = payload
        .as_object()
        .cloned()
        .ok_or_else(|| InferenceError::validation("Messages request must be a JSON object"))?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| InferenceError::validation("Messages request requires a non-empty model"))?
        .to_string();
    if !object.get("messages").is_some_and(Value::is_array) {
        return Err(InferenceError::validation(
            "Messages request requires a messages array",
        ));
    }
    let proxy = extract_proxy_metadata(&mut object)?;
    Ok(InferenceRequest {
        requested_model: model,
        protocol: ClientProtocol::Messages,
        system: vec![],
        messages: vec![],
        tools: vec![],
        tool_choice: object.get("tool_choice").cloned(),
        reasoning: object.get("thinking").cloned(),
        max_output_tokens: object.get("max_tokens").and_then(Value::as_u64),
        stream: object
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        provider_options: decode_provider_options(object.get("provider_options"))?,
        proxy,
        native_body: Value::Object(object),
    })
}

fn decode_responses_request(payload: Value) -> Result<InferenceRequest, InferenceError> {
    let mut object = payload
        .as_object()
        .cloned()
        .ok_or_else(|| InferenceError::validation("Responses request must be a JSON object"))?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| InferenceError::validation("Responses request requires a non-empty model"))?
        .to_string();
    let stream = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_output_tokens = object.get("max_output_tokens").and_then(Value::as_u64);
    let proxy = extract_proxy_metadata(&mut object)?;
    Ok(InferenceRequest {
        requested_model: model,
        protocol: ClientProtocol::Responses,
        system: vec![],
        messages: vec![],
        tools: vec![],
        tool_choice: object.get("tool_choice").cloned(),
        reasoning: object.get("reasoning").cloned(),
        max_output_tokens,
        stream,
        provider_options: decode_provider_options(object.get("provider_options"))?,
        proxy,
        native_body: Value::Object(object),
    })
}

fn extract_proxy_metadata(
    object: &mut serde_json::Map<String, Value>,
) -> Result<Option<ProxyMetadataOptions>, InferenceError> {
    let Some(value) = object.remove("claudex") else {
        return Ok(None);
    };
    let options = value.as_object().ok_or_else(|| {
        InferenceError::validation("claudex metadata extension must be an object")
    })?;
    let include_metadata = options
        .get("include_metadata")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let session_id = match options.get("session_id") {
        None => None,
        Some(Value::String(value)) if !value.is_empty() && value.len() <= 128 => {
            Some(value.clone())
        }
        Some(_) => {
            return Err(InferenceError::validation(
                "claudex.session_id must be a non-empty string of at most 128 characters",
            ));
        }
    };
    Ok(Some(ProxyMetadataOptions {
        include_metadata,
        session_id,
    }))
}

fn decode_provider_options(
    value: Option<&Value>,
) -> Result<BTreeMap<String, Value>, InferenceError> {
    match value {
        None => Ok(BTreeMap::new()),
        Some(Value::Object(options)) => Ok(options
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()),
        Some(_) => Err(InferenceError::validation(
            "provider_options must be an object",
        )),
    }
}

fn write_inference_result(
    request: Request,
    result: InferenceResult,
    protocol: ClientProtocol,
    stream: bool,
    model: &str,
) {
    let metadata = result.metadata;
    if let Some(InferenceEvent::NativeResponse {
        status,
        content_type,
        body,
        ..
    }) = result.events.first()
    {
        let content_type = if content_type.is_empty() {
            "application/json"
        } else {
            content_type
        };
        let body = match attach_metadata(body.clone(), metadata.as_ref(), stream) {
            Ok(body) => body,
            Err(error) => {
                respond_protocol_inference_error(request, protocol, error);
                return;
            }
        };
        let response = Response::new(
            (*status).into(),
            vec![content_type_header(content_type)],
            Box::new(Cursor::new(body.clone())),
            Some(body.len()),
            None,
        );
        let _ = request.respond(response);
        return;
    }
    let encoded = if stream {
        match protocol {
            ClientProtocol::Responses => encode_responses_sse(&result.events, model),
            ClientProtocol::Messages => encode_messages_sse(&result.events, model),
        }
    } else {
        match protocol {
            ClientProtocol::Responses => {
                encode_responses_json(&result.events, model).and_then(|value| {
                    serde_json::to_vec(&value)
                        .map_err(|error| InferenceError::upstream(error.to_string(), Some(502)))
                })
            }
            ClientProtocol::Messages => {
                encode_messages_json(&result.events, model).and_then(|value| {
                    serde_json::to_vec(&value)
                        .map_err(|error| InferenceError::upstream(error.to_string(), Some(502)))
                })
            }
        }
    };
    match encoded.and_then(|body| attach_metadata(body, metadata.as_ref(), stream)) {
        Ok(body) => {
            let content_type = if stream {
                "text/event-stream"
            } else {
                "application/json"
            };
            let response = Response::from_data(body)
                .with_status_code(200)
                .with_header(content_type_header(content_type));
            let _ = request.respond(response);
        }
        Err(error) => respond_protocol_inference_error(request, protocol, error),
    }
}

fn attach_metadata(
    body: Vec<u8>,
    metadata: Option<&RoutingMetadata>,
    stream: bool,
) -> Result<Vec<u8>, InferenceError> {
    let Some(metadata) = metadata else {
        return Ok(body);
    };
    if stream {
        let event = serde_json::json!({"type":"claudex.metadata","metadata":metadata});
        let mut prefixed = format!("event: claudex.metadata\ndata: {event}\n\n").into_bytes();
        prefixed.extend(body);
        return Ok(prefixed);
    }
    let mut value: Value = serde_json::from_slice(&body).map_err(|error| {
        InferenceError::upstream(
            format!("cannot attach metadata to invalid JSON response: {error}"),
            Some(502),
        )
    })?;
    let object = value.as_object_mut().ok_or_else(|| {
        InferenceError::upstream("cannot attach metadata to non-object response", Some(502))
    })?;
    object.insert(
        "claudex".into(),
        serde_json::to_value(metadata)
            .map_err(|error| InferenceError::upstream(error.to_string(), Some(502)))?,
    );
    serde_json::to_vec(&value)
        .map_err(|error| InferenceError::upstream(error.to_string(), Some(502)))
}

fn respond_protocol_inference_error(
    request: Request,
    protocol: ClientProtocol,
    error: InferenceError,
) {
    respond_protocol_error(
        request,
        protocol,
        error.status.unwrap_or(502),
        &error.message,
    );
}

fn respond_protocol_error(request: Request, protocol: ClientProtocol, status: u16, message: &str) {
    match protocol {
        ClientProtocol::Responses => respond_json_error(request, status, message),
        ClientProtocol::Messages => {
            let error_type = if status == 400 {
                "invalid_request_error"
            } else {
                "api_error"
            };
            let body = serde_json::json!({ "type": "error", "error": { "type": error_type, "message": message } }).to_string();
            let response = Response::from_string(body)
                .with_status_code(status)
                .with_header(content_type_header("application/json"));
            let _ = request.respond(response);
        }
    }
}

fn respond_json_error(request: Request, status: u16, message: &str) {
    let body = serde_json::json!({ "error": message }).to_string();
    let response = Response::from_string(body)
        .with_status_code(status)
        .with_header(content_type_header("application/json"));
    let _ = request.respond(response);
}

fn content_type_header(content_type: &str) -> Header {
    Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
        .expect("valid content-type header")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, VecDeque};
    use std::io::Read;
    use std::sync::Mutex;
    use std::time::Duration;

    struct TestServer {
        server: Arc<Server>,
        addr: String,
    }
    impl Drop for TestServer {
        fn drop(&mut self) {
            self.server.unblock();
        }
    }

    fn start_test_server(config: ProxyConfig) -> TestServer {
        let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let addr = server.server_addr().to_string();
        let handle = Arc::clone(&server);
        let config = Arc::new(config);
        thread::spawn(move || {
            for request in handle.incoming_requests() {
                handle_request(request, &config);
            }
        });
        TestServer { server, addr }
    }

    fn raw_request(
        addr: &str,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> (u16, String, Vec<u8>) {
        let url = format!("http://{addr}{path}");
        let request = match method {
            "GET" => ureq::get(&url),
            "POST" => ureq::post(&url),
            other => panic!("unsupported test method: {other}"),
        }
        .timeout(Duration::from_secs(2));
        let result = match body {
            Some(bytes) => request.send_bytes(bytes),
            None => request.call(),
        };
        let response = match result {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("transport error: {error}"),
        };
        let status = response.status();
        let content_type = response.content_type().to_string();
        let mut body = Vec::new();
        response.into_reader().read_to_end(&mut body).unwrap();
        (status, content_type, body)
    }

    fn config_with(
        f: impl Fn(InferenceRequest) -> Result<InferenceResult, InferenceError> + Send + Sync + 'static,
    ) -> ProxyConfig {
        ProxyConfig {
            inference: Arc::new(f),
        }
    }

    #[test]
    fn rejects_path_method_invalid_json_and_missing_model() {
        let config = config_with(|_| unreachable!());
        let server = start_test_server(config);
        assert_eq!(
            raw_request(&server.addr, "POST", "/v1/chat/completions", Some(b"{}")).0,
            404
        );
        assert_eq!(
            raw_request(&server.addr, "GET", RESPONSES_PATH, None).0,
            405
        );
        assert_eq!(
            raw_request(&server.addr, "POST", RESPONSES_PATH, Some(b"{")).0,
            400
        );
        assert_eq!(
            raw_request(&server.addr, "POST", RESPONSES_PATH, Some(b"{}")).0,
            400
        );
    }

    #[test]
    fn messages_route_validates_and_uses_messages_error_contract() {
        let config = config_with(|request| {
            Ok(InferenceResult {
                events: vec![InferenceEvent::NativeResponse {
                    protocol: request.protocol,
                    status: 200,
                    content_type: "application/json".into(),
                    body: br#"{"type":"message","content":[]}"#.to_vec(),
                }],
                metadata: None,
            })
        });
        let server = start_test_server(config);
        let (status, _, body) = raw_request(
            &server.addr,
            "POST",
            MESSAGES_PATH,
            Some(br#"{"model":"claude","messages":[]}"#),
        );
        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["type"],
            "message"
        );
        for (method, body) in [
            ("POST", Some(b"{".as_slice())),
            ("POST", Some(br#"{"model":"claude"}"#.as_slice())),
            ("GET", None),
        ] {
            let (status, _, body) = raw_request(&server.addr, method, MESSAGES_PATH, body);
            assert!(status == 400 || status == 405);
            let error: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(error["type"], "error");
            assert!(error["error"]["message"].is_string());
        }
    }

    #[test]
    fn documented_anthropic_request_passes_messages_adapter() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/anthropic-messages-request.json"
        ));
        let request = decode_messages_request(serde_json::from_str(fixture).unwrap()).unwrap();
        assert_eq!(request.requested_model, "claude-sonnet");
        assert!(request.stream);
        assert_eq!(request.max_output_tokens, Some(4096));
    }

    #[test]
    fn proxy_metadata_is_parsed_and_removed_before_inference() {
        let responses = decode_responses_request(serde_json::json!({"model":"gpt","claudex":{"include_metadata":true,"session_id":"opaque-client-id"}})).unwrap();
        let messages = decode_messages_request(serde_json::json!({"model":"claude","messages":[],"claudex":{"include_metadata":true,"session_id":"opaque-client-id"}})).unwrap();
        for request in [responses, messages] {
            assert_eq!(
                request.proxy.as_ref().unwrap().session_id.as_deref(),
                Some("opaque-client-id")
            );
            assert!(request.native_body.get("claudex").is_none());
        }
        assert!(
            decode_responses_request(
                serde_json::json!({"model":"gpt","claudex":{"session_id":""}})
            )
            .is_err()
        );
    }

    #[test]
    fn routes_responses_through_service_and_preserves_native_json() {
        let seen = Arc::new(Mutex::new(None));
        let seen_clone = Arc::clone(&seen);
        let config = config_with(move |request| {
            *seen_clone.lock().unwrap() = Some(request);
            Ok(InferenceResult {
                events: vec![InferenceEvent::NativeResponse {
                    protocol: ClientProtocol::Responses,
                    status: 201,
                    content_type: "application/json".into(),
                    body: b"{\"ok\":true}".to_vec(),
                }],
                metadata: None,
            })
        });
        let server = start_test_server(config);
        let body = br#"{"model":"gpt-5.5","max_output_tokens":12}"#;
        let (status, _, response) = raw_request(&server.addr, "POST", RESPONSES_PATH, Some(body));
        assert_eq!(status, 201);
        assert_eq!(response, b"{\"ok\":true}");
        let seen = seen.lock().unwrap();
        let request = seen.as_ref().unwrap();
        assert_eq!(request.requested_model, "gpt-5.5");
        assert_eq!(request.max_output_tokens, Some(12));
    }

    #[test]
    fn preserves_native_sse_bytes_and_content_type() {
        let sse = b"event: response.completed\ndata: {}\n\n".to_vec();
        let config = config_with(move |_| {
            Ok(InferenceResult {
                events: vec![InferenceEvent::NativeResponse {
                    protocol: ClientProtocol::Responses,
                    status: 200,
                    content_type: "text/event-stream".into(),
                    body: sse.clone(),
                }],
                metadata: None,
            })
        });
        let server = start_test_server(config);
        let (status, content_type, body) = raw_request(
            &server.addr,
            "POST",
            RESPONSES_PATH,
            Some(br#"{"model":"gpt","stream":true}"#),
        );
        assert_eq!(status, 200);
        assert_eq!(content_type, "text/event-stream");
        assert_eq!(body, b"event: response.completed\ndata: {}\n\n");
    }

    #[test]
    fn opt_in_metadata_is_json_field_or_first_stream_event_for_both_protocols() {
        let metadata = RoutingMetadata {
            session_id: "session-x".into(),
            provider: crate::domain::ProviderKind::Anthropic,
            requested_model: "alias".into(),
            upstream_model: "upstream".into(),
            upstream_request_id: Some("req".into()),
        };
        let json =
            attach_metadata(br#"{"type":"message"}"#.to_vec(), Some(&metadata), false).unwrap();
        let json: Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(json["claudex"]["session_id"], "session-x");
        let stream = attach_metadata(
            b"event: message_start\ndata: {}\n\n".to_vec(),
            Some(&metadata),
            true,
        )
        .unwrap();
        let stream = String::from_utf8(stream).unwrap();
        assert!(stream.starts_with("event: claudex.metadata\n"));
        assert!(stream.find("claudex.metadata").unwrap() < stream.find("message_start").unwrap());

        let unchanged_json = br#"{"strict":true}"#.to_vec();
        let unchanged_stream = b"event: response.completed\ndata: {}\n\n".to_vec();
        assert_eq!(
            attach_metadata(unchanged_json.clone(), None, false).unwrap(),
            unchanged_json
        );
        assert_eq!(
            attach_metadata(unchanged_stream.clone(), None, true).unwrap(),
            unchanged_stream
        );
    }

    struct MatrixTransport {
        responses: Mutex<VecDeque<crate::providers::ProviderHttpResponse>>,
        requests: Mutex<Vec<crate::providers::ProviderHttpRequest>>,
    }
    impl crate::providers::HttpTransport for MatrixTransport {
        fn send(
            &self,
            request: crate::providers::ProviderHttpRequest,
        ) -> Result<crate::providers::ProviderHttpResponse, String> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "missing mocked response".into())
        }
    }
    struct NoRefresh;
    impl crate::providers::CodexTokenRefresher for NoRefresh {
        fn refresh(&self, _: &mut crate::auth::CodexCredential) -> bool {
            false
        }
    }
    struct MatrixCredentials;
    impl crate::application::CredentialProvider for MatrixCredentials {
        fn load(
            &self,
            provider: crate::domain::ProviderKind,
        ) -> Result<crate::domain::ProviderCredential, InferenceError> {
            Ok(match provider {
                crate::domain::ProviderKind::Codex => crate::domain::ProviderCredential::Codex {
                    token: "codex".into(),
                    refresh_token: None,
                    account_id: None,
                    client_id: None,
                    base_url: "http://codex.test".into(),
                },
                crate::domain::ProviderKind::Anthropic => {
                    crate::domain::ProviderCredential::AnthropicApiKey("anthropic".into())
                }
            })
        }
    }

    fn matrix_config(
        provider: crate::domain::ProviderKind,
        upstream: crate::providers::ProviderHttpResponse,
    ) -> (ProxyConfig, Arc<MatrixTransport>) {
        let selected = Arc::new(MatrixTransport {
            responses: Mutex::new([upstream].into()),
            requests: Mutex::new(vec![]),
        });
        let unused = Arc::new(MatrixTransport {
            responses: Mutex::new(VecDeque::new()),
            requests: Mutex::new(vec![]),
        });
        let (codex_transport, anthropic_transport): (
            Arc<dyn crate::providers::HttpTransport>,
            Arc<dyn crate::providers::HttpTransport>,
        ) = match provider {
            crate::domain::ProviderKind::Codex => (selected.clone(), unused),
            crate::domain::ProviderKind::Anthropic => (unused, selected.clone()),
        };
        let codex: Arc<dyn InferenceProvider> =
            Arc::new(CodexProvider::new(codex_transport, Arc::new(NoRefresh)));
        let anthropic: Arc<dyn InferenceProvider> =
            Arc::new(AnthropicProvider::new(anthropic_transport));
        let capabilities: BTreeSet<_> = ["text", "tools", "reasoning", "streaming"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let mappings = [
            crate::domain::ModelMapping {
                id: "codex-alias".into(),
                provider: crate::domain::ProviderKind::Codex,
                upstream_model: "codex-upstream".into(),
                capabilities: capabilities.clone(),
                supported_options: BTreeSet::new(),
                default_max_output_tokens: None,
            },
            crate::domain::ModelMapping {
                id: "anthropic-alias".into(),
                provider: crate::domain::ProviderKind::Anthropic,
                upstream_model: "anthropic-upstream".into(),
                capabilities,
                supported_options: BTreeSet::new(),
                default_max_output_tokens: Some(1024),
            },
        ];
        let service = Arc::new(InferenceService::new(
            Arc::new(StaticModelRegistry::new(mappings).unwrap()),
            Arc::new(MatrixCredentials),
            [codex, anthropic],
            Arc::new(SequentialSessionIds::default()),
        ));
        (
            ProxyConfig {
                inference: Arc::new(move |request| service.infer(request)),
            },
            selected,
        )
    }

    fn codex_response(stream: bool) -> crate::providers::ProviderHttpResponse {
        let response = serde_json::json!({"id":"resp_1","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":1,"output_tokens":1}});
        if stream {
            crate::providers::ProviderHttpResponse {
                status: 200,
                content_type: "text/event-stream".into(),
                body: format!(
                    "event: response.completed\ndata: {}\n\n",
                    serde_json::json!({"type":"response.completed","response":response})
                )
                .into_bytes(),
            }
        } else {
            crate::providers::ProviderHttpResponse {
                status: 200,
                content_type: "application/json".into(),
                body: serde_json::to_vec(&response).unwrap(),
            }
        }
    }

    fn anthropic_response(stream: bool) -> crate::providers::ProviderHttpResponse {
        if stream {
            crate::providers::ProviderHttpResponse { status: 200, content_type: "text/event-stream".into(), body: concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n").as_bytes().to_vec() }
        } else {
            crate::providers::ProviderHttpResponse { status: 200, content_type: "application/json".into(), body: br#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":1,"output_tokens":1},"stop_reason":"end_turn"}"#.to_vec() }
        }
    }

    #[test]
    fn end_to_end_matrix_routes_by_model_and_returns_inbound_protocol() {
        for protocol in [ClientProtocol::Responses, ClientProtocol::Messages] {
            for provider in [
                crate::domain::ProviderKind::Codex,
                crate::domain::ProviderKind::Anthropic,
            ] {
                for stream in [false, true] {
                    let upstream = match provider {
                        crate::domain::ProviderKind::Codex => codex_response(stream),
                        crate::domain::ProviderKind::Anthropic => anthropic_response(stream),
                    };
                    let (config, transport) = matrix_config(provider, upstream);
                    let server = start_test_server(config);
                    let (path, model, payload) = match protocol {
                        ClientProtocol::Responses => (
                            RESPONSES_PATH,
                            if provider == crate::domain::ProviderKind::Codex {
                                "codex-alias"
                            } else {
                                "anthropic-alias"
                            },
                            serde_json::json!({"model":if provider == crate::domain::ProviderKind::Codex { "codex-alias" } else { "anthropic-alias" },"input":"hi","stream":stream}),
                        ),
                        ClientProtocol::Messages => (
                            MESSAGES_PATH,
                            if provider == crate::domain::ProviderKind::Codex {
                                "codex-alias"
                            } else {
                                "anthropic-alias"
                            },
                            serde_json::json!({"model":if provider == crate::domain::ProviderKind::Codex { "codex-alias" } else { "anthropic-alias" },"max_tokens":32,"messages":[{"role":"user","content":"hi"}],"stream":stream}),
                        ),
                    };
                    let (status, content_type, body) = raw_request(
                        &server.addr,
                        "POST",
                        path,
                        Some(&serde_json::to_vec(&payload).unwrap()),
                    );
                    assert_eq!(
                        status,
                        200,
                        "{protocol:?} -> {provider:?}, stream={stream}: {}",
                        String::from_utf8_lossy(&body)
                    );
                    if stream {
                        assert_eq!(content_type, "text/event-stream");
                        let text = String::from_utf8(body).unwrap();
                        assert!(match protocol {
                            ClientProtocol::Responses => text.contains("response.completed"),
                            ClientProtocol::Messages => text.contains("message_stop"),
                        });
                    } else {
                        let value: Value = serde_json::from_slice(&body).unwrap();
                        assert!(match protocol {
                            ClientProtocol::Responses => value.get("output").is_some(),
                            ClientProtocol::Messages => value.get("content").is_some(),
                        });
                    }
                    let sent = transport.requests.lock().unwrap();
                    assert_eq!(sent.len(), 1);
                    let sent_body: Value = serde_json::from_slice(&sent[0].body).unwrap();
                    assert_eq!(
                        sent_body["model"],
                        if provider == crate::domain::ProviderKind::Codex {
                            "codex-upstream"
                        } else {
                            "anthropic-upstream"
                        }
                    );
                    assert!(model.ends_with("alias"));
                }
            }
        }
    }
}
