//! Embeddings from a local Ollama model, as an alternative to the Apple helper.
//!
//! Deliberately shaped like [`super::Client`]: `probe` then `embed`, the same [`Failure`] values,
//! the same cancellation contract. Everything above this module treats the two as interchangeable,
//! and vectors never mix because [`model_key`](super::indexing::model_key) keys them by identifier,
//! revision and dimensions — an Ollama model's identifier is its own name, never `apple-contextual-en`.
//!
//! The transport is the agent's loopback HTTP client. `/api/embed` answers with one JSON object, so
//! it arrives as a single line of the NDJSON reader; `/api/embeddings` is the older endpoint with a
//! different request and reply shape and is not used.

use super::{Batch, Failure, Model};
use crate::agent::http::{self, HttpError};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

/// Ollama reports no revision, so vectors carry this one. Raising it retires every vector the
/// previous value wrote, which is what a change in how text reaches the model requires.
const REVISION: u32 = 1;

/// A batch large enough to keep the model busy and small enough that one failure costs little.
/// The plan's 32–64 range, taken at its lower end because a chunk is far larger than a document
/// title and the request body is bounded below.
pub const BATCH: usize = 32;

/// Per-text and per-request ceilings. A chunk is ~1,000 bytes by construction, so these bound a
/// malformed caller rather than ordinary work.
const MAX_TEXT: usize = 8_192;
const MAX_REQUEST: usize = 512 * 1024;

/// Dimensions outside this are refused by `model_key` anyway; checking here names the reason.
const MAX_DIMENSIONS: usize = 2_048;

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct Reply {
    embeddings: Option<Vec<Vec<f32>>>,
    error: Option<String>,
}

/// Speaks to one Ollama model on one loopback host.
pub struct Client {
    host: String,
    model: String,
    dimensions: Option<usize>,
}

impl Client {
    /// `host` is `host:port`; the transport refuses anything that does not resolve to loopback,
    /// so a misconfigured host fails as [`Failure::Unavailable`] rather than reaching the network.
    pub fn new(host: String, model: String) -> Self {
        Self {
            host,
            model,
            dimensions: None,
        }
    }

