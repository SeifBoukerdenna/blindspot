# Blindspot — complete feature guide and workflows

**Release: 0.2.9.** This guide describes implemented features, not the full proposed roadmap.
The release notes at the end distinguish new behavior from platform limitations.

**Local search upgrade, September 16 (unreleased):** passage embeddings, blended ranking,
code folders, PPTX/XLSX extraction, opt-in scanned-PDF OCR, PDF page previews, and Copy/Ask
Passage. The release version is unchanged; no tag or publication was made.

## 1. Open, navigate, and run actions

| Shortcut | Behavior |
|---|---|
| Your launcher shortcut | Opens/closes Blindspot; default is **⌘⇧Space**, configurable in General |
| Configured Agent shortcut | Opens Agent mode directly, if configured |
| ↑ / ↓ or Tab / ⇧Tab | Select a result; Tab accepts a selected command completion |
| Return | Runs the selected result's default action |
| ⌘K | Opens the selected result's action menu |
| ⌘Y | Quick Look for supported file results |
| ⌘Return | Reveal a file/app/executable where offered; copies an Agent command |
| ⌥Return | Copy path where offered; copies an Agent command |
| ⌃Return | Quit an app or request process termination where offered |
| ⌘1 / ⌘2 / ⌘3 / ⌘4 | Apps/mixed search, Files, Clipboard, Agent |
| ⌘M | Local model chooser in explicit Agent mode |
| ⌘, | Opens Settings while interacting with Blindspot |
| ⌘⇧, | Opens Blindspot Settings globally, subject to macOS shortcut registration |
| Escape | Cancels current work or closes the launcher/preview |

**Settings fallback:** use the Blindspot menu-bar item, search
`:settings hotkey` and press Return, or reopen `~/Applications/Blindspot.app` in Finder.
If another global shortcut conflicts, use one of these fallbacks.

Default actions: apps/files open; clipboard items copy and restore focus; process rows
open Inspect; URLs open; calculator/converter results copy; setting rows open that setting;
command rows complete the command; Agent prompt rows submit only after Return.

## 2. Find apps and files

Normal text searches apps and filenames together. Results can arrive in stages.
App ranking includes match quality and local launch history. Empty search offers suggestions.

| Query | Meaning |
|---|---|
| `Safari` | Find an application or matching filename |
| `?python` | Dedicated filename search |
| `?"quarterly report"` | Search a filename containing spaces |
| `?genetec kind:pdf` | PDFs whose filenames match Genetec |
| `?kind:pdf size:>5MB modified:week` | PDFs over 5 MB changed in the last seven days |
| `?Screenshot kind:image modified:today` | Screenshots changed today |
| `?kind:folder` | Folders |
| `?python modified:>180d` | Matching files whose contents have not changed in 180 days |
| `?python used:>180d` | Matching files with a recorded last-opened date older than 180 days |

Filters also work in normal search without `?`. Supported filters:

- `kind:` a filename extension, or `folder`, `image`, `audio`, `video`.
- `size:` `>`, `>=`, `<`, `<=`, `=` plus B, KB, MB, GB or KiB, MiB, GiB.
- `modified:` / `used:` `today`, `yesterday`, `week` (7 days), `month` (30 days),
  a day count such as `30d`, or older-than such as `>180d`.
- Quote a literal filename such as `"kind:pdf"` so it is not interpreted as a filter.
- Invalid/duplicate filters are rejected. A filter is never silently removed to produce results.

**Why `used:` may find nothing:** it depends on Spotlight's `kMDItemLastUsedDate`.
Files without a queryable last-opened date cannot match. It does not mean last modified,
created, downloaded, or indexed. On this Mac, the checked Python filename query returned
9,684 candidates but zero with the last-opened filter; a broader date positive control
also returned zero. The launcher now explains an empty filtered result. Try `?python` first. Use `modified:` only if that is the date you intend.

Filename/date search uses macOS Spotlight. It does not wait for Blindspot's content or
embedding index, and rebuilding embeddings will not supply missing Spotlight dates.

## 3. Search inside text and by meaning

Use `:content database migrations`, `documents about genetec`, or
`find code mentioning websockets`. Each content result shows a short excerpt around the words
that matched, so you can see why it was found without opening it. Text retrieval and optional semantic retrieval run
separately, so text results can arrive first. Missing semantic models/helpers fall back
to conventional text search. Ordinary search can also append content matches; use
`:content` when you specifically want content rather than filenames.

**Passage search.** Indexed files are also split into passages of roughly 1,000 bytes, overlapping
slightly so a sentence spanning a boundary still matches, and search ranks those passages rather
than whole files. Content search shows up to two distinct passages per file; ordinary launcher
search still shows one file row, so one long file cannot fill the list.
Prose, code and paged documents are split differently: code breaks at declarations and carries the
surrounding function or type names, prose keeps its nearest heading, PDFs keep real page numbers,
and PPTX keeps slide order. Word text does not claim page numbers. Very large files stop at
400 passages or 2 MiB and appear as partly indexed.

A passage result names the place to open: **p. 12** for a page, **line 31** for a line. With such a
row selected, ⌘K offers **Open at Line N**, which opens the file at that line in Zed, VS Code,
Cursor or Sublime Text if one is installed, and otherwise in iTerm using your `$EDITOR`.
The iTerm fallback accepts `vim`, `nvim`, `vi` or `nano` and may require Automation permission;
otherwise choose **Open**. PDF Return and ⌘Y use Blindspot's page-aware preview; **Open** in ⌘K
still uses the default application. Slide rows show the slide number but open the whole deck.
⌘K also offers **Copy Passage** and **Ask About This Passage…**. These use the stored passage,
not the short result subtitle; an obsolete chunk is refused after reindexing.

