import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// The Preferences window (⌘,): video/audio settings, per-system link
/// attachments, and the memory-card / HDD / save-file manager.
struct SettingsView: View {
    var body: some View {
        TabView {
            GeneralSettingsView()
                .tabItem { Label("General", systemImage: "slider.horizontal.3") }
            ControlsSettingsView()
                .tabItem { Label("Controls", systemImage: "gamecontroller") }
            AttachmentsSettingsView()
                .tabItem { Label("Attachments", systemImage: "cable.connector") }
            StorageSettingsView()
                .tabItem { Label("Storage", systemImage: "internaldrive") }
        }
        .frame(width: 520, height: 440)
    }
}

// ---- General (video / audio) ----

private struct GeneralSettingsView: View {
    @EnvironmentObject var settings: AppSettings

    var body: some View {
        Form {
            Section("Video") {
                Picker("Scaling filter", selection: $settings.videoFilter) {
                    ForEach(AppSettings.VideoFilter.allCases) { f in
                        Text(f.label).tag(f)
                    }
                }
                Picker("Upscaler", selection: $settings.upscale) {
                    ForEach(AppSettings.Upscale.allCases) { u in
                        Text(u.label).tag(u)
                    }
                }
                Picker("Retro effect", selection: $settings.videoEffect) {
                    ForEach(AppSettings.VideoEffect.allCases) { e in
                        Text(e.label).tag(e)
                    }
                }
                Toggle("Snap to integer scale", isOn: $settings.integerScale)
            }
            Section("Audio") {
                Toggle("Enable audio", isOn: $settings.audioEnabled)
                HStack {
                    Text("Volume")
                    Slider(value: $settings.volume, in: 0...1)
                        .disabled(!settings.audioEnabled)
                    Text("\(Int(settings.volume * 100))%")
                        .monospacedDigit()
                        .frame(width: 44, alignment: .trailing)
                }
            }
            Section("Emulation") {
                Toggle("Keep running when window is in the background", isOn: $settings.runInBackground)
            }
        }
        .formStyle(.grouped)
    }
}

// ---- Attachments (per-system link peripheral) ----

private struct AttachmentsSettingsView: View {
    @EnvironmentObject var settings: AppSettings

    /// Only systems that model a link port are configurable.
    private var systems: [EmuSystem] {
        EmuSystem.displayOrder.filter { $0.hasAttachments }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if systems.isEmpty {
                emptyState("No system supports link attachments yet.")
            } else {
                Form {
                    Section {
                        ForEach(systems) { system in
                            Picker(system.fullName, selection: binding(for: system)) {
                                ForEach(system.supportedAttachments) { a in
                                    Label(a.label, systemImage: a.symbol).tag(a)
                                }
                            }
                        }
                    } footer: {
                        Text("Chosen per system and applied when a game launches. "
                            + "The GBA Wireless Adapter enables Download Play and wireless multiplayer.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                }
                .formStyle(.grouped)
            }
        }
    }

    private func binding(for system: EmuSystem) -> Binding<Attachment> {
        Binding(
            get: { settings.attachment(for: system) },
            set: { settings.setAttachment($0, for: system) }
        )
    }
}

// ---- Storage (memory cards / HDD / battery saves) ----

private struct StorageSettingsView: View {
    @EnvironmentObject var settings: AppSettings
    /// Bumped to force a re-list after import/delete (SaveStore is on-disk).
    @State private var revision = 0
    @State private var selection: EmuSystem = .gba

    /// Systems that have managed on-disk storage.
    private var systems: [EmuSystem] {
        EmuSystem.displayOrder.filter { $0.saveKind != .none }
    }

    var body: some View {
        HSplitView {
            // System list, grouped by storage category.
            List(systems, selection: $selection) { system in
                Label {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(system.label)
                        Text(system.saveKind.label)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                } icon: {
                    Image(systemName: system.saveKind.symbol)
                }
                .tag(system)
            }
            .frame(minWidth: 160)

            StorageDetail(system: selection, revision: $revision)
                .frame(minWidth: 300)
        }
        .onAppear { if let first = systems.first { selection = first } }
    }
}

/// The save files for one system, with import / reveal / delete actions.
private struct StorageDetail: View {
    let system: EmuSystem
    @Binding var revision: Int

    private var entries: [SaveEntry] {
        _ = revision // re-read when revision changes
        return SaveStore.shared.entries(for: system)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("\(system.fullName) — \(system.saveKind.label)s")
                    .font(.headline)
                Spacer()
                Button { importSave() } label: { Label("Import", systemImage: "square.and.arrow.down") }
                Button { revealFolder() } label: { Label("Reveal", systemImage: "folder") }
            }

            if entries.isEmpty {
                emptyState("No \(system.saveKind.label.lowercased())s yet.\n"
                    + "They appear here automatically as you play, or import one above.")
            } else {
                List {
                    ForEach(entries) { entry in
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(entry.name).lineLimit(1)
                                Text("\(byteText(entry.size)) · \(dateText(entry.modified))")
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button(role: .destructive) {
                                SaveStore.shared.delete(entry)
                                revision += 1
                            } label: { Image(systemName: "trash") }
                                .buttonStyle(.borderless)
                        }
                        .padding(.vertical, 2)
                    }
                }
            }
        }
        .padding()
    }

    private func importSave() {
        guard let ext = system.saveKind.fileExtension else { return }
        let panel = NSOpenPanel()
        // Filter to the system's native extension (a dynamic UTType is fine for
        // raw .sav/.mcr/.img images).
        panel.allowedContentTypes = UTType(filenameExtension: ext).map { [$0] } ?? []
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            SaveStore.shared.importFile(url, for: system)
            revision += 1
        }
    }

    private func revealFolder() {
        NSWorkspace.shared.activateFileViewerSelecting([SaveStore.shared.directory(for: system)])
    }

    private func byteText(_ n: Int) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(n), countStyle: .file)
    }
    private func dateText(_ d: Date) -> String {
        let f = DateFormatter()
        f.dateStyle = .short
        f.timeStyle = .short
        return f.string(from: d)
    }
}

