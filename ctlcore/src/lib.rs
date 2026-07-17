//! Ctlcore — a tiny, dependency-free control surface for the office TUIs.
//!
//! A running editor (docxy/xlsxy/yppxy) calls [`serve`] to open a loopback-only
//! TCP listener speaking **newline-delimited JSON**, and drops a small discovery
//! file so an external agent — e.g. Claude Code in a sibling agwinterm pane —
//! can find and drive the *live* document, edits landing on the editor's own
//! undo stack rather than fighting the file on disk.
//!
//! # Wire protocol
//!
//! One JSON object per line, one response line per request (a connection carries
//! at most one in-flight request at a time):
//!
//! ```text
//! → {"token":"…","verb":"doc.read","args":{"range":"1..3"},"id":7}
//! ← {"ok":true,"result":{ … },"id":7}
//! ← {"ok":false,"error":"unknown verb","id":7}
//! ```
//!
//! The `token` (published in the discovery file, readable only by the user) must
//! match on every request; loopback binding plus the token keep other local
//! users out. `id`, if present, is echoed back for client-side correlation.
//!
//! # Threading model
//!
//! [`serve`] returns a [`Receiver<Request>`]. Each accepted connection is served
//! on its own thread that parses a line, forwards a [`Request`] over the channel,
//! and **blocks** until the consumer calls [`Request::reply_ok`] / [`Request::reply_err`].
//! The consumer is expected to be the editor's main thread, so every request is
//! applied to the document with no shared-state locking, and the reply is written
//! back to the socket by the connection thread. A request dropped without a reply
//! answers the client with a generic error, so a client never hangs.

pub mod client;
pub mod json;

use json::Json;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{SystemTime, UNIX_EPOCH};

/// A single control request handed to the consumer. Reply exactly once with
/// [`reply_ok`](Request::reply_ok) or [`reply_err`](Request::reply_err); if the
/// value is dropped without a reply, the client receives a generic error.
pub struct Request {
    /// The requested verb, e.g. `"doc.read"`.
    pub verb: String,
    /// The `args` object (or [`Json::Null`] when the request omitted it).
    pub args: Json,
    /// The client-supplied correlation id, echoed in the reply (`Null` if none).
    id: Json,
    responder: SyncSender<Reply>,
}

impl Request {
    /// Look up `key` inside the request's `args` object.
    pub fn arg(&self, key: &str) -> Option<&Json> {
        self.args.get(key)
    }

    /// Answer the request with a success `result`.
    pub fn reply_ok(self, result: Json) {
        let _ = self.responder.send(Reply {
            ok: true,
            payload: result,
            id: self.id,
        });
    }

    /// Answer the request with an error message.
    pub fn reply_err(self, msg: impl Into<String>) {
        let _ = self.responder.send(Reply {
            ok: false,
            payload: Json::Str(msg.into()),
            id: self.id,
        });
    }
}

struct Reply {
    ok: bool,
    /// The `result` value when `ok`, or an error-message string when not.
    payload: Json,
    id: Json,
}

impl Reply {
    fn to_line(&self) -> String {
        let mut pairs = vec![("ok", Json::Bool(self.ok))];
        if self.ok {
            pairs.push(("result", self.payload.clone()));
        } else {
            pairs.push(("error", self.payload.clone()));
        }
        if self.id != Json::Null {
            pairs.push(("id", self.id.clone()));
        }
        let mut line = Json::obj(pairs).to_string();
        line.push('\n');
        line
    }
}

/// A live control server. Dropping it removes the discovery file (and stops the
/// acceptor loop); in-flight connection threads are detached and end when their
/// sockets close or the process exits.
pub struct Server {
    discovery: PathBuf,
    port: u16,
    token: String,
    shutdown: Arc<AtomicBool>,
}

impl Server {
    /// The loopback port the server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The shared secret a client must present on every request.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The path of the discovery file describing this server.
    pub fn discovery_path(&self) -> &Path {
        &self.discovery
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Nudge the blocking `accept()` so the acceptor thread observes the flag.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        let _ = std::fs::remove_file(&self.discovery);
    }
}

