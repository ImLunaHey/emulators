using NAudio.Wave;

namespace EmuWin;

/// <summary>
/// Audio sink built on NAudio. The core emits interleaved f32 at its native
/// sample rate; we feed those straight to a buffered IEEE-float output (Windows
/// resamples to the device rate), applying volume.
/// </summary>
internal sealed class AudioOut : IDisposable
{
    private readonly WaveOutEvent _out = new();
    private readonly BufferedWaveProvider _provider;
    private float[] _scaled = new float[8192];
    private byte[] _bytes = new byte[8192 * 4];

    public float Volume = 1.0f;

    public AudioOut(int sampleRate, int channels)
    {
        if (channels < 1) channels = 1;
        var fmt = WaveFormat.CreateIeeeFloatWaveFormat(sampleRate, channels);
        _provider = new BufferedWaveProvider(fmt)
        {
            DiscardOnBufferOverflow = true,
            BufferDuration = TimeSpan.FromMilliseconds(200),
        };
        _out.Init(_provider);
        _out.Play();
    }

    /// <summary>Queue <paramref name="count"/> interleaved samples (volume-scaled).</summary>
    public void Push(float[] samples, int count)
    {
        if (count <= 0) return;
        if (_scaled.Length < count) _scaled = new float[count];
        if (_bytes.Length < count * 4) _bytes = new byte[count * 4];

        float v = Volume;
        for (int i = 0; i < count; i++) _scaled[i] = samples[i] * v;
        Buffer.BlockCopy(_scaled, 0, _bytes, 0, count * 4);
        _provider.AddSamples(_bytes, 0, count * 4);
    }

    public void Dispose()
    {
        _out.Stop();
        _out.Dispose();
    }
}
