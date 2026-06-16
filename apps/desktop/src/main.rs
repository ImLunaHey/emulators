//! Cross-platform desktop front-end (Windows / macOS / Linux) for every core —
//! the portable counterpart to the macOS-only SwiftUI app in apps/EmuApp.
//!
//! Pure Rust: links `emu-native` directly (no FFI), renders + UIs with egui,
//! plays audio through cpal, and reads gamepads via gilrs. Covers: open a ROM,
//! run, video (filter / integer scale / scanlines), audio (with volume),
//! keyboard + gamepad input, persisted settings, and per-game `.sav` saves.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui;
use emu_native::{
    Emu, System, BTN_DOWN, BTN_EAST, BTN_L1, BTN_L2, BTN_LEFT, BTN_NORTH, BTN_R1, BTN_R2, BTN_RIGHT,
    BTN_SELECT, BTN_SOUTH, BTN_START, BTN_UP, BTN_WEST,
};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([480.0, 360.0])
            .with_title("imlunahey emulator"),
        ..Default::default()
    };
    eframe::run_native(
        "imlunahey emulator",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

/// Persisted app settings (saved via eframe's storage).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    /// Linear (smooth) vs nearest (sharp) scaling.
    smooth: bool,
    /// Snap the picture to an integer multiple of its native size.
    integer_scale: bool,
    /// Draw CRT-style scanlines over the picture.
    scanlines: bool,
    /// Output volume, 0..1.
    volume: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            smooth: false,
            integer_scale: false,
            scanlines: false,
            volume: 1.0,
        }
    }
}