/// Start a control server for `instance` (e.g. `"docxy-<session id>"`), writing
/// its discovery file into `dir`. Returns the [`Server`] handle (keep it alive
/// for the lifetime of the surface) and the [`Receiver`] of incoming requests.
///
/// The discovery file is `dir/<instance>.json`:
/// `{"instance":"…","port":N,"token":"…","pid":N}`.
pub fn serve(dir: &Path, instance: &str) -> std::io::Result<(Server, Receiver<Request>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let token = mint_token(port);

    std::fs::create_dir_all(dir)?;
    let discovery = dir.join(format!("{instance}.json"));
    let contents = Json::obj(vec![
        ("instance", Json::Str(instance.to_string())),
        ("port", Json::Num(port as f64)),
        ("token", Json::Str(token.clone())),
        ("pid", Json::Num(std::process::id() as f64)),
    ])
    .to_string();
    // Write to a temp sibling then rename so a reader never sees a half-written
    // file (rename is atomic on the same volume on both Windows and Unix).
    let tmp = dir.join(format!("{instance}.json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, &discovery)?;

    // Editors that were hard-killed (e.g. by the terminal closing the pane) never
    // run `Server`'s Drop, so their discovery files linger. Sweep them now: any
    // sibling file whose port no longer accepts a connection is dead. This keeps
    // the directory self-healing without needing OS-specific liveness checks.
    sweep_stale(dir, &discovery);

    let (tx, rx) = mpsc::channel::<Request>();
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = Server {
        discovery: discovery.clone(),
        port,
        token: token.clone(),
        shutdown: shutdown.clone(),
    };

    std::thread::Builder::new()
        .name(format!("ctl-accept-{instance}"))
        .spawn(move || acceptor(listener, tx, token, shutdown))?;

    Ok((server, rx))
}

/// Remove discovery files (other than `own`) in `dir` whose advertised port no
/// longer accepts a loopback connection — i.e. whose editor is gone. A live
/// server accepts within the short timeout; a dead one is refused immediately.
fn sweep_stale(dir: &Path, own: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == own || path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Leave anything we can't parse as a discovery record untouched.
        let Ok(doc) = Json::parse(&text) else { continue };
        let Some(port) = doc.get("port").and_then(Json::as_i64) else {
            continue;
        };
        if !(1..=65535).contains(&port) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port as u16));
        if TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(150)).is_err() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Accept connections until the [`Server`] is dropped, serving each on its own
/// thread.
fn acceptor(
    listener: TcpListener,
    tx: mpsc::Sender<Request>,
    token: String,
    shutdown: Arc<AtomicBool>,
) {
    for stream in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let tx = tx.clone();
        let token = token.clone();
        let _ = std::thread::Builder::new()
            .name("ctl-conn".into())
            .spawn(move || serve_connection(stream, tx, &token));
    }
}

/// Read newline-delimited requests from one client until it disconnects.
fn serve_connection(stream: TcpStream, tx: mpsc::Sender<Request>, token: &str) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut writer = stream;
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return, // client closed
            Ok(_) => {}
            Err(_) => return,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let reply = match dispatch_line(trimmed, token, &tx) {
            Ok(reply) => reply,
            Err(reply) => reply,
        };
        if writer.write_all(reply.as_bytes()).is_err() || writer.flush().is_err() {
            return;
        }
    }
}

/// Validate and route one request line, returning the reply line to write back.
/// `Err` and `Ok` both carry a ready-to-write line; they differ only so the
/// happy path is obvious.
fn dispatch_line(line: &str, token: &str, tx: &mpsc::Sender<Request>) -> Result<String, String> {
    let msg = match Json::parse(line) {
        Ok(m) => m,
        Err(e) => return Err(err_line(&Json::Null, &format!("invalid json: {e}"))),
    };
    let id = msg.get("id").cloned().unwrap_or(Json::Null);

    if msg.get_str("token") != Some(token) {
        return Err(err_line(&id, "unauthorized: bad or missing token"));
    }
    let Some(verb) = msg.get_str("verb") else {
        return Err(err_line(&id, "missing verb"));
    };
    let args = msg.get("args").cloned().unwrap_or(Json::Null);

    let (responder, wait) = mpsc::sync_channel::<Reply>(1);
    let request = Request {
        verb: verb.to_string(),
        args,
        id: id.clone(),
        responder,
    };
    if tx.send(request).is_err() {
        return Err(err_line(&id, "server shutting down"));
    }
    // Block for the consumer's reply. If the `Request` was dropped un-answered,
    // the channel closes and we synthesize a generic error rather than hang.
    match wait.recv() {
        Ok(reply) => Ok(reply.to_line()),
        Err(_) => Err(err_line(&id, "request dropped without a reply")),
    }
}

fn err_line(id: &Json, msg: &str) -> String {
    Reply {
        ok: false,
        payload: Json::Str(msg.to_string()),
        id: id.clone(),
    }
    .to_line()
}

