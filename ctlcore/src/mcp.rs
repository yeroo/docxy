//! Generic [Model Context Protocol](https://modelcontextprotocol.io) stdio
//! scaffolding: the JSON-RPC 2.0 envelope, the `initialize` / `tools/list` /
//! `tools/call` / `ping` routing, and tool-definition builders. An editor
//! supplies its identity, its tool definitions, and a handler that executes a
//! tool; everything protocol-shaped lives here so docxy/xlsxy/yppxy don't
//! triplicate it.
//!
//! Transport is newline-delimited JSON-RPC over stdio: one message per line, no
//! embedded newlines, per the MCP stdio transport.

use crate::json::Json;
use std::io::{BufRead, Write};

pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// An MCP stdio server: identity + tools + the tool executor. The executor
/// returns the result text (typically JSON) or an error message — a tool-level
/// failure becomes a normal result with `isError`, not a protocol error.
pub struct McpServer<'a> {
    pub name: &'a str,
    pub version: &'a str,
    /// The `tools/list` payload: a [`Json::Arr`] of [`tool`] definitions.
    pub tools: Json,
    pub handler: &'a dyn Fn(&str, &Json) -> Result<String, String>,
}

impl McpServer<'_> {
    /// Serve until stdin closes.
    pub fn run(&self) -> std::io::Result<()> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for line in stdin.lock().lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(msg) = Json::parse(trimmed) else {
                continue; // ignore anything that isn't a JSON message
            };
            if let Some(resp) = self.handle(&msg) {
                let mut s = resp.to_string();
                s.push('\n');
                out.write_all(s.as_bytes())?;
                out.flush()?;
            }
        }
        Ok(())
    }

    /// Route one JSON-RPC message. Returns `Some(response)` for requests,
    /// `None` for notifications (and messages without a method).
    pub fn handle(&self, msg: &Json) -> Option<Json> {
        let method = msg.get_str("method")?;
        let id = msg.get("id").cloned().unwrap_or(Json::Null);
        match method {
            "initialize" => Some(ok(id, self.initialize_result())),
            "ping" => Some(ok(id, Json::obj(vec![]))),
            "tools/list" => Some(ok(id, Json::obj(vec![("tools", self.tools.clone())]))),
            "tools/call" => Some(self.handle_tool_call(id, msg.get("params"))),
            // Notifications (initialized, cancelled, …) expect no response.
            m if m.starts_with("notifications/") => None,
            other => Some(err(id, -32601, format!("method not found: {other}"))),
        }
    }

    fn handle_tool_call(&self, id: Json, params: Option<&Json>) -> Json {
        let Some(params) = params else {
            return err(id, -32602, "missing params".into());
        };
        let Some(name) = params.get_str("name") else {
            return err(id, -32602, "missing tool name".into());
        };
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Json::obj(vec![]));
        match (self.handler)(name, &args) {
            Ok(text) => ok(id, tool_result(text, false)),
            Err(e) => ok(id, tool_result(e, true)),
        }
    }

    fn initialize_result(&self) -> Json {
        Json::obj(vec![
            ("protocolVersion", Json::Str(PROTOCOL_VERSION.into())),
            (
                "capabilities",
                Json::obj(vec![("tools", Json::obj(vec![]))]),
            ),
            (
                "serverInfo",
                Json::obj(vec![
                    ("name", Json::Str(self.name.into())),
                    ("version", Json::Str(self.version.into())),
                ]),
            ),
        ])
    }
}

fn ok(id: Json, result: Json) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".into())),
        ("id", id),
        ("result", result),
    ])
}

fn err(id: Json, code: i64, message: String) -> Json {
    Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".into())),
        ("id", id),
        (
            "error",
            Json::obj(vec![
                ("code", Json::Num(code as f64)),
                ("message", Json::Str(message)),
            ]),
        ),
    ])
}

fn tool_result(text: String, is_error: bool) -> Json {
    Json::obj(vec![
        (
            "content",
            Json::Arr(vec![Json::obj(vec![
                ("type", Json::Str("text".into())),
                ("text", Json::Str(text)),
            ])]),
        ),
        ("isError", Json::Bool(is_error)),
    ])
}

/// A JSON-schema property: `{"type": ty, "description": desc}`.
pub fn prop(ty: &str, desc: &str) -> Json {
    Json::obj(vec![
        ("type", Json::Str(ty.into())),
        ("description", Json::Str(desc.into())),
    ])
}