If a query's words never all appear together, search retries with any of them rather than returning
nothing. On a disposable 134-document checkout index and 47 golden queries, blended retrieval
scored **89% hit@5 / 0.66 MRR@10**, versus **66% / 0.49** for passage words alone and **36% / 0.34**
for whole-document words. Blended p95 was 37 ms. These are checkout measurements, not a claim
about a million-document library. `make bench-search DB=... HELPERS=... BENCH_FLAGS=--check`
uses the shipping pipeline and fails a ranking-regression gate; output contains local paths.

**Content filters** use the same syntax as file search, placed anywhere after `:content`:

| Query | Meaning |
|---|---|
| `:content kind:pdf genetec` | PDFs whose text mentions Genetec |
| `:content kind:documents modified:month budget` | PDF, Word and note files changed in the last 30 days |
| `:content kind:word size:<2MB proposal` | DOCX, DOC, RTF and ODT files under 2 MB |
| `:content kind:notes signaling` | txt, md, markdown and rst notes |
| `:content kind:code websocket` | Source files |
| `:content kind:md modified:>180d roadmap` | Markdown not modified in the last 180 days |

`used:` is refused for content search, because the index does not store last-opened dates.
`documents about TOPIC` now means `:content kind:documents TOPIC`, and `find code mentioning`
means `:content kind:code`.

**Settings → Content:** Desktop and Downloads are the default folders. Explicitly saved
off settings are respected. **Add folder…** opens the macOS multi-folder chooser;
select a list row and choose **Remove selected** to stop including that folder.
Long paths are shortened to `~`, remain on one line, and have full-path tooltips.
Add a project/notes folder rather than your entire home directory where practical.

Supported indexed text/code extensions:
`txt md markdown rst csv tsv json toml yaml yml xml html css rs swift py js jsx ts tsx go java c h cpp hpp rb sh sql`.

**Search by meaning** uses the installed Ollama **Embedding model**, default `embeddinggemma:300m`,
one vector per passage. It is separate from the model answering AI questions. Empty selects
Apple's installed English contextual model. If Ollama is unavailable at indexing time, Apple is
tried automatically; if no usable backend/generation is available, word search remains usable.
No model is downloaded. Use Index → Refresh after starting Ollama or installing a model yourself.

- **What gets embedded:** prose, extracted PDF/Word/PPTX text, and code under explicitly selected
  **Code folders** (empty by default). Code elsewhere remains word-searchable. JSON, CSV, TSV,
  XML, YAML, TOML and XLSX remain exact-text only. `.gitignore` is not used; generated/dependency
  directory exclusions still apply.
- **How results are ordered:** one deterministic blended list, labelled **Words**, **Meaning**,
  or **Words + meaning**. Vectors from different models are never compared. New model generations
  build in the same database and activate after completion; older published generations remain
  available. Compact can retire inactive completed non-Apple generations.
- **Good for:** concepts whose words are not in the file. On real folders,
  `:content video surveillance security cameras` surfaced WebRTC camera examples and streaming
  API PDFs. **Not good for:** proper nouns, IDs and error codes — use exact words (`genetec`);
  meaning matches can be unrelated.
- A new or edited note is searchable by meaning as soon as it is embedded, before the cache rebuild.
- The index contains bounded excerpts, so a word appearing only beyond the excerpt may not match.

**PDF content indexing** is enabled by **Index PDF documents** in Content settings.
It extracts up to 2 MiB across the first 100 pages through an isolated local helper.
The default PDF source limit is 32 MiB, configurable from 1 to 128 MiB. New PDFs join
text and semantic search after their extraction/indexing pass; existing text indexes are reused.

Locked, image-only, oversized and unreadable PDFs have no newly extracted body. If an
older body was indexed successfully, it is retained as a fallback and may be out of date.
A changed document is retried. Turn on **Read scanned PDF pages** to revisit existing PDFs and
recognize pages with no usable text locally. OCR is off by default, attempts 20 pages per PDF by
default (configurable 1–100), and marks recognized passages **Read by OCR**. Limits can leave a
document partly indexed. The helper has a 20-second watchdog.
**Word, RTF and OpenDocument text** (DOCX, DOC, RTF, ODT) are indexed the same way when
**Index PDF and Word documents** is on. The extraction helper runs with network access removed
by the macOS sandbox before it reads a document. Before enabling this, Apple's importers were
tested with documents referencing remote images, templates, hyperlinks and INCLUDEPICTURE
fields: they made no network requests. **PPTX** slide text and **XLSX** cached cells also use the
no-network helper. Workbook sheet names and cell addresses are included in the text; formulas
are not evaluated, external links are not followed, and worksheets are never merged together.
Encrypted, corrupt, excessively expanded or unsupported archives are marked unreadable.
Native Pages/Keynote/Numbers and standalone image indexing are not supported; export to PDF,
PPTX or XLSX. Office opening is whole-file; PDF page and source-code line navigation are supported.

### What indexing status means

**Settings → Index** shows what the content index holds and what it is doing. It refreshes every
second while open; **Settings → Content → Open index** jumps there.

| Part | What it tells you |
|---|---|
| Header | Up to date, Indexing, Paused or Off; while indexing, the stage, elapsed time and folder; otherwise when the last pass finished and how long it took. **Refresh** starts a full reconciliation |
| Meaning bar | Active-model vectors out of all stored passages; not a completion percentage because some kinds stay word-only |
| Tiles | Documents stored, extracted documents with text, passages searchable by meaning, disk used (database plus semantic cache) |
| Busy-folder banner | A folder whose files changed at least 20 times in five minutes. **Exclude folder** asks first, then adds it to Content → Excluded paths |
| What's indexed | Documents by kind (notes, PDFs, code, data, web) and by top-level folder, with sizes. Click a folder to reveal it in Finder |
| Needs attention | Documents without text, locked, oversized, unreadable or partly indexed. Click to reveal |
| Recently changed | The most recently modified indexed files |
| Activity | This pass's checked, updated, unchanged, skipped and unreadable counts; what was embedded; watched folders; when counts were sampled |
| Resources | Blindspot and each helper (semantic model, vector search, PDF extraction) with memory and CPU %, and the current pace |

