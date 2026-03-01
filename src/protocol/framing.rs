//! Length-delimited Protobuf framing over QUIC streams.
//!
//! Wire format (matching the Go orchestrator exactly):
//!   [4 bytes: big-endian uint32 length][N bytes: Protobuf message]
//!
//! Maximum message size: 16 MB.

use anyhow::Result;
use prost::Message;
use quinn::{RecvStream, SendStream};

use super::errors::ProtocolError;

/// Maximum allowed message size: 16 MB.
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Write a length-delimited Protobuf message to a QUIC send stream.
///
/// Encodes the message, prepends a 4-byte big-endian length header,
/// and writes both to the stream.
pub async fn write_message(stream: &mut SendStream, msg: &impl Message) -> Result<()> {
    let data = msg.encode_to_vec();
    let size = data.len();

    if size > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge {
            size,
            max: MAX_MESSAGE_SIZE,
        }
        .into());
    }

    let header = (size as u32).to_be_bytes();
    stream.write_all(&header).await?;
    stream.write_all(&data).await?;

    Ok(())
}

/// Read a length-delimited Protobuf message from a QUIC receive stream.
///
/// Reads a 4-byte big-endian length header, then reads that many bytes
/// and decodes the Protobuf message.
pub async fn read_message<M: Message + Default>(stream: &mut RecvStream) -> Result<M> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;

    let size = u32::from_be_bytes(header) as usize;

    if size > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge {
            size,
            max: MAX_MESSAGE_SIZE,
        }
        .into());
    }

    let mut buf = vec![0u8; size];
    stream.read_exact(&mut buf).await?;

    let msg = M::decode(&buf[..])?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_message_size_constant() {
        assert_eq!(MAX_MESSAGE_SIZE, 16 * 1024 * 1024);
    }
}
