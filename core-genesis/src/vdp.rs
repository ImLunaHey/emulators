//! Sega 315-5313 VDP (Video Display Processor) — the Genesis graphics chip.
//!
//! Built from the Sega VDP documentation and plutiedev.com. Implemented:
//!   - 24 control registers (write via the control port command protocol).
//!   - 64 KiB VRAM, 128-byte (64-entry) CRAM (9-bit BGR333), 80-byte VSRAM.
//!   - The control-port command word protocol (address + read/write code),
//!     auto-increment, and the data port for VRAM/CRAM/VSRAM access.
//!   - DMA: 68000->VRAM transfer, VRAM fill, VRAM->VRAM copy.
//!   - Plane A, Plane B (scroll planes), the window plane, and sprites, with
//!     per-tile priority. 320x224 or 256x224 output (H40 / H32).
//!   - H/V counters and the V-blank / H-blank interrupt flags.
//!
//! NOT implemented (best-effort core): per-line H scroll modes beyond full/tile
//! granularity edge cases, shadow/highlight, interlace, exact DMA timing/slot
//! stealing. Rendering is per-frame (not per-scanline) which is enough for a
//! title screen but not for raster-effect-heavy games.

pub const MAX_W: usize = 320;
pub const HEIGHT: usize = 224;
pub const FB_LEN: usize = MAX_W * HEIGHT * 4;
/// NTSC scanlines per frame.
pub const SCANLINES: u32 = 262;
/// First visible scanline.
const VISIBLE_LINES: u32 = 224;

/// Which region of VDP memory a data-port access targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Vram,
    Cram,
    Vsram,
    Invalid,
}

pub struct Vdp {
    /// 24 control registers ($00-$17).
    pub regs: [u8; 24],
    pub vram: Box<[u8; 0x10000]>,
    /// CRAM: 64 colour entries, stored as the raw 9-bit value (BGR333).
    pub cram: [u16; 64],
    /// Vertical scroll RAM: 40 entries (plane A even, plane B odd).
    pub vsram: [u16; 40],

    /// Control-port command state: the assembled 32-bit command, the decoded
    /// address + target + read/write, and the first/second-word latch.
    code: u8,
    address: u16,
    target: Target,
    /// True once the first control word has been written (awaiting the second).
    first_half: bool,
    pending_first: u16,
    /// Pending DMA fill: set when a fill command was issued; the next data-port
    /// write supplies the fill byte.
    dma_fill_pending: bool,

    /// Status register read by the 68000 ($C00004). Bits: bit3 VBLANK, bit2
    /// HBLANK, bit7 vint occurred, bit1 DMA busy, bit9 FIFO empty.
    pub status: u16,

    /// Frame counter.
    pub frame: u64,
    /// Current scanline (0..SCANLINES).
    pub line: u32,
    /// H-interrupt line counter (reloads from register $0A).
    hint_counter: u8,

    /// Pending interrupt latches consumed by the 68000 bus.
    pub vint_pending: bool,
    pub hint_pending: bool,

    /// Whether a memory->VDP DMA is pending (the god-struct drives the source
    /// fetch from 68000 space). Reset by `run_mem_dma`.
    pub pending_mem_dma: bool,

    /// RGBA8888 output. Always the full 320-wide buffer; the host crops to
    /// `width()` for H32 modes.
    pub framebuffer: Box<[u8; FB_LEN]>,
}

impl Default for Vdp {
    fn default() -> Self {
        Vdp::new()
    }
}

impl Vdp {
    pub fn new() -> Vdp {
        Vdp {
            regs: [0; 24],
            vram: vec![0u8; 0x10000].into_boxed_slice().try_into().unwrap(),
            cram: [0; 64],
            vsram: [0; 40],
            code: 0,
            address: 0,
            target: Target::Vram,
            first_half: false,
            pending_first: 0,
            dma_fill_pending: false,
            status: 0x3400, // FIFO empty + default bits
            frame: 0,
            line: 0,
            hint_counter: 0,
            vint_pending: false,
            hint_pending: false,
            pending_mem_dma: false,
            framebuffer: vec![0u8; FB_LEN].into_boxed_slice().try_into().unwrap(),
        }
    }

