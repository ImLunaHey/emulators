import AppKit
import GameController

/// Merges keyboard + game-controller state into a set of logical [`Btn`]s.
/// The keyboard set is mutated by NSEvent handlers (see `EmuHub`); the controller
/// is polled each frame from the GameController framework (DualSense supported
/// natively).
final class InputManager {
    /// Logical buttons currently held on the keyboard.
    private var keyboard: Set<Btn> = []

    /// Active keyboard bindings (logical button → key). Replaced by `EmuHub`
    /// from the user's settings; defaults until then.
    var bindings: [Btn: KeyBind] = DefaultBindings.map {
        didSet { keyboard.removeAll() } // avoid stuck keys after a rebind
    }

    // ---- keyboard ----

    /// Which logical button (if any) a key-code is bound to.
    private func button(forKeyCode code: UInt16) -> Btn? {
        bindings.first { $0.value == .key(code) }?.key
    }

    func handleKey(_ event: NSEvent, down: Bool) {
        guard let b = button(forKeyCode: event.keyCode) else { return }
        if down { keyboard.insert(b) } else { keyboard.remove(b) }
    }

    /// Modifier-key bindings (e.g. the default Select = Shift) arrive via
    /// flagsChanged, not keyDown. Re-evaluate every modifier binding against the
    /// current flags.
    func handleFlags(_ event: NSEvent) {
        for (btn, bind) in bindings {
            guard case .modifier(let m) = bind else { continue }
            if event.modifierFlags.contains(m.flag) {
                keyboard.insert(btn)
            } else {
                keyboard.remove(btn)
            }
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
