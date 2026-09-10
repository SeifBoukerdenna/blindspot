---
paths:
  - "shell/**/*.swift"
---

# AppKit shell rules

The shell owns the window and the keystrokes and nothing else. If you find yourself
writing ranking, matching, scoring, or indexing logic here, it belongs in `core/`.

## Panel invariants — do not silently change these

The panel is not a normal window. These settings are load-bearing and each one was
chosen for a reason:

- `.nonactivatingPanel` in the style mask, so showing the panel does not deactivate
  the frontmost app.
- `canBecomeKey` overridden to return `true`. Without it the text field is dead.
- `collectionBehavior` includes `.canJoinAllSpaces` and `.fullScreenAuxiliary`, so the
  panel appears on every Space and over fullscreen apps.
- `isFloatingPanel = true`.
- `hidesOnDeactivate = false`, and dismissal driven manually from `windowDidResignKey`.
  This one reads backwards, so do not "fix" it: `NSPanel` defaults it to `true`, and with
  it true the panel is composited exactly once. After the first dismiss hands activation
  to another app, every later `makeKeyAndOrderFront` reports complete success —
  `isVisible`, `isKeyWindow`, `NSApp.isActive`, `isOnActiveSpace` all true, correct frame
  — while `occlusionState` never regains `.visible`. A/B'd against a scripted
  show/dismiss/show harness: true gave occlusion true/false/false, false gave
  true/true/true. Apple's "How Panels Work" confirms AppKit itself orders panels out on
  deactivation through bookkeeping with no public accessor. Real Spotlight-style
  launchers (multi.app, artlasovsky.com) independently do the same thing.
- Capture `NSWorkspace.shared.frontmostApplication` on show and reactivate it on
  dismiss. Losing focus restoration is the single most annoying possible regression.
  Deliberately *not* `NSApp.hide(_:)`, which those same launchers use: its "next app in
  line" is the system's activation stack rather than the app you actually captured.
- Never trust `isVisible` to mean "on screen". `occlusionState.contains(.visible)` is the
  only property that answers that question.

If a change would touch any of the above, say so explicitly and explain why before
making it.

## Other

- The window is constructed once at launch and reused. Never build it on the hotkey
  path — that is the difference between instant and sluggish.
- The app is `LSUIElement`. No Dock icon, no menu bar, no main window.
- Icons come from `NSWorkspace.shared.icon(forFile:)` and are cached. Do not parse
  `.icns` anywhere.
- Prefer Carbon `RegisterEventHotKey` over a `CGEventTap`. The tap needs Accessibility
  permission and that is a much worse first-run experience.
- Keep this directory under ~800 lines total. Growth here means logic is leaking out
  of Rust.
