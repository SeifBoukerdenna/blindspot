# blindspot

A macOS launcher. Cmd+Space replacement. Instant, keyboard-only, no cloud, no account, no
subscription. Nothing it does leaves this machine.

The name is the thesis: it does the things Spotlight can't see, and it deliberately doesn't know anything about you beyond what you launch.

## Non-goals

Be aggressive about these. Scope creep is the failure mode for this kind of project.

- No plugin/extension API. Features get hardcoded. This is a single-user app.
- No remote network, no cloud, no account, no telemetry. Nothing blindspot does may leave
  this machine.
- Two local exceptions, both deliberate, both narrow:
  - **Text recognition on copied images** (Vision, on-device), chosen at M5: deterministic,
    no network, the same recogniser as Live Text.
  - **One local model over loopback** (Ollama on `127.0.0.1`), chosen at M7 for the agent.
    It is opt-in behind the `>` prefix, never touched on a keystroke, never asked anything
    but what you typed, and off entirely with `agent.enabled = false` — with that flag off,
    blindspot opens no socket at all. `agent.host` is checked to be loopback on every
    request: "local" is enforced, not merely configured.

  Neither is precedent for a cloud call, an account, or a model that reads your files.
- No cloud sync, no accounts, no telemetry.
- No cross-platform. macOS only, AppKit assumptions everywhere.
- ~~No settings UI until v2. Config is a TOML file.~~ — reversed at M8. There is a settings
  window, **and config.toml is still a TOML file that blindspot never writes.** It is read
  first and treated as the floor; the window writes `overrides.toml` next to the other state
  it keeps, and every row says which of the three — built-in, your file, this window — its
  value came from. The half of the rule that mattered is intact: the file stays yours.

## Architecture

Hybrid. Rust core as a static library, thin Swift/AppKit shell.

The split exists because Rust is better at the indexing/matching/ranking and worse at
fighting the window server. Keep the Swift side as dumb as possible — it should own
the window and the keystrokes, and nothing else.

```
blindspot/
├── core/                  # Rust. cargo, crate-type = ["staticlib"]
│   ├── src/
│   │   ├── lib.rs         # C ABI surface, #[no_mangle] extern "C" fns
│   │   ├── ffi.rs         # repr(C) structs, string marshalling, free fns
│   │   ├── index/
│   │   │   ├── apps.rs    # scan app dirs, parse Info.plist
│   │   │   └── mod.rs
│   │   ├── match.rs       # nucleo wrapper
│   │   ├── agent/         # M7: the local model client, and one request's state machine
│   │   ├── exec.rs        # M7: what may run, and running it
│   │   ├── frecency.rs    # launch history + scoring
│   │   └── store.rs       # persistence (redb or rusqlite)
│   └── Cargo.toml
├── shell/                 # Swift. Xcode project or SPM + xcodebuild
│   ├── AppDelegate.swift
│   ├── HotKey.swift       # RegisterEventHotKey
│   ├── Panel.swift        # NSPanel subclass
│   ├── ResultsView.swift
│   └── Bridge.swift       # calls into libblindspot_core.a
├── include/
│   └── blindspot.h        # hand-written or cbindgen-generated
└── Makefile               # cargo build --release && xcodebuild
```

### FFI surface

Keep it tiny. Roughly:

```c
BsHandle* bs_init(const char* config_path);
BsResults bs_query(BsHandle*, const char* query, size_t limit);
void      bs_activate(BsHandle*, uint64_t result_id);
void      bs_reindex(BsHandle*);
void      bs_free_results(BsResults);
void      bs_shutdown(BsHandle*);
```

Rules:
- Rust owns all allocations it hands out. Swift must call the matching free fn.
- No panics across the boundary. Wrap entry points in `catch_unwind`, return an error code.
- Results are a `repr(C)` struct with a pointer + length, not a Swift-side array copy per keystroke.

Use `cbindgen` to generate the header so it can't drift.

## Crates

- `nucleo` — fuzzy matching. Not `fuzzy-matcher`, not a hand-rolled Smith-Waterman.
  It has a proper streaming/incremental API and is what Helix uses.
