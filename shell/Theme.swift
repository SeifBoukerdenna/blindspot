import AppKit

/// The plate: one flat surface, hairlines instead of shadows, and a single accent spent
/// on exactly one thing per screen.
///
/// Everything visual lives here so the two view files stay about layout. The palette is
/// deliberately literal rather than semantic — `.labelColor` and `controlAccentColor`
/// follow the system's appearance and accent, and this design is neither. The panel
/// pins itself to `.aqua` (see `Panel.init`) so a user in dark mode gets the whole
/// plate rather than half of one.
@MainActor
enum Theme {

    // MARK: - Palette

    /// The colourway in force. Set once at launch from the remembered name; changing it
    /// rebuilds the panel, because row views are pooled and a layer bakes its colour in
    /// when it is built.
    static var current = Palette.remembered()

    /// Switches colourway and remembers it. The caller rebuilds what is already on screen.
    static func use(_ name: String) {
        current = Palette.named(name)
        Palette.remember(name)
    }

    static var surface: NSColor { current.surface }
    static var ground: NSColor { current.ground }
    static var ink: NSColor { current.ink }
    static var inkSoft: NSColor { current.inkSoft }
    static var muted: NSColor { current.muted }
    static var faint: NSColor { current.faint }
    static var accent: NSColor { current.accent }
    static var ok: NSColor { current.ok }
    static var warn: NSColor { current.warn }
    static var danger: NSColor { current.danger }
    static var hairline: NSColor { current.hairline }
    static var rule: NSColor { current.rule }
    static var border: NSColor { current.border }
    static var selection: NSColor { current.selection }

    /// Whether the plate is dark, so the panel and the settings window can pin the window
    /// appearance to match — every system-drawn part of them resolves against it.
    static var isDark: Bool { current.isDark }

    // MARK: - Metrics

    static let panelWidth: CGFloat = 640
    static let tabBarHeight: CGFloat = 30
    static let fieldHeight: CGFloat = 56
    /// The rule under the query, as a view of its own rather than a border.
    static let ruleHeight: CGFloat = 2
    /// Horizontal padding, identical in the tab bar, the field and every row — which is
    /// what makes the tabs, the caret and the icons share one left edge.
    static let gutter: CGFloat = 20
    static let rowHeight: CGFloat = 44
    static let iconSize: CGFloat = 24
    /// Icon to text, and text to whatever sits at the right of a row.
    static let gap: CGFloat = 14
    /// The selected row's left bar.
    static let selectionBar: CGFloat = 3
    /// Names the form of a computed row — "RAW", "BINARY". Fixed, so the figures beside
    /// it line up as a column you can read down.
    static let railWidth: CGFloat = 76
    /// A proposed command's step number, or the mark saying how it ended.
    static let stepRail: CGFloat = 16

    // MARK: - Type

    /// SF Pro carries the interface. The drawing specified Archivo; nothing but the
    /// system font is guaranteed installed, and SF is the nearest grotesk that is.
    static func text(_ size: CGFloat, _ weight: NSFont.Weight = .regular) -> NSFont {
        .systemFont(ofSize: size, weight: weight)
    }

    /// Anything you could have typed: commands, paths, figures, keys.
    static func mono(_ size: CGFloat, _ weight: NSFont.Weight = .regular) -> NSFont {
        .monospacedSystemFont(ofSize: size, weight: weight)
    }

    /// Every row's text truncates with an ellipsis rather than widening the panel. As a
    /// paragraph style because an attributed string ignores the label's own
    /// `lineBreakMode`, and a long application name would otherwise push the window off
    /// the side of the screen.
    static let truncating: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        return style
    }()

    /// The small tracked labels — tabs, section titles, key hints, outcomes. `tracking`
    /// is in ems, as the drawing specifies it.
    ///
    /// The case of `string` is left alone. Most of these are written in capitals at the
    /// call site, but a status line carrying a model name or a host is not, and
    /// uppercasing one of those makes it unreadable.
    static func label(
        _ string: String, size: CGFloat, tracking: CGFloat, color: NSColor,
        weight: NSFont.Weight = .regular
    ) -> NSAttributedString {
        NSAttributedString(
            string: string,
            attributes: [
                .font: mono(size, weight),
                .foregroundColor: color,
                // Kerning is per-character and trails the last glyph too, which would
                // push a right-aligned label off its margin. Trailing space is cheaper
                // to live with than a paragraph style per label.
                .kern: size * tracking,
            ])
    }
}

