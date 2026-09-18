# Local content index: implementation and measurements

> **Historical record.** The implementation status, measurements and release instructions below
> describe their recorded revision, not the current app. See the [documentation index](README.md),
> [current feature guide](new-features.md) and [current release workflow](releasing.md).

This is an internal implementation note. The opt-in storage/scanner now runs through
background workers, live Content settings and launcher search. Validation uses synthetic
fixtures; this work has not enabled indexing of personal files. Existing Spotlight
search and redb user stores remain in place.

## Ownership and persistence

- `content.rs`: one SQLite connection per worker; separate read-only connections use
  WAL snapshots. Batches contain at most 256 documents, source text at most 64 KiB,
  and returned pages at most 100 hits. Cache budgets are 8 MiB writer / 4 MiB reader.
- An application ID and version protect foreign/future databases. Migrations are
  transactional; failure propagates without deleting the database. Corruption does
  not trigger a rebuild that could silently discard state.
- File identity uses device, inode and creation timestamp. SQLite IDs are never
  reused. An update increments a revision and invalidates its old embedding; an
  embedding response can commit only against the same ID and revision.
- A scan generation marks observed records. Only a complete scan removes unseen
  records, in cancellable batches. Partial scans preserve prior committed data.
- Embeddings have explicit model, dimensions and normalized finite float32 data.
  Vector retrieval and model migration remain unfinished. The explicit whole-index erase
  control is described below.

Schema 5 adds independent AUTOINCREMENT embedding IDs for derived vector indexes. A
replacement receives a fresh key transactionally; an insertion failure preserves the old
embedding. Existing schema-4 vectors migrate without re-inference. Migration copies the
embedding table and can require temporary disk space proportional to that table. Failed
migration preserves the old schema and bytes. Erasure retains the non-private embedding
sequence, preventing old cache keys from aliasing later records. `content/vectors.rs`
exports at most 64 validated vectors per page and resolves at most 100 candidate keys
against current document revisions/models. Full semantic retrieval integration remains
unfinished; see `semantic-search.md` for the isolated worker and its boundaries.

## Filesystem safety and resource limits

The scanner runs synchronously **on a background worker**, not on the UI actor.
`ffi/index_native.rs` owns macOS `DIR` handles and uses public `openat`/`fstatat`
operations with no-follow flags. A regression replaces an open directory with a
symlink and verifies that reads remain anchored to the original directory.

Only local-volume UTF-8 text/code files are currently supported. Symlinks, cloud
placeholders, another device's mount, hidden/generated paths, common credential
filenames, binary data and oversized sources are skipped. Defaults: 1 MiB source
size, 64 KiB indexed excerpt, depth 64, 10M visited entries, 10 ms pause per 256
visited entries. Tests and benchmarks explicitly disable pauses where indicated.
The worker uses background QoS. Native power/thermal notifications pause indexing in
Low Power Mode, serious/critical thermal states, and on battery unless enabled in settings.

Schema version 3 adds ctime to identity/mtime/size fingerprints, including a regression
for same-size edits with restored mtime. Unchanged files avoid content reads. Hard links
share a record and choose the lexicographically first path observed in a scan.
Network mounts are rejected, but macOS filesystem calls themselves are not guaranteed
to be cancellable once entered. PDF extraction is not part of this scanner yet.

## Retrieval and ranking tradeoff

Queries become bounded, quoted alphanumeric FTS terms; FTS operators are data, never
executable query syntax. SQLite progress callbacks enforce cancellation and a 250 ms
deadline. Traditional launcher search must remain available if content retrieval fails.