**Compact** (next to Refresh on the Index page) asks first, then removes vectors left by the
retired search model, merges the word index and rewrites the database file so deleted pages go
back to the disk. Indexing and content search pause until it finishes. It needs free disk space
roughly equal to the database size, and never removes documents or current vectors. The Disk
tile shows how much is reclaimable, and Activity shows the last result.

Inventory, stored and disk figures are sampled in the background at most every 15 seconds; pass
counters update immediately. **Settings → Status → Content index** gives a one-line summary.
Detailed terms:

| Item | Meaning |
|---|---|
| Indexing / visited | Filesystem entries examined in the current reconciliation, including skipped entries/directories |
| Updated | Documents newly written or changed during this pass, not the total index size |
| Source bytes | Combined source-file size for documents updated in this pass; not database, vector-cache or RAM size |
| Documents / embeddings | Sampled totals stored in the index, distinct from this pass's updates |
| DB / vector cache | SQLite main-file size and actual regular vector-file sizes, including unpublished artifacts |
| PDF extraction issues | Stored no-text, locked, oversized or unreadable statuses; a previously indexed body may still be retained |
| Unchanged | Documents whose stored content fingerprint was reusable |
| Skipped / failed | Entries excluded by policy or unavailable to read; not all visited entries become documents |
| Embedding / written | Embeddings newly generated during the current embedding pass |
| Embedding / current | Existing compatible embeddings reused during that pass |
| Semantic cache | Building or validating the local vector-search cache |
| Active path | Current configured root; `~` means the home folder, not one particular file |
| Ready | The pass finished |
| Partial | Some work could not complete; previous records are retained where necessary |
| Paused | A named power/thermal condition is preventing background work |

Storage/count diagnostics are sampled in the background; they are not instantaneous totals.
**Settings → Status → Content storage** separates database, WAL and SHM sizes from the
vector files. File sizes are logical sizes, not a total filesystem allocation
measurement. App resident memory is shown separately under **App resources**.

There is no reliable final percentage until the eligible work is known. Each counter is
shown once; the Erase row describes erasure and only shows erase progress during an erase.

**Refresh** requests reconciliation. During active indexing it cancels that pass and
queues another; committed documents/embeddings remain reusable. Avoid repeatedly pressing
it while progress is advancing. Filesystem changes normally trigger updates automatically.

**Disable indexing:** stops it and hides content results, retaining stored data.
**Erase content index:** after confirmation, disables indexing and removes stored excerpts,
embeddings and vector caches. It does not delete original files. **Stop erasing** can leave
partial cleanup; retry to finish. Removing all roots alone is not an erase command.
Upgrading the app normally reuses the separate on-disk index and compatible embeddings.

### Resource policy and current limits

- Background work pauses in Low Power Mode or serious/critical thermal conditions.
- Battery indexing is controlled by **Index on battery**; default is off.
- Power conditions are rechecked every 30 seconds as well as on system notifications.
- Default text/code source limit: **1 MiB**, configurable up to **16 MiB**.
- Separate PDF source limit: **32 MiB**, configurable up to **128 MiB**; at most 100 pages.
- **Low-impact indexing** increases pauses between scan batches from 10 ms to 50 ms.
  This slows reconciliation; it is not a measured CPU reduction or a hard quota.
- Searchable passages: at most **2 MiB / 400 passages per source**; a legacy 64 KiB excerpt is
  retained for compatibility. Maximum traversal depth: **64**.
- **Index budget (MiB)** defaults to 3072 and pauses new work when database, journals and vector
  files reach the target. Shard builds reserve an estimate first. In-flight batches may overshoot;
  this is not a hard disk quota. Increase the budget, narrow scope or Compact to resume.
- Up to **32 roots** and **128 exclusions**; work is batched and cancellable.
- Common sensitive/hidden/generated paths, symlinks, cloud placeholders and network
  volumes are skipped. Examples include Library, node_modules, target, DerivedData,
  __pycache__, venv, vendor, Pods, site-packages, bower_components, Carthage, CMake `_deps`
  and Go's module cache (`…/pkg/mod`). Additional exclusions are configurable.
- After a complete pass, filesystem events re-read **only the changed files and folders**
  (new, edited, renamed, moved or deleted). A change at a root itself, dropped events or more
  than 128 changed paths fall back to a full reconciliation.
- Event-driven passes start at least **10 seconds** apart, and the semantic model helper starts
  only when a pass changed notes or PDFs (or the previous semantic pass did not finish). A folder
  that a program rewrites constantly shows up as a busy folder on the Index page.
- These are workload limits, **not hard CPU-percent or total-RAM quotas**.
- Ollama/Qwen runs separately. Its model memory is not the launcher's own memory usage.
  **Settings → Status → App resources** and **Settings → Index → Resources** show Blindspot and its
  own helpers with resident memory and CPU percentage between two refreshes (the first refresh
  shows memory only). Ollama is a separate process and is not included; use Activity Monitor for it.
  CPU time is converted from Mach time units: builds before 0.2.5 under-reported cumulative CPU
  about 42× on Apple silicon, including in process Inspect.
- Indexed record count, source bytes, on-disk database/vector size, and RAM usage are
  different measurements. The source-byte counter covers this pass's updated documents.

## 4. Actions — select a result, then ⌘K

