// Draws the gpudash icon: four GPU-load bars in the app's temperature gradient.
// Usage: swift make_icon.swift out.png [size]
import AppKit

let args = CommandLine.arguments
let out = args[1]
let S = CGFloat(Double(args.count > 2 ? args[2] : "1024")!)
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(S), pixelsHigh: Int(S), bitsPerSample: 8,
                           samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                           bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let k = S / 1024

func rgb(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat) -> NSColor {
    NSColor(srgbRed: r / 255, green: g / 255, blue: b / 255, alpha: 1)
}

// background: macOS-style rounded square with a soft vertical gradient
let inset = 100 * k
let bg = NSRect(x: inset, y: inset, width: S - 2 * inset, height: S - 2 * inset)
let bgPath = NSBezierPath(roundedRect: bg, xRadius: 185 * k, yRadius: 185 * k)
NSGradient(starting: rgb(44, 49, 64), ending: rgb(20, 22, 30))!.draw(in: bgPath, angle: -90)
rgb(70, 76, 96).setStroke()
bgPath.lineWidth = 4 * k
bgPath.stroke()

// four bars, cool to hot
let colors = [rgb(90, 160, 255), rgb(80, 220, 140), rgb(240, 210, 80), rgb(255, 80, 70)]
let heights: [CGFloat] = [0.38, 0.58, 0.78, 0.95]
let barW = 112 * k, gap = 44 * k
let total = 4 * barW + 3 * gap
let x0 = (S - total) / 2
let base = 250 * k
let maxH = 500 * k
for i in 0..<4 {
    let x = x0 + CGFloat(i) * (barW + gap)
    // track
    let track = NSBezierPath(roundedRect: NSRect(x: x, y: base, width: barW, height: maxH), xRadius: 26 * k, yRadius: 26 * k)
    rgb(48, 53, 68).setFill()
    track.fill()
    // fill
    let h = maxH * heights[i]
    let bar = NSBezierPath(roundedRect: NSRect(x: x, y: base, width: barW, height: h), xRadius: 26 * k, yRadius: 26 * k)
    NSGradient(starting: colors[i].blended(withFraction: 0.25, of: .black)!, ending: colors[i])!.draw(in: bar, angle: 90)
}

// baseline
let line = NSBezierPath(roundedRect: NSRect(x: x0 - 30 * k, y: base - 46 * k, width: total + 60 * k, height: 18 * k),
                        xRadius: 9 * k, yRadius: 9 * k)
rgb(120, 170, 255).setFill()
line.fill()

NSGraphicsContext.current = nil
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