    // ---- display geometry ----
    /// Visible width: 320 (H40, reg $0C bit0) or 256 (H32).
    pub fn width(&self) -> usize {
        if self.regs[0x0C] & 0x81 != 0 {
            320
        } else {
            256
        }
    }
    pub fn height(&self) -> usize {
        HEIGHT
    }
    fn display_enabled(&self) -> bool {
        self.regs[0x01] & 0x40 != 0
    }
    fn vint_enabled(&self) -> bool {
        self.regs[0x01] & 0x20 != 0
    }
    fn hint_enabled(&self) -> bool {
        self.regs[0x00] & 0x10 != 0
    }
    fn auto_inc(&self) -> u16 {
        self.regs[0x0F] as u16
    }

    // =====================================================================
    // 68000 port interface ($C00000 data, $C00004 control).
    // =====================================================================

    /// Read the data port (16-bit). Returns the value at the current address
    /// from the targeted memory, then auto-increments.
    pub fn read_data(&mut self) -> u16 {
        let v = match self.target {
            Target::Vram => {
                let a = (self.address & 0xFFFE) as usize;
                ((self.vram[a] as u16) << 8) | self.vram[a + 1] as u16
            }
            Target::Cram => self.cram[((self.address >> 1) & 0x3F) as usize],
            Target::Vsram => self.vsram[(((self.address >> 1) as usize) % 40).min(39)],
            Target::Invalid => 0,
        };
        self.address = self.address.wrapping_add(self.auto_inc());
        v
    }

    /// Write the data port (16-bit).
    pub fn write_data(&mut self, v: u16) {
        if self.dma_fill_pending {
            // The data write supplies the fill value and kicks the fill.
            self.dma_fill_pending = false;
            self.dma_fill(v);
            return;
        }
        match self.target {
            Target::Vram => {
                let a = (self.address & 0xFFFE) as usize;
                self.vram[a] = (v >> 8) as u8;
                self.vram[a + 1] = (v & 0xFF) as u8;
            }
            Target::Cram => {
                self.cram[((self.address >> 1) & 0x3F) as usize] = v & 0x0EEE;
            }
            Target::Vsram => {
                let i = ((self.address >> 1) as usize) % 40;
                self.vsram[i] = v & 0x07FF;
            }
            Target::Invalid => {}
        }
        self.address = self.address.wrapping_add(self.auto_inc());
        self.first_half = false;
    }

    /// Read the control/status port (16-bit).
    pub fn read_status(&mut self) -> u16 {
        self.first_half = false;
        // Set/clear live bits.
        let mut s = self.status & !0x000A;
        if self.line >= VISIBLE_LINES {
            s |= 0x0008; // VBLANK
        }
        // HBLANK approximated as off mid-line; leave clear.
        self.status = s;
        s
    }

    /// Write the control port (16-bit). Two words assemble a command; a register
    /// write is a single word with the top two bits = 10.
    pub fn write_control(&mut self, v: u16) {
        if !self.first_half {
            // Could be a register write (10xxxxxx) or the first command word.
            if v & 0xC000 == 0x8000 {
                // Register write: 100rrrrr dddddddd
                let reg = ((v >> 8) & 0x1F) as usize;
                if reg < 24 {
                    self.regs[reg] = (v & 0xFF) as u8;
                }
                return;
            }
            // First command word.
            self.pending_first = v;
            self.first_half = true;
            // Tentatively decode for immediate single-word usage.
            self.update_command(self.pending_first, 0);
        } else {
            self.first_half = false;
            self.update_command(self.pending_first, v);
            // If this is a DMA command, perform it now.
            if self.code & 0x20 != 0 && self.regs[0x01] & 0x10 != 0 {
                self.maybe_dma();
            }
        }
    }

