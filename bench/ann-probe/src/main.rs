use std::time::Instant;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

const DIMENSIONS: usize = 512;

fn noise(seed: u64) -> Vec<f32> {
    let mut state = seed.wrapping_add(1);
    let mut values: Vec<f32> = (0..DIMENSIONS).map(|_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        ((state >> 40) as f32 / 8_388_608.0) - 1.0
    }).collect();
    let norm = values.iter().map(|value| value*value).sum::<f32>().sqrt();
    for value in &mut values { *value /= norm; }
    values
}

fn vector(seed: u64, clustered: bool) -> Vec<f32> {
    let mut values = noise(seed);
    if clustered {
        let centroid = noise((seed % 128).wrapping_add(1_000_000_000));
        for (value, center) in values.iter_mut().zip(centroid) { *value = 0.35 * *value + 0.85 * center; }
        let norm = values.iter().map(|value| value*value).sum::<f32>().sqrt();
        for value in &mut values { *value /= norm; }
    }
    values
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("fixture") {
        return run_fixture(&std::env::args().nth(2).ok_or("Expected fixture JSON path")?);
    }
    let count: usize = std::env::args().nth(1).unwrap_or_else(|| "10000".into()).parse()?;
    if !(100..=1_000_000).contains(&count) { return Err("Expected 100..1000000 vectors".into()); }
    let dataset = std::env::args().nth(2).unwrap_or_else(|| "isotropic".into());
    if !["isotropic", "clustered"].contains(&dataset.as_str()) { return Err("Expected isotropic or clustered dataset".into()); }
    let shard_size = std::env::args().nth(3).map(|value|value.parse::<usize>()).transpose()?;
    if shard_size.is_some_and(|size|size<100 || size>count || count.div_ceil(size)>1024) {
        return Err("Expected shard size 100..count and at most 1024 shards".into());
    }
    let path = std::env::temp_dir().join(format!("blindspot-ann-probe-{}",std::process::id()));
    std::fs::create_dir(&path)?;
    let result = match shard_size {
        Some(size) => run_sharded(count,size,&path,dataset=="clustered"),
        None => run(count,&path,dataset=="clustered"),
    };
    std::fs::remove_dir_all(path)?;
    result
}

fn options() -> IndexOptions {
    IndexOptions { dimensions:DIMENSIONS, metric:MetricKind::Cos,
        quantization:ScalarKind::F16, connectivity:16, expansion_add:128, expansion_search:128,
        ..IndexOptions::default() }
}

