//! The JSON a host and an integration plugin exchange: what `dxi_init`
//! takes and answers, and each method's request and answer. Each type
//! encodes and decodes, so a host and a plugin written in Rust share it.
//!
//! Decoding is lenient by field: a field that is absent, null, of another
//! type or empty is `None`, and the host keeps its own value for it.

use projcore::json::Json;

/// Which application a call is for. A process that runs several (the suite
/// with its Mail and Chat tabs) names the one each call is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum App {
    Suite,
    Mail,
    Chat,
}

impl App {
    pub fn as_str(self) -> &'static str {
        match self {
            App::Suite => "suite",
            App::Mail => "mail",
            App::Chat => "chat",
        }
    }

    pub fn parse(s: &str) -> Option<App> {
        match s {
            "suite" => Some(App::Suite),
            "mail" => Some(App::Mail),
            "chat" => Some(App::Chat),
            _ => None,
        }
    }
}

/// `dxi_init`'s input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    pub abi: u32,
    pub app: App,
    pub app_version: String,
    /// `std::env::consts::OS`: `windows`, `linux`, `macos`.
    pub os: String,
}

impl HostInfo {
    pub fn to_json(&self) -> String {
        Json::obj(vec![
            ("abi", Json::Num(f64::from(self.abi))),
            ("app", Json::str(self.app.as_str())),
            ("app_version", Json::str(&self.app_version)),
            ("os", Json::str(&self.os)),
        ])
        .to_string()
    }

    pub fn from_json(text: &str) -> Result<HostInfo, String> {
        let json = Json::parse(text)?;
        let abi = json
            .int("abi")
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("no abi")?;
        let app = json.get_str("app").and_then(App::parse).ok_or("no app")?;
        Ok(HostInfo {
            abi,
            app,
            app_version: json.text("app_version"),
            os: json.text("os"),
        })
    }
}

/// `dxi_init`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InitAnswer {
    pub plugin: String,
    pub version: String,
    /// The methods the host may call; it calls no other.
    pub methods: Vec<String>,
}

impl InitAnswer {
    pub fn to_json(&self) -> String {
        Json::obj(vec![
            ("plugin", Json::str(&self.plugin)),
            ("version", Json::str(&self.version)),
            ("methods", Json::strs(&self.methods)),
        ])
        .to_string()
    }

    pub fn from_json(text: &str) -> Result<InitAnswer, String> {
        let json = Json::parse(text)?;
        if json.as_object().is_none() {
            return Err("the answer is not an object".into());
        }
        Ok(InitAnswer {
            plugin: json.text("plugin"),
            version: json.text("version"),
            methods: json.texts("methods"),
        })
    }
}

/// The request of `branding` and `sign_in`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppRequest {
    pub app: App,
}

impl AppRequest {
    pub fn to_json(&self) -> String {
        Json::obj(vec![("app", Json::str(self.app.as_str()))]).to_string()
    }

    pub fn from_json(text: &str) -> Result<AppRequest, String> {
        let json = Json::parse(text)?;
        let app = json.get_str("app").and_then(App::parse).ok_or("no app")?;
        Ok(AppRequest { app })
    }
}

/// `branding`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Branding {
    /// The product name in window titles and About.
    pub product: Option<String>,
    pub short: Option<String>,
    pub about_line: Option<String>,
    /// An `.ico` file's bytes.
    pub icon_ico: Option<Vec<u8>>,
    /// A PNG's bytes.
    pub icon_png: Option<Vec<u8>>,
}

impl Branding {
    pub fn to_json(&self) -> String {
        let mut pairs = Vec::new();
        push_text(&mut pairs, "product", &self.product);
        push_text(&mut pairs, "short", &self.short);
        push_text(&mut pairs, "about_line", &self.about_line);
        push_bytes(&mut pairs, "icon_ico_b64", &self.icon_ico);
        push_bytes(&mut pairs, "icon_png_b64", &self.icon_png);
        Json::Obj(pairs).to_string()
    }

