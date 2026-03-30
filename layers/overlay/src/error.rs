//! Overlay error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OverlayError {
    #[error("invalid MAC address: {0}")]
    InvalidMac(String),

    #[error("command failed: {cmd} — {detail}")]
    CommandFailed { cmd: String, detail: String },

    #[error("interface not found: {0}")]
    InterfaceNotFound(String),

    #[error("VXLAN device not found for bridge: {0}")]
    VxlanNotFound(String),
}

/// Overlay result alias.
pub type Result<T> = std::result::Result<T, OverlayError>;
