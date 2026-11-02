//! AES-CBC record layer used to encrypt every TCP channel.
//!
//! C#: `_0096_0002._0001 : Stream`, keyed by `VirtualDesktop.Interfaces.RegistrationToken.Aes`
//! (`Aes.Create()` → CBC mode, `PaddingMode.None`, key and IV issued by the
//! `ComputerRegistry` web service).
//!
//! Each `Write(buffer, offset, count)` on the encrypting stream produces one *record*:
//!
//! ```text
//! plaintext  = 7-bit-varint(count) || data[count] || random padding up to a 16-byte boundary
//! ciphertext = AES-CBC(plaintext)          (no padding mode, chaining continues across records)
//! ```
//!
//! The same `ICryptoTransform` instance is reused for the lifetime of the connection, so CBC
//! chaining state (the last ciphertext block) carries over from one record to the next in each
//! direction.  The reader decrypts the first block, decodes the varint to learn the record size,
//! then reads and decrypts the remaining `ceil((varint_len + count) / 16) * 16 - 16` bytes.

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, generic_array::GenericArray};
use rand::RngCore;

use crate::error::{ProtocolError, Result};

/// AES block size in bytes.
pub const BLOCK: usize = 16;

enum Enc {
    A128(cbc::Encryptor<aes::Aes128>),
    A192(cbc::Encryptor<aes::Aes192>),
    A256(cbc::Encryptor<aes::Aes256>),
}

enum Dec {
    A128(cbc::Decryptor<aes::Aes128>),
    A192(cbc::Decryptor<aes::Aes192>),
    A256(cbc::Decryptor<aes::Aes256>),
}

/// Encodes `value` as a .NET `Write7BitEncodedInt` varint.
///
/// # Arguments
/// * `value` - non-negative length.
/// * `out` - buffer the varint bytes are appended to.
///
/// # Returns
/// Number of bytes written (1–5).
pub fn write_varint(value: u32, out: &mut Vec<u8>) -> usize {
    let mut v = value;
    let mut n = 0;
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
        n += 1;
    }
    out.push(v as u8);
    n + 1
}

/// Decodes a .NET `Read7BitEncodedInt` varint from the front of `data`.
///
/// # Arguments
/// * `data` - bytes starting with the varint.
///
/// # Returns
/// `(value, bytes consumed)`.
///
/// # Errors
/// * [`ProtocolError::Crypto`] (`"Bad string length. 7bit Int32 format"` in the original) when
///   more than five bytes have the continuation bit.
/// * [`ProtocolError::UnexpectedEof`] if `data` ends inside the varint.
pub fn read_varint(data: &[u8]) -> Result<(u32, usize)> {
    let mut value: u32 = 0;
    let mut shift = 0;
    for (i, b) in data.iter().enumerate() {
        if shift == 35 {
            return Err(ProtocolError::Crypto("bad 7-bit encoded length"));
        }
        value |= u32::from(b & 0x7F) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            return Ok((value, i + 1));
        }
    }
    Err(ProtocolError::UnexpectedEof { offset: data.len(), needed: 1 })
}

/// Rounds `n` up to a multiple of [`BLOCK`].
///
/// # Arguments
/// * `n` - byte count.
///
/// # Returns
/// The rounded count.
pub fn round_up_block(n: usize) -> usize {
    n.div_ceil(BLOCK) * BLOCK
}

/// Encrypting half of a connection.
pub struct RecordEncryptor {
    enc: Enc,
}

