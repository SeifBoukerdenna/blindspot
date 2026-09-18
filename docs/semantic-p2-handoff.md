# P2 handoff — Ollama embeddings and passage vectors

> **Historical record.** The implementation status, measurements and release instructions below
> describe their recorded revision, not the current app. See the [documentation index](README.md),
> [current feature guide](new-features.md) and [current release workflow](releasing.md).

**Superseded implementation status, September 16:** see `docs/search-implementation.md`.
The passage pipeline is now wired and measured, but installation is paused on a pre-existing
FTS checksum mismatch reproduced in the pre-upgrade snapshot. The remainder below is historical.

Written 2026-09-15 for whoever picks this up next, including a fresh agent with no context.
Everything below is uncommitted working-tree state. **Read `docs/semantic-plan.md` first** for the
overall design; this file covers only what P2 has and has not done.

## Status in one line

P2's three built slices — transport, schema, pass — are **done and green**. What remains is wiring
the pass into the background service, a Settings row for the model, and blended ranking.

## Standing constraints (do not violate)

- **Never commit, push, tag or release.** Seif does all of those himself, after manual testing.
  Hand him the command; do not run it.
- Do not rewrite `~/.config/blindspot/config.toml`, and never erase user state as a migration
  fallback. Do not use the live index for destructive tests.
- **No automatic model downloads.** If a model is needed, ask the user to run `ollama pull …`.
- Runtime processing stays local; loopback only; no telemetry, no cloud.
- Codex also edits this tree between sessions. Check `git status` and mtimes before assuming a
  modified file is yours.
- The live index at `~/.local/share/blindspot/content.sqlite` (note: **no `3` suffix**) is the
  user's real data — schema 7, ~14,510 documents, 768 MB. It has **not** been migrated by this
  work and must not be experimented on. Read it with `?mode=ro&immutable=1` if you must.

## What is done and verified

### 1. Ollama embedding transport — `core/src/semantic/ollama.rs` (new)

Mirrors the Apple helper's interface (`probe() -> Model`, `embed(&[String]) -> Batch`, same
`Failure` values) so the two are interchangeable, with Apple as the automatic fallback. Posts to
`/api/embed` through the agent's existing loopback client (`crate::agent::http::post_ndjson`) —
a single JSON object arrives as one NDJSON line. `/api/embeddings` is the older endpoint and is
deliberately not used.

Refuses, each with a test: vector count/width/finiteness mismatches, a model whose width changes
mid-run, non-loopback hosts, cancelled callers (no request is sent), malformed callers (empty,
blank, oversize, over-batch) before any connection, and tells a missing model (`ModelUnavailable`)
apart from Ollama not running (`Unavailable`, so Apple takes over silently).

Vectors can never mix between backends: `indexing::model_key` keys on identifier/revision/
dimensions, and an Ollama model's identifier is its own name. There is a test asserting the keys
differ. `BATCH = 32`, `REVISION = 1`.

**6 tests, all passing.**

### 2. Schema v9 — `core/src/content.rs`

```sql
CREATE TABLE chunk_embeddings(
 id INTEGER PRIMARY KEY AUTOINCREMENT,          -- monotonic, keys shard ranges like embeddings.id
 chunk_id INTEGER NOT NULL UNIQUE REFERENCES chunks(id) ON DELETE CASCADE,
 model TEXT NOT NULL, dimensions INTEGER NOT NULL,
 vector BLOB NOT NULL CHECK(length(vector)=dimensions*4));
```

- `SCHEMA_VERSION` 8 → 9; ladder step `if version < 9` is additive, `IF NOT EXISTS` throughout.
  Document vectors and published shards keep serving semantic search untouched.
- **No revision column, deliberately.** `write_chunks` deletes and re-inserts a document's chunks
  on every rewrite, `chunks.id` is `AUTOINCREMENT`, and `PRAGMA foreign_keys=ON` is set at open
  (`content.rs`, the pragma line in `open`), so a vector cannot outlive its text. There is a test
  for this; if anyone ever drops that pragma, stale vectors appear silently.
