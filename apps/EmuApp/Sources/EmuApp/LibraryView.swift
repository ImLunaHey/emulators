import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// The "Consoles" window: a console-first, two-level launcher. Pick a console,
/// then a game (open a file, or re-launch a recent one) — which opens the Player
/// window. ROMs can also be added globally (button or drag-and-drop); each is
/// auto-routed to the right console by extension + content sniffing.
struct LibraryView: View {
    @EnvironmentObject var hub: EmuHub
    @EnvironmentObject var library: Library
    @Environment(\.openWindow) private var openWindow
    @State private var selected: EmuSystem?
    @State private var status: String = ""
    @State private var dropTargeted = false

    var body: some View {
        ZStack {
            Color(red: 0.05, green: 0.05, blue: 0.07).ignoresSafeArea()
            if let sys = selected {
                ConsoleDetail(system: sys, onBack: { selected = nil }, launch: launch, pickBios: pickBios)
            } else {
                ConsoleGrid(select: { selected = $0 }, onAdd: addROMs, status: status)
            }
            if dropTargeted {
                RoundedRectangle(cornerRadius: 16)
                    .strokeBorder(Color.accentColor, style: StrokeStyle(lineWidth: 3, dash: [10]))
                    .padding(10)
                    .allowsHitTesting(false)
            }
        }
        .frame(minWidth: 760, minHeight: 520)
        .onDrop(of: [UTType.fileURL], isTargeted: $dropTargeted) { providers in
            handleDrop(providers)
        }
    }

    // ---- actions ----
    private func launch(_ url: URL, _ declared: EmuSystem) {
        guard let data = try? Data(contentsOf: url, options: .mappedIfSafe) else { return }
        let system = EmuSystem.sniff(data, fallback: declared) ?? declared
        let title = url.deletingPathExtension().lastPathComponent
        hub.launch(system: system, rom: data, title: title)
        library.add(url, system: system)
        openWindow(id: "player")
    }

    private func openROM(for system: EmuSystem) {
        guard let url = Self.pickFile() else { return }
        launch(url, system)
    }

    private func pickBios(for system: EmuSystem) {
        guard let url = Self.pickFile(), let data = try? Data(contentsOf: url) else { return }
        hub.setBios(data, for: system)
    }

    // ---- global add (auto-route to the right console) ----
    private func addROMs() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.prompt = "Add"
        panel.message = "Add ROMs — they'll be sorted into the right console automatically."
        guard panel.runModal() == .OK else { return }
        importURLs(panel.urls)
    }

    private func handleDrop(_ providers: [NSItemProvider]) -> Bool {
        var any = false
        for p in providers where p.canLoadObject(ofClass: URL.self) {
            any = true
            _ = p.loadObject(ofClass: URL.self) { url, _ in
                guard let url else { return }
                DispatchQueue.main.async { importURLs([url]) }
            }
        }
        return any
    }

    private func importURLs(_ urls: [URL]) {
        var perSystem: [EmuSystem: Int] = [:]
        var skipped = 0
        for url in urls {
            if let sys = Self.resolveSystem(url) {
                library.add(url, system: sys)
                perSystem[sys, default: 0] += 1
            } else {
                skipped += 1
            }
        }
        let added = perSystem.values.reduce(0, +)
        if added == 0 {
            status = skipped > 0 ? "Couldn't recognize \(skipped) file(s)" : ""
        } else {
            let parts = perSystem
                .sorted { $0.value > $1.value }
                .map { "\($0.value) \($0.key.label)" }
            var msg = "Added " + parts.joined(separator: ", ")
            if skipped > 0 { msg += " · \(skipped) skipped" }
            status = msg
        }
    }

    /// Resolve a file's system by extension, refined by content sniffing for
    /// ambiguous disc images (an Xbox `.iso` carries the XDVDFS magic).
    static func resolveSystem(_ url: URL) -> EmuSystem? {
        let declared = EmuSystem.detect(filename: url.lastPathComponent)
        if declared == nil || declared == .ps1 {
            if let data = try? Data(contentsOf: url, options: .mappedIfSafe),
               let sniffed = EmuSystem.sniff(data, fallback: declared) {
                return sniffed
            }
        }
        return declared
    }

    static func pickFile() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        return panel.runModal() == .OK ? panel.url : nil
    }
}

// MARK: - Console grid (level 1)

private struct ConsoleGrid: View {
    @EnvironmentObject var library: Library
    let select: (EmuSystem) -> Void
    let onAdd: () -> Void
    let status: String

    private let columns = [GridItem(.adaptive(minimum: 210), spacing: 18)]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 12) {
                Text("Consoles").font(.system(size: 30, weight: .bold))
                Spacer()
                if !status.isEmpty {
                    Text(status)
                        .font(.system(size: 11))
                        .foregroundColor(.white.opacity(0.55))
                        .transition(.opacity)
                }
                Button(action: onAdd) {
                    Label("Add ROMs…", systemImage: "plus")
                }
                .keyboardShortcut("o", modifiers: .command)
                .help("Add games — auto-sorted into the right console")
            }
            .padding(.horizontal, 28)
            .padding(.top, 24)
            .padding(.bottom, 16)

            ScrollView {
                LazyVGrid(columns: columns, spacing: 18) {
                    ForEach(EmuSystem.displayOrder) { sys in
                        ConsoleTile(system: sys, count: library.recents(for: sys).count) {
                            select(sys)
                        }
                    }
                }
                .padding(.horizontal, 28)
                .padding(.bottom, 28)
            }
        }
    }
}

