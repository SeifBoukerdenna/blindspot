//! The one HTTP client blindspot has, for the one server it ever talks to.
//!
//! Hand-written rather than a crate. This speaks HTTP/1.1 to 127.0.0.1, has no TLS, follows no
//! redirects, pools no connections, and makes exactly one kind of request: a POST whose reply
//! streams back as NDJSON. `ureq` would bring a client stack and a TLS dependency for that, and
//! the streaming-with-cancellation behaviour below would still have to be written by hand.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long to wait for the connection itself. Ollama is either running on this machine or it
/// is not, so a second is already generous.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// How long the stream may go silent before it is abandoned. Generous because the first reply
/// after a model is evicted includes loading it — measured at 4.4s for the default model and
/// 17s for the 27B one.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// The socket's own read timeout, which only decides how often the cancel flag is looked at.
const POLL: Duration = Duration::from_millis(100);

/// A reply larger than this is a server gone wrong, not an answer worth reading.
const MAX_BODY: usize = 8 << 20;

#[derive(Debug)]
pub enum HttpError {
    /// Nothing is listening — with Ollama, that means it is not running.
    Connect(std::io::Error),
    Io(std::io::Error),
    /// A non-200 reply and whatever the body said; Ollama sends `{"error": "…"}`.
    Status(u16, String),
    /// A reply this client cannot read as HTTP.
    Malformed(&'static str),
    /// The caller's flag went up: Esc, or the panel was dismissed.
    Cancelled,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "could not reach the server: {e}"),
            Self::Io(e) => write!(f, "connection failed: {e}"),
            Self::Status(code, body) => write!(f, "server returned {code}: {body}"),
            Self::Malformed(what) => write!(f, "unreadable reply: {what}"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// POSTs `body` to `host``path` and hands each line of the reply to `on_line` as it arrives.
///
/// `cancel` is read between reads, so a generation can be stopped without waiting for the
/// model to finish — which is what Esc does while the panel is showing a half-written command.
pub fn post_ndjson(
    host: &str,
    path: &str,
    body: &str,
    cancel: &AtomicBool,
    on_line: impl FnMut(&str),
) -> Result<(), HttpError> {
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    send(host, &request, cancel, on_line)
}

/// A GET whose whole body is wanted at once — the model list, which is small and not streamed.
pub fn get(host: &str, path: &str, cancel: &AtomicBool) -> Result<String, HttpError> {
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let mut body = String::new();
    send(host, &request, cancel, |line| {
        body.push_str(line);
        body.push('\n');
    })?;
    Ok(body)
}

fn send(
    host: &str,
    request: &str,
    cancel: &AtomicBool,
    mut on_line: impl FnMut(&str),
) -> Result<(), HttpError> {
    let address = host
        .to_socket_addrs()
        .map_err(HttpError::Connect)?
        .next()
        .ok_or(HttpError::Malformed("host resolves to nothing"))?;
    let mut stream =
        TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(HttpError::Connect)?;
    stream.set_read_timeout(Some(POLL)).map_err(HttpError::Io)?;
    // Small writes are the whole conversation here, so waiting to coalesce them only adds delay.
    let _ = stream.set_nodelay(true);

    stream
        .write_all(request.as_bytes())
        .map_err(HttpError::Io)?;
    stream.flush().map_err(HttpError::Io)?;

    Reader::new(stream, cancel).run(&mut on_line)
}

/// Reads the reply: headers first, then the body, de-chunked, split into lines.
struct Reader<'a> {
    stream: TcpStream,
    cancel: &'a AtomicBool,
    /// Bytes read but not yet consumed — headers, then chunk framing.
    raw: Vec<u8>,
    /// De-chunked body bytes not yet split into a complete line.
    body: Vec<u8>,
    last_progress: Instant,
}

impl<'a> Reader<'a> {
    fn new(stream: TcpStream, cancel: &'a AtomicBool) -> Self {
        Self {
            stream,
            cancel,
            raw: Vec::new(),
            body: Vec::new(),
            last_progress: Instant::now(),
        }
    }

    fn run(mut self, on_line: &mut impl FnMut(&str)) -> Result<(), HttpError> {
        let (status, chunked) = self.read_headers()?;
        if status != 200 {
            // The body carries the reason — a missing model, a bad request — so it is read to
            // the end rather than reported as a bare number.
            while self.fill()? {}
            let rest = std::mem::take(&mut self.raw);
            let mut reason = String::from_utf8_lossy(&rest).into_owned();
            reason.truncate(200);
            return Err(HttpError::Status(status, reason.trim().to_owned()));
        }

        loop {
            let finished = if chunked {
                self.drain_chunks()?
            } else {
                let taken = std::mem::take(&mut self.raw);
                self.body.extend_from_slice(&taken);
                false
            };
            self.emit_lines(on_line);
            if finished {
                // A body that does not end in a newline still has a last line — the model
                // list is one JSON object with nothing after it.
                self.flush_rest(on_line);
                return Ok(());
            }
            if !self.fill()? {
                // The connection closed: whatever is left is the last line, terminator or not.
                self.flush_rest(on_line);
                return Ok(());
            }
        }
    }

