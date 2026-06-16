//! Crash screen — rendered into the PPU framebuffer when the core detects a
//! fault loop (an undefined-instruction / abort exception storm), so the host
//! shows a legible error readout instead of a silent hang. Self-contained: a
//! tiny 5x7 bitmap font drawn straight into the RGBA8888 byte framebuffer.
//!
//! Ported from the PS1 core's `crash.rs`; that core's framebuffer is `[u32]`,
//! the GBA's is RGBA8888 *bytes* (240x160), so we write four bytes per pixel.

/// Dark-blue background and white foreground, as RGBA8888 byte tuples.
const BG: [u8; 4] = [0x10, 0x10, 0x60, 0xFF];
const FG: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

/// Render `lines` of text onto a `w`x`h` RGBA8888 byte framebuffer: clear to
/// the dark-blue panel, then blit the 5x7 glyphs in white. Scale 1 — at 240px
/// wide that fits ~40 characters per line.
pub fn render(fb: &mut [u8], w: usize, h: usize, lines: &[String]) {
    // Clear to the background colour.
    for px in fb.chunks_exact_mut(4) {
        px.copy_from_slice(&BG);
    }

    let margin = 8usize;
    for (li, line) in lines.iter().enumerate() {
        let y0 = margin + li * (7 + 3); // 7px glyph + 3px line gap
        for (ci, ch) in line.bytes().enumerate() {
            let x0 = margin + ci * 6; // 5px glyph + 1px gap
            draw_glyph(fb, w, h, x0, y0, ch);
        }
    }
}

/// Blit one 5x7 glyph at (x0,y0) into the byte framebuffer.
fn draw_glyph(fb: &mut [u8], w: usize, h: usize, x0: usize, y0: usize, ch: u8) {
    let rows = glyph(ch);
    for (ry, row) in rows.iter().enumerate() {
        for cx in 0..5 {
            if row & (1 << (4 - cx)) == 0 {
                continue;
            }
            let x = x0 + cx;
            let y = y0 + ry;
            if x < w && y < h {
                let off = (y * w + x) * 4;
                fb[off..off + 4].copy_from_slice(&FG);
            }
        }
    }
}

/// 5x7 bitmap for an uppercase/digit glyph (low 5 bits per row, MSB = leftmost).
/// Font covers A-Z 0-9 space : = - . only; unknown characters render as blank.
fn glyph(c: u8) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        b'0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        b'1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        b'3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        b'4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        b'5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        b'6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        b'7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        b'8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        b'9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        b'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        b'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        b'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        b'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        b'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        b'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        b'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
        b'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        b'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        b'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        b'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        b'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        b'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        b'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        b'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        b'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        b'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        b'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        b'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        b'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        b'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        b':' => [0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000],
        b'=' => [0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000],
        b'-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        b'.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100],
        _ => [0; 7], // space / unknown
    }
}
