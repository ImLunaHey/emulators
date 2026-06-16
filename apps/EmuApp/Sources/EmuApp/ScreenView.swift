import AppKit
import SwiftUI

/// A layer-backed NSView whose `content` layer the render loop writes CGImages
/// into. Nearest-neighbour by default, aspect-fit, black letterbox. An optional
/// `effect` overlay layer adds a retro look (scanlines / CRT / LCD grid) on top.
final class ScreenNSView: NSView {
    let content = CALayer()
    /// Static retro-effect overlay above the game; regenerated only on resize or
    /// effect change (not per frame), so it's cheap.
    private let effect = CALayer()
    private var effectKind: AppSettings.VideoEffect = .none
    /// Snap the picture to an integer multiple of its source size (pixel-perfect).
    private var integer = false
    /// Size of the most recently presented frame, for integer snapping.
    private var sourceSize: CGSize = .zero

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setup()
    }
    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setup()
    }

    private func setup() {
        wantsLayer = true
        layer?.backgroundColor = NSColor.black.cgColor
        content.magnificationFilter = .nearest
        content.minificationFilter = .linear
        content.contentsGravity = .resizeAspect
        // No implicit fade when the frame image is swapped.
        content.actions = ["contents": NSNull(), "frame": NSNull(), "bounds": NSNull()]
        layer?.addSublayer(content)

        effect.contentsGravity = .resize
        effect.zPosition = 1
        effect.actions = ["contents": NSNull(), "frame": NSNull(), "bounds": NSNull()]
        layer?.addSublayer(effect)
    }

    override func layout() {
        super.layout()
        effect.frame = bounds
        layoutContent()
        regenerateEffect()
    }

    /// The render loop hands each frame here so we can integer-snap the layer to
    /// the image's pixel grid when pixel-perfect mode is on.
    func present(_ image: CGImage) {
        sourceSize = CGSize(width: image.width, height: image.height)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        content.contents = image
        layoutContent()
        CATransaction.commit()
    }

    /// Apply the scaling filter (smooth vs nearest), integer snapping, and the
    /// retro overlay.
    func setVideo(smooth: Bool, integer: Bool, effect kind: AppSettings.VideoEffect) {
        content.magnificationFilter = smooth ? .linear : .nearest
        self.integer = integer
        if kind != effectKind {
            effectKind = kind
            regenerateEffect()
        }
        layoutContent()
    }

    /// Position the game layer: integer-snapped + centered when pixel-perfect,
    /// otherwise aspect-fit to the whole view.
    private func layoutContent() {
        guard bounds.width > 0, bounds.height > 0 else { return }
        if integer, sourceSize.width > 0, sourceSize.height > 0 {
            let s = max(1, floor(min(bounds.width / sourceSize.width,
                                     bounds.height / sourceSize.height)))
            let w = sourceSize.width * s
            let h = sourceSize.height * s
            content.contentsGravity = .resize
            content.frame = CGRect(x: ((bounds.width - w) / 2).rounded(),
                                   y: ((bounds.height - h) / 2).rounded(),
                                   width: w, height: h)
        } else {
            content.contentsGravity = .resizeAspect
            content.frame = bounds
        }
    }

    private func regenerateEffect() {
        let scale = window?.backingScaleFactor ?? 2
        effect.contentsScale = scale
        effect.contents = Self.effectImage(effectKind, size: bounds.size, scale: scale)
    }

    /// Build the overlay pattern (or nil for `.none`) at device resolution.
    private static func effectImage(_ kind: AppSettings.VideoEffect, size: CGSize, scale: CGFloat) -> CGImage? {
        guard kind != .none, size.width > 1, size.height > 1 else { return nil }
        let w = Int(size.width * scale)
        let h = Int(size.height * scale)
        let cs = CGColorSpaceCreateDeviceRGB()
        guard w > 0, h > 0,
              let ctx = CGContext(
                data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return nil }
        ctx.clear(CGRect(x: 0, y: 0, width: w, height: h))

        let lineH = max(1, Int(scale))          // ~1pt line
        let pitch = max(2, Int(3 * scale))      // every ~3pt

        switch kind {
        case .none:
            break
        case .scanlines, .crt:
            ctx.setFillColor(CGColor(gray: 0, alpha: 0.35))
            var y = 0
            while y < h { ctx.fill(CGRect(x: 0, y: y, width: w, height: lineH)); y += pitch }
            if kind == .crt {
                // Darken the corners for a tube vignette.
                let colors = [CGColor(gray: 0, alpha: 0), CGColor(gray: 0, alpha: 0.55)] as CFArray
                if let grad = CGGradient(colorsSpace: cs, colors: colors, locations: [0.55, 1.0]) {
                    let c = CGPoint(x: w / 2, y: h / 2)
                    ctx.drawRadialGradient(
                        grad, startCenter: c, startRadius: 0,
                        endCenter: c, endRadius: CGFloat(max(w, h)) * 0.72, options: [])
                }
            }
        case .lcd:
            // Faint dot-matrix grid (horizontal + vertical lines).
            ctx.setFillColor(CGColor(gray: 0, alpha: 0.22))
            var y = 0
            while y < h { ctx.fill(CGRect(x: 0, y: y, width: w, height: lineH)); y += pitch }
            var x = 0
            while x < w { ctx.fill(CGRect(x: x, y: 0, width: lineH, height: h)); x += pitch }
        }
        return ctx.makeImage()
    }
}

/// SwiftUI wrapper that hands its content layer to the hub's render loop and
/// applies the current video settings (filter + retro effect).
struct ScreenView: NSViewRepresentable {
    let hub: EmuHub
    @EnvironmentObject var settings: AppSettings

    func makeNSView(context: Context) -> ScreenNSView {
        let v = ScreenNSView()
        hub.attach(screen: v)
        v.setVideo(smooth: settings.upscale.smoothFilter,
                   integer: settings.upscale.integer,
                   effect: settings.videoEffect)
        return v
    }

    func updateNSView(_ nsView: ScreenNSView, context: Context) {
        nsView.setVideo(smooth: settings.upscale.smoothFilter,
                        integer: settings.upscale.integer,
                        effect: settings.videoEffect)
    }
}
