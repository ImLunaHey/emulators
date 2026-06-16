//! IE/IF/IME — interrupt controller. Ported 1:1 from src/io/irq.ts.

pub const IRQ_VBLANK: u32 = 1 << 0;
pub const IRQ_HBLANK: u32 = 1 << 1;
pub const IRQ_VCOUNT: u32 = 1 << 2;
pub const IRQ_TIMER0: u32 = 1 << 3;
pub const IRQ_TIMER1: u32 = 1 << 4;
pub const IRQ_TIMER2: u32 = 1 << 5;
pub const IRQ_TIMER3: u32 = 1 << 6;
pub const IRQ_SIO: u32 = 1 << 7;
pub const IRQ_DMA0: u32 = 1 << 8;
pub const IRQ_DMA1: u32 = 1 << 9;
pub const IRQ_DMA2: u32 = 1 << 10;
pub const IRQ_DMA3: u32 = 1 << 11;
pub const IRQ_KEYPAD: u32 = 1 << 12;
pub const IRQ_GAMEPAK: u32 = 1 << 13;

#[derive(Default)]
pub struct Irq {
    pub ie: u32,
    pub iflag: u32,
    pub ime: u32,
    /// Cached pending bit so the hot CPU loop doesn't recompute it every step.
    pub cached_pending: bool,
}

impl Irq {
    pub fn new() -> Self {
        Self::default()
    }

    fn recompute(&mut self) {
        self.cached_pending = (self.ime & 1) != 0 && (self.ie & self.iflag) != 0;
    }

    pub fn raise(&mut self, bits: u32) {
        self.iflag = (self.iflag | bits) & 0x3FFF;
        self.recompute();
    }

    pub fn set_ie(&mut self, v: u32) {
        self.ie = v & 0x3FFF;
        self.recompute();
    }

    pub fn set_ime(&mut self, v: u32) {
        self.ime = v & 1;
        self.recompute();
    }

    pub fn pending(&self) -> bool {
        self.cached_pending
    }

    /// Writes to IF clear the corresponding bits.
    pub fn ack_write16(&mut self, v: u32) {
        self.iflag &= !(v & 0x3FFF);
        self.recompute();
    }
}
