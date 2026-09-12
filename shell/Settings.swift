import AppKit
import Carbon.HIToolbox

/// A stack that lays out from the top.
///
/// An `NSScrollView`'s document view has a bottom-left origin unless it says otherwise, so
/// an unflipped stack draws its first row at the bottom and opens showing the end of the
/// list. Measured the obvious way: the Agent page opened on "Roots".
@MainActor
private final class TopStack: NSStackView {
    override var isFlipped: Bool { true }
}

/// A switch, in the plate's own language.
///
/// Not `NSSwitch`: its on state is the system accent, and this design spends exactly one
/// colour on exactly one thing per screen.
@MainActor
private final class Toggle: NSView {
    private static let size = NSSize(width: 38, height: 22)
    private let knob = PlateView(fill: Theme.surface, radius: 8)
    private var on: Bool
    private var placement: NSLayoutConstraint?

    var onChange: ((Bool) -> Void)?

    init(on: Bool) {
        self.on = on
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = Self.size.height / 2
        layer?.borderColor = Theme.hairline.cgColor
        layer?.borderWidth = 1
        translatesAutoresizingMaskIntoConstraints = false
        knob.translatesAutoresizingMaskIntoConstraints = true
        addSubview(knob)

        NSLayoutConstraint.activate([
            widthAnchor.constraint(equalToConstant: Self.size.width),
            heightAnchor.constraint(equalToConstant: Self.size.height),
        ])
        paint()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    private func paint() {
        layer?.backgroundColor = (on ? Theme.accent : Theme.ground).cgColor
        knob.layer?.backgroundColor = (on ? Theme.surface : Theme.faint).cgColor
        let inset: CGFloat = 3
        let side: CGFloat = 16
        knob.frame = NSRect(
            x: on ? Self.size.width - side - inset : inset,
            y: (Self.size.height - side) / 2, width: side, height: side)
    }

    override func mouseDown(with event: NSEvent) {
        on.toggle()
        paint()
        onChange?(on)
    }
}

/// The hotkey recorder.
///
/// Click it and it swallows the next chord. What it sends back is a Carbon key code and
/// mask — the only part of this that is genuinely shell-side, since Carbon and AppKit
/// modifier bits share no positions — and the core spells it. The window therefore never
/// knows how a chord is written, and cannot drift from the parser config.toml is read with.
@MainActor
private final class ChordWell: NSView {
    private let label = NSTextField(labelWithString: "")
    private let plate = PlateView(fill: Theme.ground, radius: 3, border: Theme.hairline)
    private var monitor: Any?
    private var resting: String

    /// A captured chord, as Carbon reckons it.
    var onChord: ((UInt32, UInt32) -> Void)?
    /// ⌫ while armed: no hotkey at all, which is a legitimate answer for the agent's.
    var onClear: (() -> Void)?

    init(_ value: String) {
        resting = value
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        label.alignment = .right
        label.translatesAutoresizingMaskIntoConstraints = false
        plate.addSubview(label)
        addSubview(plate)
        NSLayoutConstraint.activate([
            plate.leadingAnchor.constraint(equalTo: leadingAnchor),
            plate.trailingAnchor.constraint(equalTo: trailingAnchor),
            plate.topAnchor.constraint(equalTo: topAnchor),
            plate.bottomAnchor.constraint(equalTo: bottomAnchor),
            widthAnchor.constraint(equalToConstant: 180),
            label.leadingAnchor.constraint(equalTo: plate.leadingAnchor, constant: 8),
            label.trailingAnchor.constraint(equalTo: plate.trailingAnchor, constant: -8),
            label.topAnchor.constraint(equalTo: plate.topAnchor, constant: 5),
            label.bottomAnchor.constraint(equalTo: plate.bottomAnchor, constant: -5),
        ])
        rest()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    /// `isolated` so this runs on the main actor and may read `monitor`, which is `Any?`
    /// and therefore not `Sendable` — the same reason `HotKey`'s deinit is isolated.
    /// Removing the monitor matters: one left armed would keep swallowing every keystroke
    /// in the app.
    isolated deinit {
        if let monitor { NSEvent.removeMonitor(monitor) }
    }

    private func rest() {
        let empty = resting.trimmingCharacters(in: .whitespaces).isEmpty
        label.attributedStringValue = NSAttributedString(
            string: empty ? "none" : resting,
            attributes: [
                .font: Theme.mono(12),
                .foregroundColor: empty ? Theme.faint : Theme.ink,
            ])
        plate.fill(Theme.ground)
    }

    override func mouseDown(with event: NSEvent) {
        guard monitor == nil else { return disarm() }
        label.attributedStringValue = Theme.label(
            "PRESS A CHORD · ⎋ CANCEL", size: 9.5, tracking: 0.1, color: Theme.accent)
        plate.fill(Theme.accent.withAlphaComponent(0.12))
        // Local, not global: this needs no Accessibility grant, which is the same reason
        // the hotkey itself is Carbon rather than a `CGEventTap`. Returning nil swallows
        // the key so the chord being recorded cannot also act on the window.
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown]) { [weak self] event in
            self?.capture(event)
            return nil
        }
    }