/// A bare (description-less) object schema fragment: `{"type":"object",
/// "properties":…,"required":…}` — factored out of [`tool`] so a nested
/// array-of-objects `items` shape (e.g. `sheet.pivot`'s `values:
/// [{col,agg}]`) can reuse it without a name/description wrapper.
fn object_schema(props: Vec<(&str, Json)>, required: &[&str]) -> Json {
    Json::obj(vec![
        ("type", Json::Str("object".into())),
        ("properties", Json::obj(props)),
        (
            "required",
            Json::Arr(required.iter().map(|s| Json::Str((*s).into())).collect()),
        ),
    ])
}

/// An MCP tool definition with an object input schema.
pub fn tool(name: &str, description: &str, props: Vec<(&str, Json)>, required: &[&str]) -> Json {
    Json::obj(vec![
        ("name", Json::Str(name.into())),
        ("description", Json::Str(description.into())),
        ("inputSchema", object_schema(props, required)),
    ])
}

/// A JSON-schema property for an array-typed arg: `{"type":"array",
/// "items":items,"description":desc}` — [`prop`]'s scalar `{type,
/// description}` shape can't express a nested `items` schema, which a few
/// wave-1 args need (`range.set`'s `rows`, `sheet.pivot`'s `rows`/`cols`/
/// `values`). Compose `items` from [`item_ty`]/[`item_array`]/[`item_obj`].
pub fn prop_array(items: Json, desc: &str) -> Json {
    Json::obj(vec![
        ("type", Json::Str("array".into())),
        ("items", items),
        ("description", Json::Str(desc.into())),
    ])
}

/// A bare `{"type": ty}` items schema — e.g. `item_ty("string")` for
/// `sheet.pivot`'s `rows`/`cols` (arrays of header-name strings).
pub fn item_ty(ty: &str) -> Json {
    Json::obj(vec![("type", Json::Str(ty.into()))])
}

/// A bare array items schema wrapping a nested `items` schema — e.g.
/// `range.set`'s `rows: [[string]]` (each row an array of cell-text
/// strings): `item_array(item_ty("string"))`.
pub fn item_array(items: Json) -> Json {
    Json::obj(vec![("type", Json::Str("array".into())), ("items", items)])
}

/// A bare object items schema — e.g. `sheet.pivot`'s `values: [{col,agg}]`.
pub fn item_obj(props: Vec<(&str, Json)>, required: &[&str]) -> Json {
    object_schema(props, required)
}

