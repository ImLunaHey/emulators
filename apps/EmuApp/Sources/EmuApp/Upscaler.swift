import Foundation

/// CPU pixel-art upscalers (Scale2x / Scale3x, the "EPX" family) applied to the
/// RGBA8888 framebuffer before display. They interpolate along edges using only
/// exact-color matches, so sprites/text get cleaner diagonals than nearest
/// without the blur of a linear filter. A reusable output buffer avoids
/// per-frame allocations.
final class Upscaler {
    private var src: [UInt32] = []
    private var out: [UInt32] = []

    /// Upscale `pixels` (w×h RGBA8888) by `factor` (2 or 3). Returns a borrowed
    /// view of the internal output buffer + the scaled dimensions, or nil to use
    /// the source unchanged. The returned buffer is valid until the next call.
    func scale(_ pixels: UnsafePointer<UInt8>, w: Int, h: Int, factor: Int) -> (UnsafeBufferPointer<UInt32>, Int, Int)? {
        guard (factor == 2 || factor == 3), w > 0, h > 0 else { return nil }

        let count = w * h
        if src.count != count { src = [UInt32](repeating: 0, count: count) }
        src.withUnsafeMutableBytes { _ = memcpy($0.baseAddress!, pixels, count * 4) }

        let ow = w * factor
        let oh = h * factor
        if out.count != ow * oh { out = [UInt32](repeating: 0, count: ow * oh) }

        if factor == 2 {
            scale2x(w: w, h: h, ow: ow)
        } else {
            scale3x(w: w, h: h, ow: ow)
        }

        let buf = out.withUnsafeBufferPointer { $0 }
        return (buf, ow, oh)
    }

    // Clamped neighbour fetch (edges duplicate the centre pixel).
    @inline(__always) private func at(_ x: Int, _ y: Int, _ w: Int, _ h: Int) -> UInt32 {
        src[min(max(y, 0), h - 1) * w + min(max(x, 0), w - 1)]
    }

    private func scale2x(w: Int, h: Int, ow: Int) {
        out.withUnsafeMutableBufferPointer { o in
            for y in 0..<h {
                for x in 0..<w {
                    let e = at(x, y, w, h)
                    let b = at(x, y - 1, w, h)
                    let d = at(x - 1, y, w, h)
                    let f = at(x + 1, y, w, h)
                    let hh = at(x, y + 1, w, h)
                    var e0 = e, e1 = e, e2 = e, e3 = e
                    if d == b && d != hh && b != f { e0 = d }
                    if b == f && b != d && f != hh { e1 = f }
                    if d == hh && d != b && hh != f { e2 = d }
                    if hh == f && hh != d && f != b { e3 = f }
                    let oy = y * 2, ox = x * 2
                    o[oy * ow + ox] = e0
                    o[oy * ow + ox + 1] = e1
                    o[(oy + 1) * ow + ox] = e2
                    o[(oy + 1) * ow + ox + 1] = e3
                }
            }
        }
    }

    private func scale3x(w: Int, h: Int, ow: Int) {
        out.withUnsafeMutableBufferPointer { o in
            for y in 0..<h {
                for x in 0..<w {
                    let a = at(x - 1, y - 1, w, h), b = at(x, y - 1, w, h), c = at(x + 1, y - 1, w, h)
                    let d = at(x - 1, y, w, h), e = at(x, y, w, h), f = at(x + 1, y, w, h)
                    let g = at(x - 1, y + 1, w, h), hh = at(x, y + 1, w, h), i = at(x + 1, y + 1, w, h)

                    var e0 = e, e1 = e, e2 = e, e3 = e, e5 = e, e6 = e, e7 = e, e8 = e
                    if d == b && b != f && d != hh { e0 = d }
                    if (d == b && b != f && d != hh && e != c) || (b == f && b != d && f != hh && e != a) { e1 = b }
                    if b == f && b != d && f != hh { e2 = f }
                    if (d == b && b != f && d != hh && e != g) || (d == hh && d != b && hh != f && e != a) { e3 = d }
                    if (b == f && b != d && f != hh && e != i) || (hh == f && hh != d && f != b && e != c) { e5 = f }
                    if d == hh && d != b && hh != f { e6 = d }
                    if (d == hh && d != b && hh != f && e != i) || (hh == f && hh != d && f != b && e != g) { e7 = hh }
                    if hh == f && hh != d && f != b { e8 = f }

                    let oy = y * 3, ox = x * 3
                    o[oy * ow + ox] = e0; o[oy * ow + ox + 1] = e1; o[oy * ow + ox + 2] = e2
                    o[(oy + 1) * ow + ox] = e3; o[(oy + 1) * ow + ox + 1] = e; o[(oy + 1) * ow + ox + 2] = e5
                    o[(oy + 2) * ow + ox] = e6; o[(oy + 2) * ow + ox + 1] = e7; o[(oy + 2) * ow + ox + 2] = e8
                }
            }
        }
    }
}