/// Derive a hard-to-guess token from process-unique, per-run entropy. This is a
/// loopback dev-tool capability check, not a cryptographic secret: it combines
/// the wall clock, the pid, the chosen port, and an ASLR-influenced heap address.
fn mint_token(port: u16) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let heap = Box::new(0u8);
    let addr = (&*heap as *const u8) as u128;
    let mixed = nanos
        ^ (pid << 17)
        ^ ((port as u128) << 40)
        ^ addr.rotate_left(29)
        ^ addr.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!("{mixed:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::time::Duration;

    /// Drive the consumer side on a detached background thread: echo the verb
    /// back as the result, or error on `verb == "boom"`. Detached (not joined) so
    /// a still-open client socket — which keeps a connection thread, and thus a
    /// `tx`, alive — can never deadlock the test on `rx`.
    fn spawn_echo_consumer(rx: Receiver<Request>) {
        std::thread::spawn(move || {
            for req in rx {
                if req.verb == "boom" {
                    req.reply_err("kaboom");
                } else {
                    let verb = req.verb.clone();
                    let n = req.arg("n").and_then(Json::as_i64).unwrap_or(0);
                    req.reply_ok(Json::obj(vec![
                        ("verb", Json::Str(verb)),
                        ("n", Json::Num(n as f64)),
                    ]));
                }
            }
        });
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ctlcore_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Send one request line, read one reply line.
    fn roundtrip(port: u16, line: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(line.as_bytes()).unwrap();
        s.write_all(b"\n").unwrap();
        let mut reader = BufReader::new(s);
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        reply.trim().to_string()
    }

    #[test]
    fn discovery_file_written_and_removed_on_drop() {
        let dir = tmp_dir("discovery");
        let (server, rx) = serve(&dir, "docxy-abc").unwrap();
        let path = server.discovery_path().to_path_buf();
        assert!(path.exists());
        let doc = Json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc.get_str("instance"), Some("docxy-abc"));
        assert_eq!(doc.get("port").unwrap().as_i64(), Some(server.port() as i64));
        assert_eq!(doc.get_str("token"), Some(server.token()));
        drop(rx);
        drop(server);
        assert!(!path.exists(), "discovery file removed on drop");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn happy_path_request_response_with_token() {
        let dir = tmp_dir("happy");
        let (server, rx) = serve(&dir, "docxy-1").unwrap();
        spawn_echo_consumer(rx);
        let line = format!(
            "{{\"token\":\"{}\",\"verb\":\"doc.read\",\"args\":{{\"n\":5}},\"id\":9}}",
            server.token()
        );
        let reply = roundtrip(server.port(), &line);
        assert_eq!(reply, "{\"ok\":true,\"result\":{\"verb\":\"doc.read\",\"n\":5},\"id\":9}");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn error_reply_is_marked_not_ok() {
        let dir = tmp_dir("boom");
        let (server, rx) = serve(&dir, "docxy-2").unwrap();
        spawn_echo_consumer(rx);
        let line = format!("{{\"token\":\"{}\",\"verb\":\"boom\"}}", server.token());
        let reply = roundtrip(server.port(), &line);
        assert_eq!(reply, "{\"ok\":false,\"error\":\"kaboom\"}");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_token_is_rejected_before_reaching_the_consumer() {
        let dir = tmp_dir("auth");
        let (server, rx) = serve(&dir, "docxy-3").unwrap();
        spawn_echo_consumer(rx);
        let reply = roundtrip(
            server.port(),
            "{\"token\":\"wrong\",\"verb\":\"doc.read\",\"id\":1}",
        );
        assert_eq!(
            reply,
            "{\"ok\":false,\"error\":\"unauthorized: bad or missing token\",\"id\":1}"
        );
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_json_yields_an_error_line() {
        let dir = tmp_dir("malformed");
        let (server, rx) = serve(&dir, "docxy-4").unwrap();
        spawn_echo_consumer(rx);
        let reply = roundtrip(server.port(), "not json");
        assert!(reply.starts_with("{\"ok\":false,\"error\":\"invalid json:"));
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_sweeps_dead_discovery_files_but_keeps_live_ones() {
        let dir = tmp_dir("sweep");
        std::fs::create_dir_all(&dir).unwrap();
        // A live server whose file must survive the sweep.
        let (alive, _rx) = serve(&dir, "docxy-alive").unwrap();
        // A stale record pointing at a port nobody is listening on.
        let dead_port = {
            let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port() // freed when `l` drops at block end
        };
        let stale = dir.join("docxy-dead.json");
        std::fs::write(
            &stale,
            format!("{{\"instance\":\"docxy-dead\",\"port\":{dead_port},\"token\":\"x\",\"pid\":1}}"),
        )
        .unwrap();
        // A second server starting up runs the sweep.
        let (fresh, _rx2) = serve(&dir, "docxy-fresh").unwrap();
        assert!(!stale.exists(), "dead discovery file swept");
        assert!(alive.discovery_path().exists(), "live server's file kept");
        assert!(fresh.discovery_path().exists());
        drop(alive);
        drop(fresh);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multiple_requests_on_one_connection() {
        let dir = tmp_dir("multi");
        let (server, rx) = serve(&dir, "docxy-5").unwrap();
        spawn_echo_consumer(rx);
        let mut s = TcpStream::connect(("127.0.0.1", server.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let tok = server.token();
        for i in 0..3 {
            s.write_all(
                format!("{{\"token\":\"{tok}\",\"verb\":\"v\",\"args\":{{\"n\":{i}}},\"id\":{i}}}\n")
                    .as_bytes(),
            )
            .unwrap();
        }
        let mut reader = BufReader::new(s);
        for i in 0..3 {
            let mut reply = String::new();
            reader.read_line(&mut reply).unwrap();
            assert_eq!(
                reply.trim(),
                format!("{{\"ok\":true,\"result\":{{\"verb\":\"v\",\"n\":{i}}},\"id\":{i}}}")
            );
        }
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
