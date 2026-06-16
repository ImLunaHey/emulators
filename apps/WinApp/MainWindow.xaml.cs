using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using Microsoft.Win32;

namespace EmuWin;

public partial class MainWindow : Window
{
    private IntPtr _emu = IntPtr.Zero;
    private EmuSystem _system;
    private string _gameName = "";
    private string? _romPath;

    private WriteableBitmap? _bmp;
    private int _bmpW, _bmpH;
    private byte[] _fb = Array.Empty<byte>();

    private AudioOut? _audio;
    private readonly float[] _audioBuf = new float[16384];

    private readonly InputManager _input = new();
    private readonly Stopwatch _clock = Stopwatch.StartNew();
    private double _acc;
    private int _saveClock;
    private bool _paused;
    private bool _fullscreen;

    private List<Recent> _recents = new();

    public MainWindow()
    {
        InitializeComponent();
        _recents = LoadRecents();
        RefreshRecents();
        CompositionTarget.Rendering += OnRender;
        PreviewKeyDown += OnPreviewKeyDown;
        PreviewKeyUp += OnPreviewKeyUp;
        Closing += (_, _) => StopCore();
    }

    // ---- game loop ----

    private void OnRender(object? sender, EventArgs e)
    {
        double dt = _clock.Elapsed.TotalSeconds;
        _clock.Restart();
        if (_emu == IntPtr.Zero || _paused) return;
        if (dt > 0.1) dt = 0.1;
        _acc += dt;

        const double frameTime = 1.0 / 60.0;
        uint buttons = _input.Mask();
        int ran = 0;
        while (_acc >= frameTime && ran < 4)
        {
            _acc -= frameTime;
            ran++;
            EmuNative.emu_set_buttons(_emu, buttons);
            EmuNative.emu_run_frame(_emu);
            DrainAudio();
        }
        if (ran == 0) return;

        RenderFrame();
        _saveClock += ran;
        if (_saveClock >= 300) { _saveClock = 0; FlushSave(); }
    }

    private void DrainAudio()
    {
        if (_audio == null) return;
        int n = (int)EmuNative.emu_drain_audio(_emu, _audioBuf, (UIntPtr)_audioBuf.Length);
        if (n > 0) _audio.Push(_audioBuf, n);
    }

    private void RenderFrame()
    {
        uint w = EmuNative.emu_width(_emu), h = EmuNative.emu_height(_emu);
        if (w == 0 || h == 0) return;
        int len = (int)(w * h * 4);
        IntPtr ptr = EmuNative.emu_framebuffer_ptr(_emu);
        if (ptr == IntPtr.Zero || (int)EmuNative.emu_framebuffer_len(_emu) != len) return;

        if (_fb.Length < len) _fb = new byte[len];
        Marshal.Copy(ptr, _fb, 0, len);
        // RGBA (core) -> BGRA (WPF) by swapping R and B.
        for (int i = 0; i < len; i += 4) { (_fb[i], _fb[i + 2]) = (_fb[i + 2], _fb[i]); }

        if (_bmp == null || _bmpW != (int)w || _bmpH != (int)h)
        {
            _bmp = new WriteableBitmap((int)w, (int)h, 96, 96, PixelFormats.Bgra32, null);
            _bmpW = (int)w; _bmpH = (int)h;
            Screen.Source = _bmp;
        }
        _bmp.WritePixels(new Int32Rect(0, 0, (int)w, (int)h), _fb, (int)w * 4, 0);
    }

    // ---- loading ----

    private void OpenRom_Click(object sender, RoutedEventArgs e)
    {
        var dlg = new OpenFileDialog
        {
            Filter =
                "ROMs / discs|*.gba;*.nds;*.nes;*.sms;*.gg;*.gb;*.gbc;*.smc;*.sfc;*.md;*.gen;" +
                "*.smd;*.pce;*.a26;*.ngc;*.ngp;*.ws;*.wsc;*.vb;*.vboy;*.n64;*.z64;*.v64;" +
                "*.cue;*.bin;*.img;*.iso;*.pbp;*.xbe;*.xiso|All files|*.*",
        };
        if (dlg.ShowDialog() == true) LoadPath(dlg.FileName);
    }

    private void LoadPath(string path)
    {
        byte[] bytes;
        try { bytes = File.ReadAllBytes(path); }
        catch { TitleText.Text = $"Couldn't read {Path.GetFileName(path)}"; return; }

        string name = Path.GetFileNameWithoutExtension(path);
        string ext = Path.GetExtension(path).TrimStart('.');
        var detected = Systems.Detect(ext, bytes);
        if (detected is not EmuSystem sys) { TitleText.Text = $"Unknown system for {name}"; return; }

        StopCore();
        _emu = EmuNative.emu_new((uint)sys);
        if (_emu == IntPtr.Zero) { TitleText.Text = "Failed to create core"; return; }
        EmuNative.emu_load_rom(_emu, bytes, (UIntPtr)bytes.Length);

        string sp = SavePath(sys, name);
        if (File.Exists(sp))
        {
            try { var sav = File.ReadAllBytes(sp); EmuNative.emu_load_save(_emu, sav, (UIntPtr)sav.Length); }
            catch { /* ignore a bad save */ }
        }

        _system = sys; _gameName = name; _romPath = path;
        _paused = false; _saveClock = 0; _acc = 0;
        _audio = new AudioOut((int)EmuNative.emu_sample_rate(_emu), (int)EmuNative.emu_channels(_emu))
        {
            Volume = (float)VolumeSlider.Value,
        };

        TitleText.Text = $"{Systems.Label(sys)} · {name}";
        SetPlayingUi(true);
        AddRecent(path, name);
    }