struct App {
    emu: Option<Emu>,
    /// (system, game name) of the running game — for the save path.
    current: Option<(System, String)>,
    title: String,
    tex: Option<egui::TextureHandle>,
    audio: Option<AudioOut>,
    gilrs: Option<gilrs::Gilrs>,
    settings: Settings,
    settings_open: bool,
    save_clock: u32,
    last: Instant,
    acc: f32,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings = cc
            .storage
            .and_then(|s| eframe::get_value::<Settings>(s, "settings"))
            .unwrap_or_default();
        Self {
            emu: None,
            current: None,
            title: String::new(),
            tex: None,
            audio: None,
            gilrs: gilrs::Gilrs::new().ok(),
            settings,
            settings_open: false,
            save_clock: 0,
            last: Instant::now(),
            acc: 0.0,
        }
    }

    fn open_rom(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "ROMs / discs",
                &[
                    "gba", "nds", "nes", "sms", "gg", "gb", "gbc", "smc", "sfc", "md", "gen", "smd",
                    "pce", "a26", "ngc", "ngp", "ws", "wsc", "vb", "vboy", "n64", "z64", "v64",
                    "cue", "bin", "img", "iso", "pbp", "xbe", "xiso",
                ],
            )
            .pick_file()
        else {
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("game")
            .to_string();
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        let Some(system) = detect_system(ext.as_deref(), &bytes) else {
            self.title = format!("Unknown system for {name}");
            return;
        };
        let mut emu = Emu::new(system);
        emu.load_rom(&bytes);
        // Restore a previous .sav if one exists.
        if let Some(p) = save_path(system, &name) {
            if let Ok(save) = std::fs::read(&p) {
                emu.load_save(&save);
            }
        }
        self.audio = AudioOut::new();
        self.title = name.clone();
        self.current = Some((system, name));
        self.emu = Some(emu);
        self.acc = 0.0;
        self.save_clock = 0;
        self.last = Instant::now();
    }

    /// Write the running game's save to disk if it changed.
    fn flush_save(&mut self) {
        let (Some(emu), Some((system, name))) = (self.emu.as_mut(), self.current.as_ref()) else {
            return;
        };
        if !emu.save_dirty() {
            return;
        }
        let data = emu.save_data();
        if data.is_empty() {
            return;
        }
        if let Some(p) = save_path(*system, name) {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if std::fs::write(&p, &data).is_ok() {
                emu.clear_save_dirty();
            }
        }
    }

    fn stop(&mut self) {
        self.flush_save();
        self.emu = None;
        self.current = None;
        self.audio = None;
        self.tex = None;
        self.title.clear();
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "settings", &self.settings);
        // Also a good moment to flush the battery save.
        self.flush_save();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(g) = &mut self.gilrs {
            while g.next_event().is_some() {}
        }
        let gamepad_mask = self.gilrs.as_ref().map(read_gamepad).unwrap_or(0);

        if self.emu.is_some() {
            let now = Instant::now();
            let dt = (now - self.last).as_secs_f32().min(0.1);
            self.last = now;
            self.acc += dt;
            let frame_time = 1.0 / 60.0;
            let buttons = read_keyboard(ctx) | gamepad_mask;
            let volume = self.settings.volume;
            let mut ran = 0;
            while self.acc >= frame_time && ran < 4 {
                self.acc -= frame_time;
                ran += 1;
                let emu = self.emu.as_mut().unwrap();
                emu.set_buttons(buttons);
                emu.run_frame();
                if let Some(audio) = &mut self.audio {
                    audio.volume = volume;
                    let mut tmp = [0f32; 4096];
                    let n = emu.drain_audio(&mut tmp);
                    if n > 0 {
                        audio.push(&tmp[..n], emu.sample_rate(), emu.channels() as usize);
                    }
                }
            }
            // Autosave the battery roughly every 5 s when it changed.
            self.save_clock += ran;
            if self.save_clock >= 300 {
                self.save_clock = 0;
                self.flush_save();
            }
            // Upload the latest frame.
            let emu = self.emu.as_ref().unwrap();
            let (w, h) = (emu.width() as usize, emu.height() as usize);
            let fb = emu.framebuffer();
            if w > 0 && h > 0 && fb.len() == w * h * 4 {
                let img = egui::ColorImage::from_rgba_unmultiplied([w, h], fb);
                let opt = if self.settings.smooth {
                    egui::TextureOptions::LINEAR
                } else {
                    egui::TextureOptions::NEAREST
                };
                match &mut self.tex {
                    Some(t) => t.set(img, opt),
                    None => self.tex = Some(ctx.load_texture("framebuffer", img, opt)),
                }
            }
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.emu.is_some() {
                    ui.strong(&self.title);
                } else if ui.button("Open ROM…").clicked() {
                    self.open_rom();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.emu.is_some() && ui.button("Stop").clicked() {
                        self.stop();
                    }
                    ui.toggle_value(&mut self.settings_open, "⚙ Settings");
                });
            });
        });

        if self.settings_open {
            egui::SidePanel::right("settings")
                .resizable(false)
                .default_width(220.0)
                .show(ctx, |ui| {
                    ui.heading("Settings");
                    ui.separator();
                    ui.label("Video");
                    ui.checkbox(&mut self.settings.smooth, "Smooth (bilinear)");
                    ui.checkbox(&mut self.settings.integer_scale, "Integer scale");
                    ui.checkbox(&mut self.settings.scanlines, "Scanlines");
                    ui.add_space(8.0);
                    ui.label("Audio");
                    ui.add(egui::Slider::new(&mut self.settings.volume, 0.0..=1.0).text("Volume"));
                    ui.add_space(12.0);
                    ui.separator();
                    ui.label(
                        egui::RichText::new(
                            "Keyboard: arrows + Z/X (B/A), A/S (Y/X), Q/W (L/R), \
                             Enter=Start, Shift=Select. Gamepads auto-detected.",
                        )
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                if let Some(tex) = &self.tex {
                    let avail = ui.available_size();
                    let [tw, th] = tex.size();
                    let (tw, th) = (tw as f32, th as f32);
                    let mut scale = (avail.x / tw).min(avail.y / th).max(0.0);
                    if self.settings.integer_scale && scale >= 1.0 {
                        scale = scale.floor();
                    }
                    let size = egui::vec2(tw * scale, th * scale);
                    let rect = egui::Align2::CENTER_CENTER
                        .align_size_within_rect(size, ui.available_rect_before_wrap());
                    egui::Image::new(egui::load::SizedTexture::new(tex.id(), size)).paint_at(ui, rect);
                    if self.settings.scanlines {
                        draw_scanlines(ui.painter(), rect);
                    }
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("Open a ROM to start")
                                .color(egui::Color32::GRAY)
                                .size(16.0),
                        );
                    });
                }
            });
    }
}