[SQLite FTS5](https://www.sqlite.org/fts5.html) provides lexical matching and BM25.
Inspection of the bundled SQLite implementation (`fts5Bm25GetData`) showed that it
counts matching documents for each phrase to compute IDF. Merely limiting returned
rows does not bound this cost.

The current query first probes each term up to 10,001 matches. If a term exceeds the
budget, it avoids global BM25 and returns a bounded set ordered by descending index
ID, with `SearchPage.limited=true`. Otherwise it uses BM25 (title weight 4, body 1).
This is a deliberate degraded relevance mode, **not globally optimal BM25** and not
file modification recency. The UI labels these hits “limited relevance; refine query”.

## Measurements

All data below is synthetic, on this development Mac. Timings are ten-query medians
unless stated. They measure the Rust storage layer, not launcher render latency.

| Rows | Index time | DB size | Unique term | 1/1,000 topic | Ubiquitous phrase |
|---:|---:|---:|---:|---:|---:|
| 10K, initial BM25 | 0.365 s | 3.90 MB | 0.045 ms | 0.060 ms | 5.192 ms |
| 100K, initial BM25 | 5.482 s | 40.44 MB | 0.048 ms | 0.149 ms | 52.323 ms |
| 1M, initial BM25 | 42.651 s | 392.52 MB | 0.047 ms | 0.700 ms | 10/10 timed out |
| 1M, bounded BM25 candidates | 42.129 s | 392.52 MB | 0.065 ms | 0.760 ms | 33.661 ms, limited |
| 10M, bounded BM25 candidates | 431.390 s | 4.05 GB | 0.082 ms | 16.368 ms | 10/10 timed out |
| 100K, broad-term fallback | 4.732 s | 40.44 MB | 0.080 ms | 0.180 ms | 0.353 ms, degraded ranking |
| 10M, broad-term fallback | 430.212 s | 4.05 GB | 0.110 ms | 16.632 ms | 0.477 ms, degraded ranking |

The final 10M fallback run peaked at **23.99 MB RSS**, with no full corpus loaded in
application memory. Total wall time was 465.07 s including integrity checking and cleanup;
all query groups completed without cancellation. This demonstrates bounded storage memory
and useful selective retrieval, not complete 10M-record product readiness. These scale
measurements precede schema 3, stable hard-link selection and FTS secure deletion; they
are not measurements of the final product. Log: `/tmp/blindspot-content-10m-final.log`.

The real-filesystem harness created 10K small fixture files: initial indexing took
**775.060 ms**, the unchanged scan **496.360 ms**, with no content reads in the latter.
This ran without deliberate batch pauses, and overlapped the 10M storage benchmark;
it is a repeatable development measurement, not a battery or throughput guarantee.

The current schema-3/secure-delete 100K run indexed in **4.304 s**, with query medians
**0.087 ms** unique, **0.186 ms** topic, **0.091 ms** vendor and **0.352 ms** broad fallback;
all query groups completed. Log: `/tmp/blindspot-content-privacy-scale.log`.

The extended current-code 10K filesystem harness measured **837.466 ms** initial,
**463.002 ms** unchanged, **759.089 ms** updating 1K files, and **561.521 ms** reconciling
1K deletions. Expected update/removal counts and final integrity checks passed. File
creation/mutation preparation is outside the measured scan time; deliberate pauses are
zero. There is no equivalent prior update/delete baseline. Log:
`/tmp/blindspot-content-privacy-scan.log`.

## Application lifecycle and privacy

`content_service.rs` owns coalesced indexing/search workers with cancellation and epochs.
Settings are opt-in: `content.enabled`, `roots`, `excluded_paths`, `max_file_mb`, and
`on_battery`. Dedicated `:content words` results reuse file actions; normal filename
search appends deduplicated content hits without displacing existing filename ordering.
Failures leave ordinary search usable. Queries use separate read-only WAL connections.

`ContentWatcher.swift` uses public FSEvents, a two-second debounce, and power/thermal
notifications. It checks progress once per second only while indexing; there is no idle
one-second polling loop. Changes currently trigger a metadata reconciliation of configured
roots, not selective per-path traversal. Startup reconciles after missed events. Canonical
paths must agree with FSEvents (`/private/var`, not Foundation's `/var` alias); the native
harness tests actual delivery, exclusions, burst coalescing, shutdown and owner release.

Both SQLite core and FTS5 secure deletion are enabled. A regression first reproduced a
deleted term remaining in FTS segments, then passed with the FTS setting. See
[SQLite's secure-delete documentation](https://www.sqlite.org/fts5.html#the_secure_delete_configuration_option).
Removed roots are cleaned in cancellable batches after all remaining roots scan completely;
unavailable roots defer cleanup. Schema 4 adds a monotonic scan clock so root metadata can be removed while stale
scan writes remain rejected, including after erasure and re-indexing. Exclusions hide matches immediately and complete reconciliation removes records.
Disabling indexing or clearing the root list retains stored data, as the settings describe.
The Content settings page offers confirmed erasure and a Stop control. Indexing must
first be persistently disabled. The same worker serializes erasure after the cancelled
scan; content-setting mutations are refused while erasure is pending. Erasure invalidates
scan generations, removes documents/embeddings in bounded transactions, clears remaining
FTS segments and root metadata, then truncates the WAL. IDs and the non-private scan
counter remain monotonic. Cancellation or a pinned WAL reader yields an incomplete status;
retry resumes safely. Missing indexes are a successful no-op. Corrupt/foreign databases
are preserved and reported as unavailable rather than deleted as arbitrary files.

Settings saves now use private temporary files and atomic replacement. Failed writes leave
both effective values and the prior file unchanged. Unreadable, oversized or malformed
settings files block replacement; loaded values pass schema validation. Parse errors no
longer log source text. File contents are synced before rename; directory sync is best
effort. This does not provide a forensic erasure guarantee for filesystem snapshots,
SSD storage, or backups. Watch-start failure reporting and per-path incremental work remain
unfinished.

## Local embedding feasibility

`bench/EmbeddingProbe.swift` uses the optional installed
[Apple sentence embedding](https://developer.apple.com/documentation/naturallanguage/nlembedding/sentenceembedding(for:)).
On this Mac it returned English revision 1 with 512 dimensions: 52 ms initialization,
23 ms for eight query embeddings plus ranking, top-1 6/8 and top-3 8/8 on twelve
synthetic documents. It was unavailable inside the tool sandbox but available in the
native probe. No model download was requested.

This small fixture supports an optional local experiment, not a broad quality claim.
Language coverage, document chunking, vector retrieval recall, model availability,
resource scheduling and conventional fallback still need integration and evaluation.

The schema-4 10K filesystem rerun measured **970.143 ms** initial, **535.781 ms**
unchanged, **776.049 ms** for 1K updates, and **588.555 ms** for 1K deletions. Explicit
erasure of the remaining 9K records took **1,350.924 ms**, including WAL truncation and
the production erase batch pauses. Source-file counts and database integrity passed.
Scan phases disable their deliberate pauses. These single development runs are not an
improvement claim; log: `/tmp/blindspot-erasure-scan-benchmark.log`.

## Erasure validation

**284 Rust tests** passed after schema 4, including interrupted erasure/reopen/retry,
pinned-reader journal refusal, migration from versions 1 and 3, stale scan/embedding
rejection, missing-index no-op, paused-worker erasure, source preservation, failed settings
writes, corrupt-settings preservation and invalid-value rejection. **15 Swift checks**,
full app build, clippy and the **14-query** native panel smoke test passed. The panel test
opens the real confirmation sheet and presses only Cancel; it verifies settings and
erasure state are unchanged. macOS view-service access is required for this new check;
the sandbox run could not expose the sheet controls. Native test logs:
`/tmp/blindspot-erasure-full-tests.log`, `/tmp/blindspot-erasure-native-panel.log`, and
`/tmp/blindspot-erasure-action-tests.log`.

## Reproduction

```sh
MACOSX_DEPLOYMENT_TARGET=26.0 cargo run --release --offline --manifest-path core/Cargo.toml --example content_scale -- 100000
MACOSX_DEPLOYMENT_TARGET=26.0 cargo run --release --offline --manifest-path core/Cargo.toml --example index_scan -- 10000
cargo test --offline --manifest-path core/Cargo.toml content::
cargo test --offline --manifest-path core/Cargo.toml content_indexer::
```

The benchmark creates a uniquely named temporary directory and removes it on normal
completion, including ordinary errors. An externally terminated benchmark can leave
its own temporary fixture behind. It never opens a production index or personal files.

## Schema 6: derived-vector catalog

Schema 6 adds `vector_shards` without rewriting documents or embedding blobs. Each row records
one aligned key range, model/dimensions, opaque file token, checksum, vector count and file size.
Checks constrain metadata; catalog APIs additionally cap reads/publication at 256 shards and
validate current embedding identities inside the publication transaction. Migration conflict
rolls back and preserves existing vectors. Explicit erasure clears catalog rows before journal
cleanup; the content service also removes owned vector artifacts through anchored file operations.
Source files are never part of this deletion. See `semantic-search.md` for recovery and cache limits.
