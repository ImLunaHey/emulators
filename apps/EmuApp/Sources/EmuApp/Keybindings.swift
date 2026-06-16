import AppKit
import GameController

/// A bound keyboard input: either a hardware key (captured from a keyDown, by
/// `keyCode`) or a modifier key (captured from flagsChanged). Modifiers are
/// supported so the default Select = Shift keeps working and stays rebindable.
enum KeyBind: Equatable, Codable {
    case key(UInt16)
    case modifier(Modifier)

    enum Modifier: String, Codable, CaseIterable {
        case shift, control, option, command

        var flag: NSEvent.ModifierFlags {
            switch self {
            case .shift: return .shift
            case .control: return .control
            case .option: return .option
            case .command: return .command
            }
        }
        var label: String {
            switch self {
            case .shift: return "⇧ Shift"
            case .control: return "⌃ Control"
            case .option: return "⌥ Option"
            case .command: return "⌘ Command"
            }
        }
    }

    /// A short label for the controls UI.
    var label: String {
        switch self {
        case .key(let code): return KeyNames.label(for: code)
        case .modifier(let m): return m.label
        }
    }
}

/// Default keyboard layout (mirrors the app's original hard-coded mapping):
/// arrows + Z/X/A/S face, Q/W shoulders, D/F triggers, Return = Start,
/// Shift = Select.
enum DefaultBindings {
    static let map: [Btn: KeyBind] = [
        .up: .key(126), .down: .key(125), .left: .key(123), .right: .key(124),
        .south: .key(6), .east: .key(7), .west: .key(0), .north: .key(1), // Z X A S
        .l1: .key(12), .r1: .key(13), .l2: .key(2), .r2: .key(3),          // Q W D F
        .start: .key(36),                                                  // Return
        .select: .modifier(.shift),
    ]
}

/// Friendly names for macOS virtual key codes, for the controls UI.
enum KeyNames {
    static func label(for code: UInt16) -> String {
        if let named = special[code] { return named }
        if let ch = letters[code] { return ch }
        return "Key \(code)"
    }

    private static let special: [UInt16: String] = [
        36: "Return", 48: "Tab", 49: "Space", 51: "Delete", 53: "Esc",
        76: "Enter", 117: "Fwd Del",
        123: "←", 124: "→", 125: "↓", 126: "↑",
        // number row
        18: "1", 19: "2", 20: "3", 21: "4", 23: "5",
        22: "6", 26: "7", 28: "8", 25: "9", 29: "0",
        27: "-", 24: "=", 33: "[", 30: "]", 42: "\\",
        41: ";", 39: "'", 43: ",", 47: ".", 44: "/", 50: "`",
    ]

    private static let letters: [UInt16: String] = [
        0: "A", 1: "S", 2: "D", 3: "F", 4: "H", 5: "G", 6: "Z", 7: "X",
        8: "C", 9: "V", 11: "B", 12: "Q", 13: "W", 14: "E", 15: "R",
        16: "Y", 17: "T", 31: "O", 32: "U", 34: "I", 35: "P", 37: "L",
        38: "J", 40: "K", 45: "N", 46: "M",
    ]
}

// ---- game controller bindings ----

/// A bindable controller input (button, d-pad direction, or trigger). Raw values
/// persist the binding; `isPressed` reads it from a live gamepad.
enum PadInput: String, Codable, CaseIterable, Identifiable {
    case a, b, x, y
    case dpadUp, dpadDown, dpadLeft, dpadRight
    case l1, r1, l2, r2
    case menu, options

    var id: String { rawValue }

    var label: String {
        switch self {
        case .a: return "Cross (A)"
        case .b: return "Circle (B)"
        case .x: return "Square (X)"
        case .y: return "Triangle (Y)"
        case .dpadUp: return "D-Pad Up"
        case .dpadDown: return "D-Pad Down"
        case .dpadLeft: return "D-Pad Left"
        case .dpadRight: return "D-Pad Right"
        case .l1: return "L1"
        case .r1: return "R1"
        case .l2: return "L2 / ZL"
        case .r2: return "R2 / ZR"
        case .menu: return "Menu (Options)"
        case .options: return "Create (Share)"
        }
    }

    func isPressed(_ p: GCExtendedGamepad, threshold: Float = 0.3) -> Bool {
        switch self {
        case .a: return p.buttonA.isPressed
        case .b: return p.buttonB.isPressed
        case .x: return p.buttonX.isPressed
        case .y: return p.buttonY.isPressed
        case .dpadUp: return p.dpad.up.isPressed
        case .dpadDown: return p.dpad.down.isPressed
        case .dpadLeft: return p.dpad.left.isPressed
        case .dpadRight: return p.dpad.right.isPressed
        case .l1: return p.leftShoulder.isPressed
        case .r1: return p.rightShoulder.isPressed
        case .l2: return p.leftTrigger.value > threshold
        case .r2: return p.rightTrigger.value > threshold
        case .menu: return p.buttonMenu.isPressed
        case .options: return p.buttonOptions?.isPressed == true
        }
    }
}

/// Default controller layout (DualSense-style), matching the original hard-coded
/// mapping.
enum DefaultPadBindings {
    static let map: [Btn: PadInput] = [
        .south: .a, .east: .b, .west: .x, .north: .y,
        .up: .dpadUp, .down: .dpadDown, .left: .dpadLeft, .right: .dpadRight,
        .l1: .l1, .r1: .r1, .l2: .l2, .r2: .r2,
        .start: .menu, .select: .options,
    ]
}

/// The first connected extended gamepad, if any.
var firstGamepad: GCExtendedGamepad? {
    GCController.controllers().compactMap { $0.extendedGamepad }.first
}
