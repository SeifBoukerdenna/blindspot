import AppKit
import Carbon.HIToolbox

/// The mode tabs, flush left along the top of the plate.
///
/// In the panel it is a picture of the query's prefix and nothing else: the prefix in the
/// field is the state, and clicking a tab types it. The settings window reuses it for its
/// sections, which is the whole reason the two read as one application. Not an `NSSegmentedControl`, which brings a
/// bezel, the system accent and a selection style — three of the things this design
/// exists to take out.
@MainActor
final class TabBar: NSView {
    private let titles: [String]
    private let tabs: [NSTextField]
    private let status = NSTextField(labelWithString: "")
    /// What the status slot already says. Re-setting an attributed string repaints it, and
    /// this is asked on every poll.
    private var shownStatus: String?
    /// Positioned by hand in `layout()`: it has to sit under whichever tab is current,
    /// and constraints that move between four anchors are more machinery than a frame.
    private let underline = PlateView(fill: Theme.accent)
    /// Readable from outside so the settings window can re-select the page it was on
    /// after a reload.
    private(set) var selected = 0

    var onSelect: ((Int) -> Void)?

    init(titles: [String]) {
        self.titles = titles
        self.tabs = titles.map { _ in NSTextField(labelWithString: "") }
        super.init(frame: .zero)

        wantsLayer = true
        layer?.backgroundColor = Theme.ground.cgColor
        translatesAutoresizingMaskIntoConstraints = false

        let row = NSStackView(views: tabs)
        row.spacing = 24
        row.translatesAutoresizingMaskIntoConstraints = false

        status.alignment = .right
        status.lineBreakMode = .byTruncatingHead
        status.translatesAutoresizingMaskIntoConstraints = false
        status.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        let edge = PlateView(fill: Theme.hairline)
        underline.translatesAutoresizingMaskIntoConstraints = true

        for view in [row, status, edge, underline] as [NSView] { addSubview(view) }

        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
            status.leadingAnchor.constraint(
                greaterThanOrEqualTo: row.trailingAnchor, constant: 16),
            status.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            status.centerYAnchor.constraint(equalTo: centerYAnchor),
            edge.leadingAnchor.constraint(equalTo: leadingAnchor),
            edge.trailingAnchor.constraint(equalTo: trailingAnchor),
            edge.bottomAnchor.constraint(equalTo: bottomAnchor),
            edge.heightAnchor.constraint(equalToConstant: 1),
            heightAnchor.constraint(equalToConstant: Theme.tabBarHeight),
        ])
        select(0)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    func select(_ index: Int) {
        selected = index
        for (at, tab) in tabs.enumerated() {
            tab.attributedStringValue = Theme.label(
                titles[at], size: 10, tracking: 0.15,
                color: at == index ? Theme.ink : Theme.faint,
                weight: at == index ? .semibold : .regular)
        }
        // The underline is placed from the selected label's frame, so the labels have to
        // have one. On a bar built moments ago they do not, and the underline lands at
        // zero — which is exactly what a rebuilt settings window showed.
        layoutSubtreeIfNeeded()
        needsLayout = true
    }

    /// What this mode can say about itself, at the right of the bar. Empty is a
    /// perfectly good answer and most modes give it.
    func show(status text: String) {
        guard text != shownStatus else { return }
        shownStatus = text
        status.attributedStringValue = Theme.label(
            text, size: 9.5, tracking: 0.14, color: Theme.faint)
    }

    override func layout() {
        super.layout()
        guard tabs.indices.contains(selected) else { return }
        let tab = tabs[selected].frame
        // y = 0 is the bottom in an unflipped view, which is where this belongs — over
        // the hairline, which was added first and so draws under it.
        underline.frame = NSRect(x: tab.minX, y: 0, width: tab.width, height: 2)
    }

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        // Generous horizontally: the labels are ten points of type in a thirty-point
        // band, and hitting one exactly is not a thing anyone should have to do.
        guard
            let index = tabs.firstIndex(where: {
                point.x >= $0.frame.minX - 8 && point.x <= $0.frame.maxX + 8
            })
        else { return }
        onSelect?(index)
    }
}

/// The launcher window.
///
/// Built once in `applicationDidFinishLaunching` and reused for the life of the
/// process. Constructing it on the hotkey path is the difference between instant and
/// sluggish, and it is also what makes the first-responder handling below necessary.
@MainActor
final class Panel: NSPanel, NSTextFieldDelegate, NSWindowDelegate {
    /// Results fetched per query. `max_results` rows show at once; the rest scroll. Fifty
    /// because nobody scrolls further through a launcher, and the table only renders what
    /// is visible, so fetching more than fits costs nothing on the keystroke path.
    private static let resultLimit = 50

    /// Everything above the results: the mode tabs, the query, and the rule under it.
    /// The panel grows and shrinks below this; this height never changes.
    private static let chromeHeight =
        Theme.tabBarHeight + Theme.fieldHeight + Theme.ruleHeight

    /// The query, and the mode's character in front of it. Mono for the prefix because
    /// `?` and `>` are things you type at a shell, and a point smaller because mono
    /// capitals set larger than the grotesk beside them at the same size.
    private static let queryFont = Theme.text(24, .medium)
    private static let prefixFont = Theme.mono(21, .medium)