- `CHUNK_ELIGIBLE` (+ Rust mirror `chunk_eligible`) is **wider than `SEMANTIC_ELIGIBLE`**: it
  admits code and extracted documents, excluding only `json csv tsv xml yaml yml toml`. This is a
  real behaviour change — whole-file code vectors previously ranked as noise, but a passage is one
  declaration with its symbols. If the golden set shows code chunks ranking badly, narrow this one
  constant.
- `chunk_embedding_page(after, model, dimensions, cancel)` mirrors `embedding_page`: pages by chunk
  id, 128/page, returns text only when no *current* vector exists (so a resumed pass repeats
  nothing), 250 ms progress-handler deadline, `validated_path` rejection.
- `put_chunk_embedding(chunk_id, model, vector)` mirrors `put_embedding`: L2-normalizes before
  storing (readers assert unit norm), returns `false` — not an error — if the chunk was rewritten.
- Erase-all now clears `chunk_embeddings` alongside `embeddings` and `vector_shards`.

**5 tests, all passing**, including `a_version_eight_index_gains_chunk_vectors_without_losing_anything`.

## 3. The pass — `core/src/semantic/indexing.rs` (done)

- `Embedder` enum in `core/src/semantic.rs` (after `impl Client`, before `struct Connection`)
  wraps `Client` (Apple) and `ollama::Client` behind `probe`/`embed`/`close`/`batch()`.
  `batch()` returns **8** for Apple and **32** for Ollama — Apple's helper refuses more than eight
  texts, so a pass that assumed 32 would see every batch fail as `InvalidInput`.
  `Client::BATCH = 8` was added for this.
- `run_chunks(store, embedder, cancel, eligible, report)` mirrors `run`: probe → `model_key` →
  `chunk_embedding_page` loop → eligibility filter → batch at `embedder.batch()` → on
  `EmbeddingUnavailable | InvalidInput` retry one passage at a time → `persist_chunks`.
  It embeds `chunk.text` **verbatim** and must never call `passage()`, whose 1,200-byte cap is for
  whole documents and would truncate a passage's tail.
- `persist_chunks` beside `persist`: verifies `batch.model` and vector count, then counts
  `written` / `stale` from `put_chunk_embedding`.

**Tests: 4 of 4 passing**, in `mod pass_tests` at the end of `indexing.rs`. They drive a real pass
against a multi-request loopback fixture `serve(dimensions, refuse)` and need no installed model:
a full pass with resumption (asserting a settled index sends only the probe), a refused batch
degrading to one-passage-at-a-time retries, exclusion plus cancellation, and each backend's
declared batch size.

### Post-mortem: two defects these tests caught

Recorded because each was a real defect rather than a test typo, and the second would have shipped
silently. Both are fixed; the tests are green.

*First (fixture):* three of the four tests failed with `Worker(InvalidResponse)` at the
`.expect("a pass")` on the first `run_chunks` call. `serve()` derived its input count with
`body.matches("\",\"").count() + 1`, counting separators *anywhere* in the body — and the probe
`{"model":"m","input":["blindspot"]}` already contains `","` inside `"model":"m","input"`, so a
one-text request was answered with two vectors and the client rightly refused the mismatch. The
fixture now counts the elements of `"input":[…]` itself. `ollama::Client`'s strictness is correct
and must not be relaxed to make a test pass.

*Second (transport — a real bug in shipped code, not in the test):* the remaining failure was
`Worker(ModelUnavailable)`. Ollama reports a
missing model **and** a rejected request through the same `error` field, and `ollama.rs` flattened
both to `ModelUnavailable` — which `run_chunks` does not catch, since its retry arm matches
`EmbeddingUnavailable | InvalidInput`. So a too-large batch aborted the whole pass instead of
retrying one passage at a time. The mapping now treats only "not found"/"try pulling" wording as
`ModelUnavailable` (fall back to Apple) and every other error as `EmbeddingUnavailable` (retry
smaller). A 404 remains `ModelUnavailable` regardless.

The `awaiting()` helper was never implicated. It exists because `ContentStore.connection` is private
outside `content.rs`, and builds its key via `model_key(Model{identifier, revision: 1, dimensions})`
— which must keep matching what the pass wrote under, or assertions would hold for the wrong reason.

