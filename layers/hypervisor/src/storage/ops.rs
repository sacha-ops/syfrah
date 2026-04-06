//! Storage operations — high-level orchestration.
//!
//! Called by the hypervisor handler for storage lifecycle.

use syfrah_core::error::SyfrahError;
use syfrah_state::LocalDb;

use super::region::{RegionRegistry, RegionStorage};
use super::service;

/// Setup storage for a region on this node.
///
/// 1. Validate config
/// 2. Install ZeroFS if needed
/// 3. Install config + systemd unit
/// 4. Start the service
pub fn setup_region(db: &LocalDb, config: RegionStorage) -> Result<(), SyfrahError> {
    config.validate()?;

    tracing::info!(region = %config.region, "storage: setting up region");
    eprintln!("  Setting up storage for region {}...", config.region);

    // Install ZeroFS binary
    service::ensure_installed()?;

    // Install config + systemd unit
    service::install_region(&config)?;

    // Start
    service::start_region(&config.region)?;

    // Persist region config
    let mut registry = RegionRegistry::load(db)?;
    registry.upsert(config.clone());
    registry.save(db)?;

    tracing::info!(region = %config.region, "storage: region ready");
    eprintln!("  Storage for region {} ready", config.region);

    Ok(())
}

/// Remove storage for a region on this node.
pub fn remove_region(db: &LocalDb, region: &str) -> Result<(), SyfrahError> {
    tracing::info!(region, "storage: removing region");

    service::uninstall_region(region)?;

    let mut registry = RegionRegistry::load(db)?;
    registry.remove(region);
    registry.save(db)?;

    Ok(())
}

/// Start all configured storage regions on this node.
pub fn start_all(db: &LocalDb) -> Result<(), SyfrahError> {
    let registry = RegionRegistry::load(db)?;
    for region in &registry.regions {
        if !service::is_region_active(&region.region) {
            let _ = service::start_region(&region.region);
        }
    }
    Ok(())
}

/// Stop all storage regions.
pub fn stop_all(db: &LocalDb) -> Result<(), SyfrahError> {
    let registry = RegionRegistry::load(db)?;
    for region in &registry.regions {
        let _ = service::stop_region(&region.region);
    }
    Ok(())
}

/// Uninstall all storage (called by hypervisor leave).
pub fn leave() -> Result<(), SyfrahError> {
    service::uninstall_all()
}

/// Get storage status for all regions.
pub fn status(db: &LocalDb) -> Vec<RegionStatus> {
    let registry = RegionRegistry::load(db).unwrap_or_default();
    registry
        .regions
        .iter()
        .map(|r| RegionStatus {
            region: r.region.clone(),
            s3_endpoint: r.s3_endpoint.clone(),
            s3_bucket: r.s3_bucket.clone(),
            active: service::is_region_active(&r.region),
            is_default: r.is_default,
        })
        .collect()
}

/// Status of a region's storage.
#[derive(Debug, Clone)]
pub struct RegionStatus {
    pub region: String,
    pub s3_endpoint: String,
    pub s3_bucket: String,
    pub active: bool,
    pub is_default: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();
        let s = status(&db);
        assert!(s.is_empty());
    }

    #[test]
    fn setup_validates() {
        let dir = tempfile::tempdir().unwrap();
        let db = LocalDb::open_at(&dir.path().join("test.json")).unwrap();

        let bad = RegionStorage {
            region: String::new(), // invalid
            s3_endpoint: "https://s3.example.com".into(),
            s3_bucket: "bucket".into(),
            s3_access_key: "key".into(),
            s3_secret_key: "secret".into(),
            s3_region: String::new(),
            is_default: false,
        };
        assert!(setup_region(&db, bad).is_err());
    }
}
