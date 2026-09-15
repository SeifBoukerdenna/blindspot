import AppKit

// Draws Blindspot's 1024 px master icon: `swift scripts/make-icon.swift out.png`.
// scripts/make-icon.sh resamples it into shell/AppIcon.icns and media/icon.png.
//
// The plate follows Apple's macOS icon template: an 824 px rounded square with a soft shadow in a
// 1024 px canvas. macOS 26 therefore shows it as drawn instead of boxing it in a grey tile.
//
// Colours are the Ember palette the panel ships with: warm charcoal and the amber accent. The mark
// is plain geometry, an amber disc missing one spot, the name's blind spot. There are no sparkles,
// glows or gradients-as-magic, so it reads as a tool rather than an AI product.

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write(Data("usage: swift scripts/make-icon.swift out.png\n".utf8))
    exit(64)
}

let canvas = 1024
guard let bitmap = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: canvas, pixelsHigh: canvas, bitsPerSample: 8, samplesPerPixel: 4,
    hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
    let context = NSGraphicsContext(bitmapImageRep: bitmap) else { exit(1) }
NSGraphicsContext.current = context

func colour(_ hex: UInt32, _ alpha: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255, alpha: alpha)
}

/// A superellipse: its corners ease into the sides like Apple's icon shape, where circular
/// corners would show a visible kink at this size.
func plate(_ rect: CGRect, exponent: CGFloat = 5) -> NSBezierPath {
    let path = NSBezierPath()
    for step in 0...1440 {
        let angle = CGFloat(step) / 1440 * 2 * .pi
        let (c, s) = (cos(angle), sin(angle))
        let point = NSPoint(
            x: rect.midX + rect.width / 2 * copysign(pow(abs(c), 2 / exponent), c),
            y: rect.midY + rect.height / 2 * copysign(pow(abs(s), 2 / exponent), s))
        step == 0 ? path.move(to: point) : path.line(to: point)
    }
    path.close()
    return path
}

func circle(_ centre: CGPoint, _ radius: CGFloat) -> NSBezierPath {
    NSBezierPath(ovalIn: CGRect(x: centre.x - radius, y: centre.y - radius, width: radius * 2, height: radius * 2))
}

let body = CGRect(x: 100, y: 100, width: 824, height: 824)
let shape = plate(body)
let ground = NSGradient(starting: colour(0x2F2B26), ending: colour(0x151311))!
let sheen = NSGradient(starting: NSColor.white.withAlphaComponent(0.06), ending: NSColor.white.withAlphaComponent(0))!

/// The plate's own surface, drawn again inside the spot so the missing piece is exactly the
/// background rather than a flat colour that would not match the gradient.
func surface() {
    ground.draw(in: body, angle: -90)
    sheen.draw(in: CGRect(x: body.minX, y: body.midY, width: body.width, height: body.height / 2), angle: -90)
}

// Shadow, as in Apple's template.
NSGraphicsContext.saveGraphicsState()
let shadow = NSShadow()
shadow.shadowColor = NSColor.black.withAlphaComponent(0.32)
shadow.shadowBlurRadius = 22
shadow.shadowOffset = NSSize(width: 0, height: -10)
shadow.set()
colour(0x1C1A17).setFill()
shape.fill()
NSGraphicsContext.restoreGraphicsState()

NSGraphicsContext.saveGraphicsState()
shape.addClip()
surface()

// A hairline inside the edge keeps the dark plate distinct on a dark Dock or desktop.
NSColor.white.withAlphaComponent(0.08).setStroke()
let edge = plate(body.insetBy(dx: 3, dy: 3))
edge.lineWidth = 3
edge.stroke()

let centre = CGPoint(x: 512, y: 500)
let radius: CGFloat = 262
let spot = CGPoint(x: centre.x + 158, y: centre.y + 150)
let spotRadius: CGFloat = 104
let gap: CGFloat = 30

NSGraphicsContext.saveGraphicsState()
circle(centre, radius).addClip()
NSGradient(starting: colour(0xF0B849), ending: colour(0xD4972C))!
    .draw(in: CGRect(x: centre.x - radius, y: centre.y - radius, width: radius * 2, height: radius * 2), angle: -90)
circle(spot, spotRadius + gap).addClip()
surface()
NSGraphicsContext.restoreGraphicsState()

colour(0xE0A533).setFill()
circle(spot, spotRadius * 0.42).fill()

NSGraphicsContext.restoreGraphicsState()
NSGraphicsContext.current = nil

guard let png = bitmap.representation(using: .png, properties: [:]) else { exit(1) }
try png.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
