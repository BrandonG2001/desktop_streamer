//! H.264 / HEVC bitstream helpers.
//!
//! Encoders emit **Annex-B** byte streams (`00 00 01` / `00 00 00 01` start codes).  When the
//! headset reports `SharedMobileSettings.UseAnnexB == false`, the original video channel
//! (`_009D._0004._0001(ref header, IntPtr, bool)`) rewrites every start code into a 4-byte
//! big-endian NAL length ("AVCC"/"HVCC" style) and updates `FrameHeader.payload_size`.
//!
//! This module also provides access-unit splitting, which the command-line FFmpeg backend
//! needs to cut a raw elementary stream into frames.

/// Codec family of an elementary stream (AV1 OBU streams are passed through untouched).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bitstream {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    Hevc,
    /// AV1 (low-overhead OBU format; no start codes).
    Av1,
}

/// Finds the next Annex-B start code at or after `from`.
///
/// # Arguments
/// * `data` - byte stream.
/// * `from` - search start offset.
///
/// # Returns
/// `Some((start, len))` where `start` is the offset of the first zero byte of the start code
/// and `len` is 3 or 4, or `None` if no start code follows.
pub fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                if i > from && data[i - 1] == 0 {
                    return Some((i - 1, 4));
                }
                return Some((i, 3));
            }
            if data[i + 2] == 0 && i + 4 <= data.len() && data[i + 3] == 1 {
                return Some((i, 4));
            }
        }
        i += 1;
    }
    None
}

/// Splits an Annex-B buffer into NAL unit payloads (start codes removed).
///
/// Leading bytes before the first start code are ignored, and trailing zero bytes of each NAL
/// unit (`trailing_zero_8bits`) are stripped.
///
/// # Arguments
/// * `data` - Annex-B buffer.
///
/// # Returns
/// Slices of each NAL unit (header byte(s) included).
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let Some((first, len)) = find_start_code(data, 0) else {
        return out;
    };
    let mut start = first + len;
    loop {
        match find_start_code(data, start) {
            Some((next, next_len)) => {
                let mut end = next;
                while end > start && data[end - 1] == 0 {
                    end -= 1;
                }
                if end > start {
                    out.push(&data[start..end]);
                }
                start = next + next_len;
            }
            None => {
                if start < data.len() {
                    out.push(&data[start..]);
                }
                return out;
            }
        }
    }
}