extension NSColor {
    /// sRGB, not calibrated: the values here were picked as screen colours.
    convenience init(hex: UInt32) {
        self.init(
            srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: 1)
    }
}

/// A view that is one flat colour, with an optional rim.
///
/// Layer-backed so the fill costs nothing to redraw; `NSBox` would bring a title, a
/// border style and a content view we would spend the same number of lines turning off.
@MainActor
final class PlateView: NSView {
    init(fill: NSColor, radius: CGFloat = 0, border: NSColor? = nil) {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = fill.cgColor
        if radius > 0 {
            layer?.cornerRadius = radius
            layer?.cornerCurve = .continuous
            // Without this the tab band and the row hairlines draw square into the
            // corners the plate has just rounded.
            layer?.masksToBounds = true
        }
        if let border {
            layer?.borderColor = border.cgColor
            layer?.borderWidth = 1
        }
        translatesAutoresizingMaskIntoConstraints = false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not loaded from a nib") }

    /// Repaints it. A badge is the same view whether it is saying "current" or "refused",
    /// and those are not the same colour.
    func fill(_ colour: NSColor) {
        layer?.backgroundColor = colour.cgColor
    }
}

/// One colourway: ten colours, four alphas, and the four lines those alphas make.
///
/// **Every value is stored, the derived ones included.** `withAlphaComponent` allocates a
/// new `NSColor`, and `ResultRow` reads these tens of times per keystroke — deriving them
/// on each access would put fifty allocations on the frame budget that the whole design
/// exists to protect. Computed once here instead, when the palette is built.
@MainActor
struct Palette {
    let name: String
    /// One line for the picker. What it is, not how it was made.
    let note: String
    let surface: NSColor
    let ground: NSColor
    let ink: NSColor
    let inkSoft: NSColor
    let muted: NSColor
    let faint: NSColor
    let accent: NSColor
    let ok: NSColor
    let warn: NSColor
    let danger: NSColor
    let hairline: NSColor
    let rule: NSColor
    let border: NSColor
    let selection: NSColor
    /// Whether the ground is dark. Decides the window appearance, not any colour here.
    let isDark: Bool

    /// Lines are made of the type colour: ink on paper, and light on a dark plate. That is
    /// the one thing that has to move between a pale palette and a dark one — everything
    /// else is just different values.
    init(
        name: String, note: String, surface: UInt32, ground: UInt32, ink: UInt32,
        inkSoft: UInt32, muted: UInt32, faint: UInt32, accent: UInt32, ok: UInt32,
        warn: UInt32, danger: UInt32, dark: Bool, hairline: CGFloat, rule: CGFloat,
        border: CGFloat, selection: CGFloat
    ) {
        self.name = name
        self.note = note
        self.surface = NSColor(hex: surface)
        self.ground = NSColor(hex: ground)
        let type = NSColor(hex: ink)
        self.ink = type
        self.inkSoft = NSColor(hex: inkSoft)
        self.muted = NSColor(hex: muted)
        self.faint = NSColor(hex: faint)
        let spent = NSColor(hex: accent)
        self.accent = spent
        self.ok = NSColor(hex: ok)
        self.warn = NSColor(hex: warn)
        self.danger = NSColor(hex: danger)
        self.hairline = type.withAlphaComponent(hairline)
        self.rule = type.withAlphaComponent(rule)
        self.border = type.withAlphaComponent(border)
        self.selection = spent.withAlphaComponent(selection)
        isDark = dark
    }

    /// Warm dark. The one the design was drawn in.
    ///
    /// `warn` is a cool blue rather than amber: it means "left running", and an outcome
    /// drawn in the accent would read as the selected row. Every palette below keeps that
    /// rule — no outcome colour may be the accent, or near it.
    static let ember = Palette(
        name: "Ember", note: "Warm dark, amber.",
        surface: 0x131210, ground: 0x1C1A17, ink: 0xF0ECE5, inkSoft: 0xD4CFC5,
        muted: 0x938C80, faint: 0x7A7468, accent: 0xE0A533,
        ok: 0x6EC28B, warn: 0x74A9DB, danger: 0xE8705C, dark: true,
        hairline: 0.13, rule: 0.30, border: 0.18, selection: 0.16)

