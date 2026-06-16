import Foundation

/// The systems the unified native core supports. Raw values MUST match the
/// `EmuSystem` enum in emu_native.h / the Rust `System` enum.
enum EmuSystem: UInt32, CaseIterable, Identifiable {
    case gba = 0
    case ps1 = 1
    case nds = 2
    case nes = 3
    case sms = 4
    case gameGear = 5
    case gbc = 6
    case xbox = 7
    case snes = 8
    case genesis = 9
    case pce = 10
    case atari2600 = 11
    case ngpc = 12
    case wonderswan = 13
    case virtualboy = 14
    case n64 = 15

    var id: UInt32 { rawValue }

    var label: String {
        switch self {
        case .gba: return "GBA"
        case .ps1: return "PS1"
        case .nds: return "NDS"
        case .nes: return "NES"
        case .sms: return "SMS"
        case .gameGear: return "Game Gear"
        case .gbc: return "GBC"
        case .xbox: return "Xbox"
        case .snes: return "SNES"
        case .genesis: return "Genesis"
        case .pce: return "PC Engine"
        case .atari2600: return "Atari 2600"
        case .ngpc: return "NGPC"
        case .wonderswan: return "WonderSwan"
        case .virtualboy: return "Virtual Boy"
        case .n64: return "N64"
        }
    }

    var fullName: String {
        switch self {
        case .gba: return "Game Boy Advance"
        case .ps1: return "PlayStation"
        case .nds: return "Nintendo DS"
        case .nes: return "Nintendo Entertainment System"
        case .sms: return "Sega Master System"
        case .gameGear: return "Sega Game Gear"
        case .gbc: return "Game Boy Color"
        case .xbox: return "Microsoft Xbox"
        case .snes: return "Super Nintendo"
        case .genesis: return "Sega Genesis / Mega Drive"
        case .pce: return "PC Engine / TurboGrafx-16"
        case .atari2600: return "Atari 2600 (VCS)"
        case .ngpc: return "Neo Geo Pocket Color"
        case .wonderswan: return "Bandai WonderSwan Color"
        case .virtualboy: return "Nintendo Virtual Boy"
        case .n64: return "Nintendo 64"
        }
    }

    /// Signature accent color (hex), mirroring the web launcher's palette.
    var accentHex: String {
        switch self {
        case .gba: return "#7c5cff"
        case .ps1: return "#c9c9d4"
        case .nds: return "#e0e0e6"
        case .nes: return "#e4000f"
        case .sms: return "#e07b1f"
        case .gameGear: return "#1f7ae0"
        case .gbc: return "#ff5fa2"
        case .xbox: return "#9cd530"
        case .snes: return "#8b7fd4"
        case .genesis: return "#1a6dd6"
        case .pce: return "#f2a900"
        case .atari2600: return "#b8531f"
        case .ngpc: return "#2bb7c4"
        case .wonderswan: return "#3aa856"
        case .virtualboy: return "#d4233b"
        case .n64: return "#2e9e4f"
        }
    }

    /// SF Symbol used as the tile's console artwork (a representative icon — see
    /// note in `ConsoleTile`; swap in real product photos by dropping PNGs into
    /// the asset catalog and returning their name here).
    var symbol: String {
        switch self {
        case .gba: return "gamecontroller.fill"
        case .ps1: return "gamecontroller.fill"
        case .nds: return "rectangle.split.1x2.fill"
        case .nes: return "tv.fill"
        case .sms: return "tv.fill"
        case .gameGear: return "gamecontroller"
        case .gbc: return "gamecontroller.fill"
        case .xbox: return "gamecontroller.fill"
        case .snes: return "gamecontroller.fill"
        case .genesis: return "tv.fill"
        case .pce: return "tv.fill"
        case .atari2600: return "gamecontroller.fill"
        case .ngpc: return "gamecontroller"
        case .wonderswan: return "rectangle.portrait.fill"
        case .virtualboy: return "eyes.inverse"
        case .n64: return "gamecontroller.fill"
        }
    }

    /// Cores that need a BIOS/flash image to do anything useful.
    var needsBios: Bool { self == .ps1 || self == .xbox }

    /// File extensions that map to this system (lowercase, no dot).
    var extensions: [String] {
        switch self {
        case .gba: return ["gba"]
        case .ps1: return ["cue", "bin", "img", "iso", "pbp"]
        case .nds: return ["nds"]
        case .nes: return ["nes"]
        case .sms: return ["sms"]
        case .gameGear: return ["gg"]
        case .gbc: return ["gb", "gbc"]
        case .xbox: return ["xbe", "xiso"]
        case .snes: return ["smc", "sfc"]
        case .genesis: return ["md", "gen", "smd"]
        case .pce: return ["pce"]
        case .atari2600: return ["a26"]
        case .ngpc: return ["ngc", "ngp"]
        case .wonderswan: return ["ws", "wsc"]
        case .virtualboy: return ["vb", "vboy"]
        case .n64: return ["n64", "z64", "v64"]
        }
    }

