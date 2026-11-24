use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

use crate::AppError;
use crate::application::CredentialProvider;
use crate::domain::{InferenceError, ProviderCredential, ProviderKind};

pub const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const OPENAI_API_BASE_URL: &str = "https://api.openai.com/v1";
pub const ANTHROPIC_API_BASE_URL: &str = "https://api.anthropic.com/v1";
const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
#[cfg(test)]
const USER_AGENT: &str =
    "codex_vscode/0.94.0-alpha.7 (Mac OS 26.2.0; arm64) vscode/2.4.22 (codex_vscode; 0.1.0)";

#[derive(Debug, Clone)]
pub struct CodexCredential {
    pub token: String,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
    pub client_id: Option<String>,
    pub base_url: String,
}

impl CodexCredential {
    pub fn new(token: impl Into<String>, base_url: impl Into<String>) -> Self {
        CodexCredential {
            token: token.into(),
            refresh_token: None,
            account_id: None,
            client_id: None,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    #[cfg(test)]
    pub fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", self.token),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
        ];
        if let Some(account_id) = &self.account_id {
            headers.push(("ChatGPT-Account-ID".to_string(), account_id.clone()));
        }
        headers
    }
}

impl From<CodexCredential> for ProviderCredential {
    fn from(credential: CodexCredential) -> Self {
        ProviderCredential::Codex {
            token: credential.token,
            refresh_token: credential.refresh_token,
            account_id: credential.account_id,
            client_id: credential.client_id,
            base_url: credential.base_url,
        }
    }
}

pub struct LocalCredentialProvider;

impl CredentialProvider for LocalCredentialProvider {
    fn load(&self, provider: ProviderKind) -> Result<ProviderCredential, InferenceError> {
        match provider {
            ProviderKind::Codex => load_codex_credential()
                .map(ProviderCredential::from)
                .map_err(|error| InferenceError::upstream(error.0, Some(502))),
            ProviderKind::Anthropic => load_anthropic_credential()
                .map_err(|error| InferenceError::upstream(error.0, Some(502))),
        }
    }
}

pub fn load_anthropic_credential() -> Result<ProviderCredential, AppError> {
    load_anthropic_credential_from(
        std::env::var("ANTHROPIC_API_KEY").ok().as_deref(),
        std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok().as_deref(),
    )
}

fn load_anthropic_credential_from(
    api_key: Option<&str>,
    oauth_token: Option<&str>,
) -> Result<ProviderCredential, AppError> {
    if let Some(value) = api_key.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(ProviderCredential::AnthropicApiKey(value.to_string()));
    }
    if let Some(value) = oauth_token.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(ProviderCredential::AnthropicOAuth(value.to_string()));
    }
    Err(AppError(
        "no usable Anthropic credential: set ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN".into(),
    ))
}

/// Decode the payload segment of a JWT (unverified), returning empty claims on failure.
pub fn jwt_claims(token: &str) -> Value {
    decode_jwt_claims(token).unwrap_or_else(|| Value::Object(Default::default()))
}

fn decode_jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let trimmed = payload.trim_end_matches('=');
    let decoded = URL_SAFE_NO_PAD.decode(trimmed).ok()?;
    serde_json::from_slice(&decoded).ok()
}

pub fn jwt_claim(token: &str, name: &str) -> Option<Value> {
    jwt_claims(token).get(name).cloned()
}

/// Recover the OAuth client_id from the access token, falling back to the id token's `aud`.
pub fn client_id_from_tokens(access_token: &str, id_token: &str) -> Result<String, AppError> {
    if let Some(client_id) = jwt_claim(access_token, "client_id").and_then(non_empty_string) {
        return Ok(client_id);
    }

    match jwt_claim(id_token, "aud") {
        Some(Value::String(s)) if !s.is_empty() => return Ok(s),
        Some(Value::Array(items)) => {
            if let Some(first) = items.first().cloned().and_then(non_empty_string) {
                return Ok(first);
            }
        }
        _ => {}
    }

    Err(AppError(
        "could not recover OAuth client_id from Codex auth tokens".to_string(),
    ))
}

