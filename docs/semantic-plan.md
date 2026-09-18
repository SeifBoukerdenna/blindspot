# Searching inside files: plan

> **Historical record.** The implementation status, measurements and release instructions below
> describe their recorded revision, not the current app. See the [documentation index](README.md),
> [current feature guide](new-features.md) and [current release workflow](releasing.md).

A plan for making content search work on code, PDFs, slides and notes, replacing the current
one-vector-per-file design. Written 2026-09-15 against the installed 0.2.8 index.

## 1. Where we are, measured

| Fact | Value | Consequence |
|---|---|---|
| Embedded text per file | First **1,200 bytes**, one vector (`semantic/indexing.rs::passage`) | Meaning-based search only sees a file's opening paragraph |
| Stored text per file | First **64 KB** (`MAX_BODY_BYTES`) | Exact search misses the rest of long files |
| Semantic eligibility | Extracted documents, `.txt`, `.md`, `.rst`, `.html` (`SEMANTIC_ELIGIBLE`) | **Code is excluded entirely** |
| Extraction | PDF, DOCX, DOC, RTF, ODT | No PowerPoint, Keynote, Pages, Numbers, EPUB; no OCR |
| Live index | 14,503 documents, 768 MB, 14,456 vectors | — |
| Index composition | 8,211 JSON · 3,057 CSV · 1,117 PY · 456 MD · 388 JS · 333 RS · 201 HTML · 134 TXT · 117 PDF | Mostly machine data; 125 files have extracted document text |
| On disk to index | ~6,400 code and Markdown files, 22 repositories, 133 documents | Scope of the new index |
| Result shape | One row per file, "Related by meaning" tail capped at 8 | No way to reach the matching part of a long file |

**Why results feel iffy:** a 40-page PDF or a 2,000-line source file is represented by one vector
built from its first paragraph, and code never gets a vector at all. Ranking then puts exact
matches first and appends a few semantic ones, so a good match that shares no words with the query
lands at the bottom, or nowhere.

## 2. Goal

Type a phrase describing what you remember, and get the exact passage: the page of the PDF, the
slide of the deck, the function in the source file. Local, on this Mac, fast enough to feel like
search rather than a query.

**Non-goals:** cloud services, a general code intelligence server, indexing every byte of machine
data, or replacing `>docs` (which becomes a consumer of the same retrieval).

## 3. Decisions taken

| Decision | Choice |
|---|---|
| Searchable by meaning | Code, PDFs/Word/PowerPoint/Keynote, notes and text. **Not** JSON/CSV/logs, which stay exact-text only |
| Result shape | Passage rows that open at the page or line; several rows may come from one file |
| Embeddings | An Ollama embedding model by default, Apple's on-device model as the automatic fallback |
| Budget | Balanced: roughly 1–3 GB of index, compressed vectors, first full pass in tens of minutes |
| OCR | Scanned PDF pages only |
| Code scope | Only folders chosen in Settings; `.gitignore` is **not** applied, so untracked scratch files stay findable |
| Migration | Build the new index beside the old one; swap when complete |
| Ranking | One blended list, each row saying why it matched |

## 4. Design

### 4.1 Extraction: text with locations

Every file kind produces `(text, location)` pairs, where location is what the UI opens:

| Kind | Producer | Location |
|---|---|---|
| Text, Markdown, HTML | Core, UTF-8 read | Line number |
| Code | Core, UTF-8 read | Line number |
| PDF | `blindspot-extract` (PDFKit), OCR fallback per page | Page number |
| DOCX, RTF, ODT, Pages | `blindspot-extract` (NSAttributedString) | Paragraph ordinal |
| PPTX, Keynote | `blindspot-extract` (new) | Slide number |
| XLSX, Numbers, CSV | `blindspot-extract` (new), exact-text only | Sheet and row range |

- The helper keeps its no-network sandbox and per-file byte and page caps.
- OCR runs only when a PDF page yields no text, using the same Vision recogniser as clipboard
  images and Copy Text from Screen; the result is marked so the UI can say "read by OCR".
- Extraction failures keep their existing states (no text, locked, oversized, unreadable).

