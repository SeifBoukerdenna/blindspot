//! Typed transport for the isolated vector worker. Owned and called off the UI thread.

use super::{Connection, Failure};
use crate::content::vectors::Vector;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::atomic::{AtomicBool, Ordering}, time::{Duration, Instant}};

pub const SHARD_CAPACITY: usize = 65_536;
pub const MAX_SHARDS: usize = 256;
const MAX_FILE: u64 = 384 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub token: String,
    pub count: usize,
    pub bytes: u64,
    pub checksum: String,
}
impl Artifact {
    pub fn valid(&self) -> bool {
        valid_hex(&self.token,32) && valid_hex(&self.checksum,64)
            && (1..=SHARD_CAPACITY).contains(&self.count) && (64..=MAX_FILE).contains(&self.bytes)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub candidates: Vec<(i64,f32)>,
    pub unavailable: usize,
}

#[derive(Serialize)]
struct Request<'a> { version: u8, id: u64, command: Command<'a> }

#[derive(Serialize)]
#[serde(tag="operation",rename_all="camelCase")]
enum Command<'a> {
    Begin {token:&'a str,dimensions:usize,count:usize},
    Add {entries:Vec<Entry<'a>>},
    Finish {},
    Query {dimensions:usize,vector:&'a [f32],shards:Vec<Shard<'a>>,limit:usize},
}
#[derive(Serialize)]
struct Entry<'a> { key:i64, values:&'a [f32] }
#[derive(Serialize)]
struct Shard<'a> { token:&'a str, checksum:&'a str }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {version:u8,id:u64,result:Option<Output>,error:Option<WorkerError>}
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
enum WorkerError { InvalidRequest, Unavailable, Busy }
#[derive(Deserialize)]
#[serde(tag="kind",rename_all="camelCase",deny_unknown_fields)]
enum Output {
    Accepted {count:usize},
    Built {token:String,count:usize,bytes:u64,checksum:String},
    Found {hits:Vec<Hit>,unavailable:usize},
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hit {key:i64,distance:f32}

struct Build {token:String,dimensions:usize,expected:usize,added:usize,last:i64}

pub struct Client {
    path: PathBuf,
    directory: PathBuf,
    connection: Option<Connection>,
    sequence: u64,
    build: Option<Build>,
    timeout: Duration,
}

impl Client {
    pub fn new(path: PathBuf, directory: PathBuf) -> Self {
        Self {path,directory,connection:None,sequence:0,build:None,timeout:Duration::from_secs(2)}
    }

    pub fn close(&mut self) { self.connection=None; self.build=None; }

    pub fn begin(&mut self, token: &str, dimensions: usize, count: usize, cancel: &AtomicBool) -> Result<(),Failure> {
        if self.build.is_some() || !valid_hex(token,32) || !(1..=2048).contains(&dimensions) || !(1..=SHARD_CAPACITY).contains(&count) {
            return Err(Failure::InvalidInput);
        }
        let output = self.request(Command::Begin {token,dimensions,count},cancel,self.timeout)?;
        if !matches!(output,Output::Accepted {count:0}) { return self.invalid_response(); }
        self.build=Some(Build {token:token.into(),dimensions,expected:count,added:0,last:0});
        Ok(())
    }

    pub fn add(&mut self, vectors: &[Vector], cancel: &AtomicBool) -> Result<(),Failure> {
        let build = self.build.as_ref().ok_or(Failure::InvalidInput)?;
        if vectors.is_empty() || vectors.len()>64 || build.added+vectors.len()>build.expected { return Err(Failure::InvalidInput); }
        let mut last=build.last;
        for vector in vectors {
            if vector.key<=last || !valid_vector(&vector.values,build.dimensions) { return Err(Failure::InvalidInput); }
            last=vector.key;
        }
        let expected=build.added+vectors.len();
        let entries=vectors.iter().map(|vector|Entry {key:vector.key,values:&vector.values}).collect();
        let output=self.request(Command::Add {entries},cancel,self.timeout)?;
        if !matches!(output,Output::Accepted {count} if count==expected) { return self.invalid_response(); }
        let build=self.build.as_mut().ok_or(Failure::InvalidResponse)?;
        build.added=expected;
        build.last=last;
        Ok(())
    }

