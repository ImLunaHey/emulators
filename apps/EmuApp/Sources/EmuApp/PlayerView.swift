import SwiftUI

/// The "Player" window: the live game screen plus a thin status bar. The render
/// loop in `EmuHub` writes frames straight into the screen layer, so this view
/// only reacts to play/stop and metadata changes.
struct PlayerView: View {
    @EnvironmentObject var hub: EmuHub

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            if hub.isPlaying {
                ScreenView(hub: hub)
                    .ignoresSafeArea()
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

            if hub.isPlaying {
                VStack {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(hub.title)
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundColor(.white)
                            Text("\(hub.systemLabel) · \(hub.controllerInfo)")
                                .font(.system(size: 10))
                                .foregroundColor(.white.opacity(0.6))
                        }
                        Spacer()
                        Button("Stop") { hub.stop() }
                            .buttonStyle(.plain)
                            .foregroundColor(.white.opacity(0.8))
                            .padding(.horizontal, 10).padding(.vertical, 4)
                            .background(.white.opacity(0.12))
                            .cornerRadius(6)
                    }
                    .padding(10)
                    .background(.black.opacity(0.45))
                    Spacer()
                }
            }
        }
        .frame(minWidth: 480, minHeight: 360)
    }
}
