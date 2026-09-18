# Local semantic search: implementation and measurements

> **Historical record.** The implementation status, measurements and release instructions below
> describe their recorded revision, not the current app. See the [documentation index](README.md),
> [current feature guide](new-features.md) and [current release workflow](releasing.md).

Semantic retrieval is not yet exposed in launcher results. This document records the
implemented embedding boundary and the evidence informing the remaining integration.
Conventional filename and opt-in FTS content search continue independently.

## Implemented boundary

`helpers/SemanticWorker.swift` uses the public NaturalLanguage English sentence model
already available on the Mac. It does not request a model download or send text over the
network. Missing assets produce an explicit unavailable response. This model is separate
from the existing local Qwen integration; Qwen remains the text-generation engine.

The application build includes the helper in `Contents/Helpers`; the signing target signs
it before signing the enclosing bundle. It accepts only
versioned probe/embedding requests, never file paths, executable plans or shell commands.
Requests contain at most eight texts, 4 KiB each and 16 KiB total. Responses contain
validated, normalized vectors and the model identifier, revision and dimensions. The
initial implementation embeds excerpts rather than an entire large document.

`core/src/semantic.rs` owns a persistent child through a local socket. Calls belong on
background workers. Startup has a five-second deadline; subsequent requests have a
two-second deadline. Socket operations check cancellation approximately every 25 ms.
Cancellation, malformed output, disconnection or deadline expiration closes and reaps
the worker; a subsequent request can start a new one. Output is bounded to 256 KiB.
Diagnostics do not include text or malformed response contents.

Tests reproduced an interactive framing failure in Foundation's buffered input API:
the helper waited for more input instead of replying to one complete line. A bounded
POSIX read fixes it. Protocol tests now exercise a live request without closing stdin.
Six Rust transport tests pass, covering persistence, malformed output, invalid input,
cancellation, deadlines and missing models. A separate native integration test passes
against the installed English model. Eleven malformed/oversized helper protocol cases,
recovery, privacy and termination checks pass both with model access and in the
model-unavailable sandbox.

## Incremental embedding pass

`content.rs` now enumerates documents in pages of at most 128 using the primary-key cursor,
without OFFSET or a resident corpus. Current model/revision/dimension matches avoid loading
the excerpt. Pending text includes the title and a UTF-8-safe excerpt bounded to 4 KiB.
`semantic/indexing.rs` sends at most four texts per request, commits each validated result
against its expected document revision, and pauses 10 ms between batches. No database
transaction remains open during model inference. A restart skips committed current vectors;
changed/deleted identities cannot receive stale work. The original pass required no schema
change; the derived-index identity layer below introduces schema 5.

The owner supplies scope filtering and cancellation. One unembeddable text does not stop
its neighbors: failed batches retry individually and report failed items. Oversized JSON
escaping takes the same individual fallback. Blank text is reported as failed, not current.
A model change during the pass rejects the response before persistence. The indexing pass
is implemented and tested, but automatic scheduling and launcher semantic retrieval remain
unconnected.

`core/examples/embedding_scan.rs` measures the actual native helper plus SQLite persistence
over synthetic documents. Counts and integrity assertions passed:

| Documents | Fresh pass | Unchanged pass | Re-embed 10% edited |
| --- | ---: | ---: | ---: |
| 1,000 | 7,462.659 ms | 0.452 ms | 777.973 ms |
| 10,000 | 74,307.584 ms | 9.563 ms | 7,604.124 ms |

Logs: `/tmp/blindspot-embedding-1k.log`, `/tmp/blindspot-embedding-10k.log`. The 10K run
partly overlapped compilation; these are feasibility measurements, not an uncontended
before/after comparison. The unchanged pass still visits metadata for every document in
bounded pages: O(N) work, bounded memory, and no inference for current vectors. A durable
dirty-work queue may be warranted for much larger frequently changing corpora. Neither
million-document nor ten-million-document native embedding generation has been measured.

```sh
MACOSX_DEPLOYMENT_TARGET=26.0 cargo build --release --manifest-path core/Cargo.toml \
  --example embedding_scan
target/release/examples/embedding_scan 1000 \
  "$PWD/build/Blindspot.app/Contents/Helpers/blindspot-semantic"
```

## ANN evaluation

