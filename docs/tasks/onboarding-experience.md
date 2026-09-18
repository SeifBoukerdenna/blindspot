# Task: Download-to-first-use onboarding plan

Status: complete
Updated: 2026-09-17
Base: 510e5f5; working tree clean at start

## Objective and acceptance

- [x] Read current distribution, startup, configuration and setup surfaces.
- [x] Plan the journey from GitHub download to useful launcher, document and AI workflows.
- [x] Distinguish observed behavior, proposed changes and implementation validation.

This completes a planning task only. The proposed experience is not implemented.

## Scope and existing work

Owned file: this plan. No pre-existing edits.
Out of scope: app changes, publication, live settings/data changes, notarization,
model downloads, version bump and installation.

User-selected direction: **quick start with guided optional document search and AI setup**.
User refinement: guide Ollama installation/startup, first model download, hardware-based model
recommendations, indexing and Accessibility all the way to verified use. Assume the app minimum
OS/hardware requirements are met; do not add an OS eligibility questionnaire.
Other product choices below remain recommendations, not approved implementation.

## Current experience, verified from source

| Area | Current behavior | Implication |
| --- | --- | --- |
| Download | README, release generator and Makefile describe a ZIP containing the app and the complete guide as START-HERE.md. Workflow also publishes a cheatsheet and checksums. | The beginner document is currently the entire feature reference. Clearly distinguish the app ZIP from GitHub source archives. |
| Launch | AppDelegate creates the core, panel, settings and menu item, but does not normally show a first-run window. No Dock icon. | Successful launch can look like nothing happened. |
| Recovery | Reopening the app opens Settings; menu item exposes Settings, Reindex and Quit. | Keep these fallbacks; add a direct Open Blindspot action and resumable setup entry. |
| Defaults | config.rs enables login startup, clipboard history including images/OCR, and content indexing of Desktop/Downloads. Semantic search defaults off. | Several consequential behaviors begin without an explanatory choice. |
| Initialization | ffi.rs configures ContentService inside core initialization; AppDelegate starts clipboard watching and reconciles the login item. | First-run consent must gate work before these points, not after the welcome window appears. |
| AI | Model settings list installed loopback Ollama models, retain unavailable configured names and support refresh. Defaults name specific generative models. | A configured name is not evidence of a usable local model. |
| Search | Filename search, content word search, embeddings and generated answers have different dependencies. | One global “setup complete” or “index ready” indicator would mislead. |
| Documentation | README references Agent/Status; current feature guide and Settings use AI/About. | Align names across release, quick start and app. |

Sources: [AppDelegate](../../shell/AppDelegate.swift), [configuration and tests](../../core/src/config.rs),
[core initialization](../../core/src/ffi.rs), [Settings](../../shell/Settings.swift),
[menu item](../../shell/StatusItem.swift), [release generator](../../scripts/release-notes.py),
[release workflow](../../.github/workflows/release.yml), [Makefile](../../Makefile),
[feature guide](../new-features.md), [README](../../README.md).

Release workflow permits ad hoc signing if its certificate secret is absent. The identity of an
actual published artifact was not checked; do not promise stable permissions from source alone.

## Proposed journey

### 1. GitHub release: choose the correct download

Lead with “Download Blindspot for Apple silicon · macOS 26+” linking to the app ZIP.
Put installation immediately after the download, with changes and advanced verification below.
Explain that no account or AI model is needed to start. Show one small launcher example.
Label cheatsheet/checksums as supporting assets and distinguish GitHub's source-code archives.

Keep ZIP distribution for the first implementation. A DMG is a separate convenience decision,
not a prerequisite for fixing onboarding. It would not resolve first-launch security friction.

### 2. Install and open

Explain the extracted folder, moving Blindspot.app into Applications, and opening it.
Document both system Applications and the user's Applications folder, including the updater's
need for a writable install location. Avoid contradictory paths between help and error messages.

