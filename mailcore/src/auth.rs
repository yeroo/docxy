//! OAuth2 authorization-code + PKCE flow against Microsoft Entra ID.
//!
//! Device-code flow is blocked by EPAM Conditional Access, so lookxy signs
//! in interactively: a system-browser `/authorize` round trip with a
//! `http://localhost:<port>` loopback redirect (the loopback listener
//! itself lives elsewhere — this module is pure request/response so it's
//! fully fake-server-testable). Token values and the PKCE code verifier are
//! never logged.

use crate::graph::client::http_error_text;
use crate::json;
use crate::pkce::{self, Pkce};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// The Microsoft cloud a tenant lives in (#2225): where it signs in and
/// where its Graph is. Tokens of one cloud are not accepted by another, so
/// a saved sign-in records its cloud ([`TokenSet::cloud`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Cloud {
    /// Microsoft's global cloud (`login.microsoftonline.com`,
    /// `graph.microsoft.com`).
    #[default]
    Global,
    /// US Government L4 (GCC High).
    UsGov,
    /// US Government L5 (DoD). Shares its login host with [`Cloud::UsGov`],
    /// so the cloud cannot be told from the authority.
    UsGovDod,
    /// China, operated by 21Vianet.
    China,
}

impl Cloud {
    /// Every cloud mailcore knows.
    pub const ALL: [Cloud; 4] = [Cloud::Global, Cloud::UsGov, Cloud::UsGovDod, Cloud::China];

    /// The cloud's Entra ID login host, the base of `/authorize` and the
    /// token endpoint (no trailing slash).
    pub fn login_host(self) -> &'static str {
        match self {
            Cloud::Global => "https://login.microsoftonline.com",
            Cloud::UsGov | Cloud::UsGovDod => "https://login.microsoftonline.us",
            Cloud::China => "https://login.chinacloudapi.cn",
        }
    }

    /// The cloud's Microsoft Graph root (no trailing slash, no version):
    /// the Graph base is this + `/v1.0`, and a Graph scope spelled with a
    /// root carries this one.
    pub fn graph_root(self) -> &'static str {
        match self {
            Cloud::Global => "https://graph.microsoft.com",
            Cloud::UsGov => "https://graph.microsoft.us",
            Cloud::UsGovDod => "https://dod-graph.microsoft.us",
            Cloud::China => "https://microsoftgraph.chinacloudapi.cn",
        }
    }

    /// The name a saved token records (`"global"`, `"usgov"`, `"usgovdod"`,
    /// `"china"`).
    pub fn name(self) -> &'static str {
        match self {
            Cloud::Global => "global",
            Cloud::UsGov => "usgov",
            Cloud::UsGovDod => "usgovdod",
            Cloud::China => "china",
        }
    }

    /// The cloud [`Cloud::name`] names (exactly); `None` for any other text.
    pub fn from_name(name: &str) -> Option<Cloud> {
        Cloud::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// Which tenant/app registration/scopes to authenticate against.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Names the tenant: its last path segment (`…/organizations`,
    /// `…/<tenant id>`). Its host is not used; the login host is
    /// `cloud`'s.
    pub authority: String,
    pub client_id: String,
    /// The scopes the client needs (space-separated).
    pub scope: String,
    /// The scopes the client can do without (space-separated, empty by
    /// default; #2218), such as other people's photos and presence. An
    /// interactive sign-in asks `scope` and these together
    /// ([`sign_in_scope`]). When a tenant refuses that sign-in for consent,
    /// the engine signs in again asking `scope` alone, and the missing
    /// optional scopes are reported in `SyncEvent::Scopes { missing }`. A
    /// refresh asks for an optional scope only once it was granted
    /// ([`refresh_scope_for`]).
    pub optional_scope: String,
    /// The Microsoft cloud to sign in to and sync with (#2225): its login
    /// host and Graph root. [`Cloud::Global`] by default.
    pub cloud: Cloud,
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
            optional_scope: String::new(),
            cloud: Cloud::Global,
        }
    }
}

/// Access + refresh tokens returned by the token endpoint.
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_unix: u64,
    pub account: String,
    /// The scopes the token was granted, as the token response's `scope`
    /// spelled them (Graph scopes usually carry the cloud's Graph root as a
    /// prefix, `https://graph.microsoft.com/` in the global cloud). `None`
    /// is unknown: a response without `scope` (or an empty one), or a cache
    /// saved before #1875. Compare them with [`missing_scopes`].
    pub scopes: Option<Vec<String>>,
    /// The cloud that issued the token (#2225): the `AuthConfig::cloud` of
    /// the sign-in or refresh. A cache saved before #2225 loads as
    /// [`Cloud::Global`]. The engine never uses a token of another cloud
    /// than its own.
    pub cloud: Cloud,
}

/// Everything needed to drive the browser through `/authorize` and match up
/// the loopback redirect it lands on.
pub struct AuthRequest {
    pub authorize_url: String,
    pub redirect_uri: String,
    pub state: String,
    /// The scopes `/authorize` was asked for; [`redeem_code`] asks the same
    /// (#2218).
    scope: String,
    pkce: Pkce,
}

impl AuthRequest {
    /// The scopes this request's `/authorize` asks for (space-separated).
    pub fn scope(&self) -> &str {
        &self.scope
    }
}

