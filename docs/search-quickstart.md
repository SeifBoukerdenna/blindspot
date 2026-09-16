# Blindspot search — quick start

> **Current status:** The upgraded search and redesigned Index dashboard are in the working build, not yet installed. The copied-index repair passed; dashboard testing still needs to finish before the live upgrade. Some controls below will appear only after installation.

## 1. Choose what Blindspot can search

Open Blindspot and press **⌘,** for Settings, then select **Content**.

- Add the folders containing your documents and notes. Start with specific folders, not your entire home directory.
- Enable **Index PDF and Office documents** for PDF, Word, RTF, ODT, PowerPoint (`.pptx`) and Excel (`.xlsx`) text.
- Enable **Search by meaning** to find related ideas, even when the wording differs.
- For semantic code search, add projects under **Code folders**. These folders also become indexing roots.
- For scanned PDFs, enable **Read scanned PDF pages**. **OCR pages per PDF** limits the work.

Go to **Settings → Index → Refresh**. Let the indexing pass run; repeatedly refreshing restarts work.

## 2. Find something

Open Blindspot with your launcher shortcut—default **⌘⇧Space**—and try:

| Type this | Find this |
|---|---|
| `documents about genetec` | Relevant passages in documents |
| `:content video surveillance security cameras` | Indexed text matching words or related meaning |
| `:content kind:pdf genetec` | Search only PDFs |
| `:content kind:documents modified:month budget` | Documents changed in the last 30 days |
| `:content kind:code websocket` | Matching source code |
| `:content kind:xlsx renewal` | Cached workbook values and their sheet/cell labels |

Word matches may appear before semantic matches. Content search can show two distinct passages from the same file.

## 3. Use a matching passage

Select a result with **↑ / ↓**.

- **Return:** open the result; PDF passage results open at their indexed page.
- **⌘Y:** preview; PDF passages use the page-aware preview.
- **⌘K → Copy Passage:** copy the stored passage, not just the short preview text.
- **⌘K → Ask About This Passage…:** ask local AI about that passage.
- **⌘K → Open at Line N:** jump to a code line where a supported editor is available.

Word documents, slide decks and workbooks open as whole files, not at a specific paragraph, slide or cell.

## 4. Ask your documents a question

Type this and press **Return**:

```text
>docs what do my documents say about the Genetec renewal?
```

Blindspot retrieves passages and gives them to your local question model. Read the numbered sources to check the answer.

This requires an available local Ollama question model, selected in **Settings → Agent**. The question model and **Content → Embedding model** do different jobs. Blindspot does not automatically download either.

## 5. Read the Index dashboard

Open **Settings → Index**.

- **Documents / Passages:** files in the library and the smaller text sections stored for search.
- **Embedded:** passages with active-model vectors. Coverage is not a completion percentage: data and spreadsheets intentionally remain word-only.
- **Worker report:** indexing or semantic status; it may report the model or a fallback.
- **Needs attention:** extraction problems, partial documents and embedding failures.
- **Indexer activity:** files checked, updated or skipped, and embeddings written during the pass.
- **Storage / Resources:** database and vector-cache size, storage target, and sampled app/helper usage. Ollama usage is separate.

**Index pass complete** does not mean every passage has an embedding or that a database integrity check was performed.

## 6. If something is missing

1. Check that its folder is selected and not excluded.
2. Check **Needs attention** for locked, oversized, unreadable or partly indexed files.
3. Enable OCR if the PDF is a scan without selectable text.
4. If meaning search is unavailable, keep using word search. After starting Ollama, click **Refresh** to retry.
5. If the storage target is reached, increase **Index budget (MiB)**, narrow your folders, or use **Compact** to reclaim unused index space.

Compact asks for confirmation and does not delete your source files. The storage target is cooperative, not a hard quota. Everything described here processes locally.

For more shortcuts and workflows, see [the complete feature guide](new-features.md).