`bench/ann-probe` is a separate executable with USearch 2.26.2 pinned in its own lockfile.
The same version is now used by the isolated vector helper described below; it is not
linked into the launcher core. Measurements use 512-dimensional normalized vectors,
cosine distance, float16 storage, HNSW connectivity 16 and construction expansion 128.
Clustered fixtures use 128 synthetic centroids. Recall compares approximate results with
exact search on the same quantized vectors, for 20 queries. This measures approximation
error, not whether results satisfy real user queries.

| Fixture | Build | Peak RSS | Query median | Recall@10 |
| --- | ---: | ---: | ---: | ---: |
| 100K clustered, one index, expansion 128 | 28.36 s | 130.6 MB | 0.355 ms | 1.000 |
| 1M clustered, one index, expansion 128 | 965.20 s | 1,273.2 MB | 1.408 ms | 0.785 |
| 1M clustered, one index, expansion 1024 | same build | same process | 3.019 ms | 0.985 |
| 100K clustered, two shards, expansion 128 | 23.49 s | 85.9 MB | 3.556 ms | 1.000 |
| 1M clustered, 16 shards, expansion 128 | 238.51 s | 87.1 MB | 39.447 ms | 1.000 |

Shards contain at most 65,536 vectors. Sharded query times include opening, mapping and
dropping every shard; monolithic times exclude its initial mapping. The one-million
serialized indexes occupy approximately 1.173 GB in either layout. File-backed pages
may also occupy the OS cache; low process RSS does not imply negligible system memory.
These single runs do not establish sustained performance under battery, thermal or
filesystem contention. Construction time excludes embedding generation.

Ten-thousand isotropic vectors reached recall 0.515 / 0.740 / 0.910 / 0.970 at expansion
128 / 256 / 512 / 1024. Easy clustered results should therefore not be generalized to
every corpus. On the small native model fixture (12 documents, eight queries), semantic
top-1 was 6/8, top-3 8/8, and ANN recall@3 1.000. Broader relevance evaluation remains.

Reproduction:

```sh
cargo build --release --manifest-path bench/ann-probe/Cargo.toml
bench/ann-probe/target/release/blindspot-ann-probe 100000 clustered
bench/ann-probe/target/release/blindspot-ann-probe 1000000 clustered 65536
make app test-semantic
BLINDSPOT_SEMANTIC_WORKER="$PWD/build/Blindspot.app/Contents/Helpers/blindspot-semantic" \
  cargo test --manifest-path core/Cargo.toml \
  semantic::tests::native_helper_embeds_through_socket_transport -- --ignored
```

Raw run logs: `/tmp/blindspot-ann-1m-clustered.log`,
`/tmp/blindspot-ann-1m-sharded.log`, `/tmp/blindspot-ann-100k-sharded.log`,
`/tmp/blindspot-ann-native-fixture.log`. No ten-million-vector benchmark has run.

## Integration constraints

### Authoritative embedding identities

Schema 5 gives each embedding its own SQLite AUTOINCREMENT primary key, independent of
the document ID. Replacing an embedding atomically deletes the prior row and inserts a
fresh identity; failed insertion rolls back the deletion. A derived index returns these
embedding keys. `content/vectors.rs` resolves at most 100 candidates through SQLite,
checking current document revision, model and dimensions and validating returned paths.
Obsolete keys cannot resolve after updates, deletion, erasure or database reopen. Erasure
retains only the non-private sequence watermark, so later embeddings cannot alias old keys.

Vector export uses primary-key pages of at most 64, bounded blobs and finite unit-vector
validation. Invalid vectors never reach the native index builder. Migration from schema 4
copies existing vectors transactionally into the new table; a reproduced table-name
conflict leaves the old schema and vector bytes intact. Migration needs temporary disk
space proportional to the stored embeddings. It must run on the indexing worker.

The same 1K native embedding benchmark after schema 5 measured **7,518.539 ms** fresh,
**0.538 ms** unchanged and **773.955 ms** for 100 replacements, with all counts and
integrity checks passing. Earlier schema-4 numbers are in the table above; these single
runs do not establish a significant performance change. Log:
`/tmp/blindspot-embedding-v5-1k.log`.

### Isolated vector worker

`helpers/vector-worker` builds into `Contents/Helpers/blindspot-vectors`. The signing target
signs it explicitly before the bundle. Its versioned JSON protocol supports Begin, Add,
Finish, Reset and Query. No command accepts shell text or an arbitrary filesystem path.
The parent must launch it in a private cache directory. Artifact tokens are exactly 32
lowercase hexadecimal characters; graph files are created exclusively with mode 0600.

