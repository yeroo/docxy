//! OAuth2 authorization-code + PKCE flow against Microsoft Entra ID.
//!
//! Device-code flow is blocked by EPAM Conditional Access, so lookxy signs
//! in interactively: a system-browser `/authorize` round trip with a
//! `http://localhost:<port>` loopback redirect (the loopback listener
//! itself lives elsewhere — this module is pure request/response so it's
//! fully fake-server-testable). Token values and the PKCE code verifier are
//! never logged.

use crate::json;
use crate::pkce::{self, Pkce};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// Which tenant/app registration/scopes to authenticate against.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub authority: String,
    pub client_id: String,
    pub scope: String,
}

impl Default for AuthConfig {
    /// The validated client (Microsoft Graph CLI, a public client
    /// preauthorized for Graph) against the `organizations` multi-tenant
    /// endpoint — see the auth spike recorded in the project plan.
    fn default() -> AuthConfig {
        AuthConfig {
            authority: "https://login.microsoftonline.com/organizations".to_string(),
            client_id: "14d82eec-204b-4c2f-b7e8-296a70dab67e".to_string(),
            scope: "Mail.ReadWrite People.Read offline_access".to_string(),
        }
    }
}

/// Access + refresh tokens returned by the token endpoint.
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_unix: u64,
    pub account: String,
}

/// Everything needed to drive the browser through `/authorize` and match up
/// the loopback redirect it lands on.
pub struct AuthRequest {
    pub authorize_url: String,
    pub redirect_uri: String,
    pub state: String,
    pkce: Pkce,
}

