use std::io::{Cursor, Read};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::auth::{self, Auth};
use crate::AppError;

const RESPONSES_PATH: &str = "/v1/responses";

pub struct UpstreamResponse {
    pub status: u16,
    pub content_type: String,
    pub reader: Box<dyn Read + Send>,
}

impl UpstreamResponse {
    fn from_bytes(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        UpstreamResponse {
            status,
            content_type: content_type.to_string(),
            reader: Box::new(Cursor::new(body)),
        }
    }
}

type AuthLoader = Arc<dyn Fn() -> Result<Auth, AppError> + Send + Sync>;
type UpstreamSender = Arc<dyn Fn(&Auth, &[u8], bool) -> Result<UpstreamResponse, ureq::Error> + Send + Sync>;
type RefreshFn = Arc<dyn Fn(&mut Auth) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct ProxyConfig {
    auth_loader: AuthLoader,
    upstream_sender: UpstreamSender,
    refresh_func: RefreshFn,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig {
            auth_loader: Arc::new(auth::load_auth),
            upstream_sender: Arc::new(send_upstream),
            refresh_func: Arc::new(auth::refresh_auth),
        }
    }
}

/// Forward a request to the upstream Codex Responses API.
pub fn send_upstream(auth: &Auth, body: &[u8], stream: bool) -> Result<UpstreamResponse, ureq::Error> {
    let accept = if stream { "text/event-stream" } else { "application/json" };
    let url = format!("{}/responses", auth.base_url);
    let mut request = ureq::post(&url).timeout(Duration::from_secs(120));
    for (name, value) in auth.headers() {
        request = request.set(&name, &value);
    }
    request = request.set("Accept", accept);

    let response = request.send_bytes(body)?;
    let status = response.status();
    let content_type = response.content_type().to_string();
    Ok(UpstreamResponse {
        status,
        content_type,
        reader: response.into_reader(),
    })
}

fn upstream_error_response(error: ureq::Error) -> Result<UpstreamResponse, u16> {
    match error {
        ureq::Error::Status(code, response) => {
            let content_type = response.content_type().to_string();
            let body = read_response_body(response);
            Ok(UpstreamResponse::from_bytes(code, &content_type, body))
        }
        ureq::Error::Transport(_) => Err(502),
    }
}

fn read_response_body(response: ureq::Response) -> Vec<u8> {
    let mut body = Vec::new();
    let _ = response.into_reader().read_to_end(&mut body);
    body
}

/// Start the proxy server bound to `host:port`, handling each request on its own thread.
pub fn run(host: &str, port: u16) -> Result<(), AppError> {
    run_with_config(host, port, ProxyConfig::default())
}

fn run_with_config(host: &str, port: u16, config: ProxyConfig) -> Result<(), AppError> {
    let server = Server::http((host, port))
        .map_err(|e| AppError(format!("failed to bind {host}:{port}: {e}")))?;
    eprintln!("openai-api-proxy listening on http://{host}:{port}");
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
    if request.url() != RESPONSES_PATH {
        respond_json_error(request, 404, "unsupported path: only POST /v1/responses is supported");
        return;
    }

    if *request.method() != Method::Post {
        respond_json_error(request, 405, "unsupported method: only POST /v1/responses is supported");
        return;
    }

    let mut request = request;
    let mut body = Vec::new();
    if request.as_reader().read_to_end(&mut body).is_err() {
        respond_json_error(request, 400, "request body must be UTF-8 JSON");
        return;
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(_) => {
            respond_json_error(request, 400, "invalid JSON request body");
            return;
        }
    };

    let stream = payload.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let upstream_body = normalize_request_body(&body, &payload);

    let mut auth = match (config.auth_loader)() {
        Ok(auth) => auth,
        Err(error) => {
            respond_json_error(request, 502, &format!("failed to load credentials: {error}"));
            return;
        }
    };

    let sender = Arc::clone(&config.upstream_sender);
    let refresh = Arc::clone(&config.refresh_func);
    let result = auth::send_with_refresh_retry_using(
        &mut auth,
        move |auth| sender(auth, &upstream_body, stream),
        move |auth| refresh(auth),
    );

    match result {
        Ok(upstream_response) => write_upstream_response(request, upstream_response, stream),
        Err(error) => match upstream_error_response(error) {
            Ok(upstream_response) => write_upstream_response(request, upstream_response, stream),
            Err(status) => respond_json_error(request, status, "upstream request failed"),
        },
    }
}

