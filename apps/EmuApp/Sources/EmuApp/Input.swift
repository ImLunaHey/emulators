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

    /// Active controller bindings (logical button → gamepad input). Replaced by
    /// `EmuHub` from settings; defaults until then.
    var padBindings: [Btn: PadInput] = DefaultPadBindings.map

    private func pollController() -> Set<Btn> {
        guard let pad = firstGamepad else { return [] }
        var s: Set<Btn> = []
        for (btn, input) in padBindings where input.isPressed(pad) { s.insert(btn) }
        // The left analog stick always drives the d-pad directions, regardless of
        // the button bindings.
        let t: Float = 0.3
        if pad.leftThumbstick.up.value > t { s.insert(.up) }
        if pad.leftThumbstick.down.value > t { s.insert(.down) }
        if pad.leftThumbstick.left.value > t { s.insert(.left) }
        if pad.leftThumbstick.right.value > t { s.insert(.right) }
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
