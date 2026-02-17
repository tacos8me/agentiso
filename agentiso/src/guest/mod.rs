pub mod protocol;

// Re-export protocol types for use by host-side modules (vm::vsock).
#[allow(unused_imports)]
pub use protocol::*;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Variants will be constructed by the host-side vsock client (vm::vsock).
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum GuestAgentError {
    #[error("guest agent connection failed: {0}")]
    ConnectionFailed(String),

    #[error("guest agent protocol error: {0}")]
    ProtocolError(String),

    #[error("guest agent timeout")]
    Timeout,

    #[error("guest agent returned error: {code:?}: {message}")]
    RemoteError {
        code: protocol::ErrorCode,
        message: String,
    },

    #[error("message too large: {size} bytes (max {max})")]
    MessageTooLarge { size: u32, max: u32 },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Read a length-prefixed JSON message from an async reader.
pub async fn read_message<R: AsyncReadExt + Unpin, T: serde::de::DeserializeOwned>(
    reader: &mut R,
) -> Result<T, GuestAgentError> {
    let len = reader.read_u32().await?;
    if len > protocol::MAX_MESSAGE_SIZE {
        return Err(GuestAgentError::MessageTooLarge {
            size: len,
            max: protocol::MAX_MESSAGE_SIZE,
        });
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    let msg = serde_json::from_slice(&buf)?;
    Ok(msg)
}

/// Write a length-prefixed JSON message to an async writer.
pub async fn write_message<W: AsyncWriteExt + Unpin, T: serde::Serialize>(
    writer: &mut W,
    msg: &T,
) -> Result<(), GuestAgentError> {
    let encoded = protocol::encode_message(msg)?;
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}