- `plist` — parsing `Info.plist` for bundle display names and identifiers.
- `notify` — FSEvents wrapper for watching app directories.
- `redb` or `rusqlite` — frecency store. redb if you want zero C deps.
- `serde` + `toml` — config.
- `objc2` / `objc2-foundation` — only if the Rust side needs Cocoa directly. Prefer
  doing that in Swift instead.

## Milestones

Build in this order. Each one should be usable before starting the next.

**M1 — It opens things.**
Global hotkey, panel appears, type, fuzzy match over `/Applications`, Enter launches,
Esc dismisses. Ranking can be naive. This is the whole product; everything after is polish.

**M2 — It feels instant.**
`LSUIElement` background agent, window pre-warmed at launch (never construct it on
hotkey), index held in memory, query on every keystroke with no debounce. Target
sub-16ms query so it renders in one frame.

**M3 — It reads your mind.**
Frecency ranking. Typing "s" should give Slack because you open Slack constantly.
This is the single highest-leverage feature in the app and it's ~50 lines.

**M4 — Files.**
Shell out to `mdfind` first. Only build a custom index if mdfind proves inadequate —
it usually doesn't, and a custom FSEvents index is weeks of work plus a battery problem.

**M5 — Long tail.**
Calculator, clipboard history, snippets, window management. Each is independent.
Do them in the order you actually miss them.

**M7 — It does small chores, and answers.**
`>` asks a local model. A request that asks for something to be done comes back as shell
commands, shown, and run on Enter; anything else comes back as prose. The model chooses which,
in one reply — a schema with two branches, measured at 9 of 9 correct. ⌘M picks which model
answers, from what Ollama has installed. See the agent gotchas below: the measured model
numbers, why the working directory is parsed rather than asked for, and what the refusal rules
do and do not protect.

## Look

Flat plate, warm dark. Drawn from a canvas of two directions and built as the first of them
("Plate"): structure organises, and the accent is spent on exactly one thing per screen.
Every colour, size and font lives in `shell/Theme.swift` — nothing else in the shell may
name a colour.

- **Three colourways** — Ember (warm dark, the default and the one it was drawn in),
  Graphite (neutral dark), Cool paper (light) — picked in the settings window and
  remembered in `~/.local/share/blindspot/palette`, one line, the way the agent's model is.
  Not in config.toml and not across the FFI: *which palettes exist* is a fact about
  `Theme.swift`, and asking the core to describe an enum it cannot see would be backwards.
- **A `Palette` is ten colours, four alphas, and the four lines those alphas make.** Every
  value is **stored**, the derived ones included: `withAlphaComponent` allocates a new
  `NSColor` and `ResultRow` reads these tens of times per keystroke, so deriving on access
  would put fifty allocations on the frame budget. Measured before and after the refactor
  with byte-identical values — `results.update` 1.584–1.667 ms against a 1.593–1.662 ms
  baseline — and again across all three palettes, which land inside the same spread.
- **Lines are made of the type colour**: ink on paper, light on a dark plate. That is the
  one thing that has to move between a pale palette and a dark one.
- **The window appearance follows the palette**, not the system: `.darkAqua` for a dark
  plate, `.aqua` for a light one. Every system-drawn part — the field editor's selection,
  the overlay scroller — resolves against it, so a dark plate under a light-mode window
  gets a light selection band and a black scroller. Half of it tracking the system would be
  worse than none of it.
- **Changing palette rebuilds both windows rather than repainting them.** Row views are
  pooled and a `CALayer` bakes its colour when it is built, so nothing already on screen
  would change. ~40 ms, paid once, off the hotkey path. This is *why* the hotkey closures
  capture `[weak self]` and resolve the panel through the delegate: capturing a panel would
  leave the chord firing at a window that has been replaced.
- **Mode tabs across the top**, flush left, a picture of the query's prefix and nothing
  else. The right of that bar is for whatever the mode can say from what the core already
  handed over — a converted value's form, `⌘M` in the agent — and empty is a fine answer.
- **One heavy rule under the query**, hairlines everywhere else. This reverses M5's note
  that a divider between the field and the results was "the strongest 'not a Mac app'
  tell". It was, next to Liquid Glass. The panel is no longer imitating a system search
  surface, and the rule is what makes the masthead and the list read as one plate.
