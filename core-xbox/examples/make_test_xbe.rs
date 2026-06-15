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

/// NV2A "increasing methods" command header (count=1, subchannel 0).
fn hdr(method: u32) -> u32 {
    (1u32 << 18) | method
}

/// Emit an immediate-mode flat triangle (screen-space coords) into the
/// pushbuffer: BEGIN(triangles), 3 × (diffuse, posX, posY), END.
fn triangle(w: &mut Vec<u32>, verts: [(f32, f32); 3], color: u32) {
    w.push(hdr(0x17FC)); // SET_BEGIN_END
    w.push(4); // TRIANGLES
    for (x, y) in verts {
        w.push(hdr(0x194C)); // diffuse color
        w.push(color);
        w.push(hdr(0x1880)); // vertex X
        w.push(x.to_bits());
        w.push(hdr(0x1884)); // vertex Y (completes the vertex)
        w.push(y.to_bits());
    }
    w.push(hdr(0x17FC)); // SET_BEGIN_END
    w.push(0); // END -> rasterize
}

/// Emit a clip-rect + clear-to-color into the pushbuffer.
fn clip_clear(w: &mut Vec<u32>, x: u32, width: u32, y: u32, height: u32, color: u32) {
    w.push(hdr(0x0200)); // SET_SURFACE_CLIP_HORIZONTAL
    w.push(x | (width << 16));
    w.push(hdr(0x0204)); // SET_SURFACE_CLIP_VERTICAL
    w.push(y | (height << 16));
    w.push(hdr(0x1D90)); // SET_COLOR_CLEAR_VALUE (ARGB)
    w.push(color);
    w.push(hdr(0x1D94)); // CLEAR_SURFACE (color buffer)
    w.push(0xF0);
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
    //   header = (1<<18) | method ; followed by the data word. We compose a boot
    //   screen out of several clip+clear rectangles.
    let mut words: Vec<u32> = Vec::new();
    words.push(hdr(0x020C));
    words.push(640 * 4); // SET_SURFACE_PITCH
    words.push(hdr(0x0210));
    words.push(SURFACE); // SET_SURFACE_COLOR_OFFSET
    clip_clear(&mut words, 0, 640, 0, 480, 0xFF10_1820); // dark background
    clip_clear(&mut words, 0, 640, 0, 80, 0xFF6C_D84C); // top bar (Xbox green)
    clip_clear(&mut words, 220, 200, 180, 120, 0xFFE8_E8E8); // center panel
    clip_clear(&mut words, 0, 640, 440, 40, 0xFF20_50C0); // bottom bar (blue)
    // A red triangle drawn through the primitive pipeline, over the panel.
    triangle(&mut words, [(320.0, 180.0), (250.0, 320.0), (390.0, 320.0)], 0xFFD0_2020);

    // x86: write the pushbuffer into RAM, set GET then PUT (kicks the GPU), spin.
    let mut code = Vec::new();
    for (i, &w) in words.iter().enumerate() {
        mov_mem_imm(&mut code, PBUF + (i as u32) * 4, w);
    }
    mov_mem_imm(&mut code, DMA_GET, PBUF);
    mov_mem_imm(&mut code, DMA_PUT, PBUF + words.len() as u32 * 4);
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
