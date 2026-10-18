//! Big-endian binary reader / writer used for every [`NetMessage`](crate::NetMessage) payload.
//!
//! C#: `_0006_0002._0004` (writer) and `_000E._0007` (reader).  Although they look like
//! `System.IO.BinaryWriter`/`BinaryReader`, the obfuscated copies differ in two important ways:
//!
//! * multi-byte integers are written **big-endian** (network order) on little-endian hosts, and
//! * strings are prefixed with a **signed 16-bit big-endian byte count** followed by UTF-8 bytes
//!   (not the 7-bit encoded length used by .NET).
//!
//! IP addresses are written as a one byte "address length" marker (4 or 16) followed by the
//! address.  For IPv4 the reader calls `new IPAddress((long)ReadUInt32())`; because
//! `IPAddress(long)` interprets the integer in host (little-endian) order while the integer was
//! read big-endian, the **octets travel in reverse order** on the wire.  This module reproduces
//! that quirk exactly.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::error::{ProtocolError, Result};

/// Append-only big-endian writer over a `Vec<u8>`.
#[derive(Debug, Default, Clone)]
pub struct BeWriter {
    buf: Vec<u8>,
}

impl BeWriter {
    /// Creates an empty writer.
    ///
    /// # Returns
    /// A writer with no buffered bytes.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Creates a writer whose buffer has room for `capacity` bytes without reallocating.
    ///
    /// # Arguments
    /// * `capacity` - number of bytes to pre-allocate.
    ///
    /// # Returns
    /// An empty writer.
    pub fn with_capacity(capacity: usize) -> Self {
        Self { buf: Vec::with_capacity(capacity) }
    }

    /// Consumes the writer and returns the bytes written so far.
    ///
    /// # Returns
    /// The encoded byte vector.
    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    /// Borrows the bytes written so far.
    ///
    /// # Returns
    /// A slice of the encoded bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    /// Returns the number of bytes written so far.
    ///
    /// # Returns
    /// Length of the internal buffer.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Reports whether nothing has been written yet.
    ///
    /// # Returns
    /// `true` when the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Writes a boolean as a single byte (`0` or `1`), like `BinaryWriter.Write(bool)`.
    ///
    /// # Arguments
    /// * `v` - value to write.
    pub fn write_bool(&mut self, v: bool) {
        self.buf.push(u8::from(v));
    }

    /// Writes one unsigned byte.
    ///
    /// # Arguments
    /// * `v` - value to write.
    pub fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Writes one signed byte.
    ///
    /// # Arguments
    /// * `v` - value to write.
    pub fn write_i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }

    /// Writes a big-endian `i16`.
    ///
    /// # Arguments
    /// * `v` - value to write.
    pub fn write_i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Writes a big-endian `u16`.
    ///
    /// # Arguments
    /// * `v` - value to write.
    pub fn write_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Writes a big-endian `i32`.
    ///
    /// # Arguments
    /// * `v` - value to write.
    pub fn write_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Writes a big-endian `u32`.
    ///
    /// # Arguments
    /// * `v` - value to write.
    pub fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Writes a big-endian `i64`.
    ///
    /// # Arguments
    /// * `v` - value to write.
    pub fn write_i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Writes a big-endian `u64` (the original forwards `ulong` to the `long` overload).
    ///
    /// # Arguments
    /// * `v` - value to write.
    pub fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Writes an IEEE-754 `f32` big-endian.
    ///
    /// Not observed in the shipped streamer (no float overload was found) but provided for
    /// symmetry; the bit pattern is written in the same byte order as integers.
    ///
    /// # Arguments
    /// * `v` - value to write.
    pub fn write_f32(&mut self, v: f32) {
        self.write_u32(v.to_bits());
    }

    /// Writes raw bytes without any length prefix.
    ///
    /// # Arguments
    /// * `bytes` - bytes to append verbatim.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Writes a string as `i16` big-endian UTF-8 byte length followed by the UTF-8 bytes.
    ///
    /// # Arguments
    /// * `s` - string to write.
    ///
    /// # Errors
    /// [`ProtocolError::TooLarge`] when the UTF-8 encoding is longer than `i16::MAX` bytes
    /// (the original silently truncated the length to 16 bits, corrupting the stream).
    pub fn write_string(&mut self, s: &str) -> Result<()> {
        let len = i16::try_from(s.len()).map_err(|_| ProtocolError::TooLarge("string longer than 32767 bytes"))?;
        self.write_i16(len);
        self.buf.extend_from_slice(s.as_bytes());
        Ok(())
    }

    /// Writes a .NET `Guid` as its 16 raw in-memory bytes (`Guid.ToByteArray()` order).
    ///
    /// # Arguments
    /// * `guid` - the 16 bytes in .NET layout (first three fields little-endian).
    pub fn write_guid(&mut self, guid: &[u8; 16]) {
        self.buf.extend_from_slice(guid);
    }

    /// Writes an IP address in the reader-compatible format (see module docs).
    ///
    /// # Arguments
    /// * `addr` - IPv4 or IPv6 address.
    pub fn write_ip(&mut self, addr: IpAddr) {
        match addr {
            IpAddr::V4(v4) => {
                self.write_u8(4);
                // Reader: IPAddress((long)ReadUInt32BE()) -> octets = value.to_le_bytes().
                let value = u32::from_le_bytes(v4.octets());
                self.write_u32(value);
            }
            IpAddr::V6(v6) => {
                self.write_u8(16);
                self.write_bytes(&v6.octets());
            }
        }
    }

    /// Writes an `IPEndPoint`: address (see [`BeWriter::write_ip`]) then the port as `i32` BE.
    ///
    /// # Arguments
    /// * `ep` - socket address to write.
    pub fn write_endpoint(&mut self, ep: SocketAddr) {
        self.write_ip(ep.ip());
        self.write_i32(i32::from(ep.port()));
    }
}

