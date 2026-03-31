//! # syfrah-compute
//!
//! Cloud Hypervisor driver for the Syfrah control plane.
//!
//! This crate owns the full lifecycle of a VM on a single node: create, boot,
//! monitor, shutdown, delete, and reconnect after daemon restarts. It is the
//! specialist that interfaces with [Cloud Hypervisor](https://github.com/cloud-hypervisor/cloud-hypervisor)
//! and manages VM processes.
//!
//! ## Public API for forge
//!
//! Forge interacts with compute through [`VmManager`], which provides:
//! - [`VmManager::create_vm`] — spawn and boot a VM from a [`VmSpec`]
//! - [`VmManager::shutdown_vm`] — graceful shutdown via the 4-level kill chain
//! - [`VmManager::delete_vm`] — stop + clean up all artifacts
//! - [`VmManager::info`] / [`VmManager::list`] — query VM status
//! - [`VmManager::subscribe`] — receive lifecycle [`VmEvent`]s via broadcast
//! - [`VmManager::reconnect`] — recover VMs after daemon restart
//!
//! ## Event model
//!
//! See the [`events`] module for the two-level event model (internal tracing
//! vs external broadcast channel).

pub mod binary;
pub mod boot;
pub mod cli;
pub mod client;
pub mod config;
pub mod control;
pub mod disk;
pub mod error;
pub mod events;
pub mod handler;
pub mod image;
pub mod manager;
pub mod network;
pub mod phase;
pub mod preflight;
#[allow(dead_code)]
pub(crate) mod process;
#[allow(dead_code)]
mod runtime;
pub mod runtime_backend;
pub mod runtime_ch;
pub mod runtime_container;
#[cfg(test)]
pub mod test_utils;
pub mod types;

// -- Public re-exports for forge consumption ----------------------------------

pub use binary::VersionReport;
pub use control::{ComputeLayerHandler, ComputeRequest, ComputeResponse};
pub use error::{
    ClientError, ComputeError, ConcurrencyError, ConfigError, PreflightError, ProcessError,
    TransitionError,
};
pub use events::emit;
pub use manager::{ComputeConfig, ReconnectSummary, VmManager};
pub use network::{CleanupResult, NetworkCleanup, NetworkInfo};
pub use phase::VmPhase;
pub use runtime_backend::{ComputeRuntime, RuntimeHandle, RuntimeInfo, RuntimeSpec, RuntimeType};
pub use types::{GpuMode, NetworkConfig, VmEvent, VmId, VmSpec, VmStatus, VolumeAttachment};
