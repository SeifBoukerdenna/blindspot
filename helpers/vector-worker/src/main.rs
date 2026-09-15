use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, fs::{File, OpenOptions}, io::{BufRead, Read, Seek, Write},
    os::{fd::AsRawFd, unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt}}, path::{Path, PathBuf}};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

const MAX_FRAME: usize = 2 * 1024 * 1024;
const MAX_VECTORS: usize = 65_536;
const MAX_SHARDS: usize = 256;
const MAX_FILE: u64 = 384 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request { version: u8, id: u64, command: Command }

#[derive(Deserialize)]
#[serde(tag="operation", rename_all="camelCase", deny_unknown_fields)]
enum Command {
    Begin { token: String, dimensions: usize, count: usize },
    Add { entries: Vec<Entry> },
    Finish {},
    Reset {},
    Query { dimensions: usize, vector: Vec<f32>, shards: Vec<Shard>, limit: usize },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry { key: u64, values: Vec<f32> }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Shard { token: String, checksum: String }

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all="camelCase")]
enum Failure { InvalidRequest, Unavailable, Busy }

#[derive(Serialize)]
struct Response {
    version: u8,
    id: u64,
    #[serde(skip_serializing_if="Option::is_none")]
    result: Option<Output>,
    #[serde(skip_serializing_if="Option::is_none")]
    error: Option<Failure>,
}

#[derive(Serialize)]
#[serde(tag="kind", rename_all="camelCase")]
enum Output {
    Accepted { count: usize },
    Built { token: String, count: usize, bytes: u64, checksum: String },
    Found { hits: Vec<Hit>, unavailable: usize },
}

#[derive(Serialize)]
struct Hit { key: u64, distance: f32 }

struct Builder { index: Index, token: String, dimensions: usize, expected: usize, last: u64 }

#[derive(PartialEq, Eq)]
struct Fingerprint { device: u64, inode: u64, bytes: u64, modified: (i64,i64), changed: (i64,i64) }
impl Fingerprint {
    fn read(file: &File) -> Result<Self, Failure> {
        let metadata = file.metadata().map_err(|_|Failure::Unavailable)?;
        if !metadata.is_file() || metadata.len()>MAX_FILE || metadata.len()<64 || metadata.nlink()!=1 {
            return Err(Failure::Unavailable);
        }
        Ok(Self {device:metadata.dev(),inode:metadata.ino(),bytes:metadata.len(),
            modified:(metadata.mtime(),metadata.mtime_nsec()),changed:(metadata.ctime(),metadata.ctime_nsec())})
    }
}

struct Worker {
    directory: PathBuf,
    builder: Option<Builder>,
    verified: HashMap<String,(Fingerprint,String)>,
}

impl Worker {
    fn respond(&mut self, command: Command) -> Result<Output, Failure> {
        match command {
            Command::Begin {token,dimensions,count} => {
                if self.builder.is_some() { return Err(Failure::Busy); }
                if !valid_token(&token) || !(1..=2048).contains(&dimensions) || !(1..=MAX_VECTORS).contains(&count) {
                    return Err(Failure::InvalidRequest);
                }
                let index = Index::new(&options(dimensions)).map_err(|_|Failure::Unavailable)?;
                index.reserve_capacity_and_threads(count,1).map_err(|_|Failure::Unavailable)?;
                self.builder = Some(Builder {index,token,dimensions,expected:count,last:0});
                Ok(Output::Accepted {count:0})
            }
            Command::Add {entries} => {
                let builder = self.builder.as_mut().ok_or(Failure::InvalidRequest)?;
                if entries.is_empty() || entries.len()>64 || builder.index.size()+entries.len()>builder.expected {
                    return Err(Failure::InvalidRequest);
                }
                let mut last = builder.last;
                for entry in &entries {
                    if entry.key<=last || entry.key>i64::MAX as u64 || !valid_vector(&entry.values,builder.dimensions) {
                        return Err(Failure::InvalidRequest);
                    }
                    last = entry.key;
                }
                for entry in entries { builder.index.add(entry.key,&entry.values).map_err(|_|Failure::Unavailable)?; }
                builder.last = last;
                Ok(Output::Accepted {count:builder.index.size()})
            }
            Command::Finish {} => {
                let builder = self.builder.take().ok_or(Failure::InvalidRequest)?;
                if builder.index.size()!=builder.expected { return Err(Failure::InvalidRequest); }
                let path = self.directory.join(format!("{}.ann",builder.token));
                let mut file = OpenOptions::new().read(true).write(true).create_new(true).mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW).open(&path).map_err(|_|Failure::Unavailable)?;
                // macOS /dev/fd duplicates the already anchored descriptor (fd(4)).
                builder.index.save(&descriptor_path(&file)).map_err(|_|Failure::Unavailable)?;
                file.sync_all().map_err(|_|Failure::Unavailable)?;
                let fingerprint = Fingerprint::read(&file)?;
                let checksum = checksum(&mut file)?;
                Ok(Output::Built {token:builder.token,count:builder.expected,bytes:fingerprint.bytes,checksum})
            }
            Command::Reset {} => { self.builder=None; Ok(Output::Accepted {count:0}) }
            Command::Query {dimensions,vector,shards,limit} => {
                if self.builder.is_some() { return Err(Failure::Busy); }
                if !valid_vector(&vector,dimensions) || shards.len()>MAX_SHARDS || !(1..=100).contains(&limit)
                    || shards.iter().any(|shard|!valid_token(&shard.token) || !valid_checksum(&shard.checksum)) {
                    return Err(Failure::InvalidRequest);
                }
                let mut hits = Vec::new();
                let mut unavailable = 0;
                let mut seen = std::collections::HashSet::new();
                for shard in shards {
                    if !seen.insert(shard.token.clone()) { continue; }
                    match self.search(&shard,dimensions,&vector,limit) {
                        Ok(next) => {
                            hits.extend(next);
                            hits.sort_by(|a:&Hit,b:&Hit|a.distance.total_cmp(&b.distance).then(a.key.cmp(&b.key)));
                            let mut keys = std::collections::HashSet::new();
                            hits.retain(|hit|keys.insert(hit.key));
                            hits.truncate(limit);
                        }
                        Err(_) => unavailable += 1,
                    }
                }
                Ok(Output::Found {hits,unavailable})
            }
        }
    }

    fn search(&mut self, shard: &Shard, dimensions: usize, vector: &[f32], limit: usize) -> Result<Vec<Hit>, Failure> {
        let path = self.directory.join(format!("{}.ann",shard.token));
        let mut file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path).map_err(|_|Failure::Unavailable)?;
        let fingerprint = Fingerprint::read(&file)?;
        if !self.verified.get(&shard.token).is_some_and(|(old,hash)|old==&fingerprint && hash==&shard.checksum) {
            if checksum(&mut file)?!=shard.checksum { return Err(Failure::Unavailable); }
            if self.verified.len()>=MAX_SHARDS { self.verified.clear(); }
            self.verified.insert(shard.token.clone(),(fingerprint,shard.checksum.clone()));
        }
        file.rewind().map_err(|_|Failure::Unavailable)?;
        let descriptor = descriptor_path(&file);
        let metadata = Index::metadata(&descriptor).map_err(|_|Failure::Unavailable)?;
        if metadata.dimensions!=dimensions as u64 || metadata.count_present>MAX_VECTORS as u64
            || metadata.count_deleted!=0 || metadata.multi || metadata.quantization!=ScalarKind::F16 || metadata.metric!=MetricKind::Cos {
            return Err(Failure::Unavailable);
        }
        let index = Index::new(&options(dimensions)).map_err(|_|Failure::Unavailable)?;
        index.view(&descriptor).map_err(|_|Failure::Unavailable)?;
        if index.size()>MAX_VECTORS || index.dimensions()!=dimensions { return Err(Failure::Unavailable); }
        let found = index.search(vector,limit).map_err(|_|Failure::Unavailable)?;
        if found.keys.len()!=found.distances.len() || found.keys.len()>limit { return Err(Failure::Unavailable); }
        let mut hits = Vec::new();
        for (key,distance) in found.keys.into_iter().zip(found.distances) {
            if key==0 || key>i64::MAX as u64 || !distance.is_finite() || !(-0.001..=2.001).contains(&distance) {
                return Err(Failure::Unavailable);
            }
            hits.push(Hit {key,distance:distance.clamp(0.0,2.0)});
        }
        Ok(hits)
    }
}