    /// How the panel arrives and leaves. Short: a launcher that takes a quarter of a second to
    /// appear is a launcher you stop trusting.
    private static let openDuration: CFTimeInterval = 0.12
    private static let closeDuration: CFTimeInterval = 0.09
    /// It grows into place from slightly small. Scale rather than a slide, so it reads as the
    /// panel coming forward rather than sliding in from somewhere it does not live.
    private static let openScale: CGFloat = 0.97

    /// The drawing is dead square. Six points instead, so the plate does not fight the
    /// rounded corner of every other window on a Tahoe screen — the one place this
    /// departs from the canvas, and deliberately.
    private static let cornerRadius: CGFloat = 6

    private let core: Core
    private let watcher: ClipboardWatcher
    private let field = NSTextField()
    /// The prompt, drawn after the mode's character rather than by `placeholderString`:
    /// a placeholder only shows on an empty field, and in three of the four modes the
    /// field always holds at least its prefix.
    private let ghost = NSTextField(labelWithString: "")
    private let results: ResultsView
    private let preview = Preview()
    /// True while Quick Look is up. The preview takes key status, and without this the
    /// panel would tear itself down from `windowDidResignKey` with the query still in it.
    private var previewing = false

    /// The browse modes, each nothing more than the prefix that selects it — so typing `?`
    /// and pressing ⌘2 are the same act, and the tabs keep no state of their own.
    private static let modes: [(prefix: String, tab: String, prompt: String)] = [
        ("", "APPS", "Search"),
        ("?", "FILES", "Find a file"),
        (";", "CLIPS", "Search what you copied"),
        (">", "AGENT", "Ask, or say what to do and where"),
    ]
    private let tabBar = TabBar(titles: modes.map(\.tab))

    /// How far the prompt sits from the field's left edge: past the mode's character,
    /// where the caret is.
    private lazy var ghostOffset = ghost.leadingAnchor.constraint(
        equalTo: field.leadingAnchor, constant: 0)

    /// Captured on show so dismissal can hand focus back. Getting this wrong is the
    /// single most annoying possible regression in daily use.
    private var previousApp: NSRunningApplication?
    /// The labels the core gives intents that change existing files (`Intent::label`).
    private static let destructivePrefixes = ["Rename:", "Move:", "Move to Trash:"]
    private let schedule: ScheduleProvider
    private var context: LauncherContext?
    private var contextTask: Task<Void, Never>?
    private let actionRegistry: ActionRegistry
    private var offeredActions: [ResultAction] = []
    private var actionTarget: Match?
    private var actionTask: Task<Void, Never>?

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
        let schedule = ScheduleProvider()
        self.schedule = schedule
        self.actionRegistry = ActionRegistry(clipSink: core.clipSink, core: core, schedule: schedule)
        self.results = ResultsView(visibleRows: core.maxResults)

        super.init(
            contentRect: NSRect(x: 0, y: 0, width: Theme.panelWidth, height: Self.chromeHeight),
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

        // Pinned to the palette rather than to the system. Every system-drawn part of the
        // panel — the field editor's selection, the overlay scroller, a symbol image —
        // resolves against the window's appearance, so a dark plate under a light-mode
        // window gets a light selection band and a black scroller. Pinned rather than
        // followed: the palette is one fixed set of values, and half of it tracking the
        // system would be worse than none of it.
        appearance = NSAppearance(named: Theme.isDark ? .darkAqua : .aqua)

        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        isMovableByWindowBackground = false
        animationBehavior = .none

        buildContentView()
        results.onActivate = { [weak self] in self?.launchSelected() }
        preview.onClose = { [weak self] in self?.previewClosed() }
        ActionHint.startTracking()
    }

    /// A borderless window has no title bar or resize bar, so AppKit's default answer
    /// here is `false` — and a panel that cannot become key has a dead text field.
    override var canBecomeKey: Bool { true }

    /// Left at AppKit's default for a panel. Key status is what routes keystrokes; main
    /// status is not, and claiming it would be a lie for an accessory app.
    override var canBecomeMain: Bool { false }

