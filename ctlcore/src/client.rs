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

/// A discovery directory and the application prefix to select within it.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub dir: &'a Path,
    pub app: &'a str,
}

fn candidates<'a>(sources: &[Source<'a>]) -> Vec<(Instance, &'a str)> {
    let mut found = Vec::new();
    for source in sources {
        let prefix = format!("{}-", source.app);
        for instance in discover(source.dir) {
            if instance.instance.starts_with(&prefix) && instance.is_live() {
                found.push((instance, source.app));
            }
        }
    }
    found.sort_by(|(a, _), (b, _)| a.instance.cmp(&b.instance));
    found
}

/// List live instances across discovery sources. Rows include `app` to
/// distinguish applications that accept the same control verbs.
pub fn list_running_in(sources: &[Source<'_>]) -> Json {
    let running = candidates(sources)
        .into_iter()
        .map(|(i, app)| {
            Json::obj(vec![
                ("instance", Json::Str(i.instance)),
                ("port", Json::Num(i.port as f64)),
                ("pid", Json::Num(i.pid as f64)),
                ("app", Json::Str(app.to_string())),
            ])
        })
        .collect();
    Json::obj(vec![("running", Json::Arr(running))])
}

/// The running instances of `app`, as `{"running":[{instance,port,pid,app},…]}`.
pub fn list_running(dir: &Path, app: &str) -> Json {
    list_running_in(&[Source { dir, app }])
}

/// Resolve one live instance over the combined candidate set, optionally
/// filtered by an instance/pane-id substring. Returns its client and app name.
pub fn resolve_target_in<'a>(
    sources: &[Source<'a>],
    target: Option<&str>,
) -> Result<(Client, &'a str), String> {
    let mut live = candidates(sources);
    if let Some(target) = target {
        live.retain(|(i, _)| i.instance.contains(target));
    }
    let apps = sources.iter().map(|s| s.app).collect::<Vec<_>>().join("/");
    match live.len() {
        0 => Err(format!(
            "no running {apps} found — open a document in a {apps} pane first"
        )),
        1 => {
            let (instance, app) = live.remove(0);
            Ok((instance.client(), app))
        }
        _ => {
            let names = live
                .iter()
                .map(|(i, _)| i.instance.as_str())
                .collect::<Vec<_>>();
            Err(format!(
                "several {apps} instances are running ({}); pass \"target\" with a distinguishing substring (e.g. the pane id)",
                names.join(", ")
            ))
        }
    }
}

/// Find the single running `app`, optionally filtered by a `target` substring.
pub fn resolve_target(dir: &Path, app: &str, target: Option<&str>) -> Result<Client, String> {
    resolve_target_in(&[Source { dir, app }], target).map(|(client, _)| client)
}

/// Like [`resolve_target`], but for tools that can proceed without any
/// instance: zero live instances with no `target` is `Ok(None)` instead of an
/// error. A `target` that matches nothing, or an ambiguous candidate set, is
/// still an error — a bad target must not be silently ignored.
pub fn resolve_target_for_new(
    dir: &Path,
    app: &str,
    target: Option<&str>,
) -> Result<Option<Client>, String> {
    let prefix = format!("{app}-");
    let mut live: Vec<_> = discover_live(dir)
        .into_iter()
        .filter(|i| i.instance.starts_with(&prefix))
        .collect();
    if let Some(target) = target {
        live.retain(|i| i.instance.contains(target));
        if live.is_empty() {
            return Err(format!("no running {app} matches target \"{target}\""));
        }
    }
    match live.len() {
        0 => Ok(None),
        1 => Ok(Some(live.remove(0).client())),
        _ => {
            let names: Vec<&str> = live.iter().map(|i| i.instance.as_str()).collect();
            Err(format!(
                "several {app} instances are running ({}); pass \"target\" with a distinguishing substring (e.g. the pane id)",
                names.join(", ")
            ))
        }
    }
}

/// The shared engine of the `docxy_new`/`xlsxy_new` MCP tools: create a new
/// file from `blank` bytes at `args.path` (absolutized against this process's
/// cwd — the creating process and the target instance have different cwds, so
/// the absolute path is used both for creation and in the open request), then
/// open it in the resolved `app` instance via `open_verb`. Resolution runs
/// FIRST so a bad or ambiguous target creates nothing; with no target and no
/// live instance the file is still created and `opened` is false. Refuses to
/// overwrite an existing file.
pub fn new_file(
    dir: &Path,
    app: &str,
    open_verb: &str,
    blank: &[u8],
    args: &Json,
) -> Result<Json, String> {
    let path = args.get_str("path").ok_or("missing path")?;
    let abs = std::path::absolute(Path::new(path)).map_err(|e| format!("bad path: {e}"))?;
    let client = resolve_target_for_new(dir, app, args.get_str("target"))?;
    if abs.exists() {
        return Err(format!("already exists: {}", abs.display()));
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create failed: {e}"))?;
    }
    // create_new: exclusive-create, so a file appearing between the exists
    // check above and this open errors instead of being truncated.
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&abs)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("already exists: {}", abs.display())
            } else {
                format!("create failed: {e}")
            }
        })?;
    std::io::Write::write_all(&mut f, blank).map_err(|e| format!("create failed: {e}"))?;
    let abs_str = abs.display().to_string();
    match client {
        Some(client) => {
            client
                .call(
                    open_verb,
                    Json::obj(vec![("path", Json::Str(abs_str.clone()))]),
                )
                .map_err(|e| format!("created {abs_str} but open failed: {e}"))?;
            let name = client.instance().instance.clone();
            Ok(Json::obj(vec![
                ("path", Json::Str(abs_str)),
                ("opened", Json::Bool(true)),
                ("instance", Json::Str(name)),
            ]))
        }
        None => Ok(Json::obj(vec![
            ("path", Json::Str(abs_str)),
            ("opened", Json::Bool(false)),
        ])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve;

    #[test]
    fn combined_sources_list_resolve_and_report_ambiguity() {
        let root = std::env::temp_dir().join(format!("ctlcore-combined-{}", std::process::id()));
        let a = root.join("yppxy");
        let b = root.join("suite");
        let sources = [
            Source {
                dir: &a,
                app: "yppxy",
            },
            Source {
                dir: &b,
                app: "suite",
            },
        ];
        assert!(resolve_target_in(&sources, None).is_err());
        let (suite, _suite_rx) = serve(&b, "suite-left").unwrap();
        let (client, app) = resolve_target_in(&sources, None).unwrap();
        assert_eq!(app, "suite");
        assert_eq!(client.instance().instance, "suite-left");
        let (yppxy, _yppxy_rx) = serve(&a, "yppxy-right").unwrap();
        let (unrelated, _unrelated_rx) = serve(&a, "docxy-other").unwrap();
        // A discovery record whose port has closed must not become a candidate.
        let closed = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        std::fs::write(
            a.join("dead.json"),
            format!(r#"{{"instance":"yppxy-dead","port":{port},"token":"x","pid":1}}"#),
        )
        .unwrap();
        let list = list_running_in(&sources);
        let rows = list.get("running").unwrap().as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get_str("app"), Some("suite"));
        assert_eq!(rows[1].get_str("app"), Some("yppxy"));
        let error = resolve_target_in(&sources, None).err().unwrap();
        assert!(
            error.contains("suite-left")
                && error.contains("yppxy-right")
                && error.contains("pass \"target\"")
        );
        for (target, app) in [("left", "suite"), ("right", "yppxy")] {
            assert_eq!(resolve_target_in(&sources, Some(target)).unwrap().1, app);
        }
        assert!(resolve_target_in(&sources, Some("missing")).is_err());
        drop((suite, yppxy, unrelated));
        std::fs::remove_file(a.join("dead.json")).unwrap();
        std::fs::remove_dir(a).unwrap();
        std::fs::remove_dir(b).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

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

    #[test]
    fn new_file_without_instance_creates_and_reports_unopened() {
        let dir = std::env::temp_dir().join(format!("ctlcore_new_none_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("fresh.docx");
        let args = Json::obj(vec![("path", Json::Str(out.display().to_string()))]);
        let r = new_file(&dir, "docxy", "doc.open", b"BLANK", &args).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"BLANK");
        assert_eq!(r.get("opened").and_then(Json::as_bool), Some(false));
        assert!(r.get_str("path").unwrap().contains("fresh.docx"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_file_refuses_overwrite_and_bad_target_creates_nothing() {
        let dir = std::env::temp_dir().join(format!("ctlcore_new_guard_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("existing.docx");
        std::fs::write(&existing, b"OLD").unwrap();
        let args = Json::obj(vec![("path", Json::Str(existing.display().to_string()))]);
        let err = new_file(&dir, "docxy", "doc.open", b"BLANK", &args).unwrap_err();
        assert!(err.starts_with("already exists: "), "{err}");
        assert_eq!(std::fs::read(&existing).unwrap(), b"OLD");

        // A target that matches nothing errors and writes nothing.
        let fresh = dir.join("never.docx");
        let args = Json::obj(vec![
            ("path", Json::Str(fresh.display().to_string())),
            ("target", Json::Str("nope".into())),
        ]);
        let err = new_file(&dir, "docxy", "doc.open", b"BLANK", &args).unwrap_err();
        assert_eq!(err, "no running docxy matches target \"nope\"");
        assert!(!fresh.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_file_with_live_instance_creates_then_opens() {
        let dir = std::env::temp_dir().join(format!("ctlcore_new_live_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (server, rx) = serve(&dir, "docxy-new-test").unwrap();
        let (tx, opened_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for req in rx {
                if req.verb == "doc.open" {
                    tx.send(req.args.get_str("path").unwrap_or("").to_string())
                        .ok();
                    req.reply_ok(Json::obj(vec![("path", Json::Str("x".into()))]));
                } else {
                    req.reply_err("nope");
                }
            }
        });
        let out = dir.join("opened.docx");
        let args = Json::obj(vec![("path", Json::Str(out.display().to_string()))]);
        let r = new_file(&dir, "docxy", "doc.open", b"BLANK", &args).unwrap();
        assert_eq!(r.get("opened").and_then(Json::as_bool), Some(true));
        assert_eq!(r.get_str("instance"), Some("docxy-new-test"));
        // The instance received the SAME absolute path that was created.
        let sent = opened_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(sent, r.get_str("path").unwrap());
        assert!(out.exists());
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
