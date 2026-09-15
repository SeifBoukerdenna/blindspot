//! Worker-owned local embedding transport. All calls, including teardown, belong off the UI thread.

pub mod cache;
pub mod indexing;
pub mod search;
pub mod vectors;

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The embedding model the helper reports; vectors of any other model are retired.
pub const MODEL_IDENTIFIER: &str = "apple-contextual-en";
const MAX_RESPONSE: usize = 262_144;
const IO_POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Cancelled,
    TimedOut,
    Unavailable,
    InvalidInput,
    InvalidResponse,
    ModelUnavailable,
    EmbeddingUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub identifier: String,
    pub revision: u32,
    pub dimensions: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub model: Model,
    pub vectors: Vec<Vec<f32>>,
}

#[derive(Serialize)]
struct Request<'a> {
    version: u8,
    id: u64,
    operation: &'static str,
    texts: &'a [String],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum WorkerError {
    InvalidRequest,
    UnsupportedProtocol,
    InputTooLarge,
    TruncatedFrame,
    ModelUnavailable,
    EmbeddingUnavailable,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    version: u8,
    id: u64,
    model: Option<Model>,
    vectors: Option<Vec<Vec<f32>>>,
    error: Option<WorkerError>,
}

pub struct Client {
    path: PathBuf,
    connection: Option<Connection>,
    sequence: u64,
    startup_timeout: Duration,
    request_timeout: Duration,
}

impl Client {
    pub fn new(path: PathBuf) -> Self {
        Self { path, connection: None, sequence: 0,
            startup_timeout: Duration::from_secs(5), request_timeout: Duration::from_secs(2) }
    }

    pub fn close(&mut self) { self.connection = None; }

    pub fn probe(&mut self, cancel: &AtomicBool) -> Result<Model, Failure> {
        self.request("probe", &[], cancel).map(|batch|batch.model)
    }

    pub fn embed(&mut self, texts: &[String], cancel: &AtomicBool) -> Result<Batch, Failure> {
        if texts.is_empty() || texts.len()>8 || texts.iter().any(|text|text.trim().is_empty() || text.len()>4096)
            || texts.iter().map(String::len).sum::<usize>()>16_384 {
            return Err(Failure::InvalidInput);
        }
        self.request("embed", texts, cancel)
    }

    fn request(&mut self, operation: &'static str, texts: &[String], cancel: &AtomicBool) -> Result<Batch, Failure> {
        if cancel.load(Ordering::Acquire) { return Err(Failure::Cancelled); }
        self.sequence = self.sequence.checked_add(1).ok_or(Failure::Unavailable)?;
        let request = Request { version: 1, id: self.sequence, operation, texts };
        let mut bytes = serde_json::to_vec(&request).map_err(|_|Failure::InvalidInput)?;
        if bytes.len()>65_536 { return Err(Failure::InvalidInput); }
        bytes.push(b'\n');
        let deadline = Instant::now() + if self.connection.is_some() {self.request_timeout} else {self.startup_timeout};
        let result = (|| {
            if self.connection.is_none() { self.connection = Some(Connection::spawn(&self.path)?); }
            let connection = self.connection.as_mut().ok_or(Failure::Unavailable)?;
            let response: Response = connection.exchange(&bytes, deadline, cancel)?;
            if response.version!=1 || response.id!=request.id { return Err(Failure::InvalidResponse); }
            if let Some(error) = response.error {
                if response.model.is_some() || response.vectors.is_some() { return Err(Failure::InvalidResponse); }
                return Err(match error {
                    WorkerError::ModelUnavailable => Failure::ModelUnavailable,
                    WorkerError::EmbeddingUnavailable => Failure::EmbeddingUnavailable,
                    _ => Failure::InvalidResponse,
                });
            }
            let model = response.model.ok_or(Failure::InvalidResponse)?;
            if model.identifier.is_empty() || model.identifier.len()>128 || !model.identifier.is_ascii()
                || model.identifier.chars().any(char::is_control) || model.revision==0 || !(1..=2048).contains(&model.dimensions) {
                return Err(Failure::InvalidResponse);
            }
            let vectors = match (operation,response.vectors) {
                ("probe",None) => Vec::new(),
                ("embed",Some(vectors)) if vectors.len()==texts.len() => vectors,
                _ => return Err(Failure::InvalidResponse),
            };
            for vector in &vectors {
                let norm = vector.iter().map(|value|f64::from(*value).powi(2)).sum::<f64>();
                if vector.len()!=model.dimensions || vector.iter().any(|value|!value.is_finite()) || (norm-1.0).abs()>0.001 {
                    return Err(Failure::InvalidResponse);
                }
            }
            Ok(Batch {model,vectors})
        })();
        if matches!(result, Err(Failure::Cancelled | Failure::TimedOut | Failure::Unavailable | Failure::InvalidResponse)) {
            self.close();
        }
        result
    }
}