    /// One read into `raw`. `Ok(false)` means the server closed the connection.
    fn fill(&mut self) -> Result<bool, HttpError> {
        let mut chunk = [0u8; 8192];
        loop {
            if self.cancel.load(Ordering::Acquire) {
                return Err(HttpError::Cancelled);
            }
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(false),
                Ok(read) => {
                    if self.raw.len() + read > MAX_BODY {
                        return Err(HttpError::Malformed("reply too large"));
                    }
                    self.raw.extend_from_slice(&chunk[..read]);
                    self.last_progress = Instant::now();
                    return Ok(true);
                }
                // A read timeout is how often the cancel flag gets looked at, not a failure.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if self.last_progress.elapsed() > IDLE_TIMEOUT {
                        return Err(HttpError::Io(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "the server stopped sending",
                        )));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(HttpError::Io(e)),
            }
        }
    }

    /// Consumes the status line and headers. Returns the status code and whether the body is
    /// chunked, which Ollama's streaming replies always are.
    fn read_headers(&mut self) -> Result<(u16, bool), HttpError> {
        let end = loop {
            if let Some(at) = find(&self.raw, b"\r\n\r\n") {
                break at;
            }
            if !self.fill()? {
                return Err(HttpError::Malformed("no header block"));
            }
        };
        let head = String::from_utf8_lossy(&self.raw[..end]).into_owned();
        self.raw.drain(..end + 4);

        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or(HttpError::Malformed("no status line"))?;
        let chunked = lines.any(|line| {
            let (name, value) = line.split_once(':').unwrap_or((line, ""));
            name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
        });
        Ok((status, chunked))
    }

    /// Moves every complete chunk from `raw` into `body`. `Ok(true)` at the terminating
    /// zero-length chunk. A partial chunk is left in place for the next read.
    fn drain_chunks(&mut self) -> Result<bool, HttpError> {
        loop {
            let Some(header_end) = find(&self.raw, b"\r\n") else {
                return Ok(false);
            };
            let header = String::from_utf8_lossy(&self.raw[..header_end]).into_owned();
            // `size[;extension]`, in hex.
            let size = usize::from_str_radix(header.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| HttpError::Malformed("bad chunk size"))?;
            if size == 0 {
                return Ok(true);
            }
            // The chunk, plus the CRLF that follows it.
            if self.raw.len() < header_end + 2 + size + 2 {
                return Ok(false);
            }
            let start = header_end + 2;
            self.body.extend_from_slice(&self.raw[start..start + size]);
            self.raw.drain(..start + size + 2);
        }
    }

    /// Hands over whatever is left as one final line, if it is not just whitespace.
    fn flush_rest(&mut self, on_line: &mut impl FnMut(&str)) {
        let rest = String::from_utf8_lossy(&std::mem::take(&mut self.body)).into_owned();
        if !rest.trim().is_empty() {
            on_line(rest.trim());
        }
    }

    /// Hands over every complete line in `body`, keeping any partial one.
    fn emit_lines(&mut self, on_line: &mut impl FnMut(&str)) {
        while let Some(at) = self.body.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.body.drain(..=at).collect();
            let text = String::from_utf8_lossy(&line);
            let text = text.trim();
            if !text.is_empty() {
                on_line(text);
            }
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::Arc;

    /// A one-request server that replies with `reply` and reports the request it was sent.
    fn serve(reply: Vec<u8>) -> (String, std::sync::mpsc::Receiver<String>) {
        serve_with(move |mut stream| {
            let _ = stream.write_all(&reply);
        })
    }

    fn serve_with(
        respond: impl FnOnce(TcpStream) + Send + 'static,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let host = listener.local_addr().expect("an address").to_string();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("one connection");
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("a clone"));
            let mut request = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                request.push_str(&line);
            }
            let length: usize = request
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .and_then(|n| n.trim().parse().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            request.push_str(&String::from_utf8_lossy(&body));
            let _ = sender.send(request);
            respond(stream);
        });
        (host, receiver)
    }

    fn chunked(pieces: &[&str]) -> Vec<u8> {
        let mut out =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\n\r\n"
                .to_vec();
        for piece in pieces {
            out.extend_from_slice(format!("{:x}\r\n{piece}\r\n", piece.len()).as_bytes());
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    fn collect(host: &str, cancel: &AtomicBool) -> (Result<(), HttpError>, Vec<String>) {
        let mut lines = Vec::new();
        let result = post_ndjson(host, "/api/chat", "{}", cancel, |line| {
            lines.push(line.to_owned())
        });
        (result, lines)
    }

    #[test]
    fn chunked_lines_arrive_whole_however_they_are_split() {
        // Chunk boundaries deliberately fall inside lines: a real stream splits wherever it
        // likes, and Ollama's tokens do not line up with its NDJSON lines.
        let (host, _requests) = serve(chunked(&[
            "{\"a\":1}\n{\"b\"",
            ":2}\n",
            "{\"c\":3}\n{\"d\":4}\n",
        ]));
        let (result, lines) = collect(&host, &AtomicBool::new(false));
        assert!(result.is_ok(), "{:?}", result.err());
        assert_eq!(lines, ["{\"a\":1}", "{\"b\":2}", "{\"c\":3}", "{\"d\":4}"]);
    }

    #[test]
    fn an_unterminated_last_line_is_still_delivered() {
        let mut reply = chunked(&["{\"a\":1}\n{\"partial\":true}"]);
        reply.truncate(reply.len() - 5); // no terminating chunk, connection just closes
        let (host, _requests) = serve(reply);
        let (result, lines) = collect(&host, &AtomicBool::new(false));
        assert!(result.is_ok(), "{:?}", result.err());
        assert_eq!(lines, ["{\"a\":1}", "{\"partial\":true}"]);
    }

    #[test]
    fn a_body_that_does_not_end_in_a_newline_still_arrives() {
        // Ollama's streams always end with one; its model list does not.
        let (host, _requests) = serve(chunked(&[r#"{"models":[{"name":"a"}]}"#]));
        let (result, lines) = collect(&host, &AtomicBool::new(false));
        assert!(result.is_ok(), "{:?}", result.err());
        assert_eq!(lines, [r#"{"models":[{"name":"a"}]}"#]);
    }

    #[test]
    fn a_plain_body_works_too() {
        let reply =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\n\r\n{\"a\":1}\n{\"b\":2}\n"
                .to_vec();
        let (host, _requests) = serve(reply);
        let (result, lines) = collect(&host, &AtomicBool::new(false));
        assert!(result.is_ok(), "{:?}", result.err());
        assert_eq!(lines, ["{\"a\":1}", "{\"b\":2}"]);
    }

    #[test]
    fn the_request_is_a_well_formed_post() {
        let (host, requests) = serve(chunked(&["{}\n"]));
        let cancel = AtomicBool::new(false);
        let _ = post_ndjson(&host, "/api/chat", "{\"model\":\"m\"}", &cancel, |_| {});
        let request = requests.recv().expect("the server saw a request");
        assert!(
            request.starts_with("POST /api/chat HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains(&format!("Host: {host}\r\n")));
        assert!(request.contains("Content-Type: application/json\r\n"));
        assert!(request.contains("Content-Length: 13\r\n"), "{request}");
        assert!(request.ends_with("{\"model\":\"m\"}"));
    }

    #[test]
    fn a_refused_connection_says_so() {
        // Bound, then dropped: nothing is listening on that port any more.
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a loopback port")
            .port();
        let cancel = AtomicBool::new(false);
        let (result, _) = collect(&format!("127.0.0.1:{port}"), &cancel);
        assert!(matches!(result, Err(HttpError::Connect(_))), "{result:?}");
    }

    #[test]
    fn an_error_status_carries_the_servers_reason() {
        let reply = b"HTTP/1.1 404 Not Found\r\nContent-Length: 36\r\n\r\n{\"error\":\"model 'nope' not found\"}\r\n"
            .to_vec();
        let (host, _requests) = serve(reply);
        let (result, _) = collect(&host, &AtomicBool::new(false));
        match result {
            Err(HttpError::Status(404, reason)) => {
                assert!(reason.contains("not found"), "{reason}")
            }
            other => panic!("expected a 404, got {other:?}"),
        }
    }

    #[test]
    fn cancelling_stops_a_stream_that_is_still_open() {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let (host, _requests) = serve_with(move |mut stream| {
            let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
            let _ = stream.write_all(head);
            let _ = stream.write_all(b"8\r\n{\"a\":1}\n\r\n");
            let _ = stream.flush();
            // Then hold the connection open, as a model does while it is still generating.
            std::thread::sleep(Duration::from_secs(5));
        });
        let started = Instant::now();
        let mut seen = 0;
        let result = post_ndjson(&host, "/api/chat", "{}", &cancel, |_| {
            seen += 1;
            flag.store(true, Ordering::Release);
        });
        assert!(matches!(result, Err(HttpError::Cancelled)), "{result:?}");
        assert_eq!(seen, 1, "the line before the cancel still arrived");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancelling waited for the server: {:?}",
            started.elapsed()
        );
    }
}
