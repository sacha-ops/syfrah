//! Region storage configuration — S3 backend per region.
//!
//! Each region maps to one S3 bucket. Credentials are stored
//! locally in the fabric state (future: encrypted in TiKV).

use serde::{Deserialize, Serialize};
use syfrah_core::error::SyfrahError;

/// S3 storage configuration for a region.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionStorage {
    /// Region identifier (e.g., "eu", "us").
    pub region: String,
    /// S3 endpoint URL.
    pub s3_endpoint: String,
    /// S3 bucket name.
    pub s3_bucket: String,
    /// S3 access key.
    pub s3_access_key: String,
    /// S3 secret key.
    pub s3_secret_key: String,
    /// S3 region (e.g., "eu-central-1"). Empty for non-AWS.
    #[serde(default)]
    pub s3_region: String,
    /// Whether this is the default region.
    #[serde(default)]
    pub is_default: bool,
}

impl RegionStorage {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), SyfrahError> {
        if self.region.is_empty() {
            return Err(SyfrahError::validation("region cannot be empty"));
        }
        if self.s3_endpoint.is_empty() {
            return Err(SyfrahError::validation("s3_endpoint cannot be empty"));
        }
        if self.s3_bucket.is_empty() {
            return Err(SyfrahError::validation("s3_bucket cannot be empty"));
        }
        if self.s3_access_key.is_empty() {
            return Err(SyfrahError::validation("s3_access_key cannot be empty"));
        }
        if self.s3_secret_key.is_empty() {
            return Err(SyfrahError::validation("s3_secret_key cannot be empty"));
        }
        if !self.s3_endpoint.starts_with("http://") && !self.s3_endpoint.starts_with("https://") {
            return Err(SyfrahError::validation(
                "s3_endpoint must start with http:// or https://",
            ));
        }
        Ok(())
    }
}

/// Local registry of region storage configs.
/// Persisted as JSON at ~/.syfrah/regions.json.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RegionRegistry {
    pub regions: Vec<RegionStorage>,
}

impl RegionRegistry {
    const PATH: &'static str = "regions";
    const KEY: &'static str = "config";

    /// Load from local store.
    pub fn load(db: &syfrah_state::LocalDb) -> Result<Self, SyfrahError> {
        db.get(Self::PATH, Self::KEY)
            .map(|opt| opt.unwrap_or_default())
            .map_err(|e| SyfrahError::internal(e.to_string()))
    }

    /// Save to local store.
    pub fn save(&self, db: &syfrah_state::LocalDb) -> Result<(), SyfrahError> {
        db.set(Self::PATH, Self::KEY, self)
            .map_err(|e| SyfrahError::internal(e.to_string()))
    }

    /// Add or update a region.
    pub fn upsert(&mut self, config: RegionStorage) {
        if let Some(existing) = self.regions.iter_mut().find(|r| r.region == config.region) {
            *existing = config;
        } else {
            self.regions.push(config);
        }
    }

    /// Remove a region.
    pub fn remove(&mut self, region: &str) -> bool {
        let before = self.regions.len();
        self.regions.retain(|r| r.region != region);
        self.regions.len() < before
    }

    /// Find a region config.
    pub fn find(&self, region: &str) -> Option<&RegionStorage> {
        self.regions.iter().find(|r| r.region == region)
    }

    /// Get the default region (first one marked default, or first one).
    pub fn default_region(&self) -> Option<&RegionStorage> {
        self.regions
            .iter()
            .find(|r| r.is_default)
            .or_else(|| self.regions.first())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_region(name: &str) -> RegionStorage {
        RegionStorage {
            region: name.into(),
            s3_endpoint: "https://s3.example.com".into(),
            s3_bucket: format!("syfrah-{name}"),
            s3_access_key: "AKID".into(),
            s3_secret_key: "SECRET".into(),
            s3_region: String::new(),
            is_default: false,
        }
    }

    #[test]
    fn validate_ok() {
        make_region("eu").validate().unwrap();
    }

    #[test]
    fn validate_empty_region() {
        let mut r = make_region("eu");
        r.region = String::new();
        assert!(r.validate().is_err());
    }

    #[test]
    fn validate_bad_endpoint() {
        let mut r = make_region("eu");
        r.s3_endpoint = "ftp://bad".into();
        assert!(r.validate().is_err());
    }

    #[test]
    fn registry_upsert() {
        let mut reg = RegionRegistry::default();
        reg.upsert(make_region("eu"));
        reg.upsert(make_region("us"));
        assert_eq!(reg.regions.len(), 2);

        // Update eu
        let mut eu = make_region("eu");
        eu.s3_bucket = "new-bucket".into();
        reg.upsert(eu);
        assert_eq!(reg.regions.len(), 2);
        assert_eq!(reg.find("eu").unwrap().s3_bucket, "new-bucket");
    }

    #[test]
    fn registry_remove() {
        let mut reg = RegionRegistry::default();
        reg.upsert(make_region("eu"));
        reg.upsert(make_region("us"));
        assert!(reg.remove("eu"));
        assert_eq!(reg.regions.len(), 1);
        assert!(!reg.remove("eu")); // already removed
    }

    #[test]
    fn registry_default() {
        let mut reg = RegionRegistry::default();
        assert!(reg.default_region().is_none());

        reg.upsert(make_region("eu"));
        assert_eq!(reg.default_region().unwrap().region, "eu");

        let mut us = make_region("us");
        us.is_default = true;
        reg.upsert(us);
        assert_eq!(reg.default_region().unwrap().region, "us");
    }

    #[test]
    fn registry_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let db = syfrah_state::LocalDb::open_at(&dir.path().join("test.json")).unwrap();

        let mut reg = RegionRegistry::default();
        reg.upsert(make_region("eu"));
        reg.save(&db).unwrap();

        let loaded = RegionRegistry::load(&db).unwrap();
        assert_eq!(loaded.regions.len(), 1);
        assert_eq!(loaded.find("eu").unwrap().s3_bucket, "syfrah-eu");
    }

    #[test]
    fn serde_roundtrip() {
        let r = make_region("eu");
        let json = serde_json::to_string(&r).unwrap();
        let back: RegionStorage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.region, "eu");
        assert_eq!(back.s3_access_key, "AKID");
    }
}
