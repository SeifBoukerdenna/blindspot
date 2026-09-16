# Blindspot media

Screenshots and recordings of Blindspot's native macOS interface. These captures use
fictional Northwind documents, demo clipboard entries and saved snippets in an isolated
demo home. They were captured on September 15, 2026, with the repository at version 0.2.8.

[Back to the project](../README.md) · [Feature guide](../docs/new-features.md) ·
[Screenshots](#screenshots) · [Recordings](#recordings) · [Capture new media](#capture-new-media)

## Screenshots

The PNGs below are still images. Click any image to open the original at full resolution.

### Search inside documents

`documents about northwind` brings matching documents, excerpts and related results into
the launcher.

[![Document search with excerpts and related results](search-inside-documents.png)](search-inside-documents.png)

### Clipboard history

Type `;` to browse previous copies, then add words to narrow the results.

[![Clipboard history populated with fictional demo entries](clipboard-history.png)](clipboard-history.png)

### Index dashboard

Review indexing state, document categories, included folders and storage. The numbers
shown belong to the small demo fixture; they are not a performance benchmark.

[![Index dashboard with document counts, storage and folder breakdown](index-dashboard.png)](index-dashboard.png)

### Calculator and conversions

`1500MB` produces binary, decimal and byte representations that can be copied with Return.

[![1500 MB converted to GiB, GB and bytes](calculator.png)](calculator.png)

### Time zones

`3pm montreal in tokyo` converts between two places and shows both clocks, with daylight saving
taken from the zone files macOS already keeps. `time in paris` shows the time there.

[![Time zone conversion showing Tokyo, Montreal and the difference between them](time-zones.png)](time-zones.png)

### Unit conversions

`5 ft 11 in` answers in the other system. Temperatures, volumes, areas, speeds, pressure, energy,
power, angles, frequencies, data rates and fuel economy convert the same way, and `5 km to miles`
answers in one unit you name.

[![Feet and inches converted to metres and centimetres](units.png)](units.png)

### Command catalog

Type `:` to discover available commands and their descriptions.

[![Command catalog with content search and process commands](commands.png)](commands.png)

### System commands

`:system` lists Mac controls, including operations that require confirmation.

[![System command list with lock, sleep, restart and trash actions](system-commands.png)](system-commands.png)

### Saved snippets

Type `!` to find a saved snippet, or use its name directly, such as `!sig`.

[![Saved address, signature and thank-you snippets](snippets.png)](snippets.png)

### Ask your documents

`>docs When is the Northwind renewal due?` answered by the local model (qwen3.5 4B through
Ollama) from the demo documents: "October 31, 2026", citing the renewal proposal, with the
retrieved sources listed below the answer. It is one example, not a retrieval-quality benchmark.

[![Cited local answer to a question about the demo documents, with its source list](ask-your-documents.png)](ask-your-documents.png)

## Recordings

Expand a recording to view its animation, or use the direct GIF link.

<details>
<summary>Launcher workflow — apps, documents, conversions and clipboard</summary>

![Keyboard workflow switching between launcher features](launcher.gif)

[Original GIF](launcher.gif)

</details>

<details>
<summary>Document-question workflow — typed question, cited answer</summary>

![Typing a document question and receiving a cited local answer](ask-your-documents.gif)

[Original GIF](ask-your-documents.gif) · [Still image](ask-your-documents.png)

</details>

## Capture new media

The repository includes a capture harness and script:

- [bench/MediaCapture.swift](../bench/MediaCapture.swift) drives the native interface.
- [scripts/capture-media.sh](../scripts/capture-media.sh) prepares fictional documents
  and converts recordings to GIFs.

Run from the repository root:

```sh
make media
```

The capture workflow needs Screen Recording permission for the terminal, `ffmpeg`, and
the normal build tools. The AI scene also needs Ollama running with the configured local
model; the script skips that scene if the model is unavailable. `MEDIA_MODEL` selects an
already installed model, and `MEDIA_OUT` selects an output directory relative to the repo.

The script creates and removes `/Users/Shared/Demo`, and refuses to start if that path
already exists. It blocks Spotlight access for the harness and checks captured result
paths against the demo home. Indexing runs first, outside that sandbox, because the PDF and
Word extraction helper applies its own sandbox, which cannot nest; the capture refuses to start
until text from every demo PDF and Word file is searchable. Keep the mouse and keyboard idle
while capturing; changing focus can dismiss the launcher.

Before replacing these assets, inspect the screenshots and recordings for readable text,
clipped views, private data and the actual outcome shown. Caption failed or unanswered
states accurately. Keep filenames stable so README links continue to work.
