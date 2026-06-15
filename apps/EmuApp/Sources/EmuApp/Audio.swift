import AVFoundation

/// A small pull-based audio sink: the core pushes interleaved f32 samples each
/// frame; an AVAudioSourceNode pulls them at the device rate. Handles mono or
/// interleaved stereo at the core's native sample rate (the engine resamples to
/// the output device).
final class AudioPlayer {
    private let engine = AVAudioEngine()
    private var source: AVAudioSourceNode!
    private let lock = NSLock()
    private var ring: [Float] = []
    private var readIdx = 0
    private let channels: Int
    private let maxBacklog: Int

    init?(sampleRate: Double, channels: Int) {
        self.channels = channels
        self.maxBacklog = Int(sampleRate) * channels // ~1s cap to bound drift
        guard
            let fmt = AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: sampleRate,
                channels: AVAudioChannelCount(channels),
                interleaved: true
            )
        else { return nil }

        source = AVAudioSourceNode(format: fmt) { [weak self] _, _, frameCount, ablPtr -> OSStatus in
            let abl = UnsafeMutableAudioBufferListPointer(ablPtr)
            guard let self = self, let buf = abl.first,
                  let dst = buf.mData?.assumingMemoryBound(to: Float.self)
            else { return noErr }
            let need = Int(frameCount) * self.channels
            self.lock.lock()
            let available = self.ring.count - self.readIdx
            let n = min(need, max(0, available))
            for i in 0..<n { dst[i] = self.ring[self.readIdx + i] }
            for i in n..<need { dst[i] = 0 } // underrun -> silence
            self.readIdx += n
            if self.readIdx > 48_000 { // compact occasionally
                self.ring.removeFirst(self.readIdx)
                self.readIdx = 0
            }
            self.lock.unlock()
            return noErr
        }

        engine.attach(source)
        engine.connect(source, to: engine.mainMixerNode, format: fmt)
    }

    func start() { try? engine.start() }
    func stop() { engine.stop() }

    func enqueue(_ samples: ArraySlice<Float>) {
        lock.lock()
        if ring.count - readIdx > maxBacklog {
            // Falling behind (tab throttled / slow frame): drop the backlog.
            ring.removeAll(keepingCapacity: true)
            readIdx = 0
        }
        ring.append(contentsOf: samples)
        lock.unlock()
    }
}
