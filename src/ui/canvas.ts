export class CanvasView {
  ctx: CanvasRenderingContext2D;
  imageData: ImageData;

  constructor(public canvas: HTMLCanvasElement) {
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('2D context unavailable');
    this.ctx = ctx;
    this.imageData = ctx.createImageData(240, 160);
  }

  blit(frame: Uint8ClampedArray): void {
    this.imageData.data.set(frame);
    this.ctx.putImageData(this.imageData, 0, 0);
  }

  // Overlay text on top of the blitted frame using the 2D context.
  // When the game hasn't reached its first VRAM render yet (e.g. all-white
  // backdrop), draw a visible "boot screen" panel with live state so the
  // canvas conveys progress instead of looking dead.
  overlay(lines: string[], showSplash: boolean): void {
    this.ctx.save();
    if (showSplash) {
      // Dim the whole canvas first so the panel pops.
      this.ctx.fillStyle = 'rgba(10, 10, 14, 0.85)';
      this.ctx.fillRect(0, 0, 240, 160);
      // Logo block.
      this.ctx.fillStyle = '#9be7ff';
      this.ctx.font = 'bold 14px ui-monospace, SF Mono, Menlo, monospace';
      this.ctx.textBaseline = 'top';
      this.ctx.fillText('GBA-RECOMP', 12, 12);
      this.ctx.fillStyle = '#5a5a66';
      this.ctx.font = '7px ui-monospace, SF Mono, Menlo, monospace';
      this.ctx.fillText('hybrid wasm interp · arm7tdmi + thumb', 12, 28);
      // Separator.
      this.ctx.fillStyle = '#22222a';
      this.ctx.fillRect(12, 40, 216, 1);
      // Diagnostics panel.
      this.ctx.font = '7px ui-monospace, SF Mono, Menlo, monospace';
      this.ctx.fillStyle = '#9be7ff';
      for (let i = 0; i < lines.length; i++) {
        this.ctx.fillText(lines[i], 12, 48 + i * 10);
      }
    } else {
      // Compact strip when the game is rendering its own content.
      this.ctx.fillStyle = 'rgba(0, 0, 0, 0.72)';
      this.ctx.fillRect(2, 2, 236, lines.length * 11 + 6);
      this.ctx.font = '8px ui-monospace, SF Mono, Menlo, monospace';
      this.ctx.fillStyle = '#9be7ff';
      this.ctx.textBaseline = 'top';
      for (let i = 0; i < lines.length; i++) {
        this.ctx.fillText(lines[i], 6, 6 + i * 11);
      }
    }
    this.ctx.restore();
  }
}