    pub fn finish(&mut self, cancel: &AtomicBool) -> Result<Artifact,Failure> {
        let build=self.build.take().ok_or(Failure::InvalidInput)?;
        if build.added!=build.expected { self.close(); return Err(Failure::InvalidInput); }
        let output=self.request(Command::Finish {},cancel,Duration::from_secs(10))?;
        let Output::Built {token,count,bytes,checksum}=output else { return self.invalid_response(); };
        let artifact=Artifact {token,count,bytes,checksum};
        if !artifact.valid() || artifact.token!=build.token || artifact.count!=build.expected { return self.invalid_response(); }
        Ok(artifact)
    }

    pub fn search(&mut self, vector: &[f32], artifacts: &[Artifact], limit: usize, cancel: &AtomicBool) -> Result<Found,Failure> {
        if cancel.load(Ordering::Acquire) { self.close(); return Err(Failure::Cancelled); }
        if self.build.is_some() || !valid_vector(vector,vector.len()) || artifacts.len()>MAX_SHARDS
            || artifacts.iter().any(|artifact|!artifact.valid()) || !(1..=100).contains(&limit) {
            return Err(Failure::InvalidInput);
        }
        let mut tokens=std::collections::HashSet::new();
        if artifacts.iter().any(|artifact|!tokens.insert(&artifact.token)) { return Err(Failure::InvalidInput); }
        if artifacts.is_empty() { return Ok(Found {candidates:Vec::new(),unavailable:0}); }
        let shards=artifacts.iter().map(|artifact|Shard {token:&artifact.token,checksum:&artifact.checksum}).collect();
        let output=self.request(Command::Query {dimensions:vector.len(),vector,shards,limit},cancel,self.timeout)?;
        let Output::Found {hits,unavailable}=output else { return self.invalid_response(); };
        if hits.len()>limit || hits.len()>artifacts.iter().map(|artifact|artifact.count).sum::<usize>()
            || unavailable>artifacts.len() || (unavailable==artifacts.len() && !hits.is_empty())
            || hits.iter().any(|hit|hit.key<=0 || !hit.distance.is_finite() || !(0.0..=2.0).contains(&hit.distance)) {
            return self.invalid_response();
        }
        let mut keys=std::collections::HashSet::new();
        if hits.iter().any(|hit|!keys.insert(hit.key)) { return self.invalid_response(); }
        let mut candidates:Vec<_>=hits.into_iter().map(|hit|(hit.key,hit.distance)).collect();
        candidates.sort_by(|a,b|a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        Ok(Found {candidates,unavailable})
    }

    fn invalid_response<T>(&mut self) -> Result<T,Failure> { self.close(); Err(Failure::InvalidResponse) }

    fn request(&mut self, command: Command<'_>, cancel: &AtomicBool, timeout: Duration) -> Result<Output,Failure> {
        if cancel.load(Ordering::Acquire) { self.close(); return Err(Failure::Cancelled); }
        self.sequence=self.sequence.checked_add(1).ok_or(Failure::Unavailable)?;
        let mut bytes=serde_json::to_vec(&Request {version:1,id:self.sequence,command}).map_err(|_|Failure::InvalidInput)?;
        bytes.push(b'\n');
        if bytes.len()>2*1024*1024 { return Err(Failure::InvalidInput); }
        let deadline=Instant::now()+if self.connection.is_none() {timeout.max(Duration::from_secs(5))} else {timeout};
        let result=(|| {
            if self.connection.is_none() { self.connection=Some(Connection::spawn_in(&self.path,Some(&self.directory))?); }
            let response:Response=self.connection.as_mut().ok_or(Failure::Unavailable)?.exchange(&bytes,deadline,cancel)?;
            if response.version!=1 || response.id!=self.sequence { return Err(Failure::InvalidResponse); }
            match (response.result,response.error) {
                (Some(result),None) => Ok(result),
                (None,Some(WorkerError::Unavailable | WorkerError::Busy)) => Err(Failure::Unavailable),
                _ => Err(Failure::InvalidResponse),
            }
        })();
        if result.is_err() { self.close(); }
        result
    }
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len()==length && value.bytes().all(|byte|byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_vector(values: &[f32], dimensions: usize) -> bool {
    (1..=2048).contains(&dimensions) && values.len()==dimensions && values.iter().all(|value|value.is_finite())
        && (values.iter().map(|value|f64::from(*value).powi(2)).sum::<f64>()-1.0).abs()<=0.001
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::{DirBuilderExt,PermissionsExt},sync::{Arc,atomic::AtomicU64}};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(behavior: &str) -> Self {
            static NEXT:AtomicU64=AtomicU64::new(0);
            let directory=std::env::temp_dir().join(format!("blindspot-vector-client-{}-{}",std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));
            std::fs::DirBuilder::new().mode(0o700).create(&directory).unwrap();
            let source=format!(r#"#!/usr/bin/python3
import json,sys,time
count=0
token=''
for line in sys.stdin:
    request=json.loads(line)
    command=request['command']
    operation=command['operation']
    if operation=='begin':
        token=command['token']; count=0
        result={{'kind':'accepted','count':count}}
    elif operation=='add':
        count+=len(command['entries'])
        result={{'kind':'accepted','count':count}}
    elif operation=='finish':
        result={{'kind':'built','token':token,'count':count,'bytes':128,'checksum':'0'*64}}
    else:
        result={{'kind':'found','hits':[{{'key':1,'distance':0.1}}],'unavailable':0}}
    response={{'version':1,'id':request['id'],'result':result}}
    {behavior}
    print(json.dumps(response),flush=True)
"#);
            let path=directory.join("worker");
            std::fs::write(&path,source).unwrap();
            std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(directory)
        }
        fn client(&self)->Client {Client::new(self.0.join("worker"),self.0.clone())}
    }
    impl Drop for Fixture {fn drop(&mut self){std::fs::remove_dir_all(&self.0).unwrap();}}

    fn build(client:&mut Client)->Artifact {
        let cancel=AtomicBool::new(false);
        client.begin(&"a".repeat(32),2,1,&cancel).unwrap();
        client.add(&[Vector {key:1,values:vec![1.0,0.0]}],&cancel).unwrap();
        client.finish(&cancel).unwrap()
    }

    #[test]
    fn build_protocol_validates_counts_and_reuses_worker() {
        let fixture=Fixture::new("pass");
        let mut client=fixture.client();
        let artifact=build(&mut client);
        let pid=client.connection.as_ref().unwrap().child.id();
        let result=client.search(&[1.0,0.0],&[artifact],3,&AtomicBool::new(false)).unwrap();
        assert_eq!(result.candidates,vec![(1,0.1)]);
        assert_eq!(client.connection.as_ref().unwrap().child.id(),pid);
        assert!(client.build.is_none());
    }

    #[test]
    fn malformed_candidates_and_artifacts_close_the_connection() {
        for behavior in [
            "if operation=='query': response['id']+=1",
            "if operation=='query': result['hits']*=2",
            "if operation=='query': result['hits'][0]['distance']=3.0",
            "if operation=='query': result['hits'][0]['key']=0",
            "if operation=='query': result['unavailable']=2",
            "if operation=='query': result['unavailable']=1",
            "if operation=='query': response['extra']='unexpected'",
        ] {
            let fixture=Fixture::new(behavior);
            let mut client=fixture.client();
            let artifact=build(&mut client);
            assert_eq!(client.search(&[1.0,0.0],&[artifact],3,&AtomicBool::new(false)),Err(Failure::InvalidResponse),"{behavior}");
            assert!(client.connection.is_none());
        }
        for behavior in [
            "if operation=='finish': result['token']='../outside'",
            "if operation=='finish': result['checksum']='invalid'",
            "if operation=='finish': result['bytes']=999999999",
            "if operation=='finish': result['count']=2",
        ] {
            let fixture=Fixture::new(behavior);
            let mut client=fixture.client();
            let cancel=AtomicBool::new(false);
            client.begin(&"a".repeat(32),2,1,&cancel).unwrap();
            client.add(&[Vector {key:1,values:vec![1.0,0.0]}],&cancel).unwrap();
            assert_eq!(client.finish(&cancel),Err(Failure::InvalidResponse),"{behavior}");
            assert!(client.connection.is_none());
        }
    }

    #[test]
    fn invalid_inputs_do_not_start_a_worker() {
        let mut client=Client::new("/nonexistent/worker".into(),"/nonexistent/cache".into());
        let cancel=AtomicBool::new(false);
        assert_eq!(client.begin("../outside",2,1,&cancel),Err(Failure::InvalidInput));
        assert_eq!(client.begin(&"a".repeat(32),2,SHARD_CAPACITY+1,&cancel),Err(Failure::InvalidInput));
        assert_eq!(client.search(&[0.0,0.0],&[],3,&cancel),Err(Failure::InvalidInput));
        assert_eq!(client.search(&[1.0,0.0],&[],3,&AtomicBool::new(true)),Err(Failure::Cancelled));
        assert!(client.connection.is_none());
    }

    #[test]
    fn query_cancellation_and_deadline_terminate_the_worker() {
        let fixture=Fixture::new("if operation=='query': time.sleep(5)");
        let mut client=fixture.client();
        let artifact=build(&mut client);
        let cancel=Arc::new(AtomicBool::new(false));
        let trigger=Arc::clone(&cancel);
        let thread=std::thread::spawn(move||{
            std::thread::sleep(Duration::from_millis(50));
            trigger.store(true,Ordering::Release);
        });
        let started=Instant::now();
        assert_eq!(client.search(&[1.0,0.0],std::slice::from_ref(&artifact),3,&cancel),Err(Failure::Cancelled));
        assert!(started.elapsed()<Duration::from_millis(500));
        assert!(client.connection.is_none());
        thread.join().unwrap();
        build(&mut client);
        client.timeout=Duration::from_millis(50);
        let started=Instant::now();
        assert_eq!(client.search(&[1.0,0.0],&[artifact],3,&AtomicBool::new(false)),Err(Failure::TimedOut));
        assert!(started.elapsed()<Duration::from_millis(500));
        assert!(client.connection.is_none());
    }

    #[test]
    #[ignore="requires the built vector worker and native file-descriptor access"]
    fn native_round_trip_resolves_only_current_database_keys() {
        use crate::content::{ContentStore,Document};
        use std::path::Path;
        let fixture=Fixture::new("pass");
        let mut store=ContentStore::open(&fixture.0.join("content.sqlite")).unwrap();
        let scan=store.begin_scan(Path::new("/fixture")).unwrap();
        let documents:Vec<_>=["/fixture/a.txt","/fixture/b.txt"].iter().map(|path|Document {
            identity:path,path:Path::new(path),title:path,body:"fixture text",modified_ns:1,changed_ns:1,bytes:12,
            extraction: crate::content::Extraction::Text,
        }).collect();
        let identities=store.put_batch(&scan,&documents).unwrap();
        for ((id,revision),values) in identities.iter().zip([[1.0,0.0],[0.0,1.0]]) {store.put_embedding(*id,*revision,"fixture",&values).unwrap();}
        let high=store.embedding_watermark().unwrap();
        let vectors=store.embedding_vectors(0,high,"fixture",2,Arc::new(AtomicBool::new(false))).unwrap().vectors;
        let path=PathBuf::from(std::env::var_os("BLINDSPOT_VECTOR_WORKER").expect("helper path"));
        let mut client=Client::new(path,fixture.0.clone());
        client.begin(&"b".repeat(32),2,2,&AtomicBool::new(false)).unwrap();
        client.add(&vectors,&AtomicBool::new(false)).unwrap();
        let artifact=client.finish(&AtomicBool::new(false)).unwrap();
        client.close();
        let result=client.search(&[1.0,0.0],std::slice::from_ref(&artifact),2,&AtomicBool::new(false)).unwrap();
        assert_eq!(result.unavailable,0);
        let hits=store.resolve_embeddings("fixture",2,&result.candidates,Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(hits[0].id,identities[0].0);
        store.put_batch(&scan,&documents[..1]).unwrap();
        let result=client.search(&[1.0,0.0],&[artifact],2,&AtomicBool::new(false)).unwrap();
        let hits=store.resolve_embeddings("fixture",2,&result.candidates,Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(hits.len(),1);
        assert_eq!(hits[0].id,identities[1].0);
    }

    #[test]
    #[ignore="requires the built vector worker and native file-descriptor access"]
    fn native_builds_database_pages_and_cancelled_build_has_no_artifact() {
        use crate::{content::{ContentStore,Document},semantic::{Model,indexing::{build_shard,model_key,ShardRange}}};
        use std::path::Path;
        let fixture=Fixture::new("pass");
        let mut store=ContentStore::open(&fixture.0.join("content.sqlite")).unwrap();
        let scan=store.begin_scan(Path::new("/fixture")).unwrap();
        let paths:Vec<_>=(0..130).map(|i|format!("/fixture/{i}.txt")).collect();
        let documents:Vec<_>=paths.iter().map(|path|Document {
            identity:path,path:Path::new(path),title:path,body:"fixture text",modified_ns:1,changed_ns:1,bytes:12,
            extraction: crate::content::Extraction::Text,
        }).collect();
        let model=Model {identifier:"fixture".into(),revision:1,dimensions:2};
        let key=model_key(&model).unwrap();
        for (id,revision) in store.put_batch(&scan,&documents).unwrap() {
            store.put_embedding(id,revision,&key,&[1.0,0.0]).unwrap();
        }
        let high=store.embedding_watermark().unwrap();
        let cancel=Arc::new(AtomicBool::new(false));
        assert_eq!(store.embedding_count(0,high,&key,2,Arc::clone(&cancel)).unwrap(),130);
        assert!(store.embedding_count(0,65_537,&key,2,Arc::clone(&cancel)).is_err());
        assert_eq!(store.embedding_count(0,high,"other",2,Arc::clone(&cancel)).unwrap(),0);
        let path=PathBuf::from(std::env::var_os("BLINDSPOT_VECTOR_WORKER").expect("helper path"));
        let mut client=Client::new(path,fixture.0.clone());
        let mut range=ShardRange {token:"c".repeat(32),after:0,through:high};
        let result=build_shard(&store,&mut client,&model,&range,Arc::clone(&cancel), |added| {
            if added>=64 {cancel.store(true,Ordering::Release);}
        });
        assert!(matches!(result,Err(crate::semantic::indexing::Error::Cancelled)));
        assert!(client.connection.is_none());
        assert!(!fixture.0.join(format!("{}.ann",range.token)).exists());
        cancel.store(false,Ordering::Release);
        range.token="d".repeat(32);
        let artifact=build_shard(&store,&mut client,&model,&range,Arc::clone(&cancel), |_|{}).unwrap().unwrap();
        assert_eq!(artifact.count,130);
        let found=client.search(&[1.0,0.0],&[artifact],10,&cancel).unwrap();
        assert_eq!(found.unavailable,0);
        assert_eq!(found.candidates.len(),10);
        assert_eq!(store.resolve_embeddings(&key,2,&found.candidates,Arc::clone(&cancel)).unwrap().len(),10);
        let empty=ShardRange {token:"e".repeat(32),after:high,through:high};
        assert!(build_shard(&store,&mut client,&model,&empty,cancel, |_|{}).unwrap().is_none());
        assert!(!fixture.0.join(format!("{}.ann",empty.token)).exists());
        store.check_integrity().unwrap();
    }
}