| Object | Available actions |
|---|---|
| File | Open, Quick Look, Reveal in Finder, Copy Path, Copy Filename, Rename, Move, Duplicate, Compress to ZIP, Move to Trash |
| Supported text/code/PDF file | Also Summarize with Local AI and Extract Text to Clipboard |
| Application | Open, Reveal, Copy Path, Quit, Force Quit, Restart Application, Show Process; inherited file actions may also be offered |
| Text clipboard item | Copy, Paste into Previous App, Pin/Unpin, Delete, Rewrite Professionally, Summarize, Translate, Ask Local AI, Save as Snippet… |
| Quick link | Open Link (or Type Search Text), Copy URL, Delete Quick Link |
| Snippet | Paste Snippet into Previous App, Copy Snippet, Delete Snippet |
| Image clipboard item | Copy, Paste into Previous App, Pin/Unpin, Delete |
| Process/socket | Inspect, Copy PID/Port/Command/Executable Path, Show Parent/Children/Related Ports, Reveal Executable, Open Working Directory, Terminate, Force Terminate |
| App-backed process | Restart Application where safely available |
| HTTP/HTTPS URL or URL clipboard text | Open URL, Copy URL, Extract Domain |

Availability depends on the object and macOS permissions. Destructive actions ask for
confirmation; signals recheck process identity. File mutation already committed by macOS
cannot be rolled back simply by pressing Escape. Compression uses a local ZIP operation.

PDF extraction runs in a separate helper with a timeout. It accepts bounded regular files
(up to 16 MiB), examines at most 50 pages and returns about 16 KB of text. Locked,
image-only or inaccessible PDFs may have no extractable text. Local summaries cover the
extracted excerpt, not necessarily the entire document.

## 5. Developer console

| Query | Meaning |
|---|---|
| `:3000`, `:5173` | TCP/UDP sockets using the specified port |
| `:ports`, `:listening` | Visible TCP listeners and UDP sockets |
| `:localhost` | Sockets bound to loopback interfaces |
| `:processes` | Visible running processes, including those without sockets |
| `:node`, `:python`, `:docker` | Filter processes by name/executable |
| `:pid 123` | A process by PID |
| `:children 123` | Its immediate children |
| `:ports 123` | Its related sockets |

Rows show protocol/address/owner and refresh approximately every two seconds while visible.
Inspect exposes PID, parent, executable, command line, resident memory, cumulative CPU time,
uptime and working directory where available. Cumulative CPU time is not CPU percentage.
Visibility depends on permissions. The app does not elevate privileges or reconstruct an
arbitrary server's restart command/environment.

## 6. Clipboard history

Type `;` to browse or `;invoice` to search. Return copies an item and restores focus;
press ⌘V in the destination. **Paste into Previous App** automates that last step when
Accessibility permission and the destination's focus allow it.

Pin frequently used text or images with ⌘K. Pins survive count-based retention; up to
100 pins are allowed, and the global storage cap still applies. Clear all explicitly
removes pinned items too. In Clipboard settings you can enable/disable recording, set
retention count, include images, and enable on-device image-text recognition (English/French).
Concealed/password-manager clipboard markers are skipped. Ingestion queues are bounded;
very rapid copy bursts can drop older waiting items.

Storage caps: text item 1 MiB, image item 25 MiB, total stored clipboard bytes 256 MiB.
AI transformations accept text up to about 16 KB. Disabling recording stops new reads;
**Forget everything** deletes saved history after confirmation without clearing launch history.

## 7. Local AI and context

`>your question` explicitly opens general Agent mode. Return submits. Choose installed
models through **Settings → Agent** or ⌘M in Agent mode. The Settings pickers use the
configured local Ollama host and include **Refresh local models**. The current configured
choice remains represented when unavailable; choosing a model does not download one.
Set the task model and question model independently where offered.

Supported typed Agent plans: create a directory, create an empty file, list a directory,
**rename** a file or folder, **move** it into an existing folder, and **move it to the Trash**,
all inside configured allowed roots. Rename, move and trash show the exact changes and ask
before running. They never overwrite an existing item (the kernel refuses), never follow a
symlink in the path, refuse protected folders such as `.ssh` and `Library`, and cannot act on
the allowed folder itself. Trash keeps items recoverable, though Finder's Put Back does not
know their original folder. Moves across volumes are refused; use Finder for those.

**Ask your documents:** type `>docs` followed by a question, or say `ask my documents …`.
Blindspot finds the best passages in your indexed notes, PDFs and Word files (any-word matches
plus search by meaning), sends only those excerpts to your local question model, and shows the
answer with numbered **Sources**. Return on a source opens that file; ⌘K offers file actions.
Answers cite excerpts like [1]. If the excerpts do not answer the question, the model is told to
say so, but check the sources for anything important. Only the question and answer are kept in
Agent history; excerpts are not. General requests can be answered as text; model output is never an
unrestricted shell command. Past requests can be reopened without automatically rerunning them.

For context, select text in another app, open Blindspot, type one of these and press Return:

- `rewrite professionally`
- `summarize`
- `explain`
- `translate into French`
- `translate what I copied into French` (clipboard explicitly)
- `summarize this page` (supported browser accessibility content)

**AI commands** are saved prompts for selected text. Select text, open Blindspot, type the
command's name and press Return; the answer replaces nothing until you copy or paste it.

| Type | What it asks the local model |
|---|---|
| `fix grammar` | Correct spelling, grammar and punctuation, keeping tone and language |
| `make shorter` / `make longer` | Tighten or expand without inventing facts |
| `make friendlier` / `make formal` | Change the tone only |
| `bullet points` | Turn the text into concise bullets |
| `explain simply` | Explain it for a newcomer |
| `write reply` | Draft a short reply to a message, in its language |

