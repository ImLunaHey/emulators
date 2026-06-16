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
                Picker("Upscaling", selection: $settings.upscale) {
                    ForEach(AppSettings.Upscale.allCases) { u in
                        Text(u.label).tag(u)
                    }
                }
                Picker("Retro effect", selection: $settings.videoEffect) {
                    ForEach(AppSettings.VideoEffect.allCases) { e in
                        Text(e.label).tag(e)
                    }
                }
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

// ---- Controls (keyboard + controller remapping) ----

private struct ControlsSettingsView: View {
    private enum Mode: String, CaseIterable, Identifiable { case keyboard, controller; var id: String { rawValue } }
    @State private var mode: Mode = .keyboard

    var body: some View {
        VStack(spacing: 0) {
            Picker("", selection: $mode) {
                Text("Keyboard").tag(Mode.keyboard)
                Text("Controller").tag(Mode.controller)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .padding([.horizontal, .top], 14)
            .padding(.bottom, 6)

            if mode == .keyboard {
                KeyboardBindingsView()
            } else {
                ControllerBindingsView()
            }
        }
    }
}

// ---- keyboard ----

private struct KeyboardBindingsView: View {
    @EnvironmentObject var settings: AppSettings
    @State private var capturing: Btn?
    @State private var monitor: Any?

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(spacing: 6) {
                    ForEach(Btn.bindOrder) { btn in
                        BindRow(name: btn.label,
                                value: capturing == btn ? "Press a key…" : (settings.binding(for: btn)?.label ?? "—"),
                                active: capturing == btn) { beginCapture(btn) }
                    }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 6)
            }
            Divider()
            HStack {
                Text("Click a binding, then press a key (Esc cancels).")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Reset") { cancelCapture(); settings.resetBindings() }
            }
            .padding(12)
        }
        .onDisappear(perform: cancelCapture)
    }

    private func beginCapture(_ btn: Btn) {
        cancelCapture()
        capturing = btn
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .flagsChanged]) { ev in
            handleCapture(ev); return nil
        }
    }
    private func handleCapture(_ ev: NSEvent) {
        guard let btn = capturing else { return }
        switch ev.type {
        case .keyDown:
            if ev.keyCode == 53 { cancelCapture(); return }
            settings.setBinding(.key(ev.keyCode), for: btn); cancelCapture()
        case .flagsChanged:
            for m in KeyBind.Modifier.allCases where ev.modifierFlags.contains(m.flag) {
                settings.setBinding(.modifier(m), for: btn); cancelCapture(); return
            }
        default: break
        }
    }
    private func cancelCapture() {
        capturing = nil
        if let m = monitor { NSEvent.removeMonitor(m); monitor = nil }
    }
}

// ---- controller (live diagram + chip bindings) ----

private struct ControllerBindingsView: View {
    @EnvironmentObject var settings: AppSettings
    @State private var capturing: Btn?
    @State private var pressed: Set<PadInput> = []
    @State private var poll: Timer?
    @State private var connected = false

    var body: some View {
        VStack(spacing: 0) {
            if !connected {
                emptyState("Connect a controller and press any button.\nPS5 DualSense, Xbox, or any USB/Bluetooth pad works.")
            } else {
                ScrollView {
                    VStack(spacing: 12) {
                        PadDiagram(pressed: pressed)
                            .padding(.horizontal, 14)
                            .padding(.top, 8)
                        VStack(spacing: 6) {
                            ForEach(Btn.bindOrder) { btn in
                                BindRow(name: btn.label,
                                        value: capturing == btn ? "Press a button…" : (settings.padBinding(for: btn)?.glyph ?? "—"),
                                        active: capturing == btn,
                                        highlight: isLit(btn)) { beginCapture(btn) }
                            }
                        }
                        .padding(.horizontal, 14)
                    }
                    .padding(.bottom, 8)
                }
            }
            Divider()
            HStack {
                Text(connected ? "Click a binding, then press a controller button. Left stick = D-pad."
                               : "No controller connected.")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Reset") { capturing = nil; settings.resetPadBindings() }
                    .disabled(!connected)
            }
            .padding(12)
        }
        .onAppear(perform: startPolling)
        .onDisappear { poll?.invalidate(); poll = nil }
    }

    private func isLit(_ btn: Btn) -> Bool {
        if let b = settings.padBinding(for: btn) { return pressed.contains(b) }
        return false
    }

