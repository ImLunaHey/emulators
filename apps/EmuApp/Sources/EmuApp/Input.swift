import AppKit
import GameController

/// Merges keyboard + game-controller state into a set of logical [`Btn`]s.
/// The keyboard set is mutated by NSEvent handlers (see `EmuHub`); the controller
/// is polled each frame from the GameController framework (DualSense supported
/// natively).
final class InputManager {
    /// Logical buttons currently held on the keyboard.
    private var keyboard: Set<Btn> = []

    // ---- keyboard ----

    /// Map a key event to a logical button. Returns nil for unmapped keys.
    private static func button(for event: NSEvent) -> Btn? {
        switch event.keyCode {
        case 126: return .up
        case 125: return .down
        case 123: return .left
        case 124: return .right
        case 36, 76: return .start // return / keypad-enter
        default: break
        }
        switch event.charactersIgnoringModifiers?.lowercased() {
        case "z": return .south
        case "x": return .east
        case "a": return .west
        case "s": return .north
        case "q": return .l1
        case "w": return .r1
        case "d": return .l2
        case "f": return .r2
        default: return nil
        }
    }

    func handleKey(_ event: NSEvent, down: Bool) {
        guard let b = Self.button(for: event) else { return }
        if down { keyboard.insert(b) } else { keyboard.remove(b) }
    }

    /// Shift is a modifier, so it arrives via flagsChanged rather than keyDown.
    func handleFlags(_ event: NSEvent) {
        if event.modifierFlags.contains(.shift) {
            keyboard.insert(.select)
        } else {
            keyboard.remove(.select)
        }
    }

    func clear() { keyboard.removeAll() }

    // ---- controller ----

    private func pollController() -> Set<Btn> {
        guard let pad = GCController.controllers().compactMap({ $0.extendedGamepad }).first else {
            return []
        }
        var s: Set<Btn> = []
        let t: Float = 0.3
        if pad.dpad.up.isPressed || pad.leftThumbstick.up.value > t { s.insert(.up) }
        if pad.dpad.down.isPressed || pad.leftThumbstick.down.value > t { s.insert(.down) }
        if pad.dpad.left.isPressed || pad.leftThumbstick.left.value > t { s.insert(.left) }
        if pad.dpad.right.isPressed || pad.leftThumbstick.right.value > t { s.insert(.right) }
        if pad.buttonA.isPressed { s.insert(.south) }  // Cross
        if pad.buttonB.isPressed { s.insert(.east) }   // Circle
        if pad.buttonX.isPressed { s.insert(.west) }   // Square
        if pad.buttonY.isPressed { s.insert(.north) }  // Triangle
        if pad.leftShoulder.isPressed { s.insert(.l1) }
        if pad.rightShoulder.isPressed { s.insert(.r1) }
        if pad.leftTrigger.value > t { s.insert(.l2) }
        if pad.rightTrigger.value > t { s.insert(.r2) }
        if pad.buttonMenu.isPressed { s.insert(.start) }      // Options
        if pad.buttonOptions?.isPressed == true { s.insert(.select) } // Create/Share
        return s
    }

    /// All logical buttons currently active (keyboard ∪ controller).
    func currentButtons() -> Set<Btn> {
        keyboard.union(pollController())
    }

    /// Whether any DualSense / extended controller is connected.
    var controllerConnected: Bool {
        GCController.controllers().contains { $0.extendedGamepad != nil }
    }

    var controllerName: String? {
        GCController.controllers().first { $0.extendedGamepad != nil }?.vendorName
    }
}
