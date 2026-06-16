import MetalFX
import MetalKit
import SwiftUI

// Uniforms shared with the fragment shader. Field order/layout must match the
// `Uniforms` struct in `shaderSource` below.
private struct ScreenUniforms {
    var rectOrigin = SIMD2<Float>(0, 0) // game area within the drawable (0…1)
    var rectSize = SIMD2<Float>(1, 1)
    var sourceSize = SIMD2<Float>(1, 1) // game texture size in pixels
    var outputSize = SIMD2<Float>(1, 1) // drawable size in pixels
    var effect: Int32 = 0               // 0 none,1 scanlines,2 crt,3 curved,4 lcd
    var flags: Int32 = 0                // reserved
    var curvature: Float = 0.10
    var scanline: Float = 0.30
    var mask: Float = 0.18
    var vignette: Float = 0.30
}

// Embedded Metal shaders: a fullscreen triangle, then a fragment that maps the
// drawable to the game area and applies the selected retro effect (curved CRT
// with scanlines + aperture mask + vignette, flat CRT, scanlines, or LCD grid).
private let shaderSource = """
#include <metal_stdlib>
using namespace metal;

struct Uniforms {
    float2 rectOrigin;
    float2 rectSize;
    float2 sourceSize;
    float2 outputSize;
    int effect;
    int flags;
    float curvature;
    float scanline;
    float mask;
    float vignette;
};

struct VSOut { float4 pos [[position]]; float2 uv; };

vertex VSOut screen_vtx(uint vid [[vertex_id]]) {
    float2 p[3] = { float2(-1.0, -1.0), float2(3.0, -1.0), float2(-1.0, 3.0) };
    VSOut o;
    o.pos = float4(p[vid], 0.0, 1.0);
    float2 uv = p[vid] * 0.5 + 0.5;
    o.uv = float2(uv.x, 1.0 - uv.y); // flip Y to image space
    return o;
}

fragment float4 screen_frag(VSOut in [[stage_in]],
                            texture2d<float> tex [[texture(0)]],
                            sampler smp [[sampler(0)]],
                            constant Uniforms& u [[buffer(0)]]) {
    // Map drawable uv -> game uv (0..1 over the centred game area).
    float2 g = (in.uv - u.rectOrigin) / u.rectSize;

    // Barrel-distort for the curved tube.
    if (u.effect == 3) {
        float2 c = g * 2.0 - 1.0;            // -1..1
        c += c * (dot(c, c)) * u.curvature;  // pull the corners outward
        g = c * 0.5 + 0.5;
    }

    // Outside the (warped) screen = black bezel.
    if (g.x < 0.0 || g.x > 1.0 || g.y < 0.0 || g.y > 1.0) {
        return float4(0.0, 0.0, 0.0, 1.0);
    }

    float3 col = tex.sample(smp, g).rgb;

    // Scanlines (1 scanlines, 2 crt, 3 curved).
    if (u.effect == 1 || u.effect == 2 || u.effect == 3) {
        float s = 0.5 + 0.5 * cos(g.y * u.sourceSize.y * 6.2831853);
        col *= mix(1.0 - u.scanline, 1.0, s);
    }
    // Aperture/shadow mask in screen space (2 crt, 3 curved).
    if (u.effect == 2 || u.effect == 3) {
        int col3 = int(in.uv.x * u.outputSize.x) % 3;
        float3 m = float3(1.0 - u.mask);
        m[col3] = 1.0;
        col *= m;
        // Vignette toward the edges.
        float2 d = g - 0.5;
        col *= 1.0 - u.vignette * dot(d, d) * 2.2;
    }
    // LCD dot-matrix grid (4).
    if (u.effect == 4) {
        float2 px = fract(g * u.sourceSize);
        float grid = step(0.12, px.x) * step(0.12, px.y);
        col *= mix(0.78, 1.0, grid);
    }

    return float4(col, 1.0);
}
"""

/// Metal-backed game screen. The render loop hands raw RGBA frames to
/// `updateFrame`; we upload them to a texture and draw a fullscreen pass with
/// the retro-effect shader. Optionally upscales via MetalFX first.
final class MetalScreenView: MTKView, MTKViewDelegate {
    private let queue: MTLCommandQueue
    private var pipeline: MTLRenderPipelineState!
    private var nearest: MTLSamplerState!
    private var linear: MTLSamplerState!