fn non_empty_string(value: Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn default_auth_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    Path::new(&home).join(".codex").join("auth.json")
}

pub fn load_codex_credential() -> Result<CodexCredential, AppError> {
    load_codex_credential_from(&default_auth_path())
}

pub fn load_codex_credential_from(path: &Path) -> Result<CodexCredential, AppError> {
    let content = fs::read_to_string(path)
        .map_err(|e| AppError(format!("failed to read {}: {e}", path.display())))?;
    let data: Value = serde_json::from_str(&content)
        .map_err(|e| AppError(format!("invalid JSON in {}: {e}", path.display())))?;

    if let Some(access_token) = data
        .get("tokens")
        .and_then(|t| t.get("access_token"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        let tokens = data.get("tokens").expect("checked above");
        let id_token = tokens
            .get("id_token")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let account_id = tokens
            .get("account_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| chatgpt_account_id(id_token));

        let refresh_token = tokens
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let client_id = client_id_from_tokens(access_token, id_token)?;

        let mut auth = CodexCredential::new(access_token, CHATGPT_CODEX_BASE_URL);
        auth.refresh_token = refresh_token;
        auth.account_id = account_id;
        auth.client_id = Some(client_id);
        return Ok(auth);
    }

    if let Some(api_key) = data
        .get("OPENAI_API_KEY")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return Ok(CodexCredential::new(api_key, OPENAI_API_BASE_URL));
    }

    Err(AppError(format!(
        "no usable token found in {}",
        path.display()
    )))
}

fn chatgpt_account_id(id_token: &str) -> Option<String> {
    jwt_claim(id_token, "https://api.openai.com/auth")
        .and_then(|claim| claim.get("chatgpt_account_id").cloned())
        .and_then(non_empty_string)
}

/// Refresh the access token in memory. Never persists to disk. Returns success/failure
/// without raising.
pub fn refresh_auth(auth: &mut CodexCredential) -> bool {
    let Some(refresh_token) = auth.refresh_token.clone() else {
        return false;
    };

    let payload = serde_json::json!({
        "client_id": auth.client_id,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    });

    let result = ureq::post(REFRESH_URL)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(60))
        .send_json(payload);

    let response = match result {
        Ok(response) => response,
        Err(_) => return false,
    };

    let data: Value = match response.into_json() {
        Ok(data) => data,
        Err(_) => return false,
    };

    let access_token = data
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(token) = &access_token {
        auth.token = token.clone();
    }

    if let Some(refresh_token) = data.get("refresh_token").and_then(|v| v.as_str()) {
        auth.refresh_token = Some(refresh_token.to_string());
    }

    if auth.account_id.is_none() {
        let id_token = data.get("id_token").and_then(|v| v.as_str()).unwrap_or("");
        auth.account_id = chatgpt_account_id(id_token);
    }

    if auth.client_id.is_none() {
        let new_access = access_token.as_deref().unwrap_or("");
        let new_id = data.get("id_token").and_then(|v| v.as_str()).unwrap_or("");
        auth.client_id = client_id_from_tokens(new_access, new_id).ok();
    }

    access_token.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn jwt_with_claims(claims: Value) -> String {
        let payload = serde_json::to_vec(&claims).unwrap();
        let encoded = URL_SAFE_NO_PAD.encode(payload);
        format!("header.{encoded}.signature")
    }

    #[test]
    fn decodes_jwt_claims_and_returns_default_for_invalid_tokens() {
        let token = jwt_with_claims(serde_json::json!({
            "client_id": "client-123",
            "nested": {"value": true},
        }));

        assert_eq!(
            jwt_claim(&token, "client_id"),
            Some(Value::String("client-123".to_string()))
        );
        assert_eq!(jwt_claims("not-a-jwt"), Value::Object(Default::default()));
        assert_eq!(jwt_claim("not-a-jwt", "missing"), None);
    }

    #[test]
    fn recovers_client_id_from_access_token_then_id_token_audience() {
        assert_eq!(
            client_id_from_tokens(
                &jwt_with_claims(serde_json::json!({"client_id": "access-client"})),
                ""
            )
            .unwrap(),
            "access-client"
        );
        assert_eq!(
            client_id_from_tokens(
                "",
                &jwt_with_claims(serde_json::json!({"aud": "id-client"}))
            )
            .unwrap(),
            "id-client"
        );
        assert_eq!(
            client_id_from_tokens(
                "",
                &jwt_with_claims(serde_json::json!({"aud": ["first-client", "second-client"]}))
            )
            .unwrap(),
            "first-client"
        );

        let err = client_id_from_tokens("", &jwt_with_claims(serde_json::json!({"aud": []})))
            .unwrap_err();
        assert!(err.0.contains("client_id"));
    }

    fn write_auth_and_load(data: &Value) -> Result<CodexCredential, AppError> {
        let mut path = std::env::temp_dir();
        path.push(format!("claudex-proxy-test-auth-{:p}.json", data));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(serde_json::to_vec(data).unwrap().as_slice())
            .unwrap();
        drop(file);
        let result = load_codex_credential_from(&path);
        let _ = fs::remove_file(&path);
        result
    }

    #[test]
    fn loads_codex_tokens_with_account_and_client_headers() {
        let id_token = jwt_with_claims(serde_json::json!({
            "aud": "codex-client",
            "https://api.openai.com/auth": {"chatgpt_account_id": "account-from-id-token"},
        }));
        let auth = write_auth_and_load(&serde_json::json!({
            "tokens": {
                "access_token": "access-token",
                "refresh_token": "refresh-token",
                "id_token": id_token,
            }
        }))
        .unwrap();

        assert_eq!(auth.token, "access-token");
        assert_eq!(auth.refresh_token, Some("refresh-token".to_string()));
        assert_eq!(auth.account_id, Some("account-from-id-token".to_string()));
        assert_eq!(auth.client_id, Some("codex-client".to_string()));
        assert_eq!(auth.base_url, CHATGPT_CODEX_BASE_URL);
        let headers = auth.headers();
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "ChatGPT-Account-ID" && v == "account-from-id-token")
        );
    }

    #[test]
    fn loads_openai_api_key_fallback() {
        let auth = write_auth_and_load(&serde_json::json!({"OPENAI_API_KEY": "sk-local"})).unwrap();

        assert_eq!(auth.token, "sk-local");
        assert_eq!(auth.refresh_token, None);
        assert_eq!(auth.base_url, OPENAI_API_BASE_URL);
        assert!(
            !auth
                .headers()
                .iter()
                .any(|(k, _)| k == "ChatGPT-Account-ID")
        );
        assert_eq!(
            ProviderCredential::from(auth).provider(),
            crate::domain::ProviderKind::Codex
        );
    }

    #[test]
    fn fails_when_no_usable_credential_is_present() {
        let err = write_auth_and_load(&serde_json::json!({})).unwrap_err();
        assert!(err.0.contains("no usable token found in"));
    }

    #[test]
    fn anthropic_credentials_prefer_api_key_then_oauth_and_are_tagged() {
        let credential = load_anthropic_credential_from(Some(" api-key "), Some("oauth")).unwrap();
        assert_eq!(
            credential,
            ProviderCredential::AnthropicApiKey("api-key".into())
        );
        assert_eq!(credential.provider(), ProviderKind::Anthropic);

        assert_eq!(
            load_anthropic_credential_from(Some(" "), Some(" oauth ")).unwrap(),
            ProviderCredential::AnthropicOAuth("oauth".into())
        );
    }

    #[test]
    fn anthropic_credentials_reject_empty_or_missing_values() {
        assert!(
            load_anthropic_credential_from(Some(""), Some("  "))
                .unwrap_err()
                .0
                .contains("ANTHROPIC_API_KEY")
        );
        assert!(load_anthropic_credential_from(None, None).is_err());
    }
}
