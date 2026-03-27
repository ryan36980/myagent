//! Anthropic OAuth 2.0 (PKCE) authentication support.
//!
//! Provides [`AuthMode`] for dual API-key / OAuth authentication in
//! [`ClaudeProvider`](crate::provider::llm::claude::ClaudeProvider), and
//! [`TokenStore`] for managing the full PKCE flow (authorization URL
//! generation, code exchange, token refresh, file persistence).
//!
//! ## User flow
//!
//! 1. Config sets `auth.mode: "oauth"`
//! 2. User sends `/auth` in Telegram → bot returns Anthropic authorization URL
//! 3. User opens URL, logs in, authorizes → page shows authorization code
//! 4. User sends `/auth CODE` → bot exchanges code for tokens via PKCE
//! 5. Subsequent API calls use `Bearer <token>`, auto-refreshed before expiry

use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::error::{GatewayError, Result};

// ---------------------------------------------------------------------------
// Constants — Anthropic OAuth endpoints
// ---------------------------------------------------------------------------

const AUTH_URL: &str = "https://claude.ai/oauth/authorize";
const DEFAULT_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const CALLBACK_URL: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Beta header required for OAuth token usage with the Anthropic API.
pub const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";

// ---------------------------------------------------------------------------
// AuthMode — the dual-mode enum used by ClaudeProvider
// ---------------------------------------------------------------------------

/// Authentication mode for the Anthropic API.
#[derive(Clone)]
pub enum AuthMode {
    /// Traditional static API key (`x-api-key` header).
    ApiKey(String),
    /// OAuth 2.0 Bearer token with automatic refresh.
    OAuth(Arc<Mutex<TokenStore>>),
}

// ---------------------------------------------------------------------------
// TokenData — persisted to file
// ---------------------------------------------------------------------------

/// Access + refresh token pair, persisted as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenData {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix timestamp (seconds) when the access token expires.
    pub expires_at: i64,
}

// ---------------------------------------------------------------------------
// PkceState — in-memory only, during authorization flow
// ---------------------------------------------------------------------------

/// Transient PKCE state held while the user is completing the browser flow.
#[derive(Debug, Clone)]
struct PkceState {
    code_verifier: String,
    state: String,
}

// ---------------------------------------------------------------------------
// TokenStore
// ---------------------------------------------------------------------------

/// Manages the OAuth token lifecycle: PKCE generation, code exchange,
/// token refresh, and file persistence.
pub struct TokenStore {
    client_id: String,
    token_url: String,
    file_path: PathBuf,
    tokens: Option<TokenData>,
    pkce: Option<PkceState>,
    http_client: reqwest::Client,
}

