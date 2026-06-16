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

            ZStack(alignment: .topTrailing) {
                Color.black
                if hub.isPlaying {
                    ScreenView(hub: hub)
                    FpsHUD(fps: hub.fps, history: hub.fpsHistory)
                        .padding(8)
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

/// A small translucent overlay showing the current FPS as a number plus a
/// rolling sparkline graph (green ≥55, yellow ≥30, red below), with a 60 fps
/// reference line.
struct FpsHUD: View {
    let fps: Double
    let history: [Double]

    private func color(_ v: Double) -> Color {
        v >= 55 ? .green : (v >= 30 ? .yellow : .red)
    }

    var body: some View {
        VStack(alignment: .trailing, spacing: 4) {
            Text(String(format: "%.0f FPS", fps))
                .font(.system(size: 13, weight: .bold, design: .monospaced))
                .foregroundColor(color(fps))
            FpsGraph(history: history)
                .frame(width: 120, height: 36)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(.black.opacity(0.5))
        .cornerRadius(8)
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(.white.opacity(0.12), lineWidth: 1))
    }
}

/// Sparkline of recent FPS, auto-scaled to max(60, peak), with a 60 fps line.
private struct FpsGraph: View {
    let history: [Double]

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width
            let h = geo.size.height
            let peak = max(60.0, history.max() ?? 60.0)
            let yFor: (Double) -> CGFloat = { v in h - CGFloat(min(v, peak) / peak) * h }
            ZStack {
                // 60 fps reference line.
                Path { p in
                    let y = yFor(60)
                    p.move(to: CGPoint(x: 0, y: y))
                    p.addLine(to: CGPoint(x: w, y: y))
                }
                .stroke(.white.opacity(0.25), style: StrokeStyle(lineWidth: 1, dash: [3, 3]))

                // The FPS trace.
                if history.count > 1 {
                    Path { p in
                        let n = history.count
                        let dx = w / CGFloat(max(n - 1, 1))
                        for (i, v) in history.enumerated() {
                            let pt = CGPoint(x: CGFloat(i) * dx, y: yFor(v))
                            if i == 0 { p.move(to: pt) } else { p.addLine(to: pt) }
                        }
                    }
                    .stroke(
                        (history.last ?? 0) >= 55 ? Color.green
                            : ((history.last ?? 0) >= 30 ? Color.yellow : Color.red),
                        style: StrokeStyle(lineWidth: 1.5, lineJoin: .round)
                    )
                }
            }
        }
    }
}
