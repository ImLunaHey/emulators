//! Differential-trace dumper: boot OpenBIOS + the Spyro disc with instruction
//! tracing on, and write the executed-PC stream (one hex PC per line) to
//! `core-ps1/trace.txt`. Diff this against a reference emulator's PC trace
//! (e.g. PCSX-Redux running the same OpenBIOS + disc) to find the FIRST point
//! where our core diverges — that pinpoints the emulation bug breaking boot.
//!
//! Run: `cargo run --example trace_dump --release` from core-ps1/.

use ps1_core::Psx;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bios = std::fs::read(root.join("../src/assets/openbios.bin")).expect("read openbios.bin");
    let disc = std::fs::read(root.join("../public/Spyro the Dragon (USA).bin")).expect("read disc");

    let mut psx = Psx::new();
    psx.load_bios(&bios);
    psx.load_disc(disc);

    const CAP: usize = 30_000_000; // instructions to record
    psx.enable_trace(CAP);
    // Run frames until the trace buffer fills (boot is well under this).
    for _ in 0..6000 {
        psx.run_frame();
        if psx.trace.as_ref().map(|t| t.len() >= CAP).unwrap_or(true) {
            break;
        }
    }

    let trace = psx.take_trace();
    let out = std::env::args().nth(2).map(PathBuf::from).unwrap_or_else(|| root.join("trace.txt"));
    let f = std::fs::File::create(&out).expect("create trace.txt");
    let mut w = std::io::BufWriter::new(f);
    for pc in &trace {
        writeln!(w, "{:08x}", pc).unwrap();
    }
    w.flush().unwrap();
    println!("wrote {} PCs to {}", trace.len(), out.display());
}
