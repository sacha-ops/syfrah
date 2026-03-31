//! Runtime module — delegates to compute (VmManager) and overlay (NetworkBackend).
//!
//! This module wraps the existing compute and overlay layer interfaces,
//! providing a unified abstraction for Forge's reconciler and API to
//! execute infrastructure operations.

use std::sync::Arc;

/// Runtime wrapper that holds references to compute and overlay backends.
pub struct ForgeRuntime {
    /// Compute layer VM manager.
    pub vm_manager: Option<Arc<syfrah_compute::VmManager>>,
    /// Overlay layer network backend.
    pub network_backend: Option<Arc<dyn syfrah_overlay::NetworkBackend>>,
}

impl ForgeRuntime {
    /// Create a new runtime with no backends wired.
    pub fn new() -> Self {
        Self {
            vm_manager: None,
            network_backend: None,
        }
    }

    /// Create a runtime with the given backends.
    pub fn with_backends(
        vm_manager: Arc<syfrah_compute::VmManager>,
        network_backend: Arc<dyn syfrah_overlay::NetworkBackend>,
    ) -> Self {
        Self {
            vm_manager: Some(vm_manager),
            network_backend: Some(network_backend),
        }
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
    }
}
