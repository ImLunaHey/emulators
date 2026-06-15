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

    /// One-time: pre-populate the Xbox shelf with the Halo discs if they're on
    /// disk, so they show up in the launcher without a manual add.
    func seedDefaults() {
        let defaults = UserDefaults.standard
        let flag = "seeded.halo.v1"
        guard !defaults.bool(forKey: flag) else { return }
        let candidates = [
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
                result[sys] = paths.map { URL(fileURLWithPath: $0) }
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