impl RecordEncryptor {
    /// Creates an encryptor.
    ///
    /// # Arguments
    /// * `key` - 16, 24 or 32 byte AES key from the registration token.
    /// * `iv` - 16 byte initialisation vector from the registration token.
    ///
    /// # Returns
    /// The encryptor.
    ///
    /// # Errors
    /// [`ProtocolError::Crypto`] for unsupported key or IV lengths.
    pub fn new(key: &[u8], iv: &[u8]) -> Result<Self> {
        if iv.len() != BLOCK {
            return Err(ProtocolError::Crypto("IV must be 16 bytes"));
        }
        let iv = GenericArray::from_slice(iv);
        let enc = match key.len() {
            16 => Enc::A128(cbc::Encryptor::new(GenericArray::from_slice(key), iv)),
            24 => Enc::A192(cbc::Encryptor::new(GenericArray::from_slice(key), iv)),
            32 => Enc::A256(cbc::Encryptor::new(GenericArray::from_slice(key), iv)),
            _ => return Err(ProtocolError::Crypto("AES key must be 16, 24 or 32 bytes")),
        };
        Ok(Self { enc })
    }

    /// Encrypts whole blocks in place, continuing the CBC chain.
    ///
    /// # Arguments
    /// * `buf` - plaintext whose length is a multiple of 16; replaced by ciphertext.
    ///
    /// # Errors
    /// [`ProtocolError::Crypto`] if `buf.len()` is not a multiple of 16.
    pub fn encrypt_blocks(&mut self, buf: &mut [u8]) -> Result<()> {
        if !buf.len().is_multiple_of(BLOCK) {
            return Err(ProtocolError::Crypto("buffer is not block aligned"));
        }
        for block in buf.as_chunks_mut::<BLOCK>().0 {
            let block = GenericArray::from_mut_slice(block);
            match &mut self.enc {
                Enc::A128(e) => e.encrypt_block_mut(block),
                Enc::A192(e) => e.encrypt_block_mut(block),
                Enc::A256(e) => e.encrypt_block_mut(block),
            }
        }
        Ok(())
    }

    /// Builds one encrypted record for `data` (one `Stream.Write` call of the original).
    ///
    /// # Arguments
    /// * `data` - plaintext bytes.
    ///
    /// # Returns
    /// The ciphertext record.
    ///
    /// # Errors
    /// [`ProtocolError::TooLarge`] if `data` is larger than `i32::MAX` bytes.
    pub fn seal(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        let len = u32::try_from(data.len()).ok().filter(|l| *l <= i32::MAX as u32).ok_or(ProtocolError::TooLarge("record"))?;
        let mut rec = Vec::with_capacity(round_up_block(data.len() + 5));
        let prefix = write_varint(len, &mut rec);
        rec.extend_from_slice(data);
        let total = round_up_block(prefix + data.len());
        let pad = total - rec.len();
        if pad > 0 {
            let mut rnd = [0u8; BLOCK];
            rand::thread_rng().fill_bytes(&mut rnd);
            rec.extend_from_slice(&rnd[..pad]);
        }
        self.encrypt_blocks(&mut rec)?;
        Ok(rec)
    }
}

/// Decrypting half of a connection.
pub struct RecordDecryptor {
    dec: Dec,
}

/// Size information of a record, derived from its first decrypted block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordLayout {
    /// Offset of the payload inside the decrypted record (= varint length).
    pub data_offset: usize,
    /// Payload length.
    pub data_len: usize,
    /// Total ciphertext length of the record (multiple of 16, ≥ 16).
    pub total_len: usize,
}

impl RecordDecryptor {
    /// Creates a decryptor.
    ///
    /// # Arguments
    /// * `key` - 16, 24 or 32 byte AES key.
    /// * `iv` - 16 byte IV.
    ///
    /// # Returns
    /// The decryptor.
    ///
    /// # Errors
    /// [`ProtocolError::Crypto`] for unsupported key or IV lengths.
    pub fn new(key: &[u8], iv: &[u8]) -> Result<Self> {
        if iv.len() != BLOCK {
            return Err(ProtocolError::Crypto("IV must be 16 bytes"));
        }
        let iv = GenericArray::from_slice(iv);
        let dec = match key.len() {
            16 => Dec::A128(cbc::Decryptor::new(GenericArray::from_slice(key), iv)),
            24 => Dec::A192(cbc::Decryptor::new(GenericArray::from_slice(key), iv)),
            32 => Dec::A256(cbc::Decryptor::new(GenericArray::from_slice(key), iv)),
            _ => return Err(ProtocolError::Crypto("AES key must be 16, 24 or 32 bytes")),
        };
        Ok(Self { dec })
    }