Keep the non-notarized limitation visible before download. Explain the normal blocked-open
recovery through System Settings → Privacy & Security → Open Anyway, after attempting launch.
Use Apple's supported instructions rather than a quarantine-removal command in the primary path:
[Apple: safely open apps](https://support.apple.com/en-gb/102445).
Do not imply this handles malware/damaged-app alerts or organizational restrictions.

If launched from Downloads or another temporary location, offer installation guidance before
login registration. Do not silently move the app. Automatic relocation is deferred from the first slice.
Retain duplicate-copy handling; validate that onboarding does not create competing instances.

Split the shipped short START-HERE.md from the complete feature reference, keeping both available.

### 3. Welcome: make successful launch visible

Automatically show a compact native window on a genuinely new installation:

> Welcome to Blindspot
> Open apps and find things on your Mac, from your keyboard.
> Press ⌘⇧Space whenever you need it. Blindspot lives in the menu bar.

Primary action: **Set up shortcut**. Secondary action: **Open Blindspot**.
The latter leaves optional features deferred and takes the user directly to the launcher.
No permission prompts or model requirements on this screen.

### 4. Quick setup: shortcut and startup

Show the current shortcut, a change control using existing settings behavior, and a real shortcut
test. Receiving the shortcut confirms that it works; successful Carbon registration alone does not.
Provide a clickable Open Blindspot fallback if the shortcut does not fire. Explain known Spotlight
conflicts without changing system shortcuts on the user's behalf.

Show **Open at login** as a visible choice, recommended on, committed only when the user continues.
If macOS requires approval, display the actual pending state and a Settings route. Do not report
success merely because the preference says true. Avoid startup registration from a temporary copy.

### 5. First useful action

Open the launcher with a small dismissible hint: **Type Safari, then press Return**.
Allow another installed application as the example if needed. A successful app-open dispatch and
the user seeing their app are the first-use check; no indexing or model wait is required.

Teach only: shortcut to return, Escape to dismiss, and ⌘, for Settings. Put command discovery,
previews and advanced syntax in contextual help after this first success.

Provide a persistent **Set up more features…** entry in Settings and the menu bar. Optional setup
can be opened immediately or later. Dismissing onboarding must not repeatedly interrupt launches.

### 6. Optional document search

Explain “Search words inside folders you choose.” Show Desktop and Downloads as suggestions,
with a folder chooser, removal and exclusions. Do not start reading content until the selection
is explicitly confirmed. Allow no folders and Skip. Do not broaden filename or agent scope as
a side effect of choosing content folders.

Start word indexing in the background and leave the launcher usable. Show distinct states:
not enabled, starting, indexing, searchable with work remaining, paused, and needs attention.
Use actual progress/counters without an invented completion time. A denied, empty, unsupported
or cloud-only folder needs a specific explanation and retry/change-folder action; none is a
reason to rebuild or erase the index. Avoid a blanket Full Disk Access requirement.

The guided page should say **Choose folders → Allow access if asked → Start indexing → Try a
search**. Explain before confirmation that text excerpts are stored locally and indexing can take
time. Show each selected folder's state and the existing pause/resume controls. Recommend a small
notes/documents folder for the first test. For cloud-only files, explain how to download them in
Finder and then retry; never automatically download the user's cloud library. Return from an
access change should recheck the affected folder without resetting completed work.

First success: search a phrase from a user-selected supported document and open its passage.
Optionally offer a clearly labeled bundled demo, explicitly selected, outside the user's real
index; it must not make their folders appear ready when they are not.

Offer **Search by meaning** afterward. Explain its separate embedding dependency, check the
available backend with existing diagnostics, and report the backend actually in use, including
Apple fallback. Word search remains useful while embeddings are unavailable or catching up.

### 7. Optional clipboard history

Explain what will be retained locally and where to pause or clear it before enabling capture.
Recommended fresh-install choice: off until enabled, with text history first and visible image/OCR
choices. Existing users retain their settings and stored history.

First success: copy an innocuous example, reopen Blindspot and find it in Clipboard mode.
Distinguish copying a result from automated paste, which may need Accessibility.
Turning capture off stops new reads; deleting existing history is a separate explicit action.

### 8. Optional local AI: guided from installation to a working answer

Choosing **Set up local AI** enters a resumable flow: **Install Ollama → Choose a model →
Download → Test → Try it**. Each step has Back and Finish later; keep the launcher usable.
Do not send the user away with only a documentation link. Return to the exact pending step.

#### Install and start Ollama

Detect an installed application separately from a responding service. If no app is found but
an existing compatible loopback service responds, reuse it; a failed request alone cannot prove
Ollama is uninstalled. Show **Download Ollama for Mac** linking to the official download, then
explicit instructions: open the downloaded DMG, drag Ollama into Applications, and open Ollama.
Show **Open Ollama** when installed and **Check connection**. On return to Blindspot, run bounded
connection/version checks and advance when successful. Do not require Homebrew or Terminal.
Explain any Ollama-owned first-launch dialog; Blindspot's HTTP integration does not require the
optional CLI link to be installed. See [Ollama macOS installation](https://docs.ollama.com/macos).

Keep installation user-driven rather than silently installing software. Report service stopped,
startup still pending, incompatible service version and port/connection errors separately where
there is evidence. Offer retry and specific recovery. Minimum Blindspot OS support is assumed;
Ollama version compatibility with the selected model still needs checking.

#### Recommend a model for this Mac

Read chip identity, unified memory and available storage locally through public platform APIs.
Show a plain-language reason, e.g. “Your Mac has 16 GB of memory. Start with this smaller model
so there is room for your other apps.” Do not transmit hardware details or collect serial numbers.
Memory is the primary capacity constraint; chip family affects likely speed. Use measured chip
profiles where available, and conservative fallback on unfamiliar chips. Consider current memory
pressure, bounded context length, embedding work and other loaded models, not only total RAM.

Initial candidate policy for validation (our recommendation heuristic, **not** vendor-certified
RAM requirements or measured latency claims):

| Unified memory | First-model candidate | Optional alternative |
| --- | --- | --- |
| 8 GB | `qwen3.5:2b` | `qwen3.5:0.8b` for a lighter workload; explain reduced answer quality |
| 16 GB | `qwen3.5:4b` | 2b when memory is busy or response time is unsatisfactory |
| 24–32 GB | `qwen3.5:9b` | 4b for faster interactions and more headroom |
| 48 GB or more | 9b remains the quick-start recommendation | 27b as an explicit quality-oriented experiment after a successful smaller-model setup |

The current official library lists roughly 2.7 GB, 3.4 GB and 6.6 GB downloads for the 2b, 4b
and 9b variants, respectively. These are download sizes, not total working memory. Verify exact
tag, quantization, runtime compatibility and size before shipping recommendations. Use a small,
versioned curated catalog bundled with Blindspot, not an unreviewed remote recommendation feed.
Prefer explicit tags over `latest`; never silently replace or update an installed model.
Source: [Ollama Qwen 3.5 library](https://ollama.com/library/qwen3.5).

Offer one recommendation plus **Smaller download** and **Choose an installed model**. If a suitable
model is already installed, offer reuse before another download. Start with a modest tested context
budget (4K as a validation candidate); do not use the advertised maximum context as the default.
Download size, context/KV memory and runtime overhead must all fit the capacity estimate.
Do not recommend the largest model simply because it can load. Exact thresholds, tags and chip
adjustments must pass representative writing/document-answer fixtures before implementation ships.

#### First model download

Before starting, show exact model name, approximate download size, storage needed with headroom,
and purpose. Copy: “This download needs internet. Your questions and documents stay on this Mac
when using this local model.” Button: **Download [model] · approximately [size]**.
This explicit action authorizes only that download. Never pull implicitly on query, launch or retry.

Preferred product flow: Blindspot requests the selected curated model through the local Ollama
pull API and displays its streaming progress. This is a proposed addition to the current
installed-model-only runtime: inference still uses installed models, and downloads occur only
inside explicit setup. See [Ollama model pull API](https://docs.ollama.com/api/pull).

Show preparing, downloading bytes/total when known, verification, installed, and error states.
Do not confuse an unknown-size stage with a stalled download or label downloaded bytes as ready.
Provide Cancel and Retry; preserve progress if the installed Ollama version supports resumption.
Verify actual cancellation/shared-pull behavior before promising that closing a connection stops
all Ollama download work. Closing the window must explicitly offer continued background download
or cancellation, and a running download remains discoverable from setup/settings.

Handle offline/interrupted transfers, insufficient space, unavailable tags and Ollama quitting.
Retry only on user action after an actionable failure. Do not delete existing models or partial
provider files automatically. If model storage is on an unknown/custom volume, say available space
could not be verified instead of certifying the wrong disk. An advanced fallback may show a
copyable `ollama pull <exact-tag>` command, but the main path must not require Terminal.

#### Prove readiness and configure both uses

After download, refresh installed models, verify this is a local model, then run a cancellable,
bounded fixed-prompt test. Show “Loading model for the first time” separately from generating.
Check successful generation, not just presence in the model list. If memory pressure/load failure
or poor responsiveness occurs, offer a smaller candidate and explain why; another download needs
another explicit action. Do not unload unrelated models used by other applications.

Configure both general/writing and question-answering roles to the chosen tested model for a new
user, unless they deliberately choose separate ones. Avoid leaving an unavailable question-model
default behind. Preserve existing users' selections unless they explicitly change them.

Ollama also supports cloud inference: a loopback endpoint alone does not prove local processing.
Restrict recommendations/downloads to known local artifacts; validate local metadata and reject
cloud-backed models, including aliases, before tests or private prompts. At implementation time,
verify the installed service's metadata/API can establish locality; fail closed when it cannot.
Provide guidance for Ollama's local-only mode without silently rewriting its global configuration.
See [Ollama privacy and local-only mode](https://docs.ollama.com/faq).

End with **Try a question** using a harmless example. For **Ask your documents**, additionally
verify selected folders have searchable passages, show the scope, and demonstrate opening a
citation. Empty/unfinished indexing must lead back to document setup, not another model download.

#### Optional meaning-search model

Explain separately: “The answer model writes responses. The search model helps find related
passages.” If the user enables meaning search, reuse a compatible installed embedding backend,
or offer an explicit separate download of the curated embedding model with its own size and
progress. `embeddinggemma:300m` is the current app default; verify its artifact and compatibility
before offering it. See [EmbeddingGemma library](https://ollama.com/library/embeddinggemma).

Check the embedding backend, then show embedding progress independently of word indexing.
If using Apple's available backend as fallback, name it accurately. Word search does not require
this download. Limit overlapping generation/embedding work on lower-memory Macs, preserving the
core's scheduling contracts; onboarding must not start competing jobs that make the launcher lag.

### 9. Permissions at the point of use

| Feature | Permission moment | Useful fallback |
| --- | --- | --- |
| Selected text / automated paste | Explain Accessibility when the user invokes that action | Manual copy/paste where supported |
| Calendar | Ask when opening calendar functionality | Other launcher features continue |
| Screen OCR | Explain Screen Recording on invocation | Cancel and retain normal launcher use |
| App control | Explain Automation for the particular target/action | Do not execute the blocked action; show recovery |
| Protected folders | Explain access when the selected folder cannot be read | Choose another folder or retry after granting access |

#### Guided Accessibility setup

Offer **Enable selected text and paste** within optional setup, as well as at first use. Explain
that Accessibility lets Blindspot read selected text and paste on the user's behalf; app launching,
ordinary search and local AI questions do not need it. Show **Enable Accessibility** and **Later**.

On the explicit enable action, request the standard permission and guide the user to **System
Settings → Privacy & Security → Accessibility → Blindspot → On**. If the entry is absent, explain
using **+** to select the installed Blindspot.app. Use a supported settings route with a written
fallback, and keep the guide visible/resumable while they switch apps. macOS handles any required
authentication; Blindspot never changes the toggle itself.

On return, recheck trust and show Enabled or Still needs access. Then offer an explicit controlled
test: select sample text in the setup window, check it can be read, and paste a sample only into
an identified test field after the user clicks Test. Preserve/restore clipboard contents if the
chosen test needs the pasteboard. A real cross-app test should be guided and user-triggered, never
paste into whichever unrelated app happens to be focused. A checked toggle is permission status;
the functional test establishes that this action works. Reopen the app only if the observed OS
state requires it, keeping the step resumable. Handle a stale entry after an identity change with
specific guidance; never reset TCC. See [Apple Accessibility access instructions](https://support.apple.com/guide/mac-help/allow-accessibility-apps-to-access-your-mac-mh43185/mac).

Do not treat all permissions as prerequisites or probe them by executing side effects. Returning
from System Settings should refresh relevant status. Denial is a supported state, not a failed
onboarding requiring repeated prompts.

### 10. Everyday use and later updates

The final setup view reports capabilities honestly: Launcher ready; Documents indexing/ready/off;
Clipboard on/off; Local AI ready/not configured; Meaning search ready/preparing/off.
Skipping a feature is a valid completed choice. No permanent warning badge for a declined feature.

Keep resume/help discoverable, preserve choices after restart, and avoid replaying onboarding on
updates. Explain manual updates under Settings → About → Updates. Updates preserve config and data;
permission repair may be needed when signing identity changes. Signing and notarization policy
remain unchanged by this proposal.

## Implementation boundaries and proposed phases

1. **Distribution copy:** align README/release instructions and current Settings names; create a
   short shipped quick start; update Makefile packaging, release checks and guide assertions for
   the new document split. Validate generated release text and archive contents using fixtures.
2. **First-run lifecycle:** add a small onboarding coordinator/window near AppDelegate; define
   persisted versioned setup choices; gate fresh-install background work in core initialization
   and shell startup; add quick setup, launcher handoff and menu/settings recovery entries.
3. **Optional setup:** reuse Content folder settings, Index diagnostics, model picker and settings
   override machinery. Add resumable feature states and explicit capability checks without
   introducing a second configuration system. Add hardware recommendation policy, explicit model
   pulls with progress/recovery, local-model verification and guided Accessibility testing.
4. **End-to-end validation and delivery:** test fresh/returning/interrupted states and actual
   downloaded-artifact first launch, then follow the repository's app delivery process for code.

These are future implementation phases, not authorization to implement in this planning pass.
Likely integration points: shell/AppDelegate.swift, shell/StatusItem.swift, shell/Settings.swift,
shell/Panel.swift, shell/Bridge.swift, core/src/ffi.rs, core/src/config.rs and core/src/settings.rs.
Read the exact settings persistence and FFI contracts before selecting the state schema.

Critical migration rule: a missing onboarding marker does not mean a new user. Existing config,
overrides or app data must preserve behavior and avoid forced onboarding. In ambiguous cases,
preserve state and expose setup voluntarily. New-install gating must happen before workers start.
Persist choices before enabling work; make restart after interruption safe and repeatable.
Keep progress data local and avoid storing document contents in onboarding records.

## Validation to require during implementation

- Fresh disposable home: visible welcome, launcher usable, no clipboard reads/content traversal
  or login registration before relevant choices; no initial permission cascade.
- Existing-user fixtures: unchanged roots, preferences, clips, index and model choices; missing
  onboarding marker does not reset or replay setup.
- Resume fixtures: close, quit, crash/relaunch between steps; failed persistence does not enable
  unconfirmed background work; completed and skipped choices remain stable.
- Shortcut conflict: actual key delivery, change shortcut, click fallback, hidden menu-bar case.
- Content: no folders, denied access, empty/unsupported/cloud-only folder, battery pause, word
  results before embedding completion, unavailable model and changed generation.
- AI: unreachable service, no models, missing selected model, separate question model, timeout,
  cancellation, invalid/non-loopback endpoint, and successful fixed-prompt test.
- Downloads: existing model reuse, exact consent scope, progress without a known total, interruption,
  low/custom-volume storage, provider quit, unsupported tag/version, retry/resume/cancel semantics,
  and no automatic model deletion or hidden pull during inference.
- Recommendation fixtures: 8/16/24/32/48+ GB, unknown chips, memory pressure, existing local models,
  runtime overhead and context budget. Validate candidate tiers before making fit/speed promises.
- Locality: cloud model, cloud alias and unavailable locality metadata fail closed; local tests do
  not transmit private input; no global Ollama configuration changes without explicit user action.
- Accessibility: granted/denied/not listed/stale identity, return from Settings, explicit test target,
  clipboard preservation and no accidental paste into another app.
- Native UI: keyboard-only navigation, VoiceOver, reduced transparency/motion, small display,
  permission denial and returning from System Settings.
- Distribution: extracted layout, short guide links, correct install location, duplicate copy,
  writable updater location, signed downloaded ZIP and preserved quarantine on a disposable
  Mac/user environment. Ordinary local builds do not reproduce downloaded first launch.
- Use focused existing checks plus new lifecycle fixtures; build and run required release/delivery
  checks for app changes. No stress benchmarks or production-data repair.

Success criteria: a user can launch an app without optional setup, knows how to reopen Blindspot,
understands what has been enabled, can finish chosen features without terminal use inside Blindspot,
and can recover from denied access or a missing local model. Time-to-first-action is a usability
test observation, not a performance promise. Collect through observed sessions, not telemetry.

## Verification

- `make agent-context`: passed; base 510e5f5, initial tree clean.
- Read source and repository docs listed above; checked Apple guidance for blocked first launch.
- `make agent-check SCOPE=docs`: passed documentation lint and diff whitespace checks on
  510e5f5 plus this new plan; report: build/agent/check-xprgz8qa. This verification entry was
  added afterward. This report covers the initial draft, not the later guided-setup expansion.
- App build/tests/install and real release download not run: no app changes; this is a source-based
  proposal, not a clean-machine runtime audit. No live data/config inspected or modified.
- Guided-setup expansion: checked official Ollama installation, model library, pull API and FAQ,
  plus Apple Accessibility guidance. `make agent-check SCOPE=docs` passed on the expanded plan
  (build/agent/check-8labl4if); only this verification record was added afterward. Hardware model
  tiers are proposed heuristics, not benchmark results. No software/models downloaded or installed.

## Next action

Review the expanded screen sequence and candidate model tiers with the user. Implementation must
validate hardware recommendations and provider download/locality semantics before shipping. Then
implement one agreed phase in its own task, preserving existing-user defaults and data.