    private var source: MTLTexture?       // current game frame
    private var sourceSize = CGSize.zero

    // Config (set by `setVideo`).
    private var smooth = false
    private var integer = false
    private var effect: AppSettings.VideoEffect = .none
    private var useMetalFX = false

    // MetalFX spatial upscaler (lazily (re)built for the current source size).
    private var fx: Any?                   // MTLFXSpatialScaler (typed via #available)
    private var fxOutput: MTLTexture?
    private var fxInputSize = CGSize.zero

    init() {
        guard let dev = MTLCreateSystemDefaultDevice(),
              let q = dev.makeCommandQueue() else { fatalError("Metal is required") }
        queue = q
        super.init(frame: .zero, device: dev)
        colorPixelFormat = .bgra8Unorm
        framebufferOnly = true
        isPaused = true                    // we drive draws from the render loop
        enableSetNeedsDisplay = false
        autoResizeDrawable = true
        delegate = self
        layer?.isOpaque = true
        _ = buildPipeline(dev)             // best-effort; draw() guards on pipeline
    }

    required init(coder: NSCoder) { fatalError("not used") }

    @discardableResult
    private func buildPipeline(_ dev: MTLDevice) -> Bool {
        do {
            let lib = try dev.makeLibrary(source: shaderSource, options: nil)
            let desc = MTLRenderPipelineDescriptor()
            desc.vertexFunction = lib.makeFunction(name: "screen_vtx")
            desc.fragmentFunction = lib.makeFunction(name: "screen_frag")
            desc.colorAttachments[0].pixelFormat = colorPixelFormat
            pipeline = try dev.makeRenderPipelineState(descriptor: desc)
        } catch {
            NSLog("MetalScreen: pipeline build failed: \(error)")
            return false
        }
        let mk: (MTLSamplerMinMagFilter) -> MTLSamplerState? = { f in
            let s = MTLSamplerDescriptor()
            s.minFilter = f; s.magFilter = f
            s.sAddressMode = .clampToEdge; s.tAddressMode = .clampToEdge
            return dev.makeSamplerState(descriptor: s)
        }
        guard let n = mk(.nearest), let l = mk(.linear) else { return false }
        nearest = n; linear = l
        return true
    }

    /// Apply video settings (filter, integer snapping, effect, MetalFX).
    func setVideo(smooth: Bool, integer: Bool, effect: AppSettings.VideoEffect, metalFX: Bool) {
        self.smooth = smooth
        self.integer = integer
        self.effect = effect
        self.useMetalFX = metalFX
        draw() // redraw with the new look even if paused
    }

    /// Upload the latest RGBA8888 frame and draw it.
    func updateFrame(_ bytes: UnsafeRawPointer, width: Int, height: Int) {
        guard let dev = device else { return }
        if source == nil || Int(sourceSize.width) != width || Int(sourceSize.height) != height {
            let d = MTLTextureDescriptor.texture2DDescriptor(
                pixelFormat: .rgba8Unorm, width: width, height: height, mipmapped: false)
            d.usage = [.shaderRead]
            source = dev.makeTexture(descriptor: d)
            sourceSize = CGSize(width: width, height: height)
        }
        source?.replace(
            region: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0,
            withBytes: bytes, bytesPerRow: width * 4)
        draw()
    }