    private func capture(_ event: NSEvent) {
        defer { disarm() }
        if event.keyCode == UInt16(kVK_Escape) { return }
        if event.keyCode == UInt16(kVK_Delete) {
            onClear?()
            return
        }
        let flags = event.modifierFlags
        var carbon: UInt32 = 0
        // Carbon masks, not `NSEvent.ModifierFlags`: the two share no bit positions, and a
        // raw AppKit value handed to Carbon is silently a different chord.
        if flags.contains(.command) { carbon |= UInt32(cmdKey) }
        if flags.contains(.shift) { carbon |= UInt32(shiftKey) }
        if flags.contains(.option) { carbon |= UInt32(optionKey) }
        if flags.contains(.control) { carbon |= UInt32(controlKey) }
        onChord?(UInt32(event.keyCode), carbon)
    }

    private func disarm() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        rest()
    }
}

/// One row of the settings window: what the knob is, what it holds, and where that came
/// from.
///
/// Two columns. The left names the knob and explains it — and, when a write is refused,
/// says why in place of the explanation. The right carries the control and, under it, the
/// one thing a layered config has to answer: whether this value is the built-in, your
/// config.toml, or something this window set.
@MainActor
private final class SettingRow: NSView, NSTextFieldDelegate, NSTextViewDelegate {
    private static let controlWidth: CGFloat = 240

    private let setting: Setting
    /// Returns the reason a value was refused, or nil if it took.
    private let apply: (String) -> String?
    private let forget: () -> Void
    private let spell: (UInt32, UInt32) -> String?

    private let separator = PlateView(fill: Theme.hairline)
    private let label = NSTextField(labelWithString: "")
    private let help = NSTextField(labelWithString: "")
    private let origin = NSButton()

    init(
        _ setting: Setting,
        separated: Bool,
        apply: @escaping (String) -> String?,
        forget: @escaping () -> Void,
        spell: @escaping (UInt32, UInt32) -> String?
    ) {
        self.setting = setting
        self.apply = apply
        self.forget = forget
        self.spell = spell
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        separator.isHidden = !separated

        label.attributedStringValue = NSAttributedString(
            string: setting.label,
            attributes: [.font: Theme.text(13, .medium), .foregroundColor: Theme.ink])
        help.lineBreakMode = .byWordWrapping
        help.maximumNumberOfLines = 2
        help.preferredMaxLayoutWidth = 320
        say(setting.help, wrong: false)

        // A button, not a label: on a row this window set, it is how you put it back.
        origin.isBordered = false
        origin.target = self
        origin.action = #selector(reset)
        origin.attributedTitle = Self.provenance(of: setting)
        origin.isEnabled = setting.source == .window
        origin.toolTip = setting.source == .window ? "Back to config.toml" : nil
        // Nothing set a diagnostic, so "where did this come from" has no answer to give.
        origin.isHidden = setting.kind == .readonly

        let text = NSStackView(views: [label, help])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 3

        let side = NSStackView(views: [control(), origin])
        side.orientation = .vertical
        side.alignment = .trailing
        side.spacing = 6

        for view in [separator, text, side] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }

        NSLayoutConstraint.activate([
            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            separator.heightAnchor.constraint(equalToConstant: 1),

            text.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            text.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            text.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -14),
            text.trailingAnchor.constraint(lessThanOrEqualTo: side.leadingAnchor, constant: -16),

            side.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            side.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            side.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -14),
            side.widthAnchor.constraint(equalToConstant: Self.controlWidth),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    /// The second line: what this knob is for, or why the last write was refused.
    private func say(_ message: String, wrong: Bool) {
        help.attributedStringValue = NSAttributedString(
            string: message,
            attributes: [
                .font: Theme.text(11),
                .foregroundColor: wrong ? Theme.danger : Theme.faint,
            ])
    }

    /// Sends a value, and either lets the window reload or says why it was refused.
    private func commit(_ value: String) {
        if let why = apply(value) {
            say(why, wrong: true)
            NSSound.beep()
        }
    }

    @objc private func reset() {
        forget()
    }

    private static func provenance(of setting: Setting) -> NSAttributedString {
        let (word, colour): (String, NSColor) =
            switch setting.source {
            case .builtIn: ("DEFAULT", Theme.faint)
            case .configFile: ("CONFIG.TOML", Theme.muted)
            case .window: ("SET HERE · RESET", Theme.accent)
            }
        let line = setting.live ? word : "\(word) · RESTART"
        return Theme.label(line, size: 9, tracking: 0.12, color: colour)
    }

    // MARK: - Controls

    private func control() -> NSView {
        switch setting.kind {
        case .flag:
            let toggle = Toggle(on: setting.value == "true")
            toggle.onChange = { [weak self] on in self?.commit(on ? "true" : "false") }
            return trailing(toggle)

        case .count, .number:
            return trailing(well(width: 90, mono: true))

        case .chord:
            let well = ChordWell(setting.value)
            well.onChord = { [weak self] code, mask in
                guard let self else { return }
                guard let spelled = spell(code, mask) else {
                    say("that key has no name blindspot can write down", wrong: true)
                    NSSound.beep()
                    return
                }
                commit(spelled)
            }
            well.onClear = { [weak self] in self?.commit("") }
            return trailing(well)

        case .paths:
            return pathList()

        case .text:
            return trailing(well(width: 200, mono: false))

        case .readonly:
            let value = NSTextField(labelWithString: setting.value)
            value.font = Theme.text(12)
            value.textColor = Theme.inkSoft
            value.alignment = .right
            return value
        }
    }

    /// A one-line editable well. The plate draws the border, not `NSTextField`'s bezel —
    /// the system bezel is the loudest thing on a dark ground and this design is hairlines.
    private func well(width: CGFloat, mono: Bool) -> NSView {
        let field = NSTextField(string: setting.value)
        field.font = mono ? Theme.mono(12) : Theme.text(12)
        field.textColor = Theme.ink
        field.alignment = .right
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.lineBreakMode = .byTruncatingHead
        field.delegate = self
        field.translatesAutoresizingMaskIntoConstraints = false

        let plate = PlateView(fill: Theme.ground, radius: 3, border: Theme.hairline)
        plate.addSubview(field)
        NSLayoutConstraint.activate([
            plate.widthAnchor.constraint(equalToConstant: width),
            field.leadingAnchor.constraint(equalTo: plate.leadingAnchor, constant: 8),
            field.trailingAnchor.constraint(equalTo: plate.trailingAnchor, constant: -8),
            field.topAnchor.constraint(equalTo: plate.topAnchor, constant: 5),
            field.bottomAnchor.constraint(equalTo: plate.bottomAnchor, constant: -5),
        ])
        return plate
    }