The worker opens files with no-follow flags, checks regular-file type/size/link count, and
passes the anchored descriptor through macOS's documented `/dev/fd` interface to USearch.
Queries verify SHA-256 before native parsing. At most 256 verified fingerprints are cached;
inode, size, mtime or ctime changes require re-verification. Tests alter graph bytes and
restore mtime: the checksum still rejects the file. Native header checks bound dimensions,
record count and format. A failed shard is reported while healthy shards remain searchable.
Native crashes remain confined to the helper process; application search integration remains
unfinished.

Limits: 2 MiB request frame, 64 vectors per Add, 65,536 vectors per shard, 2,048 dimensions,
384 MiB serialized shard, 256 shard references and 100 returned candidates. Queries map and
drop one shard at a time. Those are resource ceilings, not promises of interactive latency
at every ceiling. The earlier ANN benchmarks exclude protocol and checksum overhead.

`bench/VectorWorkerTests.py` passes against the actual bundled helper: build/reopen/query,
private files, checksums, corrupted-shard fallback, traversal/symlink/hard-link rejection,
strict malformed-command handling and oversized/truncated frames. A test reproduced serde
accepting extra fields on unit command variants; strict empty-object variants fix that.
The tool sandbox denies the native descriptor interface, so this harness needs native
filesystem access. It uses only temporary synthetic data, never a personal content index.

The helper has its own lockfile (USearch 2.26.2, NumKong 7.8.2, SHA-2 0.10.9). Dependency
notices must be included before final redistribution. No custom third-party extension
setup or untrusted in-process plugin mechanism is introduced.

```sh
make app test-vectors
```

### Core transport and bounded shard assembly

`semantic/vectors.rs` now provides the typed core client. It reuses the embedding transport's
socket deadlines, cancellation checks and child cleanup. Build counts, tokens, checksums,
file sizes, candidate IDs/distances and unavailable-shard counts are validated. Unexpected
or malformed responses close the worker. Ordinary requests have two-second deadlines,
cold startup allows five seconds, and final file flush/checksum allows ten seconds. These
operations belong on background workers. Cancellation kills the owned helper and a later
request can start a new one.

`semantic/indexing.rs::build_shard` counts a bounded key range, exports at most 64 vectors
per page and sends each page to the helper. It pauses 10 ms between batches. No database
transaction remains open during native construction. A changed row count refuses final
completion. Empty ranges produce no artifact. The native integration test cancels after
the first page, verifies no shard file exists, then builds successfully and resolves query
keys through SQLite. A separate test leaves a graph stale after a document edit and proves
that the old key cannot become a current result.

`core/examples/vector_roundtrip.rs` measures the complete pipeline using synthetic clustered
512-dimensional vectors: SQLite preparation, paginated export, IPC, native construction,
file checksums, worker restart, queries and SQLite candidate resolution. Its query checks
require an indexed vector to retrieve its own document; this is not broad semantic recall
evaluation. At 10K records, preparation took **536.559 ms**, shard construction **4,622.690 ms**,
first query **23.869 ms**, and twenty warm queries had median **0.566 ms**, maximum **0.668 ms**.
The first query includes process startup and checksum verification. Embedding inference is
excluded. Log: `/tmp/blindspot-vector-roundtrip-10k.log`.

At 100K records, preparation took **5,374.297 ms** and two-shard construction took
**44,735.997 ms**, producing **117,260,788 bytes** of graph files. First query was
**195.723 ms**; warm-query median **2.613 ms**, maximum **2.972 ms**. All query and integrity
assertions passed. Log: `/tmp/blindspot-vector-roundtrip-100k.log`. These runs did not
overlap compilation. Cold checksum/startup cost must not delay conventional results;
the application needs an independent semantic query lifetime. These measurements do not
include sentence-model inference or UI rendering and are not comparable as an optimization
claim against the standalone ANN probe's different query workload.

```sh
MACOSX_DEPLOYMENT_TARGET=26.0 cargo build --release --manifest-path core/Cargo.toml \
  --example vector_roundtrip
target/release/examples/vector_roundtrip 10000 \
  "$PWD/build/Blindspot.app/Contents/Helpers/blindspot-vectors"
```