/// Cheap CRT scanlines: a darkened 1px line every 3px down the picture.
fn draw_scanlines(painter: &egui::Painter, rect: egui::Rect) {
    let color = egui::Color32::from_black_alpha(70);
    let mut y = rect.top();
    while y < rect.bottom() {
        painter.hline(rect.x_range(), y, egui::Stroke::new(1.0, color));
        y += 3.0;
    }
}

/// On-disk save path: `<data dir>/saves/<system>/<game>.sav`.
fn save_path(system: System, game: &str) -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("me", "wvvw", "imlunahey-emulator")?;
    let safe: String = game
        .chars()
        .map(|c| if "/\\:?\"<>|".contains(c) { '_' } else { c })
        .collect();
    Some(
        dirs.data_dir()
            .join("saves")
            .join(sys_label(system))
            .join(format!("{safe}.sav")),
    )
}

fn sys_label(s: System) -> &'static str {
    match s {
        System::Gba => "GBA",
        System::Ps1 => "PS1",
        System::Nds => "NDS",
        System::Nes => "NES",
        System::Sms => "SMS",
        System::GameGear => "GameGear",
        System::Gbc => "GBC",
        System::Xbox => "Xbox",
        System::Snes => "SNES",
        System::Genesis => "Genesis",
        System::Pce => "PCE",
        System::Atari2600 => "Atari2600",
        System::Ngpc => "NGPC",
        System::WonderSwan => "WonderSwan",
        System::VirtualBoy => "VirtualBoy",
        System::N64 => "N64",
    }
}

/// Pick a system from the content extension; falls back to the Xbox disc magic.
fn detect_system(ext: Option<&str>, data: &[u8]) -> Option<System> {
    const MAGIC: &[u8] = b"MICROSOFT*XBOX*MEDIA";
    if data.len() >= 0x10000 + MAGIC.len() && &data[0x10000..0x10000 + MAGIC.len()] == MAGIC {
        return Some(System::Xbox);
    }
    Some(match ext? {
        "gba" => System::Gba,
        "nds" => System::Nds,
        "nes" => System::Nes,
        "sms" => System::Sms,
        "gg" => System::GameGear,
        "gb" | "gbc" => System::Gbc,
        "smc" | "sfc" => System::Snes,
        "md" | "gen" | "smd" => System::Genesis,
        "pce" => System::Pce,
        "a26" => System::Atari2600,
        "ngc" | "ngp" => System::Ngpc,
        "ws" | "wsc" => System::WonderSwan,
        "vb" | "vboy" => System::VirtualBoy,
        "n64" | "z64" | "v64" => System::N64,
        "xbe" | "xiso" => System::Xbox,
        "cue" | "bin" | "img" | "iso" | "pbp" => System::Ps1,
        _ => return None,
    })
}

/// Read the keyboard into the logical `BTN_*` mask. Defaults mirror the web /
/// macOS app: arrows, Z=B, X=A, A=Y, S=X, Q=L, W=R, Enter=Start, Shift=Select.
fn read_keyboard(ctx: &egui::Context) -> u32 {
    ctx.input(|i| {
        let mut m = 0u32;
        let mut k = |key, flag| {
            if i.key_down(key) {
                m |= flag;
            }
        };
        k(egui::Key::ArrowUp, BTN_UP);
        k(egui::Key::ArrowDown, BTN_DOWN);
        k(egui::Key::ArrowLeft, BTN_LEFT);
        k(egui::Key::ArrowRight, BTN_RIGHT);
        k(egui::Key::X, BTN_EAST); // A
        k(egui::Key::Z, BTN_SOUTH); // B
        k(egui::Key::S, BTN_NORTH); // X
        k(egui::Key::A, BTN_WEST); // Y
        k(egui::Key::Q, BTN_L1);
        k(egui::Key::W, BTN_R1);
        k(egui::Key::D, BTN_L2);
        k(egui::Key::F, BTN_R2);
        k(egui::Key::Enter, BTN_START);
        if i.modifiers.shift {
            m |= BTN_SELECT;
        }
        m
    })
}