/// A JSON-schema property for an object-typed arg: `{"type":"object",
/// "properties":…,"required":…,"description":desc}` — [`item_obj`]'s bare
/// object schema plus a top-level `description`, for a nested-object
/// argument that itself needs documenting, e.g. `cell.format`'s `patch`.
pub fn prop_obj(props: Vec<(&str, Json)>, required: &[&str], desc: &str) -> Json {
    let Json::Obj(mut pairs) = object_schema(props, required) else {
        unreachable!("object_schema always returns Json::Obj")
    };
    pairs.push(("description".to_string(), Json::Str(desc.into())));
    Json::Obj(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_with(handler: &dyn Fn(&str, &Json) -> Result<String, String>) -> McpServer<'_> {
        McpServer {
            name: "testapp",
            version: "1.2.3",
            tools: Json::Arr(vec![tool(
                "testapp_echo",
                "Echo the input.",
                vec![("text", prop("string", "What to echo."))],
                &["text"],
            )]),
            handler,
        }
    }

    fn req(method: &str, id: i64) -> Json {
        Json::obj(vec![
            ("jsonrpc", Json::Str("2.0".into())),
            ("method", Json::Str(method.into())),
            ("id", Json::Num(id as f64)),
        ])
    }

    #[test]
    fn initialize_advertises_identity_and_tools() {
        let h = |_: &str, _: &Json| Ok(String::new());
        let s = server_with(&h);
        let r = s.handle(&req("initialize", 1)).unwrap();
        let result = r.get("result").unwrap();
        assert_eq!(result.get_str("protocolVersion"), Some(PROTOCOL_VERSION));
        assert!(result.get("capabilities").unwrap().get("tools").is_some());
        let info = result.get("serverInfo").unwrap();
        assert_eq!(info.get_str("name"), Some("testapp"));
        assert_eq!(info.get_str("version"), Some("1.2.3"));
        assert_eq!(r.get("id").unwrap().as_i64(), Some(1));
    }

    #[test]
    fn tools_list_returns_definitions_with_schemas() {
        let h = |_: &str, _: &Json| Ok(String::new());
        let s = server_with(&h);
        let r = s.handle(&req("tools/list", 2)).unwrap();
        let tools = r
            .get("result")
            .unwrap()
            .get("tools")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].get_str("name"), Some("testapp_echo"));
        assert_eq!(
            tools[0].get("inputSchema").unwrap().get_str("type"),
            Some("object")
        );
    }

    #[test]
    fn tool_call_routes_to_handler_and_wraps_results() {
        let h = |name: &str, args: &Json| {
            if name == "testapp_echo" {
                Ok(format!("echo:{}", args.get_str("text").unwrap_or("")))
            } else {
                Err("unknown tool".into())
            }
        };
        let s = server_with(&h);
        let call = Json::obj(vec![
            ("jsonrpc", Json::Str("2.0".into())),
            ("method", Json::Str("tools/call".into())),
            ("id", Json::Num(3.0)),
            (
                "params",
                Json::obj(vec![
                    ("name", Json::Str("testapp_echo".into())),
                    (
                        "arguments",
                        Json::obj(vec![("text", Json::Str("hi".into()))]),
                    ),
                ]),
            ),
        ]);
        let r = s.handle(&call).unwrap();
        let result = r.get("result").unwrap();
        assert_eq!(result.get("isError").unwrap().as_bool(), Some(false));
        let text = result.get("content").unwrap().as_array().unwrap()[0]
            .get_str("text")
            .unwrap();
        assert_eq!(text, "echo:hi");

        // A handler error becomes an isError result, not a protocol error.
        let bad = Json::obj(vec![
            ("jsonrpc", Json::Str("2.0".into())),
            ("method", Json::Str("tools/call".into())),
            ("id", Json::Num(4.0)),
            (
                "params",
                Json::obj(vec![("name", Json::Str("nope".into()))]),
            ),
        ]);
        let r = s.handle(&bad).unwrap();
        assert_eq!(
            r.get("result").unwrap().get("isError").unwrap().as_bool(),
            Some(true)
        );
    }

    #[test]
    fn notifications_get_no_response_and_unknown_methods_error() {
        let h = |_: &str, _: &Json| Ok(String::new());
        let s = server_with(&h);
        let note = Json::obj(vec![
            ("jsonrpc", Json::Str("2.0".into())),
            ("method", Json::Str("notifications/initialized".into())),
        ]);
        assert!(s.handle(&note).is_none());
        let r = s.handle(&req("frobnicate", 5)).unwrap();
        assert_eq!(
            r.get("error").unwrap().get("code").unwrap().as_i64(),
            Some(-32601)
        );
    }

    #[test]
    fn prop_array_wraps_items_with_type_and_description() {
        let p = prop_array(item_ty("string"), "Some strings.");
        assert_eq!(p.get_str("type"), Some("array"));
        assert_eq!(p.get_str("description"), Some("Some strings."));
        assert_eq!(p.get("items").unwrap().get_str("type"), Some("string"));
    }

    #[test]
    fn item_ty_is_a_bare_type_only_fragment() {
        let it = item_ty("integer");
        assert_eq!(it.get_str("type"), Some("integer"));
        assert!(it.get("description").is_none());
    }

    #[test]
    fn item_array_wraps_a_nested_items_schema() {
        let it = item_array(item_ty("string"));
        assert_eq!(it.get_str("type"), Some("array"));
        assert_eq!(it.get("items").unwrap().get_str("type"), Some("string"));
        assert!(it.get("description").is_none());
    }

    #[test]
    fn item_obj_builds_a_bare_object_schema_with_required() {
        let it = item_obj(
            vec![
                ("col", prop("string", "col desc")),
                ("agg", prop("string", "agg desc")),
            ],
            &["col", "agg"],
        );
        assert_eq!(it.get_str("type"), Some("object"));
        assert!(it.get("properties").unwrap().get("col").is_some());
        assert!(it.get("properties").unwrap().get("agg").is_some());
        assert_eq!(it.get("required").unwrap().to_string(), "[\"col\",\"agg\"]");
        assert!(it.get("description").is_none());
    }

    #[test]
    fn prop_obj_is_item_obj_plus_a_top_level_description() {
        let p = prop_obj(
            vec![("bold", prop("boolean", "bold desc"))],
            &[],
            "patch desc",
        );
        assert_eq!(p.get_str("type"), Some("object"));
        assert!(p.get("properties").unwrap().get("bold").is_some());
        assert_eq!(p.get("required").unwrap().to_string(), "[]");
        assert_eq!(p.get_str("description"), Some("patch desc"));
    }
}
