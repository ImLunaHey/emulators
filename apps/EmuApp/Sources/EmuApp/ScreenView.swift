import AppKit
import SwiftUI

/// A layer-backed NSView whose `content` layer the render loop writes CGImages
/// into. Nearest-neighbour, aspect-fit, black letterbox — the right look for
/// pixel art.
final class ScreenNSView: NSView {
    let content = CALayer()

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
        layer?.addSublayer(content)
    }

    override func layout() {
        super.layout()
        content.frame = bounds
    }
}

/// SwiftUI wrapper that hands its content layer to the hub's render loop.
struct ScreenView: NSViewRepresentable {
    let hub: EmuHub

    func makeNSView(context: Context) -> ScreenNSView {
        let v = ScreenNSView()
        hub.attach(layer: v.content, view: v)
        return v
    }

    func updateNSView(_ nsView: ScreenNSView, context: Context) {}
}