## Verified state of everything else

- `cargo test -p blindspot_core`: **366 passed, 0 failed**, 7 ignored (those need the native
  vector helper).
- `cargo test -p blindspot-retrieval`: 7 passed.
- `cargo clippy --workspace --all-targets --locked`: clean, exit 0.
- `git diff --check`: clean.
- Live index untouched: schema 7, 14,510 documents, no `chunks` table.

## Environment facts worth not rediscovering

- **`timeout` does not exist on macOS.** Wrapping `cargo test` in it fails with
  `command not found` and, if you pipe through `grep`, produces silent empty output that looks
  like a hang. I lost a cycle to this. Also, `$?` after a pipeline reads the *last* command's
  status, not `cargo`'s.
- `embeddinggemma:300m` is installed, **768 dimensions**, and answers `/api/embed` on
  `127.0.0.1:11434` (Ollama 0.33.3). Warm throughput measured at **5,440 chunks/min** at batch 32
  (4,949 at batch 8) — 5.4× the plan's ≥1,000/min target; a 70k-chunk first pass ≈ 13 minutes.
- A PostToolUse hook runs rustfmt over the whole crate after every edit. It has reformatted ~1,000
  lines of hand-packed code in `semantic/{vectors,indexing,cache,search}.rs` and
  `content/vectors.rs` that nobody edited. **This is formatting only** — verified by reading the
  diffs and by mtimes all matching the minute of my own edits, not Codex. Note `git diff -w` does
  *not* prove this, because a line split into five still counts as five additions.
  Seif may want to `git checkout --` those five files before committing.
- The same hook reports `error: could not compile … due to N previous errors` for what are often
  just warnings promoted by `-D warnings`. Always re-run clippy yourself to see the real text.
- Anchors go stale constantly because of that formatter. Read a region immediately before editing
  it; never anchor on remembered text.
- `ContentStore` has **no** `remove_path`. The only public removal is
  `remove_unseen_under(&scan, path, cancel)`, which deletes what a fresh scan did not see.

## Next steps, in order

1. **Wire the pass into the service.** `content_service.rs` ~line 779 gates semantic work on
   `config.semantic && (total.semantic_updates > 0 || !semantic_clean)`, builds a
   `semantic::Client` from `request.helpers`, runs `indexing::run`, then `maintain_cache`.
   The chunk pass needs to hang off the same scheduler, choosing `Embedder::Ollama` when a model
   name is configured and Ollama answers, else `Embedder::Apple`.
2. **A Settings row for the embedding model** — `core/src/settings.rs`, beside `content.semantic`
   (~line 182). Ollama host already exists as `config.agent.host`, loopback-validated at load.
3. **Blended ranking**: passage BM25 + chunk vectors, per the plan's §4.4. `semantic/search.rs`
   has `fuse(lexical, semantic)` for documents today; passages need the equivalent with
   per-document diversity so one file cannot fill the list.
4. **Measure with `make bench-search`** against `bench/search-queries.toml`. Baseline to beat:
   P1 gave passages hit@5 70% / MRR 0.52 (documents: 36% / 0.34). Keep the output local — the
   query set contains the user's paths.
5. Update `docs/new-features.md` and the handoff sections of `CLAUDE.md` / `AGENTS.md` for v9.

## Files touched by P2

| File | State |
|---|---|
| `core/src/semantic/ollama.rs` | new, green |
| `core/src/semantic.rs` | `pub mod ollama;`, `Client::BATCH`, `Embedder` enum |
| `core/src/content.rs` | `SCHEMA_V9`, `CHUNK_ELIGIBLE`, `chunk_eligible`, `PendingChunk`, `ChunkEmbeddingPage`, `chunk_embedding_page`, `put_chunk_embedding`, v9 ladder step, erase sweep, 5 tests |
| `core/src/semantic/indexing.rs` | `run_chunks`, `persist_chunks`, `mod pass_tests` (4 passing) |

Nothing is committed, and nothing here is wired into the running app yet: the pass exists and is
tested, but no scheduler calls it, so building and installing today would migrate the index to v9
and embed nothing. The commit is Seif's to make once he has tested manually.
