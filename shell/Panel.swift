import AppKit
import Carbon.HIToolbox

/// The launcher window.
///
/// Built once in `applicationDidFinishLaunching` and reused for the life of the
/// process. Constructing it on the hotkey path is the difference between instant and
/// sluggish, and it is also what makes the first-responder handling below necessary.
@MainActor
final class Panel: NSPanel, NSTextFieldDelegate, NSWindowDelegate {
    private static let width: CGFloat = 640

    /// Results fetched per query. `max_results` rows show at once; the rest scroll. Fifty
    /// because nobody scrolls further through a launcher, and the table only renders what
    /// is visible, so fetching more than fits costs nothing on the keystroke path.
    private static let resultLimit = 50
    private static let fieldHeight: CGFloat = 62

    /// Tahoe roughly doubled the system window corner radius; the old 12 read as a
    /// pre-Tahoe panel sitting next to modern ones. Apple publishes no exact value and
    /// `NSViewCornerConfiguration.containerConcentric` — the API that would answer this
    /// properly — is macOS 27+, so this stays a hardcoded number for now.
    private static let cornerRadius: CGFloat = 20

    private let core: Core
    private let watcher: ClipboardWatcher
    private let field = NSTextField()
    private let results: ResultsView

    /// The browse modes, each nothing more than the prefix that selects it — so typing `?`
    /// and pressing ⌘2 are the same act, and the chips keep no state of their own.
    private static let modes: [(prefix: String, symbol: String, name: String)] = [
        ("", "square.grid.2x2", "Apps"),
        ("?", "doc", "Files"),
        (";", "clipboard", "Clipboard"),
    ]
    private var chips: [NSButton] = []

    /// Captured on show so dismissal can hand focus back. Getting this wrong is the
    /// single most annoying possible regression in daily use.
    private var previousApp: NSRunningApplication?

    /// The screen y-coordinate of the panel's top edge, held fixed while the list grows
    /// and shrinks underneath it. Without this the panel appears to jump as you type.
    private var topEdge: CGFloat = 0
    private var leftEdge: CGFloat = 0

    /// A scroll view has no height of its own, so the list's is set explicitly — to its
    /// rows, capped at `max_results`, beyond which it scrolls.
    private lazy var resultsHeight = results.heightAnchor.constraint(equalToConstant: 0)

