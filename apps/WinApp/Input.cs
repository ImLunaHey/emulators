using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Windows.Input;

namespace EmuWin;

/// <summary>
/// Merges keyboard + XInput controller state into a logical <see cref="Btn"/>
/// mask. Defaults mirror the macOS/web apps: arrows, Z=B, X=A, A=Y, S=X, Q=L,
/// W=R, Enter=Start, Shift=Select; controllers use Cross→A / Circle→B.
/// </summary>
internal sealed class InputManager
{
    private readonly HashSet<Key> _keys = new();

    public void KeyDown(Key k) => _keys.Add(k);
    public void KeyUp(Key k) => _keys.Remove(k);
    public void Clear() => _keys.Clear();

    public uint Mask()
    {
        uint m = 0;
        void K(Key key, Btn b) { if (_keys.Contains(key)) m |= (uint)b; }
        K(Key.Up, Btn.Up); K(Key.Down, Btn.Down); K(Key.Left, Btn.Left); K(Key.Right, Btn.Right);
        K(Key.X, Btn.East); K(Key.Z, Btn.South); K(Key.S, Btn.North); K(Key.A, Btn.West);
        K(Key.Q, Btn.L1); K(Key.W, Btn.R1); K(Key.D, Btn.L2); K(Key.F, Btn.R2);
        K(Key.Enter, Btn.Start);
        if (_keys.Contains(Key.LeftShift) || _keys.Contains(Key.RightShift)) m |= (uint)Btn.Select;
        m |= Gamepad();
        return m;
    }

    // ---- XInput (Xbox controllers; PS controllers also enumerate via XInput on
    // Windows through driver shims / Steam) ----

    [StructLayout(LayoutKind.Sequential)]
    private struct XGamepad
    {
        public ushort wButtons;
        public byte bLeftTrigger, bRightTrigger;
        public short sThumbLX, sThumbLY, sThumbRX, sThumbRY;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct XState { public uint dwPacketNumber; public XGamepad Gamepad; }

    [DllImport("xinput1_4.dll", EntryPoint = "XInputGetState")]
    private static extern uint XInputGetState(uint index, out XState state);

    private const ushort DPAD_UP = 0x0001, DPAD_DOWN = 0x0002, DPAD_LEFT = 0x0004,
        DPAD_RIGHT = 0x0008, START = 0x0010, BACK = 0x0020, LSHOULDER = 0x0100,
        RSHOULDER = 0x0200, A = 0x1000, B = 0x2000, X = 0x4000, Y = 0x8000;
    private const short STICK_DEADZONE = 12000;
    private const byte TRIGGER_THRESHOLD = 64;

    private uint Gamepad()
    {
        for (uint i = 0; i < 4; i++)
        {
            if (XInputGetState(i, out var s) != 0) continue; // 0 = ERROR_SUCCESS
            var g = s.Gamepad;
            uint m = 0;
            void Bit(ushort flag, Btn b) { if ((g.wButtons & flag) != 0) m |= (uint)b; }
            Bit(DPAD_UP, Btn.Up); Bit(DPAD_DOWN, Btn.Down);
            Bit(DPAD_LEFT, Btn.Left); Bit(DPAD_RIGHT, Btn.Right);
            Bit(A, Btn.East);   // Xbox A (bottom / Cross) → emulator A
            Bit(B, Btn.South);  // Xbox B (right / Circle) → emulator B
            Bit(X, Btn.West);   // Xbox X (left / Square) → Y
            Bit(Y, Btn.North);  // Xbox Y (top / Triangle) → X
            Bit(LSHOULDER, Btn.L1); Bit(RSHOULDER, Btn.R1);
            Bit(START, Btn.Start); Bit(BACK, Btn.Select);
            if (g.bLeftTrigger > TRIGGER_THRESHOLD) m |= (uint)Btn.L2;
            if (g.bRightTrigger > TRIGGER_THRESHOLD) m |= (uint)Btn.R2;
            // Left stick → d-pad.
            if (g.sThumbLY > STICK_DEADZONE) m |= (uint)Btn.Up;
            if (g.sThumbLY < -STICK_DEADZONE) m |= (uint)Btn.Down;
            if (g.sThumbLX < -STICK_DEADZONE) m |= (uint)Btn.Left;
            if (g.sThumbLX > STICK_DEADZONE) m |= (uint)Btn.Right;
            return m; // first connected pad
        }
        return 0;
    }
}
