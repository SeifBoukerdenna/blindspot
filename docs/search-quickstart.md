# Search inside files with Blindspot

Start with [installation and onboarding](quick-start.md) if this is your first launch.
Existing installations retain their index and chosen folders; a normal update does not require
a rebuild. This guide covers the current search workflow.

## 1. Choose what Blindspot can search

For guided setup, open **Set up Blindspot… → Document search** and choose folders, then
**Start indexing**. For detailed controls, press **⌘,** for Settings and select **Content**;
ensure content indexing is enabled before requesting a scan.

- Add the folders containing your documents and notes. Start with specific folders, not your entire home directory.
- Enable **Index PDF and Office documents** for PDF, Word, RTF, ODT, PowerPoint (`.pptx`) and Excel (`.xlsx`) text.
- Enable **Search by meaning** to find related ideas, even when the wording differs.
- For semantic code search, add projects under **Code folders**. These folders also become indexing roots.
- For scanned PDFs, enable **Read scanned PDF pages**. **OCR pages per PDF** limits the work.

Go to **Settings → Index → Overview → Rescan folders**. The view updates automatically;
the button is unavailable while work is running or paused.

## 2. Find something

Choose **Documents** from the menu at the right of the search field to insert `:content `,
then type your topic. Choose **Apps**, **Files**, **Clipboard**, or **Assistant** to switch back.
The prefixes are still editable, and ⌘1–⌘4 keep their existing shortcuts.

Open Blindspot with your launcher shortcut—default **⌘⇧Space**—and try:

| Type this | Find this |
|---|---|
| `documents about community gardens` | Relevant passages about a topic |
| `:content reducing electricity use in winter` | Words or related ideas in your indexed text |
| `:content kind:pdf baggage allowance` | Airline rules in PDFs |
| `:content kind:documents modified:month volunteer onboarding` | Recently changed volunteer guides or notes |
| `:content kind:code retry backoff` | Source code handling retries |
| `:content kind:xlsx camping supplies` | Packing-list values and their sheet/cell labels |

These are example queries, not bundled documents. Substitute topics that exist in your selected folders.

Word matches may appear before semantic matches. Content search can show two distinct passages from the same file.

## 3. Use a matching passage

Select a result with **↑ / ↓**.

- **Return:** open the result; PDF passage results open at their indexed page.
- **⌘Y:** read the full stored passage with source location and query-word highlights. Use **← / →** buttons or **⌘[ / ⌘]** for previous/next passage results, including other files.
- **View document:** switch from the passage reader to the PDF page preview or whole-file Quick Look.
- **Escape / ⌘Y / ⌘W:** close the passage reader and return to your search.
- **⌘K → Why this result?:** see the recorded word, meaning or blended evidence and freshness status.
- **⌘K → Copy Passage:** copy the stored passage, not just the short preview text.
- **⌘K → Ask About This Passage…:** ask local AI about that passage.
- **⌘K → Open at Line N:** jump to a code line where a supported editor is available.

Word documents, slide decks and workbooks open as whole files, not at a specific paragraph, slide or cell.
The reader shows one bounded indexed passage, not the whole file. Highlights are literal word
matches; meaning-only results may have none. If a passage was replaced by reindexing, search again.

## 4. Ask your documents a question

Type this and press **Return**:

```text
>docs which expenses are covered by the conference travel policy?
```

Blindspot retrieves passages and gives them to your local question model. Read the numbered sources to check the answer.

This requires an installed local Ollama question model. **Set up Blindspot → Local AI** guides
installation, suggests a model for your Mac, and offers an explicitly confirmed download/test.
**Settings → AI** holds the advanced choices. The question model and **Content → Embedding model**
do different jobs; neither is silently downloaded.

## 5. Read the Index dashboard

Open **Settings → Index**.