fn options(dimensions: usize) -> IndexOptions {
    IndexOptions {dimensions,metric:MetricKind::Cos,quantization:ScalarKind::F16,connectivity:16,
        expansion_add:128,expansion_search:128,..IndexOptions::default()}
}
fn valid_token(token: &str) -> bool { token.len()==32 && token.bytes().all(|byte|byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) }
fn valid_checksum(hash: &str) -> bool { hash.len()==64 && hash.bytes().all(|byte|byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) }
fn valid_vector(values: &[f32], dimensions: usize) -> bool {
    (1..=2048).contains(&dimensions) && values.len()==dimensions && values.iter().all(|value|value.is_finite())
        && (values.iter().map(|value|f64::from(*value).powi(2)).sum::<f64>()-1.0).abs()<=0.001
}
fn descriptor_path(file: &File) -> String { format!("/dev/fd/{}",file.as_raw_fd()) }
fn checksum(file: &mut File) -> Result<String, Failure> {
    file.rewind().map_err(|_|Failure::Unavailable)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8;65_536];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|_|Failure::Unavailable)?;
        if read==0 { break; }
        total += read as u64;
        if total>MAX_FILE { return Err(Failure::Unavailable); }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}",hash.finalize()))
}

fn run(directory: &Path) -> Result<(), ()> {
    let metadata = std::fs::metadata(directory).map_err(|_|())?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 { return Err(()); }
    let mut worker = Worker {directory:directory.into(),builder:None,verified:HashMap::new()};
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut frame = Vec::new();
    loop {
        frame.clear();
        let count = input.by_ref().take((MAX_FRAME+1) as u64).read_until(b'\n',&mut frame).map_err(|_|())?;
        if count==0 { return Ok(()); }
        if frame.len()>MAX_FRAME || frame.last()!=Some(&b'\n') { return Err(()); }
        let request = serde_json::from_slice::<Request>(&frame);
        let (id,result) = match request {
            Ok(request) if request.version==1 && request.id>0 => {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||worker.respond(request.command)))
                    .unwrap_or(Err(Failure::Unavailable));
                (request.id,result)
            }
            _ => (0,Err(Failure::InvalidRequest)),
        };
        if result.is_err() { worker.builder=None; }
        let response = match result {
            Ok(result) => Response {version:1,id,result:Some(result),error:None},
            Err(error) => Response {version:1,id,result:None,error:Some(error)},
        };
        serde_json::to_writer(&mut output,&response).map_err(|_|())?;
        output.write_all(b"\n").map_err(|_|())?;
        output.flush().map_err(|_|())?;
    }
}

fn main() {
    std::panic::set_hook(Box::new(|_|{}));
    if std::env::args_os().len()!=1 || std::env::current_dir().ok().and_then(|directory|run(&directory).ok()).is_none() {
        eprintln!("Vector worker unavailable");
        std::process::exit(1);
    }
}
