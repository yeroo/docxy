//! In-process fake HTTP server for tests. Not part of the shipped binary.
//!
//! Binds an ephemeral loopback port, speaks just enough HTTP/1.1 to serve
//! canned [`Route`] responses and record the requests it received. One
//! request per connection, no keep-alive — this is test infrastructure for
//! `auth`, `graph`, and `sync` tests, not a general-purpose server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A canned response served for requests whose method matches exactly and
/// whose path starts with `path_prefix`.
pub struct Route {
    pub method: String,
    pub path_prefix: String,
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

/// A request the fake server received, captured for test assertions.
///
/// `method`, `headers`, and `body` are unused by this module's own test but
/// are part of the public surface later `auth`/`graph`/`sync` tests rely on.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// An in-process HTTP server for tests. Bind address is ephemeral
/// (`127.0.0.1:0`); point HTTP clients at `base_url`. Shuts its listener
/// thread down on `Drop`.
pub struct FakeServer {
    pub base_url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: Arc<AtomicBool>,
    port: u16,
    handle: Option<JoinHandle<()>>,
}

impl FakeServer {
    pub fn start(routes: Vec<Route>) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback port");
        let port = listener.local_addr().expect("local_addr").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread_requests = Arc::clone(&requests);
        let thread_shutdown = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if thread_shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                handle_connection(stream, &routes, &thread_requests);
            }
        });

        FakeServer {
            base_url: format!("http://127.0.0.1:{port}"),
            requests,
            shutdown,
            port,
            handle: Some(handle),
        }
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Unblock the listener thread's blocking `accept` with a throwaway
        // connection; ignore failures (thread may already be exiting).
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Read one HTTP/1.1 request (request line, headers, optional
/// Content-Length body), match it against `routes`, write the response, and
/// record the request.
fn handle_connection(
    stream: TcpStream,
    routes: &[Route],
    requests: &Arc<Mutex<Vec<RecordedRequest>>>,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return; // throwaway shutdown connection or client closed early
    }
    let mut parts = request_line.split_whitespace();
    let Some(method) = parts.next() else { return };
    let Some(path) = parts.next() else { return };
    let method = method.to_string();
    let path = path.to_string();

    let mut headers = Vec::new();
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }

    let mut body_bytes = vec![0u8; content_length];
    if content_length > 0 && reader.read_exact(&mut body_bytes).is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&body_bytes).into_owned();

    requests.lock().expect("requests lock").push(RecordedRequest {
        method: method.clone(),
        path: path.clone(),
        headers,
        body,
    });

    let route = routes
        .iter()
        .find(|r| r.method == method && path.starts_with(r.path_prefix.as_str()));

    let mut stream = stream;
    match route {
        Some(route) => {
            let mut response = format!(
                "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n",
                route.status,
                reason_phrase(route.status),
                route.body.len()
            );
            for (name, value) in &route.headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str("\r\n");
            response.push_str(&route.body);
            let _ = stream.write_all(response.as_bytes());
        }
        None => {
            let body = "not found";
            let response = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    }
    let _ = stream.flush();
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_canned_response_and_records_request() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/ping".into(),
            status: 200,
            body: r#"{"ok":true}"#.into(),
            headers: vec![],
        }]);
        let resp = ureq::get(&format!("{}/ping", srv.base_url)).call().unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.into_string().unwrap(), r#"{"ok":true}"#);
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, "/ping");
    }
}
