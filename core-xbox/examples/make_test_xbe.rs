//! Generate a minimal homebrew Xbox executable (.xbe) that clears the screen to
//! a colour via the NV2A pushbuffer — the classic Xbox "hello world". It boots
//! through the real path (XBE loader -> x86 interpreter -> NV2A FIFO -> PGRAPH
//! clear -> PCRTC scanout) and renders a visible boot screen, with no kernel
//! imports.
//!
//!   cargo run --example make_test_xbe -- /tmp/clear.xbe
//!
//! The x86 it emits: write a 6-method pushbuffer (surface setup + clear) into
//! RAM, point the NV2A channel GET/PUT at it (MMIO writes), then spin.

use std::io::Write;

const BASE: u32 = 0x0001_0000;
const ENTRY: u32 = 0x0001_1000;
const CODE_FILE_OFF: usize = 0x1000;
const SEC_HDR_OFF: usize = 0x180;
const PBUF: u32 = 0x0002_0000; // pushbuffer in RAM
const SURFACE: u32 = 0x0010_0000; // color surface in RAM (1 MB)

// NV2A USER channel registers (MMIO).
const DMA_PUT: u32 = 0xFD80_0040;
const DMA_GET: u32 = 0xFD80_0044;

fn put32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

/// `MOV dword [disp32], imm32` = C7 05 <disp32> <imm32>.
fn mov_mem_imm(code: &mut Vec<u8>, disp: u32, imm: u32) {
    code.push(0xC7);
    code.push(0x05);
    code.extend_from_slice(&disp.to_le_bytes());
    code.extend_from_slice(&imm.to_le_bytes());
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/clear.xbe".into());

    // The pushbuffer: NV2A "increasing methods" command words (count=1, subch=0):
    //   header = (1<<18) | method ; followed by the data word.
    let hdr = |method: u32| (1u32 << 18) | method;
    let cmds: [u32; 12] = [
        hdr(0x020C), 640 * 4,        // SET_SURFACE_PITCH
        hdr(0x0210), SURFACE,        // SET_SURFACE_COLOR_OFFSET
        hdr(0x0200), 640 << 16,      // SET_SURFACE_CLIP_HORIZONTAL (width 640)
        hdr(0x0204), 480 << 16,      // SET_SURFACE_CLIP_VERTICAL  (height 480)
        hdr(0x1D90), 0xFF6C_D84C,    // SET_COLOR_CLEAR_VALUE — Xbox green (ARGB)
        hdr(0x1D94), 0x0000_00F0,    // CLEAR_SURFACE (color buffer)
    ];

    // x86: write the pushbuffer into RAM, set GET then PUT (kicks the GPU), spin.
    let mut code = Vec::new();
    for (i, &w) in cmds.iter().enumerate() {
        mov_mem_imm(&mut code, PBUF + (i as u32) * 4, w);
    }
    mov_mem_imm(&mut code, DMA_GET, PBUF);
    mov_mem_imm(&mut code, DMA_PUT, PBUF + cmds.len() as u32 * 4);
    code.push(0xEB); // JMP rel8
    code.push(0xFE); // -2 (spin)

    // Assemble the XBE.
    let mut xbe = vec![0u8; CODE_FILE_OFF + code.len()];
    xbe[0..4].copy_from_slice(b"XBEH");
    put32(&mut xbe, 0x104, BASE); // base address
    put32(&mut xbe, 0x108, 0x1000); // size of headers
    put32(&mut xbe, 0x11C, 1); // number of sections
    put32(&mut xbe, 0x120, BASE + SEC_HDR_OFF as u32); // section headers vaddr
    put32(&mut xbe, 0x128, ENTRY ^ 0xA8FC_57AB); // entry (retail XOR)
    put32(&mut xbe, 0x130, 0x1_0000); // stack commit
    put32(&mut xbe, 0x158, (BASE + 0xFF0) ^ 0x5B6D_40B6); // kernel thunk -> zero (no imports)

    // Section header.
    put32(&mut xbe, SEC_HDR_OFF, 0x0000_0007); // flags
    put32(&mut xbe, SEC_HDR_OFF + 0x04, ENTRY); // vaddr
    put32(&mut xbe, SEC_HDR_OFF + 0x08, code.len() as u32); // vsize
    put32(&mut xbe, SEC_HDR_OFF + 0x0C, CODE_FILE_OFF as u32); // raw addr
    put32(&mut xbe, SEC_HDR_OFF + 0x10, code.len() as u32); // raw size

    xbe[CODE_FILE_OFF..CODE_FILE_OFF + code.len()].copy_from_slice(&code);

    std::fs::File::create(&path).unwrap().write_all(&xbe).unwrap();
    println!("wrote {} ({} bytes, {} bytes code)", path, xbe.len(), code.len());
}