- **The accent appears once per screen**: a 3pt bar and a 16% wash on the selected row,
  the characters nucleo matched, the caret, the mode's prefix character. Nowhere else, and
  outcomes never. `warn` is where that bites: it means "left running", whose obvious colour
  on a warm plate is the accent's own amber, so it is a cool blue instead — the only cold
  thing on the screen, which is what makes it read as a state rather than a selection.
- **Application icons print grey except the selected row's.** Both forms are cached; the
  grey one is built alongside the colour one in `IconCache.prewarm`, off the hotkey path,
  because doing it lazily would put a Core Image render on the keystroke that selects a row.
- **`NSGlassEffectView` is gone**, with the 20pt Tahoe radius. An opaque layer-backed view
  at 6pt instead — square in the drawing, six points here so the plate does not fight the
  rounded corner of every other window on the screen. A material that samples the desktop
  would put a different colour under every hairline.
- **The prompt is not `placeholderString`.** A placeholder only shows on an empty field,
  and three of the four modes always hold at least their prefix, so it is a label drawn
  just past the caret.

**Matched characters come from the core, not from Swift.** `bs_query` returns `highlights`
— the offsets `Ranker::highlights` got from nucleo — because a subsequence match
re-derived in Swift would agree with nucleo most of the time and be quietly wrong the rest.
They are **`char` offsets, not bytes and not UTF-16 units**; `ResultRow` converts, with a
fast path for the names that are all one to one. `Pattern::indices` re-runs the match
keeping the position matrix, so it runs once per *shown* row after the cut, never inside
the ranking loop.

## Gotchas

These are the things that will eat a day each if you hit them cold.

**Cmd+Space cannot be taken programmatically.** Spotlight's binding lives in a
system-owned plist and macOS will not let you rebind it. The user unbinds it by hand in
System Settings → Keyboard → Keyboard Shortcuts → Spotlight, then blindspot registers
it. Document this in the README; it is not a bug.

**Hotkey registration.** Carbon's `RegisterEventHotKey` is deprecated-looking but still
the correct answer — it works with no special permissions. A `CGEventTap` would require
Accessibility permission, which is a much worse first-run experience. Don't reach for
the tap unless you need modifier-only or double-tap chords.

**The panel is not a normal window.** It needs:
- `NSPanel` with `.nonactivatingPanel` in the style mask
- `isFloatingPanel = true`, `level = .floating` (or `.popUpMenu` to sit above more)
- `collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]`
  so it appears over fullscreen apps and on every Space. Not `.stationary`: Apple
  documents `.managed`/`.transient`/`.stationary` as mutually exclusive, and `.transient`
  is already the default above the normal window level — that default is what keeps the
  panel out of Mission Control the way Spotlight is.
- `hidesOnDeactivate = false`, with dismissal driven manually from `windowDidResignKey`.
  Counter-intuitive and load-bearing — see `.claude/rules/appkit.md` for the measurements.
  With it `true` the panel is composited exactly once and never again.
- Override `canBecomeKey` to return `true` — a nonactivating panel won't take key
  focus otherwise and your text field will be dead.

**Focus restoration.** On dismiss, the previously-frontmost app must come back. Capture
it on show (`NSWorkspace.shared.frontmostApplication`) and reactivate on hide. Getting
this wrong is subtle and infuriating in daily use.

**App discovery paths.** Not just `/Applications`. Also `/System/Applications`,
`/System/Applications/Utilities`, `~/Applications`, and `/System/Library/CoreServices`.
Read `CFBundleDisplayName` falling back to `CFBundleName` falling back to the bundle
filename. Skip bundles with `LSUIElement`/`LSBackgroundOnly` set — those aren't launchable.

**Icons.** `NSWorkspace.shared.icon(forFile:)` on the Swift side. Cache them. Don't try
to parse `.icns` in Rust.

**TCC permissions cache by bundle ID and code signature.** During development,
rebuilding changes the signature and macOS may silently revoke previously-granted
permissions, or worse, keep stale grants. If permissions behave impossibly, that's why.
`tccutil reset All com.yourname.blindspot` is the reset button.

