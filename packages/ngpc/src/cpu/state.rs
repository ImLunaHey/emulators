//! TLCS-900/H register model + flags. Built from the Toshiba TLCS-900/H1 User's
//! Manual and the Mednafen/NeoPop register layout notes.
//!
//! REGISTER FILE. The TLCS-900/H has four register BANKS. Each bank holds four
//! 32-bit "general" registers XWA/XBC/XDE/XHL. The four pointer registers
//! XIX/XIY/XIZ/XSP are global (NOT banked). The active bank is selected by the
//! 2-bit RFP field in the Status Register.
//!
//! We model the whole file as a flat `[u32; 4*4 + 4]` array:
//!   indices 0..16  : bank0 XWA,XBC,XDE,XHL, bank1 …, bank2 …, bank3 …
//!   indices 16..20 : XIX, XIY, XIZ, XSP (global)
//!
//! Register CODES (the 8-bit code emitted by register-direct operands) index
//! this file. Per the manual the codes are byte-granular within a 256-byte
//! "register space"; the upper area (>= 0xE0) holds the current-bank shortcut
//! codes for WA/BC/DE/HL and the global IX/IY/IZ/SP, while the explicit
//! 0x00-based codes select absolute bank registers. We resolve a code to a
//! (file-index, byte-offset, size) triple in `reg_addr`.
//!
//! FLAGS (low byte of the Status Register), verified against the Mednafen NGPC
//! layout: S=bit7 Z=bit6 H=bit4 V/P=bit2 N=bit1 C=bit0.

pub const FLAG_C: u8 = 1 << 0;
pub const FLAG_N: u8 = 1 << 1;
pub const FLAG_V: u8 = 1 << 2; // overflow (arith) / parity (logical)
pub const FLAG_H: u8 = 1 << 4;
pub const FLAG_Z: u8 = 1 << 6;
pub const FLAG_S: u8 = 1 << 7;

/// File index of a global pointer register relative to the start of the global
/// area (which begins at file index 16).
pub const XIX_IDX: usize = 16;
pub const XIY_IDX: usize = 17;
pub const XIZ_IDX: usize = 18;
pub const XSP_IDX: usize = 19;

#[derive(Clone)]
pub struct Cpu {
    /// Flat register file: 4 banks × 4 general regs, then 4 global pointers.
    pub regs: [u32; 20],

    /// Program counter (24-bit used; stored in 32).
    pub pc: u32,

    /// Status register high byte: bits[6:4] = IFF/ILM interrupt mask level,
    /// bits[1:0] = RFP register-bank select.
    pub sr_hi: u8,
    /// Flag register (low byte of SR).
    pub f: u8,
    /// Alternate flag register (EX F,F').
    pub f_alt: u8,

    pub halted: bool,

    /// Level-sensitive interrupt request: highest pending interrupt level
    /// (0 = none). The bus owner raises this; the CPU compares it to ILM.
    pub int_request: u8,
    /// Vector address for the pending interrupt (where to load PC from).
    pub int_vector: u32,

    /// Set when an instruction decoded to an unimplemented/illegal opcode, so
    /// the host can latch a fault and paint a crash screen.
    pub illegal: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu::new()
    }
}

impl Cpu {
    pub fn new() -> Cpu {
        Cpu {
            regs: [0; 20],
            pc: 0,
            sr_hi: 0x70, // ILM = 7 (all maskable interrupts blocked), RFP = 0
            f: 0,
            f_alt: 0,
            halted: false,
            int_request: 0,
            int_vector: 0,
            illegal: false,
        }
    }

    /// Active register bank (RFP, 0-3).
    #[inline]
    pub fn rfp(&self) -> usize {
        (self.sr_hi & 0x03) as usize
    }

    #[inline]
    pub fn set_rfp(&mut self, bank: u8) {
        self.sr_hi = (self.sr_hi & !0x03) | (bank & 0x03);
    }

    /// Interrupt mask level (ILM, 0-7).
    #[inline]
    pub fn ilm(&self) -> u8 {
        (self.sr_hi >> 4) & 0x07
    }

    #[inline]
    pub fn set_ilm(&mut self, lvl: u8) {
        self.sr_hi = (self.sr_hi & !0x70) | ((lvl & 0x07) << 4);
    }

    /// The full 16-bit status register.
    #[inline]
    pub fn sr(&self) -> u16 {
        ((self.sr_hi as u16) << 8) | self.f as u16
    }

    #[inline]
    pub fn set_sr(&mut self, v: u16) {
        self.sr_hi = (v >> 8) as u8;
        self.f = (v & 0xFF) as u8;
    }

    // ---- current-bank general register access by short index 0..4 ----
    // 0=XWA 1=XBC 2=XDE 3=XHL within the active bank.
    #[inline]
    fn gen_index(&self, n: usize) -> usize {
        self.rfp() * 4 + n
    }