    pub fn close(&mut self) {
        self.dimensions = None;
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Embeds one short probe text to learn the model's width, since Ollama only reports
    /// dimensions by answering. The width is remembered, so later batches can reject a reply
    /// whose vectors changed shape underneath us.
    pub fn probe(&mut self, cancel: &AtomicBool) -> Result<Model, Failure> {
        let batch = self.request(std::slice::from_ref(&"blindspot".to_owned()), cancel)?;
        Ok(batch.model)
    }

    pub fn embed(&mut self, texts: &[String], cancel: &AtomicBool) -> Result<Batch, Failure> {
        if texts.is_empty()
            || texts.len() > BATCH
            || texts
                .iter()
                .any(|text| text.trim().is_empty() || text.len() > MAX_TEXT)
        {
            return Err(Failure::InvalidInput);
        }
        self.request(texts, cancel)
    }

    fn request(&mut self, texts: &[String], cancel: &AtomicBool) -> Result<Batch, Failure> {
        if cancel.load(Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        if self.model.trim().is_empty() || self.model.len() > 128 || !self.model.is_ascii() {
            return Err(Failure::ModelUnavailable);
        }
        let body = serde_json::to_string(&Request {
            model: &self.model,
            input: texts,
        })
        .map_err(|_| Failure::InvalidInput)?;
        if body.len() > MAX_REQUEST {
            return Err(Failure::InvalidInput);
        }

        // One JSON object, so one line; anything further is a server this client does not know.
        let mut line = String::new();
        let mut extra = false;
        http::post_ndjson(&self.host, "/api/embed", &body, cancel, |piece| {
            if line.is_empty() {
                line.push_str(piece);
            } else if !piece.trim().is_empty() {
                extra = true;
            }
        })
        .map_err(transport)?;
        if extra {
            return Err(Failure::InvalidResponse);
        }

        let reply: Reply = serde_json::from_str(&line).map_err(|_| Failure::InvalidResponse)?;
        if let Some(error) = reply.error.as_deref() {
            // Ollama reports both a missing model and a rejected request through this one field,
            // and the two need opposite handling: a missing model means fall back to the Apple
            // helper, while a request this server would not serve means retry it smaller. Only the
            // first is `ModelUnavailable`; everything else is reported as the batch's failure so
            // the caller's one-at-a-time retry engages.
            let missing = error.contains("not found") || error.contains("try pulling");
            return Err(if missing {
                Failure::ModelUnavailable
            } else {
                Failure::EmbeddingUnavailable
            });
        }
        let vectors = reply.embeddings.ok_or(Failure::InvalidResponse)?;
        if vectors.len() != texts.len() {
            return Err(Failure::InvalidResponse);
        }
        let dimensions = vectors
            .first()
            .map(Vec::len)
            .ok_or(Failure::InvalidResponse)?;
        if dimensions == 0 || dimensions > MAX_DIMENSIONS {
            return Err(Failure::InvalidResponse);
        }
        // A short or ragged reply would otherwise reach the shard builder, which assumes every
        // vector in a model's set has the same width.
        if vectors.iter().any(|vector| vector.len() != dimensions) {
            return Err(Failure::InvalidResponse);
        }
        if vectors.iter().flatten().any(|value| !value.is_finite()) {
            return Err(Failure::InvalidResponse);
        }
        if self.dimensions.is_some_and(|known| known != dimensions) {
            return Err(Failure::InvalidResponse);
        }
        self.dimensions = Some(dimensions);
        Ok(Batch {
            model: Model {
                identifier: self.model.clone(),
                revision: REVISION,
                dimensions,
            },
            vectors,
        })
    }
}

/// Ollama not running is [`Failure::Unavailable`], not an error worth showing: the Apple helper
/// takes over. A refusal to leave loopback arrives here as `Malformed` and means the same thing.
fn transport(error: HttpError) -> Failure {
    match error {
        HttpError::Cancelled => Failure::Cancelled,
        HttpError::Connect(_) | HttpError::Malformed(_) => Failure::Unavailable,
        HttpError::Io(error) if error.kind() == std::io::ErrorKind::TimedOut => Failure::TimedOut,
        HttpError::Io(_) => Failure::Unavailable,
        // 404 is how Ollama reports a model it does not have.
        HttpError::Status(404, _) => Failure::ModelUnavailable,
        HttpError::Status(_, _) => Failure::EmbeddingUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::indexing::model_key;
    use std::io::{BufRead, Write};
    use std::net::{TcpListener, TcpStream};

    /// A one-request server replying with `body` as JSON, reporting what it was sent.
    fn serve(status: &str, body: &str) -> (String, std::sync::mpsc::Receiver<String>) {
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        serve_with(move |mut stream| {
            let _ = stream.write_all(reply.as_bytes());
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
            let _ = std::io::Read::read_exact(&mut reader, &mut body);
            request.push_str(&String::from_utf8_lossy(&body));
            let _ = sender.send(request);
            respond(stream);
        });
        (host, receiver)
    }

    fn texts(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn a_batch_is_sent_as_one_request_and_comes_back_as_vectors() {
        let (host, requests) = serve("200 OK", r#"{"embeddings":[[1.0,0.0],[0.0,1.0]]}"#);
        let mut client = Client::new(host, "embeddinggemma:300m".into());
        let batch = client
            .embed(&texts(&["first", "second"]), &AtomicBool::new(false))
            .expect("vectors");
        assert_eq!(batch.vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert_eq!(batch.model.dimensions, 2);
        assert_eq!(batch.model.identifier, "embeddinggemma:300m");

        let request = requests.recv().expect("the request");
        assert!(
            request.starts_with("POST /api/embed HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(
            request.contains(r#""input":["first","second"]"#),
            "{request}"
        );
        // The two backends must never share a key, or a search would mix their vector spaces.
        assert_ne!(
            model_key(&batch.model).expect("a key"),
            model_key(&Model {
                identifier: crate::semantic::MODEL_IDENTIFIER.into(),
                revision: REVISION,
                dimensions: 2,
            })
            .expect("a key")
        );
    }

    #[test]
    fn a_reply_that_does_not_match_the_request_is_refused() {
        // Too few vectors, a ragged width, a non-finite value, no embeddings field, and an
        // error field: each would otherwise reach the shard builder as a valid-looking batch.
        for body in [
            r#"{"embeddings":[[1.0,0.0]]}"#,
            r#"{"embeddings":[[1.0,0.0],[0.0]]}"#,
            r#"{"embeddings":[[1.0,0.0],[0.0,null]]}"#,
            r#"{"model":"x"}"#,
            r#"{"error":"model not found"}"#,
        ] {
            let (host, _) = serve("200 OK", body);
            let mut client = Client::new(host, "m".into());
            let result = client.embed(&texts(&["first", "second"]), &AtomicBool::new(false));
            assert!(result.is_err(), "{body} was accepted");
        }
    }

    #[test]
    fn a_model_whose_width_changes_underneath_us_is_refused() {
        let (host, _) = serve("200 OK", r#"{"embeddings":[[1.0,0.0,0.0]]}"#);
        let mut client = Client::new(host, "m".into());
        let model = client.probe(&AtomicBool::new(false)).expect("a model");
        assert_eq!(model.dimensions, 3);

        let (host, _) = serve("200 OK", r#"{"embeddings":[[1.0,0.0]]}"#);
        client.host = host;
        assert_eq!(
            client.embed(&texts(&["again"]), &AtomicBool::new(false)),
            Err(Failure::InvalidResponse)
        );
    }

    #[test]
    fn a_missing_model_is_told_apart_from_a_server_that_is_not_running() {
        let (host, _) = serve("404 Not Found", r#"{"error":"model 'nope' not found"}"#);
        let mut client = Client::new(host, "nope".into());
        assert_eq!(
            client.embed(&texts(&["text"]), &AtomicBool::new(false)),
            Err(Failure::ModelUnavailable)
        );

        // A 200 carrying some other complaint is the batch's problem, not the model's: the caller
        // retries one text at a time rather than giving up on Ollama and falling back to Apple.
        let (host, _) = serve("200 OK", r#"{"error":"input is too large for this model"}"#);
        let mut client = Client::new(host, "m".into());
        assert_eq!(
            client.embed(&texts(&["text"]), &AtomicBool::new(false)),
            Err(Failure::EmbeddingUnavailable)
        );

        // Nothing listening: the Apple helper should take over rather than the user seeing a failure.
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("a loopback port")
            .local_addr()
            .expect("an address")
            .port();
        let mut client = Client::new(format!("127.0.0.1:{port}"), "m".into());
        assert_eq!(
            client.embed(&texts(&["text"]), &AtomicBool::new(false)),
            Err(Failure::Unavailable)
        );
    }

    #[test]
    fn nothing_leaves_loopback_and_a_cancelled_caller_sends_nothing() {
        let mut client = Client::new("example.com:11434".into(), "m".into());
        assert_eq!(
            client.embed(&texts(&["text"]), &AtomicBool::new(false)),
            Err(Failure::Unavailable)
        );

        let (host, requests) = serve("200 OK", r#"{"embeddings":[[1.0]]}"#);
        let mut client = Client::new(host, "m".into());
        assert_eq!(
            client.embed(&texts(&["text"]), &AtomicBool::new(true)),
            Err(Failure::Cancelled)
        );
        assert!(
            requests
                .recv_timeout(std::time::Duration::from_millis(250))
                .is_err(),
            "a cancelled request still reached the server"
        );
    }

    #[test]
    fn malformed_callers_are_refused_before_any_connection() {
        // No server: reaching one would hang rather than return, so these must fail locally.
        let mut client = Client::new("127.0.0.1:1".into(), "m".into());
        let cancel = AtomicBool::new(false);
        assert_eq!(client.embed(&[], &cancel), Err(Failure::InvalidInput));
        assert_eq!(
            client.embed(&texts(&["   "]), &cancel),
            Err(Failure::InvalidInput)
        );
        assert_eq!(
            client.embed(&["x".repeat(MAX_TEXT + 1)], &cancel),
            Err(Failure::InvalidInput)
        );
        assert_eq!(
            client.embed(&vec!["text".to_owned(); BATCH + 1], &cancel),
            Err(Failure::InvalidInput)
        );

        let mut client = Client::new("127.0.0.1:1".into(), "  ".into());
        assert_eq!(
            client.embed(&texts(&["text"]), &cancel),
            Err(Failure::ModelUnavailable)
        );
    }
}