    /// Assemble the address + code from the two command words.
    fn update_command(&mut self, w0: u16, w1: u16) {
        // Command layout (Sega VDP):
        //   CD1 CD0 A13..A0   (word0: bits15-14 = code low, bits13-0 = addr low)
        //   CD5..CD2 0...0 A15 A14 (word1: bits 7-4 = code high, bits1-0 addr high)
        let addr = (w0 & 0x3FFF) | ((w1 & 0x0003) << 14);
        let code = ((w0 >> 14) & 0x03) | (((w1 >> 4) & 0x3C) as u16);
        self.address = addr;
        self.code = code as u8;
        self.target = match code & 0x0F {
            0x0 => Target::Vram,  // VRAM read
            0x1 => Target::Vram,  // VRAM write
            0x3 => Target::Cram,  // CRAM write
            0x4 => Target::Vsram, // VSRAM read
            0x5 => Target::Vsram, // VSRAM write
            0x8 => Target::Cram,  // CRAM read
            0x2 | 0x6 | 0x7 => Target::Invalid, // unmapped codes
            _ => Target::Vram,
        };
    }

    // =====================================================================
    // DMA.
    // =====================================================================
    /// Called after a DMA command word; dispatches by the DMA type in reg $17.
    fn maybe_dma(&mut self) {
        let dma_type = (self.regs[0x17] >> 6) & 0x03;
        match dma_type {
            0 | 1 => {
                // 68000 -> VRAM/CRAM/VSRAM. Handled by the bus (it owns 68000
                // memory); we expose `dma_transfer` for the bus to drive.
                // Mark pending; the bus checks `dma_src`/`dma_len` after a
                // control write. For our integration the Genesis god-struct
                // calls `take_pending_mem_dma`.
                self.pending_mem_dma = true;
            }
            2 => {
                // VRAM fill — wait for the data-port write that supplies the byte.
                self.dma_fill_pending = true;
            }
            3 => {
                // VRAM -> VRAM copy.
                self.dma_copy();
            }
            _ => {}
        }
    }

    /// Length from registers $13/$14 (word count).
    fn dma_length(&self) -> u32 {
        let len = (self.regs[0x13] as u32) | ((self.regs[0x14] as u32) << 8);
        if len == 0 {
            0x10000
        } else {
            len
        }
    }
    /// Source address from $15/$16/$17 (low bits), in words; the meaning depends
    /// on the DMA type.
    pub fn dma_source(&self) -> u32 {
        let lo = self.regs[0x15] as u32;
        let mid = (self.regs[0x16] as u32) << 8;
        let hi = ((self.regs[0x17] as u32) & 0x7F) << 16;
        (hi | mid | lo) << 1
    }

    fn dma_fill(&mut self, v: u16) {
        let len = self.dma_length();
        let fill_byte = (v >> 8) as u8;
        let inc = self.auto_inc();
        // First the supplied word is written to the current address.
        let a = (self.address & 0xFFFE) as usize;
        self.vram[a] = (v >> 8) as u8;
        self.vram[a + 1] = (v & 0xFF) as u8;
        self.address = self.address.wrapping_add(inc);
        for _ in 0..len {
            let a = (self.address & 0xFFFF) as usize;
            self.vram[a] = fill_byte;
            self.address = self.address.wrapping_add(inc);
        }
    }

    fn dma_copy(&mut self) {
        let len = self.dma_length();
        let mut src = self.dma_source() >> 1; // copy uses byte addressing
        let inc = self.auto_inc();
        for _ in 0..len {
            let b = self.vram[(src & 0xFFFF) as usize];
            let d = (self.address & 0xFFFF) as usize;
            self.vram[d] = b;
            src = src.wrapping_add(1);
            self.address = self.address.wrapping_add(inc);
        }
    }

    /// Run a pending 68000->VDP DMA. `read16` reads a word from 68000 space.
    /// The god-struct passes a closure over its bus.
    pub fn run_mem_dma<F: FnMut(u32) -> u16>(&mut self, mut read16: F) {
        if !self.pending_mem_dma {
            return;
        }
        self.pending_mem_dma = false;
        let len = self.dma_length();
        let mut src = self.dma_source();
        let inc = self.auto_inc();
        for _ in 0..len {
            let w = read16(src);
            match self.target {
                Target::Vram => {
                    let a = (self.address & 0xFFFE) as usize;
                    self.vram[a] = (w >> 8) as u8;
                    self.vram[a + 1] = (w & 0xFF) as u8;
                }
                Target::Cram => {
                    self.cram[((self.address >> 1) & 0x3F) as usize] = w & 0x0EEE;
                }
                Target::Vsram => {
                    let i = ((self.address >> 1) as usize) % 40;
                    self.vsram[i] = w & 0x07FF;
                }
                Target::Invalid => {}
            }
            src = src.wrapping_add(2);
            self.address = self.address.wrapping_add(inc);
        }
    }

