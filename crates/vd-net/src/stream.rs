//! Socket halves with an optional AES record layer.
//!
//! The original wraps the `NetworkStream` of every channel in `_0096_0002._0001` when the session
//! is encrypted.  Every `Write` call becomes one encrypted record, so callers here must hand a
//! whole logical packet to [`SecureWriter::write_packet`] (the original also builds each packet
//! in a buffer and issues a single `Write`).

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use vd_protocol::crypto::{BLOCK, RecordDecryptor, RecordEncryptor};

use crate::error::{NetError, Result};

/// AES key material of a session (from `RegistrationToken`).
#[derive(Clone, PartialEq, Eq)]
pub struct SessionKey {
    /// AES key (16/24/32 bytes).
    pub key: Vec<u8>,
    /// 16-byte IV.
    pub iv: Vec<u8>,
}

impl std::fmt::Debug for SessionKey {
    /// Formats the key without revealing key material.
    ///
    /// # Arguments
    /// * `f` - formatter.
    ///
    /// # Returns
    /// Formatter result.
    ///
    /// # Errors
    /// Only formatter errors.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionKey({} bit)", self.key.len() * 8)
    }
}

/// Reading half of a channel.
pub struct SecureReader {
    inner: OwnedReadHalf,
    dec: Option<RecordDecryptor>,
    /// Decrypted bytes of the current record not yet handed out.
    pending: Vec<u8>,
    pos: usize,
}

/// Writing half of a channel.
pub struct SecureWriter {
    inner: OwnedWriteHalf,
    enc: Option<RecordEncryptor>,
}

/// Splits a connected socket into (optionally encrypted) halves.
///
/// # Arguments
/// * `stream` - connected TCP stream.
/// * `key` - session key, or `None` for plaintext.
///
/// # Returns
/// `(reader, writer)`.
///
/// # Errors
/// [`NetError::Protocol`] if the key material is invalid.
pub fn split(stream: TcpStream, key: Option<&SessionKey>) -> Result<(SecureReader, SecureWriter)> {
    let (r, w) = stream.into_split();
    let (dec, enc) = match key {
        Some(k) => (Some(RecordDecryptor::new(&k.key, &k.iv)?), Some(RecordEncryptor::new(&k.key, &k.iv)?)),
        None => (None, None),
    };
    Ok((SecureReader { inner: r, dec, pending: Vec::new(), pos: 0 }, SecureWriter { inner: w, enc }))
}

impl SecureReader {
    /// Reports whether the record layer is active.
    ///
    /// # Returns
    /// `true` when encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.dec.is_some()
    }

    async fn fill_record(&mut self) -> Result<()> {
        let dec = self.dec.as_mut().expect("only called when encrypted");
        let mut first = [0u8; BLOCK];
        read_exact_or_closed(&mut self.inner, &mut first).await?;
        dec.decrypt_blocks(&mut first)?;
        let layout = RecordDecryptor::layout(&first)?;
        let mut rec = vec![0u8; layout.total_len];
        rec[..BLOCK].copy_from_slice(&first);
        if layout.total_len > BLOCK {
            read_exact_or_closed(&mut self.inner, &mut rec[BLOCK..]).await?;
            dec.decrypt_blocks(&mut rec[BLOCK..])?;
        }
        rec.truncate(layout.data_offset + layout.data_len);
        rec.drain(..layout.data_offset);
        self.pending = rec;
        self.pos = 0;
        Ok(())
    }

    /// Fills `buf` completely.
    ///
    /// # Arguments
    /// * `buf` - destination.
    ///
    /// # Errors
    /// * [`NetError::Closed`] if the peer closed the connection (the original raises
    ///   `EndOfStreamException`, which resets the channel).
    /// * [`NetError::Io`] / [`NetError::Protocol`] on socket or record errors.
    pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        if self.dec.is_none() {
            return read_exact_or_closed(&mut self.inner, buf).await;
        }
        let mut filled = 0;
        while filled < buf.len() {
            if self.pos >= self.pending.len() {
                self.fill_record().await?;
                continue;
            }
            let n = (buf.len() - filled).min(self.pending.len() - self.pos);
            buf[filled..filled + n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
            self.pos += n;
            filled += n;
        }
        Ok(())
    }

    /// Reads a fixed-size array.
    ///
    /// # Returns
    /// The bytes.
    ///
    /// # Errors
    /// See [`SecureReader::read_exact`].
    pub async fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        self.read_exact(&mut out).await?;
        Ok(out)
    }

    /// Reads exactly `n` bytes into a new vector.
    ///
    /// # Arguments
    /// * `n` - byte count.
    ///
    /// # Returns
    /// The bytes.
    ///
    /// # Errors
    /// See [`SecureReader::read_exact`].
    pub async fn read_vec(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut out = vec![0u8; n];
        self.read_exact(&mut out).await?;
        Ok(out)
    }
}

impl SecureWriter {
    /// Writes one logical packet (one encrypted record when encryption is on).
    ///
    /// # Arguments
    /// * `packet` - bytes to send.
    ///
    /// # Errors
    /// [`NetError::Io`] on socket errors, [`NetError::Protocol`] on record errors.
    pub async fn write_packet(&mut self, packet: &[u8]) -> Result<()> {
        match &mut self.enc {
            Some(enc) => {
                let rec = enc.seal(packet)?;
                self.inner.write_all(&rec).await?;
            }
            None => self.inner.write_all(packet).await?,
        }
        Ok(())
    }

    /// Shuts down the sending direction.
    ///
    /// # Errors
    /// [`NetError::Io`] if the shutdown fails.
    pub async fn shutdown(&mut self) -> Result<()> {
        self.inner.shutdown().await?;
        Ok(())
    }
}

/// `read_exact` that maps EOF to [`NetError::Closed`].
///
/// # Arguments
/// * `r` - reader.
/// * `buf` - destination.
///
/// # Errors
/// [`NetError::Closed`] on EOF, [`NetError::Io`] otherwise.
async fn read_exact_or_closed<R: AsyncReadExt + Unpin>(r: &mut R, buf: &mut [u8]) -> Result<()> {
    match r.read_exact(buf).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(NetError::Closed),
        Err(e) => Err(NetError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn encrypted_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let key = SessionKey { key: vec![1; 32], iv: vec![2; 16] };
        let k2 = key.clone();
        let server = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let (mut r, _w) = split(s, Some(&k2)).unwrap();
            let a: [u8; 3] = r.read_array().await.unwrap();
            let b = r.read_vec(5).await.unwrap();
            (a, b)
        });
        let c = TcpStream::connect(addr).await.unwrap();
        let (_r, mut w) = split(c, Some(&key)).unwrap();
        w.write_packet(b"abcdefgh").await.unwrap();
        let (a, b) = server.await.unwrap();
        assert_eq!(&a, b"abc");
        assert_eq!(b, b"defgh");
    }
}
