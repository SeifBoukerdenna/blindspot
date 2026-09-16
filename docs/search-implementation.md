# Search upgrade implementation — September 16, 2026

Unreleased working-tree changes, still version 0.2.8. Existing unrelated edits were preserved.
No commit, push, tag, release, live-index migration or installation was performed.

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

## Installation gate: existing FTS mismatch

The live database was opened read-only. A consistent `VACUUM INTO` snapshot was taken at:

`build/search-upgrade-rollback-20260916/before.sqlite`

The schema-7 snapshot has **14,522 documents and 14,452 legacy embeddings**. Migration on a copy
preserved both counts, but the external-content FTS integrity check failed:

`fts5: checksum mismatch for table "content_fts"`

Checking an independent pre-migration copy reproduced the same mismatch **before migration**.
This is not evidence of lost source documents; it means the derived FTS index and document table
do not agree. The live index has not been repaired or migrated. The repeated-failure stop in
`verify-loop` leaves installation paused rather than treating preserved row counts as integrity.

Next: diagnose/rebuild only the derived FTS on a disposable copy, verify document/vector contents
and FTS integrity, then agree the live repair/installation procedure with the user. Do not erase
the database or silently treat this as a successful migration. Keep the original snapshot.

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
- Recheck artifacts against final source, signature and header before installing; the old
  installed app is unchanged. No release packaging/publication was performed.