struct Connection {
    socket: UnixStream,
    child: Child,
}

impl Connection {
    fn spawn(path: &Path) -> Result<Self, Failure> {
        Self::spawn_in(path,None)
    }

    fn spawn_in(path: &Path, directory: Option<&Path>) -> Result<Self, Failure> {
        if !path.is_absolute() { return Err(Failure::Unavailable); }
        let (socket, peer) = UnixStream::pair().map_err(|_|Failure::Unavailable)?;
        socket.set_read_timeout(Some(IO_POLL)).map_err(|_|Failure::Unavailable)?;
        socket.set_write_timeout(Some(IO_POLL)).map_err(|_|Failure::Unavailable)?;
        let input = peer.try_clone().map_err(|_|Failure::Unavailable)?;
        let mut command = Command::new(path);
        if let Some(directory) = directory {
            if !directory.is_absolute() { return Err(Failure::Unavailable); }
            command.current_dir(directory);
        }
        let child = command.stdin(Stdio::from(OwnedFd::from(input)))
            .stdout(Stdio::from(OwnedFd::from(peer)))
            .stderr(Stdio::null()).spawn().map_err(|_|Failure::Unavailable)?;
        Ok(Self { socket, child })
    }

    fn exchange<T: serde::de::DeserializeOwned>(&mut self, request: &[u8], deadline: Instant, cancel: &AtomicBool) -> Result<T, Failure> {
        let mut sent = 0;
        while sent<request.len() {
            check(deadline,cancel)?;
            match self.socket.write(&request[sent..]) {
                Ok(0) => return Err(Failure::Unavailable),
                Ok(count) => sent+=count,
                Err(error) if retryable(&error) => {},
                Err(_) => return Err(Failure::Unavailable),
            }
        }
        let mut response = Vec::new();
        let mut buffer = [0u8;4096];
        loop {
            check(deadline,cancel)?;
            match self.socket.read(&mut buffer) {
                Ok(0) => return Err(Failure::Unavailable),
                Ok(count) => {
                    response.extend_from_slice(&buffer[..count]);
                    if response.len()>MAX_RESPONSE+1 { return Err(Failure::InvalidResponse); }
                    if let Some(end) = response.iter().position(|byte|*byte==b'\n') {
                        if end+1!=response.len() { return Err(Failure::InvalidResponse); }
                        check(deadline,cancel)?;
                        return serde_json::from_slice(&response[..end]).map_err(|_|Failure::InvalidResponse);
                    }
                }
                Err(error) if retryable(&error) => {},
                Err(_) => return Err(Failure::Unavailable),
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn check(deadline: Instant, cancel: &AtomicBool) -> Result<(), Failure> {
    if cancel.load(Ordering::Acquire) { return Err(Failure::Cancelled); }
    if Instant::now()>=deadline { return Err(Failure::TimedOut); }
    Ok(())
}

fn retryable(error: &std::io::Error) -> bool {
    matches!(error.kind(),std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, atomic::AtomicU64};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(behavior: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!("blindspot-semantic-client-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir(&directory).unwrap();
            let script = format!(r#"#!/usr/bin/python3
import json, sys, time
for line in sys.stdin:
    request = json.loads(line)
    response = {{"version":1,"id":request["id"],"model":{{"identifier":"fixture","revision":1,"dimensions":2}}}}
    if request["operation"] == "embed":
        response["vectors"] = [[1.0,0.0] for _ in request["texts"]]
    {behavior}
    print(json.dumps(response), flush=True)
"#);
            let path = directory.join("worker");
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(directory)
        }
        fn client(&self) -> Client { Client::new(self.0.join("worker")) }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { std::fs::remove_dir_all(&self.0).unwrap(); }
    }

    #[test]
    fn persistent_worker_validates_and_reuses_connection() {
        let fixture = Fixture::new("pass");
        let mut client = fixture.client();
        let cancel = AtomicBool::new(false);
        assert_eq!(client.probe(&cancel).unwrap().dimensions, 2);
        let pid = client.connection.as_ref().unwrap().child.id();
        assert_eq!(client.embed(&["synthetic sentence".into()], &cancel).unwrap().vectors, vec![vec![1.0, 0.0]]);
        assert_eq!(client.connection.as_ref().unwrap().child.id(), pid);
        client.close();
        assert!(client.connection.is_none());
        assert!(client.probe(&cancel).is_ok());
        assert_ne!(client.connection.as_ref().unwrap().child.id(), pid);
    }

    #[test]
    fn rejects_malformed_responses_and_drops_worker() {
        for behavior in [
            "response['id'] += 1",
            "response['extra'] = 'unexpected'",
            "response['vectors'] = [[0.0,0.0]]",
            "response['vectors'] = [[1.0]]",
            "response['model']['dimensions'] = 4096",
            "response['model']['revision'] = 0",
            "response['error'] = 'modelUnavailable'",
            "print('x' * 262145, flush=True); continue",
        ] {
            let fixture = Fixture::new(behavior);
            let mut client = fixture.client();
            assert_eq!(client.embed(&["fixture".into()], &AtomicBool::new(false)), Err(Failure::InvalidResponse), "{behavior}");
            assert!(client.connection.is_none());
        }
    }

    #[test]
    fn invalid_input_never_launches_worker() {
        let mut client = Client::new(PathBuf::from("/nonexistent/blindspot-worker"));
        for texts in [vec![], vec![" ".into()], vec!["x".repeat(4097)], vec!["x".into();9], vec!["x".repeat(4096);5]] {
            assert_eq!(client.embed(&texts, &AtomicBool::new(false)), Err(Failure::InvalidInput));
            assert!(client.connection.is_none());
        }
    }

    #[test]
    fn cancellation_interrupts_inference_and_allows_restart() {
        let fixture = Fixture::new("if request['operation'] == 'embed': time.sleep(5)");
        let mut client = fixture.client();
        let cancel = Arc::new(AtomicBool::new(false));
        client.probe(&cancel).unwrap();
        let trigger = Arc::clone(&cancel);
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            trigger.store(true, Ordering::Release);
        });
        let started = Instant::now();
        assert_eq!(client.embed(&["fixture".into()], &cancel), Err(Failure::Cancelled));
        assert!(started.elapsed()<Duration::from_millis(500));
        assert!(client.connection.is_none());
        thread.join().unwrap();
        cancel.store(false, Ordering::Release);
        assert!(client.probe(&cancel).is_ok());
    }

    #[test]
    fn inference_deadline_terminates_worker() {
        let fixture = Fixture::new("if request['operation'] == 'embed': time.sleep(5)");
        let mut client = fixture.client();
        let cancel = AtomicBool::new(false);
        client.probe(&cancel).unwrap();
        client.request_timeout = Duration::from_millis(60);
        let started = Instant::now();
        assert_eq!(client.embed(&["fixture".into()], &cancel), Err(Failure::TimedOut));
        assert!(started.elapsed()<Duration::from_millis(500));
        assert!(client.connection.is_none());
    }

    #[test]
    fn missing_model_is_a_recoverable_response() {
        let fixture = Fixture::new("response = {'version':1,'id':request['id'],'error':'modelUnavailable'}");
        let mut client = fixture.client();
        assert_eq!(client.probe(&AtomicBool::new(false)), Err(Failure::ModelUnavailable));
        assert!(client.connection.is_some());
    }

    fn indexed_fixture(fixture: &Fixture, count: usize) -> crate::content::ContentStore {
        let mut store = crate::content::ContentStore::open(&fixture.0.join("content.sqlite")).unwrap();
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let paths: Vec<_> = (0..count).map(|i|format!("/fixture/{i}.txt")).collect();
        let documents: Vec<_> = paths.iter().map(|path|crate::content::Document {
            identity:path, path:Path::new(path), title:path.rsplit('/').next().unwrap(),
            body:"fixture document about database transactions",modified_ns:1,changed_ns:1,bytes:44,
            extraction: crate::content::Extraction::Text,
        }).collect();
        store.put_batch(&scan,&documents).unwrap();
        store
    }

    #[test]
    fn indexing_resumes_committed_batches_after_cancellation_and_reopen() {
        let fixture = Fixture::new("pass");
        let mut store = indexed_fixture(&fixture,9);
        let mut client = fixture.client();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = indexing::run(&mut store,&mut client,Arc::clone(&cancel), |_|true, |progress| {
            if progress.written>=4 { cancel.store(true,Ordering::Release); }
        });
        assert!(matches!(result,Err(indexing::Error::Cancelled)));
        drop(store);
        client.close();
        let mut store = crate::content::ContentStore::open(&fixture.0.join("content.sqlite")).unwrap();
        cancel.store(false,Ordering::Release);
        let resumed = indexing::run(&mut store,&mut client,Arc::clone(&cancel), |_|true, |_|{}).unwrap();
        assert_eq!(resumed.current,4);
        assert_eq!(resumed.written,5);
        let unchanged = indexing::run(&mut store,&mut client,cancel, |_|true, |_|{}).unwrap();
        assert_eq!(unchanged.current,9);
        assert_eq!(unchanged.written,0);
    }

    #[test]
    fn indexing_rejects_mid_pass_model_changes_without_harming_lexical_search() {
        let fixture = Fixture::new("if request['operation'] == 'embed': response['model']['revision'] += 1");
        let mut store = indexed_fixture(&fixture,3);
        let result = indexing::run(&mut store,&mut fixture.client(),Arc::new(AtomicBool::new(false)), |_|true, |_|{});
        assert!(matches!(result,Err(indexing::Error::Worker(Failure::InvalidResponse))));
        let key = indexing::model_key(&Model {identifier:"fixture".into(),revision:1,dimensions:2}).unwrap();
        assert_eq!(store.embedding_page(0,&key,2,Arc::new(AtomicBool::new(false))).unwrap().pending.len(),3);
        assert_eq!(store.search("transactions",10,Arc::new(AtomicBool::new(false))).unwrap().hits.len(),3);
    }

    #[test]
    fn indexing_isolates_unembeddable_text_and_respects_scope_filter() {
        let fixture = Fixture::new("if any('1.txt' in text for text in request['texts']): response = {'version':1,'id':request['id'],'error':'embeddingUnavailable'}");
        let mut store = indexed_fixture(&fixture,3);
        let progress = indexing::run(&mut store,&mut fixture.client(),Arc::new(AtomicBool::new(false)),
            |path| !path.ends_with("2.txt"), |_|{}).unwrap();
        assert_eq!(progress.written,1);
        assert_eq!(progress.failed,1);
        assert_eq!(progress.excluded,1);
        assert_eq!(store.search("transactions",10,Arc::new(AtomicBool::new(false))).unwrap().hits.len(),3);
    }

    #[test]
    fn indexing_splits_escaped_payloads_and_reports_blank_text() {
        let fixture = Fixture::new("pass");
        let mut store = indexed_fixture(&fixture,4);
        let scan = store.begin_scan(Path::new("/fixture")).unwrap();
        let paths: Vec<_> = (0..4).map(|i|format!("/fixture/{i}.txt")).collect();
        let body = "\u{1}".repeat(4096);
        let documents: Vec<_> = paths.iter().map(|path|crate::content::Document {
            identity:path,path:Path::new(path),title:"",body:&body,modified_ns:2,changed_ns:2,bytes:4096,
            extraction: crate::content::Extraction::Text,
        }).collect();
        store.put_batch(&scan,&documents).unwrap();
        let progress = indexing::run(&mut store,&mut fixture.client(),Arc::new(AtomicBool::new(false)), |_|true, |_|{}).unwrap();
        assert_eq!(progress.written,4);
        assert_eq!(progress.failed,0);
        store.put_batch(&scan,&[crate::content::Document {
            identity:"blank",path:Path::new("/fixture/blank.txt"),title:"",body:"",modified_ns:1,changed_ns:1,bytes:0,
            extraction: crate::content::Extraction::Text,
        }]).unwrap();
        let progress = indexing::run(&mut store,&mut fixture.client(),Arc::new(AtomicBool::new(false)), |_|true, |_|{}).unwrap();
        assert_eq!(progress.current,4);
        assert_eq!(progress.failed,1);
        assert_eq!(progress.written,0);
    }

    #[test]
    #[ignore = "requires a built native helper and the installed Apple English sentence model"]
    fn native_content_embedding_pass_persists_and_reuses_vectors() {
        let fixture = Fixture::new("pass");
        let mut store = indexed_fixture(&fixture,3);
        let mut client = Client::new(PathBuf::from(std::env::var_os("BLINDSPOT_SEMANTIC_WORKER").expect("helper path")));
        let first = indexing::run(&mut store,&mut client,Arc::new(AtomicBool::new(false)), |_|true, |_|{}).unwrap();
        assert_eq!(first.written,3);
        let next = indexing::run(&mut store,&mut client,Arc::new(AtomicBool::new(false)), |_|true, |_|{}).unwrap();
        assert_eq!(next.written,0);
        assert_eq!(next.current,3);
        store.check_integrity().unwrap();
    }

    #[test]
    #[ignore = "requires a built native helper and the installed Apple English sentence model"]
    fn native_helper_embeds_through_socket_transport() {
        let path = std::env::var_os("BLINDSPOT_SEMANTIC_WORKER").expect("helper path");
        let mut client = Client::new(PathBuf::from(path));
        let cancel = AtomicBool::new(false);
        let model = client.probe(&cancel).unwrap();
        assert_eq!(model.identifier, "apple-contextual-en");
        let result = client.embed(&["A document about relational databases.".into(), "Notes on SQL database tables.".into()], &cancel).unwrap();
        assert_eq!(result.model, model);
        assert_eq!(result.vectors.len(), 2);
    }
}
