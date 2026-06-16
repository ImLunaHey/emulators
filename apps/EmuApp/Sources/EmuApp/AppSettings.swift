import Foundation

/// App-wide, persisted settings, shared via the environment so the library and
/// every player window read one set of values. Backed by `UserDefaults`; each
/// property publishes on change (so SwiftUI views update) and writes through.
final class AppSettings: ObservableObject {
    /// How the screen is scaled up. Pixel-art cores want the sharp (nearest)
    /// filter; the smooth (linear) filter softens upscaling.
    enum VideoFilter: String, CaseIterable, Identifiable {
        case sharp
        case smooth
        var id: String { rawValue }
        var label: String { self == .sharp ? "Sharp (nearest)" : "Smooth (linear)" }
    }

    /// A retro display overlay drawn on top of the game.
    enum VideoEffect: String, CaseIterable, Identifiable {
        case none
        case scanlines
        case crt
        case lcd
        var id: String { rawValue }
        var label: String {
            switch self {
            case .none: return "None"
            case .scanlines: return "Scanlines"
            case .crt: return "CRT"
            case .lcd: return "LCD Grid"
            }
        }
    }

    @Published var videoFilter: VideoFilter {
        didSet { d.set(videoFilter.rawValue, forKey: K.videoFilter) }
    }
    @Published var videoEffect: VideoEffect {
        didSet { d.set(videoEffect.rawValue, forKey: K.videoEffect) }
    }
    @Published var integerScale: Bool {
        didSet { d.set(integerScale, forKey: K.integerScale) }
    }
    @Published var audioEnabled: Bool {
        didSet { d.set(audioEnabled, forKey: K.audioEnabled) }
    }
    /// Output volume, 0...1.
    @Published var volume: Double {
        didSet { d.set(volume, forKey: K.volume) }
    }
    /// Keep emulating windows that aren't frontmost (off = pause in background).
    @Published var runInBackground: Bool {
        didSet { d.set(runInBackground, forKey: K.runInBackground) }
    }

    /// Per-system attachment choice, persisted as one JSON map keyed by the
    /// system's raw value.
    @Published private var attachments: [UInt32: UInt32] {
        didSet { persistAttachments() }
    }

    /// Custom keyboard bindings (logical button → key), overlaid on the
    /// defaults. Persisted as JSON keyed by the button's raw name.
    @Published private var keyBinds: [Btn: KeyBind] {
        didSet { persistKeyBinds() }
    }

    private let d: UserDefaults
    private enum K {
        static let videoFilter = "settings.video.filter"
        static let videoEffect = "settings.video.effect"
        static let integerScale = "settings.video.integerScale"
        static let audioEnabled = "settings.audio.enabled"
        static let volume = "settings.audio.volume"
        static let runInBackground = "settings.emu.runInBackground"
        static let attachments = "settings.attachments"
        static let keyBinds = "settings.keyBinds"
    }

    init(defaults: UserDefaults = .standard) {
        d = defaults
        videoFilter = VideoFilter(rawValue: d.string(forKey: K.videoFilter) ?? "") ?? .sharp
        videoEffect = VideoEffect(rawValue: d.string(forKey: K.videoEffect) ?? "") ?? .none
        integerScale = d.object(forKey: K.integerScale) as? Bool ?? false
        audioEnabled = d.object(forKey: K.audioEnabled) as? Bool ?? true
        volume = d.object(forKey: K.volume) as? Double ?? 1.0
        runInBackground = d.object(forKey: K.runInBackground) as? Bool ?? true
        if let raw = d.string(forKey: K.attachments),
           let data = raw.data(using: .utf8),
           let map = try? JSONDecoder().decode([String: UInt32].self, from: data) {
            attachments = Dictionary(uniqueKeysWithValues: map.compactMap { key, value in
                UInt32(key).map { ($0, value) }
            })
        } else {
            attachments = [:]
        }
        if let raw = d.string(forKey: K.keyBinds),
           let data = raw.data(using: .utf8),
           let map = try? JSONDecoder().decode([String: KeyBind].self, from: data) {
            keyBinds = Dictionary(uniqueKeysWithValues: map.compactMap { key, value in
                Btn(rawValue: key).map { ($0, value) }
            })
        } else {
            keyBinds = [:]
        }
    }

    // ---- key bindings ----

    /// The effective binding for a button (custom override, else the default).
    func binding(for btn: Btn) -> KeyBind? {
        keyBinds[btn] ?? DefaultBindings.map[btn]
    }

    /// The full effective binding table (defaults overlaid with overrides).
    var effectiveBindings: [Btn: KeyBind] {
        var map = DefaultBindings.map
        for (btn, bind) in keyBinds { map[btn] = bind }
        return map
    }

    func setBinding(_ bind: KeyBind, for btn: Btn) {
        keyBinds[btn] = bind
    }

    /// Restore every button to its default key.
    func resetBindings() {
        keyBinds = [:]
    }

    private func persistKeyBinds() {
        let map = Dictionary(uniqueKeysWithValues: keyBinds.map { ($0.key.rawValue, $0.value) })
        if let data = try? JSONEncoder().encode(map), let raw = String(data: data, encoding: .utf8) {
            d.set(raw, forKey: K.keyBinds)
        }
    }

    // ---- per-system attachment ----

    func attachment(for system: EmuSystem) -> Attachment {
        guard let raw = attachments[system.rawValue], let a = Attachment(rawValue: raw) else {
            // Default a system that has a link port to a plain cable; else none.
            return system.hasAttachments ? .linkCable : .none
        }
        return a
    }

    func setAttachment(_ attachment: Attachment, for system: EmuSystem) {
        attachments[system.rawValue] = attachment.rawValue
    }

    private func persistAttachments() {
        let map = Dictionary(uniqueKeysWithValues: attachments.map { (String($0.key), $0.value) })
        if let data = try? JSONEncoder().encode(map), let raw = String(data: data, encoding: .utf8) {
            d.set(raw, forKey: K.attachments)
        }
    }
}
