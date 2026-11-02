//! Sample conversion shared by the audio back-ends.
//!
//! C#: the CSCore chain built by `LoopbackCapture` —
//! `SoundInSource → ChangeSampleRate(48000) → ToSampleSource → GainSource(0.9) → ToStereo →
//! ToWaveSource(16)` — reduced to plain functions: decode any capture format to interleaved
//! `f32`, convert channels to stereo, resample to 48 kHz, apply gain, encode `s16le`, and cut
//! the stream into 1152-byte packets.

/// Sample encoding of a captured buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// 32-bit IEEE float (the usual WASAPI mix format).
    F32,
    /// 16-bit signed integer.
    I16,
    /// 24-bit signed integer packed in 3 bytes.
    I24,
    /// 32-bit signed integer (or 24-bit in a 32-bit container, left-justified).
    I32,
}

impl SampleFormat {
    /// Bytes per sample.
    ///
    /// # Returns
    /// 2, 3 or 4.
    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::I16 => 2,
            SampleFormat::I24 => 3,
            SampleFormat::F32 | SampleFormat::I32 => 4,
        }
    }
}

/// Multiplies signed 16-bit little-endian samples by `gain` with saturation.
///
/// # Arguments
/// * `pcm` - samples, modified in place.
/// * `gain` - linear gain.
pub fn apply_gain_s16(pcm: &mut [u8], gain: f32) {
    if (gain - 1.0).abs() < f32::EPSILON {
        return;
    }
    for s in pcm.as_chunks_mut::<2>().0 {
        let v = f32::from(i16::from_le_bytes([s[0], s[1]])) * gain;
        s.copy_from_slice(&(v.clamp(-32768.0, 32767.0) as i16).to_le_bytes());
    }
}

/// Decodes interleaved samples to `f32` in `[-1, 1]`.
///
/// # Arguments
/// * `data` - raw bytes (trailing partial samples are ignored).
/// * `format` - sample encoding.
/// * `out` - destination (appended to).
pub fn decode_to_f32(data: &[u8], format: SampleFormat, out: &mut Vec<f32>) {
    let n = format.bytes();
    out.reserve(data.len() / n);
    for s in data.chunks_exact(n) {
        out.push(match format {
            SampleFormat::F32 => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            SampleFormat::I16 => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
            SampleFormat::I24 => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
            SampleFormat::I32 => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
        });
    }
}

/// Streaming converter: any channel count / sample rate → 48 kHz stereo `s16le` with gain.
#[derive(Debug)]
pub struct StereoResampler {
    in_rate: u32,
    in_channels: usize,
    gain: f32,
    /// Fractional read position into the current input block, in input frames.
    pos: f64,
    /// Last stereo frame of the previous block (for interpolation across blocks).
    prev: [f32; 2],
}

impl StereoResampler {
    /// Output sample rate.
    pub const OUT_RATE: u32 = 48_000;

    /// Creates a converter.
    ///
    /// # Arguments
    /// * `in_rate` - input sample rate.
    /// * `in_channels` - input channel count (≥ 1).
    /// * `gain` - linear gain applied to the output.
    ///
    /// # Returns
    /// The converter.
    pub fn new(in_rate: u32, in_channels: usize, gain: f32) -> Self {
        Self { in_rate: in_rate.max(1), in_channels: in_channels.max(1), gain, pos: 0.0, prev: [0.0; 2] }
    }

    fn frame(&self, samples: &[f32], i: usize) -> [f32; 2] {
        let c = self.in_channels;
        let f = &samples[i * c..i * c + c];
        if c == 1 { [f[0], f[0]] } else { [f[0], f[1]] }
    }

    /// Converts one block of interleaved `f32` input, appending `s16le` stereo output.
    ///
    /// # Arguments
    /// * `input` - interleaved input samples (whole frames).
    /// * `out` - destination bytes (appended to).
    pub fn process(&mut self, input: &[f32], out: &mut Vec<u8>) {
        let frames = input.len() / self.in_channels;
        if frames == 0 {
            return;
        }
        let step = f64::from(self.in_rate) / f64::from(Self::OUT_RATE);
        let gain = self.gain;
        let push = |l: f32, r: f32, out: &mut Vec<u8>| {
            for v in [l, r] {
                let s = (v * gain * 32767.0).clamp(-32768.0, 32767.0) as i16;
                out.extend_from_slice(&s.to_le_bytes());
            }
        };
        if self.in_rate == Self::OUT_RATE {
            for i in 0..frames {
                let [l, r] = self.frame(input, i);
                push(l, r, out);
            }
            self.prev = self.frame(input, frames - 1);
            return;
        }
        // Linear interpolation; position -1 refers to the last frame of the previous block.
        while self.pos < frames as f64 - 1.0 {
            let i = self.pos.floor();
            let t = (self.pos - i) as f32;
            let a = if i < 0.0 { self.prev } else { self.frame(input, i as usize) };
            let b = self.frame(input, (i + 1.0) as usize);
            push(a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, out);
            self.pos += step;
        }
        self.pos -= frames as f64;
        self.prev = self.frame(input, frames - 1);
    }
}

/// Cuts a byte stream into fixed-size packets.
#[derive(Debug, Default)]
pub struct Packetizer {
    size: usize,
    buf: Vec<u8>,
}

impl Packetizer {
    /// Creates a packetizer.
    ///
    /// # Arguments
    /// * `size` - packet size in bytes.
    ///
    /// # Returns
    /// The packetizer.
    pub fn new(size: usize) -> Self {
        Self { size: size.max(1), buf: Vec::with_capacity(size * 4) }
    }

    /// Appends bytes and returns every complete packet.
    ///
    /// # Arguments
    /// * `data` - new bytes.
    ///
    /// # Returns
    /// Complete packets in order.
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let n = self.buf.len() / self.size;
        let mut out = Vec::with_capacity(n);
        for chunk in self.buf.chunks_exact(self.size) {
            out.push(chunk.to_vec());
        }
        self.buf.drain(..n * self.size);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_saturates() {
        let mut pcm = 30000i16.to_le_bytes().to_vec();
        apply_gain_s16(&mut pcm, 2.0);
        assert_eq!(i16::from_le_bytes([pcm[0], pcm[1]]), 32767);
    }

    #[test]
    fn resamples_44k1_to_48k() {
        let mut r = StereoResampler::new(44_100, 2, 1.0);
        let input = vec![0.5f32; 44_100 * 2];
        let mut out = Vec::new();
        r.process(&input, &mut out);
        let frames = out.len() / 4;
        assert!((47_990..=48_000).contains(&frames), "{frames}");
        assert_eq!(i16::from_le_bytes([out[400], out[401]]), 16383);
    }

    #[test]
    fn mono_is_duplicated_and_packetized() {
        let mut r = StereoResampler::new(48_000, 1, 1.0);
        let mut out = Vec::new();
        r.process(&[0.25; 288], &mut out);
        let mut p = Packetizer::new(1152);
        let pkts = p.push(&out);
        assert_eq!(pkts.len(), 1);
        assert_eq!(&pkts[0][0..2], &pkts[0][2..4]);
    }

    #[test]
    fn decodes_formats() {
        let mut v = Vec::new();
        decode_to_f32(&(-32768i16).to_le_bytes(), SampleFormat::I16, &mut v);
        decode_to_f32(&[0x00, 0x00, 0x40], SampleFormat::I24, &mut v);
        assert_eq!(v, [-1.0, 0.5]);
    }
}
