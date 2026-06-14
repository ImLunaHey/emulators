// TODO: DMA (OAM DMA 0xFF46 + CGB HDMA/GDMA 0xFF51-0xFF55).
//
// Empty placeholder struct so the `Gbc` god-struct can own a slot; the porting
// agent for this file replaces it with the real subsystem state + behavior.

#[derive(Default)]
pub struct Dma;
