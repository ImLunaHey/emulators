import CEmuNative
import Foundation

/// A link peripheral on a core's serial port. Raw values match `EmuAttachment`
/// in emu_native.h / the Rust `Attachment` enum.
enum Attachment: UInt32, CaseIterable, Identifiable {
    case none = 0
    case linkCable = 1
    case wirelessAdapter = 2

    var id: UInt32 { rawValue }

    var label: String {
        switch self {
        case .none: return "None"
        case .linkCable: return "Link Cable"
        case .wirelessAdapter: return "Wireless Adapter"
        }
    }

    var symbol: String {
        switch self {
        case .none: return "cable.connector.slash"
        case .linkCable: return "cable.connector"
        case .wirelessAdapter: return "wifi"
        }
    }

    var detail: String {
        switch self {
        case .none: return "Nothing plugged into the link port."
        case .linkCable: return "Serial link cable for local multiplayer / trading."
        case .wirelessAdapter: return "GBA Wireless Adapter for Download Play and wireless multiplayer."
        }
    }
}

/// On-disk save category for a system. Raw values match `EmuSaveKind`.
enum SaveKind: UInt32 {
    case none = 0
    case battery = 1
    case memoryCard = 2
    case hdd = 3

    /// The native file extension for this category (nil = nothing to manage).
    var fileExtension: String? {
        switch self {
        case .none: return nil
        case .battery: return "sav"
        case .memoryCard: return "mcr"
        case .hdd: return "img"
        }
    }

    var label: String {
        switch self {
        case .none: return "No saves"
        case .battery: return "Battery Save"
        case .memoryCard: return "Memory Card"
        case .hdd: return "Hard Disk"
        }
    }

    var symbol: String {
        switch self {
        case .none: return "nosign"
        case .battery: return "memorychip"
        case .memoryCard: return "sdcard"
        case .hdd: return "internaldrive"
        }
    }
}

extension EmuSystem {
    /// Link attachments this system supports (always includes `.none`). Derived
    /// from the core's `emu_supported_attachments` bitmask (`1 << kind`).
    var supportedAttachments: [Attachment] {
        let mask = emu_supported_attachments(rawValue)
        return Attachment.allCases.filter { $0 == .none || (mask & (1 << $0.rawValue)) != 0 }
    }

    /// True if this system models any link attachment beyond "none".
    var hasAttachments: Bool { emu_supported_attachments(rawValue) != 0 }

    /// On-disk save category for this system.
    var saveKind: SaveKind { SaveKind(rawValue: emu_save_kind(rawValue)) ?? .none }
}