    /// One folder per line, committed when focus leaves.
    ///
    /// A text view rather than a row of fields with add and remove buttons: a folder list
    /// *is* a short piece of text, and the file it ends up in is a list of strings.
    private func pathList() -> NSView {
        let view = NSTextView()
        view.string = setting.items.joined(separator: "\n")
        view.font = Theme.mono(11)
        view.textColor = Theme.inkSoft
        view.backgroundColor = .clear
        view.drawsBackground = false
        view.isRichText = false
        view.isAutomaticQuoteSubstitutionEnabled = false
        view.alignment = .right
        view.delegate = self
        view.textContainerInset = NSSize(width: 0, height: 0)
        view.isVerticallyResizable = false
        view.translatesAutoresizingMaskIntoConstraints = false

        let plate = PlateView(fill: Theme.ground, radius: 3, border: Theme.hairline)
        plate.addSubview(view)
        NSLayoutConstraint.activate([
            view.leadingAnchor.constraint(equalTo: plate.leadingAnchor, constant: 8),
            view.trailingAnchor.constraint(equalTo: plate.trailingAnchor, constant: -8),
            view.topAnchor.constraint(equalTo: plate.topAnchor, constant: 6),
            view.bottomAnchor.constraint(equalTo: plate.bottomAnchor, constant: -6),
            view.heightAnchor.constraint(
                equalToConstant: CGFloat(max(setting.items.count, 1)) * 15),
        ])
        return plate
    }

    private func trailing(_ view: NSView) -> NSView {
        let box = NSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        box.addSubview(view)
        NSLayoutConstraint.activate([
            view.trailingAnchor.constraint(equalTo: box.trailingAnchor),
            view.topAnchor.constraint(equalTo: box.topAnchor),
            view.bottomAnchor.constraint(equalTo: box.bottomAnchor),
            view.leadingAnchor.constraint(greaterThanOrEqualTo: box.leadingAnchor),
        ])
        return box
    }

    // MARK: - Commit

    /// Enter, and clicking away. Both, because a settings field you typed in and then left
    /// should not quietly discard what you typed.
    func controlTextDidEndEditing(_ obj: Notification) {
        guard let field = obj.object as? NSTextField, field.stringValue != setting.value else {
            return
        }
        commit(field.stringValue)
    }

    func textDidEndEditing(_ notification: Notification) {
        guard let view = notification.object as? NSTextView else { return }
        let items = view.string
            .components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        guard items != setting.items else { return }
        commit(Setting.joined(items))
    }
}

/// A row that does something rather than holding something.
///
/// Deliberately not a `Kind` in the schema: an action has no value, no default and nowhere
/// for a provenance to come from, and every consumer of a settings row would have to branch
/// around the one case where `value` means nothing.
@MainActor
private final class ActionRow: NSView {
    private let act: () -> Void

    init(label: String, help: String, button: String, act: @escaping () -> Void) {
        self.act = act
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false

        let separator = PlateView(fill: Theme.hairline)
        let title = NSTextField(labelWithString: "")
        title.attributedStringValue = NSAttributedString(
            string: label,
            attributes: [.font: Theme.text(13, .medium), .foregroundColor: Theme.ink])
        let note = NSTextField(labelWithString: "")
        note.attributedStringValue = NSAttributedString(
            string: help,
            attributes: [.font: Theme.text(11), .foregroundColor: Theme.faint])
        note.lineBreakMode = .byWordWrapping
        note.maximumNumberOfLines = 2
        note.preferredMaxLayoutWidth = 320

        let press = NSButton(title: button, target: self, action: #selector(fire))
        press.isBordered = false
        press.attributedTitle = Theme.label(
            button.uppercased(), size: 10, tracking: 0.1, color: Theme.danger)

        let text = NSStackView(views: [title, note])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 3

        for view in [separator, text, press] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            separator.heightAnchor.constraint(equalToConstant: 1),
            text.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            text.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            text.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -14),
            press.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            press.centerYAnchor.constraint(equalTo: text.centerYAnchor),
            press.leadingAnchor.constraint(greaterThanOrEqualTo: text.trailingAnchor, constant: 16),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    @objc private func fire() { act() }
}

/// One colourway, with its own colours as the sample.
///
/// Its own page rather than a settings row: the core cannot enumerate what palettes exist,
/// because which ones exist is a fact about `Theme.swift`. Asking Rust to describe an enum
/// it cannot see would be backwards.
@MainActor
private final class PaletteRow: NSView {
    private let choose: () -> Void