Save your own with `:prompt KEYWORD INSTRUCTION`, e.g. `:prompt tldr Summarize this in one
sentence.` → Return. Run it by selecting text and typing `ai tldr`. Saved commands always start
with `ai `, so a keyword such as `mail` never turns an app search into an AI request. `:prompts`
lists built-in and saved commands (Return fills the field; ⌘K → **Delete AI Command** removes a
saved one) and `:unprompt tldr` removes one. They are stored with quick links in
`shortcuts.toml`: up to 256, instructions up to 2 KiB. The selected text is sent to the local
model as reference data, never as instructions, and the output is text only.

Finder-selected supported files can supply text to `summarize`. Supported frontmost apps
can expose their focused document. `open current project`/`open this project` offers the
current Terminal/iTerm/Finder folder where scripting/process APIs provide it.

Accessibility enables selection reading and automated paste. Automation permission may be
needed for Finder/browser/Terminal/iTerm context. Permissions are requested on explicit
context use. Safari, Chrome, Brave and Edge page text depends on accessibility support;
there is no network page fetch or browser-history/database scraping. Unsupported context
shows unavailable. AI transformations of supplied document/clipboard/selection text are read-only.

### Recognized natural-language shortcuts

These are deterministic aliases, not a promise to understand every possible phrasing:

- `documents about TOPIC`, `document about TOPIC`, `files about TOPIC`;
  also `find documents about`, `find the document about`, `find files about`,
  and `find code mentioning` followed by your words → content search.
- `close whatever is using port 3000`, `kill the process using port 3000`,
  `terminate the process using port 3000`, `kill process on port 3000` → select
  an owner, then review termination. Replace 3000 with a valid port.
- `show everything listening on localhost`, `show localhost ports` → loopback sockets.
- `show listening ports`, `show all ports` → socket console.
- `find screenshots from this week larger than 5 MB` → explicit screenshot/date/size filters.
- `show me files i haven't touched in six months`, `find files not opened in six months`
  → last-opened filter, with the same Spotlight limitations described above.
- `open current project`, `open this project` → current folder context.
- `my schedule`, `next meeting`, `join my next meeting`, `what's on today` → calendar.

## 8. Quick links and snippets

| Query | Meaning |
|---|---|
| `:link gh https://github.com/search?q={query}` | Save a quick link; Return on the row saves it |
| `gh blindspot launcher` | Open the saved link with the search text filled in |
| `:links` / `:links git` | List quick links; Return on a link row starts `gh ` so you can type |
| `:unlink gh` | Remove a quick link |
| `:snippet sig Best regards,\nSeif` | Save a snippet; `\n` becomes a new line, `\t` a tab |
| `!` / `!sig` | List or find snippets; Return pastes into the app you were using |
| `:snippets` / `:unsnippet sig` | List or remove snippets |

Keywords are 1–32 lowercase letters, digits, `-` or `_`. Quick links must be `http://` or
`https://` addresses, so a saved keyword can never launch a file or another app's URL scheme.
Search text is percent-encoded where `{query}` appears. Snippets hold up to 16 KiB and can use
`{date}`, `{time}` and `{clipboard}`, filled when pasted. Paste needs Accessibility permission;
without it the snippet is copied and ⌘V finishes the paste. To turn copied text into a snippet,
type `;`, select it, press ⌘K → **Save as Snippet…** and choose a keyword.

Both are stored in `~/.local/share/blindspot/shortcuts.toml` (private to your account, written
atomically), never in config.toml. Up to 256 of each. Typing an abbreviation in other apps does
not expand it: that would need system-wide keystroke monitoring, which Blindspot does not do.

### Fallback searches

When a search settles with fewer than three results, Blindspot adds next steps below them:

- **Search documents for “…”** fills in `:content …`.
- **Ask your documents** and **Ask local AI** fill in `>docs …` or `>…`; press Return again to send.
- **Search the web for “…”** opens the address in **Settings → General → Fallback web search**
  (DuckDuckGo by default). It must be an `http`/`https` address containing `{query}`; leave it
  empty to hide the row.

Nothing is searched online or sent to a model until you choose a row.

## 9. System commands

Type at least three letters of a command's name; up to three matching commands appear among
the results. `:system` lists them all.

| Type | Command |
|---|---|
| `lock` | **Lock Screen.** Uses ⌃⌘Q when Accessibility is allowed; otherwise it turns the displays off, which locks when a password is required after sleep (the macOS default) |
| `sleep` / `sleep displays` | Sleep the Mac, or just the displays |
| `screen saver` | Start the screen saver |
| `ocr`, `copy text from screen`, `grab text` | **Copy Text from Screen.** Select an area and its text is copied; a short notice says how many words. Recognition runs on this Mac with the same engine as Live Text, and the screenshot is deleted straight away. The first use asks for Screen Recording permission |
| `restart`, `shut down`, `log out` | **Asks first**, then asks macOS as the Apple menu does, so apps with unsaved work can still cancel |
| `empty trash` | **Asks first**; deletes the Trash permanently through Finder |
| `eject` | Eject removable and network disks; reports any that are in use |
| `dark mode` | Toggle light and dark appearance through System Events |
| `mute` | Toggle sound output mute |
| `quit all` | **Asks first**; asks every open app except Finder and Blindspot to quit |

macOS asks once before Blindspot may control Finder, System Events or the login window. If that
was declined, the row explains where to allow it: **System Settings → Privacy & Security →
Automation**.

## 10. Calendar: my schedule and joining meetings

Type `my schedule`, `:schedule`, `next meeting` or `join my next meeting`. Everyday questions work too, typed plainly
or after `>`: `do I have anything on my calendar today`, `when is my next meeting`,
`> any meetings this afternoon`. The local AI cannot read your calendar, so these show the
schedule instead of asking it. Requests to create or move events, and searches for meeting notes
or documents, are not captured.

