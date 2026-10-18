//! AV1 low-overhead bitstream (OBU) helpers for the raw `obu` muxer output.
//!
//! The original only produced AV1 with NVENC/AMF and sent each temporal unit as one frame.
//! FFmpeg's `obu` muxer writes OBUs with `obu_has_size_field = 1`, each temporal unit starting
//! with an `OBU_TEMPORAL_DELIMITER`, which this module uses to split the stream.

/// OBU type of a temporal delimiter.
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
/// OBU type of a sequence header (sent with every key frame by hardware encoders).
pub const OBU_SEQUENCE_HEADER: u8 = 1;

/// Reads a LEB128 value.
///
/// # Arguments
/// * `data` - bytes starting at the value.
///
/// # Returns
/// `(value, length)` or `None` if truncated / longer than 8 bytes.
pub fn read_leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, b) in data.iter().take(8).enumerate() {
        value |= u64::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// Parses the OBU at the start of `data`.
///
/// # Arguments
/// * `data` - bytes starting with an OBU header.
///
/// # Returns
/// `(obu type, total OBU length)` or `None` if incomplete or without a size field.
pub fn parse_obu(data: &[u8]) -> Option<(u8, usize)> {
    let header = *data.first()?;
    let ty = (header >> 3) & 0x0F;
    let has_ext = header & 0x04 != 0;
    let has_size = header & 0x02 != 0;
    if !has_size {
        return None;
    }
    let mut pos = 1 + usize::from(has_ext);
    let (size, n) = read_leb128(data.get(pos..)?)?;
    pos += n;
    let total = pos.checked_add(usize::try_from(size).ok()?)?;
    (data.len() >= total).then_some((ty, total))
}

/// Incremental temporal-unit splitter.
#[derive(Debug, Default)]
pub struct TemporalUnitSplitter {
    buf: Vec<u8>,
    /// Offset of the current temporal unit's first OBU.
    tu_start: usize,
    /// Offset of the next unparsed OBU.
    cursor: usize,
}

impl TemporalUnitSplitter {
    /// Creates a splitter.
    ///
    /// # Returns
    /// An empty splitter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes and returns every completed temporal unit.
    ///
    /// # Arguments
    /// * `data` - newly read bytes.
    ///
    /// # Returns
    /// `(temporal unit, contains sequence header)` pairs.
    pub fn push(&mut self, data: &[u8]) -> Vec<(Vec<u8>, bool)> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some((ty, len)) = parse_obu(&self.buf[self.cursor..]) {
            if ty == OBU_TEMPORAL_DELIMITER && self.cursor > self.tu_start {
                let tu = self.buf[self.tu_start..self.cursor].to_vec();
                let key = contains_sequence_header(&tu);
                out.push((tu, key));
                self.tu_start = self.cursor;
            }
            self.cursor += len;
        }
        if self.tu_start > 0 {
            self.buf.drain(..self.tu_start);
            self.cursor -= self.tu_start;
            self.tu_start = 0;
        }
        out
    }
}

/// Reports whether a temporal unit contains a sequence header OBU (treated as a key frame).
///
/// # Arguments
/// * `tu` - temporal unit bytes.
///
/// # Returns
/// `true` if a sequence header is present.
pub fn contains_sequence_header(tu: &[u8]) -> bool {
    let mut pos = 0;
    while let Some((ty, len)) = parse_obu(&tu[pos..]) {
        if ty == OBU_SEQUENCE_HEADER {
            return true;
        }
        pos += len;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_temporal_delimiters() {
        // TD (type 2, size 0), SEQ (type 1, size 1), FRAME (type 6, size 2), TD, FRAME
        let td = [0x12, 0x00];
        let seq = [0x0A, 0x01, 0xAA];
        let frame = [0x32, 0x02, 0x01, 0x02];
        let mut stream = Vec::new();
        for part in [&td[..], &seq, &frame, &td, &frame, &td] {
            stream.extend_from_slice(part);
        }
        let mut s = TemporalUnitSplitter::new();
        let tus = s.push(&stream);
        assert_eq!(tus.len(), 2);
        assert!(tus[0].1);
        assert!(!tus[1].1);
    }
}