    private func buildContentView() {
        // The plate. Opaque and layer-backed rather than `NSGlassEffectView`: this design
        // is a printed surface, and a material that samples the desktop behind it would
        // put a different colour under every hairline. Autoresizing left on, because a
        // window sizes its content view with `frame`, not with constraints.
        let container = PlateView(
            fill: Theme.surface, radius: Self.cornerRadius, border: Theme.border)
        container.translatesAutoresizingMaskIntoConstraints = true

        field.font = Self.queryFont
        field.textColor = Theme.ink
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

        ghost.font = Self.queryFont
        ghost.textColor = Theme.faint
        ghost.lineBreakMode = .byTruncatingTail
        ghost.translatesAutoresizingMaskIntoConstraints = false

        tabBar.onSelect = { [weak self] index in self?.switchMode(to: index) }

        // The one heavy line in the design, and the only thing separating the query from
        // its results. Reversing M5's note that a divider here is "the strongest 'not a
        // Mac app' tell" — which it was, next to Liquid Glass. This panel is not trying
        // to look like a system search surface any more, and the rule is what makes the
        // masthead and the list read as two parts of one plate.
        let rule = PlateView(fill: Theme.rule)

        for view in [tabBar, field, ghost, rule, results] as [NSView] {
            container.addSubview(view)
        }

        NSLayoutConstraint.activate([
            tabBar.topAnchor.constraint(equalTo: container.topAnchor),
            tabBar.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            tabBar.trailingAnchor.constraint(equalTo: container.trailingAnchor),

            field.leadingAnchor.constraint(
                equalTo: container.leadingAnchor, constant: Theme.gutter),
            field.trailingAnchor.constraint(
                equalTo: container.trailingAnchor, constant: -Theme.gutter),
            // The field takes its intrinsic text height and is centred in a
            // fixed-height band, rather than being stretched to fill it. An
            // `NSTextField` draws its text at the top of an oversized frame, so
            // constraining the height directly left the query hugging the ceiling with
            // a band of dead space under it.
            field.centerYAnchor.constraint(
                equalTo: container.topAnchor,
                constant: Theme.tabBarHeight + Theme.fieldHeight / 2),

            ghostOffset,
            ghost.centerYAnchor.constraint(equalTo: field.centerYAnchor),
            ghost.trailingAnchor.constraint(lessThanOrEqualTo: field.trailingAnchor),

            rule.topAnchor.constraint(
                equalTo: container.topAnchor,
                constant: Theme.tabBarHeight + Theme.fieldHeight),
            rule.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            rule.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            rule.heightAnchor.constraint(equalToConstant: Theme.ruleHeight),

            // Full bleed. A row carries its own twenty points of padding, which puts its
            // icon under the caret, and the hairline between two rows runs to both edges
            // of the plate rather than stopping short of them.
            results.topAnchor.constraint(equalTo: rule.bottomAnchor),
            results.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            results.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            resultsHeight,
        ])

        contentView = container
    }

    /// Colours the mode's character in the field, and keeps the caret on the accent.
    ///
    /// Attributes only — the string is never touched — so this cannot re-enter
    /// `controlTextDidChange`. Reapplied on every change because typing resets the field
    /// editor's attributes to the field's own, and the field's own are for the query.
    private func styleQuery() {
        guard let editor = field.currentEditor() as? NSTextView else { return }
        editor.insertionPointColor = Theme.accent
        guard let storage = editor.textStorage else { return }
        let length = (field.stringValue as NSString).length
        storage.beginEditing()
        storage.addAttributes(
            [.font: Self.queryFont, .foregroundColor: Theme.ink],
            range: NSRange(location: 0, length: length))
        if length > 0, !Self.modes[Self.mode(of: field.stringValue)].prefix.isEmpty {
            storage.addAttributes(
                [.font: Self.prefixFont, .foregroundColor: Theme.accent],
                range: NSRange(location: 0, length: 1))
        }
        storage.endEditing()
    }

    /// Places and fills the prompt: shown only while nothing has been typed past the
    /// mode's character, and offset by exactly as much room as that character takes.
    private func layoutPrompt(mode: Int, text: String) {
        let prefix = Self.modes[mode].prefix
        ghost.isHidden = text.count > prefix.count
        guard !ghost.isHidden else { return }
        ghost.stringValue = Self.modes[mode].prompt
        // Three points clear of the caret even with no prefix, so the caret blinks beside
        // the prompt rather than through its first letter.
        ghostOffset.constant =
            prefix.isEmpty
            ? 3
            : (prefix as NSString).size(withAttributes: [.font: Self.prefixFont]).width + 10
    }

    // MARK: - Showing and dismissing

    func toggle() {
        if isVisible {
            dismiss(restoringFocus: true)
        } else {
            show()
        }
    }

    /// The second hotkey: straight into the agent, as if `>` had been typed.
    ///
    /// Pressed while the agent is already showing, it closes — the same key does the same
    /// thing twice. Pressed while blindspot is showing something else, it switches mode
    /// rather than closing, which is what the keystroke asked for.
    func toggleAgent() {
        let alreadyThere = isVisible && Self.mode(of: field.stringValue) == 3
        if alreadyThere {
            dismiss(restoringFocus: true)
            return
        }
        if !isVisible {
            show()
        }
        switchMode(to: 3)
    }

    func show() {
        let interval = Diagnostics.performance.beginInterval("LauncherShow")
        defer { Diagnostics.performance.endInterval("LauncherShow", interval) }
        // Captured before anything touches the window server, while the answer is still
        // whatever the user was actually in.
        previousApp = NSWorkspace.shared.frontmostApplication
        contextTask?.cancel()
        context = nil
        if let app = previousApp {
            let id = app.bundleIdentifier
            let pid = app.processIdentifier
            contextTask = Task { @MainActor [weak self] in
                let snapshot = await LauncherContext.capture(applicationID: id, processID: pid)
                guard !Task.isCancelled, let self, self.isVisible else { return }
                self.context = snapshot
                if LauncherContext.isTextCommand(self.field.stringValue) || self.core.promptInstruction(for: self.field.stringValue) != nil || LocalRequest.parse(self.field.stringValue) != nil { self.refresh(poll: true) }
            }
        }

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
        entranceDue = true
        refresh()

        // Order first, focus second, and set first responder on every single show.
        // `initialFirstResponder` fires only the first time a window is placed on
        // screen — for a pre-warmed panel reused forever that means once, and every
        // later invocation would come up with a dead field.
        makeKeyAndOrderFront(nil)
        makeFirstResponder(field)
        animateIn()
    }

