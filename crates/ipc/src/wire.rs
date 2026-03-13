//! Wire format: length-prefixed JSON over Unix Domain Sockets.
//!
//! Format: `[4-byte big-endian length][JSON bytes]`

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::messages::Envelope;

/// Maximum message size (1 MB) to prevent resource exhaustion.
pub const MAX_MESSAGE_SIZE: u32 = 1024 * 1024;

/// Read a single envelope from a stream.
pub async fn read_envelope<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<Envelope, Box<dyn std::error::Error + Send + Sync>> {
    // Read 4-byte big-endian length prefix
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);

    if len > MAX_MESSAGE_SIZE {
        return Err(format!(
            "Message too large: {} bytes (max {})",
            len, MAX_MESSAGE_SIZE
        )
        .into());
    }

    // Read the JSON payload
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;

    let envelope: Envelope = serde_json::from_slice(&buf)?;
    Ok(envelope)
}

/// Write an envelope to a stream with length prefix.
pub async fn write_envelope<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    envelope: &Envelope,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let json = serde_json::to_vec(envelope)?;
    let len = json.len() as u32;

    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&json).await?;
    writer.flush().await?;

    Ok(())
}
