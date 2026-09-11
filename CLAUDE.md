# blindspot

A macOS launcher. Cmd+Space replacement. Instant, keyboard-only, no AI, no account, no subscription.

The name is the thesis: it does the things Spotlight can't see, and it deliberately doesn't know anything about you beyond what you launch.

## Non-goals

Be aggressive about these. Scope creep is the failure mode for this kind of project.

- No plugin/extension API. Features get hardcoded. This is a single-user app.
- No AI, no natural language queries, no network calls of any kind.
  Text recognition on copied images (Vision, on-device) is a deliberate exception, chosen
  at M5: deterministic, no network, the same recogniser as Live Text — not an AI feature.
  It is not precedent for anything generative or natural-language.
- No cloud sync, no accounts, no telemetry.
- No cross-platform. macOS only, AppKit assumptions everywhere.
- No settings UI until v2. Config is a TOML file.

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
```

The default hotkey is **not** `cmd+space`. Spotlight owns ⌘Space until the user unbinds it
by hand, and Carbon reports success registering a chord another app already holds — so a
`cmd+space` default would register cleanly and then never fire. Set `hotkey = "cmd+space"`
after unbinding Spotlight's; blindspot warns at launch if Spotlight still has it. An
unparseable hotkey falls back to the default with a line on stderr, never a dead launcher.
Both settings are read once at launch: quit and reopen after changing them.

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
