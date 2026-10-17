//! The client side of a [`ctlcore`](crate) control surface: read discovery
//! files to find running servers, then send verbs and get results back.
//!
//! Used by adapters that bridge the control protocol to another surface (e.g.
//! docxy's MCP server), and by tests.

use crate::json::Json;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Mutex;
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
        self.probe().is_some()
    }

    /// A client bound to this instance.
    pub fn client(&self) -> Client {
        Client {
            instance: self.clone(),
            conn: Mutex::new(None),
        }
    }

    /// Open a connection, giving up after `timeout`.
    fn open(&self, timeout: Duration) -> Result<Conn, String> {
        let stream = TcpStream::connect_timeout(&self.addr(), timeout)
            .map_err(|e| format!("connect failed: {e}"))?;
        stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
        let reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|e| format!("connect failed: {e}"))?,
        );
        Ok(Conn {
            reader,
            writer: stream,
            token: self.token.clone(),
        })
    }

    /// The liveness probe: a connection when the server accepts one, which a
    /// caller that goes on to talk to it keeps (a probe it drops costs a
    /// TIME_WAIT port on Windows).
    fn probe(&self) -> Option<Conn> {
        self.open(Duration::from_millis(200)).ok()
    }

    /// A client whose first call uses `conn`, a connection to this instance
    /// from [`probe`](Instance::probe).
    fn client_with(&self, conn: Conn) -> Client {
        Client {
            instance: self.clone(),
            conn: Mutex::new(Some(conn)),
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

/// How long a call waits for the server's reply. A cold instance answering its first
/// request - a fresh suite on a slow CI runner opening a document - has taken over 10 s,
/// so this leaves room for that; a hung server still fails the call.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// A request/response client. Its [`call`](Client::call)s share one
/// connection, opened on the first call (unless the client was primed with the
/// connection of the liveness probe that found it) and kept for as long as the
/// `Client` lives: one connection per call left a client port in TIME_WAIT per verb,
/// and a UI harness sweep ran Windows' ephemeral ports out (#1234).
pub struct Client {
    instance: Instance,
    /// The cached connection; `None` for an unprimed client before its first
    /// call, and after a transport failure. The lock also keeps one request in flight at a time.
    conn: Mutex<Option<Conn>>,
}

impl Client {
    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    /// Send `verb` with `args`, returning the server's `result` JSON — or an
    /// `Err` carrying either a transport failure or the server's own
    /// `{ok:false,error}` message.
    ///
    /// A transport failure drops the cached connection, so the next call
    /// connects afresh; the failed call is not re-sent, since the server may
    /// already have run its verb. A refusal keeps the connection.
    pub fn call(&self, verb: &str, args: Json) -> Result<Json, String> {
        // The slot is only a cache, so a lock poisoned by a panicking caller
        // is still fine to use.
        let mut slot = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let conn = match &mut *slot {
            Some(conn) => conn,
            None => slot.insert(self.connect()?),
        };
        match conn.exchange(verb, args) {
            Ok(answer) => answer,
            Err(transport) => {
                *slot = None;
                Err(transport)
            }
        }
    }

    /// A connection the caller owns, separate from the one
    /// [`call`](Client::call) caches, kept open for many calls. It needs
    /// neither this `Client` nor its lock once made, so a caller can hold it
    /// on its own (the UI harness polling a terminal editor's counters,
    /// #1204).
    pub fn connect(&self) -> Result<Conn, String> {
        self.instance.open(Duration::from_millis(500))
    }
}

/// One open connection to a control server; see [`Client::connect`].
pub struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    token: String,
}

impl Conn {
    /// [`Client::call`] on this connection.
    pub fn call(&mut self, verb: &str, args: Json) -> Result<Json, String> {
        self.exchange(verb, args)?
    }

    /// One request and its reply. The outer `Err` is a transport or protocol
    /// failure, after which the stream can no longer be trusted (a late reply
    /// would answer the next request); the inner one is the server's own
    /// refusal, which leaves the connection usable.
    fn exchange(&mut self, verb: &str, args: Json) -> Result<Result<Json, String>, String> {
        let mut line = Json::obj(vec![
            ("token", Json::Str(self.token.clone())),
            ("verb", Json::Str(verb.to_string())),
            ("args", args),
        ])
        .to_string();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .map_err(|e| format!("write failed: {e}"))?;
        self.writer.flush().ok();

        let mut resp = String::new();
        let n = self
            .reader
            .read_line(&mut resp)
            .map_err(|e| match e.kind() {
                // A read timeout surfaces as WouldBlock on Unix and TimedOut on Windows; the
                // raw text ("Resource temporarily unavailable") hides that the server was slow.
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                    format!("no reply within {}s to `{verb}`", READ_TIMEOUT.as_secs())
                }
                _ => format!("read failed: {e}"),
            })?;
        if n == 0 {
            return Err(format!(
                "the server closed the connection before replying to `{verb}`"
            ));
        }
        let j = Json::parse(resp.trim()).map_err(|e| format!("bad response: {e}"))?;
        Ok(if j.get("ok").and_then(Json::as_bool) == Some(true) {
            Ok(j.get("result").cloned().unwrap_or(Json::Null))
        } else {
            Err(j.get_str("error").unwrap_or("unknown error").to_string())
        })
    }
}

