//! Typed errors for the protocol layer.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("message size {size} exceeds maximum allowed {max}")]
    MessageTooLarge { size: usize, max: usize },

    #[error("failed to encode protobuf message: {0}")]
    EncodeError(#[from] prost::EncodeError),

    #[error("failed to decode protobuf message: {0}")]
    DecodeError(#[from] prost::DecodeError),

    #[error("QUIC connection error: {0}")]
    ConnectionError(#[from] quinn::ConnectionError),

    #[error("QUIC read error: {0}")]
    ReadError(#[from] quinn::ReadExactError),

    #[error("QUIC write error: {0}")]
    WriteError(#[from] quinn::WriteError),

    #[error("TLS configuration error: {0}")]
    TlsError(String),

    #[error("unknown request type in PluginRequest")]
    UnknownRequest,

    #[error("tool not found: {0}")]
    ToolNotFound(String),

    #[error("stream closed unexpectedly")]
    StreamClosed,
}
