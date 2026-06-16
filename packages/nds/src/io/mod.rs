//! DS IO subsystems (DMA, math accelerator, IPC/FIFO, IRQ, RTC, sound, SPI,
//! timers, touch). Stubs pre-declared here; each lands in its own file so
//! parallel porting agents don't race on this module list.

pub mod dma;
pub mod ds_math;
pub mod ipc;
pub mod irq;
pub mod rtc;
pub mod sound;
pub mod spi;
pub mod timers;
pub mod touch;
