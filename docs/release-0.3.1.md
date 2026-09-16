# Blindspot 0.3.1

## What's new

- **Check a file…** in Settings → Index → Diagnostics explains why a selected file may be missing from content search, with relevant settings links.
- **⌘Y on a passage** opens a reading window with full stored passage text, literal-word highlights, source location and previous/next result navigation (⌘[ / ⌘]). View document retains PDF-page/Quick Look access.
- **⌘W closes Settings** without quitting, including while a text field is focused.

No index migration or rebuild. No automatic model downloads or changes to your selected folders.

## Use it

Try `:content bicycle maintenance`, select a passage and press ⌘Y. Browse results with ⌘[ / ⌘],
or use View document. If the expected file is missing, open Settings → Index → Diagnostics →
Check a file… and choose it. Use an example topic that exists in your own selected folders.

See [quick start](search-quickstart.md) and [complete cheatsheet](new-features.md).

## Publishing — manual

The local package is not a published GitHub release. Review the changes, commit and push to
`main` yourself, then run:

```sh
./scripts/release.sh 0.3.1
```

That script asks before creating/pushing the tag. A pushed `v0.3.1` tag starts the existing
GitHub Release workflow; pushing `main` alone only runs CI. Do not rerun it if the tag already
exists. No commits, tags, pushes, repository settings or releases are changed by this handoff.

## Limits

File diagnostics inspect metadata and existing records, not document contents or FTS integrity.
Unrecorded failures have no certain cause; unavailable paths have a grouped explanation.
Embedding counts describe the active generation, not model/cache readiness. Previews show a
single bounded indexed passage and the current result snapshot, not all neighboring chunks.
Literal highlights do not explain semantic matches. No notarization or Apple-server steps.

## Local delivery and validation — September 16, 2026

Installed and relaunched at `~/Applications/Blindspot.app`, version **0.3.1**. Strict recursive
code-signature verification passed. The build-path app was moved, not left as a second app.

- Signed package: `build/Blindspot-0.3.1.zip`.
- Complete guide: `build/Blindspot-0.3.1-Cheatsheet.md`.
- Verified checksums: `build/Blindspot-0.3.1-SHA256SUMS.txt`.
- Verified rollback archive: `build/rollback-0.3.1.GIa7cU/Blindspot-0.3.0.zip`.
- UI captures: `build/ui-03/settings-file-diagnostic.png` and `build/ui-03/passage-preview.png`.

Passed: four diagnostic Rust tests; the existing passage scope/filter/storage test; workspace
clippy with warnings denied; generated C-header comparison; whitespace check; release-note
generation; native panel/Settings/diagnostic/preview smoke checks; 58 Index dashboard cases;
existing PDF page navigation/cancellation and OCR checks; eight Office extraction cases.
The new reader checks cover Unicode highlighting, result navigation, stale loads, close/reopen
and unavailable passages. Screenshots of both new interfaces were reviewed.

The Settings fixture now runs under the real AppKit application loop; an early exit is not
accepted as completion. Settings also handles ⌘W directly on its window to avoid relying only
on accessory-app menu focus; the native Close menu remains available.

Before/after read-only live checks matched: schema 10, **209 documents, 5,245 passages,
2,255 passage embeddings, one active generation, 13 partial documents**. The database inode
and logical size were unchanged. Hashes of config.toml, local overrides and palette matched.
No live rescan, repair, rebuild or power-policy change was performed for verification.

Not run: the full Rust suite, stress/latency benchmarks, or a complete live-data UI walkthrough.
The local package step still reports four existing missing upstream license records in
`build/release-licenses/COLLECTION.txt`. GitHub CI/release validation remains part of your manual
publication workflow; this signed local build is not a published GitHub release.