    /// Fades and scales the panel in. The window is already on screen when this runs, so the
    /// first frame is drawn at full size and then scaled — which is why the starting alpha is
    /// set before `makeKeyAndOrderFront` would otherwise show it.
    private func animateIn() {
        guard let layer = contentView?.layer else { return }
        alphaValue = 0
        layer.transform = Self.scaled(layer, by: Self.openScale)
        NSAnimationContext.runAnimationGroup { context in
            context.duration = Self.openDuration
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            animator().alphaValue = 1
            layer.transform = CATransform3DIdentity
        }
    }

    /// Scales a layer about its own centre.
    ///
    /// Around the centre by translating rather than by moving `anchorPoint`: changing the
    /// anchor point *moves* a layer, and the panel's content stayed displaced by half its size
    /// after the animation had finished.
    private static func scaled(_ layer: CALayer, by scale: CGFloat) -> CATransform3D {
        let bounds = layer.bounds
        var transform = CATransform3DMakeTranslation(bounds.midX, bounds.midY, 0)
        transform = CATransform3DScale(transform, scale, scale, 1)
        return CATransform3DTranslate(transform, -bounds.midX, -bounds.midY, 0)
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
        // Quick Look took key, not another application. Going away here would close the
        // panel behind the preview and lose the query that found the file.
        guard !previewing else { return }
        dismiss(restoringFocus: false)
    }

    /// Quick Look went away, by whatever route. The panel takes key back with the query
    /// and the selection exactly as they were.
    private func previewClosed() {
        previewing = false
        guard isVisible else { return }
        makeKeyAndOrderFront(nil)
        makeFirstResponder(field)
    }

    /// ⌘Y. Only a row with something on disk behind it — a calculated value and a clip
    /// have no file to look at.
    private func previewSelected() {
        guard let match = results.selectedMatch, match.kind == .app || match.kind == .file
        else {
            NSSound.beep()
            return
        }
        // Raised *before* the preview opens, not after: `toggle` orders Quick Look in and
        // takes key status before it returns, so a flag set on the way out would arrive
        // after `windowDidResignKey` had already dismissed the panel. Measured exactly
        // that way round.
        previewing = true
        previewing = preview.toggle(match.path, page: match.page)
    }