    /// Decrypts whole blocks in place, continuing the CBC chain.
    ///
    /// # Arguments
    /// * `buf` - ciphertext whose length is a multiple of 16; replaced by plaintext.
    ///
    /// # Errors
    /// [`ProtocolError::Crypto`] if `buf.len()` is not a multiple of 16.
    pub fn decrypt_blocks(&mut self, buf: &mut [u8]) -> Result<()> {
        if !buf.len().is_multiple_of(BLOCK) {
            return Err(ProtocolError::Crypto("buffer is not block aligned"));
        }
        for block in buf.as_chunks_mut::<BLOCK>().0 {
            let block = GenericArray::from_mut_slice(block);
            match &mut self.dec {
                Dec::A128(d) => d.decrypt_block_mut(block),
                Dec::A192(d) => d.decrypt_block_mut(block),
                Dec::A256(d) => d.decrypt_block_mut(block),
            }
        }
        Ok(())
    }

    /// Computes the record layout from the first **decrypted** block.
    ///
    /// # Arguments
    /// * `first_block` - first 16 plaintext bytes of the record.
    ///
    /// # Returns
    /// The [`RecordLayout`].
    ///
    /// # Errors
    /// Propagates [`read_varint`] errors; [`ProtocolError::InvalidLength`] for absurd lengths.
    pub fn layout(first_block: &[u8; BLOCK]) -> Result<RecordLayout> {
        let (len, prefix) = read_varint(first_block)?;
        if len > i32::MAX as u32 {
            return Err(ProtocolError::InvalidLength { what: "encrypted record", length: i64::from(len) });
        }
        let total_len = round_up_block(prefix + len as usize);
        Ok(RecordLayout { data_offset: prefix, data_len: len as usize, total_len })
    }

    /// Decrypts a complete record held in memory (convenience for tests and buffered readers).
    ///
    /// # Arguments
    /// * `record` - exactly one ciphertext record.
    ///
    /// # Returns
    /// The plaintext payload.
    ///
    /// # Errors
    /// [`ProtocolError::Crypto`] / [`ProtocolError::UnexpectedEof`] on malformed input.
    pub fn open(&mut self, record: &[u8]) -> Result<Vec<u8>> {
        if record.len() < BLOCK {
            return Err(ProtocolError::UnexpectedEof { offset: record.len(), needed: BLOCK - record.len() });
        }
        let mut buf = record.to_vec();
        self.decrypt_blocks(&mut buf[..BLOCK])?;
        let layout = Self::layout(buf[..BLOCK].try_into().expect("16 bytes"))?;
        if buf.len() < layout.total_len {
            return Err(ProtocolError::UnexpectedEof { offset: buf.len(), needed: layout.total_len - buf.len() });
        }
        self.decrypt_blocks(&mut buf[BLOCK..layout.total_len])?;
        Ok(buf[layout.data_offset..layout.data_offset + layout.data_len].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_matches_dotnet() {
        let mut v = Vec::new();
        write_varint(300, &mut v);
        assert_eq!(v, [0xAC, 0x02]);
        assert_eq!(read_varint(&v).unwrap(), (300, 2));
    }

    #[test]
    fn records_chain_across_writes() {
        let key = [7u8; 16];
        let iv = [9u8; 16];
        let mut enc = RecordEncryptor::new(&key, &iv).unwrap();
        let mut dec = RecordDecryptor::new(&key, &iv).unwrap();
        for msg in [&b"hello"[..], &[0u8; 40][..], &b""[..]] {
            let rec = enc.seal(msg).unwrap();
            assert_eq!(rec.len() % 16, 0);
            assert_eq!(dec.open(&rec).unwrap(), msg);
        }
    }
}