    private func startPolling() {
        poll?.invalidate()
        poll = Timer.scheduledTimer(withTimeInterval: 1.0 / 30.0, repeats: true) { _ in
            guard let pad = firstGamepad else {
                if connected { connected = false }
                if !pressed.isEmpty { pressed = [] }
                return
            }
            if !connected { connected = true }
            let now = Set(PadInput.allCases.filter { $0.isPressed(pad) })
            if now != pressed {
                // While capturing, bind the first newly-pressed input.
                if let btn = capturing, let hit = now.subtracting(pressed).first {
                    settings.setPadBinding(hit, for: btn)
                    capturing = nil
                }
                pressed = now
            }
        }
    }

    private func beginCapture(_ btn: Btn) { capturing = (capturing == btn) ? nil : btn }
}

/// One binding row: name on the left, a clickable chip on the right.
private struct BindRow: View {
    let name: String
    let value: String
    var active = false
    var highlight = false
    let action: () -> Void

    var body: some View {
        HStack {
            Text(name).font(.system(size: 12))
            Spacer()
            Button(action: action) {
                Text(value)
                    .font(.system(size: 12, weight: .medium))
                    .frame(minWidth: 96)
                    .padding(.vertical, 5)
                    .background(active ? Color.accentColor.opacity(0.85)
                                       : (highlight ? Color.green.opacity(0.7) : Color.gray.opacity(0.18)))
                    .foregroundColor(active || highlight ? .white : .primary)
                    .clipShape(RoundedRectangle(cornerRadius: 7))
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 3)
        .background(Color.primary.opacity(0.04))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }
}

/// A live PS-style controller diagram; elements light up as buttons are pressed.
private struct PadDiagram: View {
    let pressed: Set<PadInput>
    private func on(_ i: PadInput) -> Bool { pressed.contains(i) }

    var body: some View {
        HStack(alignment: .top, spacing: 18) {
            // Left: triggers + d-pad.
            VStack(spacing: 14) {
                HStack(spacing: 6) { pill("L2", on(.l2)); pill("L1", on(.l1)) }
                dpad
            }
            Spacer(minLength: 0)
            VStack(spacing: 8) {
                HStack(spacing: 6) { pill("Share", on(.options)); pill("Menu", on(.menu)) }
            }
            .padding(.top, 18)
            Spacer(minLength: 0)
            // Right: triggers + face diamond.
            VStack(spacing: 14) {
                HStack(spacing: 6) { pill("R1", on(.r1)); pill("R2", on(.r2)) }
                diamond
            }
        }
        .padding(.vertical, 16)
        .padding(.horizontal, 18)
        .background(Color.black.opacity(0.35))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(.white.opacity(0.08)))
    }

    private var dpad: some View {
        let s: CGFloat = 26
        return ZStack {
            VStack(spacing: 2) {
                arrow("arrowtriangle.up.fill", on(.dpadUp))
                HStack(spacing: 2) {
                    arrow("arrowtriangle.left.fill", on(.dpadLeft))
                    Color.clear.frame(width: s, height: s)
                    arrow("arrowtriangle.right.fill", on(.dpadRight))
                }
                arrow("arrowtriangle.down.fill", on(.dpadDown))
            }
        }
    }

    private var diamond: some View {
        VStack(spacing: 2) {
            face("△", .green, on(.y))
            HStack(spacing: 2) {
                face("□", .pink, on(.x))
                Color.clear.frame(width: 30, height: 30)
                face("◯", .red, on(.b))
            }
            face("✕", .blue, on(.a))
        }
    }

    private func face(_ sym: String, _ color: Color, _ lit: Bool) -> some View {
        Text(sym)
            .font(.system(size: 14, weight: .bold))
            .frame(width: 30, height: 30)
            .background(Circle().fill(lit ? color : color.opacity(0.22)))
            .foregroundColor(lit ? .white : color)
    }
    private func arrow(_ sym: String, _ lit: Bool) -> some View {
        Image(systemName: sym)
            .font(.system(size: 11))
            .frame(width: 26, height: 26)
            .background(RoundedRectangle(cornerRadius: 5).fill(lit ? Color.accentColor : Color.gray.opacity(0.25)))
            .foregroundColor(lit ? .white : .secondary)
    }
    private func pill(_ label: String, _ lit: Bool) -> some View {
        Text(label)
            .font(.system(size: 10, weight: .semibold))
            .padding(.horizontal, 9).padding(.vertical, 5)
            .background(Capsule().fill(lit ? Color.accentColor : Color.gray.opacity(0.22)))
            .foregroundColor(lit ? .white : .secondary)
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