    /// Best-effort detection from a filename. `.iso` is ambiguous (PS1 vs Xbox);
    /// the caller should also sniff the bytes (see `EmuSystem.sniff`).
    static func detect(filename: String) -> EmuSystem? {
        let lower = filename.lowercased()
        // ".xiso.iso" redump naming -> Xbox.
        if lower.hasSuffix(".xiso.iso") || lower.hasSuffix(".xbe") || lower.hasSuffix(".xiso") {
            return .xbox
        }
        guard let ext = lower.split(separator: ".").last.map(String.init) else { return nil }
        for sys in EmuSystem.allCases where sys.extensions.contains(ext) {
            return sys
        }
        return nil
    }

    /// Disambiguate disc images by content: an Xbox disc carries the XDVDFS magic
    /// "MICROSOFT*XBOX*MEDIA" at sector 32 (offset 0x10000).
    static func sniff(_ data: Data, fallback: EmuSystem?) -> EmuSystem? {
        let magic = Array("MICROSOFT*XBOX*MEDIA".utf8)
        let off = 0x10000
        if data.count >= off + magic.count {
            let slice = data.subdata(in: off..<(off + magic.count))
            if Array(slice) == magic { return .xbox }
        }
        return fallback
    }

    /// All systems in canonical display order for the console grid.
    static var displayOrder: [EmuSystem] {
        [.gba, .nds, .gbc, .snes, .nes, .n64, .genesis, .sms, .gameGear,
         .pce, .ps1, .xbox, .atari2600, .ngpc, .wonderswan, .virtualboy]
    }
}

/// Logical, controller-agnostic buttons. Per-system bit layouts are derived from
/// these so keyboard and gamepad share one path. The String raw value is a
/// stable key for persisting custom key bindings.
enum Btn: String, CaseIterable, Identifiable {
    case up, down, left, right
    case south, east, west, north // face buttons by physical position
    case l1, r1, l2, r2
    case start, select

    var id: String { rawValue }

    /// Human label for the controls UI. Face buttons use the standard
    /// position→letter abstraction (Nintendo / libretro RetroPad): right = A,
    /// bottom = B, top = X, left = Y. The per-system bit layout translates these
    /// to each console's actual buttons.
    var label: String {
        switch self {
        case .up: return "Up"
        case .down: return "Down"
        case .left: return "Left"
        case .right: return "Right"
        case .east: return "A"   // right face
        case .south: return "B"  // bottom face
        case .north: return "X"  // top face
        case .west: return "Y"   // left face
        case .l1: return "L"
        case .r1: return "R"
        case .l2: return "L2"
        case .r2: return "R2"
        case .start: return "Start"
        case .select: return "Select"
        }
    }

    /// Display order for the controls list (d-pad, face, shoulders, system).
    static var bindOrder: [Btn] {
        [.up, .down, .left, .right, .south, .east, .west, .north,
         .l1, .r1, .l2, .r2, .start, .select]
    }
}

