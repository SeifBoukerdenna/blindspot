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
        dismiss(restoringFocus: false)
    }

    /// `restoringFocus` is false when dismissing because the user launched something:
    /// reactivating the app they came from would fight the app they just asked for.
    private func dismiss(restoringFocus: Bool) {
        guard !isDismissing else { return }
        isDismissing = true
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

    /// True while the agent is generating or running, which is what makes Esc mean "stop".
    private var agentWorking = false

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
        let text = field.stringValue
        let mode = Self.mode(of: text)
        tabBar.select(mode)
        layoutPrompt(mode: mode, text: text)
        styleQuery()

        // An empty field is the welcome screen — suggested apps and recent files — which
        // reverses M2's "nothing until you type". That was right while the only thing an
        // empty query could return was the alphabetical head of the index.
        let (matches, pending) = core.query(text, limit: Self.resultLimit)
        tabBar.show(status: Self.status(for: mode, in: matches))
        agentWorking = pending && text.hasPrefix(">")
        if !(poll && matches == results.matches) {
            // Rows animate in where rows actually arrive: the panel opening, a change of mode,
            // and the agent's steps landing one at a time. Typing just replaces the list.
            let entering = entranceDue || (poll && text.hasPrefix(">"))
            entranceDue = false
            results.update(matches, entering: entering)
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
        if match.kind.isAgent {
            performAgent(action, on: match)
            return
        }
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

    /// The agent's rows, where ↩ means "go on" rather than "open something": send the
    /// request, run what came back, or copy a finished command's output. The panel stays up
    /// throughout — the answer and then the run both land in it.
    private func performAgent(_ action: Action, on match: Match) {
        switch (action, match.kind) {
        case (.open, .agentPrompt):
            // Without the mode prefix: the core keys the session to the request it was asked,
            // and it strips `>` before it ever sees one.
            core.agentSubmit(Self.request(in: field.stringValue))
            refresh()
        case (.open, .agentStep):
            core.agentRun()
            refresh()
        case (.open, .agentOk), (.open, .agentFailed):
            // The output, because that is what you came back for; ⌘↩ still copies the command.
            watcher.writeOwn { $0.setString(match.subtitle, forType: .string) }
        case (.open, .agentAnswer):
            watcher.writeOwn { $0.setString(match.name, forType: .string) }
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
            watcher.writeOwn { $0.setString(match.name, forType: .string) }
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
            // `sendAction(to: nil)` walks the responder chain to the app delegate, which is
            // the only thing that knows about the settings window.
            NSApp.sendAction(Selector(("openSettings")), to: nil, from: self)
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
        return ""
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