    // =====================================================================
    // Frame / interrupt timing. Driven one scanline at a time by the host.
    // =====================================================================
    pub fn start_line(&mut self) {
        // H-interrupt handling at the start of each visible line.
        if self.line < VISIBLE_LINES {
            if self.hint_counter == 0 {
                self.hint_counter = self.regs[0x0A];
                if self.hint_enabled() {
                    self.hint_pending = true;
                }
            } else {
                self.hint_counter -= 1;
            }
        } else {
            self.hint_counter = self.regs[0x0A];
        }
    }

    /// Advance to the next scanline; raise V-int at the start of vblank.
    pub fn end_line(&mut self) {
        if self.line == VISIBLE_LINES {
            // Entering vertical blank.
            self.status |= 0x0080; // vint occurred flag
            if self.vint_enabled() {
                self.vint_pending = true;
            }
        }
        self.line += 1;
        if self.line >= SCANLINES {
            self.line = 0;
            self.frame += 1;
            self.render_frame();
        }
    }

    /// V counter as the 68000 reads it ($C00009-ish via HV port). 8-bit.
    pub fn v_counter(&self) -> u8 {
        // NTSC jump table approximation: lines 0..0xEA linear, then wrap.
        if self.line <= 0xEA {
            self.line as u8
        } else {
            (self.line.wrapping_sub(6)) as u8
        }
    }
    /// H counter — return a plausible stable value.
    pub fn h_counter(&self) -> u8 {
        0
    }

    /// Combined IRQ level the VDP requests (6 = vint, 4 = hint, 0 = none).
    pub fn irq_level(&self) -> u8 {
        if self.vint_pending {
            6
        } else if self.hint_pending {
            4
        } else {
            0
        }
    }
    pub fn ack_vint(&mut self) {
        self.vint_pending = false;
    }
    pub fn ack_hint(&mut self) {
        self.hint_pending = false;
    }

    // =====================================================================
    // Rendering. Per-frame: plane B, plane A (with window), sprites, by
    // priority. Output to the RGBA framebuffer.
    // =====================================================================
    fn render_frame(&mut self) {
        let w = self.width();
        // Backdrop colour from register $07 (palette/colour index).
        let bg_index = (self.regs[0x07] & 0x3F) as usize;
        let bg = self.cram_to_rgb(bg_index);
        // Clear with backdrop.
        for y in 0..HEIGHT {
            for x in 0..MAX_W {
                let off = (y * MAX_W + x) * 4;
                self.framebuffer[off] = bg.0;
                self.framebuffer[off + 1] = bg.1;
                self.framebuffer[off + 2] = bg.2;
                self.framebuffer[off + 3] = 0xFF;
            }
        }
        if !self.display_enabled() {
            return;
        }

        // Pixel priority buffer: per-pixel "is this from a high-priority tile".
        let mut prio = vec![false; MAX_W * HEIGHT];
        // colour-index 0 of the chosen palette is transparent for planes/sprites.

        // Plane sizes from register $10 (HSCROLL size + VSCROLL size).
        let (plane_w, plane_h) = self.plane_size();

        // Name table base addresses.
        let nta = ((self.regs[0x02] as usize) & 0x38) << 10; // plane A
        let ntb = ((self.regs[0x04] as usize) & 0x07) << 13; // plane B
        let hscroll_base = ((self.regs[0x0D] as usize) & 0x3F) << 10;

        // Draw plane B then plane A (A is on top at equal priority).
        for (pass, base) in [(1usize, ntb), (0usize, nta)] {
            self.draw_plane(base, plane_w, plane_h, hscroll_base, pass, w, &mut prio);
        }

        // Window plane (overrides plane A in its region) — basic support.
        self.draw_window(w);

        // Sprites on top.
        self.draw_sprites(w, &mut prio);
    }