/// Read the first connected gamepad into the logical `BTN_*` mask. Cross → A,
/// Circle → B (PlayStation convention), left stick acts as the d-pad.
fn read_gamepad(gilrs: &gilrs::Gilrs) -> u32 {
    use gilrs::{Axis, Button};
    let Some((_, pad)) = gilrs.gamepads().next() else {
        return 0;
    };
    let mut m = 0u32;
    let mut b = |btn, flag| {
        if pad.is_pressed(btn) {
            m |= flag;
        }
    };
    b(Button::DPadUp, BTN_UP);
    b(Button::DPadDown, BTN_DOWN);
    b(Button::DPadLeft, BTN_LEFT);
    b(Button::DPadRight, BTN_RIGHT);
    b(Button::South, BTN_EAST); // Cross → A
    b(Button::East, BTN_SOUTH); // Circle → B
    b(Button::West, BTN_WEST); // Square → Y
    b(Button::North, BTN_NORTH); // Triangle → X
    b(Button::LeftTrigger, BTN_L1);
    b(Button::RightTrigger, BTN_R1);
    b(Button::LeftTrigger2, BTN_L2);
    b(Button::RightTrigger2, BTN_R2);
    b(Button::Start, BTN_START);
    b(Button::Select, BTN_SELECT);
    let t = 0.4;
    let (x, y) = (pad.value(Axis::LeftStickX), pad.value(Axis::LeftStickY));
    if y > t {
        m |= BTN_UP;
    }
    if y < -t {
        m |= BTN_DOWN;
    }
    if x < -t {
        m |= BTN_LEFT;
    }
    if x > t {
        m |= BTN_RIGHT;
    }
    m
}

/// cpal audio sink. Resamples the core's interleaved f32 (mono or stereo) to the
/// device's stereo output rate (nearest-neighbour) through a shared ring buffer.
struct AudioOut {
    buf: Arc<Mutex<VecDeque<f32>>>,
    device_rate: u32,
    resample_pos: f32,
    volume: f32,
    _stream: cpal::Stream,
}

impl AudioOut {
    fn new() -> Option<AudioOut> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let host = cpal::default_host();
        let device = host.default_output_device()?;
        let supported = device.default_output_config().ok()?;
        let device_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let buf: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let cb_buf = buf.clone();
        let config: cpal::StreamConfig = supported.config();

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut b = cb_buf.lock().unwrap();
                    for frame in data.chunks_mut(channels) {
                        let l = b.pop_front().unwrap_or(0.0);
                        let r = b.pop_front().unwrap_or(l);
                        for (i, s) in frame.iter_mut().enumerate() {
                            *s = match i {
                                0 => l,
                                1 => r,
                                _ => 0.0,
                            };
                        }
                    }
                },
                |err| eprintln!("audio stream error: {err}"),
                None,
            )
            .ok()?;
        stream.play().ok()?;
        Some(AudioOut {
            buf,
            device_rate,
            resample_pos: 0.0,
            volume: 1.0,
            _stream: stream,
        })
    }

    fn push(&mut self, samples: &[f32], src_rate: u32, src_channels: usize) {
        if samples.is_empty() || self.device_rate == 0 {
            return;
        }
        let vol = self.volume;
        let frames: Vec<(f32, f32)> = if src_channels >= 2 {
            samples
                .chunks_exact(2)
                .map(|c| (c[0] * vol, c[1] * vol))
                .collect()
        } else {
            samples.iter().map(|&v| (v * vol, v * vol)).collect()
        };
        let step = src_rate as f32 / self.device_rate as f32;
        let mut out = self.buf.lock().unwrap();
        if out.len() > (self.device_rate as usize) / 2 {
            out.clear();
            self.resample_pos = 0.0;
        }
        let mut pos = self.resample_pos;
        while (pos as usize) < frames.len() {
            let (l, r) = frames[pos as usize];
            out.push_back(l);
            out.push_back(r);
            pos += step;
        }
        self.resample_pos = pos - frames.len() as f32;
    }
}