- **Overview:** the actual stage, time spent in that stage, current root folder, and entries checked/documents updated this pass. Scanning has no known total. Once pending embeddings are counted, a separate meter shows passages attempted and remaining for that embedding pass—not completion of the whole index. Failed attempts are not counted as successful embeddings. There is no ETA.
- **Your library:** stored documents, passages, disk size and active-model embeddings, separate from the current pass. Embedding coverage is not completion: data and spreadsheets intentionally remain word-only.
- **Folders:** watched locations and indexed-folder counts. **Manage folders…** takes you straight to the folder controls in Content. Expand File types or Recently changed; click an indexed folder or recent file to reveal it in Finder.
- **Diagnostics:** extraction problems, partial documents and embedding failures. Expand Last reported pass, Storage or Resources for detailed counters, disk usage and sampled app/helper usage. Ollama usage is separate. **Compact…** is here too.
- **Indexing settings…:** jumps to the indexing controls in Content.

The three tabs stay visible while you scroll, and automatic updates keep your selected tab.
If work is **Paused**, the reason appears in full. A power-policy pause clears automatically;
a manual pause waits for **Resume indexing**. Rescanning is not a resume control.

**Index pass complete** does not mean every passage has an embedding or that a database integrity check was performed.

### Indexing controls and model checks

- **Overview → Pause indexing / Resume indexing:** pauses background indexing for this app session while existing results remain searchable. Completed documents and embeddings are reused on resume. Resuming does not override Low Power Mode, battery policy or thermal protection; quitting clears the manual pause.
- **Folders → Rescan:** scans just that watched root, including its subfolders. Other roots are not rescanned. Use the top-level **Rescan folders** button for all roots.
- **Diagnostics → Retry failed items:** retries recorded extraction problems and partial documents, then missing eligible embeddings. Successful files and vectors are reused. It does not raise OCR/size/storage limits or discover files that were never recorded; those need a folder rescan.
- **Overview → Check model:** sends a short fixed test phrase to your selected local embedding backend. Shows availability, dimensions, check duration and whether the name/revision/dimensions match an active generation. It does not send your documents, download a model or rebuild the index by itself.

If Ollama was unavailable—or Apple fallback was used—Blindspot checks again automatically,
starting at about 30 seconds and backing off to 15 minutes. Checks run in the background even
with Settings closed, and wait during pauses or active indexing. Once the selected model works,
an embedding-only recovery pass starts without a filesystem rescan. It keeps older search data
until the new generation is usable. A manual model check can bring a waiting recovery forward.

Compatibility checks do not pin an Ollama model's weights by digest; replacing weights under
the same name and dimensions is not detected by this check.

## 6. If something is missing

Start with **Settings → Index → Diagnostics → Check a file…**. Choose a source such as
`Bicycle route notes.md`. Blindspot checks folder scope, exclusions, type/size limits, last
extraction, whether source metadata changed, and active-generation embedding coverage. Read the
suggested fix; **Open relevant settings…** takes you to the applicable control. The check itself
changes nothing and does not read the document body, download it or send it to a model.

No indexed record does not identify a definite cause: it may not have been scanned, or a read or
encoding check may have skipped it. An indexed file is not guaranteed to rank for every query.
Recheck after changes; this is a snapshot, not a live file monitor.

1. Check that its folder is selected and not excluded.
2. Check **Index → Diagnostics → Needs attention** for locked, oversized, unreadable or partly indexed files.
3. Enable OCR if the PDF is a scan without selectable text.
4. If meaning search is unavailable, keep using word search. Start Ollama, then use **Index → Overview → Check model**, or leave automatic recovery to retry. Resume indexing if you paused it yourself.
5. If the storage target is reached, increase **Index budget (MiB)**, narrow your folders, or use **Index → Diagnostics → Compact…** to reclaim unused index space.

Compact asks for confirmation and does not delete your source files. The storage target is cooperative, not a hard quota. Everything described here processes locally.

For more shortcuts and workflows, see [the complete feature guide](new-features.md).

## 7. Appearance and settings

Open Settings with **⌘,** and choose a section in the sidebar. Resize the window if you want
more room. Press **⌘W** to close Settings without quitting Blindspot, including while editing
a text field. **AI** replaces the old Agent tab; **About** replaces Status and contains updates.
**Appearance** retains your palettes. macOS **Reduce Transparency** or **Increase Contrast**
automatically uses an opaque surface, and **Reduce Motion** disables launcher/list animations.
Your saved settings, folder selections, clipboard history, and search shortcuts are unchanged.