    init(_ palette: Palette, current: Bool, separated: Bool, choose: @escaping () -> Void) {
        self.choose = choose
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false

        let separator = PlateView(fill: Theme.hairline)
        separator.isHidden = !separated

        let title = NSTextField(labelWithString: "")
        title.attributedStringValue = NSAttributedString(
            string: palette.name,
            attributes: [.font: Theme.text(13, .medium), .foregroundColor: Theme.ink])

        let note = NSTextField(labelWithString: "")
        note.attributedStringValue = NSAttributedString(
            string: palette.isDark ? "Dark plate." : "Light plate.",
            attributes: [.font: Theme.text(11), .foregroundColor: Theme.faint])

        // The sample is the palette's own values, so the row is the thing it describes.
        let swatches = NSStackView(views: [palette.surface, palette.ground, palette.ink,
                                           palette.accent].map(Self.swatch))
        swatches.spacing = 4

        let mark = NSTextField(labelWithString: "")
        mark.attributedStringValue = Theme.label(
            current ? "CURRENT" : "", size: 9, tracking: 0.12, color: Theme.accent)

        let text = NSStackView(views: [title, note])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 3

        let side = NSStackView(views: [swatches, mark])
        side.orientation = .vertical
        side.alignment = .trailing
        side.spacing = 6

        for view in [separator, text, side] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            separator.topAnchor.constraint(equalTo: topAnchor),
            separator.leadingAnchor.constraint(equalTo: leadingAnchor),
            separator.trailingAnchor.constraint(equalTo: trailingAnchor),
            separator.heightAnchor.constraint(equalToConstant: 1),
            text.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Theme.gutter),
            text.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            text.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -14),
            side.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Theme.gutter),
            side.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            side.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -14),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    private static func swatch(_ colour: NSColor) -> NSView {
        let chip = PlateView(fill: colour, radius: 3, border: Theme.hairline)
        chip.widthAnchor.constraint(equalToConstant: 26).isActive = true
        chip.heightAnchor.constraint(equalToConstant: 18).isActive = true
        return chip
    }

    override func mouseDown(with event: NSEvent) { choose() }
}

/// The preferences window.
///
/// A separate window rather than a fifth panel mode: recording a chord, editing a folder
/// list and nudging a number are controls, not rows you arrow through. It is the same plate
/// as the panel — same palette, same tab bar, same gutter — so the two read as one
/// application rather than an app and its preferences.
///
/// Every row it draws comes from `Core.settings()`, and every value it writes is validated
/// by the core. The shell holds no schema and no parser of its own, which is what keeps
/// this window from drifting from config.toml.
@MainActor
final class SettingsWindow: NSObject, NSWindowDelegate {
    private static let width: CGFloat = 640
    private static let height: CGFloat = 460

    private let core: Core
    private let window: NSWindow
    /// All three are rebuilt when the colourway changes: a `CALayer` bakes its colour in
    /// when it is built, so repainting in place would leave every hairline and the tab
    /// bar's band in the old palette.
    private var tabs: TabBar
    private var rows = TopStack()
    private var scroll = NSScrollView()
    private var sections: [String] = []
    private var settings: [Setting] = []

    /// Called after every accepted write, so the shell can take up the half of a setting
    /// it holds rather than reads: the two Carbon hotkeys, the panel's row count, the
    /// login item.
    var onChange: (() -> Void)?
    /// Called when the colourway changes, so the panel can be rebuilt in it.
    var onPalette: (() -> Void)?

