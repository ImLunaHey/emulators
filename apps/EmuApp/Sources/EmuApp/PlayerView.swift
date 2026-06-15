import SwiftUI

/// The "Player" window: the live game screen plus a thin status bar. The render
/// loop in `EmuHub` writes frames straight into the screen layer, so this view
/// only reacts to play/stop and metadata changes.
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
