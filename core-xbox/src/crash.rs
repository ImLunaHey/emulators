//! Crash screen — rendered into the display framebuffer when the core detects a
//! fault loop (an unhandled-exception storm), so the host shows a legible error
//! readout instead of a silent black hang. Self-contained: a tiny 5x7 bitmap
//! font drawn straight into the GPU's RGBA framebuffer, on a dark panel in the
//! Xbox's signature green. Mirrors the PS1 core's crash screen.

use crate::gpu::Gpu;

/// Crash-screen display size — the Xbox NTSC default.
pub const W: usize = 640;
pub const H: usize = 480;

const BG: u32 = rgb(0x06, 0x0E, 0x06); // near-black, faint green tint
const FG: u32 = rgb(0x6C, 0xD8, 0x4C); // Xbox green

/// Pack RGB into the framebuffer's RGBA8888 word (A in the high byte; the host
/// reads the buffer as little-endian bytes R,G,B,A).
const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    0xFF00_0000 | ((b as u32) << 16) | ((g as u32) << 8) | (r as u32)
}

/// Render `lines` of text onto the GPU's framebuffer and switch the display to
/// the fixed crash size, so [`Gpu::frame`] presents the crash screen.
pub fn render(gpu: &mut Gpu, lines: &[String]) {
    gpu.display_w = W as u16;
    gpu.display_h = H as u16;
    if gpu.framebuffer.len() != W * H {
        gpu.framebuffer.resize(W * H, BG);
    }
    for px in gpu.framebuffer[..W * H].iter_mut() {
        *px = BG;
    }

    const SCALE: usize = 3;
    let margin = 24;
    for (li, line) in lines.iter().enumerate() {
        let y0 = margin + li * (7 * SCALE + 5);
        for (ci, ch) in line.bytes().enumerate() {
            let x0 = margin + ci * (5 * SCALE + SCALE);
            draw_glyph(&mut gpu.framebuffer, x0, y0, ch, SCALE);
        }
    }
}

/// Blit one `SCALE`-magnified glyph at (x0,y0) into the framebuffer.
fn draw_glyph(fb: &mut [u32], x0: usize, y0: usize, ch: u8, scale: usize) {
    let rows = glyph(ch);
    for (ry, row) in rows.iter().enumerate() {
        for cx in 0..5 {
            if row & (1 << (4 - cx)) == 0 {
                continue;
            }
            for sy in 0..scale {
                for sx in 0..scale {
                    let x = x0 + cx * scale + sx;
                    let y = y0 + ry * scale + sy;
                    if x < W && y < H {
                        fb[y * W + x] = FG;
                    }
                }
            }
        }
    }
}

/// 5x7 bitmap for an uppercase/digit glyph (low 5 bits per row, MSB = leftmost).
/// Unknown characters render as blank (space).
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
        b'#' => [0b01010, 0b11111, 0b01010, 0b01010, 0b01010, 0b11111, 0b01010],
        _ => [0; 7], // space / unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_sets_crash_geometry_and_draws() {
        let mut gpu = Gpu::new();
        render(&mut gpu, &["XBOX FAULT".to_string(), "UD AT 1234".to_string()]);
        assert_eq!(gpu.display_w as usize, W);
        assert_eq!(gpu.display_h as usize, H);
        assert_eq!(gpu.framebuffer.len(), W * H);
        // The panel background is present and at least one foreground pixel was
        // drawn for the text.
        assert!(gpu.framebuffer.iter().any(|&p| p == BG));
        assert!(gpu.framebuffer.iter().any(|&p| p == FG));
    }
}