    /// Neutral dark. The accent is the only hue on screen, which is what the grey-icon
    /// rule was built for.
    static let graphite = Palette(
        name: "Graphite", note: "Neutral dark, periwinkle.",
        surface: 0x1A1A1C, ground: 0x232326, ink: 0xF2F2F4, inkSoft: 0xD6D6DA,
        muted: 0x94949C, faint: 0x6E6E77, accent: 0x4D8DFF,
        ok: 0x4ADE80, warn: 0xFBBF24, danger: 0xF87171, dark: true,
        hairline: 0.13, rule: 0.30, border: 0.18, selection: 0.18)

    /// Cool dark. Reads closest to a terminal.
    static let slate = Palette(
        name: "Slate", note: "Cool dark, cyan.",
        surface: 0x0F1416, ground: 0x171E21, ink: 0xE6EDEE, inkSoft: 0xC6D0D2,
        muted: 0x849498, faint: 0x5F6E71, accent: 0x4FD1E0,
        ok: 0x7BD88F, warn: 0xE0B457, danger: 0xF0776B, dark: true,
        hairline: 0.13, rule: 0.30, border: 0.18, selection: 0.16)

    /// Indigo dark. The softest of the four, and the only one with a cool ground and a
    /// warm accent.
    static let nocturne = Palette(
        name: "Nocturne", note: "Indigo dark, rose.",
        surface: 0x14131C, ground: 0x1D1B28, ink: 0xEDEAF5, inkSoft: 0xCFCBDE,
        muted: 0x8F8AA3, faint: 0x6B6780, accent: 0xF07BA0,
        ok: 0x7DD3A0, warn: 0xF3C969, danger: 0xEF6F6F, dark: true,
        hairline: 0.14, rule: 0.30, border: 0.20, selection: 0.16)

    /// Warm light. The plate inverted: hairlines in ink, and a window appearance to match.
    static let parchment = Palette(
        name: "Parchment", note: "Warm light, ink blue.",
        surface: 0xFFFFFF, ground: 0xF4F2ED, ink: 0x16150F, inkSoft: 0x33302A,
        muted: 0x6F6B63, faint: 0x9A958C, accent: 0x1F4FD8,
        ok: 0x1A7F37, warn: 0x9A6700, danger: 0xB42318, dark: false,
        hairline: 0.11, rule: 0.30, border: 0.15, selection: 0.09)

    /// Cool light, and the loudest accent of the six.
    static let coolPaper = Palette(
        name: "Cool paper", note: "Cool light, vermilion.",
        surface: 0xFFFFFF, ground: 0xF1F3F5, ink: 0x111315, inkSoft: 0x2E3338,
        muted: 0x69707A, faint: 0x99A1AB, accent: 0xE2552F,
        ok: 0x0E7C4A, warn: 0x9A6700, danger: 0xC0341B, dark: false,
        hairline: 0.11, rule: 0.30, border: 0.15, selection: 0.10)

    /// Dark first, then light — the order the picker draws them.
    static let all: [Palette] = [ember, graphite, slate, nocturne, parchment, coolPaper]

    static func named(_ name: String) -> Palette {
        all.first { $0.name == name } ?? ember
    }

    // MARK: - Remembering which one

    /// One line, beside the other state blindspot keeps — the same arrangement the picked
    /// model uses. Not config.toml: that file is yours to edit, and the core could not
    /// describe these anyway, since which palettes exist is a fact about this file.
    private static var file: URL? {
        guard let home = ProcessInfo.processInfo.environment["HOME"], !home.isEmpty else {
            return nil
        }
        return URL(fileURLWithPath: home)
            .appendingPathComponent(".local/share/blindspot/palette")
    }

    static func remembered() -> Palette {
        guard let file, let text = try? String(contentsOf: file, encoding: .utf8) else {
            return ember
        }
        return named(text.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    /// Best effort, like everything else blindspot writes: a launcher must not fail
    /// because it could not record a preference.
    static func remember(_ name: String) {
        guard let file else { return }
        try? FileManager.default.createDirectory(
            at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? name.write(to: file, atomically: true, encoding: .utf8)
    }
}