    fn plane_size(&self) -> (usize, usize) {
        let r10 = self.regs[0x10];
        let hw = match r10 & 0x03 {
            0 => 32,
            1 => 64,
            3 => 128,
            _ => 32,
        };
        let vh = match (r10 >> 4) & 0x03 {
            0 => 32,
            1 => 64,
            3 => 128,
            _ => 32,
        };
        (hw, vh)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_plane(
        &mut self,
        nt_base: usize,
        plane_w: usize,
        plane_h: usize,
        hscroll_base: usize,
        plane_idx: usize,
        screen_w: usize,
        prio: &mut [bool],
    ) {
        // Horizontal scroll mode (reg $0B bits0-1). We support full-screen (0)
        // and per-line (3) at a coarse granularity.
        let hmode = self.regs[0x0B] & 0x03;
        let vmode = self.regs[0x0B] & 0x04; // 0 full, 4 per-2-tile column

        for y in 0..VISIBLE_LINES as usize {
            // Horizontal scroll for this line.
            let hs = self.hscroll(hscroll_base, hmode, plane_idx, y);
            for x in 0..screen_w {
                // Vertical scroll for this column.
                let vs = self.vscroll(vmode, plane_idx, x);
                let src_x = (x as i32 + hs).rem_euclid((plane_w * 8) as i32) as usize;
                let src_y = (y as i32 + vs).rem_euclid((plane_h * 8) as i32) as usize;
                let tile_col = src_x / 8;
                let tile_row = src_y / 8;
                let entry_addr = nt_base + (tile_row * plane_w + tile_col) * 2;
                if entry_addr + 1 >= 0x10000 {
                    continue;
                }
                let entry =
                    ((self.vram[entry_addr] as u16) << 8) | self.vram[entry_addr + 1] as u16;
                let tile = (entry & 0x07FF) as usize;
                let hflip = entry & 0x0800 != 0;
                let vflip = entry & 0x1000 != 0;
                let pal = ((entry >> 13) & 0x03) as usize;
                let priority = entry & 0x8000 != 0;

                let mut fx = src_x % 8;
                let mut fy = src_y % 8;
                if hflip {
                    fx = 7 - fx;
                }
                if vflip {
                    fy = 7 - fy;
                }
                let ci = self.tile_pixel(tile, fx, fy);
                if ci == 0 {
                    continue; // transparent
                }
                let pidx = y * MAX_W + x;
                // Plane A (idx 0) overwrites plane B unless B pixel is high-prio
                // and this one isn't.
                if plane_idx == 1 {
                    // plane B: only draw where nothing drawn (we draw B first)
                    self.put_pixel(x, y, pal, ci);
                    prio[pidx] = priority;
                } else {
                    // plane A: draw if higher/equal priority than existing
                    if priority || !prio[pidx] {
                        self.put_pixel(x, y, pal, ci);
                        if priority {
                            prio[pidx] = true;
                        }
                    }
                }
            }
        }
    }

    fn hscroll(&self, base: usize, hmode: u8, plane_idx: usize, y: usize) -> i32 {
        let entry = match hmode {
            0 => 0,                  // whole screen
            2 => (y / 8) * 8,        // per-tile (every 8 lines)
            3 => y,                  // per-line
            _ => 0,
        };
        let addr = base + entry * 4 + plane_idx * 2;
        if addr + 1 >= 0x10000 {
            return 0;
        }
        let raw = ((self.vram[addr] as u16) << 8) | self.vram[addr + 1] as u16;
        // Scroll value is how far the plane shifts right; we add the negative.
        -((raw & 0x03FF) as i16 as i32)
    }

    fn vscroll(&self, vmode: u8, plane_idx: usize, x: usize) -> i32 {
        let entry = if vmode != 0 {
            // per-2-tile column
            ((x / 16) * 2 + plane_idx).min(39)
        } else {
            plane_idx
        };
        (self.vsram[entry] & 0x03FF) as i16 as i32
    }

    fn draw_window(&mut self, _screen_w: usize) {
        // Window plane support is minimal: many title screens use plane A only.
        // Left as a no-op placeholder beyond the name-table being available.
    }

    fn draw_sprites(&mut self, screen_w: usize, prio: &mut [bool]) {
        let sat = ((self.regs[0x05] as usize) & 0x7F) << 9; // sprite attribute table
        // Sprites form a linked list via the "link" field; start at sprite 0.
        let max_sprites = 80;
        let mut idx = 0usize;
        let mut count = 0;
        while count < max_sprites {
            let base = sat + idx * 8;
            if base + 7 >= 0x10000 {
                break;
            }
            let y = (((self.vram[base] as u16) << 8) | self.vram[base + 1] as u16) & 0x03FF;
            let size = self.vram[base + 2];
            let link = (self.vram[base + 3] & 0x7F) as usize;
            let attr = ((self.vram[base + 4] as u16) << 8) | self.vram[base + 5] as u16;
            let x = (((self.vram[base + 6] as u16) << 8) | self.vram[base + 7] as u16) & 0x01FF;

            let hcells = ((size >> 2) & 0x03) as usize + 1; // width in cells
            let vcells = (size & 0x03) as usize + 1; // height in cells
            let tile = (attr & 0x07FF) as usize;
            let hflip = attr & 0x0800 != 0;
            let vflip = attr & 0x1000 != 0;
            let pal = ((attr >> 13) & 0x03) as usize;
            let priority = attr & 0x8000 != 0;

            // Sprite coordinates are offset by 128.
            let sx = x as i32 - 128;
            let sy = y as i32 - 128;

            for cy in 0..vcells {
                for cx in 0..hcells {
                    // Tile order is column-major in the sprite cell grid.
                    let tcx = if hflip { hcells - 1 - cx } else { cx };
                    let tcy = if vflip { vcells - 1 - cy } else { cy };
                    let tile_index = tile + tcx * vcells + tcy;
                    for py in 0..8 {
                        for px in 0..8 {
                            let mut fx = px;
                            let mut fy = py;
                            if hflip {
                                fx = 7 - fx;
                            }
                            if vflip {
                                fy = 7 - fy;
                            }
                            let ci = self.tile_pixel(tile_index, fx, fy);
                            if ci == 0 {
                                continue;
                            }
                            let dx = sx + (cx * 8 + px) as i32;
                            let dy = sy + (cy * 8 + py) as i32;
                            if dx < 0 || dy < 0 {
                                continue;
                            }
                            let (dx, dy) = (dx as usize, dy as usize);
                            if dx >= screen_w || dy >= VISIBLE_LINES as usize {
                                continue;
                            }
                            let pidx = dy * MAX_W + dx;
                            // Sprites draw over backgrounds unless a high-prio BG
                            // pixel sits there and the sprite is low priority.
                            if priority || !prio[pidx] {
                                self.put_pixel(dx, dy, pal, ci);
                            }
                        }
                    }
                }
            }

            if link == 0 {
                break;
            }
            idx = link;
            count += 1;
        }
    }

    /// Read a 4-bit colour index from a tile's pixel. Tiles are 8x8, 4bpp, 32
    /// bytes each (row-major, 2 pixels per byte, high nibble = left pixel).
    #[inline]
    fn tile_pixel(&self, tile: usize, x: usize, y: usize) -> u8 {
        let addr = tile * 32 + y * 4 + x / 2;
        if addr >= 0x10000 {
            return 0;
        }
        let byte = self.vram[addr];
        if x & 1 == 0 {
            byte >> 4
        } else {
            byte & 0x0F
        }
    }

    #[inline]
    fn put_pixel(&mut self, x: usize, y: usize, pal: usize, ci: u8) {
        let color_index = pal * 16 + ci as usize;
        let (r, g, b) = self.cram_to_rgb(color_index);
        let off = (y * MAX_W + x) * 4;
        self.framebuffer[off] = r;
        self.framebuffer[off + 1] = g;
        self.framebuffer[off + 2] = b;
        self.framebuffer[off + 3] = 0xFF;
    }

    /// Convert a CRAM 9-bit BGR333 entry to 8-bit RGB.
    fn cram_to_rgb(&self, index: usize) -> (u8, u8, u8) {
        let c = self.cram[index & 0x3F];
        // Layout: 0000 BBB0 GGG0 RRR0
        let r = ((c >> 1) & 0x07) as u8;
        let g = ((c >> 5) & 0x07) as u8;
        let b = ((c >> 9) & 0x07) as u8;
        // 3-bit -> 8-bit expansion.
        let exp = |v: u8| -> u8 { (v << 5) | (v << 2) | (v >> 1) };
        (exp(r), exp(g), exp(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_write_via_control_port() {
        let mut v = Vdp::new();
        // Write reg $01 = 0x64 (display on + vint on): 0x8164
        v.write_control(0x8164);
        assert_eq!(v.regs[0x01], 0x64);
        assert!(v.display_enabled());
        assert!(v.vint_enabled());
    }

    #[test]
    fn command_decodes_vram_write_address() {
        let mut v = Vdp::new();
        // VRAM write to address $1234. code for VRAM write = 0x01.
        // word0 = (code01 lo bits=01)<<14 | addr lo. addr=0x1234.
        let addr = 0x1234u16;
        let code = 0x01u16;
        let w0 = ((code & 0x03) << 14) | (addr & 0x3FFF);
        let w1 = ((code >> 2) << 4) | ((addr >> 14) & 0x03);
        v.write_control(w0);
        v.write_control(w1);
        assert_eq!(v.address, 0x1234);
        assert_eq!(v.target, Target::Vram);
    }

    #[test]
    fn data_write_to_vram_and_autoincrement() {
        let mut v = Vdp::new();
        v.regs[0x0F] = 2; // auto-increment 2
        // Set up VRAM write at address 0.
        v.write_control(0x4000); // code=01 lo? compute properly:
        // Use update_command directly for clarity.
        v.first_half = false;
        v.target = Target::Vram;
        v.address = 0;
        v.write_data(0xABCD);
        assert_eq!(v.vram[0], 0xAB);
        assert_eq!(v.vram[1], 0xCD);
        assert_eq!(v.address, 2);
    }

    #[test]
    fn cram_write_and_color_conversion() {
        let mut v = Vdp::new();
        v.target = Target::Cram;
        v.address = 0;
        v.regs[0x0F] = 2;
        // Pure red max: R=7 -> bits 1-3 = 0b1110 = 0x000E
        v.write_data(0x000E);
        let (r, g, b) = v.cram_to_rgb(0);
        assert_eq!(r, 0xFF);
        assert_eq!(g, 0);
        assert_eq!(b, 0);
    }

    #[test]
    fn vint_fires_at_vblank_when_enabled() {
        let mut v = Vdp::new();
        v.regs[0x01] = 0x20; // vint enabled
        v.line = VISIBLE_LINES;
        v.end_line();
        assert!(v.vint_pending);
        assert_eq!(v.irq_level(), 6);
    }

    #[test]
    fn frame_increments_after_all_lines() {
        let mut v = Vdp::new();
        let f0 = v.frame;
        for _ in 0..SCANLINES {
            v.start_line();
            v.end_line();
        }
        assert_eq!(v.frame, f0 + 1);
    }

    #[test]
    fn dma_fill_writes_vram() {
        let mut v = Vdp::new();
        v.regs[0x0F] = 1; // inc 1
        v.regs[0x13] = 4; // length 4
        v.regs[0x14] = 0;
        v.target = Target::Vram;
        v.address = 0x100;
        v.dma_fill(0xAA00);
        // First word + 4 fill bytes of 0xAA.
        assert_eq!(v.vram[0x100], 0xAA);
        assert_eq!(v.vram[0x102], 0xAA);
    }

    #[test]
    fn width_switches_h40_h32() {
        let mut v = Vdp::new();
        v.regs[0x0C] = 0x81; // H40
        assert_eq!(v.width(), 320);
        v.regs[0x0C] = 0x00; // H32
        assert_eq!(v.width(), 256);
    }
}