    init(core: Core, watcher: ClipboardWatcher) {
        self.core = core
        self.watcher = watcher
        self.results = ResultsView(visibleRows: core.maxResults)

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
        results.onActivate = { [weak self] in self?.launchSelected() }
        ActionHint.startTracking()
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
        // Pasted text keeps no newlines: a line copied from a terminal usually ends in one,
        // and a query with a line break in it matches nothing and draws on two lines.
        field.cell?.usesSingleLineMode = true
        field.delegate = self
        field.translatesAutoresizingMaskIntoConstraints = false

        chips = Self.modes.enumerated().map { index, mode in
            let chip = NSButton(
                image: NSImage(systemSymbolName: mode.symbol, accessibilityDescription: mode.name)
                    ?? NSImage(),
                target: self, action: #selector(chipClicked(_:)))
            chip.tag = index
            chip.bezelStyle = .glass
            chip.borderShape = .circle
            chip.toolTip = "\(mode.name)  ⌘\(index + 1)"
            // Clicking a chip must leave the caret in the field, or typing would stop.
            chip.refusesFirstResponder = true
            return chip
        }
        let chipBar = NSStackView(views: chips)
        chipBar.spacing = 6
        chipBar.translatesAutoresizingMaskIntoConstraints = false

        container.addSubview(field)
        container.addSubview(chipBar)
        container.addSubview(results)

        NSLayoutConstraint.activate([
            field.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 20),
            field.trailingAnchor.constraint(equalTo: chipBar.leadingAnchor, constant: -12),
            chipBar.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -16),
            chipBar.centerYAnchor.constraint(equalTo: field.centerYAnchor),
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
            results.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 8),
            results.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -8),
            resultsHeight,
        ])

        // Liquid Glass, the macOS 26 native panel material. Replaces an
        // `NSVisualEffectView` with `.hudWindow`, which was semantically wrong — that
        // material is for heads-up overlays like the volume indicator, not a search
        // surface.
        let glass = NSGlassEffectView()
        glass.cornerRadius = Self.cornerRadius
        glass.style = .regular
        glass.contentView = container

        // `cornerRadius` above rounds the glass material but not the window's alpha, so
        // the system shadow — and the one-pixel rim macOS draws with it — still traced
        // the rectangular frame, leaving a square outline showing past every rounded
        // corner. Contrary to what this code first assumed, `NSGlassEffectView` does not
        // clip the shadow. A/B'd on window captures: `invalidateShadow()` changed
        // nothing; a rounded, masking layer is what makes the rim follow the curve.
        glass.wantsLayer = true
        glass.layer?.cornerRadius = Self.cornerRadius
        glass.layer?.cornerCurve = .continuous
        glass.layer?.masksToBounds = true

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

        placeOnActiveScreen()

        // Emptied and laid out before ordering in, so the panel is placed and sized by a
        // single `setFrame` and appears at its final height. Placing it at field height
        // and growing it to the welcome screen after measured two resizes, 9.4ms of the
        // show path. Clearing before `makeFirstResponder` still clears the live field
        // editor: checked by typing, dismissing and re-showing.
        field.stringValue = ""
        refresh()

        // Order first, focus second, and set first responder on every single show.
        // `initialFirstResponder` fires only the first time a window is placed on
        // screen — for a pre-warmed panel reused forever that means once, and every
        // later invocation would come up with a dead field.
        makeKeyAndOrderFront(nil)
        makeFirstResponder(field)
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

    /// `poll` marks a re-query while an answer is still pending. An unchanged list is then
    /// left alone rather than re-applied, which would snap the highlight back to the top
    /// under a user who has already started pressing ↓.
    private func refresh(poll: Bool = false) {
        let text = field.stringValue
        let mode = Self.mode(of: text)
        for (index, chip) in chips.enumerated() {
            chip.tintProminence = index == mode ? .primary : .none
        }

        // An empty field is the welcome screen — suggested apps and recent files — which
        // reverses M2's "nothing until you type". That was right while the only thing an
        // empty query could return was the alphabetical head of the index.
        let (matches, pending) = core.query(text, limit: Self.resultLimit)
        if !(poll && matches == results.matches) {
            results.update(matches)
            layoutForResults()
        }

        // A file search is still running in Rust, so re-ask shortly. Polling rather than
        // a callback keeps the FFI at one entry point and keeps threads out of Swift; a
        // re-query costs ~0.4ms, so the poll is free next to the ~120ms `mdfind` it is
        // waiting on. Guarded on the text being unchanged so a stale poll cannot
        // overwrite what the user has since typed.
        if pending {
            Task { @MainActor [weak self] in
                try? await Task.sleep(for: .milliseconds(60))
                guard let self, self.isVisible, self.field.stringValue == text else { return }
                self.refresh(poll: true)
            }
        }
    }

    // MARK: - Actions

    /// What Enter does, by modifier. Each arrives by a different route — measured with a
    /// harness posting every combination to this panel's field: ↩ and ⇧↩ as
    /// `insertNewline:`, ⌥↩ as `insertNewlineIgnoringFieldEditor:`, ⌃↩ as
    /// `insertLineBreak:`, and ⌘↩ only to `performKeyEquivalent`, after which the field
    /// editor sees a bare `noop:` that says nothing about which key it was.
    private enum Action {
        case open, reveal, copyPath, quit
    }

    private func perform(_ action: Action) {
        guard let match = results.selectedMatch else { return }
        let hasPath = match.kind == .app || match.kind == .file

        switch action {
        case .open:
            launchSelected()
        case .reveal where hasPath:
            NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: match.path)])
            dismiss(restoringFocus: false)
        case .copyPath where hasPath:
            // Through `writeOwn`, so the path never lands in clipboard history.
            watcher.writeOwn { $0.setString(match.path, forType: .string) }
            dismiss(restoringFocus: true)
        case .quit where match.kind == .app:
            // Looked up afresh rather than trusting the hint's snapshot from show time.
            let target = ActionHint.resolved(match.path)
            let app = NSWorkspace.shared.runningApplications.first { app in
                guard let path = app.bundleURL?.path else { return false }
                return path == match.path || path == target
            }
            guard let app else {
                NSSound.beep()
                return
            }
            app.terminate()
            dismiss(restoringFocus: true)
        case .reveal, .copyPath, .quit:
            // A clip or a calculated value has no file behind it and no process to quit.
            NSSound.beep()
        }
    }

    /// ⌘↩ and ⌘1–3, none of which reach `doCommandBy` in a usable form — see `Action`.
    /// Matched by key code, so the mode keys sit on the number row whatever the layout.
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .subtracting([.numericPad, .function, .capsLock])
        guard flags == .command else { return super.performKeyEquivalent(with: event) }
        switch Int(event.keyCode) {
        case kVK_Return, kVK_ANSI_KeypadEnter:
            perform(.reveal)
        case kVK_ANSI_1: switchMode(to: 0)
        case kVK_ANSI_2: switchMode(to: 1)
        case kVK_ANSI_3: switchMode(to: 2)
        default: return super.performKeyEquivalent(with: event)
        }
        return true
    }

    // MARK: - Browse modes

    private static func mode(of text: String) -> Int {
        modes.indices.dropFirst().first { text.hasPrefix(modes[$0].prefix) } ?? 0
    }

    /// Swaps the query's prefix and keeps what was typed after it, so switching mode
    /// re-asks the same question of a different source.
    private func switchMode(to index: Int) {
        let text = field.stringValue
        let typed = text.dropFirst(Self.modes[Self.mode(of: text)].prefix.count)
            .drop { $0 == " " }
        field.stringValue = Self.modes[index].prefix + typed
        // Programmatic edits bypass `controlTextDidChange`, and leave the caret where it was.
        field.currentEditor()?.selectedRange = NSRange(
            location: (field.stringValue as NSString).length, length: 0)
        refresh()
    }

    @objc private func chipClicked(_ sender: NSButton) {
        switchMode(to: sender.tag)
    }

    private func launchSelected() {
        guard let match = results.selectedMatch else { return }

        switch match.kind {
        case .calc, .tool:
            // Copy and hand focus back, so the answer can be pasted straight into
            // whatever the user was working in. No `activate`: a calculated row has no
            // stable id, and recording one would put junk in the frecency store.
            watcher.writeOwn { $0.setString(match.name, forType: .string) }
            dismiss(restoringFocus: true)

        case .clipText, .clipImage:
            // Back onto the pasteboard, then focus returns so ⌘V pastes it. Not pasted
            // automatically: synthesising ⌘V needs Accessibility permission, a prompt
            // blindspot has so far never had to show.
            if let data = core.clipContent(match.id, part: .full) {
                watcher.writeOwn { pasteboard in
                    if match.kind == .clipText {
                        pasteboard.setString(String(decoding: data, as: UTF8.self), forType: .string)
                    } else if let image = NSImage(data: data) {
                        // `writeObjects` offers several representations, not just PNG,
                        // so apps that only accept TIFF still take the paste.
                        pasteboard.writeObjects([image])
                    }
                }
                core.activate(match.id)
            }
            dismiss(restoringFocus: true)

        case .app, .file:
            core.activate(match.id)

            let configuration = NSWorkspace.OpenConfiguration()
            configuration.activates = true
            // Reuse a running copy rather than starting a second one.
            configuration.createsNewApplicationInstance = false

            let path = match.path
            let url = URL(fileURLWithPath: path)
            // Runs off the main thread, so nothing in here may touch AppKit.
            let report: @Sendable (NSRunningApplication?, (any Error)?) -> Void = { _, error in
                if let error {
                    NSLog("blindspot: could not open %@: %@", path, error.localizedDescription)
                }
            }

            if match.kind == .app {
                NSWorkspace.shared.openApplication(
                    at: url, configuration: configuration, completionHandler: report)
            } else {
                // Hands the file to whichever application owns it, rather than treating
                // the path as a bundle to launch.
                NSWorkspace.shared.open(
                    url, configuration: configuration, completionHandler: report)
            }

            // Dismissed immediately rather than from the completion handler: the open is
            // asynchronous and the panel should not sit there while Launch Services works.
            dismiss(restoringFocus: false)

        case .header:
            // Unreachable: `selectedMatch` never returns a header.
            break
        }
    }

    /// Enter in all its forms, Up and Down have to be intercepted before the field editor
    /// acts on them — a newline is meaningless in a one-line search field, and the arrows
    /// should move the selection rather than the caret.
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
            perform(.open)
            return true
        case #selector(NSResponder.insertNewlineIgnoringFieldEditor(_:)):
            perform(.copyPath)
            return true
        case #selector(NSResponder.insertLineBreak(_:)):
            perform(.quit)
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

    /// Picks where the panel goes; the layout that follows sets the frame.
    private func placeOnActiveScreen() {
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
        leftEdge = visible.midX - Self.width / 2
    }

    /// Grows and shrinks downward, keeping the top edge where `placeOnActiveScreen` put it.
    private func layoutForResults() {
        resultsHeight.constant = results.fittingHeight
        let height = Self.fieldHeight + results.fittingHeight
        let target = NSRect(x: leftEdge, y: topEdge - height, width: Self.width, height: height)
        // Measured: across a realistic typing burst the height changes on only 4 of 12
        // keystrokes, while `setFrame(display:)` costs ~0.7ms median and 2.7ms at p99.
        guard target != frame else { return }
        // Drawn now only if on screen; ordering in draws it anyway.
        setFrame(target, display: isVisible)
    }
}