    init(core: Core) {
        self.core = core
        settings = core.settings()
        sections = Self.sections(of: settings)
        tabs = TabBar(titles: sections.map { $0.uppercased() })

        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: Self.width, height: Self.height),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false)
        super.init()

        window.title = "blindspot"
        window.titlebarAppearsTransparent = true
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.center()

        buildContent()
        choose(0)
    }

    /// Builds the plate, the tab bar and the scroller in whatever palette is current.
    ///
    /// Called again when the colourway changes, keeping the `NSWindow` itself so it does
    /// not flash or move out from under the pointer.
    private func buildContent() {
        // Pinned to match the palette, for the same reason the panel is: the title bar,
        // the scroller and every other system-drawn part resolve against the window's
        // appearance, and a light title bar over a dark plate reads as two applications.
        window.appearance = NSAppearance(named: Theme.isDark ? .darkAqua : .aqua)
        window.backgroundColor = Theme.ground

        tabs = TabBar(titles: sections.map { $0.uppercased() })
        tabs.onSelect = { [weak self] index in self?.choose(index) }

        rows = TopStack()
        rows.orientation = .vertical
        rows.alignment = .leading
        rows.spacing = 0
        rows.translatesAutoresizingMaskIntoConstraints = false

        scroll = NSScrollView()
        scroll.documentView = rows
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.translatesAutoresizingMaskIntoConstraints = false

        let plate = PlateView(fill: Theme.surface)
        plate.translatesAutoresizingMaskIntoConstraints = true
        for view in [tabs, scroll] as [NSView] { plate.addSubview(view) }

        NSLayoutConstraint.activate([
            tabs.topAnchor.constraint(equalTo: plate.topAnchor),
            tabs.leadingAnchor.constraint(equalTo: plate.leadingAnchor),
            tabs.trailingAnchor.constraint(equalTo: plate.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: tabs.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: plate.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: plate.trailingAnchor),
            scroll.bottomAnchor.constraint(equalTo: plate.bottomAnchor),
            // The document view is as wide as the clip, so a row's trailing gutter lands
            // where the tab bar's does rather than at the end of its own text.
            rows.widthAnchor.constraint(equalTo: scroll.widthAnchor),
        ])
        window.contentView = plate
    }

    /// The sections that actually have rows, in the order the schema lists them.
    private static func sections(of settings: [Setting]) -> [String] {
        var seen: Set<String> = []
        var found = settings.map(\.section).filter { seen.insert($0).inserted }
        // The window's own page, not the schema's — see `PaletteRow`. Before Status, so
        // the two read-mostly pages sit together at the end.
        if let status = found.firstIndex(of: Self.status) {
            found.insert(Self.appearance, at: status)
        } else {
            found.append(Self.appearance)
        }
        return found
    }

    /// Re-reads everything and shows the window.
    ///
    /// Re-read on every open rather than watched: config.toml is hand-edited, and the
    /// answer to "did my edit take" should be "open the window", not "restart".
    func show() {
        reload()
        if sections.indices.contains(tabs.selected), sections[tabs.selected] == Self.status {
            probe()
        }
        // An accessory app's window will not take key focus unless the app activates.
        NSApp.activate()
        window.makeKeyAndOrderFront(nil)
    }

    private func reload() {
        settings = core.settings()
        let refreshed = Self.sections(of: settings)
        if refreshed == sections {
            select(tabs.selected)
        } else {
            sections = refreshed
            select(0)
        }
    }

    private static let status = "Status"
    private static let clipboard = "Clipboard"
    private static let appearance = "Appearance"

    /// A tab was clicked. Separate from `select` so that arriving at the Status page asks
    /// the core to probe, while the redraw that lands the answer does not ask again — which
    /// is what keeps the two from chasing each other forever.
    private func choose(_ index: Int) {
        select(index)
        guard sections.indices.contains(index), sections[index] == Self.status else { return }
        probe()
    }

    /// Asks whether the model's host answers, then redraws once the answer has had time to
    /// arrive. The core does the asking on a thread; a synchronous check here would hang
    /// the window for a connect timeout whenever Ollama is not running.
    private func probe() {
        core.refreshDiagnostics()
        Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(600))
            guard let self, window.isVisible,
                sections.indices.contains(tabs.selected),
                sections[tabs.selected] == Self.status
            else { return }
            settings = core.settings()
            select(tabs.selected)
        }
    }

    /// Switches colourway.
    ///
    /// Both windows are rebuilt rather than repainted: row views are pooled and a layer
    /// bakes its colour when it is built, so nothing already on screen would change. The
    /// panel is rebuilt by the delegate, which owns it — and that is safe only because the
    /// hotkey closures resolve the panel through it rather than capturing one.
    private func repaint(as name: String) {
        guard name != Theme.current.name else { return }
        Theme.use(name)
        onPalette?()
        rebuild()
    }

    /// Rebuilds this window's own content in the new palette, keeping the window itself so
    /// it does not flash or move out from under the pointer.
    func rebuild() {
        let page = tabs.selected
        buildContent()
        select(sections.indices.contains(page) ? page : 0)
    }

    /// Forgets every clip, after asking. Modelled on the Spotlight warning in
    /// `AppDelegate` rather than `die`, which always terminates.
    private func clearClips() {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Forget every clip?"
        alert.informativeText =
            "Clipboard history is its own file, so what you have launched is untouched. "
            + "This cannot be undone."
        alert.addButton(withTitle: "Forget")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate()
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        core.clearClips()
        reload()
    }

    private func select(_ index: Int) {
        guard sections.indices.contains(index) else { return }
        tabs.select(index)
        let section = sections[index]
        for view in rows.arrangedSubviews {
            rows.removeArrangedSubview(view)
            view.removeFromSuperview()
        }
        if section == Self.appearance {
            for (at, palette) in Palette.all.enumerated() {
                let name = palette.name
                let row = PaletteRow(
                    palette,
                    current: palette.name == Theme.current.name,
                    separated: at > 0
                ) { [weak self] in self?.repaint(as: name) }
                rows.addArrangedSubview(row)
                row.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
            }
            tabs.show(status: "")
            return
        }

        for (at, setting) in settings.filter({ $0.section == section }).enumerated() {
            let key = setting.key
            let row = SettingRow(
                setting,
                separated: at > 0,
                apply: { [weak self] value in self?.write(key, value) },
                forget: { [weak self] in self?.forget(key) },
                spell: { [weak self] code, mask in
                    self?.core.formatHotkey(keyCode: code, modifiers: mask)
                })
            rows.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
        }
        // Not a setting, so not in the schema — it is hardcoded onto the page it belongs
        // to, which is what CLAUDE.md's non-goals ask for in a single-user app.
        if section == Self.clipboard {
            let clear = ActionRow(
                label: "Clear history now",
                help: "Forgets every clip. Launch history is a separate file and is untouched.",
                button: "Forget everything"
            ) { [weak self] in self?.clearClips() }
            rows.addArrangedSubview(clear)
            clear.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
        }

        // A page you switch to starts at its top, not wherever the last one was scrolled.
        rows.layoutSubtreeIfNeeded()
        scroll.contentView.scroll(to: .zero)
        scroll.reflectScrolledClipView(scroll.contentView)

        // What the tab bar's right-hand slot is for: how much of this page is yours.
        let set = settings.filter { $0.section == section && $0.source == .window }.count
        tabs.show(status: set == 0 ? "" : "\(set) SET HERE")
    }

    /// Returns the reason a write was refused, and reloads when it was not.
    private func write(_ key: String, _ value: String) -> String? {
        if let why = core.set(key, to: value) { return why }
        settled()
        return nil
    }

    private func forget(_ key: String) {
        // A reset cannot be refused for a key that came out of the schema, and a refusal
        // here would have nowhere to be shown — the row is about to be rebuilt.
        _ = core.reset(key)
        settled()
    }

    /// After a write: the shell takes up what it holds a copy of, and the page redraws so
    /// every row's provenance is current.
    private func settled() {
        onChange?()
        reload()
    }
}