fn normalize_request_body(body: &[u8], payload: &Value) -> Vec<u8> {
    let Value::Object(map) = payload else {
        return body.to_vec();
    };
    if !map.contains_key("max_output_tokens") {
        return body.to_vec();
    }
    let mut normalized = map.clone();
    normalized.remove("max_output_tokens");
    serde_json::to_vec(&Value::Object(normalized)).unwrap_or_else(|_| body.to_vec())
}

fn write_upstream_response(request: Request, upstream: UpstreamResponse, stream: bool) {
    let content_type = if stream {
        "text/event-stream".to_string()
    } else {
        upstream.content_type.clone()
    };
    let header = content_type_header(&content_type);
    let response = Response::new(upstream.status.into(), vec![header], upstream.reader, None, None);
    let _ = request.respond(response);
}

fn respond_json_error(request: Request, status: u16, message: &str) {
    let body = serde_json::json!({ "error": message }).to_string();
    let header = content_type_header("application/json");
    let response = Response::from_string(body)
        .with_status_code(status)
        .with_header(header);
    let _ = request.respond(response);
}

fn content_type_header(content_type: &str) -> Header {
    Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
        .expect("valid content-type header")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct TestServer {
        server: Arc<Server>,
        addr: String,
    }

    fn start_test_server(config: ProxyConfig) -> TestServer {
        let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let addr = server.server_addr().to_string();
        let handle = Arc::clone(&server);
        let config = Arc::new(config);
        thread::spawn(move || {
            for request in handle.incoming_requests() {
                let config = Arc::clone(&config);
                thread::spawn(move || handle_request(request, &config));
            }
        });
        TestServer { server, addr }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.server.unblock();
        }
    }

    fn raw_request(addr: &str, method: &str, path: &str, body: Option<&[u8]>) -> (u16, String, Vec<u8>) {
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
            Err(ureq::Error::Transport(t)) => panic!("transport error: {t}"),
        };

        let status = response.status();
        let content_type = response.content_type().to_string();
        let mut body = Vec::new();
        let _ = response.into_reader().read_to_end(&mut body);
        (status, content_type, body)
    }

    fn config_with(
        upstream_sender: impl Fn(&Auth, &[u8], bool) -> Result<UpstreamResponse, ureq::Error> + Send + Sync + 'static,
    ) -> ProxyConfig {
        ProxyConfig {
            auth_loader: Arc::new(|| Ok(Auth::new("token", auth::CHATGPT_CODEX_BASE_URL))),
            upstream_sender: Arc::new(upstream_sender),
            refresh_func: Arc::new(|_| false),
        }
    }

    #[test]
    fn rejects_unsupported_path() {
        let config = config_with(|_, _, _| unreachable!("should not call upstream"));
        let server = start_test_server(config);
        let (status, _, _) = raw_request(&server.addr, "POST", "/v1/chat/completions", Some(b"{}"));
        assert_eq!(status, 404);
    }

    #[test]
    fn rejects_unsupported_method() {
        let config = config_with(|_, _, _| unreachable!("should not call upstream"));
        let server = start_test_server(config);
        let (status, _, _) = raw_request(&server.addr, "GET", "/v1/responses", None);
        assert_eq!(status, 405);
    }

    #[test]
    fn rejects_invalid_json_body() {
        let config = config_with(|_, _, _| unreachable!("should not call upstream"));
        let server = start_test_server(config);
        let (status, _, body) = raw_request(&server.addr, "POST", "/v1/responses", Some(b"{\"input\":"));
        assert_eq!(status, 400);
        assert!(String::from_utf8_lossy(&body).contains("invalid JSON"));
    }

    #[test]
    fn strips_max_output_tokens_and_preserves_other_fields() {
        let seen: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let seen_clone = Arc::clone(&seen);
        let config = config_with(move |_, body, stream| {
            assert!(stream);
            *seen_clone.lock().unwrap() = Some(serde_json::from_slice(body).unwrap());
            Ok(UpstreamResponse::from_bytes(200, "text/event-stream", b"".to_vec()))
        });
        let server = start_test_server(config);
        let payload = serde_json::json!({
            "model": "gpt-5.5",
            "max_output_tokens": 32000,
            "store": false,
            "stream": true,
        });
        let (status, _, _) = raw_request(
            &server.addr,
            "POST",
            "/v1/responses",
            Some(serde_json::to_vec(&payload).unwrap().as_slice()),
        );
        assert_eq!(status, 200);
        let forwarded = seen.lock().unwrap().clone().unwrap();
        assert!(forwarded.get("max_output_tokens").is_none());
        assert_eq!(forwarded.get("model").unwrap(), "gpt-5.5");
    }

    #[test]
    fn preserves_original_bytes_when_no_normalization_needed() {
        let seen: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let config = config_with(move |_, body, _| {
            *seen_clone.lock().unwrap() = body.to_vec();
            Ok(UpstreamResponse::from_bytes(200, "application/json", b"{}".to_vec()))
        });
        let server = start_test_server(config);
        let request_body = b"{ \"model\": \"gpt-test\", \"input\": [], \"store\": false }";
        let (status, _, _) = raw_request(&server.addr, "POST", "/v1/responses", Some(request_body));
        assert_eq!(status, 200);
        assert_eq!(seen.lock().unwrap().as_slice(), request_body);
    }

    #[test]
    fn refreshes_auth_and_retries_once_after_unauthorized() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_clone = Arc::clone(&calls);
        let config = ProxyConfig {
            auth_loader: Arc::new(|| {
                let mut auth = Auth::new("expired", auth::CHATGPT_CODEX_BASE_URL);
                auth.refresh_token = Some("refresh".to_string());
                auth.client_id = Some("client".to_string());
                Ok(auth)
            }),
            upstream_sender: Arc::new(move |auth, _, _| {
                let mut calls = calls_clone.lock().unwrap();
                calls.push(auth.token.clone());
                if calls.len() == 1 {
                    let response = ureq::Response::new(401, "Unauthorized", "expired").unwrap();
                    Err(ureq::Error::Status(401, response))
                } else {
                    Ok(UpstreamResponse::from_bytes(200, "application/json", b"{\"ok\":true}".to_vec()))
                }
            }),
            refresh_func: Arc::new(|auth| {
                auth.token = "refreshed".to_string();
                true
            }),
        };
        let server = start_test_server(config);
        let (status, _, body) = raw_request(&server.addr, "POST", "/v1/responses", Some(b"{}"));
        assert_eq!(status, 200);
        assert_eq!(body, b"{\"ok\":true}");
        assert_eq!(calls.lock().unwrap().as_slice(), &["expired".to_string(), "refreshed".to_string()]);
    }

    #[test]
    fn passes_through_non_refreshable_upstream_errors() {
        let config = config_with(|_, _, _| {
            let response = ureq::Response::new(400, "Bad Request", "{\"error\":\"bad\"}").unwrap();
            Err(ureq::Error::Status(400, response))
        });
        let server = start_test_server(config);
        let (status, _, body) = raw_request(&server.addr, "POST", "/v1/responses", Some(b"{}"));
        assert_eq!(status, 400);
        assert_eq!(body, b"{\"error\":\"bad\"}");
    }
}