/// Converts Annex-B into 4-byte big-endian length-prefixed NAL units.
///
/// # Arguments
/// * `data` - Annex-B buffer.
///
/// # Returns
/// The length-prefixed buffer (empty if no start code was found).
pub fn annex_b_to_length_prefixed(data: &[u8]) -> Vec<u8> {
    let nals = split_annex_b(data);
    let mut out = Vec::with_capacity(data.len() + nals.len());
    for nal in nals {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// Converts 4-byte length-prefixed NAL units back into Annex-B (4-byte start codes).
///
/// # Arguments
/// * `data` - length-prefixed buffer.
///
/// # Returns
/// `Some(annex_b)` or `None` if a length runs past the end of the buffer.
pub fn length_prefixed_to_annex_b(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i + 4 <= data.len() {
        let len = u32::from_be_bytes(data[i..i + 4].try_into().ok()?) as usize;
        i += 4;
        let nal = data.get(i..i + len)?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        i += len;
    }
    (i == data.len()).then_some(out)
}

/// Returns the NAL unit type of a NAL (header included).
///
/// # Arguments
/// * `codec` - bitstream family.
/// * `nal` - NAL unit bytes without start code.
///
/// # Returns
/// The type, or `None` for an empty NAL or AV1.
pub fn nal_type(codec: Bitstream, nal: &[u8]) -> Option<u8> {
    let b = *nal.first()?;
    match codec {
        Bitstream::H264 => Some(b & 0x1F),
        Bitstream::Hevc => Some((b >> 1) & 0x3F),
        Bitstream::Av1 => None,
    }
}

/// Reports whether a NAL unit is part of a random-access (key) picture.
///
/// # Arguments
/// * `codec` - bitstream family.
/// * `nal` - NAL unit bytes.
///
/// # Returns
/// `true` for H.264 IDR (5) and HEVC IRAP (16..=21) slices.
pub fn is_keyframe_nal(codec: Bitstream, nal: &[u8]) -> bool {
    match (codec, nal_type(codec, nal)) {
        (Bitstream::H264, Some(5)) => true,
        (Bitstream::Hevc, Some(t)) => (16..=21).contains(&t),
        _ => false,
    }
}

/// Reports whether a NAL unit is a VCL (picture data) NAL whose slice is the first of a picture.
///
/// For H.264 this tests `first_mb_in_slice == 0` (first `ue(v)` bit is `1`); for HEVC it tests
/// `first_slice_segment_in_pic_flag`.
///
/// # Arguments
/// * `codec` - bitstream family.
/// * `nal` - NAL unit bytes.
///
/// # Returns
/// `true` if the NAL starts a new picture.
pub fn starts_picture(codec: Bitstream, nal: &[u8]) -> bool {
    match codec {
        Bitstream::H264 => {
            matches!(nal_type(codec, nal), Some(1..=5)) && nal.get(1).is_some_and(|b| b & 0x80 != 0)
        }
        Bitstream::Hevc => matches!(nal_type(codec, nal), Some(0..=31)) && nal.get(2).is_some_and(|b| b & 0x80 != 0),
        Bitstream::Av1 => false,
    }
}

/// Reports whether a NAL unit is an access-unit delimiter or parameter set (which belong to the
/// *next* picture when they appear after VCL data).
///
/// # Arguments
/// * `codec` - bitstream family.
/// * `nal` - NAL unit bytes.
///
/// # Returns
/// `true` for AUD/SPS/PPS (H.264 9/7/8) or AUD/VPS/SPS/PPS/prefix-SEI (HEVC 35/32/33/34/39).
pub fn is_au_prefix(codec: Bitstream, nal: &[u8]) -> bool {
    match (codec, nal_type(codec, nal)) {
        (Bitstream::H264, Some(t)) => matches!(t, 6..=9),
        (Bitstream::Hevc, Some(t)) => matches!(t, 32..=35 | 39),
        _ => false,
    }
}

/// Incremental Annex-B access-unit splitter for raw elementary streams read from a pipe.
#[derive(Debug)]
pub struct AccessUnitSplitter {
    codec: Bitstream,
    buf: Vec<u8>,
    /// Offset in `buf` where the current access unit begins.
    au_start: usize,
    /// Whether VCL data has been seen in the current access unit.
    seen_vcl: bool,
    /// Offset up to which `buf` has been scanned for start codes.
    scanned: usize,
}

impl AccessUnitSplitter {
    /// Creates a splitter.
    ///
    /// # Arguments
    /// * `codec` - H.264 or HEVC (AV1 is not supported by this splitter).
    ///
    /// # Returns
    /// The splitter.
    pub fn new(codec: Bitstream) -> Self {
        Self { codec, buf: Vec::new(), au_start: 0, seen_vcl: false, scanned: 0 }
    }

    /// Appends bytes and returns every access unit that is now complete.
    ///
    /// An access unit is complete when the first NAL of the *next* picture (prefix NAL or first
    /// slice) has been seen.  Returned buffers are Annex-B including their start codes.
    ///
    /// # Arguments
    /// * `data` - newly read bytes.
    ///
    /// # Returns
    /// Completed access units (possibly empty).
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut done = Vec::new();
        loop {
            let Some((sc, sc_len)) = find_start_code(&self.buf, self.scanned) else {
                self.scanned = self.buf.len().saturating_sub(3);
                break;
            };
            let nal_start = sc + sc_len;
            // Need at least 3 bytes of NAL to classify it.
            if self.buf.len() < nal_start + 3 {
                self.scanned = sc;
                break;
            }
            let nal = &self.buf[nal_start..(nal_start + 3).min(self.buf.len())];
            let boundary = self.seen_vcl && (is_au_prefix(self.codec, nal) || starts_picture(self.codec, nal));
            if boundary && sc > self.au_start {
                done.push(self.buf[self.au_start..sc].to_vec());
                self.au_start = sc;
                self.seen_vcl = false;
            }
            if matches!(nal_type(self.codec, nal), Some(t) if (self.codec == Bitstream::H264 && (1..=5).contains(&t)) || (self.codec == Bitstream::Hevc && t <= 31)) {
                self.seen_vcl = true;
            }
            self.scanned = nal_start;
        }
        if self.au_start > 0 {
            self.buf.drain(..self.au_start);
            self.scanned -= self.au_start.min(self.scanned);
            self.au_start = 0;
        }
        done
    }

    /// Returns any buffered (incomplete) access unit, e.g. at end of stream.
    ///
    /// # Returns
    /// The remaining bytes, or `None` if nothing is buffered.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.buf.is_empty() {
            return None;
        }
        self.au_start = 0;
        self.scanned = 0;
        self.seen_vcl = false;
        Some(std::mem::take(&mut self.buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NAL units never end in 0x00 (rbsp_trailing_bits), so the vectors end in non-zero bytes.
    const SPS: &[u8] = &[0x67, 0x42, 0x0A];
    const IDR: &[u8] = &[0x65, 0x88, 0x84];
    const P: &[u8] = &[0x41, 0x9A, 0x01];

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(n);
        }
        v
    }

    #[test]
    fn conversion_round_trip() {
        let a = annexb(&[SPS, IDR]);
        let lp = annex_b_to_length_prefixed(&a);
        assert_eq!(&lp[..4], &[0, 0, 0, 3]);
        assert_eq!(length_prefixed_to_annex_b(&lp).unwrap(), a);
    }

    #[test]
    fn splitter_cuts_pictures() {
        let mut s = AccessUnitSplitter::new(Bitstream::H264);
        let stream = annexb(&[SPS, IDR, P, P]);
        let mut aus = s.push(&stream);
        aus.extend(s.flush());
        assert_eq!(aus.len(), 3);
        assert!(split_annex_b(&aus[0]).iter().any(|n| is_keyframe_nal(Bitstream::H264, n)));
    }
}
