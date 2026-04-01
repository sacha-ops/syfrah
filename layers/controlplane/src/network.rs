//! Raft network implementation — HTTP/JSON over syfrah0 on port 7200.

use std::future::Future;
use std::io;

use openraft::errors::{RPCError, ReplicationClosed, StreamingError, Unreachable};
use openraft::network::v2::RaftNetworkV2;
use openraft::network::{RPCOption, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, SnapshotResponse, VoteRequest, VoteResponse,
};
use openraft::type_config::alias::{SnapshotOf, VoteOf};
use openraft::OptionalSend;

use crate::types::{SyfrahNode, SyfrahRaftConfig};

/// Factory that creates network clients for each target node.
pub struct SyfrahNetworkFactory {
    client: reqwest::Client,
}

impl SyfrahNetworkFactory {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("failed to build reqwest client"),
        }
    }
}

impl Default for SyfrahNetworkFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// A network client for a single target Raft node.
pub struct SyfrahNetwork {
    target_addr: String,
    client: reqwest::Client,
}

impl RaftNetworkFactory<SyfrahRaftConfig> for SyfrahNetworkFactory {
    type Network = SyfrahNetwork;

    async fn new_client(&mut self, _target: u64, node: &SyfrahNode) -> Self::Network {
        SyfrahNetwork {
            target_addr: node.addr.clone(),
            client: self.client.clone(),
        }
    }
}

impl RaftNetworkV2<SyfrahRaftConfig> for SyfrahNetwork {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<SyfrahRaftConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<SyfrahRaftConfig>, RPCError<SyfrahRaftConfig>> {
        let url = format!("http://{}/raft/append_entries", self.target_addr);
        let resp = self
            .client
            .post(&url)
            .json(&rpc)
            .send()
            .await
            .map_err(|e| RPCError::Unreachable(Unreachable::new(&e)))?;

        let result: AppendEntriesResponse<SyfrahRaftConfig> = resp
            .json()
            .await
            .map_err(|e| RPCError::Unreachable(Unreachable::new(&e)))?;
        Ok(result)
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<SyfrahRaftConfig>,
        _option: RPCOption,
    ) -> Result<VoteResponse<SyfrahRaftConfig>, RPCError<SyfrahRaftConfig>> {
        let url = format!("http://{}/raft/vote", self.target_addr);
        let resp = self
            .client
            .post(&url)
            .json(&rpc)
            .send()
            .await
            .map_err(|e| RPCError::Unreachable(Unreachable::new(&e)))?;

        let result: VoteResponse<SyfrahRaftConfig> = resp
            .json()
            .await
            .map_err(|e| RPCError::Unreachable(Unreachable::new(&e)))?;
        Ok(result)
    }

    async fn full_snapshot(
        &mut self,
        vote: VoteOf<SyfrahRaftConfig>,
        snapshot: SnapshotOf<SyfrahRaftConfig>,
        _cancel: impl Future<Output = ReplicationClosed> + OptionalSend + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<SyfrahRaftConfig>, StreamingError<SyfrahRaftConfig>> {
        let url = format!("http://{}/raft/install_snapshot", self.target_addr);

        #[derive(serde::Serialize)]
        struct SnapshotReq {
            vote: VoteOf<SyfrahRaftConfig>,
            meta: openraft::alias::SnapshotMetaOf<SyfrahRaftConfig>,
            data: Vec<u8>,
        }

        let req = SnapshotReq {
            vote,
            meta: snapshot.meta,
            data: snapshot.snapshot.into_inner(),
        };

        let resp = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(|e| {
                StreamingError::Unreachable(Unreachable::new(&io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    e.to_string(),
                )))
            })?;

        let result: SnapshotResponse<SyfrahRaftConfig> = resp.json().await.map_err(|e| {
            StreamingError::Unreachable(Unreachable::new(&io::Error::new(
                io::ErrorKind::InvalidData,
                e.to_string(),
            )))
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openraft::network::RaftNetworkFactory;

    #[tokio::test]
    async fn factory_creates_client() {
        let mut factory = SyfrahNetworkFactory::new();
        let node = SyfrahNode {
            addr: "[::1]:7200".to_string(),
        };
        let network = factory.new_client(1, &node).await;
        assert_eq!(network.target_addr, "[::1]:7200");
    }

    #[test]
    fn factory_default() {
        let _ = SyfrahNetworkFactory::default();
    }
}