    pub fn from_json(text: &str) -> Result<Branding, String> {
        let json = Json::parse(text)?;
        Ok(Branding {
            product: text_of(&json, "product"),
            short: text_of(&json, "short"),
            about_line: text_of(&json, "about_line"),
            icon_ico: bytes_of(&json, "icon_ico_b64"),
            icon_png: bytes_of(&json, "icon_png_b64"),
        })
    }
}

/// `sign_in`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignIn {
    pub client_id: Option<String>,
    /// A tenant id or domain.
    pub tenant: Option<String>,
    pub login_hint: Option<String>,
    pub cloud: Option<String>,
}

impl SignIn {
    pub fn to_json(&self) -> String {
        let mut pairs = Vec::new();
        push_text(&mut pairs, "client_id", &self.client_id);
        push_text(&mut pairs, "tenant", &self.tenant);
        push_text(&mut pairs, "login_hint", &self.login_hint);
        push_text(&mut pairs, "cloud", &self.cloud);
        Json::Obj(pairs).to_string()
    }

    pub fn from_json(text: &str) -> Result<SignIn, String> {
        let json = Json::parse(text)?;
        Ok(SignIn {
            client_id: text_of(&json, "client_id"),
            tenant: text_of(&json, "tenant"),
            login_hint: text_of(&json, "login_hint"),
            cloud: text_of(&json, "cloud"),
        })
    }
}

/// `token`'s request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRequest {
    pub app: App,
    /// Space-separated, as OAuth writes them.
    pub scopes: String,
    pub client_id: Option<String>,
    pub tenant: Option<String>,
}

impl TokenRequest {
    pub fn to_json(&self) -> String {
        let mut pairs = vec![
            ("app".to_string(), Json::str(self.app.as_str())),
            ("scopes".to_string(), Json::str(&self.scopes)),
        ];
        push_text(&mut pairs, "client_id", &self.client_id);
        push_text(&mut pairs, "tenant", &self.tenant);
        Json::Obj(pairs).to_string()
    }

    pub fn from_json(text: &str) -> Result<TokenRequest, String> {
        let json = Json::parse(text)?;
        let app = json.get_str("app").and_then(App::parse).ok_or("no app")?;
        Ok(TokenRequest {
            app,
            scopes: json.text("scopes"),
            client_id: text_of(&json, "client_id"),
            tenant: text_of(&json, "tenant"),
        })
    }
}

/// `token`'s answer.
#[derive(Clone, PartialEq, Eq)]
pub enum TokenAnswer {
    Token {
        access_token: String,
        expires_at_unix: i64,
        account: Option<String>,
        /// The scopes the token holds, space-separated, when the plugin
        /// knows them.
        scopes: Option<String>,
    },
    /// A sign-in that the person has to take part in.
    InteractionRequired {
        why: String,
    },
    Unavailable {
        why: String,
    },
}

/// The access token is left out, so a log of a `{:?}` never holds it.
impl std::fmt::Debug for TokenAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenAnswer::Token {
                expires_at_unix, ..
            } => f
                .debug_struct("Token")
                .field("expires_at_unix", expires_at_unix)
                .finish_non_exhaustive(),
            TokenAnswer::InteractionRequired { why } => f
                .debug_struct("InteractionRequired")
                .field("why", why)
                .finish(),
            TokenAnswer::Unavailable { why } => {
                f.debug_struct("Unavailable").field("why", why).finish()
            }
        }
    }
}

impl TokenAnswer {
    pub fn to_json(&self) -> String {
        match self {
            TokenAnswer::Token {
                access_token,
                expires_at_unix,
                account,
                scopes,
            } => {
                let mut pairs = vec![
                    ("status".to_string(), Json::str("ok")),
                    ("access_token".to_string(), Json::str(access_token)),
                    (
                        "expires_at_unix".to_string(),
                        Json::Num(*expires_at_unix as f64),
                    ),
                ];
                push_text(&mut pairs, "account", account);
                push_text(&mut pairs, "scopes", scopes);
                Json::Obj(pairs).to_string()
            }
            TokenAnswer::InteractionRequired { why } => Json::obj(vec![
                ("status", Json::str("interaction_required")),
                ("why", Json::str(why)),
            ])
            .to_string(),
            TokenAnswer::Unavailable { why } => Json::obj(vec![
                ("status", Json::str("unavailable")),
                ("why", Json::str(why)),
            ])
            .to_string(),
        }
    }