**Clipboard history (M5) has no push API.** Poll `NSPasteboard.general.changeCount` on
a timer (~200ms is fine). Respect the pasteboard markers so you don't capture password
manager output — this is a real privacy obligation, not a nicety. nspasteboard.org
defines four, not two: `org.nspasteboard.TransientType`, `ConcealedType`,
`AutoGeneratedType`, and 1Password's proprietary `com.agilebits.onepassword`. The spec
says concealed content should never be recorded to a file, and history persists to disk,
so marked copies are refused before their contents are even read.

**The clipboard permission alert is documented but, so far, not enforced.** `NSPasteboard.h`
says *"The default behavior for the General pasteboard is to ask upon programmatic
access,"* and a background poller never qualifies for the *"user originated and paste
related"* exemption. In practice, on macOS 26.4 (25E246), **it did not fire**: blindspot
recorded clips with no alert and no TCC prompt. Why is unknown — the header does not say
the behaviour is gated, and the pasteboard has no preview key in `defaults`. Treat it as
something a future macOS may switch on. If it does, a denied read returns nil and history
records nothing; nothing crashes. The Makefile signs with Developer ID so a grant would
survive rebuilds if one is ever needed — unverified, since none has been needed yet.

**The agent is a guardrail, not a sandbox.** `exec.rs` refuses elevation, deletion, command
substitution, background jobs, pipes into a shell, and any path outside the folder the request
named or inside `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/Library`. That blocks the obvious and cannot
block the clever. What actually protects you is that every command is shown in full and nothing
runs without a keypress. Every rule there came from a measured answer, not from imagination:
asked innocent questions, the default model proposed deleting `~/dev/api` outright ("start
fresh with an empty venv"), piping a downloaded installer into bash ("install homebrew"), and
`cd /dev` for a request that said `~/dev`.

**The model is never asked where to run — it is told.** Across five local models the working
directory was the field they got wrong most: one turned `~/dev` into `/dev`, another into
`~/.dev`, and several answered with the parent of what was asked for. `agent::directory_in`
resolves it from your own words instead — a path (`~/dev/api`), or the plain name of a folder
that really exists in your home, so "inside of desktop" means `~/Desktop` — and the resolved
path is then handed to the model as a `Working directory:` line. Measured: without that line
"in Developer make a folder called scratch" produced `/Developer`, the root, which the rules
then refused; with it, `mkdir -p scratch`.

**A command that never exits is a legitimate request.** `python3 -m http.server` does not
finish, and waiting for it to would mean sitting at "Running…" until the timeout killed it.
The running step therefore shows how long it has been going, and ↩ leaves it running — the
process is detached and outlives the panel — while ⎋ still stops it. Dropping a `Child` does
not signal it on Unix, which is what makes leaving it running a matter of not waiting.

**Every task is kept**, one JSON object per line in
`~/.local/share/blindspot/agent-history.jsonl`: the request, the model, the folder, the
commands, and what became of it. `>` with nothing typed lists them, newest first; ↩ puts a past
request back in the field rather than re-running it behind your back. Separate from
`agent.log`, which stays a running commentary for debugging.

**Naming a folder narrows the scope; it is not required.** With one named, commands may touch
only that folder. With none, they run from home and may touch anything under `roots` — still
never a protected folder, still shown before anything runs. Refusing a request for want of a
`~/` was refusing requests that were perfectly clear.

**Local model latency, measured** (48GB Apple Silicon, Ollama 0.33.3, ten chores each):

| model | cold load | command visible | whole reply | tok/s | correct |
| --- | --- | --- | --- | --- | --- |
| qwen3.5:0.8b-mlx | 1.3s | 0.26s | 0.58s | 123 | 9/10 basic, 7/10 hard |
| **qwen3.5:4b-mlx** (default) | 4.4s | **0.83s** | 2.55s | 42 | 10/10 both |
| qwen3.5:9b-mlx | 5.6s | 1.43s | 4.68s | 23 | 10/10 basic |
| qwen3.8:27b-mlx | 16.8s | ~2s | 8.61s | 8.5 | 10/10 basic |

`qwen3.5:2b-mlx` is disqualified: it silently corrupted paths (`~/dev` became `~/.dev`) while
producing correct-looking commands. What matters is "command visible", not the reply time — the
panel shows each command the moment its string closes in the stream, and the schema is trimmed
(command before why, no `dir`, no `summary`) because that halved both numbers. Streaming is also
why `keep_alive` is long: the cold load is otherwise paid again every five minutes.

**Two models, chosen by a rule that only picks who to ask.** A request naming a folder goes to
`model` (fast); anything else goes to `question_model`. The model itself still decides whether
to answer or propose commands, so the rule being wrong costs seconds, never correctness. ⌘M
overrides it from the models Ollama reports, remembered in
`~/.local/share/blindspot/agent-model` rather than written back into config.toml — that file is
the user's to edit. On these chores the 4B answered general questions correctly in 0.6s against
the 27B's ~8s, so the bigger default buys richer prose, not right answers.

**One sentence in the prompt is load-bearing.** Telling the model that *the launcher runs the
commands and it never needs access itself* is not politeness: without it, asked to clone a
repository into a folder with a long path, the 4B answered "I cannot access your local file
system" **ten times out of ten** — while the same request with a short path succeeded. Measure
prompt changes against real requests, including ugly ones, not against tidy examples.

## Conventions

- Rust: `cargo clippy -- -D warnings` clean. `unsafe` only in `ffi.rs`, and every
  `unsafe` block gets a `// SAFETY:` comment.
- No `unwrap()` outside tests. The core must never panic — a crash here kills the
  hotkey daemon and the user loses Cmd+Space entirely.
- Swift side stays under ~800 lines. If it's growing past that, logic is leaking out
  of Rust and should move back.
- Benchmark the query path. Add a criterion bench early; the whole value proposition
  is latency, and latency regressions are invisible until they aren't.

## Config

`~/.config/blindspot/config.toml`

```toml
hotkey = "cmd+shift+space"
max_results = 8
launch_at_login = true
app_paths = ["/Applications", "/System/Applications", "~/Applications"]

[frecency]
half_life_days = 14

[clips]
enabled = true                      # off stops the poller; nothing is recorded
keep = 200                          # oldest go first. The byte caps are not settable
images = true                       # off means text only
ocr = true                          # read text in an image, on-device, so it is searchable

[agent]
enabled = true
model = "qwen3.5:4b-mlx"            # chores: a request naming a folder
question_model = "qwen3.8:27b-mlx"  # everything else; empty means use `model` for both
host = "127.0.0.1:11434"            # loopback only, checked on every request
keep_alive = "30m"
timeout_secs = 120
roots = ["~"]                       # the only folders commands may touch
```

The default hotkey is **not** `cmd+space`. Spotlight owns ⌘Space until the user unbinds it
by hand, and Carbon reports success registering a chord another app already holds — so a
`cmd+space` default would register cleanly and then never fire. Set `hotkey = "cmd+space"`
after unbinding Spotlight's; blindspot warns at launch if Spotlight still has it. An
unparseable hotkey falls back to the default with a line on stderr, never a dead launcher.

**Nothing here needs a restart any more, except `agent.enabled`.** M8's settings window
rebinds the hotkeys through `HotKey.rebind`, swaps the config the core reads, and
re-derives the three things that hold a computed copy rather than reading it where they use
it — the frecency curve, the agent session's settings, and the clip cap. `agent.enabled` is
the deliberate exception: it decides whether a `Session` exists at all, and that is the flag
the "opens no socket" promise rests on.

**config.toml is still never written.** The window's values go to
`~/.local/share/blindspot/overrides.toml`, which is read on top of this file; deleting that
file undoes everything the window ever set. Every row in the window says which of the three
its value came from.

## Open questions

- ~~redb vs rusqlite for the frecency store~~ — settled at M3: **redb**. Measured, on its
  own "less friction" criterion: redb adds 1 transitive crate and no C build, rusqlite
  adds 11 and a `libsqlite3-sys` build. See the note at the top of `core/src/store.rs`.
- ~~`NSTableView` or hand-drawn results view~~ — hand-drawn through M5, then **`NSTableView`**
  once the list scrolled. Measured: refilling 50 stacked rows per keystroke took 1.56ms
  median and peaked at 21ms, while a table builds only the visible rows. Its cell reuse
  did not work for programmatic views (400 updates vended 2,950 rows), so `ResultsView`
  recycles rows itself via `tableView(_:didRemove:forRow:)` — keep that pool.
- Whether to sign/notarize. Not needed for personal use; needed the moment you want to
  give it to someone else without them fighting Gatekeeper.
