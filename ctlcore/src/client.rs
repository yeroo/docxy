//! The client side of a [`ctlcore`](crate) control surface: read discovery
//! files to find running servers, then send verbs and get results back.
//!
//! Used by adapters that bridge the control protocol to another surface (e.g.
//! docxy's MCP server), and by tests.

use crate::json::Json;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

/// A control endpoint parsed from a discovery file.
#[derive(Debug, Clone)]
pub struct Instance {
    pub instance: String,
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

impl Instance {
    fn from_json(j: &Json) -> Option<Instance> {
        Some(Instance {
            instance: j.get_str("instance")?.to_string(),
            port: u16::try_from(j.get("port")?.as_i64()?).ok()?,
            token: j.get_str("token")?.to_string(),
            pid: j.get("pid").and_then(Json::as_i64).unwrap_or(0) as u32,
        })
    }

    fn addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// Whether the server is currently accepting connections.
    pub fn is_live(&self) -> bool {
        TcpStream::connect_timeout(&self.addr(), Duration::from_millis(200)).is_ok()
    }

    /// A client bound to this instance.
    pub fn client(&self) -> Client {
        Client {
            instance: self.clone(),
        }
    }
}

/// Every discovery record in `dir` (any `*.json` that parses as one), regardless
/// of whether its server is still alive; sorted by instance name.
pub fn discover(dir: &Path) -> Vec<Instance> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(j) = Json::parse(&text) {
            if let Some(inst) = Instance::from_json(&j) {
                out.push(inst);
            }
        }
    }
    out.sort_by(|a, b| a.instance.cmp(&b.instance));
    out
}

/// Discovery records whose server currently accepts a connection.
pub fn discover_live(dir: &Path) -> Vec<Instance> {
    discover(dir)
        .into_iter()
        .filter(Instance::is_live)
        .collect()
}

/// A request/response client. Each [`call`](Client::call) opens a fresh
/// short-lived connection, matching the server's one-request-per-line model.
pub struct Client {
    instance: Instance,
}

impl Client {
    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    /// Send `verb` with `args`, returning the server's `result` JSON — or an
    /// `Err` carrying either a transport failure or the server's own
    /// `{ok:false,error}` message.
    pub fn call(&self, verb: &str, args: Json) -> Result<Json, String> {
        let mut stream =
            TcpStream::connect_timeout(&self.instance.addr(), Duration::from_millis(500))
                .map_err(|e| format!("connect failed: {e}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();

        let mut line = Json::obj(vec![
            ("token", Json::Str(self.instance.token.clone())),
            ("verb", Json::Str(verb.to_string())),
            ("args", args),
        ])
        .to_string();
        line.push('\n');
        stream
            .write_all(line.as_bytes())
            .map_err(|e| format!("write failed: {e}"))?;
        stream.flush().ok();

        let mut reader = BufReader::new(stream);
        let mut resp = String::new();
        reader
            .read_line(&mut resp)
            .map_err(|e| format!("read failed: {e}"))?;
        let j = Json::parse(resp.trim()).map_err(|e| format!("bad response: {e}"))?;
        if j.get("ok").and_then(Json::as_bool) == Some(true) {
            Ok(j.get("result").cloned().unwrap_or(Json::Null))
        } else {
            Err(j.get_str("error").unwrap_or("unknown error").to_string())
        }
    }
}

/// The running instances of `app` (discovery files under `dir` whose instance
/// name starts with `<app>-` and whose server accepts a connection), as a JSON
/// tool result: `{"running":[{instance,port,pid},…]}`.
pub fn list_running(dir: &Path, app: &str) -> Json {
    let prefix = format!("{app}-");
    let running = discover_live(dir)
        .into_iter()
        .filter(|i| i.instance.starts_with(&prefix))
        .map(|i| {
            Json::obj(vec![
                ("instance", Json::Str(i.instance)),
                ("port", Json::Num(i.port as f64)),
                ("pid", Json::Num(i.pid as f64)),
            ])
        })
        .collect();
    Json::obj(vec![("running", Json::Arr(running))])
}

/// Find the single `app` instance to act on: the only one running, or the one
/// selected by a `target` substring of its instance/pane id.
pub fn resolve_target(dir: &Path, app: &str, target: Option<&str>) -> Result<Client, String> {
    let prefix = format!("{app}-");
    let mut live: Vec<_> = discover_live(dir)
        .into_iter()
        .filter(|i| i.instance.starts_with(&prefix))
        .collect();
    if let Some(target) = target {
        live.retain(|i| i.instance.contains(target));
    }
    match live.len() {
        0 => Err(format!(
            "no running {app} found — open a document in a {app} pane first"
        )),
        1 => Ok(live.remove(0).client()),
        _ => {
            let names: Vec<&str> = live.iter().map(|i| i.instance.as_str()).collect();
            Err(format!(
                "several {app} instances are running ({}); pass \"target\" with a distinguishing substring (e.g. the pane id)",
                names.join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve;

    #[test]
    fn discovers_and_calls_a_live_server() {
        let dir = std::env::temp_dir().join(format!("ctlcore_client_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (server, rx) = serve(&dir, "docxy-client-test").unwrap();
        // Consumer that answers doc.path-style calls.
        std::thread::spawn(move || {
            for req in rx {
                if req.verb == "doc.path" {
                    req.reply_ok(Json::obj(vec![("path", Json::Str("x.docx".into()))]));
                } else {
                    req.reply_err("nope");
                }
            }
        });

        let live = discover_live(&dir);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].instance, "docxy-client-test");
        assert_eq!(live[0].port, server.port());

        let client = live[0].client();
        let r = client.call("doc.path", Json::Null).unwrap();
        assert_eq!(r.get_str("path"), Some("x.docx"));
        assert_eq!(client.call("nope", Json::Null).unwrap_err(), "nope");

        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_instances_are_filtered_out() {
        let dir = std::env::temp_dir().join(format!("ctlcore_dead_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("docxy-ghost.json"),
            "{\"instance\":\"docxy-ghost\",\"port\":9,\"token\":\"t\",\"pid\":1}",
        )
        .unwrap();
        assert_eq!(discover(&dir).len(), 1);
        assert_eq!(discover_live(&dir).len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