    /// `restoringFocus` is false when dismissing because the user launched something:
    /// reactivating the app they came from would fight the app they just asked for.
    private func dismiss(restoringFocus: Bool) {
        guard !isDismissing else { return }
        actionTask?.cancel()
        contextTask?.cancel()
        contextTask = nil
        context = nil
        isDismissing = true
        pollTask?.cancel()
        pollTask = nil
        core.cancelSearch()
        defer { isDismissing = false }

        // Faded out when you dismissed it, instant when you launched something: there the app
        // you asked for is already coming forward, and a panel lingering over it reads as lag.
        if restoringFocus, isVisible {
            fadeOut()
        } else {
            orderOut(nil)
        }

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

    /// Hides the panel after a short fade. `orderOut` happens at the end, so the window is
    /// really gone rather than merely transparent — `windowDidResignKey` and the show path
    /// both depend on `isVisible` meaning what it says.
    private func fadeOut() {
        let contentLayer = contentView?.layer
        NSAnimationContext.runAnimationGroup { context in
            context.duration = Self.closeDuration
            context.timingFunction = CAMediaTimingFunction(name: .easeIn)
            animator().alphaValue = 0
            if let contentLayer {
                contentLayer.transform = Self.scaled(contentLayer, by: Self.openScale)
            }
        } completionHandler: { [weak self] in
            // The completion runs on the main queue but carries no isolation of its own.
            MainActor.assumeIsolated {
                guard let self else { return }
                self.orderOut(nil)
                self.alphaValue = 1
                self.contentView?.layer?.transform = CATransform3DIdentity
            }
        }
    }

    /// Takes up a setting the panel holds a copy of rather than asking the core per query.
    ///
    /// Only `max_results`: everything else the panel draws is read from the core on the
    /// keystroke that needs it, so a swapped config is already in force.
    func applySettings() {
        results.show(atMost: core.maxResults)
        guard isVisible else { return }
        refresh()
        layoutForResults()
    }

    func refreshContentResults() {
        let text = field.stringValue
        guard isVisible, !agentWorking, !text.hasPrefix(">"), !text.hasPrefix(";"),
            (!text.hasPrefix(":") || text.hasPrefix(":content")),
            !LauncherContext.isTextCommand(text), core.promptInstruction(for: text) == nil else { return }
        refresh(poll: true)
    }

    /// Gets out of the way so another window can take key.
    ///
    /// Deliberately not restoring focus: the user asked for the settings window, and
    /// reactivating whatever they came from would drag it back over the thing they just
    /// opened. The panel would dismiss itself from `windowDidResignKey` anyway — doing it
    /// first means the two are not racing over who is key.
    func standDown() {
        guard isVisible else { return }
        dismiss(restoringFocus: false)
    }

    /// Esc. Reached through the responder chain from the field editor, which is why the
    /// window is the right place to catch it regardless of what holds focus.
    override func cancelOperation(_ sender: Any?) {
        // Esc stops the agent first and dismisses second: a model mid-answer or a command
        // mid-run is the thing you want stopped, and the panel is where you see that it was.
        if agentWorking {
            core.agentCancel()
            refresh()
            return
        }
        dismiss(restoringFocus: true)
    }

    /// What is in the field. Read by the latency harness and the navigation checks.
    var query: String { field.stringValue }

    /// Opens settings. Held rather than dispatched through the responder chain — see the
    /// `kVK_ANSI_Comma` case in `performKeyEquivalent`.
    var onSettings: (() -> Void)?
    var onOpenSetting: ((String) -> Void)?

    /// True while the agent is generating or running, which is what makes Esc mean "stop".
    private var agentWorking = false
    private var pollTask: Task<Void, Never>?

    /// Set when the next refresh is one whose rows should animate in — a show, or a mode
    /// change — rather than the steady replacement typing does.
    private var entranceDue = false

    /// The typed request with the agent's prefix taken off, matching what the core does.
    private static func request(in text: String) -> String {
        guard text.hasPrefix(">") else { return text }
        return String(text.dropFirst().drop { $0 == " " })
    }

    // MARK: - Query

    func controlTextDidChange(_ obj: Notification) {
        refresh()
    }

    /// `poll` marks a re-query while an answer is still pending. An unchanged list is then
    /// left alone rather than re-applied, which would snap the highlight back to the top
    /// under a user who has already started pressing ↓.
    private func refresh(poll: Bool = false) {
        let interval = Diagnostics.performance.beginInterval("SearchRefresh")
        defer { Diagnostics.performance.endInterval("SearchRefresh", interval) }
        pollTask?.cancel()
        pollTask = nil
        let text = field.stringValue
        let mode = Self.mode(of: text)

        // None of the chrome can have moved on a poll: the poll path is guarded on the
        // text being unchanged, and the mode, the prompt and the query's styling are all
        // derived from it. Doing it anyway forced a layout of the tab bar and a mutation
        // of the field editor's storage sixteen times a second while the model streamed —
        // which is what made a generation flicker.
        if !poll {
            tabBar.select(mode)
            layoutPrompt(mode: mode, text: text)
            styleQuery()
        }

        // An empty field is the welcome screen — suggested apps and recent files — which
        // reverses M2's "nothing until you type". That was right while the only thing an
        // empty query could return was the alphabetical head of the index.
        let contextual = LauncherContext.isTextCommand(text) || core.promptInstruction(for: text) != nil
        let plan = LocalRequest.parse(text)
        let effectiveQuery = plan?.query ?? (contextual ? ">" + text : text)
        var (matches, pending) = core.query(effectiveQuery, limit: Self.resultLimit)
        if plan == .currentProject {
            if let url = context?.projectDirectory {
                matches = [Match(id: 0, name: url.lastPathComponent, kind: .file, path: url.path,
                    score: 0, timestamp: 0, width: 0, height: 0, detail: "Current folder", highlights: [])]
            } else {
                matches = [Match(id: 0, name: "Read current folder", kind: .command, path: "open current project",
                    score: 0, timestamp: 0, width: 0, height: 0, detail: "Return requests context access", highlights: [])]
            }
            pending = false
        }
        if plan == .schedule {
            schedule.onChange = { [weak self] in self?.refresh() }
            matches = schedule.rows()
            pending = false
        }
        if case .createEvent(let draft) = plan {
            schedule.onChange = { [weak self] in self?.refresh() }
            matches = schedule.draftRows(draft)
            pending = false
        }
        tabBar.show(status: plan?.status ?? Self.status(for: mode, in: matches))
        agentWorking = pending && effectiveQuery.hasPrefix(">")
        if !(poll && matches == results.matches) {
            // Rows animate in where rows actually *arrive*: the panel opening, a change of
            // mode, and a new step landing. Counting rows rather than asking "is this a
            // poll in agent mode" — the model streams characters, so the command on the
            // last row grows on nearly every poll, and the old test replayed the whole
            // table's fade sixteen times a second. That was the flicker.
            let arrived = poll && matches.count > results.matches.count
            let entering = entranceDue || arrived
            entranceDue = false
            results.update(matches, entering: entering)
            layoutForResults()
        }

        // A file search is still running in Rust, so re-ask shortly. Polling rather than
        // a callback keeps the FFI at one entry point and keeps threads out of Swift; a
        // re-query costs ~0.4ms, so the poll is free next to the ~120ms `mdfind` it is
        // waiting on. Guarded on the text being unchanged so a stale poll cannot
        // overwrite what the user has since typed.
        if pending || effectiveQuery.hasPrefix(":") {
            pollTask = Task { @MainActor [weak self] in
                do { try await Task.sleep(for: .milliseconds(pending ? 60 : 2000)) }
                catch { return }
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
        if match.kind.isAgent {
            performAgent(action, on: match)
            return
        }
        if action == .open { launchSelected(); return }
        let shortcut: String
        switch action {
        case .reveal: shortcut = "⌘↩"
        case .copyPath: shortcut = "⌥↩"
        case .quit: shortcut = "⌃↩"
        case .open: return
        }
        guard let offered = actionRegistry.actions(for: match).first(where: { $0.shortcut == shortcut }) else {
            NSSound.beep(); return
        }
        runAction(offered, on: match)
    }

    private func showActions() {
        guard actionTask == nil, let match = results.selectedMatch else { return }
        actionTarget = match
        offeredActions = actionRegistry.actions(for: match)
        let menu = NSMenu(title: "Actions")
        for (index, action) in offeredActions.enumerated() {
            let item = NSMenuItem(title: action.label + (action.shortcut.isEmpty ? "" : "  " + action.shortcut),
                                  action: #selector(runOfferedAction(_:)), keyEquivalent: "")
            item.tag = index
            item.target = self
            menu.addItem(item)
        }
        guard !offeredActions.isEmpty else { NSSound.beep(); return }
        menu.popUp(positioning: menu.items.first, at: NSPoint(x: Theme.gutter, y: 0), in: field)
    }

    @objc private func runOfferedAction(_ sender: NSMenuItem) {
        guard let match = actionTarget, offeredActions.indices.contains(sender.tag) else { return }
        let action = offeredActions[sender.tag]
        runAction(action, on: match)
    }

    private func runAction(_ action: ResultAction, on match: Match) {
        guard actionTask == nil else { return }
        if let message = action.confirmation {
            let alert = NSAlert()
            alert.messageText = message
            alert.addButton(withTitle: "Cancel")
            alert.addButton(withTitle: action.label)
            previewing = true
            let response = alert.runModal()
            previewing = false
            guard response == .alertSecondButtonReturn else { return }
        }
        tabBar.show(status: "WORKING…  ⎋ CANCEL")
        actionTask = Task { @MainActor [weak self] in
            guard let self else { return }
            self.previewing = action.presentsModal
            defer {
                self.actionTask = nil
                self.previewing = self.preview.isShowing
            }
            do {
                let effect = try await self.actionRegistry.perform(action, on: match, confirmed: true)
                guard !Task.isCancelled, self.isVisible else { return }
                switch effect {
                case .query(let text):
                    self.field.stringValue = text
                    self.field.currentEditor()?.selectedRange = NSRange(location: (text as NSString).length, length: 0)
                    self.refresh()
                case .setting(let key):
                    self.dismiss(restoringFocus: false)
                    self.onOpenSetting?(key)
                case .none: self.refresh()
                case .preview(let path):
                    self.previewing = true
                    self.previewing = self.preview.toggle(path)
                case .previewPage(let path, let page):
                    self.previewing = true
                    self.previewing = self.preview.toggle(path, page: page)
                case .dismiss(let restoringFocus): self.dismiss(restoringFocus: restoringFocus)
                case .localAI(let request, let reference):
                    self.field.stringValue = ">" + request
                    self.core.agentSubmit(request, selectedText: reference)
                    self.refresh()
                case .message(let title, let body):
                    self.previewing = true
                    let alert = NSAlert()
                    alert.messageText = title
                    alert.informativeText = body
                    alert.addButton(withTitle: "Done")
                    alert.runModal()
                    self.refresh()
                case .clipboard(let data, let image, let paste):
                    self.watcher.writeOwn { pasteboard in
                        if image {
                            if let decoded = NSImage(data: data) { pasteboard.writeObjects([decoded]) }
                        } else {
                            pasteboard.setString(String(decoding: data, as: UTF8.self), forType: .string)
                        }
                    }
                    let target = self.previousApp
                    self.dismiss(restoringFocus: true)
                    if paste, let target {
                        Task { @MainActor in
                            try? await Task.sleep(for: .milliseconds(150))
                            guard !target.isTerminated, AXIsProcessTrusted(),
                                  NSWorkspace.shared.frontmostApplication?.processIdentifier == target.processIdentifier,
                                  let down = CGEvent(keyboardEventSource: nil, virtualKey: 9, keyDown: true),
                                  let up = CGEvent(keyboardEventSource: nil, virtualKey: 9, keyDown: false) else { return }
                            down.flags = .maskCommand; up.flags = .maskCommand
                            down.postToPid(target.processIdentifier); up.postToPid(target.processIdentifier)
                        }
                    }
                }
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled && self.isVisible else { return }
                self.previewing = true
                let alert = NSAlert()
                alert.messageText = "Action could not finish"
                alert.informativeText = error.localizedDescription
                alert.runModal()
                self.refresh()
            }
        }
    }

    /// The agent's rows, where ↩ means "go on" rather than "open something": send the
    /// request, run what came back, or copy a finished command's output. The panel stays up
    /// throughout — the answer and then the run both land in it.
    private func performAgent(_ action: Action, on match: Match) {
        switch (action, match.kind) {
        case (.open, .agentPrompt):
            // Without the mode prefix: the core keys the session to the request it was asked,
            // and it strips `>` before it ever sees one.
            let request = Self.request(in: field.stringValue)
            // An explicit `>` is the general local-model prompt. A request such as
            // `>explain Rust` must remain an ordinary question even though “explain” is
            // also a contextual text action when typed in the normal launcher mode.
            guard !field.stringValue.hasPrefix(">"),
                  LauncherContext.isTextCommand(request) || core.promptInstruction(for: request) != nil else {
                core.agentSubmit(request)
                refresh()
                return
            }
            guard actionTask == nil else { return }
            let query = field.stringValue
            tabBar.show(status: "READING CONTEXT…  ⎋ CANCEL")
            actionTask = Task { @MainActor [weak self] in
                guard let self else { return }
                defer { self.actionTask = nil }
                do {
                    if self.context?.reference(for: request) == nil, let app = self.previousApp {
                        self.contextTask?.cancel()
                        let snapshot = await LauncherContext.capture(applicationID: app.bundleIdentifier,
                            processID: app.processIdentifier, requestAccess: true, request: request)
                        guard !Task.isCancelled, self.isVisible, self.field.stringValue == query else { return }
                        self.context = snapshot
                    }
                    var reference = self.context?.reference(for: request)
                    let lower = request.lowercased()
                    if reference == nil, !lower.contains("copied"), !lower.contains("clipboard"), !lower.contains("page"),
                       case let .available(url) = self.context?.documentURL, url.isFileURL, FileText.supports(url) {
                        reference = try await FileText.read(url)
                    }
                    guard !Task.isCancelled, self.isVisible, self.field.stringValue == query else { return }
                    guard let reference else {
                        self.tabBar.show(status: "CONTEXT UNAVAILABLE · SELECT TEXT OR CHECK PERMISSIONS")
                        return
                    }
                    self.core.agentSubmit(request, selectedText: reference)
                    self.refresh()
                } catch {
                    guard !Task.isCancelled, self.isVisible, self.field.stringValue == query else { return }
                    self.tabBar.show(status: "DOCUMENT TEXT UNAVAILABLE")
                }
            }
        case (.open, .agentStep):
            // Rename, move and trash change things that already exist, so the plan is spelled out and
            // confirmed first; creating or listing still runs on Return.
            let changes = results.matches.filter { match in
                match.kind == .agentStep && Self.destructivePrefixes.contains { match.name.hasPrefix($0) }
            }
            if !changes.isEmpty {
                let alert = NSAlert()
                alert.alertStyle = .warning
                alert.messageText = changes.count == 1 ? "Run this change?" : "Run these \(changes.count) changes?"
                alert.informativeText = changes.map(\.name).joined(separator: "\n")
                    + "\n\nNothing is overwritten. Items moved to the Trash can be restored from there."
                alert.addButton(withTitle: "Run")
                alert.addButton(withTitle: "Cancel")
                previewing = true
                let response = alert.runModal()
                previewing = false
                guard response == .alertFirstButtonReturn else { return }
            }
            core.agentRun()
            refresh()
        case (.open, .agentOk), (.open, .agentFailed):
            // The output, because that is what you came back for; ⌘↩ still copies the command.
            watcher.copy { $0.setString(match.subtitle, forType: .string) }
        case (.open, .agentAnswer):
            watcher.copy { $0.setString(match.name, forType: .string) }
        case (.open, .agentRunning):
            // Leave it running: a server never exits, and waiting for it to is not an answer.
            core.agentDetach()
            refresh()
        case (.open, .agentPast):
            // Back into the field, ready to send again — never re-run behind your back.
            field.stringValue = ">\(match.name)"
            field.currentEditor()?.selectedRange = NSRange(
                location: (field.stringValue as NSString).length, length: 0)
            refresh()
        case (.open, .agentModel):
            // The row's id is its place in the list the core handed over.
            core.agentChoose(Int(match.id))
            refresh()
        case (.copyPath, _), (.reveal, _):
            watcher.copy { $0.setString(match.name, forType: .string) }
        default:
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
        case kVK_ANSI_4: switchMode(to: 3)
        case kVK_ANSI_Y:
            previewSelected()
        case kVK_ANSI_K:
            showActions()
        case kVK_ANSI_Comma:
            // Handled here rather than left to fall through to the main menu.
            //
            // The menu route does work — measured, `NSApp.sendEvent` reaches the delegate
            // both with the panel up and without. But it only works while blindspot is
            // getting key events at all, and a `.nonactivatingPanel` means blindspot is
            // never the frontmost application: with the panel shut, ⌘, belongs to whatever
            // app actually is. Claiming it explicitly while the panel is key at least makes
            // the one case that *can* work not depend on menu validation finding a target.
            //
            // A held callback rather than `NSApp.sendAction(to: nil)`, which walks the
            // responder chain to find the app delegate and then does nothing at all —
            // silently, returning false — if the walk comes up empty. That walk begins at
            // the *key* window, and this panel's key status is the one thing that is not
            // dependable here: it is a `.nonactivatingPanel`, so blindspot is never the
            // frontmost application and `NSApp.keyWindow` is not always what you would
            // expect. A closure the delegate handed over cannot come up empty.
            onSettings?()
        case kVK_ANSI_M:
            // The model picker, in place of the results. Only in agent mode: everywhere else
            // there is no model to pick.
            guard Self.mode(of: field.stringValue) == 3 else {
                return super.performKeyEquivalent(with: event)
            }
            core.agentModels()
            refresh()
        default: return super.performKeyEquivalent(with: event)
        }
        return true
    }

    // MARK: - Browse modes

    private static func mode(of text: String) -> Int {
        modes.indices.dropFirst().first { text.hasPrefix(modes[$0].prefix) } ?? 0
    }

    /// The right of the tab bar: whatever a mode can say about itself from what the core
    /// has already handed over. Nothing here is worth a second FFI call per keystroke, so
    /// most modes say nothing, which is a perfectly good answer.
    private static func status(for mode: Int, in matches: [Match]) -> String {
        if mode == 3 { return "MODEL ⌘M" }
        // A converted value names its own form — "BYTES", "ISO 8601" — and that is the
        // one label that says what a screenful of figures is.
        if let computed = matches.first(where: { $0.kind == .tool }), !computed.detail.isEmpty {
            return computed.detail.uppercased()
        }
        return "ACTIONS ⌘K"
    }

    /// Swaps the query's prefix and keeps what was typed after it, so switching mode
    /// re-asks the same question of a different source.
    private func switchMode(to index: Int) {
        let text = field.stringValue
        let typed = text.dropFirst(Self.modes[Self.mode(of: text)].prefix.count)
            .drop { $0 == " " }
        entranceDue = true
        field.stringValue = Self.modes[index].prefix + typed
        // Programmatic edits bypass `controlTextDidChange`, and leave the caret where it was.
        field.currentEditor()?.selectedRange = NSRange(
            location: (field.stringValue as NSString).length, length: 0)
        refresh()
    }

    private func launchSelected() {
        guard let match = results.selectedMatch else { return }

        let plan = LocalRequest.parse(field.stringValue)
        if plan == .currentProject, match.kind == .command {
            guard actionTask == nil, let app = previousApp else { return }
            let query = field.stringValue
            tabBar.show(status: "READING CURRENT FOLDER…  ⎋ CANCEL")
            actionTask = Task { @MainActor [weak self] in
                let snapshot = await LauncherContext.capture(applicationID: app.bundleIdentifier,
                    processID: app.processIdentifier, requestAccess: true)
                guard let self else { return }
                defer { self.actionTask = nil }
                guard !Task.isCancelled, self.isVisible, self.field.stringValue == query else { return }
                self.context = snapshot
                self.refresh()
                if snapshot.projectDirectory == nil { self.tabBar.show(status: "CURRENT FOLDER UNAVAILABLE · CHECK PERMISSIONS") }
            }
            return
        }
        if case .terminatePort = plan, match.kind == .port {
            guard let action = actionRegistry.actions(for: match).first(where: {
                $0.id == ResultActionID(provider: "native.process", operation: "terminate")
            }) else { NSSound.beep(); return }
            runAction(action, on: match)
            return
        }

        switch match.kind {
        case .command, .setting, .shortcut, .quickLink, .snippet, .system, .prompt, .event:
            guard let action = actionRegistry.actions(for: match).first else { return }
            runAction(action, on: match)
        case .port:
            guard let action = actionRegistry.actions(for: match).first else { return }
            runAction(action, on: match)

        case .calc, .tool:
            if let action = actionRegistry.actions(for: match).first {
                runAction(action, on: match)
                return
            }
            // Copy and hand focus back, so the answer can be pasted straight into
            // whatever the user was working in. No `activate`: a calculated row has no
            // stable id, and recording one would put junk in the frecency store.
            watcher.copy { $0.setString(match.name, forType: .string) }
            dismiss(restoringFocus: true)

        case .clipText, .clipImage:
            guard let action = actionRegistry.actions(for: match).first(where: {
                $0.id == ResultActionID(provider: "native.clipboard", operation: "copy")
            }) else { return }
            runAction(action, on: match)

        case .app, .file:
            guard let action = actionRegistry.actions(for: match).first else { return }
            if match.id != 0 { core.activate(match.id) }
            runAction(action, on: match)

        case .header, .agentPrompt, .agentStep, .agentBlocked, .agentOk, .agentFailed,
            .agentAnswer, .agentModel, .agentRunning, .agentPast:
            // Headers are never selected, and agent rows are handled by `performAgent`.
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
        // Tab and ⇧Tab do what the arrows do. There is nowhere else for focus to go — the
        // field is the only thing in the panel that takes it — so the default behaviour
        // would either put a tab in the query or ring the bell.
        case #selector(NSResponder.insertTab(_:)):
            if results.selectedMatch?.kind == .command {
                launchSelected()
                return true
            }
            results.moveSelection(by: 1)
            return true
        case #selector(NSResponder.insertBacktab(_:)):
            results.moveSelection(by: -1)
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
            // first (rdar://8967168), which would swallow it. Routed through the override
            // rather than dismissing here, so Esc still stops the agent first.
            cancelOperation(nil)
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
        leftEdge = visible.midX - Theme.panelWidth / 2
    }

    /// Grows and shrinks downward, keeping the top edge where `placeOnActiveScreen` put it.
    private func layoutForResults() {
        resultsHeight.constant = results.fittingHeight
        let height = Self.chromeHeight + results.fittingHeight
        let target = NSRect(
            x: leftEdge, y: topEdge - height, width: Theme.panelWidth, height: height)
        // Measured: across a realistic typing burst the height changes on only 4 of 12
        // keystrokes, while `setFrame(display:)` costs ~0.7ms median and 2.7ms at p99.
        guard target != frame else { return }
        // Drawn now only if on screen; ordering in draws it anyway.
        setFrame(target, display: isVisible)
    }
}
