using System.Runtime.InteropServices;

namespace EmuWin;

/// <summary>P/Invoke bindings to the Rust core's C ABI (emu_native.dll).</summary>
internal static class EmuNative
{
    private const string Dll = "emu_native";

    [DllImport(Dll)] public static extern IntPtr emu_new(uint system);
    [DllImport(Dll)] public static extern void emu_free(IntPtr emu);

    [DllImport(Dll)]
    [return: MarshalAs(UnmanagedType.U1)]
    public static extern bool emu_load_rom(IntPtr emu, byte[] data, UIntPtr len);

    [DllImport(Dll)]
    [return: MarshalAs(UnmanagedType.U1)]
    public static extern bool emu_load_bios(IntPtr emu, byte[] data, UIntPtr len);

    [DllImport(Dll)] public static extern void emu_run_frame(IntPtr emu);
    [DllImport(Dll)] public static extern void emu_set_buttons(IntPtr emu, uint logical);

    [DllImport(Dll)] public static extern IntPtr emu_framebuffer_ptr(IntPtr emu);
    [DllImport(Dll)] public static extern UIntPtr emu_framebuffer_len(IntPtr emu);
    [DllImport(Dll)] public static extern uint emu_width(IntPtr emu);
    [DllImport(Dll)] public static extern uint emu_height(IntPtr emu);

    [DllImport(Dll)] public static extern UIntPtr emu_drain_audio(IntPtr emu, float[] outBuf, UIntPtr max);
    [DllImport(Dll)] public static extern uint emu_sample_rate(IntPtr emu);
    [DllImport(Dll)] public static extern uint emu_channels(IntPtr emu);

    // Saves.
    [DllImport(Dll)] public static extern uint emu_save_kind(uint system);
    [DllImport(Dll)] public static extern UIntPtr emu_save_data_len(IntPtr emu);
    [DllImport(Dll)] public static extern UIntPtr emu_save_data(IntPtr emu, byte[] outBuf, UIntPtr max);
    [DllImport(Dll)] public static extern void emu_load_save(IntPtr emu, byte[] data, UIntPtr len);

    [DllImport(Dll)]
    [return: MarshalAs(UnmanagedType.U1)]
    public static extern bool emu_save_dirty(IntPtr emu);

    [DllImport(Dll)] public static extern void emu_clear_save_dirty(IntPtr emu);
}

/// <summary>Systems, matching the Rust <c>System</c> enum / EMU_SYSTEM_*.</summary>
internal enum EmuSystem : uint
{
    Gba = 0, Ps1 = 1, Nds = 2, Nes = 3, Sms = 4, GameGear = 5, Gbc = 6, Xbox = 7,
    Snes = 8, Genesis = 9, Pce = 10, Atari2600 = 11, Ngpc = 12, WonderSwan = 13,
    VirtualBoy = 14, N64 = 15,
}

/// <summary>Logical button bits (EMU_BTN_*) for <c>emu_set_buttons</c>.</summary>
[Flags]
internal enum Btn : uint
{
    Up = 1 << 0, Down = 1 << 1, Left = 1 << 2, Right = 1 << 3,
    South = 1 << 4, East = 1 << 5, West = 1 << 6, North = 1 << 7,
    L1 = 1 << 8, R1 = 1 << 9, L2 = 1 << 10, R2 = 1 << 11,
    Start = 1 << 12, Select = 1 << 13,
}

internal static class Systems
{
    /// <summary>Pick a system from a file extension (+ Xbox disc sniff).</summary>
    public static EmuSystem? Detect(string ext, byte[] data)
    {
        // Xbox disc magic at sector 32 overrides an ambiguous .iso.
        var magic = "MICROSOFT*XBOX*MEDIA"u8.ToArray();
        if (data.Length >= 0x10000 + magic.Length &&
            data.AsSpan(0x10000, magic.Length).SequenceEqual(magic))
            return EmuSystem.Xbox;

        return ext.ToLowerInvariant() switch
        {
            "gba" => EmuSystem.Gba,
            "nds" => EmuSystem.Nds,
            "nes" => EmuSystem.Nes,
            "sms" => EmuSystem.Sms,
            "gg" => EmuSystem.GameGear,
            "gb" or "gbc" => EmuSystem.Gbc,
            "smc" or "sfc" => EmuSystem.Snes,
            "md" or "gen" or "smd" => EmuSystem.Genesis,
            "pce" => EmuSystem.Pce,
            "a26" => EmuSystem.Atari2600,
            "ngc" or "ngp" => EmuSystem.Ngpc,
            "ws" or "wsc" => EmuSystem.WonderSwan,
            "vb" or "vboy" => EmuSystem.VirtualBoy,
            "n64" or "z64" or "v64" => EmuSystem.N64,
            "xbe" or "xiso" => EmuSystem.Xbox,
            "cue" or "bin" or "img" or "iso" or "pbp" => EmuSystem.Ps1,
            _ => null,
        };
    }

    public static string Label(EmuSystem s) => s switch
    {
        EmuSystem.Gba => "GBA", EmuSystem.Ps1 => "PS1", EmuSystem.Nds => "NDS",
        EmuSystem.Nes => "NES", EmuSystem.Sms => "SMS", EmuSystem.GameGear => "GameGear",
        EmuSystem.Gbc => "GBC", EmuSystem.Xbox => "Xbox", EmuSystem.Snes => "SNES",
        EmuSystem.Genesis => "Genesis", EmuSystem.Pce => "PCE",
        EmuSystem.Atari2600 => "Atari2600", EmuSystem.Ngpc => "NGPC",
        EmuSystem.WonderSwan => "WonderSwan", EmuSystem.VirtualBoy => "VirtualBoy",
        EmuSystem.N64 => "N64", _ => "Unknown",
    };
}