fn merge(found: &mut Vec<(u64,f32)>, keys: Vec<u64>, distances: Vec<f32>) {
    found.extend(keys.into_iter().zip(distances));
    found.sort_by(|a,b|a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    found.truncate(10);
}

fn run_sharded(count: usize, shard_size: usize, directory: &std::path::Path, clustered: bool) -> Result<(), Box<dyn std::error::Error>> {
    let queries: Vec<_> = (0..20).map(|id|vector((count+id) as u64,clustered)).collect();
    let mut exact = vec![Vec::new();queries.len()];
    let mut files = Vec::new();
    let mut build_ms = 0.0;
    let mut estimated_peak = 0;
    let mut serialized = 0;
    for start in (0..count).step_by(shard_size) {
        let end = (start+shard_size).min(count);
        let started = Instant::now();
        let index = Index::new(&options())?;
        index.reserve_capacity_and_threads(end-start,1)?;
        for id in start..end { index.add(id as u64,&vector(id as u64,clustered))?; }
        let path = directory.join(format!("{start}.usearch"));
        index.save(path.to_str().ok_or("path")?)?;
        build_ms += started.elapsed().as_secs_f64()*1000.0;
        estimated_peak = estimated_peak.max(index.memory_usage());
        serialized += std::fs::metadata(&path)?.len();
        for (query,truth) in queries.iter().zip(&mut exact) {
            let found = index.exact_search(query,10)?;
            merge(truth,found.keys,found.distances);
        }
        files.push(path);
    }
    println!("vectors={count} shard_size={shard_size} shards={} clustered={clustered} build_ms={build_ms:.3} estimated_peak_index_bytes={estimated_peak} serialized_bytes={serialized}",files.len());
    for expansion in [128,512] {
        let mut latency = Vec::new();
        let mut correct = 0;
        for (query,truth) in queries.iter().zip(&exact) {
            let started = Instant::now();
            let mut found = Vec::new();
            for file in &files {
                let index = Index::new(&options())?;
                index.view(file.to_str().ok_or("path")?)?;
                index.change_expansion_search(expansion);
                let next = index.search(query,10)?;
                merge(&mut found,next.keys,next.distances);
            }
            latency.push(started.elapsed().as_secs_f64()*1000.0);
            assert_eq!(found.len(),10);
            assert!(found.iter().all(|(_,distance)|distance.is_finite()));
            correct += found.iter().filter(|(id,_)|truth.iter().any(|(expected,_)|id==expected)).count();
        }
        latency.sort_by(f64::total_cmp);
        println!("expansion={expansion} recall_at_10={:.3} median_with_mapping_ms={:.3} max_ms={:.3}",correct as f64/200.0,latency[latency.len()/2],latency.last().ok_or("timing")?);
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    revision: u64,
    dimensions: usize,
    documents: Vec<Vec<f32>>,
    queries: Vec<Vec<f32>>,
    expected: Vec<usize>,
}

fn run_fixture(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(1_048_577).read_to_end(&mut bytes)?;
    if bytes.len()>1_048_576 { return Err("Fixture too large".into()); }
    let fixture: Fixture = serde_json::from_slice(&bytes)?;
    if fixture.dimensions!=DIMENSIONS || fixture.documents.len()<3 || fixture.documents.len()>1000
        || fixture.queries.is_empty() || fixture.queries.len()!=fixture.expected.len()
        || fixture.expected.iter().any(|id|*id>=fixture.documents.len())
        || fixture.documents.iter().chain(&fixture.queries).any(|vector|vector.len()!=DIMENSIONS || vector.iter().any(|value|!value.is_finite())) {
        return Err("Invalid fixture vectors".into());
    }
    let options = options();
    let index = Index::new(&options)?;
    index.reserve_capacity_and_threads(fixture.documents.len(),1)?;
    for (id,vector) in fixture.documents.iter().enumerate() { index.add(id as u64,vector)?; }
    let mut top1 = 0;
    let mut top3 = 0;
    let mut recall = 0;
    for (query,expected) in fixture.queries.iter().zip(&fixture.expected) {
        let mut exact: Vec<_> = fixture.documents.iter().enumerate().map(|(id,vector)| {
            let dot = vector.iter().zip(query).map(|(a,b)|f64::from(*a)*f64::from(*b)).sum::<f64>();
            (id,dot)
        }).collect();
        exact.sort_by(|a,b|b.1.total_cmp(&a.1));
        let found = index.search(query,3)?;
        if found.keys.first() == Some(&(*expected as u64)) { top1+=1; }
        if found.keys.contains(&(*expected as u64)) { top3+=1; }
        recall += found.keys.iter().filter(|key|exact[..3].iter().any(|(id,_)|*id as u64==**key)).count();
    }
    println!("native_revision={} documents={} queries={} top1={top1} top3={top3} ann_recall_at_3={:.3}",
        fixture.revision,fixture.documents.len(),fixture.queries.len(),recall as f64/(fixture.queries.len()*3) as f64);
    Ok(())
}

fn run(count: usize, directory: &std::path::Path, clustered: bool) -> Result<(), Box<dyn std::error::Error>> {
    let options = options();
    let index = Index::new(&options)?;
    index.reserve_capacity_and_threads(count,1)?;
    let started = Instant::now();
    for id in 0..count { index.add(id as u64,&vector(id as u64,clustered))?; }
    println!("vectors={count} dimensions={DIMENSIONS} clustered={clustered} build_ms={:.3} index_memory_bytes={} acceleration={}",started.elapsed().as_secs_f64()*1000.0,index.memory_usage(),index.hardware_acceleration());
    let queries: Vec<_> = (0..20).map(|id|vector((count+id) as u64,clustered)).collect();
    let exact: Vec<_> = queries.iter().map(|query|index.exact_search(query,10)).collect::<Result<_,_>>()?;
    let file = directory.join("index.usearch");
    index.save(file.to_str().ok_or("path")?)?;
    println!("serialized_bytes={}",std::fs::metadata(&file)?.len());
    drop(index);
    let started = Instant::now();
    let mapped = Index::new(&options)?;
    mapped.view(file.to_str().ok_or("path")?)?;
    println!("mapped_open_ms={:.3}",started.elapsed().as_secs_f64()*1000.0);
    for expansion in [128,256,512,1024] {
        mapped.change_expansion_search(expansion);
        let mut latency = Vec::new();
        let mut correct = 0;
        for (query,truth) in queries.iter().zip(&exact) {
            let started = Instant::now();
            let found = mapped.search(query,10)?;
            latency.push(started.elapsed().as_secs_f64()*1000.0);
            assert_eq!(found.keys.len(),10);
            assert!(found.distances.iter().all(|value|value.is_finite()));
            correct += found.keys.iter().filter(|key|truth.keys.contains(key)).count();
        }
        latency.sort_by(f64::total_cmp);
        println!("expansion={expansion} recall_at_10={:.3} median_ms={:.3} max_ms={:.3}",correct as f64/200.0,latency[latency.len()/2],latency.last().ok_or("timing")?);
    }
    Ok(())
}