- **First use:** a row asks for Calendar access. Press Return and macOS shows its permission
  prompt. If access was declined, the row opens **Privacy & Security → Calendars** instead.
- **What it shows:** the remaining events for today and tomorrow (up to 24), with times and
  labels such as *In 12 min* or *Now*. It covers every calendar in the Calendar app, including
  iCloud, Google and Exchange accounts added in System Settings.
- **Joining:** an event with a Zoom, Google Meet, Microsoft Teams, Webex, Whereby, Jitsi,
  GoTo Meeting, Amazon Chime or FaceTime link in its URL, location or notes shows a video icon.
  Return opens that link over https; ⌘K offers **Copy Meeting Link** and **Open Calendar**.
  Links to other sites are never offered as Join.
- **Other events:** Return opens Calendar.

Events are read on this Mac each time the list is shown, and nothing is stored or sent.

**Adding events:** type what you want, plainly or after `>`, and press Return on the preview.

| Type | Preview |
|---|---|
| `schedule a meeting today about an exam at 6pm` | Meeting about exam · Today 6:00–7:00 PM |
| `schedule a meeting for tomorrow at 6pm` | Meeting · Tomorrow 6:00–7:00 PM |
| `book lunch with Sarah friday at noon` | Lunch with Sarah · Friday 12:00–1:00 PM |
| `schedule study session tomorrow 2-4pm` | Study session · Tomorrow 2:00–4:00 PM |
| `add dentist appointment tomorrow at 3pm for 30 minutes` | Dentist appointment · 3:00–3:30 PM |
| `schedule exam tomorrow` | Exam · Tomorrow, all day |

- **How it's read:** the date and time come from macOS's own date detector on this Mac, not the AI.
  The preview row shows exactly what will be saved, in your default calendar for new events, and lists
  that day's existing events so a clash is visible.
- **Defaults:** events last an hour unless you give a range (`2-4pm`) or a length (`for 30 minutes`).
- **After adding:** the row changes to *Added* and Return opens Calendar. To edit or delete the event,
  use Calendar.
- **What counts:** requests starting with `schedule` or `book`, or with `add`, `create`, `plan` or
  `set up` plus the word meeting, event, appointment, call or calendar. `schedule today` still shows
  your schedule, and anything about files, notes, reminders or timers is left alone.

## 11. Calculator and utilities

Type these in normal search; select the desired output and press Return to copy.

| Example | Feature |
|---|---|
| `12 * 34`, `(20 + 5) / 2`, `15% * 89` | Arithmetic, parentheses and percentages |
| `now` | Current epoch seconds and milliseconds |
| `@1757548800`, `@1757548800123` | Epoch seconds/milliseconds to UTC/ISO and local display |
| `uuid` | Random UUID v4 |
| `sha256 hello` | SHA-256 of the supplied text; hex/Base64 outputs |
| `json {"a":1}` | Formatted and compact JSON; parse errors explained |
| `b64 SGVsbG8=` / `b64 hello` | Base64 decode where valid UTF-8, and encode |
| `url hello world` / `url hello%20world` | URL percent encoding/decoding |
| `hex 255` / `0xff` | Decimal/hex/binary representations |
| `1500MB`, `3.5 GiB` | Decimal/binary byte conversions |
| `90s`, `1h 30m`, `250ms` | Durations, seconds and milliseconds |
| `180cm`, `5 ft 11 in`, `10 miles` | Metric/imperial lengths |
| `70kg`, `150 lbs`, `1 lb 8 oz` | Metric/imperial weights |
| `72f`, `-40c`, `300 kelvin` | Temperatures: Celsius, Fahrenheit, kelvin |
| `2 L`, `1 cup`, `12 fl oz`, `3 tbsp` | Volumes, metric and US measures |
| `1 acre`, `1000 sq ft`, `50 m²` | Areas |
| `100 km/h`, `60 mph`, `20 knots` | Speeds |
| `32 psi`, `1 atm` | Pressure |
| `500 kcal`, `3 kWh`, `150 hp` | Energy and power |
| `90°`, `2.4 GHz` | Angles and frequencies |
| `100 Mbps`, `50 MB/s` | Data rates, with how long 1 GB takes |
| `30 mpg`, `7 L/100 km` | Fuel economy |
| `5 km to miles`, `72f in c`, `90 min to hours` | Convert to one unit you name |
| `time in paris`, `tokyo time` | The current time somewhere else |
| `3pm montreal in tokyo`, `9am to london`, `noon in delhi` | Time zone conversion |
| `https://example.com` | Open/copy URL or extract its domain |

Byte units include B/KB/MB/GB/TB/PB and KiB/MiB/GiB/TiB/PiB. Duration units include
ns/us/ms/s/min/h/d and common spelled-out forms. Lengths include mm/cm/m/km/in/ft/yd/mi;
weights include mg/g/kg/tonnes/oz/lb/stone. `m` alone can mean metres or minutes,
so inspect the result label. Compound supported units can be combined.

**More units:**
- Volumes and areas answer in the other system: `2 L` shows quarts and gallons, `1 cup` shows millilitres.
- Temperatures show the other two scales; the rest show the two most useful forms.
- Add `to` or `in` and a unit for exactly one answer: `5 km to miles`, `1.5 cups to ml`.
- US measures are used for cups, pints, quarts, gallons and mpg.
- A lone `k` is not read as kelvin, because `5k` usually means five thousand.

**Time zones:**
- `time in paris` shows the time there, its zone and how far ahead or behind you it is.
- `3pm montreal in tokyo` converts a time between two places and shows both clocks.
- `3pm in tokyo` reads 3pm as Tokyo's time and shows yours; `3pm to tokyo` reads it as yours.
- Places are city names from macOS's time-zone database (Tokyo, New York, São Paulo is `sao paulo`),
  common cities and countries without a zone of their own (Montreal, San Francisco, India), and
  abbreviations such as `pst`, `cet` and `utc`.