/// Errors from the token endpoint or from parsing its response.
#[derive(Debug, Clone)]
pub enum AuthError {
    /// The server rejected the request (non-2xx with a parseable OAuth
    /// error body): its `error` code (empty when the body has only an
    /// `error_description`) and `error_description`. Whether it means the
    /// grant itself was refused is [`AuthError::needs_sign_in`] (#2167).
    Denied {
        error: String,
        description: Option<String>,
    },
    /// Transport-level failure, or a response body that could not be read.
    Http(String),
    /// A non-2xx response with no parseable OAuth error body (#2113,
    /// #1959): often a front end's HTML error page. `Display` shows one
    /// capped line (`HTTP 502 Bad Gateway`), never the body; the body is
    /// kept here for diagnostics (`Debug`).
    Status { status: u16, body: String },
    /// The response body wasn't the JSON shape we expected.
    Parse(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::Denied { error, description } => write!(
                f,
                "authorization denied: {}",
                description.as_deref().unwrap_or(error)
            ),
            AuthError::Http(m) => write!(f, "HTTP error: {m}"),
            AuthError::Status { status, body } => {
                write!(f, "HTTP error: {}", http_error_text(*status, body))
            }
            AuthError::Parse(m) => write!(f, "failed to parse token response: {m}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// The OAuth `error` codes that refuse the grant itself (#2167): the
/// refresh token, the consent or the client is no good, and only an
/// interactive sign-in helps.
const REFUSAL_CODES: [&str; 7] = [
    "invalid_grant",
    "interaction_required",
    "consent_required",
    "login_required",
    "invalid_client",
    "unauthorized_client",
    "invalid_scope",
];

impl AuthError {
    /// Whether this failure refuses the grant, so the person has to sign
    /// in again (#2167): an OAuth error body whose `error` is one of
    /// [`REFUSAL_CODES`] (any case), whatever the HTTP status. Everything
    /// else is transient: a transport failure or a body that could not be
    /// read (`Http`), a non-2xx with no OAuth body (`Status`: a 5xx, 429 or
    /// 408, a front end's HTML page), a 2xx that does not parse (`Parse`),
    /// and any other OAuth code (`temporarily_unavailable`,
    /// `server_error`, ...). When unsure, transient: being offline for a
    /// while is recoverable, a thrown-away sign-in is not.
    pub fn needs_sign_in(&self) -> bool {
        match self {
            AuthError::Denied { error, .. } => REFUSAL_CODES
                .iter()
                .any(|c| c.eq_ignore_ascii_case(error.trim())),
            AuthError::Http(_) | AuthError::Status { .. } | AuthError::Parse(_) => false,
        }
    }

    /// The OAuth `error` code of a refusal, trimmed and ASCII-lowercased
    /// (#2169), as [`AuthError::needs_sign_in`] compares it: `Some` for
    /// [`AuthError::Denied`] (its code may be empty), `None` for any other
    /// error.
    pub fn refusal_code(&self) -> Option<String> {
        match self {
            AuthError::Denied { error, .. } => Some(error.trim().to_ascii_lowercase()),
            AuthError::Http(_) | AuthError::Status { .. } | AuthError::Parse(_) => None,
        }
    }

    /// The Entra error number of a refusal (#2169): `Some(n)` when this is
    /// [`AuthError::Denied`] whose `error_description`, leading whitespace
    /// trimmed, starts `AADSTS<n>:` with `n` fitting a `u32`
    /// (`"AADSTS70008: The refresh token has expired …"` is `70008`).
    /// `None` otherwise: no description, no colon after the digits, no
    /// digits, a number too large, or any other error. The number names
    /// the cause (inactivity, a password change, MFA) without the
    /// description's text, which may name the user or the tenant.
    pub fn aadsts(&self) -> Option<u32> {
        let AuthError::Denied {
            description: Some(d),
            ..
        } = self
        else {
            return None;
        };
        aadsts_of(d)
    }

    /// Whether the token endpoint refused for consent (#2218): a
    /// [`AuthError::Denied`] that [`is_consent_refusal`] holds for. False
    /// for every other error.
    pub fn consent_refused(&self) -> bool {
        match self {
            AuthError::Denied { error, description } => {
                is_consent_refusal(error, description.as_deref())
            }
            AuthError::Http(_) | AuthError::Status { .. } | AuthError::Parse(_) => false,
        }
    }
}

/// The Entra error number at the start of an `error_description`: `Some(n)`
/// when `desc`, leading whitespace trimmed, starts `AADSTS<n>:` with `n`
/// fitting a `u32`, else `None` (#2169, [`AuthError::aadsts`]).
fn aadsts_of(desc: &str) -> Option<u32> {
    let rest = desc.trim_start().strip_prefix("AADSTS")?;
    let (digits, _) = rest.split_once(':')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// AADSTS65001: the user or administrator has not consented to a scope.
const AADSTS_NO_CONSENT: u32 = 65001;
/// AADSTS65004: the user declined to consent.
const AADSTS_CONSENT_DECLINED: u32 = 65004;

/// Whether an OAuth refusal (an `/authorize` redirect's or the token
/// endpoint's `error` and `error_description`) is a refusal for consent
/// (#2218): `error` (trimmed, any ASCII case) is `consent_required`, or the
/// description's leading `AADSTS<n>:` is 65001 (not consented; Entra sends
/// it under `consent_required`, `interaction_required` or `invalid_grant`)
/// or 65004 (the person declined the consent screen; under
/// `access_denied`). A bare `access_denied`, which a person cancelling the
/// account picker also gets, is not.
pub(crate) fn is_consent_refusal(error: &str, description: Option<&str>) -> bool {
    error.trim().eq_ignore_ascii_case("consent_required")
        || matches!(
            description.and_then(aadsts_of),
            Some(AADSTS_NO_CONSENT | AADSTS_CONSENT_DECLINED)
        )
}

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
/// with. It asks [`sign_in_scope`]: `cfg.scope` and `cfg.optional_scope`.
pub fn begin_auth(cfg: &AuthConfig, redirect_uri: &str) -> AuthRequest {
    begin_auth_with_scope(cfg, redirect_uri, &sign_in_scope(cfg))
}

/// [`begin_auth`] asking `scope` (space-separated) instead (#2218): the
/// engine's consent fallback asks `cfg.scope` alone. The URL is on
/// `cfg.cloud`'s login host (#2225), and the scopes asked are the wire
/// scope of `scope` ([`wire_scope`]).
pub fn begin_auth_with_scope(cfg: &AuthConfig, redirect_uri: &str, scope: &str) -> AuthRequest {
    let tenant = tenant_of(&cfg.authority);
    let scope = wire_scope(cfg.cloud, scope);
    let scope = scope.as_str();
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
        ("scope", scope),
        ("state", state.as_str()),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("prompt", "select_account"),
    ]);
    let authorize_url = format!(
        "{}/{tenant}/oauth2/v2.0/authorize?{query}",
        cfg.cloud.login_host()
    );

    AuthRequest {
        authorize_url,
        redirect_uri: redirect_uri.to_string(),
        state,
        scope: scope.to_string(),
        pkce,
    }
}

/// Redeems the authorization code returned on the loopback redirect for a
/// token set, asking the scopes `req`'s `/authorize` asked
/// ([`AuthRequest::scope`], #2218). `http_base` is normally
/// `cfg.cloud`'s login host ([`Cloud::login_host`]); tests point it at a
/// fake server.
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
        ("scope", req.scope.as_str()),
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
///
/// `granted` is the saved token's grant ([`TokenSet::scopes`]); the
/// refresh asks for [`refresh_scope_for`] of it (#2164, #2218), not
/// `cfg.scope` in full. Entra's refresh-token grant refuses a scope the
/// person never consented to (`invalid_grant`, AADSTS65001), so a scope a
/// host has just added to `cfg.scope` would sign every saved sign-in out at
/// its next refresh. Asked only for the grant, the token keeps working and
/// the new scope is reported missing (`SyncEvent::Scopes`); the interactive
/// sign-in ([`begin_auth`], [`redeem_code`]) still asks [`sign_in_scope`]
/// in full. The scopes are sent as their wire scope ([`wire_scope`]).
pub fn refresh(
    cfg: &AuthConfig,
    http_base: &str,
    refresh_token: &str,
    granted: Option<&[String]>,
) -> Result<TokenSet, AuthError> {
    let scope = wire_scope(cfg.cloud, &refresh_scope_for(cfg, granted));
    let body = pkce::form_urlencode(&[
        ("grant_type", "refresh_token"),
        ("client_id", cfg.client_id.as_str()),
        ("refresh_token", refresh_token),
        ("scope", scope.as_str()),
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

    parse_token_response(&response_body, fallback_refresh_token, cfg.cloud)
}

/// Interprets a non-2xx token-endpoint response. Entra ID error bodies are
/// JSON (`{"error":"...","error_description":"..."}`); when we can parse
/// one out, surface it as `Denied` with its code (#2167) and description
/// (diagnostic text from the server, never a token or the code verifier).
/// Anything else becomes a `Status` error, which keeps the body but
/// displays one line.
fn classify_error(status: u16, body: &str) -> AuthError {
    if let Ok(v) = json::parse(body) {
        let error = v.get("error").and_then(|d| d.as_str());
        let description = v.get("error_description").and_then(|d| d.as_str());
        if error.is_some() || description.is_some() {
            return AuthError::Denied {
                error: error.unwrap_or_default().to_string(),
                description: description.map(str::to_string),
            };
        }
    }
    AuthError::Status {
        status,
        body: body.to_string(),
    }
}

fn parse_token_response(
    body: &str,
    fallback_refresh_token: &str,
    cloud: Cloud,
) -> Result<TokenSet, AuthError> {
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
    let scopes = v
        .get("scope")
        .and_then(|x| x.as_str())
        .and_then(split_scopes);

    Ok(TokenSet {
        access_token,
        refresh_token,
        expires_at_unix,
        account,
        scopes,
        cloud,
    })
}

/// The granted scopes of a token response's `scope` (space-separated).
/// Empty or whitespace only is `None`, like an absent `scope`: nothing is
/// known. On a refresh the engine then keeps the previous grant: Entra
/// always names the granted scopes, so the last grant it did name stands,
/// rather than assuming everything requested was granted (RFC 6749 §5.1's
/// reading of an omitted `scope`), which would hide a missing scope.
fn split_scopes(scope: &str) -> Option<Vec<String>> {
    let scopes: Vec<String> = scope.split_ascii_whitespace().map(str::to_string).collect();
    (!scopes.is_empty()).then_some(scopes)
}

/// The length of the Graph root + `/` that `scope` starts with
/// (case-insensitively), of any cloud ([`Cloud::graph_root`]), with that
/// cloud; `None` when it starts with none.
fn graph_root_of(scope: &str) -> Option<(Cloud, usize)> {
    Cloud::ALL.into_iter().find_map(|cloud| {
        // Bytes, so a scope with non-ASCII text is never cut inside a
        // character; the root and `/` are ASCII, so `n` is a boundary.
        let (root, bytes) = (cloud.graph_root().as_bytes(), scope.as_bytes());
        (bytes.len() > root.len()
            && bytes[..root.len()].eq_ignore_ascii_case(root)
            && bytes[root.len()] == b'/')
            .then_some((cloud, root.len() + 1))
    })
}

/// A scope as compared: ASCII-lowercased, without a leading Graph root +
/// `/` of any cloud (matched case-insensitively; #2225), so
/// `Mail.ReadWrite`, `https://graph.microsoft.com/Mail.ReadWrite` and
/// `https://graph.microsoft.us/Mail.ReadWrite` are the same scope. A grant
/// carries its own cloud's root only, so stripping every cloud's keeps
/// [`missing_scopes`] free of the configured cloud.
pub(crate) fn normalise_scope(scope: &str) -> String {
    let rest = match graph_root_of(scope) {
        Some((_, n)) => &scope[n..],
        None => scope,
    };
    rest.to_ascii_lowercase()
}

/// The scopes as sent to `cloud`'s `/authorize` and token endpoint
/// (#2225). Microsoft's national-cloud docs: a scope spelled with a Graph
/// root must use the cloud's own Graph root, and short names
/// (`Mail.ReadWrite`) are valid as they are. So each space-separated
/// scope that starts with another cloud's Graph root + `/` gets `cloud`'s
/// root instead; every other scope is kept. When none is rewritten,
/// `scope` comes back byte for byte (a global caller's requests are as
/// before); otherwise the scopes are joined with single spaces.
pub(crate) fn wire_scope(cloud: Cloud, scope: &str) -> String {
    let mut rewritten = false;
    let scopes: Vec<String> = scope
        .split_ascii_whitespace()
        .map(|s| match graph_root_of(s) {
            Some((of, n)) if of != cloud => {
                rewritten = true;
                format!("{}/{}", cloud.graph_root(), &s[n..])
            }
            _ => s.to_string(),
        })
        .collect();
    if rewritten {
        scopes.join(" ")
    } else {
        scope.to_string()
    }
}

/// The OpenID Connect and meta scopes, which Entra does not list as granted
/// (or lists as it likes) and which name no Graph permission: never missing.
/// Takes a normalised scope.
fn is_meta_scope(normalised: &str) -> bool {
    matches!(
        normalised,
        "offline_access" | "openid" | "profile" | "email"
    ) || normalised == ".default"
        || normalised.ends_with("/.default")
}

/// The scopes of `requested` (space-separated, as in [`AuthConfig::scope`])
/// that `granted` lacks, spelled as `requested` writes them, each once, in
/// `requested`'s order. Compared case-insensitively and without the Graph
/// root prefix (any cloud's, [`Cloud::graph_root`]) Microsoft puts on
/// granted Graph scopes; `offline_access`, `openid`, `profile`, `email` and any `.default`
/// scope are never missing.
pub fn missing_scopes(requested: &str, granted: &[String]) -> Vec<String> {
    let granted: Vec<String> = granted.iter().map(|s| normalise_scope(s)).collect();
    requested_scopes(requested)
        .filter(|(_, n)| !is_meta_scope(n) && !granted.contains(n))
        .map(|(scope, _)| scope.to_string())
        .collect()
}

/// What a refresh asks for (#2164): the scopes of `requested` (as in
/// [`AuthConfig::scope`]) that `granted` holds, plus `requested`'s own meta
/// scopes (`offline_access`, `openid`, `profile`, `email`, `.default`),
/// spelled as `requested` writes them, each once, in its order, compared
/// as [`missing_scopes`] compares. A scope the grant lacks (one a host has
/// just added) is not asked for, nor is one the grant holds that
/// `requested` no longer names. Nothing is added: not even
/// `offline_access`, which is itself a consentable permission.
///
/// `requested` as it is when the grant is unknown (`None`), or when it
/// shares no scope with `requested` but meta ones: a refresh of only
/// `offline_access` would get a token for nothing the host uses, and asked
/// in full the refresh works or is refused, which signs out as before.
pub fn refresh_scope(requested: &str, granted: Option<&[String]>) -> String {
    let Some(granted) = granted else {
        return requested.to_string();
    };
    let granted: Vec<String> = granted.iter().map(|s| normalise_scope(s)).collect();
    let mut shares = false;
    let mut asked: Vec<&str> = Vec::new();
    for (scope, n) in requested_scopes(requested) {
        if is_meta_scope(&n) {
            asked.push(scope);
        } else if granted.contains(&n) {
            shares = true;
            asked.push(scope);
        }
    }
    if shares {
        asked.join(" ")
    } else {
        requested.to_string()
    }
}

/// The scopes of `cfg.optional_scope` that `cfg.scope` does not already
/// name, as `optional_scope` spells them, each once, in its order, compared
/// as [`missing_scopes`] compares (#2218).
fn extra_optional_scopes(cfg: &AuthConfig) -> Vec<(&str, String)> {
    let required: Vec<String> = requested_scopes(&cfg.scope).map(|(_, n)| n).collect();
    requested_scopes(&cfg.optional_scope)
        .filter(|(_, n)| !required.contains(n))
        .collect()
}

/// What an interactive sign-in asks for (#2218): `cfg.scope`, followed by
/// the scopes of `cfg.optional_scope` it does not already name (each once,
/// as `optional_scope` spells them, compared as [`missing_scopes`]
/// compares). Exactly `cfg.scope` when `optional_scope` adds nothing
/// (empty, blank, or only scopes `scope` names), so such a caller's
/// requests are as before. [`missing_scopes`] of it is what
/// `SyncEvent::Scopes { missing }` reports.
pub fn sign_in_scope(cfg: &AuthConfig) -> String {
    let extra = extra_optional_scopes(cfg);
    if extra.is_empty() {
        return cfg.scope.clone();
    }
    let mut asked: Vec<&str> = cfg.scope.split_ascii_whitespace().collect();
    asked.extend(extra.iter().map(|(scope, _)| *scope));
    asked.join(" ")
}

/// What a refresh asks for (#2164, #2218). With no optional scope to add
/// (as [`sign_in_scope`] decides), [`refresh_scope`] of `cfg.scope`.
/// Otherwise: with the grant unknown (`None`: a cache from before #1875, or
/// a response without `scope`), `cfg.scope` alone, so such a token goes
/// without the optional scopes until the next interactive sign-in and is
/// never refused over them. With a known grant, [`refresh_scope`] of
/// `cfg.scope`, followed by each optional scope the grant holds (each once,
/// as `optional_scope` spells it; a meta optional scope only when the grant
/// lists it), so a sign-in that went without the optional scopes never
/// refreshes asking for them and a granted optional scope is never
/// dropped. A grant that shares no non-meta scope with `cfg.scope` but
/// holds an optional one asks `cfg.scope` in full (`refresh_scope`'s
/// fallback, which counts `cfg.scope` alone) plus that optional scope.
pub fn refresh_scope_for(cfg: &AuthConfig, granted: Option<&[String]>) -> String {
    let extra = extra_optional_scopes(cfg);
    if extra.is_empty() {
        return refresh_scope(&cfg.scope, granted);
    }
    let Some(granted) = granted else {
        return cfg.scope.clone();
    };
    let required = refresh_scope(&cfg.scope, Some(granted));
    let held: Vec<String> = granted.iter().map(|s| normalise_scope(s)).collect();
    let mut asked: Vec<&str> = required.split_ascii_whitespace().collect();
    asked.extend(
        extra
            .iter()
            .filter(|(_, n)| held.contains(n))
            .map(|(scope, _)| *scope),
    );
    asked.join(" ")
}

/// The scopes of `requested` (space-separated), each with its
/// [`normalise_scope`]d form, the first spelling of each only, in order.
fn requested_scopes(requested: &str) -> impl Iterator<Item = (&str, String)> {
    let mut seen: Vec<String> = Vec::new();
    requested.split_ascii_whitespace().filter_map(move |scope| {
        let n = normalise_scope(scope);
        if seen.contains(&n) {
            return None;
        }
        seen.push(n.clone());
        Some((scope, n))
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
    use crate::graph::client::HTTP_ERROR_TEXT_MAX;
    use crate::testserver::{FakeServer, Route};

    fn cfg() -> AuthConfig {
        AuthConfig {
            authority: "x/organizations".into(),
            client_id: "cid".into(),
            scope: "Mail.ReadWrite People.Read offline_access".into(),
            optional_scope: String::new(),
            cloud: Cloud::Global,
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
        // With no optional scope, the interactive sign-in asks `cfg.scope`
        // in full (#2164, #2218).
        assert!(req.authorize_url.contains(&format!(
            "&{}&",
            pkce::form_urlencode(&[("scope", cfg().scope.as_str())])
        )));
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
        // The code redemption asks what `/authorize` asked: here
        // `cfg.scope` in full (#2164, #2218).
        assert_eq!(reqs[0].form_value("scope"), Some(cfg().scope));
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
        let t = refresh(&cfg(), &srv.base_url, "RT1", None).unwrap();
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
        let t = refresh(&cfg(), &srv.base_url, "RT1", None).unwrap();
        assert_eq!(t.access_token, "AT2");
        assert_eq!(t.refresh_token, "RT1");
    }

    const GRANTED: &str = "https://graph.microsoft.com/Mail.ReadWrite https://graph.microsoft.com/People.Read openid profile offline_access";

    fn granted() -> Vec<String> {
        GRANTED.split(' ').map(str::to_string).collect()
    }

    fn token_route(body: &str) -> Route {
        Route {
            method: "POST".into(),
            path_prefix: "/organizations/oauth2/v2.0/token".into(),
            status: 200,
            headers: vec![],
            body: body.into(),
        }
    }

    #[test]
    fn redeem_code_keeps_granted_scopes() {
        let srv = FakeServer::start(vec![token_route(&format!(
            r#"{{"access_token":"AT","refresh_token":"RT","expires_in":3600,"scope":"{GRANTED}"}}"#
        ))]);
        let req = begin_auth(&cfg(), "http://localhost:8400");
        let t = redeem_code(&cfg(), &srv.base_url, &req, "THECODE").unwrap();
        assert_eq!(t.scopes, Some(granted()));
    }

    #[test]
    fn refresh_keeps_granted_scopes() {
        let srv = FakeServer::start(vec![token_route(&format!(
            r#"{{"access_token":"AT2","expires_in":3600,"scope":"{GRANTED}"}}"#
        ))]);
        let t = refresh(&cfg(), &srv.base_url, "RT1", None).unwrap();
        assert_eq!(t.scopes, Some(granted()));
    }

    #[test]
    fn token_without_scope_has_unknown_scopes() {
        let srv = FakeServer::start(vec![token_route(
            r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#,
        )]);
        let req = begin_auth(&cfg(), "http://localhost:8400");
        let t = redeem_code(&cfg(), &srv.base_url, &req, "THECODE").unwrap();
        assert_eq!(t.scopes, None);
        // Present but blank is unknown too.
        let t = parse_token_response(r#"{"access_token":"AT","scope":"  "}"#, "", Cloud::Global)
            .unwrap();
        assert_eq!(t.scopes, None);
    }

    fn refresh_scope_of(requested: &str, granted: &[&str]) -> String {
        let granted: Vec<String> = granted.iter().map(|s| s.to_string()).collect();
        refresh_scope(requested, Some(&granted))
    }

    #[test]
    fn refresh_scope_asks_the_grant_still_requested() {
        // Mail.Send is not granted (just added); People.Read is granted but
        // no longer requested.
        assert_eq!(
            refresh_scope("Mail.ReadWrite Mail.Send offline_access", Some(&granted())),
            "Mail.ReadWrite offline_access"
        );
    }

    #[test]
    fn refresh_scope_without_grant_is_requested() {
        let requested = "Mail.ReadWrite Mail.Send offline_access";
        assert_eq!(refresh_scope(requested, None), requested);
    }

    #[test]
    fn refresh_scope_keeps_offline_access_only_from_the_request() {
        let granted = ["https://graph.microsoft.com/Mail.ReadWrite"];
        assert_eq!(
            refresh_scope_of("offline_access Mail.ReadWrite Mail.Send", &granted),
            "offline_access Mail.ReadWrite"
        );
        assert_eq!(
            refresh_scope_of(
                "Mail.ReadWrite Mail.Send",
                &[
                    "https://graph.microsoft.com/Mail.ReadWrite",
                    "offline_access"
                ]
            ),
            "Mail.ReadWrite"
        );
    }

    #[test]
    fn refresh_scope_of_meta_scopes_only_is_requested() {
        let requested = "https://graph.microsoft.com/.default offline_access";
        assert_eq!(refresh_scope(requested, Some(&granted())), requested);
    }

    #[test]
    fn refresh_scope_keeps_meta_and_default_scopes_once() {
        assert_eq!(
            refresh_scope_of(
                "openid MAIL.READWRITE profile email mail.readwrite Mail.Send \
                 https://graph.microsoft.com/Mail.ReadWrite OFFLINE_ACCESS offline_access",
                &["https://graph.microsoft.com/Mail.ReadWrite"]
            ),
            "openid MAIL.READWRITE profile email OFFLINE_ACCESS"
        );
        assert_eq!(
            refresh_scope_of(
                "https://graph.microsoft.com/Mail.ReadWrite api://app/.default",
                &["Mail.ReadWrite"]
            ),
            "https://graph.microsoft.com/Mail.ReadWrite api://app/.default"
        );
    }

    #[test]
    fn refresh_scope_with_no_shared_scope_is_requested() {
        let requested = "Mail.Send Calendars.ReadWrite offline_access";
        assert_eq!(refresh_scope(requested, Some(&granted())), requested);
        assert_eq!(refresh_scope(requested, Some(&[])), requested);
    }

    #[test]
    fn refresh_posts_the_granted_scope() {
        let cfg = AuthConfig {
            scope: "Mail.ReadWrite Mail.Send People.Read offline_access".into(),
            ..cfg()
        };
        let srv = FakeServer::start(vec![token_route(
            r#"{"access_token":"AT2","expires_in":3600}"#,
        )]);
        refresh(&cfg, &srv.base_url, "RT1", Some(&granted())).unwrap();
        refresh(&cfg, &srv.base_url, "RT1", None).unwrap();
        let reqs = srv.requests();
        assert_eq!(
            reqs[0].form_value("scope").as_deref(),
            Some("Mail.ReadWrite People.Read offline_access")
        );
        assert!(!reqs[0].body.contains("Mail.Send"), "{}", reqs[0].body);
        assert_eq!(reqs[1].form_value("scope"), Some(cfg.scope));
        assert_eq!(
            reqs[0].form_value("grant_type").as_deref(),
            Some("refresh_token")
        );
    }

    #[test]
    fn missing_scopes_ignores_case_prefix_and_meta_scopes() {
        assert!(missing_scopes("Mail.ReadWrite People.Read offline_access", &granted()).is_empty());
        assert!(missing_scopes("mail.readwrite PEOPLE.READ", &granted()).is_empty());
        assert!(
            missing_scopes(
                "https://GRAPH.microsoft.com/mail.readwrite",
                &["Mail.ReadWrite".to_string()]
            )
            .is_empty()
        );
        assert!(missing_scopes("openid profile email offline_access", &[]).is_empty());
        assert!(missing_scopes("https://graph.microsoft.com/.default .default", &[]).is_empty());
    }

    #[test]
    fn missing_scopes_reports_an_ungranted_scope() {
        assert_eq!(
            missing_scopes("Mail.ReadWrite Mail.Send offline_access", &granted()),
            vec!["Mail.Send".to_string()]
        );
        // Each once, as `requested` spells it, in its order.
        assert_eq!(
            missing_scopes(
                "Calendars.Read Mail.Send mail.send Calendars.Read",
                &granted()
            ),
            vec!["Calendars.Read".to_string(), "Mail.Send".to_string()]
        );
    }

    fn token_error(status: u16, body: &str) -> AuthError {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/organizations/oauth2/v2.0/token".into(),
            status,
            headers: vec![],
            body: body.into(),
        }]);
        match refresh(&cfg(), &srv.base_url, "RT1", None) {
            Ok(_) => panic!("the token endpoint answered {status}"),
            Err(e) => e,
        }
    }

    /// #2113 (#1959 plan-auth-http-display): a token endpoint's HTML error
    /// page shows as one capped line naming the status, never the markup;
    /// the body stays in the error for diagnostics.
    #[test]
    fn plan_auth_http_display_shows_one_line_for_an_html_body() {
        let page = "<!DOCTYPE html>\r\n<html><head><title>502 - Web server received an invalid response</title></head>\r\n<body><h1>Server Error</h1>\r\n</body></html>\r\n";
        let e = token_error(502, page);
        let shown = e.to_string();
        assert_eq!(shown, "HTTP error: HTTP 502 Bad Gateway");
        assert!(!shown.contains('<') && !shown.contains('\n'), "{shown}");
        assert!(shown.chars().count() <= "HTTP error: ".len() + HTTP_ERROR_TEXT_MAX);
        assert!(format!("{e:?}").contains("Server Error"), "{e:?}");
        // No reason phrase: the status alone.
        assert_eq!(
            classify_error(599, "<html>x</html>").to_string(),
            "HTTP error: HTTP 599"
        );
    }

    #[test]
    fn plan_auth_http_display_empty_body() {
        let e = token_error(503, "");
        assert_eq!(e.to_string(), "HTTP error: HTTP 503 Service Unavailable");
        assert!(matches!(e, AuthError::Status { status: 503, .. }), "{e:?}");
    }

    /// An OAuth JSON error body is still `Denied` with its description.
    #[test]
    fn oauth_json_error_is_still_denied() {
        let e = token_error(
            400,
            r#"{"error":"invalid_grant","error_description":"AADSTS70008: expired"}"#,
        );
        assert_eq!(e.to_string(), "authorization denied: AADSTS70008: expired");
    }

    /// #2167: each code that refuses the grant needs a sign-in, whatever
    /// its case and status; the description is still what `Display` shows.
    #[test]
    fn needs_sign_in_for_refusals() {
        for code in REFUSAL_CODES {
            let e = classify_error(400, &format!(r#"{{"error":"{code}"}}"#));
            assert!(e.needs_sign_in(), "{code}: {e:?}");
            assert_eq!(e.to_string(), format!("authorization denied: {code}"));
        }
        let e = token_error(
            401,
            r#"{"error":"Interaction_Required","error_description":"AADSTS50076: MFA"}"#,
        );
        assert!(e.needs_sign_in(), "{e:?}");
        assert_eq!(e.to_string(), "authorization denied: AADSTS50076: MFA");
        // A refusal served with a 5xx is still a refusal.
        assert!(classify_error(500, r#"{"error":"invalid_grant"}"#).needs_sign_in());
    }

    /// #2169: a refusal's code is trimmed and lowercased; only `Denied`
    /// has one.
    #[test]
    fn refusal_code_is_the_trimmed_lowercased_error() {
        let denied = |error: &str| AuthError::Denied {
            error: error.into(),
            description: Some("AADSTS70008: x".into()),
        };
        assert_eq!(
            denied(" Invalid_Grant\t").refusal_code().as_deref(),
            Some("invalid_grant")
        );
        assert_eq!(denied("").refusal_code().as_deref(), Some(""));
        for other in [
            AuthError::Http("invalid_grant".into()),
            AuthError::Status {
                status: 400,
                body: "invalid_grant".into(),
            },
            AuthError::Parse("invalid_grant".into()),
        ] {
            assert_eq!(other.refusal_code(), None, "{other:?}");
        }
    }

    /// #2169: the AADSTS number is the digits between `AADSTS` and the
    /// first colon at the description's start, and nothing else is.
    #[test]
    fn aadsts_is_the_number_before_the_colon() {
        let denied = |d: Option<&str>| AuthError::Denied {
            error: "invalid_grant".into(),
            description: d.map(str::to_string),
        };
        let cases: [(Option<&str>, Option<u32>); 10] = [
            (
                Some("AADSTS70008: The refresh token has expired"),
                Some(70008),
            ),
            (Some("  AADSTS50076: MFA required"), Some(50076)),
            (Some("AADSTS0:"), Some(0)),
            (Some("AADSTS50000"), None),
            (Some("xAADSTS1: no"), None),
            (Some("AADSTS: no number"), None),
            (Some("AADSTS99999999999: too large"), None),
            (Some("AADSTS+5: signed"), None),
            (Some("aadsts70008: lower case"), None),
            (None, None),
        ];
        for (d, want) in cases {
            assert_eq!(denied(d).aadsts(), want, "{d:?}");
        }
        let from_body = token_error(
            400,
            r#"{"error":"invalid_grant","error_description":"AADSTS70008: expired"}"#,
        );
        assert_eq!(from_body.aadsts(), Some(70008));
        for other in [
            AuthError::Http("AADSTS1: x".into()),
            AuthError::Status {
                status: 400,
                body: "AADSTS1: x".into(),
            },
            AuthError::Parse("AADSTS1: x".into()),
        ] {
            assert_eq!(other.aadsts(), None, "{other:?}");
        }
    }

    /// #2167: a dropped connection, a 5xx/429/408 with no OAuth body, a
    /// transient OAuth code and a 2xx that does not parse are all
    /// transient.
    #[test]
    fn transient_failures_do_not_need_sign_in() {
        let fail = |base: &str| match refresh(&cfg(), base, "RT1", None) {
            Ok(_) => panic!("{base} answered with a token"),
            Err(e) => e,
        };
        let refused_port = fail("http://127.0.0.1:9");
        assert!(
            matches!(refused_port, AuthError::Http(_)),
            "{refused_port:?}"
        );
        let closed = {
            let srv = FakeServer::start(vec![Route {
                method: "POST".into(),
                path_prefix: "/organizations/oauth2/v2.0/token".into(),
                status: 0,
                headers: vec![],
                body: String::new(),
            }]);
            fail(&srv.base_url)
        };
        assert!(matches!(closed, AuthError::Http(_)), "{closed:?}");
        let unparsed = {
            let srv = FakeServer::start(vec![token_route("<html>ok</html>")]);
            fail(&srv.base_url)
        };
        assert!(matches!(unparsed, AuthError::Parse(_)), "{unparsed:?}");
        let errors = [
            refused_port,
            closed,
            unparsed,
            token_error(503, "<html><body>Service Unavailable</body></html>"),
            token_error(429, ""),
            token_error(408, ""),
            token_error(400, r#"{"error":"temporarily_unavailable"}"#),
            token_error(
                500,
                r#"{"error":"server_error","error_description":"AADSTS50000"}"#,
            ),
            token_error(400, r#"{"error":"invalid_request"}"#),
            token_error(400, r#"{"error_description":"no code"}"#),
        ];
        for e in errors {
            assert!(!e.needs_sign_in(), "{e:?}");
        }
    }

    /// [`cfg`] with the people scopes as optional (#2218).
    fn optional_cfg() -> AuthConfig {
        AuthConfig {
            optional_scope: "User.ReadBasic.All Presence.Read.All".into(),
            ..cfg()
        }
    }

    fn scope_param(url: &str) -> Option<String> {
        let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
        crate::sync::engine::parse_query(query)
            .into_iter()
            .find(|(k, _)| k == "scope")
            .map(|(_, v)| v)
    }

    #[test]
    fn sign_in_scope_appends_optional_once() {
        // Nothing to add: `cfg.scope` byte for byte.
        for optional in [
            "",
            "   ",
            "mail.readwrite https://graph.microsoft.com/People.Read OFFLINE_ACCESS",
        ] {
            let c = AuthConfig {
                scope: "Mail.ReadWrite  People.Read offline_access".into(),
                optional_scope: optional.into(),
                ..cfg()
            };
            assert_eq!(sign_in_scope(&c), c.scope, "{optional:?}");
        }
        let c = AuthConfig {
            optional_scope: "User.ReadBasic.All mail.readwrite Presence.Read.All \
                             user.readbasic.all"
                .into(),
            ..cfg()
        };
        assert_eq!(
            sign_in_scope(&c),
            "Mail.ReadWrite People.Read offline_access User.ReadBasic.All Presence.Read.All"
        );
    }

    #[test]
    fn begin_auth_asks_both_lists_and_redeem_asks_what_was_authorized() {
        let c = optional_cfg();
        let full = begin_auth(&c, "http://localhost:8400");
        assert_eq!(full.scope(), sign_in_scope(&c));
        assert_eq!(scope_param(&full.authorize_url), Some(sign_in_scope(&c)));
        let small = begin_auth_with_scope(&c, "http://localhost:8400", &c.scope);
        assert_eq!(small.scope(), c.scope);
        assert_eq!(scope_param(&small.authorize_url), Some(c.scope.clone()));

        let srv = FakeServer::start(vec![token_route(
            r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#,
        )]);
        redeem_code(&c, &srv.base_url, &full, "CODE1").unwrap();
        redeem_code(&c, &srv.base_url, &small, "CODE2").unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].form_value("scope"), Some(sign_in_scope(&c)));
        assert_eq!(reqs[1].form_value("scope"), Some(c.scope.clone()));
    }

    fn denied(error: &str, description: Option<&str>) -> AuthError {
        AuthError::Denied {
            error: error.into(),
            description: description.map(str::to_string),
        }
    }

    #[test]
    fn consent_refusal_rule() {
        let yes = [
            ("consent_required", None),
            (" Consent_Required ", Some("AADSTS50076: MFA")),
            (
                "invalid_grant",
                Some("AADSTS65001: The user or administrator has not consented"),
            ),
            (
                "interaction_required",
                Some("AADSTS65001: The user or administrator has not consented"),
            ),
            (
                "access_denied",
                Some("AADSTS65004: User declined to consent"),
            ),
            (
                "access_denied",
                Some("  AADSTS65004: User declined to consent"),
            ),
        ];
        for (error, description) in yes {
            assert!(
                is_consent_refusal(error, description),
                "{error} {description:?}"
            );
            assert!(denied(error, description).consent_refused(), "{error}");
        }
        let no = [
            ("access_denied", None),
            ("access_denied", Some("the user cancelled")),
            ("access_denied", Some("AADSTS50058: no session")),
            ("invalid_client", Some("AADSTS7000218: secret")),
            ("interaction_required", Some("AADSTS50076: MFA")),
            ("invalid_grant", Some("AADSTS650010: not a number we know")),
            ("invalid_grant", Some("see AADSTS65001: later in the text")),
        ];
        for (error, description) in no {
            assert!(
                !is_consent_refusal(error, description),
                "{error} {description:?}"
            );
            assert!(!denied(error, description).consent_refused(), "{error}");
        }
        for e in [
            AuthError::Http("AADSTS65001: x".into()),
            AuthError::Status {
                status: 400,
                body: "consent_required".into(),
            },
            AuthError::Parse("consent_required".into()),
        ] {
            assert!(!e.consent_refused(), "{e:?}");
        }
    }

    fn refresh_scope_for_of(c: &AuthConfig, granted: Option<&[&str]>) -> String {
        let granted: Option<Vec<String>> =
            granted.map(|g| g.iter().map(|s| s.to_string()).collect());
        refresh_scope_for(c, granted.as_deref())
    }

    #[test]
    fn refresh_scope_for_cases() {
        // No optional scope: #2164's `refresh_scope`, whatever the grant.
        let plain = AuthConfig {
            scope: "Mail.ReadWrite Mail.Send offline_access".into(),
            optional_scope: " mail.send ".into(),
            ..cfg()
        };
        let grants: [Option<&[&str]>; 4] = [
            None,
            Some(&[]),
            Some(&["https://graph.microsoft.com/Mail.ReadWrite"]),
            Some(&["Calendars.Read"]),
        ];
        for g in grants {
            let owned: Option<Vec<String>> = g.map(|g| g.iter().map(|s| s.to_string()).collect());
            assert_eq!(
                refresh_scope_for_of(&plain, g),
                refresh_scope(&plain.scope, owned.as_deref()),
                "{g:?}"
            );
        }

        let c = optional_cfg();
        // Unknown grant: `scope` alone.
        assert_eq!(refresh_scope_for_of(&c, None), c.scope);
        // A sign-in that went without the optional scopes.
        assert_eq!(
            refresh_scope_for_of(
                &c,
                Some(&[
                    "https://graph.microsoft.com/Mail.ReadWrite",
                    "https://graph.microsoft.com/People.Read",
                ])
            ),
            "Mail.ReadWrite People.Read offline_access"
        );
        // A granted optional scope is kept, as `optional_scope` spells it.
        assert_eq!(
            refresh_scope_for_of(
                &c,
                Some(&[
                    "https://graph.microsoft.com/Mail.ReadWrite",
                    "https://graph.microsoft.com/presence.read.all",
                ])
            ),
            "Mail.ReadWrite offline_access Presence.Read.All"
        );
        // Only an optional scope granted: `cfg.scope` in full (#2164's
        // fallback) plus that scope.
        assert_eq!(
            refresh_scope_for_of(
                &c,
                Some(&["https://graph.microsoft.com/User.ReadBasic.All"])
            ),
            "Mail.ReadWrite People.Read offline_access User.ReadBasic.All"
        );
        // A meta optional scope only when the grant lists it.
        let meta = AuthConfig {
            optional_scope: "openid Presence.Read.All".into(),
            ..cfg()
        };
        assert_eq!(
            refresh_scope_for_of(&meta, Some(&["Mail.ReadWrite", "Presence.Read.All"])),
            "Mail.ReadWrite offline_access Presence.Read.All"
        );
        assert_eq!(
            refresh_scope_for_of(&meta, Some(&["Mail.ReadWrite", "openid"])),
            "Mail.ReadWrite offline_access openid"
        );

        let srv = FakeServer::start(vec![token_route(
            r#"{"access_token":"AT2","expires_in":3600}"#,
        )]);
        let g = vec!["https://graph.microsoft.com/Mail.ReadWrite".to_string()];
        refresh(&c, &srv.base_url, "RT1", Some(&g)).unwrap();
        refresh(&c, &srv.base_url, "RT1", None).unwrap();
        let reqs = srv.requests();
        assert_eq!(
            reqs[0].form_value("scope").as_deref(),
            Some("Mail.ReadWrite offline_access")
        );
        assert_eq!(reqs[1].form_value("scope"), Some(c.scope.clone()));
    }

    #[test]
    fn tenant_of_trims_trailing_slash() {
        assert_eq!(
            tenant_of("https://login.microsoftonline.com/organizations/"),
            "organizations"
        );
    }

    // --- National clouds (#2225) -----------------------------------------

    /// AC 1: the issue's login host and Graph root per cloud, and the
    /// name a saved token records.
    #[test]
    fn each_cloud_has_its_hosts_and_name() {
        let want = [
            (
                Cloud::Global,
                "https://login.microsoftonline.com",
                "https://graph.microsoft.com",
                "global",
            ),
            (
                Cloud::UsGov,
                "https://login.microsoftonline.us",
                "https://graph.microsoft.us",
                "usgov",
            ),
            (
                Cloud::UsGovDod,
                "https://login.microsoftonline.us",
                "https://dod-graph.microsoft.us",
                "usgovdod",
            ),
            (
                Cloud::China,
                "https://login.chinacloudapi.cn",
                "https://microsoftgraph.chinacloudapi.cn",
                "china",
            ),
        ];
        assert_eq!(Cloud::ALL.to_vec(), want.map(|w| w.0).to_vec());
        for (cloud, login, graph, name) in want {
            assert_eq!(cloud.login_host(), login);
            assert_eq!(cloud.graph_root(), graph);
            assert_eq!(cloud.name(), name);
            assert_eq!(Cloud::from_name(name), Some(cloud));
        }
        for other in ["", "Global", "USGOV", "mars", " china"] {
            assert_eq!(Cloud::from_name(other), None, "{other:?}");
        }
    }

    /// AC 1: the default config is today's, in the global cloud.
    #[test]
    fn auth_config_default_is_global() {
        let c = AuthConfig::default();
        assert_eq!(c.cloud, Cloud::Global);
        assert_eq!(Cloud::default(), Cloud::Global);
        assert_eq!(
            c.authority,
            "https://login.microsoftonline.com/organizations"
        );
        assert_eq!(c.client_id, "14d82eec-204b-4c2f-b7e8-296a70dab67e");
        assert_eq!(c.scope, "Mail.ReadWrite People.Read offline_access");
        assert_eq!(c.optional_scope, "");
    }

    /// AC 2: `/authorize` is on the cloud's login host, whatever host the
    /// authority names; the tenant is its last segment.
    #[test]
    fn authorize_url_uses_each_clouds_login_host() {
        for cloud in Cloud::ALL {
            let c = AuthConfig {
                authority: "https://login.microsoftonline.com/contoso".into(),
                cloud,
                ..cfg()
            };
            for req in [
                begin_auth(&c, "http://localhost:8400"),
                begin_auth_with_scope(&c, "http://localhost:8400", "Mail.ReadWrite"),
            ] {
                let want = format!("{}/contoso/oauth2/v2.0/authorize?", cloud.login_host());
                assert!(
                    req.authorize_url.starts_with(&want),
                    "{cloud:?}: {}",
                    req.authorize_url
                );
            }
        }
    }

    /// AC 6: the token parsed from a token response is the configured
    /// cloud's, from a redemption and a refresh alike.
    #[test]
    fn refresh_records_the_cloud() {
        let srv = FakeServer::start(vec![token_route(
            r#"{"access_token":"AT2","refresh_token":"RT2","expires_in":3600}"#,
        )]);
        for cloud in Cloud::ALL {
            let c = AuthConfig { cloud, ..cfg() };
            let t = refresh(&c, &srv.base_url, "RT1", None).unwrap();
            assert_eq!(t.cloud, cloud);
            let req = begin_auth(&c, "http://localhost:8400");
            let t = redeem_code(&c, &srv.base_url, &req, "CODE").unwrap();
            assert_eq!(t.cloud, cloud);
        }
    }

    /// AC 4: every cloud's Graph root is stripped, whatever its case, so a
    /// national cloud's grant is not missing a short-named scope.
    #[test]
    fn normalise_scope_strips_every_clouds_graph_root() {
        for cloud in Cloud::ALL {
            let root = cloud.graph_root();
            assert_eq!(
                normalise_scope(&format!("{root}/Mail.ReadWrite")),
                "mail.readwrite"
            );
            assert_eq!(
                normalise_scope(&format!("{}/Mail.ReadWrite", root.to_ascii_uppercase())),
                "mail.readwrite"
            );
            assert_eq!(normalise_scope(&format!("{root}/.default")), ".default");
        }
        // Not a Graph root: kept, lowercased. A root without its `/` is no
        // prefix, nor is a host that only starts like one.
        assert_eq!(normalise_scope("api://x/Y.Read"), "api://x/y.read");
        assert_eq!(
            normalise_scope("https://graph.microsoft.usx/Mail.Read"),
            "https://graph.microsoft.usx/mail.read"
        );
        assert_eq!(
            normalise_scope("https://graph.microsoft.us"),
            "https://graph.microsoft.us"
        );
        // Non-ASCII text is never cut inside a character.
        assert_eq!(
            normalise_scope("https://graph.microsoft.cömmon/x"),
            "https://graph.microsoft.cömmon/x"
        );
        for cloud in Cloud::ALL {
            let granted = vec![
                format!("{}/Mail.ReadWrite", cloud.graph_root()),
                format!("{}/People.Read", cloud.graph_root()),
                "offline_access".to_string(),
            ];
            assert!(
                missing_scopes("Mail.ReadWrite People.Read offline_access", &granted).is_empty(),
                "{cloud:?}"
            );
            assert_eq!(
                refresh_scope("Mail.ReadWrite Mail.Send offline_access", Some(&granted)),
                "Mail.ReadWrite offline_access",
                "{cloud:?}"
            );
        }
    }

    /// AC 5: a scope spelled with another cloud's Graph root is sent on
    /// the configured cloud's; everything else byte for byte.
    #[test]
    fn wire_scope_moves_graph_roots_to_the_cloud() {
        assert_eq!(
            wire_scope(
                Cloud::UsGov,
                "https://graph.microsoft.com/.default Mail.ReadWrite offline_access"
            ),
            "https://graph.microsoft.us/.default Mail.ReadWrite offline_access"
        );
        assert_eq!(
            wire_scope(
                Cloud::China,
                "HTTPS://GRAPH.MICROSOFT.COM/Mail.Read  https://dod-graph.microsoft.us/User.Read"
            ),
            "https://microsoftgraph.chinacloudapi.cn/Mail.Read \
             https://microsoftgraph.chinacloudapi.cn/User.Read"
        );
        // Nothing to rewrite: byte for byte, whitespace and all.
        for (cloud, scope) in [
            (Cloud::Global, "Mail.ReadWrite People.Read offline_access"),
            (
                Cloud::Global,
                "  https://graph.microsoft.com/Mail.Read   openid ",
            ),
            (Cloud::UsGovDod, "Mail.ReadWrite  api://x/y offline_access"),
            (
                Cloud::UsGovDod,
                "https://dod-graph.microsoft.us/Mail.Read  openid",
            ),
        ] {
            assert_eq!(wire_scope(cloud, scope), scope, "{cloud:?}");
        }
        // `/authorize`, the redemption and the refresh ask the wire scope.
        let c = AuthConfig {
            scope: "https://graph.microsoft.com/Mail.ReadWrite offline_access".into(),
            cloud: Cloud::UsGovDod,
            ..cfg()
        };
        let wire = "https://dod-graph.microsoft.us/Mail.ReadWrite offline_access";
        let req = begin_auth(&c, "http://localhost:8400");
        assert_eq!(req.scope(), wire);
        assert_eq!(scope_param(&req.authorize_url).as_deref(), Some(wire));
        let srv = FakeServer::start(vec![token_route(
            r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#,
        )]);
        redeem_code(&c, &srv.base_url, &req, "CODE").unwrap();
        refresh(&c, &srv.base_url, "RT1", None).unwrap();
        let sent: Vec<Option<String>> = srv
            .requests()
            .iter()
            .map(|r| r.form_value("scope"))
            .collect();
        assert_eq!(sent, vec![Some(wire.to_string()), Some(wire.to_string())]);
    }
}
