import Foundation
import SwiftUI

/// Persisted "recently played" lists, per system (paths in UserDefaults). The
/// macOS app has no ROM store of its own — you open files from disk — so this is
/// the console-first library's content.
final class Library: ObservableObject {
    @Published private(set) var recentsBySystem: [EmuSystem: [URL]] = [:]
    private let key = "recentsBySystem.v1"

    init() { load() }

    func recents(for system: EmuSystem) -> [URL] { recentsBySystem[system] ?? [] }

    func add(_ url: URL, system: EmuSystem) {
        var list = recentsBySystem[system] ?? []
        list.removeAll { $0 == url }
        list.insert(url, at: 0)
        if list.count > 12 { list = Array(list.prefix(12)) }
        recentsBySystem[system] = list
        save()
    }

    func remove(_ url: URL, system: EmuSystem) {
        recentsBySystem[system]?.removeAll { $0 == url }
        save()
    }

    /// One-time: pre-populate the Xbox shelf with the Halo discs and the bundled
    /// nxdk homebrew demo XBEs if they're on disk, so they show up in the
    /// launcher without a manual add.
    func seedDefaults() {
        let defaults = UserDefaults.standard
        // v3: paths moved from core-xbox/demos to packages/xbox/demos in the
        // monorepo restructure, so re-seed with the new locations.
        let flag = "seeded.xbox.v3"
        guard !defaults.bool(forKey: flag) else { return }
        // Repo root is two levels up from the SwiftPM executable's package dir.
        let repoRoot = "/Users/luna/code/imlunahey/emulator"
        let candidates = [
            "\(repoRoot)/packages/xbox/demos/triangle.xbe",
            "\(repoRoot)/packages/xbox/demos/hello.xbe",
            "/Users/luna/Downloads/Halo - Combat Evolved (USA).xiso.iso",
            "/Users/luna/Downloads/Halo 2 (USA, Europe) (En,Ja,Fr,De,Es,It,Zh,Ko)/Halo 2 (USA, Europe) (En,Ja,Fr,De,Es,It,Zh,Ko).xiso.iso",
        ]
        var seededAny = false
        for path in candidates where FileManager.default.fileExists(atPath: path) {
            add(URL(fileURLWithPath: path), system: .xbox)
            seededAny = true
        }
        if seededAny { defaults.set(true, forKey: flag) }
    }

    private func save() {
        var dict: [String: [String]] = [:]
        for (sys, urls) in recentsBySystem { dict[String(sys.rawValue)] = urls.map(\.path) }
        UserDefaults.standard.set(dict, forKey: key)
    }

    private func load() {
        guard let dict = UserDefaults.standard.dictionary(forKey: key) as? [String: [String]] else { return }
        var result: [EmuSystem: [URL]] = [:]
        for (k, paths) in dict {
            if let raw = UInt32(k), let sys = EmuSystem(rawValue: raw) {
                // Drop entries whose file no longer exists (e.g. a demo that
                // moved) so a stale path can't surface as "No game running".
                let live = paths.filter { FileManager.default.fileExists(atPath: $0) }
                if !live.isEmpty { result[sys] = live.map { URL(fileURLWithPath: $0) } }
            }
        }
        recentsBySystem = result
    }
}

extension Color {
    /// Build a Color from a "#rrggbb" hex string (falls back to gray).
    init(hex: String) {
        let s = hex.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
        var v: UInt64 = 0
        guard Scanner(string: s).scanHexInt64(&v), s.count == 6 else {
            self = .gray
            return
        }
        self = Color(
            red: Double((v >> 16) & 0xFF) / 255.0,
            green: Double((v >> 8) & 0xFF) / 255.0,
            blue: Double(v & 0xFF) / 255.0
        )
    }
}