    private void SetPlayingUi(bool playing)
    {
        var vis = playing ? Visibility.Visible : Visibility.Collapsed;
        PauseBtn.Visibility = ResetBtn.Visibility = StopBtn.Visibility = vis;
        OpenBtn.Visibility = playing ? Visibility.Collapsed : Visibility.Visible;
        HomePanel.Visibility = playing ? Visibility.Collapsed : Visibility.Visible;
        if (!playing) { Screen.Source = null; _bmp = null; TitleText.Text = ""; }
    }

    private void StopCore()
    {
        if (_emu != IntPtr.Zero)
        {
            FlushSave();
            EmuNative.emu_free(_emu);
            _emu = IntPtr.Zero;
        }
        _audio?.Dispose();
        _audio = null;
        _input.Clear();
    }

    // ---- toolbar ----

    private void Stop_Click(object sender, RoutedEventArgs e) { StopCore(); SetPlayingUi(false); }

    private void Pause_Click(object sender, RoutedEventArgs e) => TogglePause();

    private void TogglePause()
    {
        _paused = !_paused;
        PauseBtn.Content = _paused ? "Resume" : "Pause";
        if (!_paused) _clock.Restart();
    }

    private void Reset_Click(object sender, RoutedEventArgs e)
    {
        if (_romPath is string p) LoadPath(p);
    }

    private void Volume_Changed(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (_audio != null) _audio.Volume = (float)e.NewValue;
    }

    private void ToggleFullscreen()
    {
        _fullscreen = !_fullscreen;
        if (_fullscreen)
        {
            TopBar.Visibility = Visibility.Collapsed;
            WindowStyle = WindowStyle.None;
            ResizeMode = ResizeMode.NoResize;
            WindowState = WindowState.Maximized;
        }
        else
        {
            TopBar.Visibility = Visibility.Visible;
            WindowStyle = WindowStyle.SingleBorderWindow;
            ResizeMode = ResizeMode.CanResize;
            WindowState = WindowState.Normal;
        }
    }

    // ---- input ----

    private void OnPreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.F11) { ToggleFullscreen(); e.Handled = true; return; }
        if (_emu == IntPtr.Zero) return;
        if (e.Key == Key.P) { TogglePause(); e.Handled = true; return; }
        _input.KeyDown(e.Key);
        e.Handled = true;
    }

    private void OnPreviewKeyUp(object sender, KeyEventArgs e)
    {
        if (_emu == IntPtr.Zero) return;
        _input.KeyUp(e.Key);
        e.Handled = true;
    }

    // ---- saves ----

    private void FlushSave()
    {
        if (_emu == IntPtr.Zero || !EmuNative.emu_save_dirty(_emu)) return;
        int len = (int)EmuNative.emu_save_data_len(_emu);
        if (len == 0) return;
        var buf = new byte[len];
        int n = (int)EmuNative.emu_save_data(_emu, buf, (UIntPtr)len);
        try
        {
            string sp = SavePath(_system, _gameName);
            Directory.CreateDirectory(Path.GetDirectoryName(sp)!);
            File.WriteAllBytes(sp, n == len ? buf : buf[..n]);
            EmuNative.emu_clear_save_dirty(_emu);
        }
        catch { /* disk error: keep dirty, retry next interval */ }
    }

    private static string DataDir =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
            "imlunahey-emulator");

    private static string SavePath(EmuSystem sys, string game)
    {
        string safe = string.Concat(game.Select(c => "/\\:?\"<>|".Contains(c) ? '_' : c));
        return Path.Combine(DataDir, "saves", Systems.Label(sys), safe + ".sav");
    }

    // ---- recents ----

    public sealed class Recent
    {
        public string Path { get; set; } = "";
        public string Name { get; set; } = "";
    }

    private static string RecentsPath => Path.Combine(DataDir, "recents.json");

    private static List<Recent> LoadRecents()
    {
        try
        {
            if (File.Exists(RecentsPath))
                return JsonSerializer.Deserialize<List<Recent>>(File.ReadAllText(RecentsPath)) ?? new();
        }
        catch { /* ignore */ }
        return new();
    }

    private void SaveRecents()
    {
        try
        {
            Directory.CreateDirectory(DataDir);
            File.WriteAllText(RecentsPath, JsonSerializer.Serialize(_recents));
        }
        catch { /* ignore */ }
    }

    private void AddRecent(string path, string name)
    {
        _recents.RemoveAll(r => r.Path == path);
        _recents.Insert(0, new Recent { Path = path, Name = name });
        if (_recents.Count > 12) _recents = _recents.Take(12).ToList();
        SaveRecents();
        RefreshRecents();
    }

    private void RefreshRecents()
    {
        RecentsList.ItemsSource = null;
        RecentsList.ItemsSource = _recents;
        RecentsList.DisplayMemberPath = nameof(Recent.Name);
    }

    private void Recent_DoubleClick(object sender, MouseButtonEventArgs e)
    {
        if (RecentsList.SelectedItem is Recent r) LoadPath(r.Path);
    }
}
