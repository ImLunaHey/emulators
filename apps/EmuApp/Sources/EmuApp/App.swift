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

/// Startup configuration parsed once from CLI args + environment. When
/// `playerOnly` is set the app skips the library window entirely and opens
/// straight into a single player for `request` — e.g.
///
///   swift run EmuApp --player "/path/to/game.iso" [--system 7] [--bios bios.bin]
///
/// (env equivalents: EMU_PLAYER_ONLY=1, EMU_AUTOLOAD, EMU_SYSTEM, EMU_BIOS.)
struct LaunchConfig {
    let playerOnly: Bool
    let request: LaunchRequest?
    let biosPath: String?

    static let current = parse()

    private static func value(_ args: [String], after flag: String) -> String? {
        guard let i = args.firstIndex(of: flag), i + 1 < args.count else { return nil }
        let next = args[i + 1]
        return next.hasPrefix("--") ? nil : next
    }

    private static func parse() -> LaunchConfig {
        let args = CommandLine.arguments
        let env = ProcessInfo.processInfo.environment

        var path = value(args, after: "--player") ?? value(args, after: "--rom") ?? env["EMU_AUTOLOAD"]
        let wantsPlayer = args.contains("--player") || env["EMU_PLAYER_ONLY"] != nil
        let biosPath = value(args, after: "--bios") ?? env["EMU_BIOS"]

        guard wantsPlayer, let p = path, FileManager.default.fileExists(atPath: p) else {
            return LaunchConfig(playerOnly: false, request: nil, biosPath: biosPath)
        }
        path = p
        let url = URL(fileURLWithPath: p)

        // System: explicit --system / EMU_SYSTEM, else detect by name + sniff.
        let system: EmuSystem
        if let s = value(args, after: "--system") ?? env["EMU_SYSTEM"],
           let raw = UInt32(s), let sys = EmuSystem(rawValue: raw) {
            system = sys
        } else {
            let declared = EmuSystem.detect(filename: url.lastPathComponent)
            if let head = try? Data(contentsOf: url, options: .mappedIfSafe) {
                system = EmuSystem.sniff(head, fallback: declared) ?? declared ?? .gba
            } else {
                system = declared ?? .gba
            }
        }
        let title = url.deletingPathExtension().lastPathComponent
        return LaunchConfig(
            playerOnly: true,
            request: LaunchRequest(system: system, path: p, title: title),
            biosPath: biosPath)
    }
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
        // Primary window: the console library, OR — in player-only mode
        // (--player <path>) — a single player for the requested game. The
        // mode switch is a View-level `if` (SceneBuilder can't do if/else).
        WindowGroup {
            Group {
                if LaunchConfig.current.playerOnly {
                    PlayerWindow(request: LaunchConfig.current.request)
                } else {
                    LibraryView()
                }
            }
            .environmentObject(library)
            .environmentObject(bios)
        }
        .defaultSize(width: 920, height: 640)

        // Additional players: each launch from the library opens its own window
        // here (several games / emulators at once). Unused in player-only mode.
        WindowGroup(for: LaunchRequest.self) { $request in
            PlayerWindow(request: request)
                .environmentObject(bios)
        }
        .defaultSize(width: 768, height: 576)
    }
}
