//! Control plane layer — distributed consensus via Raft for Syfrah.
//!
//! Uses openraft to replicate state machine commands across cluster nodes.
//! Phase 2: mutations routed through Raft, multi-node coordination.

pub mod client;
pub mod commands;
pub mod gossip;
pub mod idempotency;
pub mod log_storage;
pub mod network;
pub mod scheduler;
pub mod server;
pub mod state_machine;
pub mod types;

pub use client::RaftClient;
pub use commands::{StateMachineCommand, StateMachineResponse};
pub use gossip::{GossipCluster, GossipConfig, GossipNodeId, HypervisorGossipReport, MemberState};
pub use idempotency::IdempotencyJournal;
pub use log_storage::RedbLogStore;
pub use network::{SyfrahNetwork, SyfrahNetworkFactory};
pub use scheduler::{
    AdmissionResult, PlacementConstraints, PlacementDecision, Scheduler, SchedulerError,
    MAX_ADMISSION_RETRIES,
};
pub use server::RaftServer;
pub use state_machine::{PlacementEvent, RedbStateMachine};
pub use types::{SyfrahNode, SyfrahRaftConfig};

/// The concrete Raft type for Syfrah.
pub type SyfrahRaft = openraft::Raft<SyfrahRaftConfig, std::sync::Arc<RedbStateMachine>>;
