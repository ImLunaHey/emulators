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
        // AVAudioEngine nodes require the *standard* (non-interleaved float)
        // format — an interleaved format fails connect() with -10868. The core
        // hands us interleaved samples, so we de-interleave in the render block.
        guard let fmt = AVAudioFormat(standardFormatWithSampleRate: sampleRate,
                                      channels: AVAudioChannelCount(channels))
        else { return nil }

        source = AVAudioSourceNode(format: fmt) { [weak self] _, _, frameCount, ablPtr -> OSStatus in
            let abl = UnsafeMutableAudioBufferListPointer(ablPtr)
            guard let self = self else { return noErr }
            let frames = Int(frameCount)
            self.lock.lock()
            let availFrames = (self.ring.count - self.readIdx) / self.channels
            let n = min(frames, max(0, availFrames))
            for ch in 0..<self.channels where ch < abl.count {
                guard let dst = abl[ch].mData?.assumingMemoryBound(to: Float.self) else { continue }
                for f in 0..<n { dst[f] = self.ring[self.readIdx + f * self.channels + ch] }
                for f in n..<frames { dst[f] = 0 } // underrun -> silence
            }
            self.readIdx += n * self.channels
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

    /// Output volume (0...1), applied at the engine's main mixer.
    var volume: Float {
        get { engine.mainMixerNode.outputVolume }
        set { engine.mainMixerNode.outputVolume = max(0, min(1, newValue)) }
    }

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