The cache design keeps the shard catalog in the same SQLite database as
the authoritative embeddings. Publication must validate the current key range in the
same transaction. Queries should read catalog and candidate metadata through the same
database connection, preventing an external old catalog from being paired with a recreated
database. Graph files remain disposable, private artifacts. Erasure must stop helper work,
clear catalog metadata and delete both published and orphaned graph files before reporting
success. The lifecycle implementation is described below; application scheduling remains unfinished.

SQLite remains authoritative for document identity, revisions and embeddings. ANN files
must be disposable derived data, with atomic publication, model-version validation,
bounded reconstruction and stale/deleted result validation against SQLite. The measured
memory cost rules out rebuilding a monolithic million-vector graph in the launcher.
Bounded shards warrant further integration testing; ten million vectors could make
searching every shard too slow, so that scale still needs routing or another approach.

Before exposing semantic results, integration must connect the incremental embedding pass
to scheduling, resource policy, cancellation, artifact erasure, crash recovery, query-time scope checks
and lexical fallback. Native ANN parsing should be isolated from launcher failures.
The extension/distribution ecosystem remains outside this iteration's requested scope.

## Cache lifecycle implementation (schema 6)

`content::vectors` now persists a bounded shard catalog in the same SQLite database as
its authoritative embedding keys. Publication takes an immediate transaction and verifies
current model, dimensions, record count and highest key across the whole fixed shard range.
A document edit, deletion or vector replacement during construction therefore cannot publish
an obsolete graph. Failed migration/publication preserves the previous state. Reads reject
malformed metadata and more than 256 catalog rows. Sparse key-range discovery skips retired
IDs, including after erasure, rather than iterating from zero through the lifetime sequence.

`semantic::indexing::maintain_cache` builds and publishes missing shards, verifies existing
checksums through the isolated vector worker before reuse, rebuilds corrupt artifacts, and
prunes orphaned files. Work remains cancellable between bounded exports and during helper
requests. The native regression covers cancellation/retry, reopening, deliberate corruption,
updated embeddings and cleanup. Only the serialized index owner should invoke maintenance
and pruning; query owners must use their own connection and refresh the catalog per request.

`semantic::cache` creates the private local `content-vectors` sibling directory only when
requested. Cleanup uses an owned directory descriptor and `unlinkat`, accepts only opaque
32-hex `.ann` filenames, never traverses a symlink and preserves unrelated files. Private
ownership/mode and local-volume checks reject unsafe cache directories. Erase-index now
clears the SQLite catalog and both published/orphaned files, including when the database is
missing. Cleanup failure reports incomplete erasure and permits retry. This is logical data
removal, not a guarantee against filesystem snapshots or forensic recovery.

The maintenance pass is implemented and tested but **not yet scheduled by the application**.
Semantic settings, independent query lifetime, hybrid ranking, and launcher results still
need integration. Idle query helpers will need explicit closure during erasure when those
persistent owners are introduced; no such application-owned semantic helper exists yet.

The updated `vector_roundtrip` example measures this lifecycle. At 10K synthetic 512-dimension
vectors: preparation 568.375 ms, build/publication 3,992.980 ms, unchanged-cache validation
29.278 ms, first query 22.026 ms, warm median 0.589 ms (maximum 0.695 ms), erasure 809.638 ms.
One graph occupied 11,727,168 bytes; all retrieval, reuse, erasure and integrity assertions
passed. Log: `/tmp/blindspot-cache-roundtrip-10k.log`. No compilation overlapped the run.
Earlier build-only timings measured a different lifecycle; the variation is not evidence of
an optimization. Sentence inference, launcher rendering and broad relevance recall are excluded.

At 100K, the lifecycle run produced two shards totaling 117,260,788 bytes: preparation
5,472.753 ms, build/publication 38,882.565 ms, unchanged-cache validation 265.450 ms,
first query 202.223 ms, warm median 3.119 ms (maximum 3.614 ms), erasure 9,675.905 ms.
All assertions passed; no compilation overlapped the run. Log:
`/tmp/blindspot-cache-roundtrip-100k.log`. Cold retrieval still exceeds the conventional
first-result budget, and erasure must remain background/cancellable. The earlier 100K
warm median was 2.613 ms; these individual runs do not establish a statistically reliable
regression or improvement, but neither supports delaying conventional results for semantics.
