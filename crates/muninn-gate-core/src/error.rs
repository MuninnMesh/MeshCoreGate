//! Core error type shared by gateway modules.

use core::fmt;

/// Errors returned by platform-independent gateway code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error
{
    /// A fixed-capacity collection is full.
    Capacity,
    /// Gateway configuration is missing or invalid.
    InvalidConfig,
    /// MeshCore operation failed.
    Meshcore,
    /// Configured telemetry producer was not found.
    ProducerNotFound,
    /// Radio operation failed.
    Radio,
    /// Rendering output exceeded the fixed buffer.
    RenderBufferFull,
    /// Persistent storage operation failed.
    Storage,
    /// Platform time operation failed.
    Time,
    /// Operation is unsupported by the selected platform.
    Unsupported,
}

impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self {
            Self::Capacity => f.write_str("fixed-capacity collection is full"),
            Self::InvalidConfig => f.write_str("invalid gateway configuration"),
            Self::Meshcore => f.write_str("MeshCore operation failed"),
            Self::ProducerNotFound => f.write_str("configured producer was not found"),
            Self::Radio => f.write_str("radio operation failed"),
            Self::RenderBufferFull => f.write_str("render buffer is full"),
            Self::Storage => f.write_str("persistent storage operation failed"),
            Self::Time => f.write_str("platform time operation failed"),
            Self::Unsupported => f.write_str("operation is not supported on this platform"),
        }
    }
}

impl From<fmt::Error> for Error
{
    fn from(_: fmt::Error) -> Self
    {
        Self::RenderBufferFull
    }
}
