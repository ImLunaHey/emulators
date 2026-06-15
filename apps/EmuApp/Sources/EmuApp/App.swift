import AppKit
import SwiftUI

/// A request to launch a game in its own player window. Carries the ROM by path
/// (small + Codable) so the player window loads the bytes itself; `uid` makes
/// every request unique so opening the same game twice yields a SECOND window
/// (multiple copies / multiple emulators can run at once).
struct LaunchRequest: Codable, Hashable, Identifiable {
    var id: String { uid }
    let systemRaw: UInt32
    let path: String
    let title: String
    let uid: String

    init(system: EmuSystem, path: String, title: String) {
        self.systemRaw = system.rawValue
        self.path = path
        self.title = title
        self.uid = UUID().uuidString
    }

    var system: EmuSystem { EmuSystem(rawValue: systemRaw) ?? .gba }
}

/// Shared, in-memory BIOS/flash images per system (PS1, Xbox). Lives at the app
/// level so the single library window and every player window see the same set.
final class BiosStore: ObservableObject {
    @Published private(set) var images: [EmuSystem: Data] = [:]
    func set(_ data: Data, for system: EmuSystem) { images[system] = data }
    func has(_ system: EmuSystem) -> Bool { images[system] != nil }
    func data(for system: EmuSystem) -> Data? { images[system] }
}

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

/// macOS front-end for the emulator. One console-first library window, plus a
/// player WindowGroup: each launch opens its OWN player window with its OWN
/// `EmuHub` (render loop + session), so several games / emulators run at once.
@main
struct EmuAppMain: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var library = Library()
    @StateObject private var bios = BiosStore()

    var body: some Scene {
        Window("Consoles", id: "library") {
            LibraryView()
                .environmentObject(library)
                .environmentObject(bios)
        }
        .defaultSize(width: 920, height: 640)

        // One window per LaunchRequest. Distinct `uid`s mean the same game can be
        // opened in multiple simultaneous windows.
        WindowGroup(for: LaunchRequest.self) { $request in
            PlayerWindow(request: request)
                .environmentObject(bios)
        }
        .defaultSize(width: 768, height: 576)
    }
}