    /// A status the host does not know, or an `ok` without a token or an
    /// expiry, is `Unavailable`.
    pub fn from_json(text: &str) -> Result<TokenAnswer, String> {
        let json = Json::parse(text)?;
        let why = json.text("why");
        Ok(match json.get_str("status") {
            Some("ok") => match (text_of(&json, "access_token"), json.int("expires_at_unix")) {
                (Some(access_token), Some(expires_at_unix)) => TokenAnswer::Token {
                    access_token,
                    expires_at_unix,
                    account: text_of(&json, "account"),
                    scopes: text_of(&json, "scopes"),
                },
                _ => TokenAnswer::Unavailable {
                    why: "the answer has no access_token or expires_at_unix".into(),
                },
            },
            Some("interaction_required") => TokenAnswer::InteractionRequired { why },
            Some("unavailable") => TokenAnswer::Unavailable { why },
            other => TokenAnswer::Unavailable {
                why: format!("unknown status {}", other.unwrap_or("(none)")),
            },
        })
    }
}

/// An id and a display name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Named {
    pub id: Option<String>,
    pub name: Option<String>,
}

/// `context`'s answer: where the person works.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Context {
    pub organization: Option<Named>,
    pub desktop: Option<Named>,
}

impl Context {
    pub fn to_json(&self) -> String {
        let mut pairs = Vec::new();
        for (key, named) in [
            ("organization", &self.organization),
            ("desktop", &self.desktop),
        ] {
            if let Some(named) = named {
                let mut inner = Vec::new();
                push_text(&mut inner, "id", &named.id);
                push_text(&mut inner, "name", &named.name);
                pairs.push((key.to_string(), Json::Obj(inner)));
            }
        }
        Json::Obj(pairs).to_string()
    }

    pub fn from_json(text: &str) -> Result<Context, String> {
        let json = Json::parse(text)?;
        let named = |key: &str| {
            let value = json.get(key)?;
            let named = Named {
                id: text_of(value, "id"),
                name: text_of(value, "name"),
            };
            (named.id.is_some() || named.name.is_some()).then_some(named)
        };
        Ok(Context {
            organization: named("organization"),
            desktop: named("desktop"),
        })
    }
}

fn text_of(json: &Json, key: &str) -> Option<String> {
    json.get_str(key)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn bytes_of(json: &Json, key: &str) -> Option<Vec<u8>> {
    json.get_str(key)
        .and_then(base64_decode)
        .filter(|b| !b.is_empty())
}

fn push_text(pairs: &mut Vec<(String, Json)>, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        pairs.push((key.to_string(), Json::str(value)));
    }
}

