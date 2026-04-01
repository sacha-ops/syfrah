//! CLI commands for `syfrah controlplane`.

pub mod init;
pub mod join;
pub mod members;
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
}

/// Run a control plane CLI command.
pub async fn run(command: ControlPlaneCommand) -> anyhow::Result<()> {
    match command {
        ControlPlaneCommand::Init => init::run().await,
        ControlPlaneCommand::Join => join::run().await,
        ControlPlaneCommand::Status { json } => status::run(json).await,
        ControlPlaneCommand::Members => members::run().await,
    }
}
