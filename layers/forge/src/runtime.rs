//! Runtime module — delegates to compute (VmManager) and overlay (NetworkBackend).
//!
//! This module wraps the existing compute and overlay layer interfaces,
//! providing a unified abstraction for Forge's reconciler and API to
//! execute infrastructure operations.
//!
//! ## Trait boundaries
//!
//! The `ComputeBackend` trait defines the interface between Forge and the
//! compute layer. The `NetworkRuntime` trait defines the interface between
//! Forge and the overlay layer. Both are async and object-safe.
//!
//! In production, `ForgeRuntime` wraps `VmManager` and `NetworkBackend`.
//! In tests, mock implementations can be substituted.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Error type for runtime operations.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("compute error: {0}")]
    Compute(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("runtime not available: {0}")]
    Unavailable(String),
}

/// Compute backend trait — the interface between Forge and the compute layer.
///
/// This trait abstracts the VmManager so the API and reconciler modules
/// do not depend directly on syfrah-compute types.
#[async_trait::async_trait]
pub trait ComputeBackend: Send + Sync {
    /// Create and boot a VM from a spec.
    async fn create_vm(
        &self,
        spec: syfrah_compute::VmSpec,
    ) -> Result<syfrah_compute::VmStatus, RuntimeError>;

    /// List all VMs.
    async fn list_vms(&self) -> Vec<syfrah_compute::VmStatus>;

    /// Get a single VM's status.
    async fn get_vm(&self, id: &str) -> Result<syfrah_compute::VmStatus, RuntimeError>;

    /// Delete a VM (stop + cleanup).
    async fn delete_vm(&self, id: &str) -> Result<(), RuntimeError>;

    /// Start a stopped VM.
    async fn start_vm(&self, id: &str) -> Result<syfrah_compute::VmStatus, RuntimeError>;

    /// Stop a running VM.
    async fn stop_vm(&self, id: &str) -> Result<(), RuntimeError>;

    /// Reboot a VM (stop + start).
    async fn reboot_vm(&self, id: &str) -> Result<syfrah_compute::VmStatus, RuntimeError>;
}

/// Implementation of ComputeBackend backed by VmManager.
pub struct VmManagerBackend {
    vm_manager: Arc<syfrah_compute::VmManager>,
}

impl VmManagerBackend {
    pub fn new(vm_manager: Arc<syfrah_compute::VmManager>) -> Self {
        Self { vm_manager }
    }
}

#[async_trait::async_trait]
impl ComputeBackend for VmManagerBackend {
    async fn create_vm(
        &self,
        spec: syfrah_compute::VmSpec,
    ) -> Result<syfrah_compute::VmStatus, RuntimeError> {
        self.vm_manager
            .create_vm(spec)
            .await
            .map_err(|e| RuntimeError::Compute(e.to_string()))
    }

    async fn list_vms(&self) -> Vec<syfrah_compute::VmStatus> {
        self.vm_manager.list().await
    }

    async fn get_vm(&self, id: &str) -> Result<syfrah_compute::VmStatus, RuntimeError> {
        self.vm_manager
            .info(id)
            .await
            .map_err(|e| RuntimeError::Compute(e.to_string()))
    }

    async fn delete_vm(&self, id: &str) -> Result<(), RuntimeError> {
        self.vm_manager
            .delete_vm(id)
            .await
            .map_err(|e| RuntimeError::Compute(e.to_string()))
    }

    async fn start_vm(&self, id: &str) -> Result<syfrah_compute::VmStatus, RuntimeError> {
        self.vm_manager
            .start_vm(id)
            .await
            .map_err(|e| RuntimeError::Compute(e.to_string()))
    }

    async fn stop_vm(&self, id: &str) -> Result<(), RuntimeError> {
        self.vm_manager
            .shutdown_vm(id)
            .await
            .map_err(|e| RuntimeError::Compute(e.to_string()))
    }

    async fn reboot_vm(&self, id: &str) -> Result<syfrah_compute::VmStatus, RuntimeError> {
        // Reboot = stop then start
        self.vm_manager
            .shutdown_vm(id)
            .await
            .map_err(|e| RuntimeError::Compute(e.to_string()))?;
        self.vm_manager
            .start_vm(id)
            .await
            .map_err(|e| RuntimeError::Compute(e.to_string()))
    }
}

/// Capacity info extracted from the runtime.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RuntimeCapacityInfo {
    pub total_vcpus_used: u32,
    pub total_memory_used_mb: u64,
    pub vm_count: u32,
}

/// Runtime wrapper that holds references to compute and overlay backends.
pub struct ForgeRuntime {
    /// Compute layer VM manager.
    pub vm_manager: Option<Arc<syfrah_compute::VmManager>>,
    /// Overlay layer network backend.
    pub network_backend: Option<Arc<dyn syfrah_overlay::NetworkBackend>>,
    /// Compute backend (trait-based interface for API/reconciler).
    pub compute: Option<Arc<dyn ComputeBackend>>,
}

impl ForgeRuntime {
    /// Create a new runtime with no backends wired.
    pub fn new() -> Self {
        Self {
            vm_manager: None,
            network_backend: None,
            compute: None,
        }
    }

    /// Create a runtime with the given backends.
    pub fn with_backends(
        vm_manager: Arc<syfrah_compute::VmManager>,
        network_backend: Arc<dyn syfrah_overlay::NetworkBackend>,
    ) -> Self {
        let compute: Arc<dyn ComputeBackend> =
            Arc::new(VmManagerBackend::new(Arc::clone(&vm_manager)));
        Self {
            vm_manager: Some(vm_manager),
            network_backend: Some(network_backend),
            compute: Some(compute),
        }
    }

    /// Get the compute backend, if available.
    pub fn compute(&self) -> Result<&dyn ComputeBackend, RuntimeError> {
        self.compute
            .as_deref()
            .ok_or_else(|| RuntimeError::Unavailable("compute backend not initialized".into()))
    }
}

impl Default for ForgeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_creates_empty() {
        let rt = ForgeRuntime::new();
        assert!(rt.vm_manager.is_none());
        assert!(rt.network_backend.is_none());
        assert!(rt.compute.is_none());
    }

    #[test]
    fn compute_backend_unavailable() {
        let rt = ForgeRuntime::new();
        assert!(rt.compute().is_err());
    }
}
