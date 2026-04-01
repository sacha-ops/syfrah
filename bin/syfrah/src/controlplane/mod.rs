//! CLI commands for `syfrah controlplane`.

pub mod init;
pub mod status;

use clap::Subcommand;

/// Control plane management commands.
#[derive(Subcommand)]
pub enum ControlPlaneCommand {
    /// Initialize the control plane (single-node Raft bootstrap)
    Init,
    /// Show control plane status (Raft leader, term, members)
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Run a control plane CLI command.
pub async fn run(command: ControlPlaneCommand) -> anyhow::Result<()> {
    match command {
        ControlPlaneCommand::Init => init::run().await,
        ControlPlaneCommand::Status { json } => status::run(json).await,
    }
}
