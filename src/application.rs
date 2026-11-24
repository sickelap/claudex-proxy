use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::domain::{
    ClientProtocol, InferenceError, InferenceEvent, InferenceRequest, InferenceResult,
    ModelMapping, ProviderCredential, ProviderKind, RoutingMetadata,
};

pub trait ModelRegistry: Send + Sync {
    fn resolve(&self, public_model: &str) -> Result<ModelMapping, InferenceError>;
}

pub trait CredentialProvider: Send + Sync {
    fn load(&self, provider: ProviderKind) -> Result<ProviderCredential, InferenceError>;
}

pub trait InferenceProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn infer(
        &self,
        mapping: &ModelMapping,
        credential: ProviderCredential,
        request: &InferenceRequest,
    ) -> Result<Vec<InferenceEvent>, InferenceError>;
}

pub trait SessionIdGenerator: Send + Sync {
    fn generate(&self) -> String;
}

#[derive(Default)]
pub struct SequentialSessionIds(AtomicU64);

impl SessionIdGenerator for SequentialSessionIds {
    fn generate(&self) -> String {
        format!("session-{}", self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

#[derive(Default)]
pub struct StaticModelRegistry {
    mappings: HashMap<String, ModelMapping>,
}

impl StaticModelRegistry {
    pub fn new(mappings: impl IntoIterator<Item = ModelMapping>) -> Result<Self, InferenceError> {
        let mut registry = Self::default();
        for mapping in mappings {
            if registry
                .mappings
                .insert(mapping.id.clone(), mapping)
                .is_some()
            {
                return Err(InferenceError::validation("duplicate public model id"));
            }
        }
        Ok(registry)
    }
}

impl ModelRegistry for StaticModelRegistry {
    fn resolve(&self, public_model: &str) -> Result<ModelMapping, InferenceError> {
        self.mappings
            .get(public_model)
            .cloned()
            .ok_or_else(|| InferenceError::validation(format!("unsupported model: {public_model}")))
    }
}

pub struct InferenceService {
    registry: Arc<dyn ModelRegistry>,
    credentials: Arc<dyn CredentialProvider>,
    providers: HashMap<ProviderKind, Arc<dyn InferenceProvider>>,
    sessions: Arc<dyn SessionIdGenerator>,
}

impl InferenceService {
    pub fn new(
        registry: Arc<dyn ModelRegistry>,
        credentials: Arc<dyn CredentialProvider>,
        providers: impl IntoIterator<Item = Arc<dyn InferenceProvider>>,
        sessions: Arc<dyn SessionIdGenerator>,
    ) -> Self {
        Self {
            registry,
            credentials,
            providers: providers
                .into_iter()
                .map(|provider| (provider.kind(), provider))
                .collect(),
            sessions,
        }
    }

    pub fn infer(&self, request: InferenceRequest) -> Result<InferenceResult, InferenceError> {
        let mapping = self.registry.resolve(&request.requested_model)?;
        self.validate_options(&request, &mapping)?;
        let provider = self.providers.get(&mapping.provider).ok_or_else(|| {
            InferenceError::upstream(
                format!("provider {:?} is unavailable", mapping.provider),
                None,
            )
        })?;
        let credential = self.credentials.load(mapping.provider)?;
        if credential.provider() != mapping.provider {
            return Err(InferenceError::upstream(
                "credential provider mismatch",
                None,
            ));
        }
        let events = provider.infer(&mapping, credential, &request)?;
        let metadata = request
            .proxy
            .as_ref()
            .filter(|options| options.include_metadata)
            .map(|options| {
                let upstream_request_id = events.iter().find_map(|event| match event {
                    InferenceEvent::ResponseStart {
                        upstream_request_id,
                    } => upstream_request_id.clone(),
                    _ => None,
                });
                RoutingMetadata {
                    session_id: options
                        .session_id
                        .clone()
                        .unwrap_or_else(|| self.sessions.generate()),
                    provider: mapping.provider,
                    requested_model: request.requested_model.clone(),
                    upstream_model: mapping.upstream_model.clone(),
                    upstream_request_id,
                }
            });
        Ok(InferenceResult { events, metadata })
    }

    fn validate_options(
        &self,
        request: &InferenceRequest,
        mapping: &ModelMapping,
    ) -> Result<(), InferenceError> {
        if request.native_body.get("provider").is_some() {
            return Err(InferenceError::validation(
                "clients cannot select a provider directly",
            ));
        }
        match (request.protocol, mapping.provider) {
            (ClientProtocol::Responses, ProviderKind::Anthropic) => {
                for field in ["audio", "modalities", "parallel_tool_calls"] {
                    if request.native_body.get(field).is_some() {
                        return Err(InferenceError::validation(format!(
                            "field {field} cannot be translated safely"
                        )));
                    }
                }
            }
            (ClientProtocol::Messages, ProviderKind::Codex) => {
                if request.native_body.get("metadata").is_some()
                    || request.native_body.get("service_tier").is_some()
                    || contains_block_type(&request.native_body, "thinking")
                {
                    return Err(InferenceError::validation(
                        "Messages request contains features that cannot be translated safely to Codex",
                    ));
                }
            }
            _ => {}
        }
        let mut required = Vec::new();
        if request.stream {
            required.push("streaming");
        }
        if request.native_body.get("tools").is_some() {
            required.push("tools");
        }
        if request.native_body.get("reasoning").is_some()
            || request.native_body.get("thinking").is_some()
        {
            required.push("reasoning");
        }
        for capability in required {
            if !mapping.capabilities.contains(capability) {
                return Err(InferenceError::validation(format!(
                    "model {} does not support {capability}",
                    mapping.id
                )));
            }
        }
        let expected_namespace = match mapping.provider {
            ProviderKind::Codex => "codex.",
            ProviderKind::Anthropic => "anthropic.",
        };
        for option in request.provider_options.keys() {
            if !option.starts_with(expected_namespace) {
                return Err(InferenceError::validation(format!(
                    "provider option {option} does not match selected provider"
                )));
            }
            if !mapping.supported_options.contains(option) {
                return Err(InferenceError::validation(format!(
                    "unsupported provider option: {option}"
                )));
            }
        }
        Ok(())
    }
}

fn contains_block_type(value: &serde_json::Value, expected: &str) -> bool {
    match value {
        serde_json::Value::Array(items) => {
            items.iter().any(|item| contains_block_type(item, expected))
        }
        serde_json::Value::Object(map) => {
            map.get("type").and_then(serde_json::Value::as_str) == Some(expected)
                || map.values().any(|item| contains_block_type(item, expected))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;
    use std::thread;

    use crate::domain::{ClientProtocol, ProxyMetadataOptions};

    struct FakeCredentials;
    impl CredentialProvider for FakeCredentials {
        fn load(&self, provider: ProviderKind) -> Result<ProviderCredential, InferenceError> {
            Ok(match provider {
                ProviderKind::Codex => ProviderCredential::Codex {
                    token: "codex".into(),
                    refresh_token: None,
                    account_id: None,
                    client_id: None,
                    base_url: "http://codex".into(),
                },
                ProviderKind::Anthropic => ProviderCredential::AnthropicApiKey("anthropic".into()),
            })
        }
    }

    struct FakeProvider {
        kind: ProviderKind,
        calls: Mutex<Vec<String>>,
    }
    impl InferenceProvider for FakeProvider {
        fn kind(&self) -> ProviderKind {
            self.kind
        }
        fn infer(
            &self,
            mapping: &ModelMapping,
            _: ProviderCredential,
            request: &InferenceRequest,
        ) -> Result<Vec<InferenceEvent>, InferenceError> {
            self.calls
                .lock()
                .unwrap()
                .push(request.requested_model.clone());
            Ok(vec![InferenceEvent::TextDelta {
                index: 0,
                delta: mapping.upstream_model.clone(),
            }])
        }
    }

    fn mapping(id: &str, provider: ProviderKind, upstream: &str) -> ModelMapping {
        ModelMapping {
            id: id.into(),
            provider,
            upstream_model: upstream.into(),
            capabilities: BTreeSet::new(),
            supported_options: BTreeSet::new(),
            default_max_output_tokens: None,
        }
    }

    fn request(model: &str) -> InferenceRequest {
        InferenceRequest {
            requested_model: model.into(),
            protocol: ClientProtocol::Responses,
            system: vec![],
            messages: vec![],
            tools: vec![],
            tool_choice: None,
            reasoning: None,
            max_output_tokens: None,
            stream: false,
            provider_options: BTreeMap::new(),
            proxy: None,
            native_body: serde_json::json!({"model": model}),
        }
    }

    fn service() -> Arc<InferenceService> {
        let registry = StaticModelRegistry::new([
            mapping("gpt", ProviderKind::Codex, "gpt-upstream"),
            mapping("claude", ProviderKind::Anthropic, "claude-upstream"),
        ])
        .unwrap();
        let codex: Arc<dyn InferenceProvider> = Arc::new(FakeProvider {
            kind: ProviderKind::Codex,
            calls: Mutex::new(vec![]),
        });
        let anthropic: Arc<dyn InferenceProvider> = Arc::new(FakeProvider {
            kind: ProviderKind::Anthropic,
            calls: Mutex::new(vec![]),
        });
        Arc::new(InferenceService::new(
            Arc::new(registry),
            Arc::new(FakeCredentials),
            [codex, anthropic],
            Arc::new(SequentialSessionIds::default()),
        ))
    }

    #[test]
    fn selects_provider_by_mapping_and_rejects_unknown_models() {
        let service = service();
        let result = service.infer(request("claude")).unwrap();
        assert_eq!(
            result.events[0],
            InferenceEvent::TextDelta {
                index: 0,
                delta: "claude-upstream".into()
            }
        );
        assert_eq!(
            service.infer(request("unknown")).unwrap_err().status,
            Some(400)
        );
    }

    #[test]
    fn concurrent_requests_keep_independent_session_state() {
        let service = service();
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let service = Arc::clone(&service);
                thread::spawn(move || {
                    let mut request = request(if index % 2 == 0 { "gpt" } else { "claude" });
                    request.proxy = Some(ProxyMetadataOptions {
                        include_metadata: true,
                        session_id: None,
                    });
                    service.infer(request).unwrap().metadata.unwrap()
                })
            })
            .collect();
        let metadata: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        let ids: BTreeSet<_> = metadata
            .iter()
            .map(|item| item.session_id.clone())
            .collect();
        assert_eq!(ids.len(), 8);
        assert!(metadata.iter().all(|item| (item.requested_model == "gpt"
            && item.provider == ProviderKind::Codex)
            || (item.requested_model == "claude" && item.provider == ProviderKind::Anthropic)));
    }

    #[test]
    fn rejects_capability_and_provider_option_mismatches_before_provider_call() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        struct CountingProvider(Arc<Mutex<Vec<String>>>);
        impl InferenceProvider for CountingProvider {
            fn kind(&self) -> ProviderKind {
                ProviderKind::Codex
            }
            fn infer(
                &self,
                _: &ModelMapping,
                _: ProviderCredential,
                request: &InferenceRequest,
            ) -> Result<Vec<InferenceEvent>, InferenceError> {
                self.0.lock().unwrap().push(request.requested_model.clone());
                Ok(vec![])
            }
        }
        let mut mapping = mapping("limited", ProviderKind::Codex, "gpt");
        mapping.supported_options.insert("codex.allowed".into());
        let provider: Arc<dyn InferenceProvider> = Arc::new(CountingProvider(Arc::clone(&calls)));
        let service = InferenceService::new(
            Arc::new(StaticModelRegistry::new([mapping]).unwrap()),
            Arc::new(FakeCredentials),
            [provider],
            Arc::new(SequentialSessionIds::default()),
        );
        let mut unsupported = request("limited");
        unsupported.stream = true;
        assert!(service.infer(unsupported).is_err());
        let mut mismatched = request("limited");
        mismatched
            .provider_options
            .insert("anthropic.cache_control".into(), Value::Bool(true));
        assert!(service.infer(mismatched).is_err());
        let mut unknown = request("limited");
        unknown
            .provider_options
            .insert("codex.unknown".into(), Value::Bool(true));
        assert!(service.infer(unknown).is_err());
        let mut lossy = request("limited");
        lossy.protocol = ClientProtocol::Messages;
        lossy.native_body = serde_json::json!({"messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"private","signature":"sig"}]}]});
        assert!(service.infer(lossy).is_err());
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn generated_metadata_contains_only_safe_routing_context() {
        let service = service();
        let mut request = request("gpt");
        request.proxy = Some(ProxyMetadataOptions {
            include_metadata: true,
            session_id: None,
        });
        request.native_body = serde_json::json!({"authorization":"secret","account_id":"private"});
        let metadata = service.infer(request).unwrap().metadata.unwrap();
        let value = serde_json::to_value(metadata).unwrap();
        let text = value.to_string();
        assert!(
            value["session_id"]
                .as_str()
                .unwrap()
                .starts_with("session-")
        );
        assert!(
            !text.contains("secret")
                && !text.contains("private")
                && !text.contains("authorization")
                && !text.contains("account_id")
        );
    }
}