// ---- Controls (keyboard remapping) ----

private struct ControlsSettingsView: View {
    @EnvironmentObject var settings: AppSettings
    @State private var capturing: Btn?
    @State private var monitor: Any?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {
                    ForEach(Btn.bindOrder) { btn in
                        HStack {
                            Text(btn.label)
                            Spacer()
                            Button { beginCapture(btn) } label: {
                                Text(capturing == btn
                                     ? "Press a key…"
                                     : (settings.binding(for: btn)?.label ?? "—"))
                                    .frame(minWidth: 92)
                            }
                            .tint(capturing == btn ? .accentColor : nil)
                        }
                    }
                } header: {
                    Text("Keyboard")
                } footer: {
                    Text("Click a button, then press the key to bind it (Esc cancels). "
                        + "Controllers use a fixed DualSense-style layout.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .formStyle(.grouped)

            HStack {
                Spacer()
                Button("Reset to Defaults") {
                    cancelCapture()
                    settings.resetBindings()
                }
            }
            .padding(.horizontal)
            .padding(.bottom, 12)
        }
        .onDisappear(perform: cancelCapture)
    }

    private func beginCapture(_ btn: Btn) {
        cancelCapture()
        capturing = btn
        // Swallow key/modifier events while binding so they don't leak to the UI.
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .flagsChanged]) { ev in
            handleCapture(ev)
            return nil
        }
    }

    private func handleCapture(_ ev: NSEvent) {
        guard let btn = capturing else { return }
        switch ev.type {
        case .keyDown:
            if ev.keyCode == 53 { cancelCapture(); return } // Esc cancels
            settings.setBinding(.key(ev.keyCode), for: btn)
            cancelCapture()
        case .flagsChanged:
            // Bind to the modifier that's now held (ignore pure releases).
            for m in KeyBind.Modifier.allCases where ev.modifierFlags.contains(m.flag) {
                settings.setBinding(.modifier(m), for: btn)
                cancelCapture()
                return
            }
        default:
            break
        }
    }

    private func cancelCapture() {
        capturing = nil
        if let m = monitor { NSEvent.removeMonitor(m); monitor = nil }
    }
}

@ViewBuilder
private func emptyState(_ text: String) -> some View {
    VStack {
        Spacer()
        Text(text)
            .multilineTextAlignment(.center)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity)
        Spacer()
    }
}