extension EmuSystem {
    /// Build the active-high "pressed" bitmask the unified FFI expects for this
    /// system, from a set of logical buttons. Layouts mirror the web players.
    func keyMask(_ pressed: Set<Btn>) -> UInt32 {
        func bit(_ b: Btn, _ shift: UInt32) -> UInt32 { pressed.contains(b) ? (1 << shift) : 0 }
        switch self {
        case .gba, .nds:
            // GBA KEYINPUT order (NDS shares it; FFI handles NDS active-low + ext).
            var m: UInt32 = 0
            m |= bit(.east, 0)   // A
            m |= bit(.south, 1)  // B
            m |= bit(.select, 2)
            m |= bit(.start, 3)
            m |= bit(.right, 4)
            m |= bit(.left, 5)
            m |= bit(.up, 6)
            m |= bit(.down, 7)
            m |= bit(.r1, 8)     // R
            m |= bit(.l1, 9)     // L
            if self == .nds {
                m |= bit(.north, 10) // X
                m |= bit(.west, 11)  // Y
            }
            return m
        case .nes:
            var m: UInt32 = 0
            m |= bit(.east, 0)   // A
            m |= bit(.south, 1)  // B
            m |= bit(.select, 2)
            m |= bit(.start, 3)
            m |= bit(.up, 4)
            m |= bit(.down, 5)
            m |= bit(.left, 6)
            m |= bit(.right, 7)
            return m
        case .gbc:
            var m: UInt32 = 0
            m |= bit(.east, 0)
            m |= bit(.south, 1)
            m |= bit(.select, 2)
            m |= bit(.start, 3)
            m |= bit(.right, 4)
            m |= bit(.left, 5)
            m |= bit(.up, 6)
            m |= bit(.down, 7)
            return m
        case .sms, .gameGear:
            var m: UInt32 = 0
            m |= bit(.up, 0)
            m |= bit(.down, 1)
            m |= bit(.left, 2)
            m |= bit(.right, 3)
            m |= bit(.south, 4) // button 1
            m |= bit(.east, 5)  // button 2
            m |= bit(.start, 6)
            return m
        case .ps1:
            var m: UInt32 = 0
            m |= bit(.select, 0)
            m |= bit(.start, 3)
            m |= bit(.up, 4)
            m |= bit(.right, 5)
            m |= bit(.down, 6)
            m |= bit(.left, 7)
            m |= bit(.l1, 10)
            m |= bit(.r1, 11)
            m |= bit(.north, 12) // Triangle
            m |= bit(.east, 13)  // Circle
            m |= bit(.south, 14) // Cross
            m |= bit(.west, 15)  // Square
            return m
        case .xbox:
            var m: UInt32 = 0
            m |= bit(.start, 0)
            m |= bit(.select, 1) // Back
            m |= bit(.up, 2)
            m |= bit(.down, 3)
            m |= bit(.left, 4)
            m |= bit(.right, 5)
            m |= bit(.south, 6) // A
            m |= bit(.east, 7)  // B
            m |= bit(.west, 8)  // X
            m |= bit(.north, 9) // Y
            m |= bit(.l1, 10)   // White
            m |= bit(.r1, 11)   // Black
            m |= bit(.l2, 12)   // Left trigger
            m |= bit(.r2, 13)   // Right trigger
            return m
        case .snes:
            // B Y Select Start Up Down Left Right A X L R
            var m: UInt32 = 0
            m |= bit(.south, 0)  // B
            m |= bit(.west, 1)   // Y
            m |= bit(.select, 2)
            m |= bit(.start, 3)
            m |= bit(.up, 4)
            m |= bit(.down, 5)
            m |= bit(.left, 6)
            m |= bit(.right, 7)
            m |= bit(.east, 8)   // A
            m |= bit(.north, 9)  // X
            m |= bit(.l1, 10)
            m |= bit(.r1, 11)
            return m
        case .genesis:
            // Up Down Left Right A B C Start (+ X Y Z Mode for 6-button)
            var m: UInt32 = 0
            m |= bit(.up, 0)
            m |= bit(.down, 1)
            m |= bit(.left, 2)
            m |= bit(.right, 3)
            m |= bit(.west, 4)   // A
            m |= bit(.south, 5)  // B
            m |= bit(.east, 6)   // C
            m |= bit(.start, 7)
            m |= bit(.l1, 8)     // X
            m |= bit(.north, 9)  // Y
            m |= bit(.r1, 10)    // Z
            m |= bit(.select, 11) // Mode
            return m
        case .pce:
            // Up Down Left Right I II Select Run
            var m: UInt32 = 0
            m |= bit(.up, 0)
            m |= bit(.down, 1)
            m |= bit(.left, 2)
            m |= bit(.right, 3)
            m |= bit(.east, 4)   // I
            m |= bit(.south, 5)  // II
            m |= bit(.select, 6)
            m |= bit(.start, 7)  // Run
            return m
        case .atari2600:
            // Joystick + fire, plus console switches.
            var m: UInt32 = 0
            m |= bit(.up, 0)
            m |= bit(.down, 1)
            m |= bit(.left, 2)
            m |= bit(.right, 3)
            m |= bit(.south, 4)  // Fire
            m |= bit(.start, 5)  // Reset
            m |= bit(.select, 6) // Select
            return m
        case .ngpc:
            // D-pad + A B + Option.
            var m: UInt32 = 0
            m |= bit(.up, 0)
            m |= bit(.down, 1)
            m |= bit(.left, 2)
            m |= bit(.right, 3)
            m |= bit(.south, 4)  // A
            m |= bit(.east, 5)   // B
            m |= bit(.start, 6)  // Option
            return m
        case .wonderswan:
            // X-pad (used as the d-pad) + A B + Start.
            var m: UInt32 = 0
            m |= bit(.up, 0)
            m |= bit(.down, 1)
            m |= bit(.left, 2)
            m |= bit(.right, 3)
            m |= bit(.east, 4)   // A
            m |= bit(.south, 5)  // B
            m |= bit(.start, 6)
            return m
        case .virtualboy:
            // Right d-pad + A B + L/R triggers + Start/Select.
            var m: UInt32 = 0
            m |= bit(.up, 0)
            m |= bit(.down, 1)
            m |= bit(.left, 2)
            m |= bit(.right, 3)
            m |= bit(.east, 4)   // A
            m |= bit(.south, 5)  // B
            m |= bit(.l1, 6)
            m |= bit(.r1, 7)
            m |= bit(.start, 8)
            m |= bit(.select, 9)
            return m
        case .n64:
            // A B Z Start, d-pad, L R, C-buttons (mapped to face north/west).
            var m: UInt32 = 0
            m |= bit(.east, 0)   // A
            m |= bit(.south, 1)  // B
            m |= bit(.l2, 2)     // Z
            m |= bit(.start, 3)
            m |= bit(.up, 4)
            m |= bit(.down, 5)
            m |= bit(.left, 6)
            m |= bit(.right, 7)
            m |= bit(.l1, 8)     // L
            m |= bit(.r1, 9)     // R
            return m
        }
    }
}