- Daylight saving comes from the zone files macOS already keeps, so it is exact and works offline.
- An unknown place shows nothing rather than a guess.

## 12. Settings and discovery

Type `:` or `:help` for the first-party command catalog; `:po` then Tab completes
`:ports`. `:settings WORDS` searches labels, sections and keys, e.g. `:settings hotkey`,
`:settings clipboard`, `:settings agent.model`.

**Settings → Status** also shows the installed Blindspot version.

**Updates:** press **Settings → Status → Updates → Check for updates**.

- **Checking:** Blindspot asks GitHub for the latest release of SeifBoukerdenna/blindspot. It never
  checks in the background.
- **Installing:** **Install** downloads the release and verifies it before anything on disk changes:
  - the zip must match the release's SHA-256 checksum file;
  - it may contain only the expected folder;
  - the app inside must have a valid code signature, Blindspot's bundle identifier and the release's
    version.
- **Relaunch:** Blindspot then asks before it quits, replaces itself in place and reopens. Settings,
  clipboard history and the index are untouched.
- **Signing:**
  - A release signed by the same Developer ID as your copy installs directly.
  - An ad hoc release shows a warning first, because macOS treats it as a new app and asks again for
    permissions such as Accessibility.
  - A release signed by a different developer is refused.
- **Location:** Blindspot must live in a folder you can write to, such as Applications in your home
  folder. Updating does not work from a copy opened straight out of Downloads, which macOS runs from
  a temporary read-only location.

Settings cover launcher/Agent shortcuts, login startup, result count, app folders,
launch-history ranking decay, indexed roots/exclusions/source size/power policy,
clipboard retention/images/OCR, local AI models/host/keep-alive/timeout/allowed roots,
appearance palettes, and diagnostic status. Reset returns a setting to its inherited
configuration/default; the origin label explains whether it was set in this window.

## 13. Practical workflows

### Find and summarize a Genetec document

1. Open Blindspot and try `?genetec` to locate matching filenames immediately.
2. Add `kind:pdf` for PDFs. Select a result and press ⌘Y to confirm it is the right file.
3. Press ⌘K → **Summarize with Local AI**. Wait for the local model's response.
4. For topic search inside PDFs and notes, add the folder (for example `~/Genetec_capstone`)
   in Content settings and keep **Index PDF documents** on. Wait until **Settings → Index** says
   Up to date, then type `documents about genetec` or `:content genetec`. PDFs whose
   filenames never mention Genetec appear because their extracted text does — verified with
   exported API-reference PDFs in `~/doc-capstone`.
5. For related material that does not use the word itself, try
   `:content video surveillance security cameras`. Rows marked **Meaning** are approximate;
   they are blended with word matches, not appended as a separate tail.
6. If a document is missing, check **Settings → Index → Needs attention** (no text, locked, over
   the size limit, unreadable, partly indexed) and the size limit. For scanned PDFs, enable
   **Read scanned PDF pages**, allow the reconciliation to finish, then try the query again.
7. To search implementation concepts, add the repository under **Code folders**. Search
   `:content kind:code rebuild vector shards`, then Return to the matching line. To inspect
   workbook values, try `:content kind:xlsx renewal`; sheet/row/cell labels accompany cached values.

### Ask a question about your own documents

1. Make sure the folder is indexed: **Settings → Index** should say Up to date.
2. Open Blindspot and type `>docs what did the capstone decide about WebRTC signaling`,
   or `ask my documents what did the capstone decide about WebRTC signaling`, then Return.
3. Read the answer and its numbered sources. Return on a source opens it; ⌘Y previews it.
4. If the answer says the documents do not cover it, try distinctive words from the document
   or check the file with `:content kind:documents signaling`.

### Save a quick link and a snippet

1. Type `:link gh https://github.com/search?q={query}` and press Return.
2. Type `gh blindspot launcher` and press Return to open the search.
3. Type `:snippet addr 123 Example Street\nMontreal` and press Return.
4. In any text field, open Blindspot, type `!addr` and press Return to paste it.

### Tidy files with the agent

1. Type `>rename ~/Downloads/report-final.pdf to capstone-report.pdf`, then Return.
2. Review the proposed step, for example *Rename: Downloads/report-final.pdf → capstone-report.pdf*.
3. Press Return on the step and confirm **Run**. If the name exists, nothing is changed.
4. `>move the old screenshots in Downloads to Desktop/Archive` or `>move ~/Downloads/old.zip to
   the trash` work the same way; every change is listed before it runs.

### Reclaim disk space from the index

1. Open **Settings → Index** and read the Disk tile's reclaimable amount.
2. Press **Compact**, read the confirmation and choose Compact.
3. Watch the header stages; Activity shows before and after sizes when it is done.

### Free a development port safely

1. Type `:3000` and press Return to inspect the owner.
2. Verify the executable, PID and command line. Use ⌘K → **Show Related Ports** if needed.
3. Choose **Terminate** and confirm the specific process.
4. Re-query the port. Use Force Terminate only when appropriate; restart your server
   through its normal terminal/tool rather than an inferred command.

### Rewrite, translate and reuse a snippet

1. Select a paragraph in the source app, open Blindspot, type `rewrite professionally`, Return.
2. Copy the answer with Return when it is selected, then paste into your destination.
3. Alternatively copy text first and run `translate what I copied into French`.
4. Find reusable text with `;`, open ⌘K and Pin it. Later use Copy or Paste into Previous App.

### Maintain the index without wasting work

