//! Headless driver to debug the native FFI without the GUI: load a ROM and run
//! frames with timing. `cargo run --example run_headless -- <system_id> <path>`
//! (system ids: 0 GBA, 1 PS1, 2 NDS, 3 NES, 4 SMS, 5 GG, 6 GBC, 7 Xbox)

use std::fs;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sys: u32 = args[1].parse().unwrap();
    let path = &args[2];
    let data = fs::read(path).unwrap();
    println!("rom: {} ({} bytes)", path, data.len());

    unsafe {
        let e = emu_native::emu_new(sys);
        assert!(!e.is_null(), "emu_new returned null");
        let ok = emu_native::emu_load_rom(e, data.as_ptr(), data.len());
        println!(
            "load_rom={ok} w={} h={} rate={} ch={}",
            emu_native::emu_width(e),
            emu_native::emu_height(e),
            emu_native::emu_sample_rate(e),
            emu_native::emu_channels(e)
        );

        let total = Instant::now();
        for i in 0..120 {
            let f = Instant::now();
            emu_native::emu_run_frame(e);
            if i < 3 || i == 119 {
                println!("  frame {i}: {:?}", f.elapsed());
            }
        }
        println!("120 frames in {:?}", total.elapsed());

        let p = emu_native::emu_framebuffer_ptr(e);
        let n = emu_native::emu_framebuffer_len(e);
        let s = std::slice::from_raw_parts(p, n);
        let sum: u64 = s.iter().map(|&b| b as u64).sum();
        println!("framebuffer len={n} checksum={sum}");

        let mut audio = vec![0f32; 8192];
        let na = emu_native::emu_drain_audio(e, audio.as_mut_ptr(), audio.len());
        println!("audio drained: {na} samples");

        emu_native::emu_free(e);
    }
    println!("OK");
}
