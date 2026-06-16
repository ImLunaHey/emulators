// swift-tools-version:5.9
import PackageDescription

// macOS native front-end for the emulator. Links the unified `emu-native` static
// archive (all cores) via a thin C target, and presents a two-window SwiftUI app
// (a console library + a game player) with keyboard and PS5/DualSense controller
// input. Build the static lib first:  ../../scripts/build-macos.sh
let package = Package(
    name: "EmuApp",
    platforms: [.macOS(.v13)],
    targets: [
        // C shim exposing the emu_native.h header as a Swift-importable module.
        .target(name: "CEmuNative"),
        .executableTarget(
            name: "EmuApp",
            dependencies: ["CEmuNative"],
            linkerSettings: [
                // The Rust static archive (built by scripts/build-macos.sh).
                .unsafeFlags([
                    "-L../../packages/native/target/release",
                    "-lemu_native",
                ]),
                .linkedFramework("AppKit"),
                .linkedFramework("Metal"),
                .linkedFramework("MetalKit"),
                .linkedFramework("MetalFX"),
                .linkedFramework("QuartzCore"),
                .linkedFramework("CoreGraphics"),
                .linkedFramework("AVFoundation"),
                .linkedFramework("GameController"),
            ]
        ),
    ]
)
