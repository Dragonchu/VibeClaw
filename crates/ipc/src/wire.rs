//! Wire format: length-prefixed JSON over Unix Domain Sockets.
//!
//! Format: `[4-byte big-endian length][JSON bytes]`
//!
//! The `_with_fds` variants additionally support passing file descriptors
//! via SCM_RIGHTS (Unix ancillary data). When an envelope carries FDs, the
//! sender injects an `fd_count` key into the serialised JSON and then sends
//! each FD individually via `passfd`. The wire format for the JSON portion
//! remains identical, so peers that use the plain `read_envelope` /
//! `write_envelope` functions stay compatible as long as they never
//! exchange FD-bearing messages.

use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;

use passfd::FdPassingExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::messages::Envelope;

/// Maximum message size (1 MB) to prevent resource exhaustion.
pub const MAX_MESSAGE_SIZE: u32 = 1024 * 1024;

// ---------------------------------------------------------------------------
// Standard (split-stream) API — no FD passing
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// FD-passing API — works on the full (unsplit) tokio UnixStream
// ---------------------------------------------------------------------------

/// Write an envelope to a `tokio::net::UnixStream`, optionally passing file
/// descriptors via SCM_RIGHTS.
///
/// When `envelope.fds` is non-empty, the serialised JSON includes an
/// `fd_count` key and each FD is sent afterwards via `passfd`.
pub async fn write_envelope_with_fds(
    stream: &tokio::net::UnixStream,
    envelope: &Envelope,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fd_count = envelope.fds.len();

    // Serialize — inject fd_count only when there are FDs to send.
    let json = if fd_count > 0 {
        let mut val = serde_json::to_value(envelope)?;
        val["fd_count"] = serde_json::json!(fd_count);
        serde_json::to_vec(&val)?
    } else {
        serde_json::to_vec(envelope)?
    };

    let len = json.len() as u32;

    // Build a combined buffer so the length prefix and JSON travel together.
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&json);

    write_all_async(stream, &buf).await?;

    // Send each FD via passfd (1-byte dummy + SCM_RIGHTS per FD).
    for fd in &envelope.fds {
        send_fd_async(stream, fd.as_raw_fd()).await?;
    }

    Ok(())
}

/// Read an envelope from a `tokio::net::UnixStream`, receiving any file
/// descriptors passed via SCM_RIGHTS.
pub async fn read_envelope_with_fds(
    stream: &tokio::net::UnixStream,
) -> Result<Envelope, Box<dyn std::error::Error + Send + Sync>> {
    // Read 4-byte length prefix.
    let mut len_buf = [0u8; 4];
    read_exact_async(stream, &mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);

    if len > MAX_MESSAGE_SIZE {
        return Err(format!(
            "Message too large: {} bytes (max {})",
            len, MAX_MESSAGE_SIZE
        )
        .into());
    }

    // Read JSON payload.
    let mut json_buf = vec![0u8; len as usize];
    read_exact_async(stream, &mut json_buf).await?;

    // Extract fd_count (injected by write_envelope_with_fds) before
    // deserialising into Envelope (which silently ignores unknown keys).
    let json_val: serde_json::Value = serde_json::from_slice(&json_buf)?;
    let fd_count = json_val
        .get("fd_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let mut envelope: Envelope = serde_json::from_value(json_val)?;

    // Receive FDs via passfd.
    for _ in 0..fd_count {
        let raw = recv_fd_async(stream).await?;
        envelope
            .fds
            .push(Arc::new(unsafe { OwnedFd::from_raw_fd(raw) }));
    }

    Ok(envelope)
}

// ---------------------------------------------------------------------------
// Async helpers for raw UnixStream I/O
// ---------------------------------------------------------------------------

async fn read_exact_async(
    stream: &tokio::net::UnixStream,
    buf: &mut [u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut total = 0;
    while total < buf.len() {
        stream.readable().await?;
        match stream.try_read(&mut buf[total..]) {
            Ok(0) => return Err("Connection closed".into()),
            Ok(n) => total += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

async fn write_all_async(
    stream: &tokio::net::UnixStream,
    buf: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut total = 0;
    while total < buf.len() {
        stream.writable().await?;
        match stream.try_write(&buf[total..]) {
            Ok(n) => total += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// FD send / recv via passfd + tokio try_io
// ---------------------------------------------------------------------------

async fn send_fd_async(
    stream: &tokio::net::UnixStream,
    fd: std::os::unix::io::RawFd,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        stream.writable().await?;
        match stream.try_io(tokio::io::Interest::WRITABLE, || {
            // Create a non-owning std UnixStream from the raw fd so we can
            // call the passfd extension trait, then `forget` it to avoid
            // closing the fd that tokio still owns.
            let std_stream =
                unsafe { std::os::unix::net::UnixStream::from_raw_fd(stream.as_raw_fd()) };
            let result = std_stream.send_fd(fd);
            std::mem::forget(std_stream);
            result
        }) {
            Ok(()) => return Ok(()),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

async fn recv_fd_async(
    stream: &tokio::net::UnixStream,
) -> Result<std::os::unix::io::RawFd, Box<dyn std::error::Error + Send + Sync>> {
    loop {
        stream.readable().await?;
        match stream.try_io(tokio::io::Interest::READABLE, || {
            let std_stream =
                unsafe { std::os::unix::net::UnixStream::from_raw_fd(stream.as_raw_fd()) };
            let result = std_stream.recv_fd();
            std::mem::forget(std_stream);
            result
        }) {
            Ok(fd) => return Ok(fd),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e.into()),
        }
    }
}
