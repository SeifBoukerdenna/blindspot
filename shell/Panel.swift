import AppKit

/// The launcher window.
///
/// Built once in `applicationDidFinishLaunching` and reused for the life of the
/// process. Constructing it on the hotkey path is the difference between instant and
/// sluggish, and it is also what makes the first-responder handling below necessary.
@MainActor
final class Panel: NSPanel, NSTextFieldDelegate, NSWindowDelegate {
    private static let width: CGFloat = 640
    private static let fieldHeight: CGFloat = 62

    /// Tahoe roughly doubled the system window corner radius; the old 12 read as a
    /// pre-Tahoe panel sitting next to modern ones. Apple publishes no exact value and
    /// `NSViewCornerConfiguration.containerConcentric` — the API that would answer this
    /// properly — is macOS 27+, so this stays a hardcoded number for now.
    private static let cornerRadius: CGFloat = 20

    private let core: Core
    private let field = NSTextField()
    private let results: ResultsView

    /// Captured on show so dismissal can hand focus back. Getting this wrong is the
    /// single most annoying possible regression in daily use.
    private var previousApp: NSRunningApplication?

    /// The screen y-coordinate of the panel's top edge, held fixed while the list grows
    /// and shrinks underneath it. Without this the panel appears to jump as you type.
    private var topEdge: CGFloat = 0

    /// The height last handed to `setFrame`, so an unchanged one can be skipped.
    private var lastHeight: CGFloat = -1

    init(core: Core) {
        self.core = core
        self.results = ResultsView(capacity: max(core.maxResults, 1))

        super.init(
            contentRect: NSRect(x: 0, y: 0, width: Self.width, height: Self.fieldHeight),
            // `.nonactivatingPanel` so showing the panel does not deactivate whatever
            // the user was working in. `.borderless` because there is no title bar —
            // which is exactly why `canBecomeKey` has to be overridden below.
            styleMask: [.nonactivatingPanel, .borderless],
            backing: .buffered,
            defer: false
        )

        isFloatingPanel = true
        level = .floating

        // `hidesOnDeactivate = false`, departing from the invariant list in
        // `.claude/rules/appkit.md`. This is not a preference — with it true, the panel
        // shows exactly once and is never composited again.
        //
        // Measured: after the first dismiss hands activation to another app, every later
        // `makeKeyAndOrderFront` reports total success — `isVisible`, `isKeyWindow`,
        // `NSApp.isActive` and `isOnActiveSpace` all true, alpha 1, correct frame — while
        // `occlusionState` never regains `.visible`. The window server simply never maps
        // it. A/B against a scripted show/dismiss/show harness: with the flag true,
        // occlusion went true/false/false; with it false, true/true/true. Ordering the
        // window with `orderFrontRegardless()` instead changed nothing, so the ordering
        // call was never the problem.
        //
        // The behaviour the flag existed for — panel disappears when you click away — is
        // preserved by `windowDidResignKey` below, so the invariant's *intent* survives
        // even though the flag does not.
        hidesOnDeactivate = false
        delegate = self

        // `.canJoinAllSpaces` puts the panel on every Space and `.fullScreenAuxiliary`
        // lets it join a fullscreen app's Space — window level alone cannot do that,
        // since level only orders windows *within* a Space.
        //
        // `.stationary` is deliberately absent, departing from the flag list in
        // CLAUDE.md's gotchas. Apple documents `.managed`/`.transient`/`.stationary` as
        // mutually exclusive, with `.transient` the automatic default for any window
        // above the normal level — which is what keeps a panel out of Mission Control
        // the way Spotlight is. `.stationary` would override that default and pin the
        // panel like a desktop icon instead.
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]

        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        isMovableByWindowBackground = false
        animationBehavior = .none

