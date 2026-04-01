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
pub mod remote_create;
pub mod reschedule;
pub mod scheduler;
pub mod server;
pub mod state_machine;
pub mod types;

pub use client::{RaftClient, RaftMetricsSnapshot};
pub use commands::{StateMachineCommand, StateMachineResponse};
pub use gossip::{
    GossipCluster, GossipConfig, GossipMetricsSnapshot, GossipNodeId, HypervisorGossipReport,
    MemberState,
};
pub use idempotency::IdempotencyJournal;
pub use log_storage::RedbLogStore;
pub use network::{SyfrahNetwork, SyfrahNetworkFactory};
pub use remote_create::{
    create_vm_on_remote, forge_addr_from_fabric_ipv6, RemoteCreateVmRequest, RemoteCreateVmResponse,
};
pub use reschedule::{RescheduleOutcome, RescheduleSummary, Rescheduler, VmPlacementInfo};
pub use scheduler::{
    AdmissionResult, PlacementConstraints, PlacementDecision, Scheduler, SchedulerError,
    MAX_ADMISSION_RETRIES,
};
pub use server::RaftServer;
pub use state_machine::{
    FullSnapshotData, PlacementEvent, RedbStateMachine, DEFAULT_SNAPSHOT_THRESHOLD,
};
pub use types::{SyfrahNode, SyfrahRaftConfig};

/// The concrete Raft type for Syfrah.
pub type SyfrahRaft = openraft::Raft<SyfrahRaftConfig, std::sync::Arc<RedbStateMachine>>;
