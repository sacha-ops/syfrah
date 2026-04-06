//! Storage — ZeroFS orchestration over S3.
//!
//! Each region has its own S3 backend. ZeroFS provides encrypted,
//! compressed, cached access to S3 as a filesystem (NFS/9P) or
//! block device (NBD).
//!
//! This module handles:
//! - ZeroFS binary installation
//! - Per-region S3 configuration
//! - ZeroFS systemd service lifecycle
//! - Region registration and credential management

pub mod ops;
pub mod region;
pub mod service;
