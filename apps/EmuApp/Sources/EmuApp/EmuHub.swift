import AppKit
import Combine
import QuartzCore

/// Owns the running session and drives the ~60 Hz render loop. Lives on the main
/// thread (the timer runs on the main run loop); SwiftUI views observe it.
final class EmuHub: ObservableObject {
    @Published var isPlaying = false
    @Published var title = ""
    @Published var systemLabel = ""
    /// The running system (nil when stopped) — drives the in-game control bar.
    @Published var currentSystem: EmuSystem?
    @Published var controllerInfo = "No controller"
    /// Most recent measured frames-per-second (updated ~4×/sec).
    @Published var fps: Double = 0
    /// Rolling FPS history for the on-screen graph (oldest first, ~last 6 s).
    @Published var fpsHistory: [Double] = []

    private var emu: Emulator?
    private let input = InputManager()
    private var audio: AudioPlayer?
    private var timer: Timer?
    private var keyMonitor: Any?
    private weak var screenLayer: CALayer?
    private var audioBuf = [Float](repeating: 0, count: 16_384)
    private let colorSpace = CGColorSpaceCreateDeviceRGB()
    private var lastControllerInfo = ""
    private var fpsAccum = 0
    private var fpsClock = CACurrentMediaTime()

    /// In-memory BIOS/flash images per system (PS1, Xbox).
    private var bios: [EmuSystem: Data] = [:]

    /// App settings (video/audio/attachments). Set via `configure` before launch.
    private var settings: AppSettings?
    /// The running game, so we can persist its save on stop / autosave.
    private var current: (system: EmuSystem, title: String)?
    /// Frames since the last autosave check (~every 5 s).
    private var saveClock = 0

    /// Inject the shared settings (call before `launch`).
    func configure(settings: AppSettings) { self.settings = settings }

    // ---- screen wiring ----
    /// The view hosting our screen layer; used to tell whether this window is the
    /// key window (so only the focused game consumes input when several run).
    private weak var hostView: NSView?
    func attach(layer: CALayer, view: NSView) {
        screenLayer = layer
        hostView = view
    }

    /// True when this hub's window is frontmost (or, before the view attaches,
    /// when it's the only window — single-window behaviour is unchanged).
    private var inputFocused: Bool {
        guard let win = hostView?.window else { return true }
        return win.isKeyWindow
    }

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
        // Restore the persisted save, then attach the chosen link peripheral.
        if let saved = SaveStore.shared.load(system: system, game: title) {
            e.loadSave(saved)
        }
        if let s = settings {
            e.setAttachment(s.attachment(for: system))
        }
        e.clearSaveDirty()
        emu = e
        current = (system, title)
        self.title = title
        self.systemLabel = system.label
        self.currentSystem = system
        isPlaying = true
        applyVideoSettings()
        applyBindings()

        if settings?.audioEnabled ?? true {
            audio = AudioPlayer(sampleRate: e.sampleRate, channels: e.channels)
            audio?.volume = Float(settings?.volume ?? 1.0)
            audio?.start()
        }

        installKeyMonitor()
        let t = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in self?.tick() }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        if let m = keyMonitor { NSEvent.removeMonitor(m); keyMonitor = nil }
        flushSave()
        audio?.stop()
        audio = nil
        emu = nil
        current = nil
        currentSystem = nil
        isPlaying = false
        input.clear()
    }

    /// Switch the link attachment on the running game and remember the choice.
    func setAttachment(_ attachment: Attachment) {
        guard let e = emu, let game = current else { return }
        e.setAttachment(attachment)
        settings?.setAttachment(attachment, for: game.system)
    }

    /// Persist the running game's save to disk if it changed.
    private func flushSave() {
        guard let e = emu, let game = current, e.saveDirty, let data = e.saveData() else { return }
        SaveStore.shared.save(data, system: game.system, game: game.title)
        e.clearSaveDirty()
    }

    /// Push the user's key bindings into the input manager (call on launch and
    /// whenever the bindings change).
    func applyBindings() {
        if let s = settings { input.bindings = s.effectiveBindings }
    }

    /// Push the current video settings into the screen layer (filter, scaling).
    func applyVideoSettings() {
        guard let layer = screenLayer, let s = settings else { return }
        layer.magnificationFilter = s.videoFilter == .smooth ? .linear : .nearest
    }

    /// Apply changed audio settings to a live session.
    func applyAudioSettings() {
        guard let s = settings else { return }
        audio?.volume = s.audioEnabled ? Float(s.volume) : 0
    }

    // ---- frame ----
    private func tick() {
        guard let e = emu else { return }
        // Optionally pause windows that aren't frontmost (off by default).
        if !(settings?.runInBackground ?? true) && !inputFocused {
            return
        }
        // Only the focused window drives input; background games keep emulating
        // with no buttons pressed.
        e.setKeys(inputFocused ? e.system.keyMask(input.currentButtons()) : 0)
        e.runFrame()

        // Autosave battery-backed games ~every 5 s when the save changed, so a
        // crash or force-quit can't lose much progress.
        saveClock += 1
        if saveClock >= 300 {
            saveClock = 0
            flushSave()
        }
        present(e)
        let n = e.drainAudio(into: &audioBuf)
        if n > 0 { audio?.enqueue(audioBuf[0..<n]) }

        // Only touch @Published state when it actually changes — mutating it every
        // frame forces SwiftUI to re-render both windows 60x/sec and starves the
        // main thread (looks like a freeze in a debug build). The screen itself
        // updates via the CALayer in present(), independent of SwiftUI.
        let info = input.controllerConnected
            ? (input.controllerName ?? "Controller connected")
            : "No controller — using keyboard"
        if info != lastControllerInfo {
            lastControllerInfo = info
            controllerInfo = info
        }

        // Frame-rate measurement — refresh the published value + graph history
        // ~4×/sec so the on-screen counter is responsive but cheap.
        fpsAccum += 1
        let now = CACurrentMediaTime()
        let dt = now - fpsClock
        if dt >= 0.25 {
            let measured = Double(fpsAccum) / dt
            fps = measured
            fpsHistory.append(measured)
            if fpsHistory.count > 240 { fpsHistory.removeFirst(fpsHistory.count - 240) }
            fpsAccum = 0
            fpsClock = now
        }
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
            // Only the focused game's hub handles keys (the monitor is app-wide,
            // so every open game would otherwise react to one keypress).
            if !self.inputFocused { return ev }
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