/// A discovery directory and the application prefix to select within it.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub dir: &'a Path,
    pub app: &'a str,
}

/// The live instances of `sources` with the connection each probe made, so
/// the one chosen is not connected to a second time.
fn candidates<'a>(sources: &[Source<'a>]) -> Vec<(Instance, &'a str, Conn)> {
    let mut found = Vec::new();
    for source in sources {
        let prefix = format!("{}-", source.app);
        for instance in discover(source.dir) {
            if !instance.instance.starts_with(&prefix) {
                continue;
            }
            if let Some(conn) = instance.probe() {
                found.push((instance, source.app, conn));
            }
        }
    }
    found.sort_by(|(a, ..), (b, ..)| a.instance.cmp(&b.instance));
    found
}

/// List live instances across discovery sources. Rows include `app` to
/// distinguish applications that accept the same control verbs.
pub fn list_running_in(sources: &[Source<'_>]) -> Json {
    let running = candidates(sources)
        .into_iter()
        .map(|(i, app, _)| {
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
        live.retain(|(i, ..)| i.instance.contains(target));
    }
    let apps = sources.iter().map(|s| s.app).collect::<Vec<_>>().join("/");
    match live.len() {
        0 => Err(format!(
            "no running {apps} found — open a document in a {apps} pane first"
        )),
        1 => {
            let (instance, app, conn) = live.remove(0);
            Ok((instance.client_with(conn), app))
        }
        _ => {
            let names = live
                .iter()
                .map(|(i, ..)| i.instance.as_str())
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
    let mut live = candidates(&[Source { dir, app }]);
    if let Some(target) = target {
        live.retain(|(i, ..)| i.instance.contains(target));
        if live.is_empty() {
            return Err(format!("no running {app} matches target \"{target}\""));
        }
    }
    match live.len() {
        0 => Ok(None),
        1 => {
            let (instance, _, conn) = live.remove(0);
            Ok(Some(instance.client_with(conn)))
        }
        _ => {
            let names: Vec<&str> = live.iter().map(|(i, ..)| i.instance.as_str()).collect();
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};

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

    /// A resolved client's first call rides the connection the liveness probe
    /// made, so resolving and calling costs one connection (#1405).
    #[test]
    fn a_resolved_client_reuses_the_probe_connection() {
        let dir = std::env::temp_dir().join(format!("ctlcore_prime_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("docxy-prime.json"),
            format!(r#"{{"instance":"docxy-prime","port":{port},"token":"t","pid":1}}"#),
        )
        .unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&accepted);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                count.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;
                    let mut line = String::new();
                    while reader.read_line(&mut line).unwrap_or(0) > 0 {
                        line.clear();
                        let _ = writer.write_all(b"{\"ok\":true,\"result\":1}\n");
                    }
                });
            }
        });
        let sources = [Source {
            dir: &dir,
            app: "docxy",
        }];
        let (client, _) = resolve_target_in(&sources, None).unwrap();
        assert_eq!(client.call("v", Json::Null).unwrap().as_f64(), Some(1.0));
        assert_eq!(client.call("v", Json::Null).unwrap().as_f64(), Some(1.0));
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        let client = resolve_target_for_new(&dir, "docxy", None)
            .unwrap()
            .unwrap();
        assert_eq!(client.call("v", Json::Null).unwrap().as_f64(), Some(1.0));
        assert_eq!(accepted.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One connection answers many calls, in order (#1204).
    #[test]
    fn one_connection_carries_many_calls() {
        let dir = std::env::temp_dir().join(format!("ctlcore_conn_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (server, rx) = serve(&dir, "docxy-conn-test").unwrap();
        std::thread::spawn(move || {
            for req in rx {
                let n = req.args.get("n").cloned().unwrap_or(Json::Null);
                if req.verb == "echo" {
                    req.reply_ok(n);
                } else {
                    let msg = format!("unknown verb '{}'", req.verb);
                    req.reply_err(msg);
                }
            }
        });
        let client = discover_live(&dir).remove(0).client();
        let mut conn = client.connect().unwrap();
        for i in 0..50 {
            let got = conn
                .call("echo", Json::obj(vec![("n", Json::Num(f64::from(i)))]))
                .unwrap();
            assert_eq!(got.as_f64(), Some(f64::from(i)));
        }
        assert_eq!(
            conn.call("nope", Json::Null).unwrap_err(),
            "unknown verb 'nope'",
            "a refusal leaves the connection usable"
        );
        assert!(
            conn.call("echo", Json::obj(vec![("n", Json::Num(1.0))]))
                .is_ok()
        );
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
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

    /// A stand-in control server on a loopback port that counts the
    /// connections it accepts. It answers `echo` with its `n` and anything
    /// else with a refusal. With `drop_first`, it answers one request on the
    /// first connection, closes that socket and reports on the channel.
    fn stand_in(drop_first: bool) -> (Instance, Arc<AtomicUsize>, mpsc::Receiver<()>) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = Arc::new(AtomicUsize::new(0));
        let (dropped_tx, dropped) = mpsc::channel();
        let counted = accepts.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let first = counted.fetch_add(1, Ordering::SeqCst) == 0;
                let limit = if drop_first && first { Some(1) } else { None };
                let dropped_tx = dropped_tx.clone();
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut answered = 0;
                    let mut line = String::new();
                    while limit != Some(answered) {
                        line.clear();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let req = Json::parse(line.trim()).unwrap();
                        let reply = if req.get_str("verb") == Some("echo") {
                            let n = req
                                .get("args")
                                .and_then(|a| a.get("n"))
                                .cloned()
                                .unwrap_or(Json::Null);
                            Json::obj(vec![("ok", Json::Bool(true)), ("result", n)])
                        } else {
                            Json::obj(vec![
                                ("ok", Json::Bool(false)),
                                ("error", Json::Str("no such verb".into())),
                            ])
                        };
                        writer.write_all(format!("{reply}\n").as_bytes()).unwrap();
                        answered += 1;
                    }
                    drop((reader, writer));
                    dropped_tx.send(()).ok();
                });
            }
        });
        let instance = Instance {
            instance: "stand-in".into(),
            port,
            token: "t".into(),
            pid: 0,
        };
        (instance, accepts, dropped)
    }

    fn echo(n: usize) -> Json {
        Json::obj(vec![("n", Json::Num(n as f64))])
    }

    /// Every call on one `Client` goes over one connection, so a run of verbs
    /// holds no ephemeral port per call (#1234).
    #[test]
    fn a_client_reuses_one_connection() {
        let (instance, accepts, _dropped) = stand_in(false);
        let client = instance.client();
        for i in 0..100 {
            assert_eq!(
                client.call("echo", echo(i)).unwrap().as_f64(),
                Some(i as f64)
            );
        }
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            1,
            "100 calls, one connection"
        );
    }

    /// A refusal is the server's answer, not a broken stream: the connection
    /// stays cached (#1234).
    #[test]
    fn a_refusal_keeps_the_connection() {
        let (instance, accepts, _dropped) = stand_in(false);
        let client = instance.client();
        assert!(client.call("echo", echo(1)).is_ok());
        assert_eq!(client.call("nope", Json::Null).unwrap_err(), "no such verb");
        assert!(client.call("echo", echo(2)).is_ok());
        assert_eq!(accepts.load(Ordering::SeqCst), 1);
    }

    /// When the server end of the cached connection goes away, that call
    /// fails (it is not re-sent: the verb may have run) and the next call
    /// reconnects (#1234).
    #[test]
    fn a_broken_connection_fails_once_then_reconnects() {
        let (instance, accepts, dropped) = stand_in(true);
        let client = instance.client();
        assert!(client.call("echo", echo(1)).is_ok());
        dropped.recv_timeout(Duration::from_secs(5)).unwrap();
        let err = client.call("echo", echo(2)).unwrap_err();
        assert!(
            err == "the server closed the connection before replying to `echo`"
                || err.starts_with("read failed: ")
                || err.starts_with("write failed: "),
            "a transport failure, got: {err}"
        );
        assert_eq!(client.call("echo", echo(3)).unwrap().as_f64(), Some(3.0));
        assert_eq!(accepts.load(Ordering::SeqCst), 2);
    }

    /// The issue's own load: 20,000 sequential calls on one client all
    /// succeed. One connection per call ran Windows' ephemeral ports out with
    /// os error 10048 (#1234).
    #[test]
    fn twenty_thousand_calls_on_one_client_all_succeed() {
        let dir = std::env::temp_dir().join(format!("ctlcore_20k_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (server, rx) = serve(&dir, "docxy-20k-test").unwrap();
        std::thread::spawn(move || {
            for req in rx {
                let n = req.args.get("n").cloned().unwrap_or(Json::Null);
                req.reply_ok(n);
            }
        });
        let client = discover_live(&dir).remove(0).client();
        for i in 0..20_000 {
            let got = client.call("echo", echo(i));
            assert_eq!(got.map(|j| j.as_f64()), Ok(Some(i as f64)), "call {i}");
        }
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