        buildContentView()
    }

    /// A borderless window has no title bar or resize bar, so AppKit's default answer
    /// here is `false` — and a panel that cannot become key has a dead text field.
    override var canBecomeKey: Bool { true }

    /// Left at AppKit's default for a panel. Key status is what routes keystrokes; main
    /// status is not, and claiming it would be a lie for an accessory app.
    override var canBecomeMain: Bool { false }

    private func buildContentView() {
        let container = NSView()

        field.placeholderString = "Search"
        // Regular, not light. Large light-weight system text is a Big Sur-era look and
        // reads as dated next to current system search fields.
        field.font = .systemFont(ofSize: 26, weight: .regular)
        field.isBezeled = false
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.lineBreakMode = .byTruncatingTail
        field.delegate = self
        field.translatesAutoresizingMaskIntoConstraints = false

        container.addSubview(field)
        container.addSubview(results)

        NSLayoutConstraint.activate([
            field.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 20),
            field.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -20),
            // The field takes its intrinsic text height and is centred in a
            // fixed-height band, rather than being stretched to fill it. An
            // `NSTextField` draws its text at the top of an oversized frame, so
            // constraining the height directly left the query hugging the ceiling with
            // a band of dead space under it.
            field.centerYAnchor.constraint(
                equalTo: container.topAnchor, constant: Self.fieldHeight / 2),

            // No separator between field and results. A ruled line is the single
            // strongest "not a Mac app" tell here — system search surfaces (Spotlight,
            // Safari's Smart Search, Finder) separate the query from its results with
            // spacing and material continuity, never a divider.
            results.topAnchor.constraint(
                equalTo: container.topAnchor, constant: Self.fieldHeight),
            results.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            results.trailingAnchor.constraint(equalTo: container.trailingAnchor),
        ])

        // Liquid Glass, the macOS 26 native panel material. Replaces an
        // `NSVisualEffectView` with `.hudWindow`, which was semantically wrong — that
        // material is for heads-up overlays like the volume indicator, not a search
        // surface. It also clips its own corners *and* the window shadow, which spares
        // us the `maskImage` dance a visual effect view needs to stop the shadow
        // haloing square past rounded corners.
        let glass = NSGlassEffectView()
        glass.cornerRadius = Self.cornerRadius
        glass.style = .regular
        glass.contentView = container

        contentView = glass
    }

    // MARK: - Showing and dismissing

    func toggle() {
        if isVisible {
            dismiss(restoringFocus: true)
        } else {
            show()
        }
    }

    func show() {
        // Captured before anything touches the window server, while the answer is still
        // whatever the user was actually in.
        previousApp = NSWorkspace.shared.frontmostApplication

        // Returns immediately; the walk runs on a thread inside Rust and swaps the new
        // list in when it finishes. An app installed since launch therefore shows up on
        // the *next* invocation, not this one.
        core.reindex()

        positionOnActiveScreen()

        // Order first, focus second, and set first responder on every single show.
        // `initialFirstResponder` fires only the first time a window is placed on
        // screen — for a pre-warmed panel reused forever that means once, and every
        // later invocation would come up with a dead field.
        makeKeyAndOrderFront(nil)
        makeFirstResponder(field)

        // After `makeFirstResponder`, so the live field editor is cleared too and not
        // just the cell's backing value.
        field.stringValue = ""
        refresh()
    }

    /// Guards against re-entry: `orderOut` makes the panel resign key, which calls
    /// `windowDidResignKey`, which dismisses.
    private var isDismissing = false

    /// Stands in for `hidesOnDeactivate`, which had to be turned off — see the note in
    /// `init`. Clicking into another app makes the panel resign key, and the panel should
    /// get out of the way exactly as it used to.
    ///
    /// Focus is deliberately *not* restored here: the user picked the app they clicked
    /// on, and reactivating whatever they came from would drag them back.
    func windowDidResignKey(_ notification: Notification) {
        dismiss(restoringFocus: false)
    }

    /// `restoringFocus` is false when dismissing because the user launched something:
    /// reactivating the app they came from would fight the app they just asked for.
    private func dismiss(restoringFocus: Bool) {
        guard !isDismissing else { return }
        isDismissing = true
        defer { isDismissing = false }

        orderOut(nil)

        // Order out before handing activation back. A nonactivating panel's key status
        // is independent of app activation, so a panel still ordered in can sit on top
        // of the app we just brought forward.
        if restoringFocus {
            _ = previousApp?.activate()
        }
        previousApp = nil

        // Deferred a turn so dismissal itself stays snappy, and done here rather than on
        // show because the panel is already off screen — this must never land on the
        // keystroke path. Paths already cached cost a dictionary lookup each, so the only
        // real work is for apps that appeared since the last rescan.
        Task { @MainActor [weak self] in
            guard let self else { return }
            IconCache.prewarm(self.core.allPaths())
        }
    }

    /// Esc. Reached through the responder chain from the field editor, which is why the
    /// window is the right place to catch it regardless of what holds focus.
    override func cancelOperation(_ sender: Any?) {
        dismiss(restoringFocus: true)
    }

    // MARK: - Query

    func controlTextDidChange(_ obj: Notification) {
        refresh()
    }

    private func refresh() {
        let text = field.stringValue
        // Nothing until the user types. An empty query returns the head of the index,
        // which is the same eight alphabetically-first apps every single time — noise,
        // not a starting point, and no real launcher shows it. It also means the panel
        // opens as one field with no rows and no icons to render.
        guard !text.trimmingCharacters(in: .whitespaces).isEmpty else {
            results.update([])
            layoutForResults()
            return
        }

        let (matches, pending) = core.query(text, limit: core.maxResults)
        results.update(matches)
        layoutForResults()

        // A file search is still running in Rust, so re-ask shortly. Polling rather than
        // a callback keeps the FFI at one entry point and keeps threads out of Swift; a
        // re-query costs ~0.4ms, so the poll is free next to the ~120ms `mdfind` it is
        // waiting on. Guarded on the text being unchanged so a stale poll cannot
        // overwrite what the user has since typed.
        if pending {
            Task { @MainActor [weak self] in
                try? await Task.sleep(for: .milliseconds(60))
                guard let self, self.isVisible, self.field.stringValue == text else { return }
                self.refresh()
            }
        }
    }

    private func launchSelected() {
        guard let match = results.selectedMatch else { return }

        // A no-op in the core today; the call site exists so M3's frecency work is a
        // pure-Rust change.
        core.activate(match.id)

        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = true
        // Reuse a running copy rather than starting a second one.
        configuration.createsNewApplicationInstance = false

        let path = match.path
        let url = URL(fileURLWithPath: path)
        switch match.kind {
        case .app:
            NSWorkspace.shared.openApplication(at: url, configuration: configuration) { _, error in
                // Runs off the main thread, so nothing here may touch AppKit.
                if let error {
                    NSLog("blindspot: could not launch %@: %@", path, error.localizedDescription)
                }
            }
        case .file:
            // Hands the file to whichever application owns it, rather than treating the
            // path as a bundle to launch.
            NSWorkspace.shared.open(url, configuration: configuration) { _, error in
                if let error {
                    NSLog("blindspot: could not open %@: %@", path, error.localizedDescription)
                }
            }
        }

        // Dismissed immediately rather than from the completion handler: the open is
        // asynchronous and the panel should not sit there while Launch Services works.
        dismiss(restoringFocus: false)
    }

    /// Enter, Up and Down have to be intercepted before the field editor acts on them —
    /// a newline is meaningless in a one-line search field, and the arrows should move
    /// the selection rather than the caret.
    func control(
        _ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector
    ) -> Bool {
        switch commandSelector {
        case #selector(NSResponder.moveUp(_:)):
            results.moveSelection(by: -1)
            return true
        case #selector(NSResponder.moveDown(_:)):
            results.moveSelection(by: 1)
            return true
        case #selector(NSResponder.insertNewline(_:)):
            launchSelected()
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            // Belt and braces alongside the window's `cancelOperation` override. Esc
            // should reach the window through the responder chain, but AppKit has a
            // long-standing habit of routing Esc in a text field into `complete:`
            // first (rdar://8967168), which would swallow it.
            dismiss(restoringFocus: true)
            return true
        default:
            return false
        }
    }

    // MARK: - Geometry

    private func positionOnActiveScreen() {
        // The screen under the pointer, not `NSScreen.main` — on a multi-monitor desk
        // the launcher should appear where you are looking.
        let mouse = NSEvent.mouseLocation
        let screen =
            NSScreen.screens.first { $0.frame.contains(mouse) } ?? NSScreen.main
            ?? NSScreen.screens.first
        guard let screen else { return }

        let visible = screen.visibleFrame
        // A little above centre: the eye lands high on the screen, and the results list
        // then grows downward into empty space.
        topEdge = visible.maxY - visible.height * 0.22
        setFrame(
            NSRect(
                x: visible.midX - Self.width / 2,
                y: topEdge - Self.fieldHeight,
                width: Self.width,
                height: Self.fieldHeight
            ),
            display: false
        )
        // The frame was just set outright, possibly on a different screen, so the
        // remembered height no longer describes what is on screen.
        lastHeight = Self.fieldHeight
    }

    /// Grows and shrinks downward, keeping the top edge where `positionOnActiveScreen`
    /// put it.
    private func layoutForResults() {
        let height = Self.fieldHeight + results.fittingHeight
        // Measured: across a realistic typing burst the height changes on only 4 of 12
        // keystrokes, while `setFrame(display:)` costs ~0.7ms median and 2.7ms at p99.
        guard height != lastHeight else { return }
        lastHeight = height
        setFrame(
            NSRect(x: frame.origin.x, y: topEdge - height, width: Self.width, height: height),
            display: true
        )
    }
}
