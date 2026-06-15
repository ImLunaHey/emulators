import AppKit
import SwiftUI

/// Hosts one game in its own window: owns a private `EmuHub` (render loop +
/// session) and loads the ROM named by the `LaunchRequest`. Input is gated on
/// window focus inside the hub, so the keyboard/controller only drives the
/// frontmost game when several are open at once.
struct PlayerWindow: View {
    let request: LaunchRequest?
    @EnvironmentObject var bios: BiosStore
    @StateObject private var hub = EmuHub()
    @State private var launched = false

    var body: some View {
        PlayerView()
            .environmentObject(hub)
            .onAppear { launchIfNeeded() }
            .onDisappear { hub.stop() }
    }

    private func launchIfNeeded() {
        guard !launched, let request else { return }
        launched = true
        let url = URL(fileURLWithPath: request.path)
        guard let data = try? Data(contentsOf: url, options: .mappedIfSafe) else {
            hub.title = "Couldn't read \(url.lastPathComponent)"
            return
        }
        // Prefer an already-loaded BIOS; otherwise a --bios/EMU_BIOS path.
        if let b = bios.data(for: request.system) {
            hub.setBios(b, for: request.system)
        } else if let bp = LaunchConfig.current.biosPath,
                  let bd = try? Data(contentsOf: URL(fileURLWithPath: bp)) {
            bios.set(bd, for: request.system)
            hub.setBios(bd, for: request.system)
        }
        hub.launch(system: request.system, rom: data, title: request.title)
    }
}

/// The game screen plus a thin status bar. The render loop in `EmuHub` writes
/// frames straight into the screen layer, so this view only reacts to play/stop
/// and metadata changes.
struct PlayerView: View {
    @EnvironmentObject var hub: EmuHub

    var body: some View {
        VStack(spacing: 0) {
            if hub.isPlaying {
                // A real top bar (not an overlay) so it never covers the screen.
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(hub.title)
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundColor(.white)
                            .lineLimit(1)
                        Text("\(hub.systemLabel) · \(hub.controllerInfo)")
                            .font(.system(size: 10))
                            .foregroundColor(.white.opacity(0.6))
                    }
                    Spacer()
                    Button("Stop") { hub.stop() }
                        .buttonStyle(.plain)
                        .foregroundColor(.white.opacity(0.85))
                        .padding(.horizontal, 10).padding(.vertical, 4)
                        .background(.white.opacity(0.12))
                        .cornerRadius(6)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(Color(red: 0.08, green: 0.08, blue: 0.1))
            }

            ZStack {
                Color.black
                if hub.isPlaying {
                    ScreenView(hub: hub)
                } else {
                    VStack(spacing: 10) {
                        Text("No game running")
                            .font(.system(size: 18, weight: .semibold))
                            .foregroundColor(.white.opacity(0.8))
                        Text("Launch one from the Consoles window.")
                            .font(.system(size: 12))
                            .foregroundColor(.white.opacity(0.5))
                    }
                }
            }
        }
        .frame(minWidth: 480, minHeight: 360)
        .background(Color.black)
    }
}
