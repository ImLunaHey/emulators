import CEmuNative
import Foundation

/// Swift wrapper over the `emu_*` C ABI (one opaque handle per session).
/// Not thread-safe; drive it from one thread (the render loop).
final class Emulator {
    private let handle: OpaquePointer
    let system: EmuSystem

    init?(system: EmuSystem) {
        guard let h = emu_new(system.rawValue) else { return nil }
        self.handle = h
        self.system = system
    }

    deinit {
        emu_free(handle)
    }

    @discardableResult
    func loadROM(_ data: Data) -> Bool {
        data.withUnsafeBytes { raw in
            emu_load_rom(handle, raw.bindMemory(to: UInt8.self).baseAddress, data.count)
        }
    }

    @discardableResult
    func loadBIOS(_ data: Data) -> Bool {
        data.withUnsafeBytes { raw in
            emu_load_bios(handle, raw.bindMemory(to: UInt8.self).baseAddress, data.count)
        }
    }

    func runFrame() { emu_run_frame(handle) }
    func setKeys(_ bits: UInt32) { emu_set_keys(handle, bits) }

    var width: Int { Int(emu_width(handle)) }
    var height: Int { Int(emu_height(handle)) }
    var frameCount: UInt32 { emu_frame_count(handle) }
    var sampleRate: Double { Double(emu_sample_rate(handle)) }
    var channels: Int { Int(emu_channels(handle)) }

    /// The current framebuffer as a borrowed pointer (valid until the next
    /// `runFrame`). Use inside the closure only.
    func withFramebuffer<R>(_ body: (UnsafePointer<UInt8>?, Int) -> R) -> R {
        let ptr = emu_framebuffer_ptr(handle)
        let len = emu_framebuffer_len(handle)
        return body(ptr, len)
    }

    /// Drain audio samples into `buffer`; returns the count written.
    func drainAudio(into buffer: inout [Float]) -> Int {
        buffer.withUnsafeMutableBufferPointer { buf in
            Int(emu_drain_audio(handle, buf.baseAddress, buf.count))
        }
    }
}