fn push_bytes(pairs: &mut Vec<(String, Json)>, key: &str, value: &Option<Vec<u8>>) {
    if let Some(value) = value {
        pairs.push((key.to_string(), Json::Str(base64_encode(value))));
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (RFC 4648 section 4), with padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// [`base64_encode`]'s inverse; `None` on a character outside the alphabet,
/// a length that is not a multiple of 4, or padding anywhere but the end.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let value = |c: u8| B64.iter().position(|&b| b == c).map(|p| p as u32);
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let chunks = bytes.len() / 4;
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && index + 1 != chunks) {
            return None;
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | value(c)?;
        }
        n <<= 6 * pad as u32;
        let take = 3 - pad;
        out.extend_from_slice(&n.to_be_bytes()[1..1 + take]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, coded) in vectors {
            assert_eq!(base64_encode(plain.as_bytes()), coded);
            assert_eq!(base64_decode(coded).as_deref(), Some(plain.as_bytes()));
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&all)), Some(all));
    }

    #[test]
    fn base64_refuses_malformed_text() {
        for bad in ["Zg=", "Zg==Zg==", "Z===", "Zm9*", "Zm9v\n"] {
            assert_eq!(base64_decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn host_info_round_trips() {
        let info = HostInfo {
            abi: 1,
            app: App::Mail,
            app_version: "0.1.0".into(),
            os: "windows".into(),
        };
        assert_eq!(
            info.to_json(),
            r#"{"abi":1,"app":"mail","app_version":"0.1.0","os":"windows"}"#
        );
        assert_eq!(HostInfo::from_json(&info.to_json()), Ok(info));
        assert!(HostInfo::from_json(r#"{"abi":1,"app":"other"}"#).is_err());
    }

    #[test]
    fn a_missing_or_empty_field_keeps_the_hosts_own() {
        let branding = Branding::from_json(
            r#"{"product":"","short":7,"about_line":"Line","icon_ico_b64":"not base64!"}"#,
        )
        .unwrap();
        assert_eq!(
            branding,
            Branding {
                about_line: Some("Line".into()),
                ..Branding::default()
            }
        );
        assert_eq!(SignIn::from_json("{}"), Ok(SignIn::default()));
        assert_eq!(
            Context::from_json(r#"{"organization":{},"desktop":{"name":"D"}}"#),
            Ok(Context {
                organization: None,
                desktop: Some(Named {
                    id: None,
                    name: Some("D".into())
                }),
            })
        );
    }

    #[test]
    fn answers_round_trip() {
        let branding = Branding {
            product: Some("Product".into()),
            short: Some("P".into()),
            about_line: None,
            icon_ico: Some(vec![0, 0, 1, 0]),
            icon_png: Some(vec![0x89, b'P', b'N', b'G']),
        };
        assert_eq!(Branding::from_json(&branding.to_json()), Ok(branding));
        let sign_in = SignIn {
            client_id: Some("c".into()),
            tenant: Some("t".into()),
            login_hint: Some("person@example.com".into()),
            cloud: None,
        };
        assert_eq!(SignIn::from_json(&sign_in.to_json()), Ok(sign_in));
        let context = Context {
            organization: Some(Named {
                id: Some("o".into()),
                name: Some("Org".into()),
            }),
            desktop: None,
        };
        assert_eq!(Context::from_json(&context.to_json()), Ok(context));
        let request = TokenRequest {
            app: App::Chat,
            scopes: "User.Read offline_access".into(),
            client_id: None,
            tenant: Some("t".into()),
        };
        assert_eq!(TokenRequest::from_json(&request.to_json()), Ok(request));
    }

    #[test]
    fn token_answers_round_trip_and_unknown_ones_are_unavailable() {
        for answer in [
            TokenAnswer::Token {
                access_token: "a".into(),
                expires_at_unix: 1_800_000_000,
                account: Some("person@example.com".into()),
                scopes: None,
            },
            TokenAnswer::InteractionRequired { why: "w".into() },
            TokenAnswer::Unavailable { why: "u".into() },
        ] {
            assert_eq!(TokenAnswer::from_json(&answer.to_json()), Ok(answer));
        }
        for text in [
            r#"{"status":"ok","expires_at_unix":1}"#,
            r#"{"status":"ok","access_token":"a"}"#,
            r#"{"status":"later"}"#,
            "{}",
        ] {
            assert!(
                matches!(
                    TokenAnswer::from_json(text),
                    Ok(TokenAnswer::Unavailable { .. })
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn a_token_answers_debug_text_holds_no_token() {
        let answer = TokenAnswer::Token {
            access_token: "secret-value".into(),
            expires_at_unix: 1,
            account: Some("person@example.com".into()),
            scopes: None,
        };
        let text = format!("{answer:?}");
        assert!(
            !text.contains("secret-value") && !text.contains("person@"),
            "{text}"
        );
    }
}