impl TokenStore {
    /// Create a new `TokenStore`.  Call [`load()`](Self::load) afterwards to
    /// restore tokens from a previous session.
    pub fn new(
        client_id: String,
        token_url: Option<String>,
        file_path: PathBuf,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            client_id,
            token_url: token_url.unwrap_or_else(|| DEFAULT_TOKEN_URL.into()),
            file_path,
            tokens: None,
            pkce: None,
            http_client,
        }
    }

    // -- PKCE helpers -------------------------------------------------------

    /// Generate a PKCE code_verifier (high-entropy random string) and its
    /// S256 code_challenge.  Returns `(code_verifier, code_challenge)`.
    fn generate_pkce() -> (String, String) {
        // Use two UUID v4s concatenated for 32 random bytes (256 bits).
        let verifier = format!(
            "{}{}",
            uuid::Uuid::new_v4().as_simple(),
            uuid::Uuid::new_v4().as_simple(),
        );
        let challenge = {
            let hash = Sha256::digest(verifier.as_bytes());
            URL_SAFE_NO_PAD.encode(hash)
        };
        (verifier, challenge)
    }

    // -- Authorization flow -------------------------------------------------

    /// Start the OAuth authorization flow.  Returns the URL the user should
    /// open in a browser.  The PKCE `code_verifier` is held in memory until
    /// [`exchange_code`] is called.
    pub fn start_auth(&mut self) -> String {
        let (verifier, challenge) = Self::generate_pkce();
        // Generate a random state token for CSRF protection (32 bytes, base64url).
        let state = URL_SAFE_NO_PAD.encode(Sha256::digest(
            uuid::Uuid::new_v4().as_bytes(),
        ));
        self.pkce = Some(PkceState {
            code_verifier: verifier,
            state: state.clone(),
        });

        // Use url::Url to properly encode all query parameters.
        let mut url = url::Url::parse(AUTH_URL).expect("AUTH_URL is valid");
        url.query_pairs_mut()
            .append_pair("code", "true")
            .append_pair("client_id", &self.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", CALLBACK_URL)
            .append_pair("scope", SCOPES)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state);

        url.to_string()
    }

    /// Exchange an authorization code for access + refresh tokens.
    pub async fn exchange_code(&mut self, code: &str) -> Result<()> {
        let pkce = self.pkce.take().ok_or_else(|| {
            GatewayError::Config(
                "no pending PKCE state — call /auth first to start the flow".into(),
            )
        })?;

        let body = serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": &self.client_id,
            "code": code,
            "state": &pkce.state,
            "redirect_uri": CALLBACK_URL,
            "code_verifier": &pkce.code_verifier,
        });

        let resp = self
            .http_client
            .post(&self.token_url)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::Config(format!(
                "token exchange failed: {body}"
            )));
        }

        let token_resp: TokenResponse = resp.json().await.map_err(|e| {
            GatewayError::Config(format!("failed to parse token response: {e}"))
        })?;

        let now = chrono::Utc::now().timestamp();
        self.tokens = Some(TokenData {
            access_token: token_resp.access_token,
            refresh_token: token_resp.refresh_token,
            expires_at: now + token_resp.expires_in,
        });

        self.save().await?;
        info!("OAuth tokens obtained and saved");
        Ok(())
    }

    /// Refresh the access token using the stored refresh token.
    pub async fn refresh(&mut self) -> Result<()> {
        let refresh_token = self
            .tokens
            .as_ref()
            .map(|t| t.refresh_token.clone())
            .ok_or_else(|| GatewayError::Config("no refresh token available".into()))?;

        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": &self.client_id,
            "refresh_token": &refresh_token,
        });

        debug!("refreshing OAuth access token");

        let resp = self
            .http_client
            .post(&self.token_url)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            // invalid_grant = refresh token permanently revoked/expired
            if body.contains("invalid_grant") {
                self.tokens = None;
                let _ = tokio::fs::remove_file(&self.file_path).await;
                info!("cleared invalid OAuth tokens");
                return Err(GatewayError::Config(
                    "refresh token expired — please re-authorize with /auth".into(),
                ));
            }
            return Err(GatewayError::Config(format!(
                "token refresh failed: {body}"
            )));
        }

        let token_resp: TokenResponse = resp.json().await.map_err(|e| {
            GatewayError::Config(format!("failed to parse refresh response: {e}"))
        })?;

        let now = chrono::Utc::now().timestamp();
        self.tokens = Some(TokenData {
            access_token: token_resp.access_token,
            refresh_token: token_resp.refresh_token,
            expires_at: now + token_resp.expires_in,
        });

        self.save().await?;
        info!("OAuth access token refreshed");
        Ok(())
    }

    // -- Token access -------------------------------------------------------

    /// Return a valid access token, automatically refreshing if it expires
    /// within 5 minutes.
    pub async fn get_token(&mut self) -> Result<String> {
        let needs_refresh = match &self.tokens {
            Some(t) => {
                let now = chrono::Utc::now().timestamp();
                now >= t.expires_at - 300 // 5 minutes before expiry
            }
            None => {
                return Err(GatewayError::Config(
                    "not authenticated — use /auth to log in".into(),
                ));
            }
        };

        if needs_refresh {
            self.refresh().await?;
        }

        Ok(self
            .tokens
            .as_ref()
            .map(|t| t.access_token.clone())
            .unwrap_or_default())
    }

    // -- Persistence --------------------------------------------------------

    /// Load tokens from the configured file path.  Silently succeeds if the
    /// file does not exist (no prior tokens).
    pub async fn load(&mut self) -> Result<()> {
        match tokio::fs::read_to_string(&self.file_path).await {
            Ok(contents) => {
                let data: TokenData = serde_json::from_str(&contents).map_err(|e| {
                    GatewayError::Config(format!("failed to parse token file: {e}"))
                })?;
                debug!(file = %self.file_path.display(), "loaded OAuth tokens from file");
                self.tokens = Some(data);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!(file = %self.file_path.display(), "no token file found (first run)");
                Ok(())
            }
            Err(e) => Err(GatewayError::Io(e)),
        }
    }

    /// Persist current tokens to the configured file path.
    pub async fn save(&self) -> Result<()> {
        if let Some(ref tokens) = self.tokens {
            let json = serde_json::to_string_pretty(tokens).map_err(|e| {
                GatewayError::Config(format!("failed to serialize tokens: {e}"))
            })?;
            tokio::fs::write(&self.file_path, json).await?;
            debug!(file = %self.file_path.display(), "saved OAuth tokens to file");
        }
        Ok(())
    }

    // -- Status helpers -----------------------------------------------------

    /// Clear all tokens and PKCE state.
    pub fn clear(&mut self) {
        self.tokens = None;
        self.pkce = None;
        info!("OAuth tokens cleared");
    }

    /// Whether we have a stored token (may be expired).
    pub fn is_authenticated(&self) -> bool {
        self.tokens.is_some()
    }

    /// Return a human-readable status string.
    pub fn status(&self) -> String {
        match &self.tokens {
            Some(t) => {
                let now = chrono::Utc::now().timestamp();
                let remaining = t.expires_at - now;
                if remaining > 0 {
                    let hours = remaining / 3600;
                    let mins = (remaining % 3600) / 60;
                    format!(
                        "Authenticated (token expires in {}h {}m)",
                        hours, mins
                    )
                } else {
                    "Authenticated (token expired, will refresh on next request)".into()
                }
            }
            None => {
                if self.pkce.is_some() {
                    "Authorization in progress — waiting for code".into()
                } else {
                    "Not authenticated — use /auth to start".into()
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Token endpoint response
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    /// Seconds until the access token expires.
    expires_in: i64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_sha256_base64url() {
        let (verifier, challenge) = TokenStore::generate_pkce();

        // Verifier should be 64 hex chars (2 UUID v4 simple = 32 hex each)
        assert_eq!(verifier.len(), 64);

        // Verify the challenge = base64url(SHA256(verifier))
        let expected_hash = Sha256::digest(verifier.as_bytes());
        let expected_challenge = URL_SAFE_NO_PAD.encode(expected_hash);
        assert_eq!(challenge, expected_challenge);

        // base64url-no-pad should not contain + / =
        assert!(!challenge.contains('+'));
        assert!(!challenge.contains('/'));
        assert!(!challenge.contains('='));
    }

    #[test]
    fn pkce_produces_unique_values() {
        let (v1, _) = TokenStore::generate_pkce();
        let (v2, _) = TokenStore::generate_pkce();
        assert_ne!(v1, v2, "two PKCE verifiers should differ");
    }

    #[test]
    fn start_auth_returns_valid_url() {
        let mut store = TokenStore::new(
            "test-client-id".into(),
            None,
            PathBuf::from("/tmp/test_tokens.json"),
            reqwest::Client::new(),
        );

        let url = store.start_auth();
        assert!(url.starts_with(AUTH_URL));
        assert!(url.contains("client_id=test-client-id"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri="));
        assert!(url.contains("state="), "URL must include state parameter");
        assert!(url.contains("code=true"), "URL must include code=true");
        assert!(store.pkce.is_some(), "PKCE state should be set");
    }

    #[tokio::test]
    async fn load_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");

        let mut store = TokenStore::new(
            "cid".into(),
            None,
            path.clone(),
            reqwest::Client::new(),
        );

        // Save tokens
        store.tokens = Some(TokenData {
            access_token: "sk-ant-oat01-test".into(),
            refresh_token: "sk-ant-ort01-test".into(),
            expires_at: 1700000000,
        });
        store.save().await.unwrap();

        // Load into a fresh store
        let mut store2 = TokenStore::new(
            "cid".into(),
            None,
            path,
            reqwest::Client::new(),
        );
        store2.load().await.unwrap();
        let loaded = store2.tokens.unwrap();
        assert_eq!(loaded.access_token, "sk-ant-oat01-test");
        assert_eq!(loaded.refresh_token, "sk-ant-ort01-test");
        assert_eq!(loaded.expires_at, 1700000000);
    }

    #[tokio::test]
    async fn load_missing_file_succeeds() {
        let mut store = TokenStore::new(
            "cid".into(),
            None,
            PathBuf::from("/tmp/nonexistent_oauth_test_file.json"),
            reqwest::Client::new(),
        );
        // Should not error
        store.load().await.unwrap();
        assert!(store.tokens.is_none());
    }

    #[test]
    fn token_expiry_detection() {
        let now = chrono::Utc::now().timestamp();
        let store = TokenStore {
            client_id: String::new(),
            token_url: String::new(),
            file_path: PathBuf::new(),
            tokens: Some(TokenData {
                access_token: "test".into(),
                refresh_token: "test".into(),
                expires_at: now + 100, // expires in 100s (< 5 min)
            }),
            pkce: None,
            http_client: reqwest::Client::new(),
        };
        // Token expires within 5 minutes, so needs_refresh should be true
        let needs_refresh = store.tokens.as_ref().map_or(false, |t| {
            chrono::Utc::now().timestamp() >= t.expires_at - 300
        });
        assert!(needs_refresh, "token expiring in 100s should need refresh");
    }

    #[test]
    fn clear_removes_tokens_and_pkce() {
        let mut store = TokenStore::new(
            "cid".into(),
            None,
            PathBuf::new(),
            reqwest::Client::new(),
        );
        store.tokens = Some(TokenData {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 0,
        });
        store.pkce = Some(PkceState {
            code_verifier: "v".into(),
            state: "s".into(),
        });

        store.clear();
        assert!(store.tokens.is_none());
        assert!(store.pkce.is_none());
    }

    #[test]
    fn status_messages() {
        let mut store = TokenStore::new(
            "cid".into(),
            None,
            PathBuf::new(),
            reqwest::Client::new(),
        );

        // Not authenticated
        assert!(store.status().contains("Not authenticated"));

        // Auth in progress
        store.pkce = Some(PkceState {
            code_verifier: "v".into(),
            state: "s".into(),
        });
        assert!(store.status().contains("in progress"));

        // Authenticated with valid token
        store.pkce = None;
        store.tokens = Some(TokenData {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: chrono::Utc::now().timestamp() + 7200,
        });
        let s = store.status();
        assert!(s.contains("Authenticated"), "got: {s}");
        assert!(s.contains("expires in"), "got: {s}");

        // Expired token
        store.tokens = Some(TokenData {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: chrono::Utc::now().timestamp() - 100,
        });
        assert!(store.status().contains("expired"));
    }

    #[tokio::test]
    async fn exchange_code_without_pkce_errors() {
        let mut store = TokenStore::new(
            "cid".into(),
            None,
            PathBuf::new(),
            reqwest::Client::new(),
        );
        // No start_auth() called, so no PKCE state
        let err = store.exchange_code("some-code").await.unwrap_err();
        match err {
            GatewayError::Config(msg) => assert!(msg.contains("no pending PKCE")),
            other => panic!("expected Config error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn exchange_code_with_mock() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "sk-ant-oat01-mock",
                "refresh_token": "sk-ant-ort01-mock",
                "expires_in": 43200
            })))
            .mount(&mock_server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");

        let mut store = TokenStore::new(
            "test-client".into(),
            Some(format!("{}/v1/oauth/token", mock_server.uri())),
            path,
            reqwest::Client::new(),
        );

        // Must start auth first to set PKCE state
        let _url = store.start_auth();

        // Exchange code
        store.exchange_code("test-auth-code").await.unwrap();

        let tokens = store.tokens.as_ref().unwrap();
        assert_eq!(tokens.access_token, "sk-ant-oat01-mock");
        assert_eq!(tokens.refresh_token, "sk-ant-ort01-mock");
        assert!(tokens.expires_at > chrono::Utc::now().timestamp());
    }

    #[tokio::test]
    async fn refresh_with_mock() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "sk-ant-oat01-refreshed",
                "refresh_token": "sk-ant-ort01-refreshed",
                "expires_in": 43200
            })))
            .mount(&mock_server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");

        let mut store = TokenStore::new(
            "test-client".into(),
            Some(format!("{}/v1/oauth/token", mock_server.uri())),
            path,
            reqwest::Client::new(),
        );

        // Set existing tokens (expired)
        store.tokens = Some(TokenData {
            access_token: "old".into(),
            refresh_token: "sk-ant-ort01-original".into(),
            expires_at: 0,
        });

        store.refresh().await.unwrap();

        let tokens = store.tokens.as_ref().unwrap();
        assert_eq!(tokens.access_token, "sk-ant-oat01-refreshed");
        assert_eq!(tokens.refresh_token, "sk-ant-ort01-refreshed");
    }
}
