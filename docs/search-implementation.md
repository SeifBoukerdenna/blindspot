# Search upgrade implementation — September 16, 2026

Version **0.2.9** is now signed, installed at `~/Applications/Blindspot.app`, and running.
The user created the existing `v0.2.9` release tag; the agent did not commit, push or change it.
The local follow-up changes below remain uncommitted. Existing user configuration was preserved.

## Implemented

- Schema 10: additive passage backfill, parallel model generations, immutable chunk identities,
  int8 vectors with per-vector scale, and passage shard catalogs. Legacy documents and vectors
  are retained. Backfill revisits unchanged metadata; stored passages can extend beyond 64 KiB.
- Production content search and `>docs` share chunk FTS and passage vectors, deterministic
  reciprocal-rank fusion, and at most two substantially distinct passages per file. Ordinary
  launcher content rows remain one per file. Lexical results can arrive before semantics.
- Ollama passage indexing is wired into the worker, using installed embeddinggemma by default
  and Apple fallback at indexing time. Model names/dimensions are isolated. Failed individual
  passages stay word-only; a completed pass with usable vectors can activate.
- Code-folder selection, embedding-model setting, OCR opt-in/page budget, and a storage target.
  JSON/CSV/config and XLSX remain word-only. No automatic downloads or indexing-scope expansion.
- PDF page breaks preserve real page numbers. Blank/scanned pages can use local Vision OCR;
  recognized passages are marked, and capped documents are counted as partial.
- PPTX presentation order and XLSX cached values via the system archive library and bounded XML
  parsing in the no-network helper. No archive extraction to disk, formulas, macros, external
  relationships or XML DTD/entity expansion. Sheet boundaries are preserved.
- PDF Return/preview opens the indexed page inside Blindspot with background loading and a
  stale-result guard. Code lines use installed editor actions. Copy/Ask Passage reads the exact
  immutable chunk on a worker and refuses obsolete or out-of-scope rows. Document-answer source
  rows carry PDF page/code line metadata through the ABI.
- Index dashboard reports passage counts, active-generation vectors, partial documents, actual
  vector-file bytes, and the storage target. Compact retires inactive completed non-Apple models.
- A production-path quality harness, scratch-index builder, read-only inspector/snapshot tool,
  and native Office/PDF tests. The complete user guide is `docs/new-features.md`.

## Validation observed

- `cargo test --workspace --locked --quiet`: **371 core + 9 retrieval passed**, 7 native tests
  ignored. Initial sandbox failures were local-socket/metadata access restrictions; rerun with
  those permissions passed. Subsequent affected agent tests: 12 passed; ABI tests: 44 passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `make app`, `make test-actions`: passed. Native fixture checks passed for PDF page navigation,
  cancellation, OCR disabled/enabled/page budget, and eight Office safety/extraction cases.
- Native passage-service test and PDF indexing test passed on disposable fixtures.
- 47 golden queries against a disposable checkout index: 134 documents / 1,975 passage vectors,
  embeddinggemma 768 dimensions. Passage words: hit@5 **66%**, MRR@10 **0.49**; blended:
  **89%**, **0.66**, p95 **37 ms**. Words and meaning subsets improved; `--check` passed.
  This is file relevance on a small code checkout, not held-out document/location or scale proof.
- `git diff --check`: passed. Developer ID signing works outside the tool sandbox.
- Follow-up: 30 dashboard fixture states passed across dark/light palettes, including action
  callbacks, stale-state clearing and layout bounds; six PNGs rendered. The earlier assertion
  failure was caused by missing placement constraints in the fixture, now fixed.
- Rebuilt 0.2.9, explicitly signed its helpers and bundle, and verified the installed app with
  `codesign --verify --deep --strict`. Verified version 0.2.9 and its running process.
- Opened the installed Settings → Index page: real passage backfill, worker status, storage and
  partial-document warnings were visible and updating. Full embedding completion is pending.

## Resolved installation gate: existing FTS mismatch

The live database was opened read-only. A consistent `VACUUM INTO` snapshot was taken at:

`build/search-upgrade-rollback-20260916/before.sqlite`

The schema-7 snapshot has **14,522 documents and 14,452 legacy embeddings**. Migration on a copy
preserved both counts, but the external-content FTS integrity check failed:

`fts5: checksum mismatch for table "content_fts"`

Checking an independent pre-migration copy reproduced the same mismatch **before migration**.
This was a mismatch between derived FTS postings and the document table; the internal FTS
structure check passed. A copy-only rebuild fixed it. The historical cause has not been established.

After user authorization, the old app was stopped and a fresh snapshot was repaired and migrated
to schema 10. SQLite, foreign-key and both FTS integrity checks passed. Exact original-column
comparisons confirmed all five authoritative tables unchanged, including **14,527 documents and
14,454 legacy embeddings**. The validated copy replaced the live database before installing 0.2.9.
No full erasure was needed. The app is now backfilling passages from the existing selected roots.

Rollback material is in `build/rollback-0.2.9-20260916/` (private directory):

- `Blindspot-before.zip`: previous installed app; archive integrity checked.
- `index/before.sqlite`: consistent pre-repair snapshot.
- `index/original-live.sqlite`, with its matching WAL/SHM: original live database moved aside.
- `index/migration-check.sqlite`: validated repaired/migrated copy used for installation.

Never restore a database while Blindspot is running, or mix its WAL files with another database.
Configuration, overrides, clipboard, history and existing vector-cache files were left untouched.

## Remaining roadmap work / limitations

- This is not completion of every item in the original roadmap. Native iWork extraction,
  typed Word paragraph/XLSX row-range navigation and Office deep links remain unimplemented.
  Word/workbooks open whole-file; worksheet/row/cell labels are searchable text.
- No Settings model-check button or digest-pinned Ollama model identity yet. Refresh after
  backend availability changes; automatic timed retry/switching without filesystem work remains.
- Explicit kind/size/date filters use exact scoped semantic scoring for up to 2,048 candidates.
  Larger scopes use bounded ANN candidates, then authoritative filtering; filtered recall is
  not exhaustive. Missing/corrupt shards degrade to text and the bounded delta until Refresh
  rebuilds them. Scope and deleted-chunk validation still prevent stale/private result leakage.
- Eight model-generation slots are bounded; Compact currently retires completed inactive
  non-Apple generations, not abandoned incomplete generations. Further lifecycle cleanup needed.
- Literal filename search remains its existing launcher branch, not a third passage-fusion list.
  No additional recency/repository-proximity reranker or optional generative reranker was added.
- Large-library, multilingual, held-out PDF/Office relevance and combined helper/Ollama memory
  budgets remain unmeasured. Disk target is cooperative, not a hard quota. No full live panel
  smoke test was run because its current harness initializes the real state directory.
- The local installer now explains that invalid signatures require `make install`, which builds
  and signs first. No release packaging/publication was performed by the agent.