    #[inline]
    pub fn xwa(&self) -> u32 {
        self.regs[self.gen_index(0)]
    }
    #[inline]
    pub fn xsp(&self) -> u32 {
        self.regs[XSP_IDX]
    }
    #[inline]
    pub fn set_xsp(&mut self, v: u32) {
        self.regs[XSP_IDX] = v;
    }
    #[inline]
    pub fn pc(&self) -> u32 {
        self.pc & 0xFF_FFFF
    }

    /// Set the C flag from a boolean.
    #[inline]
    pub fn set_flag(&mut self, mask: u8, on: bool) {
        if on {
            self.f |= mask;
        } else {
            self.f &= !mask;
        }
    }
    #[inline]
    pub fn flag(&self, mask: u8) -> bool {
        self.f & mask != 0
    }
}

impl Cpu {
    /// Resolve an 8-bit register CODE to a (file-index, byte-offset-within-u32)
    /// pair. The TLCS-900 register-addressing codes (Toshiba manual): the low
    /// codes 0x00-0x3F address the four banks' general registers byte-by-byte
    /// (bank = code>>4, within-bank reg = (code>>2)&3, byte = code&3); the high
    /// codes 0xE0-0xFF are the current-bank / global shortcuts used by almost
    /// all real code:
    ///   0xE0..0xEF : current-bank general regs, byte granular
    ///                (E0=RA0=W? ) — we map E0..EF to current bank XWA..XHL bytes
    ///   0xF0..0xFF : global IX/IY/IZ/SP byte granular
    /// We return the file index and the byte offset; callers combine with the
    /// access size.
    fn reg_file(&self, code: u8) -> (usize, u32) {
        let c = code as usize;
        if c < 0x40 {
            // Absolute bank addressing: bank = bits[5:4], reg = bits[3:2],
            // byte = bits[1:0].
            let bank = (c >> 4) & 0x03;
            let reg = (c >> 2) & 0x03;
            let byte = (c & 0x03) as u32;
            (bank * 4 + reg, byte)
        } else if c >= 0xF0 {
            // Global pointer registers IX/IY/IZ/SP, byte granular.
            let which = (c >> 2) & 0x03; // 0=IX 1=IY 2=IZ 3=SP
            let byte = (c & 0x03) as u32;
            (XIX_IDX + which, byte)
        } else if c >= 0xE0 {
            // Current-bank general registers, byte granular.
            let reg = (c >> 2) & 0x03;
            let byte = (c & 0x03) as u32;
            (self.rfp() * 4 + reg, byte)
        } else {
            // 0x40-0xDF: previous-bank / extended — fall back to current bank,
            // treating it like the 0xE0 region. Rarely used by NGPC code.
            let reg = (c >> 2) & 0x03;
            let byte = (c & 0x03) as u32;
            (self.rfp() * 4 + reg, byte)
        }
    }

    /// Read a register by code at the given size. The byte-offset from the code
    /// selects the sub-register (e.g. A is byte0 of XWA, W is byte1, etc.).
    pub fn read_reg(&self, code: u8, size: Size) -> u32 {
        let (idx, byte) = self.reg_file(code);
        let v = self.regs[idx];
        match size {
            Size::Byte => (v >> (byte * 8)) & 0xFF,
            // Word reads take bytes [byte..byte+2]; for the canonical even codes
            // this is the low or high half.
            Size::Word => (v >> ((byte & 0x02) * 8)) & 0xFFFF,
            Size::Long => v,
        }
    }

    /// Write a register by code at the given size.
    pub fn write_reg(&mut self, code: u8, size: Size, val: u32) {
        let (idx, byte) = self.reg_file(code);
        let cur = self.regs[idx];
        self.regs[idx] = match size {
            Size::Byte => {
                let sh = byte * 8;
                (cur & !(0xFF << sh)) | ((val & 0xFF) << sh)
            }
            Size::Word => {
                let sh = (byte & 0x02) * 8;
                (cur & !(0xFFFF << sh)) | ((val & 0xFFFF) << sh)
            }
            Size::Long => val,
        };
    }
}

/// Operand width for the size-polymorphic instruction handlers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    Byte,
    Word,
    Long,
}

impl Size {
    #[inline]
    pub fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Long => 4,
        }
    }
    /// Sign bit mask for this width.
    #[inline]
    pub fn sign_mask(self) -> u32 {
        match self {
            Size::Byte => 0x80,
            Size::Word => 0x8000,
            Size::Long => 0x8000_0000,
        }
    }
    /// Value mask for this width.
    #[inline]
    pub fn mask(self) -> u32 {
        match self {
            Size::Byte => 0xFF,
            Size::Word => 0xFFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }
}
