//! Distributed state backed by TiKV.
//!
//! Provides the same API patterns as `LayerDb` (redb) but backed by TiKV
//! for distributed, replicated state across the mesh.
//!
//! Keys are prefixed by namespace: `{namespace}/{key}` to partition data.
//!
//! # Usage
//!
//! ```no_run
//! use syfrah_hypervisor::controlplane::ClusterDb;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let db = ClusterDb::connect(&["http://[fd01::1]:2379"]).await?;
//! db.put("vms", "vm-001", &serde_json::json!({"name": "web-1"})).await?;
//! let vm: Option<serde_json::Value> = db.get("vms", "vm-001").await?;
//! # Ok(())
//! # }
//! ```

use serde::de::DeserializeOwned;
use serde::Serialize;
use tikv_client::RawClient;

use syfrah_core::error::SyfrahError;

/// Distributed KV store backed by TiKV.
#[derive(Clone)]
pub struct ClusterDb {
    client: RawClient,
}

impl ClusterDb {
    /// Connect to a TiKV cluster via PD endpoints.
    pub async fn connect(pd_endpoints: &[&str]) -> Result<Self, SyfrahError> {
        let endpoints: Vec<String> = pd_endpoints.iter().map(|s| s.to_string()).collect();
        let client = RawClient::new(endpoints)
            .await
            .map_err(|e| SyfrahError::internal(format!("TiKV connect failed: {e}")))?;

        Ok(Self { client })
    }

    /// Put a serializable value.
    pub async fn put<T: Serialize>(
        &self,
        namespace: &str,
        key: &str,
        value: &T,
    ) -> Result<(), SyfrahError> {
        let full_key = format!("{namespace}/{key}");
        let data = serde_json::to_vec(value)
            .map_err(|e| SyfrahError::internal(format!("serialization: {e}")))?;
        self.client
            .put(full_key.into_bytes(), data)
            .await
            .map_err(|e| SyfrahError::internal(format!("TiKV put failed: {e}")))?;
        Ok(())
    }

    /// Get a deserializable value.
    pub async fn get<T: DeserializeOwned>(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<T>, SyfrahError> {
        let full_key = format!("{namespace}/{key}");
        let value = self
            .client
            .get(full_key.into_bytes())
            .await
            .map_err(|e| SyfrahError::internal(format!("TiKV get failed: {e}")))?;

        match value {
            Some(data) => {
                let parsed = serde_json::from_slice(&data)
                    .map_err(|e| SyfrahError::internal(format!("serialization: {e}")))?;
                Ok(Some(parsed))
            }
            None => Ok(None),
        }
    }

    /// Delete a key.
    pub async fn delete(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<(), SyfrahError> {
        let full_key = format!("{namespace}/{key}");
        self.client
            .delete(full_key.into_bytes())
            .await
            .map_err(|e| SyfrahError::internal(format!("TiKV delete failed: {e}")))?;
        Ok(())
    }

    /// List all keys with a prefix (scan).
    pub async fn list<T: DeserializeOwned>(
        &self,
        namespace: &str,
        prefix: &str,
    ) -> Result<Vec<(String, T)>, SyfrahError> {
        let full_prefix = format!("{namespace}/{prefix}");
        let end_key = prefix_end(&full_prefix);

        let pairs = self
            .client
            .scan(full_prefix.into_bytes()..end_key.into_bytes(), 10000)
            .await
            .map_err(|e| SyfrahError::internal(format!("TiKV scan failed: {e}")))?;

        let ns_prefix = format!("{namespace}/");
        let mut results = Vec::new();
        for pair in pairs {
            let key_bytes: Vec<u8> = Vec::from(pair.key().clone());
            let key_str = String::from_utf8(key_bytes).unwrap_or_default();
            let short_key = key_str.strip_prefix(&ns_prefix).unwrap_or(&key_str).to_string();
            let value: T = serde_json::from_slice(pair.value())
                .map_err(|e| SyfrahError::internal(format!("serialization: {e}")))?;
            results.push((short_key, value));
        }

        Ok(results)
    }

    /// Check if a key exists.
    pub async fn exists(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<bool, SyfrahError> {
        let full_key = format!("{namespace}/{key}");
        let value = self
            .client
            .get(full_key.into_bytes())
            .await
            .map_err(|e| SyfrahError::internal(format!("TiKV get failed: {e}")))?;
        Ok(value.is_some())
    }

    /// Batch put multiple key-value pairs atomically.
    pub async fn batch_put<T: Serialize>(
        &self,
        namespace: &str,
        entries: &[(&str, &T)],
    ) -> Result<(), SyfrahError> {
        let mut kvs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for (key, value) in entries {
            let full_key = format!("{namespace}/{key}");
            let data = serde_json::to_vec(value)
                .map_err(|e| SyfrahError::internal(format!("serialization: {e}")))?;
            kvs.push((full_key.into_bytes(), data));
        }

        self.client
            .batch_put(kvs)
            .await
            .map_err(|e| SyfrahError::internal(format!("TiKV batch_put failed: {e}")))?;

        Ok(())
    }
}

/// Compute the end key for a prefix scan (increment last byte).
fn prefix_end(prefix: &str) -> String {
    let mut bytes = prefix.as_bytes().to_vec();
    // Increment the last byte to get exclusive end
    while let Some(last) = bytes.last_mut() {
        if *last < 0xFF {
            *last += 1;
            return String::from_utf8(bytes).unwrap_or_default();
        }
        bytes.pop();
    }
    // All 0xFF — no end bound needed, but return empty
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_end_basic() {
        assert_eq!(prefix_end("vms/"), "vms0"); // '/' + 1 = '0'
    }

    #[test]
    fn prefix_end_alpha() {
        assert_eq!(prefix_end("abc"), "abd");
    }

    #[test]
    fn key_format() {
        let full = format!("{}/{}", "vms", "vm-001");
        assert_eq!(full, "vms/vm-001");
    }
}
