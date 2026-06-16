//! Cross-platform desktop front-end (Windows / macOS / Linux) for every core —
//! the portable counterpart to the macOS-only SwiftUI app in apps/EmuApp.
//!
//! Pure Rust: links `emu-native` directly (no FFI), renders + UIs with egui,
//! plays audio through cpal, and reads gamepads via gilrs. This v1 covers the
//! essentials (open a ROM, run, video, audio, keyboard + gamepad); settings,
//! retro effects/upscaling, remapping, and save management land on top.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::VecDeque;
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

struct App {
    emu: Option<Emu>,
    title: String,
    tex: Option<egui::TextureHandle>,
    audio: Option<AudioOut>,
    gilrs: Option<gilrs::Gilrs>,
    last: Instant,
    acc: f32,
}

impl App {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            emu: None,
            title: String::new(),
            tex: None,
            audio: None,
            gilrs: gilrs::Gilrs::new().ok(),
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
        self.audio = AudioOut::new();
        self.title = name;
        self.emu = Some(emu);
        self.acc = 0.0;
        self.last = Instant::now();
    }

    fn stop(&mut self) {
        self.emu = None;
        self.audio = None;
        self.tex = None;
        self.title.clear();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Pump gamepad events so button state is current.
        if let Some(g) = &mut self.gilrs {
            while g.next_event().is_some() {}
        }
        let gamepad_mask = self.gilrs.as_ref().map(read_gamepad).unwrap_or(0);

        // Advance emulation to keep ~60 fps, capped so a slow frame can't spiral.
        if self.emu.is_some() {
            let now = Instant::now();
            let dt = (now - self.last).as_secs_f32().min(0.1);
            self.last = now;
            self.acc += dt;
            let frame_time = 1.0 / 60.0;
            let buttons = read_keyboard(ctx) | gamepad_mask;
            let mut ran = 0;
            while self.acc >= frame_time && ran < 4 {
                self.acc -= frame_time;
                ran += 1;
                let emu = self.emu.as_mut().unwrap();
                emu.set_buttons(buttons);
                emu.run_frame();
                if let Some(audio) = &mut self.audio {
                    let mut tmp = [0f32; 4096];
                    let n = emu.drain_audio(&mut tmp);
                    if n > 0 {
                        audio.push(&tmp[..n], emu.sample_rate(), emu.channels() as usize);
                    }
                }
            }
            // Upload the latest frame to the texture once per UI repaint.
            let emu = self.emu.as_ref().unwrap();
            let (w, h) = (emu.width() as usize, emu.height() as usize);
            let fb = emu.framebuffer();
            if w > 0 && h > 0 && fb.len() == w * h * 4 {
                let img = egui::ColorImage::from_rgba_unmultiplied([w, h], fb);
                match &mut self.tex {
                    Some(t) => t.set(img, egui::TextureOptions::NEAREST),
                    None => {
                        self.tex =
                            Some(ctx.load_texture("framebuffer", img, egui::TextureOptions::NEAREST))
                    }
                }
            }
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.emu.is_some() {
                    ui.strong(&self.title);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Stop").clicked() {
                            self.stop();
                        }
                    });
                } else {
                    if ui.button("Open ROM…").clicked() {
                        self.open_rom();
                    }
                    if !self.title.is_empty() {
                        ui.label(&self.title);
                    }
                }
            });
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                if let Some(tex) = &self.tex {
                    let avail = ui.available_size();
                    let [tw, th] = tex.size();
                    let (tw, th) = (tw as f32, th as f32);
                    let scale = (avail.x / tw).min(avail.y / th).max(0.0);
                    let size = egui::vec2(tw * scale, th * scale);
                    ui.centered_and_justified(|ui| {
                        ui.add(egui::Image::new(egui::load::SizedTexture::new(tex.id(), size)));
                    });
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
    // Left analog stick → d-pad.
    let t = 0.4;
    let x = pad.value(Axis::LeftStickX);
    let y = pad.value(Axis::LeftStickY);
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
            _stream: stream,
        })
    }

    fn push(&mut self, samples: &[f32], src_rate: u32, src_channels: usize) {
        if samples.is_empty() || self.device_rate == 0 {
            return;
        }
        let frames: Vec<(f32, f32)> = if src_channels >= 2 {
            samples.chunks_exact(2).map(|c| (c[0], c[1])).collect()
        } else {
            samples.iter().map(|&v| (v, v)).collect()
        };
        let step = src_rate as f32 / self.device_rate as f32; // src advance per out frame
        let mut out = self.buf.lock().unwrap();
        // Bound latency: if we're more than ~0.25 s behind, resync.
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