private struct ConsoleTile: View {
    let system: EmuSystem
    let count: Int
    let action: () -> Void
    @State private var hover = false

    var body: some View {
        let accent = Color(hex: system.accentHex)
        Button(action: action) {
            VStack(alignment: .leading, spacing: 6) {
                Text(system.label)
                    .font(.system(size: 26, weight: .heavy))
                    .foregroundColor(.white)
                Text(system.fullName)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundColor(.white.opacity(0.8))
                Spacer()
                HStack {
                    Text(count == 0 ? "No games yet" : "\(count) recent")
                        .font(.system(size: 11))
                        .foregroundColor(.white.opacity(0.7))
                    Spacer()
                }
            }
            .padding(16)
            .frame(height: 124, alignment: .topLeading)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                LinearGradient(
                    colors: [accent.opacity(0.85), accent.opacity(0.35)],
                    startPoint: .topLeading, endPoint: .bottomTrailing
                )
            )
            .cornerRadius(16)
            .overlay(
                RoundedRectangle(cornerRadius: 16)
                    .stroke(.white.opacity(hover ? 0.5 : 0.12), lineWidth: 1)
            )
            .shadow(color: accent.opacity(hover ? 0.6 : 0.25), radius: hover ? 16 : 8, y: 4)
            .scaleEffect(hover ? 1.02 : 1.0)
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
        .animation(.easeOut(duration: 0.12), value: hover)
    }
}

// MARK: - Console detail (level 2)

private struct ConsoleDetail: View {
    @EnvironmentObject var hub: EmuHub
    @EnvironmentObject var library: Library
    let system: EmuSystem
    let onBack: () -> Void
    let launch: (URL, EmuSystem) -> Void
    let pickBios: (EmuSystem) -> Void

    var body: some View {
        let accent = Color(hex: system.accentHex)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 12) {
                Button(action: onBack) {
                    Label("Consoles", systemImage: "chevron.left")
                }
                .buttonStyle(.plain)
                .foregroundColor(.white.opacity(0.8))
                Spacer()
            }
            .padding(.horizontal, 28)
            .padding(.top, 20)

            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text(system.label)
                    .font(.system(size: 34, weight: .heavy))
                    .foregroundColor(accent)
                Text(system.fullName)
                    .font(.system(size: 14))
                    .foregroundColor(.white.opacity(0.6))
                Spacer()
            }
            .padding(.horizontal, 28)
            .padding(.top, 8)

            HStack(spacing: 10) {
                Button { launchPick() } label: {
                    Label("Open \(system.label) ROM…", systemImage: "folder")
                }
                if system.needsBios {
                    Button { pickBios(system) } label: {
                        Label(hub.hasBios(system) ? "BIOS loaded ✓" : "Load BIOS…",
                              systemImage: "memorychip")
                    }
                }
                Spacer()
            }
            .padding(.horizontal, 28)
            .padding(.vertical, 16)

            if system.needsBios && !hub.hasBios(system) {
                Text(system == .xbox
                     ? "Tip: a flash BIOS isn't required just to mount a disc and read its title."
                     : "This system needs a BIOS to boot.")
                    .font(.system(size: 11))
                    .foregroundColor(.white.opacity(0.5))
                    .padding(.horizontal, 28)
                    .padding(.bottom, 8)
            }

            let recents = library.recents(for: system)
            if recents.isEmpty {
                Spacer()
                VStack(spacing: 8) {
                    Text("No \(system.label) games yet")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundColor(.white.opacity(0.7))
                    Text("Open a ROM to start playing.")
                        .font(.system(size: 12))
                        .foregroundColor(.white.opacity(0.45))
                }
                .frame(maxWidth: .infinity)
                Spacer()
            } else {
                ScrollView {
                    LazyVStack(spacing: 8) {
                        ForEach(recents, id: \.self) { url in
                            GameRow(url: url, accent: accent,
                                    play: { launch(url, system) },
                                    remove: { library.remove(url, system: system) })
                        }
                    }
                    .padding(.horizontal, 28)
                    .padding(.bottom, 24)
                }
            }
        }
    }

    private func launchPick() {
        guard let url = LibraryView.pickFile() else { return }
        launch(url, system)
    }
}

private struct GameRow: View {
    let url: URL
    let accent: Color
    let play: () -> Void
    let remove: () -> Void
    @State private var hover = false

    var body: some View {
        HStack(spacing: 12) {
            RoundedRectangle(cornerRadius: 6).fill(accent.opacity(0.7)).frame(width: 8, height: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text(url.deletingPathExtension().lastPathComponent)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundColor(.white)
                    .lineLimit(1)
                Text(url.path)
                    .font(.system(size: 10))
                    .foregroundColor(.white.opacity(0.4))
                    .lineLimit(1)
            }
            Spacer()
            if hover {
                Button(action: remove) { Image(systemName: "trash") }
                    .buttonStyle(.plain)
                    .foregroundColor(.white.opacity(0.6))
            }
            Button(action: play) { Image(systemName: "play.fill") }
                .buttonStyle(.plain)
                .foregroundColor(accent)
        }
        .padding(12)
        .background(.white.opacity(hover ? 0.08 : 0.04))
        .cornerRadius(10)
        .contentShape(Rectangle())
        .onTapGesture(perform: play)
        .onHover { hover = $0 }
    }
}
