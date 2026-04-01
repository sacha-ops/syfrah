//! CLI commands for `syfrah controlplane`.

pub mod init;
pub mod join;
pub mod members;
pub mod promote;
pub mod status;

use clap::Subcommand;

/// Control plane management commands.
#[derive(Subcommand)]
pub enum ControlPlaneCommand {
    /// Initialize the control plane (single-node Raft bootstrap)
    Init,
    /// Join an existing Raft cluster
    Join,
    /// Show control plane status (Raft leader, term, members)
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List all Raft cluster members with their roles
    Members,
    /// Promote a learner to voter
    Promote {
        /// Node name to promote (e.g. hv-eu-2)
        node: String,
    },
    /// Demote a voter to learner
    Demote {
        /// Node name to demote (e.g. hv-eu-2)
        node: String,
    },
}

/// Run a control plane CLI command.
pub async fn run(command: ControlPlaneCommand) -> anyhow::Result<()> {
    match command {
        ControlPlaneCommand::Init => init::run().await,
        ControlPlaneCommand::Join => join::run().await,
        ControlPlaneCommand::Status { json } => status::run(json).await,
        ControlPlaneCommand::Members => members::run().await,
        ControlPlaneCommand::Promote { node } => promote::run_promote(&node).await,
        ControlPlaneCommand::Demote { node } => promote::run_demote(&node).await,
    }
}
