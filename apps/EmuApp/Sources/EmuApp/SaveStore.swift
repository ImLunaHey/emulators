import Foundation

/// One managed save file (battery `.sav`, PS1 `.mcr`, or Xbox HDD image).
struct SaveEntry: Identifiable {
    let url: URL
    var id: URL { url }
    var name: String { url.deletingPathExtension().lastPathComponent }
    var size: Int { (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0 }
    var modified: Date {
        (try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate)
            ?? .distantPast
    }
}

/// Persists per-game save data as native files under Application Support, and
/// lists / imports / exports / deletes them for the management UI.
///
/// Layout: `.../Application Support/EmuApp/saves/<System>/<game>.<ext>` where the
/// extension is the system's native format (`sav` / `mcr` / `img`). Battery saves
/// are interchangeable with other emulators that use raw `.sav` images.
struct SaveStore {
    static let shared = SaveStore()
    private let fm = FileManager.default

    /// `.../Application Support/EmuApp/saves/<System>/`, created on demand.
    func directory(for system: EmuSystem) -> URL {
        let base = fm.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("EmuApp", isDirectory: true)
            .appendingPathComponent("saves", isDirectory: true)
            .appendingPathComponent(system.label, isDirectory: true)
        try? fm.createDirectory(at: base, withIntermediateDirectories: true)
        return base
    }

    /// File URL for a game's save, or nil if the system has no managed storage.
    func url(for system: EmuSystem, game: String) -> URL? {
        guard let ext = system.saveKind.fileExtension else { return nil }
        return directory(for: system)
            .appendingPathComponent(sanitize(game))
            .appendingPathExtension(ext)
    }

    func load(system: EmuSystem, game: String) -> Data? {
        guard let url = url(for: system, game: game) else { return nil }
        return try? Data(contentsOf: url)
    }

    func save(_ data: Data, system: EmuSystem, game: String) {
        guard let url = url(for: system, game: game) else { return }
        try? data.write(to: url, options: .atomic)
    }

    /// All managed save files for a system, newest first.
    func entries(for system: EmuSystem) -> [SaveEntry] {
        guard let ext = system.saveKind.fileExtension else { return [] }
        let dir = directory(for: system)
        let urls = (try? fm.contentsOfDirectory(
            at: dir,
            includingPropertiesForKeys: [.fileSizeKey, .contentModificationDateKey])) ?? []
        return urls
            .filter { $0.pathExtension.lowercased() == ext }
            .map(SaveEntry.init)
            .sorted { $0.modified > $1.modified }
    }

    func delete(_ entry: SaveEntry) {
        try? fm.removeItem(at: entry.url)
    }

    /// Import an external save file for `system` under its own name.
    func importFile(_ source: URL, for system: EmuSystem) {
        let name = source.deletingPathExtension().lastPathComponent
        if let data = try? Data(contentsOf: source) {
            save(data, system: system, game: name)
        }
    }

    private func sanitize(_ name: String) -> String {
        String(name.map { "/:\0".contains($0) ? "_" : $0 })
    }
}
