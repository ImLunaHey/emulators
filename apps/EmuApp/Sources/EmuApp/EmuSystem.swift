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
        [.gba, .nds, .gbc, .nes, .sms, .gameGear, .ps1, .xbox]
    }
}

/// Logical, controller-agnostic buttons. Per-system bit layouts are derived from
/// these so keyboard and gamepad share one path.
enum Btn: CaseIterable {
    case up, down, left, right
    case south, east, west, north // face buttons by physical position
    case l1, r1, l2, r2
    case start, select
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
        }
    }
}