/// Errors from the token endpoint or from parsing its response.
#[derive(Debug, Clone)]
pub enum AuthError {
    /// The server rejected the request (non-2xx with a parseable OAuth
    /// error body): `error`/`error_description`.
    Denied(String),
    /// Transport-level failure, or a non-2xx response with no parseable
    /// OAuth error body.
    Http(String),
    /// The response body wasn't the JSON shape we expected.
    Parse(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::Denied(m) => write!(f, "authorization denied: {m}"),
            AuthError::Http(m) => write!(f, "HTTP error: {m}"),
            AuthError::Parse(m) => write!(f, "failed to parse token response: {m}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// The tenant segment for the `/oauth2/v2.0/...` endpoints: the last path
/// segment of `authority` (e.g. `.../organizations` -> `organizations`).
/// `authority` is user-overridable, so tolerate a trailing slash (which
/// would otherwise yield an empty tenant and a malformed `//oauth2/...`
/// URL) by trimming it first.
fn tenant_of(authority: &str) -> &str {
    let authority = authority.trim_end_matches('/');
    authority.rsplit('/').next().unwrap_or(authority)
}

/// Builds the `/authorize` URL to open in the system browser, along with
/// the PKCE verifier and state needed to redeem the code it redirects back
/// with.
pub fn begin_auth(cfg: &AuthConfig, redirect_uri: &str) -> AuthRequest {
    let tenant = tenant_of(&cfg.authority);
    let pkce = Pkce::generate();
    // Reuse the same OS-randomness-backed generator for the anti-CSRF
    // `state` value; only its verifier (32 random bytes, base64url) is
    // used, the paired challenge is simply discarded.
    let state = Pkce::generate().verifier;

    let query = pkce::form_urlencode(&[
        ("client_id", cfg.client_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("response_mode", "query"),
        ("scope", cfg.scope.as_str()),
        ("state", state.as_str()),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("prompt", "select_account"),
    ]);
    let authorize_url =
        format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize?{query}");

    AuthRequest {
        authorize_url,
        redirect_uri: redirect_uri.to_string(),
        state,
        pkce,
    }
}

/// Redeems the authorization code returned on the loopback redirect for a
/// token set. `http_base` is normally `https://login.microsoftonline.com`;
/// tests point it at a fake server.
pub fn redeem_code(
    cfg: &AuthConfig,
    http_base: &str,
    req: &AuthRequest,
    code: &str,
) -> Result<TokenSet, AuthError> {
    let body = pkce::form_urlencode(&[
        ("grant_type", "authorization_code"),
        ("client_id", cfg.client_id.as_str()),
        ("code", code),
        ("redirect_uri", req.redirect_uri.as_str()),
        ("code_verifier", req.pkce.verifier.as_str()),
        ("scope", cfg.scope.as_str()),
    ]);
    // No prior refresh token exists yet on this path, so there's nothing
    // to fall back to if the server omits one.
    post_token(cfg, http_base, &body, "")
}

/// Exchanges a refresh token for a fresh token set. Entra ID may omit
/// `refresh_token` from the response (refresh tokens aren't always
/// rotated); when it does, the caller's `refresh_token` is carried forward
/// into the returned `TokenSet` rather than being silently replaced with
/// an empty string.
pub fn refresh(
    cfg: &AuthConfig,
    http_base: &str,
    refresh_token: &str,
) -> Result<TokenSet, AuthError> {
    let body = pkce::form_urlencode(&[
        ("grant_type", "refresh_token"),
        ("client_id", cfg.client_id.as_str()),
        ("refresh_token", refresh_token),
        ("scope", cfg.scope.as_str()),
    ]);
    post_token(cfg, http_base, &body, refresh_token)
}

fn post_token(
    cfg: &AuthConfig,
    http_base: &str,
    body: &str,
    fallback_refresh_token: &str,
) -> Result<TokenSet, AuthError> {
    let tenant = tenant_of(&cfg.authority);
    let url = format!("{http_base}/{tenant}/oauth2/v2.0/token");

    let response_body = match ureq::post(&url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(body)
    {
        Ok(resp) => resp
            .into_string()
            .map_err(|e| AuthError::Http(e.to_string()))?,
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            return Err(classify_error(status, &body));
        }
        Err(ureq::Error::Transport(t)) => return Err(AuthError::Http(t.to_string())),
    };

    parse_token_response(&response_body, fallback_refresh_token)
}

/// Interprets a non-2xx token-endpoint response. Entra ID error bodies are
/// JSON (`{"error":"...","error_description":"..."}`); when we can parse
/// one out, surface it as `Denied` (the description is diagnostic text
/// from the server, never a token or the code verifier). Anything else
/// becomes an opaque `Http` error.
fn classify_error(status: u16, body: &str) -> AuthError {
    if let Ok(v) = json::parse(body) {
        if let Some(desc) = v.get("error_description").and_then(|d| d.as_str()) {
            return AuthError::Denied(desc.to_string());
        }
        if let Some(err) = v.get("error").and_then(|d| d.as_str()) {
            return AuthError::Denied(err.to_string());
        }
    }
    AuthError::Http(format!("HTTP {status}: {body}"))
}

fn parse_token_response(body: &str, fallback_refresh_token: &str) -> Result<TokenSet, AuthError> {
    let v = json::parse(body).map_err(|e| AuthError::Parse(e.to_string()))?;

    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| AuthError::Parse("response has no access_token".to_string()))?
        .to_string();
    // Entra ID doesn't always rotate/re-send `refresh_token` on a refresh
    // response; when it's absent, keep whatever refresh token the caller
    // already had rather than overwriting it with an empty string.
    let refresh_token = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or(fallback_refresh_token)
        .to_string();
    let expires_in = v
        .get("expires_in")
        .and_then(|x| x.as_i64())
        .unwrap_or(0)
        .max(0) as u64;
    let expires_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + expires_in;
    let account = v
        .get("id_token")
        .and_then(|x| x.as_str())
        .and_then(preferred_username)
        .unwrap_or_default();

    Ok(TokenSet {
        access_token,
        refresh_token,
        expires_at_unix,
        account,
    })
}

/// Extracts the `preferred_username` claim from a JWT's (unverified)
/// payload segment. The loopback flow already binds the response to this
/// process via `state` and the token came straight from Entra ID over TLS,
/// so this is read for display purposes, not as an authorization decision.
fn preferred_username(id_token: &str) -> Option<String> {
    let payload_b64 = id_token.split('.').nth(1)?;
    let payload_bytes = pkce::base64url_decode(payload_b64)?;
    let payload_str = std::str::from_utf8(&payload_bytes).ok()?;
    let v = json::parse(payload_str).ok()?;
    v.get("preferred_username")?.as_str().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{FakeServer, Route};

    fn cfg() -> AuthConfig {
        AuthConfig {
            authority: "x/organizations".into(),
            client_id: "cid".into(),
            scope: "Mail.ReadWrite People.Read offline_access".into(),
        }
    }

    #[test]
    fn begin_auth_builds_authorize_url_with_challenge_and_state() {
        let req = begin_auth(&cfg(), "http://localhost:8400");
        assert!(
            req.authorize_url
                .contains("/organizations/oauth2/v2.0/authorize")
        );
        assert!(req.authorize_url.contains("client_id=cid"));
        assert!(req.authorize_url.contains("code_challenge="));
        assert!(req.authorize_url.contains("code_challenge_method=S256"));
        assert!(req.authorize_url.contains(&format!("state={}", req.state)));
        assert!(
            req.authorize_url
                .contains("redirect_uri=http%3A%2F%2Flocalhost%3A8400")
        );
    }

    #[test]
    fn redeem_code_returns_token() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/organizations/oauth2/v2.0/token".into(),
            status: 200,
            headers: vec![],
            body: r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600,"scope":"Mail.ReadWrite"}"#
                .into(),
        }]);
        let req = begin_auth(&cfg(), "http://localhost:8400");
        let t = redeem_code(&cfg(), &srv.base_url, &req, "THECODE").unwrap();
        assert_eq!(t.access_token, "AT");
        assert_eq!(t.refresh_token, "RT");
        assert!(t.expires_at_unix > 0);
        // the verifier must have been sent
        let reqs = srv.requests();
        assert!(reqs[0].body.contains("code_verifier="));
        assert!(reqs[0].body.contains("grant_type=authorization_code"));
    }

    #[test]
    fn refresh_swaps_tokens() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/organizations/oauth2/v2.0/token".into(),
            status: 200,
            headers: vec![],
            body: r#"{"access_token":"AT2","refresh_token":"RT2","expires_in":3600}"#.into(),
        }]);
        let t = refresh(&cfg(), &srv.base_url, "RT1").unwrap();
        assert_eq!(t.access_token, "AT2");
        assert_eq!(t.refresh_token, "RT2");
    }

    #[test]
    fn refresh_missing_refresh_token_reuses_old() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/organizations/oauth2/v2.0/token".into(),
            status: 200,
            headers: vec![],
            body: r#"{"access_token":"AT2","expires_in":3600}"#.into(),
        }]);
        let t = refresh(&cfg(), &srv.base_url, "RT1").unwrap();
        assert_eq!(t.access_token, "AT2");
        assert_eq!(t.refresh_token, "RT1");
    }

    #[test]
    fn tenant_of_trims_trailing_slash() {
        assert_eq!(
            tenant_of("https://login.microsoftonline.com/organizations/"),
            "organizations"
        );
    }
}
