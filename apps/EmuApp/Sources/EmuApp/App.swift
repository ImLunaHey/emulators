import AppKit
import SwiftUI

/// Forces the app to be a regular, foreground GUI app (a bare SwiftPM executable
/// otherwise launches without a Dock icon / window focus).
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}

/// macOS front-end for the emulator. Two windows: a console-first library and a
/// game player, both sharing one `EmuHub` (the running session + render loop).
@main
struct EmuAppMain: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var hub = EmuHub()
    @StateObject private var library = Library()

    var body: some Scene {
        Window("Consoles", id: "library") {
            LibraryView()
                .environmentObject(hub)
                .environmentObject(library)
        }
        .defaultSize(width: 920, height: 640)

        Window("Player", id: "player") {
            PlayerView()
                .environmentObject(hub)
        }
        .defaultSize(width: 768, height: 576)
    }
}
