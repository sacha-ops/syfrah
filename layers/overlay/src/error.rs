use thiserror::Error;

/// Errors returned by overlay networking operations.
#[derive(Debug, Error)]
pub enum OverlayError {
    /// A shell/system command failed.
    #[error("command failed: {0}")]
    CommandFailed(String),

    /// The requested network interface does not exist.
    #[error("interface not found: {0}")]
    InterfaceNotFound(String),

    /// A firewall / nftables rule could not be applied.
    #[error("rule application failed: {0}")]
    RuleApplicationFailed(String),
}

pub type Result<T> = std::result::Result<T, OverlayError>;
