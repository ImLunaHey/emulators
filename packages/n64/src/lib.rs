//! Pure-Rust Nintendo 64 core, built from scratch against the n64brew wiki, the
//! NEC VR4300 / MIPS R4300i user manual, and the n64dev hardware docs. Sibling
//! of the GBA (`../core`), NDS, PS1, NES, GBC and SMS cores in this repo.
//!
//! Ownership model (see the sibling cores' CONTRACT.md): one [`N64`] god-struct
//! owns the VR4300 CPU + RDRAM + every RCP register block + the cartridge and
//! implements the CPU's [`bus::Bus`]. Cross-subsystem calls pass `&mut`
//! references resolved with `mem::take` at the call site — no `Rc`/`RefCell`.
//! The N64 is BIG-ENDIAN; multi-byte values in RDRAM / ROM are stored MSB-first.
//!
//! FOUNDATION SCOPE — what works vs. what is stubbed:
//!
//! | Subsystem            | Status                                              |
//! |----------------------|-----------------------------------------------------|
//! | VR4300 integer ISA   | implemented (MIPS III, branch delay, HI/LO, 64-bit) |
//! | COP0 / exceptions    | implemented (Status/Cause/EPC, timer, TLB, ERET)    |
//! | COP1 / FPU           | implemented (load/store/move/arith subset)          |
//! | Memory map / bus     | implemented (RDRAM + all RCP blocks + cart)         |
//! | ROM load + byteswap  | implemented (.z64/.n64/.v64)                         |
//! | HLE IPL3 boot        | implemented (CIC 6102 register state + boot copy)   |
//! | MI / interrupts      | implemented (intr aggregation -> Cause.IP2)         |
//! | PI / SI DMA + joybus | implemented (cart DMA, controller read)             |
//! | VI scanout           | implemented (RDRAM framebuffer -> RGBA8888)          |
//! | RSP scalar+vector    | STUBBED (DMA + regs only; microcode does not run)   |
//! | RDP rasteriser       | STUBBED (regs only; no triangles drawn)             |
//! | AI audio             | STUBBED (regs only; no samples produced)            |
//!
//! Builds as a normal rlib for host `cargo test`; the web target layers a
//! wasm-bindgen surface on top. The core stays binding-agnostic.

// --- CPU (VR4300 + COP0 + COP1 + interpreter).
pub mod cpu;

// --- Foundation: the bus the interpreter codes against + the memory map.
pub mod bus;
pub mod regions;

// --- RCP + system subsystems (one struct per device).
pub mod ai;
pub mod boot;
pub mod cart;
pub mod crash;
pub mod mi;
pub mod pi;
pub mod rdp;
pub mod rsp;
pub mod si;
pub mod vi;

// --- Top-level orchestrator (the god-struct + Bus impl).
pub mod n64;

// --- Web target: the wasm-bindgen surface (`WasmN64`). wasm32-only so host
// `cargo test` never invokes the macro, mirroring the sibling cores.
#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use cpu::{Cop0, Cop1, Cpu, Exception};
pub use n64::N64;