### 4.2 Chunking

One vector per file is the core defect. Chunks are the unit of storage, search and display.

- **Prose (notes, PDFs, Office):** ~1,000 characters with ~15% overlap, split at paragraph and
  heading boundaries. Each chunk is prefixed with its heading breadcrumb ("Renewal proposal ›
  What changes"), which measurably helps both search and the model.
- **Code:** split at top-level declarations with a per-language brace or indentation heuristic,
  target 40–80 lines, never mid-function where avoidable. Each chunk is prefixed with the
  repository-relative path and enclosing symbol ("core/src/content.rs › fn search_filtered").
  Identifiers are also stored split (`searchFiltered` → `search filtered`) so exact search finds
  them however they were typed.
- **Slides and sheets:** one chunk per slide or per sheet section, never merged across boundaries.
- Chunk text is stored, so results can show the matching passage without re-reading the file.
- **Very large files are capped by both bytes and chunks:** a per-file byte ceiling (raised well
  above today's 64 KB) and a per-file chunk ceiling, whichever is reached first. A 50 MB log or a
  5,000-page PDF is indexed up to its cap and marked "partly indexed" in the Index page, so the
  dashboard never implies a file was read in full when it was not.

Estimated for the current disk: ~70,000 chunks from chosen code folders, notes and documents.

### 4.3 Embeddings

- **Default model:** an Ollama embedding model, `embeddinggemma:300m` unless measured otherwise
  (768 dimensions, strong multilingual and code performance, small enough to keep resident).
  Alternatives to measure: `qwen3-embedding:0.6b` (1024 d, stronger, slower), `nomic-embed-text`
  (768 d, long context, weaker on French).
- **Fallback:** Apple's `NLContextualEmbedding`, as today, when Ollama is not running. Vectors
  record their model, so the two never mix: the model key already lives in the schema and in the
  shard catalogue.
- **Batching:** 32–64 chunks per request against `/api/embed`, bounded queue, cancellation, and the
  same loopback-only validation the agent uses.
- **Storage:** int8-quantized vectors with a per-vector scale (4× smaller than float32, ~1% recall
  cost at this scale), full precision kept only in the ANN shards.
- **Model changes:** changing the model in Settings marks vectors stale and re-embeds in the
  background; search keeps working on the old model's shards until the new ones are published.

### 4.4 Retrieval and ranking

Two stages, then an optional third:

1. **Candidates**, in parallel:
   - BM25 over chunk text (FTS5), plus a boost when the query matches the file name or symbol;
   - vector search over chunk embeddings (USearch shards plus the exact delta for new chunks);
   - literal path and file-name matches, as today.
2. **Fusion:** reciprocal-rank fusion over the three lists with tuned weights, then signals:
   exact-phrase bonus, recency, same-repository proximity, kind preference, and **per-document
   diversity** so one huge file cannot fill the list. Each row records why it matched
   (exact words, meaning, file name) and the UI shows that.
3. **Rerank (later, `>docs` only):** re-score the top ~20 passages with the local model when one
   is loaded, for answer quality rather than browse speed.

### 4.5 Results and actions

- A row is a passage: title is the file, subtitle is the matching text with highlights, and the
  rail shows the location ("p. 12", "line 340", "slide 4").
- Return opens at that location. For code, the first of these that exists is used, so nothing has
  to be installed or configured:
  1. **Zed**, through the CLI inside its own bundle (`/Applications/Zed.app/Contents/MacOS/cli
     path:line`), which is present on this Mac, so no PATH setup is needed.
  2. Another editor already installed that takes a line: VS Code (`code -g path:line`), Cursor,
     Sublime, BBEdit.
  3. **iTerm2**, opening a window that runs `$EDITOR` at the line (`nvim +340 path` here), through
     Apple Events, which macOS asks about once.
  4. The default application for the file, without a line.
  PDFs open at the page in Preview, slides at the slide where the app supports it.
- ⌘K adds: copy passage, copy file path, reveal, ask the documents about this passage.
- ⌘Y preview scrolls to the passage.

### 4.6 Settings

- **Code folders:** the list of folders whose code is indexed (empty by default).
- **Embedding model:** Ollama model name, with a "check" button that reports dimensions and speed;
  Apple fallback is automatic.
- **OCR scanned pages:** on or off, with a page budget.
- **Index budget:** target size, with the dashboard showing what is used and what would be freed.

## 5. Schema and migration

Schema v8, built in a **new database file** beside the current one:

```
documents(id, identity, root, path, title, kind, language, modified_ns, bytes, extraction, revision, seen, changed_ns)
chunks(id, document_id, ordinal, location, text, symbols, bytes, hash)
chunks_fts(text, symbols)                      -- FTS5, external content over chunks
embeddings(chunk_id, model, dimensions, revision, vector, scale)
vector_shards(after_key, through_key, model, dimensions, token, checksum, count, bytes)
```

- `documents.body` disappears; chunk text replaces it, so nothing is truncated at 64 KB.
- Vectors key on `chunk_id`, keeping the existing 65,536-wide shard ranges and reuse logic.
- **Migration:** the new database is built by a normal indexing pass while the old one keeps
  serving search. When the pass completes and a smoke query set passes, the two swap atomically and
  the old file is deleted. Peak disk is the sum of both, which the Index page shows before starting.
- Nothing user-owned is touched: config, overrides, clipboard history and shortcuts are separate
  files.

## 6. Budgets and targets

| Measure | Target |
|---|---|
| Index size | ≤ 3 GB for the chosen scope (estimate: 400–600 MB) |
| Query latency, warm | p50 ≤ 60 ms, p95 ≤ 120 ms for a blended search |
| Embedding throughput | ≥ 1,000 chunks/minute with Ollama on this Mac |
| First full pass | ≤ 30 minutes for ~70,000 chunks |
| Indexing memory | ≤ 400 MB resident, including helpers |
| Idle CPU | 0% when no file has changed (as today) |

## 7. How we will know it worked

- **A golden set in the repository** (`bench/search-queries.toml`): 40–60 real queries, each with
  the file and location that should come first ("where did we decide the signalling protocol",
  "the function that rebuilds vector shards", "Northwind renewal deadline"). Half are phrased with
  words the file does not contain. **Queries and paths only, never file contents**, and paths are
  written relative to the home folder so the set says nothing about what the files hold.
- **Metrics:** recall@5, MRR@10 and p95 latency, before and after each phase.
- **A harness:** `make bench-search` runs the set against a copy of the index and prints a table;
  regressions fail. Kept out of CI (it needs a real index) but run for every phase.

## 7a. Baseline, measured 2026-09-15

`make bench-search` against the live 0.2.8 index, 47 queries (21 whose words are in the file, 26
phrased without them):

| Search | hit@1 | hit@5 | hit@10 | MRR@10 | median | p95 |
|---|---|---|---|---|---|---|
| Exact only, words | 50% | 50% | 55% | 0.50 | 0 ms | 1 ms |
| Exact only, meaning | 8% | 20% | 20% | 0.14 | 0 ms | 1 ms |
| **Exact only, all** | **28%** | **34%** | **36%** | **0.31** | 0 ms | 1 ms |
| Exact + semantic, all | 19% | 32% | 34% | 0.25 | 9 ms | 10 ms |

Two findings, both explaining the feel:

- **Semantic search currently makes ranking worse**, not better: fusing it drops hit@1 from 28% to
  19%. One vector per file, built from its first 1,200 bytes, is noise at this corpus size.
- **Every query term must match.** `expression()` joins tokens with `AND`, so a natural-language
  query ("how do I publish a new release") matches nothing at all, however well the file fits.
  32 of 47 queries were not in the top five, most of them not found at all.

## 7c. P1 measured, 2026-09-15

Chunking alone, before any embedding work. Measured on a scratch index of this repository (125
documents, 1,859 passages, about 15 per document), exact search only, the same 47 queries:

| Search | hit@1 | hit@5 | hit@10 | MRR@10 | p95 |
|---|---|---|---|---|---|
| Documents (today's behaviour) | 32% | 36% | 36% | 0.34 | 0 ms |
| **Passages (chunks)** | **38%** | **70%** | **81%** | **0.52** | 3 ms |
| Passages, meaning-phrased only | 30% | 67% | 78% | 0.45 | 3 ms |

Meaning-phrased queries went from 19% to 67% hit@5 **without a model**: most of what felt like a
semantic failure was a truncation and query problem. Two changes did it: passages instead of whole
documents, and falling back from all-terms to any-terms when the strict pass returns a thin page.

Caveats, so these are not read as more than they are:

- The scratch index covers this checkout, not the home folder, so the absolute numbers are not
  comparable with the 34% baseline in 7a. Document-versus-passage is comparable: identical content,
  identical queries.
- No extracted documents appeared in this corpus, so the page path is covered by unit tests rather
  than by this measurement.
- The query set and this plan quote every query, so the benchmark excludes both from scoring; it
  scored its own sources before that was fixed.

## 7b. Structure: a workspace and a pure crate

Decided 2026-09-15, before P1 landed:

- The repository is a Cargo workspace (`core`, `crates/retrieval`); the vector worker stays
  standalone, since it carries a C++ dependency nothing else links, and its licences come from its
  own lock.
- **`blindspot-retrieval`** holds retrieval decisions that are pure functions of text: chunking
  now, ranking and quantization as they land. No dependencies, so its tests run in a second
  (`make test-retrieval`) and ranking can be measured without an index.
- **Storage stays in `blindspot_core`**: chunks, FTS, embeddings and shards live in one SQLite
  database beside the documents, and splitting that across a crate boundary would cost more than
  it buys.
- The embedding transports (Ollama and the Apple helper) may become a second crate at P2, once
  their interface is real rather than imagined.

## 8. Phases

Each phase ships on its own and is measured against the golden set.

| Phase | Work | Expected effect |
|---|---|---|
| **P0** | Golden set and `make bench-search`, measuring today's index | A number to beat |
| **P1** | Chunk schema, chunk-level exact search, passage rows, open-at-location | Long files searchable and reachable, before any model work |
| **P2** | Ollama embedding service, per-chunk vectors, blended ranking, Apple fallback | The main quality jump |
| **P3** | Code-aware chunking and symbol search, code folders in Settings | Code search becomes usable |
| **P4** | PPTX, Keynote, Pages, Numbers extraction | Slides and spreadsheets searchable |
| **P5** | OCR for scanned PDF pages | Scanned documents searchable |
| **P6** | `>docs` on chunks, optional rerank | Better answers and sources |
| **P7** | Background rebuild, budget controls, dashboard for the new shape | Safe migration and visible costs |

## 9. Risks

| Risk | Mitigation |
|---|---|
| Ollama absent or slow | Apple fallback keeps search working; batching and a bounded queue keep the machine responsive |
| Index growth beyond budget | int8 vectors, chunk caps per file, kinds excluded by default, budget shown in Settings |
| Migration loses the working index | New database built beside the old; swap only after a smoke query set passes |
| Ranking regressions | Golden set per phase; weights tuned on it rather than on impressions |
| Code chunking by heuristics misplaces boundaries | Fall back to fixed-size windows; boundaries only affect where a passage starts, not whether it is found |
| Re-embedding on model change costs a full pass | Old shards keep serving; the pass is background and resumable |

## 10. Settled, and what is left

Settled on 2026-09-15:

- **Code passages open in Zed** when it is installed, then another installed editor, then iTerm2
  running `$EDITOR`, then the default application.
- **`.gitignore` is not applied** to chosen code folders; the existing dependency and build
  exclusions (`node_modules`, `target`, `venv`, `DerivedData`, …) still apply.
- **Large files are capped by both bytes and chunks**, and shown as partly indexed.
- **The golden query set lives in the repository**, as queries and relative paths only.

Left to decide while building:

- The embedding model, chosen by measurement on the golden set: `embeddinggemma:300m` against
  `qwen3-embedding:0.6b` and `nomic-embed-text`.
- Whether chunk overlap of 15% earns its storage on this corpus.
- Whether the `>docs` rerank is worth the extra latency once blended ranking exists.