1. Keep roots limited to folders you need. Exclude generated/private subfolders.
2. Open **Settings → Index**. The header shows the stage and elapsed time; the meaning bar shows
   active-model passage coverage. **Embedded this pass** counts passages, not documents.
3. If a busy-folder banner names generated data (logs, feeds, caches), choose **Exclude folder**.
   If **Resources** shows sustained CPU you do not want, turn on **Low-impact indexing**;
   passes take longer. Turning off **Search by meaning** stops embedding work entirely.
4. If paused, read the specific reason; power checks repeat automatically.
5. Let a running pass finish. Use Refresh for a reconciliation, not as a progress refresh button.
6. Upgrade the app normally. Do not erase the index merely to install a new build.

## 14. Privacy, limitations, and 0.2.9 release notes

Data and inference remain local by default. No cloud dependency or custom third-party
extension setup is added. The local model service must be running. This is a locally
signed macOS 26+ Apple Silicon build, not a notarized public release.

Not implemented: universal document-format indexing, arbitrary shell automation,
arbitrary process restart, complete browser automation, public extension distribution,
hard CPU/RAM quotas, or validated 10-million-record production operation. macOS limits
process visibility, selected-text access, last-opened metadata and atomic PID identity checks.

**0.2.9 changes**

- **In-app updates:** Settings → Status → Updates checks GitHub for the latest release, then
  downloads, verifies and installs it and relaunches, asking first.
- **Add calendar events:** `schedule a meeting today about an exam at 6pm` shows a preview; Return
  adds it. Calendar questions such as `do I have anything on my calendar today` show your schedule,
  with or without `>`.
- **App icon.**
- **Time zones:** `time in paris`, `3pm montreal in tokyo`, daylight saving included, offline.
- **Copy Text from Screen:** `ocr` or `copy text from screen`, then select an area.
- **More conversions:** temperature, volume, area, speed, pressure, energy, power, angles,
  frequencies, data rates and fuel economy, plus `5 km to miles`-style targets. Conversion labels
  no longer wrap mid-word.

**0.2.8 changes**

- **System commands:** lock, sleep, restart, shut down, log out, empty Trash, eject, dark mode,
  mute and quit all apps from search or `:system`. Anything that ends work or deletes asks first.
- **Fallback searches:** search documents, ask documents, ask local AI or search the web when a
  search finds little. The web address is a setting and can be emptied.
- **My schedule:** today's and tomorrow's events with one-keystroke Join for detected meeting links.
- **AI commands:** built-in `fix grammar`, `make shorter` and six others for selected text, plus
  your own saved prompts with `:prompt` and `ai KEYWORD`.

**0.2.7 changes**

- **Ask your documents** (`>docs …`): local answers from indexed passages with openable sources.
- **Why it matched:** content results show an excerpt; `:content` accepts `kind:`, `size:` and
  `modified:` filters, with `kind:documents`, `kind:word`, `kind:notes` and `kind:code`.
- **Word, RTF and ODT indexing**, with the extraction helper's network access removed.
- **Compact** on the Index page reclaims space from retired vectors and deleted pages.
- **Agent file actions:** rename, move and move to Trash, confirmed first, never overwriting.
- **Quick links and snippets:** `:link`, `:snippet`, `!keyword`, and Save as Snippet on clips.

**0.2.6 changes**

- **Settings → Index:** a dedicated page showing what is indexed (by kind and top-level folder),
  what needs attention, recently changed files, activity and per-process resources. It replaces
  the text status that previously listed every counter in one paragraph.
- **Fixed continuous re-indexing:** a folder rewritten every second kept indexing permanently
  busy (measured on this Mac: 34 passes in 20 seconds and 15–28% CPU). Event-driven passes are
  now at least 10 seconds apart, the semantic helper starts only when notes or PDFs changed,
  and busy folders are listed with a one-click, confirmed exclusion.
- **One copy at a time:** a second copy of Blindspot — for example an unzipped build left in
  Downloads beside the installed app — now asks whether to quit the other copy or itself. Two
  copies indexing the same folders into one index caused the once-a-second passes seen before.

**0.2.5 changes**

- **Search by meaning rebuilt:** contextual on-device embeddings, prose/PDF-only eligibility,
  exact matches first with a capped *Related by meaning* tail, and a semantic cache that keeps
  serving while it is rebuilt (newer vectors are searched exactly meanwhile). Measured on a backup
  copy of this Mac's 41,296-document index: 2,007 eligible documents embedded in 43 s, cache built
  in 0.9 s (the previous cache held all 41k documents, 46 MB), warm semantic query about 8 ms.
- **PDF content indexing** through an isolated helper with per-document status and a size limit.
  On three real folders, 100 PDFs were extracted and 1 recorded as having no text.
- **Less background churn:** event-driven refreshes re-read only changed paths instead of the
  whole root; common dependency caches are excluded.
- **Visibility:** the indexing dashboard, helper processes and CPU % in App resources, and a
  corrected CPU-time conversion.
- **Settings:** Index PDF documents, Maximum document size and Low-impact indexing.
- **One-time upgrade effects:** schema 7 adds an extraction-status column and reuses existing
  documents. The first pass embeds eligible prose/PDF documents with the new model (their old
  sentence vectors are replaced; vectors stored for code and data files are left in place but
  unused) and removes index records under newly excluded dependency caches — 15,921 Go
  module-cache files on this Mac. The old semantic cache is replaced. Source files are never modified.

The Settings shortcuts, visible version, source-byte accounting and empty filtered-search
explanations from 0.2.4 remain available. Still not included: native iWork extraction, Office deep links, hard CPU/RAM
quotas, copying or deleting files from the agent, system-wide snippet expansion, creating or
editing calendar events, and changing system settings beyond the commands listed in section 9. The
related-by-meaning tail and document answers are approximate; check the cited sources.
