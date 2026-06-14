//! DS memory foundation: shared RAM, both CPU buses, the VRAM bank router,
//! and the memory-map constants. Ported from ../../ds-recomp/src/memory/.

pub mod bus7;
pub mod bus9;
pub mod regions;
pub mod shared;
pub mod vram_router;

pub use bus7::Bus7;
pub use bus9::Bus9;
pub use shared::{SharedMemory, WramCnt};
pub use vram_router::VramRouter;
