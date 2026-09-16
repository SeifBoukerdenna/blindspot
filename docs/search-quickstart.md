# Blindspot 0.3.0 — quick start

> **The translucent 0.3.0 interface uses your existing index.** No rebuild is needed for this UI update. Check **Settings → Index** for actual indexing and embedding status.

For future local builds, run `make install` from the repository root. It builds, signs and installs the app. `scripts/release.sh` handles the release workflow; running `scripts/install.sh` directly requires an already complete, freshly signed bundle.

## 1. Choose what Blindspot can search

Open Blindspot and press **⌘,** for Settings, then select **Content**.

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
- **⌘Y:** preview; PDF passages use the page-aware preview.
- **⌘K → Copy Passage:** copy the stored passage, not just the short preview text.
- **⌘K → Ask About This Passage…:** ask local AI about that passage.
- **⌘K → Open at Line N:** jump to a code line where a supported editor is available.

Word documents, slide decks and workbooks open as whole files, not at a specific paragraph, slide or cell.

## 4. Ask your documents a question

Type this and press **Return**:

```text
>docs which expenses are covered by the conference travel policy?
```

Blindspot retrieves passages and gives them to your local question model. Read the numbered sources to check the answer.

This requires an available local Ollama question model, selected in **Settings → AI** (formerly Agent). The question model and **Content → Embedding model** do different jobs. Blindspot does not automatically download either.

## 5. Read the Index dashboard

Open **Settings → Index**.

- **Overview:** the actual stage, time spent in that stage, current root folder, and entries checked/documents updated this pass. The activity bar means work is running—not a completion percentage. No reliable total or ETA is available yet.
- **Your library:** stored documents, passages, disk size and active-model embeddings, separate from the current pass. Embedding coverage is not completion: data and spreadsheets intentionally remain word-only.
- **Folders:** watched locations and indexed-folder counts. **Manage folders…** takes you straight to the folder controls in Content. Expand File types or Recently changed; click an indexed folder or recent file to reveal it in Finder.
- **Diagnostics:** extraction problems, partial documents and embedding failures. Expand Last reported pass, Storage or Resources for detailed counters, disk usage and sampled app/helper usage. Ollama usage is separate. **Compact…** is here too.
- **Indexing settings…:** jumps to the indexing controls in Content.

The three tabs stay visible while you scroll, and automatic updates keep your selected tab.
If work is **Paused**, the reason appears in full; indexing resumes automatically when the
power or thermal condition clears. Rescanning is not a resume control.

**Index pass complete** does not mean every passage has an embedding or that a database integrity check was performed.

## 6. If something is missing

1. Check that its folder is selected and not excluded.
2. Check **Index → Diagnostics → Needs attention** for locked, oversized, unreadable or partly indexed files.
3. Enable OCR if the PDF is a scan without selectable text.
4. If meaning search is unavailable, keep using word search. After starting Ollama, use **Index → Overview → Rescan folders** to retry once the current pass has stopped.
5. If the storage target is reached, increase **Index budget (MiB)**, narrow your folders, or use **Index → Diagnostics → Compact…** to reclaim unused index space.

Compact asks for confirmation and does not delete your source files. The storage target is cooperative, not a hard quota. Everything described here processes locally.

For more shortcuts and workflows, see [the complete feature guide](new-features.md).

## 7. Appearance and settings

Open Settings with **⌘,** and choose a section in the sidebar. Resize the window if you want
more room. **AI** replaces the old Agent tab; **About** replaces Status and contains updates.
**Appearance** retains your palettes. macOS **Reduce Transparency** or **Increase Contrast**
automatically uses an opaque surface, and **Reduce Motion** disables launcher/list animations.
Your saved settings, folder selections, clipboard history, and search shortcuts are unchanged.