    // MTKViewDelegate
    func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}

    func draw(in view: MTKView) {
        guard let pipeline,
              let src = source,
              let drawable = currentDrawable,
              let pass = currentRenderPassDescriptor,
              let cmd = queue.makeCommandBuffer()
        else { return }

        // Pick the texture the shader samples: MetalFX-upscaled, or the source.
        let sampleTex = metalFXOutput(for: src, in: cmd) ?? src

        var u = uniforms(for: sampleTex)
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 1)

        if let enc = cmd.makeRenderCommandEncoder(descriptor: pass) {
            enc.setRenderPipelineState(pipeline)
            enc.setFragmentTexture(sampleTex, index: 0)
            enc.setFragmentSamplerState(smooth ? linear : nearest, index: 0)
            enc.setFragmentBytes(&u, length: MemoryLayout<ScreenUniforms>.stride, index: 0)
            enc.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
            enc.endEncoding()
        }
        cmd.present(drawable)
        cmd.commit()
    }

    private func uniforms(for sampleTex: MTLTexture) -> ScreenUniforms {
        var u = ScreenUniforms()
        let out = CGSize(width: bounds.width * (window?.backingScaleFactor ?? 2),
                         height: bounds.height * (window?.backingScaleFactor ?? 2))
        u.outputSize = SIMD2(Float(out.width), Float(out.height))
        u.sourceSize = SIMD2(Float(sampleTex.width), Float(sampleTex.height))
        u.effect = effect.shaderCode
        if effect != .crtCurved { u.curvature = 0 }

        // Fit the game into the drawable: integer-snapped or aspect-fit.
        let sw = sourceSize.width, sh = sourceSize.height
        guard sw > 0, sh > 0, out.width > 0, out.height > 0 else { return u }
        var dispW: CGFloat, dispH: CGFloat
        if integer {
            let k = max(1, floor(min(out.width / sw, out.height / sh)))
            dispW = sw * k; dispH = sh * k
        } else {
            let srcAspect = sw / sh
            if out.width / out.height > srcAspect {
                dispH = out.height; dispW = out.height * srcAspect
            } else {
                dispW = out.width; dispH = out.width / srcAspect
            }
        }
        let rsx = Float(dispW / out.width), rsy = Float(dispH / out.height)
        u.rectSize = SIMD2(rsx, rsy)
        u.rectOrigin = SIMD2((1 - rsx) / 2, (1 - rsy) / 2)
        return u
    }

    // Build/run a MetalFX spatial upscale to 3× when enabled. Returns nil to use
    // the source unchanged (disabled, unsupported, or on any failure).
    private func metalFXOutput(for src: MTLTexture, in cmd: MTLCommandBuffer) -> MTLTexture? {
        guard useMetalFX, let dev = device else { return nil }
        if #available(macOS 13.0, *) {
            let outW = src.width * 3, outH = src.height * 3
            if fx == nil || fxInputSize != sourceSize || fxOutput?.width != outW {
                let d = MTLFXSpatialScalerDescriptor()
                d.inputWidth = src.width; d.inputHeight = src.height
                d.outputWidth = outW; d.outputHeight = outH
                d.colorTextureFormat = .rgba8Unorm
                d.outputTextureFormat = .rgba8Unorm
                d.colorProcessingMode = .perceptual
                guard let scaler = d.makeSpatialScaler(device: dev) else { return nil }
                fx = scaler
                fxInputSize = sourceSize
                let td = MTLTextureDescriptor.texture2DDescriptor(
                    pixelFormat: .rgba8Unorm, width: outW, height: outH, mipmapped: false)
                td.usage = [.shaderRead, .renderTarget]
                td.storageMode = .private
                fxOutput = dev.makeTexture(descriptor: td)
            }
            guard let scaler = fx as? MTLFXSpatialScaler, let outTex = fxOutput else { return nil }
            scaler.colorTexture = src
            scaler.outputTexture = outTex
            scaler.encode(commandBuffer: cmd)
            return outTex
        }
        return nil
    }
}

/// SwiftUI wrapper that hands the Metal view to the hub's render loop and keeps
/// the current video settings applied.
struct ScreenView: NSViewRepresentable {
    let hub: EmuHub
    @EnvironmentObject var settings: AppSettings

    func makeNSView(context: Context) -> MetalScreenView {
        let v = MetalScreenView()
        hub.attach(screen: v)
        apply(v)
        return v
    }

    func updateNSView(_ nsView: MetalScreenView, context: Context) {
        apply(nsView)
    }

    private func apply(_ v: MetalScreenView) {
        v.setVideo(smooth: settings.upscale.smoothFilter,
                   integer: settings.upscale.integer,
                   effect: settings.videoEffect,
                   metalFX: settings.upscale == .metalfx)
    }
}
