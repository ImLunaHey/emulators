import AppKit
import Combine
import QuartzCore

/// Owns the running session and drives the ~60 Hz render loop. Lives on the main
/// thread (the timer runs on the main run loop); SwiftUI views observe it.
final class EmuHub: ObservableObject {
    @Published var isPlaying = false
    @Published var title = ""
    @Published var systemLabel = ""
    @Published var controllerInfo = "No controller"

    private var emu: Emulator?
    private let input = InputManager()
    private var audio: AudioPlayer?
    private var timer: Timer?
    private var keyMonitor: Any?
    private weak var screenLayer: CALayer?
    private var audioBuf = [Float](repeating: 0, count: 16_384)
    private let colorSpace = CGColorSpaceCreateDeviceRGB()

    /// In-memory BIOS/flash images per system (PS1, Xbox).
    private var bios: [EmuSystem: Data] = [:]

    // ---- screen wiring ----
    func attach(layer: CALayer) { screenLayer = layer }

    // ---- BIOS ----
    func setBios(_ data: Data, for system: EmuSystem) { bios[system] = data }
    func hasBios(_ system: EmuSystem) -> Bool { bios[system] != nil }

    // ---- lifecycle ----
    func launch(system: EmuSystem, rom: Data, title: String) {
        stop()
        guard let e = Emulator(system: system) else {
            self.title = "Failed to create \(system.label) core"
            return
        }
        if let b = bios[system] { _ = e.loadBIOS(b) }
        _ = e.loadROM(rom)
        emu = e
        self.title = title
        self.systemLabel = system.label
        isPlaying = true

        audio = AudioPlayer(sampleRate: e.sampleRate, channels: e.channels)
        audio?.start()

        installKeyMonitor()
        let t = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in self?.tick() }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        if let m = keyMonitor { NSEvent.removeMonitor(m); keyMonitor = nil }
        audio?.stop()
        audio = nil
        emu = nil
        isPlaying = false
        input.clear()
    }

    // ---- frame ----
    private func tick() {
        guard let e = emu else { return }
        e.setKeys(e.system.keyMask(input.currentButtons()))
        e.runFrame()
        present(e)
        let n = e.drainAudio(into: &audioBuf)
        if n > 0 { audio?.enqueue(audioBuf[0..<n]) }
        controllerInfo = input.controllerConnected
            ? (input.controllerName ?? "Controller connected")
            : "No controller — using keyboard"
    }

    private func present(_ e: Emulator) {
        let w = e.width
        let h = e.height
        guard w > 0, h > 0 else { return }
        let image: CGImage? = e.withFramebuffer { ptr, len in
            guard let ptr, len == w * h * 4 else { return nil }
            let data = Data(bytes: ptr, count: len) // copy: ptr is valid only until next runFrame
            guard let provider = CGDataProvider(data: data as CFData) else { return nil }
            let info = CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)
            return CGImage(
                width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 32,
                bytesPerRow: w * 4, space: colorSpace, bitmapInfo: info,
                provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent
            )
        }
        if let image {
            CATransaction.begin()
            CATransaction.setDisableActions(true) // no implicit fade between frames
            screenLayer?.contents = image
            CATransaction.commit()
        }
    }

    private func installKeyMonitor() {
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp, .flagsChanged]) { [weak self] ev in
            guard let self else { return ev }
            // Don't swallow Cmd-shortcuts (Cmd-Q, etc.).
            if ev.modifierFlags.contains(.command) { return ev }
            switch ev.type {
            case .keyDown:
                self.input.handleKey(ev, down: true)
                return nil // swallow so the app doesn't beep on game keys
            case .keyUp:
                self.input.handleKey(ev, down: false)
                return nil
            case .flagsChanged:
                self.input.handleFlags(ev)
                return ev
            default:
                return ev
            }
        }
    }
}
