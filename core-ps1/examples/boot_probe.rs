//! Native boot probe: load OpenBIOS + a disc, run frames, and report progress
//! (PC / CD read state / display / VRAM activity + crash dump). The HLE disc-boot
//! in `Psx` loads the game EXE once the kernel vectors are installed.
//!
//! Run: `cargo run --example boot_probe --release [-- /path/to/disc.bin]`
//! (defaults to the bundled Spyro disc).

use ps1_core::Psx;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bios = std::fs::read(root.join("../src/assets/openbios.bin")).expect("read openbios.bin");
    let disc_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../public/Spyro the Dragon (USA).bin"));
    let disc = std::fs::read(&disc_path).expect("read disc");
    println!("bios {} bytes, disc {} ({} sectors)", bios.len(), disc_path.display(), disc.len() / 2352);

    let mut psx = Psx::new();
    psx.load_bios(&bios);
    psx.load_disc(disc);

    const FRAMES: u32 = 3000;
    let mut first_cd = 0u32;
    let mut first_draw = 0u32;
    for f in 0..FRAMES {
        psx.run_frame();
        if first_cd == 0 && psx.debug_cd_lba() != 0 {
            first_cd = f;
            println!(">>> FIRST CD READ at frame {} lba={}", f, psx.debug_cd_lba());
        }
        let nb_now = psx.gpu.vram.iter().filter(|&&p| p != 0).count();
        if first_draw == 0 && nb_now != 0 {
            first_draw = f;
            println!(">>> FIRST VRAM DRAW at frame {} ({} px)", f, nb_now);
        }
        if f % 500 == 0 {
            println!(
                "f={:>4} pc=0x{:08x} cdLba={} vram={} exc={} cnt[0x800d01e4]={}",
                f, psx.debug_pc(), psx.debug_cd_lba(),
                nb_now, psx.cpu.exceptions, psx.mem.ram_read32(0xd01e4) as i32,
            );
        }
        if psx.fault.is_some() {
            println!("FAULTED at frame {}: {:?}", f, psx.fault.unwrap());
            break;
        }
    }
    let nb = psx.gpu.vram.iter().filter(|&&p| p != 0).count();
    println!("\nfinal: pc=0x{:08x} cdLba={} disp={}x{} vramNonzero={}",
        psx.debug_pc(), psx.debug_cd_lba(), psx.gpu.display_w, psx.gpu.display_h, nb);
}