/// Cursor-based big-endian reader over a borrowed byte slice.
#[derive(Debug, Clone)]
pub struct BeReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BeReader<'a> {
    /// Creates a reader positioned at the start of `data`.
    ///
    /// # Arguments
    /// * `data` - bytes to decode.
    ///
    /// # Returns
    /// A new reader.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Returns the current read offset.
    ///
    /// # Returns
    /// Number of bytes consumed so far.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Returns how many unread bytes remain.
    ///
    /// # Returns
    /// Remaining byte count.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Borrows all unread bytes and advances to the end.
    ///
    /// # Returns
    /// The rest of the buffer (possibly empty).
    pub fn read_to_end(&mut self) -> &'a [u8] {
        let rest = &self.data[self.pos..];
        self.pos = self.data.len();
        rest
    }

    /// Reads exactly `n` bytes.
    ///
    /// # Arguments
    /// * `n` - number of bytes.
    ///
    /// # Returns
    /// A slice of `n` bytes borrowed from the input.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] if fewer than `n` bytes remain.
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(ProtocolError::UnexpectedEof { offset: self.pos, needed: n - self.remaining() });
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// Reads a fixed-size array.
    ///
    /// # Returns
    /// The next `N` bytes copied into an array.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] if fewer than `N` bytes remain.
    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.read_bytes(N)?);
        Ok(out)
    }

    /// Reads a boolean byte (any non-zero value is `true`).
    ///
    /// # Returns
    /// The decoded boolean.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] at end of data.
    pub fn read_bool(&mut self) -> Result<bool> {
        Ok(self.read_u8()? != 0)
    }

    /// Reads one unsigned byte.
    ///
    /// # Returns
    /// The byte.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] at end of data.
    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(self.read_array::<1>()?[0])
    }

    /// Reads one signed byte.
    ///
    /// # Returns
    /// The byte reinterpreted as `i8`.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] at end of data.
    pub fn read_i8(&mut self) -> Result<i8> {
        Ok(self.read_u8()? as i8)
    }

    /// Reads a big-endian `i16`.
    ///
    /// # Returns
    /// The decoded value.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] at end of data.
    pub fn read_i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.read_array()?))
    }

    /// Reads a big-endian `u16`.
    ///
    /// # Returns
    /// The decoded value.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] at end of data.
    pub fn read_u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.read_array()?))
    }

    /// Reads a big-endian `i32`.
    ///
    /// # Returns
    /// The decoded value.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] at end of data.
    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.read_array()?))
    }

    /// Reads a big-endian `u32`.
    ///
    /// # Returns
    /// The decoded value.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] at end of data.
    pub fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.read_array()?))
    }

    /// Reads a big-endian `i64`.
    ///
    /// # Returns
    /// The decoded value.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] at end of data.
    pub fn read_i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.read_array()?))
    }

    /// Reads a big-endian `u64`.
    ///
    /// # Returns
    /// The decoded value.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] at end of data.
    pub fn read_u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.read_array()?))
    }

    /// Reads a big-endian IEEE-754 `f32` (see [`BeWriter::write_f32`]).
    ///
    /// # Returns
    /// The decoded value.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] at end of data.
    pub fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    /// Reads an `i16`-length-prefixed UTF-8 string.
    ///
    /// # Returns
    /// The decoded string (empty for length 0).
    ///
    /// # Errors
    /// * [`ProtocolError::InvalidLength`] for a negative length (the original throws `IOException`).
    /// * [`ProtocolError::UnexpectedEof`] if the string is truncated.
    /// * [`ProtocolError::InvalidUtf8`] if the bytes are not UTF-8.
    pub fn read_string(&mut self) -> Result<String> {
        let len = self.read_i16()?;
        if len < 0 {
            return Err(ProtocolError::InvalidLength { what: "string", length: i64::from(len) });
        }
        let bytes = self.read_bytes(len as usize)?;
        Ok(String::from_utf8(bytes.to_vec())?)
    }

    /// Reads a 16-byte .NET `Guid` in raw `ToByteArray()` layout.
    ///
    /// # Returns
    /// The 16 raw bytes.
    ///
    /// # Errors
    /// [`ProtocolError::UnexpectedEof`] if fewer than 16 bytes remain.
    pub fn read_guid(&mut self) -> Result<[u8; 16]> {
        self.read_array()
    }

    /// Reads an IP address (length marker + address, IPv4 octets reversed; see module docs).
    ///
    /// # Returns
    /// The decoded address.
    ///
    /// # Errors
    /// * [`ProtocolError::UnexpectedEof`] if truncated.
    /// * [`ProtocolError::BadAddressLength`] — note the original treats every marker other than 4
    ///   as IPv6; this port is stricter and rejects anything but 4 or 16.
    pub fn read_ip(&mut self) -> Result<IpAddr> {
        match self.read_u8()? {
            4 => {
                let value = self.read_u32()?;
                Ok(IpAddr::V4(Ipv4Addr::from(value.to_le_bytes())))
            }
            16 => Ok(IpAddr::V6(Ipv6Addr::from(self.read_array::<16>()?))),
            other => Err(ProtocolError::BadAddressLength(other)),
        }
    }

    /// Reads an `IPEndPoint` (address followed by a big-endian `i32` port).
    ///
    /// # Returns
    /// The socket address.
    ///
    /// # Errors
    /// Propagates [`BeReader::read_ip`] errors; [`ProtocolError::InvalidLength`] for ports outside
    /// `0..=65535`.
    pub fn read_endpoint(&mut self) -> Result<SocketAddr> {
        let ip = self.read_ip()?;
        let port = self.read_i32()?;
        let port = u16::try_from(port).map_err(|_| ProtocolError::InvalidLength { what: "port", length: i64::from(port) })?;
        Ok(SocketAddr::new(ip, port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_are_big_endian() {
        let mut w = BeWriter::new();
        w.write_i32(0x0102_0304);
        w.write_i16(-2);
        assert_eq!(w.as_slice(), &[1, 2, 3, 4, 0xFF, 0xFE]);
        let mut r = BeReader::new(w.as_slice());
        assert_eq!(r.read_i32().unwrap(), 0x0102_0304);
        assert_eq!(r.read_i16().unwrap(), -2);
    }

    #[test]
    fn strings_use_i16_prefix() {
        let mut w = BeWriter::new();
        w.write_string("héllo").unwrap();
        assert_eq!(&w.as_slice()[..2], &[0, 6]);
        assert_eq!(BeReader::new(w.as_slice()).read_string().unwrap(), "héllo");
    }

    #[test]
    fn ipv4_octets_are_reversed_on_the_wire() {
        let mut w = BeWriter::new();
        let ep: SocketAddr = "192.168.1.20:38810".parse().unwrap();
        w.write_endpoint(ep);
        assert_eq!(&w.as_slice()[..5], &[4, 20, 1, 168, 192]);
        assert_eq!(BeReader::new(w.as_slice()).read_endpoint().unwrap(), ep);
    }
}
