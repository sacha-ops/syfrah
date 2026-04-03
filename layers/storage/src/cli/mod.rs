//! CLI commands for `syfrah volume ...` and `syfrah storage ...`.
//!
//! Provides subcommands for volume lifecycle management, storage
//! health/status inspection, and storage-layer utilities (e.g. ZeroFS
//! version). Each handler communicates with the daemon via the control
//! socket.

pub mod fmt;
pub mod health;
pub mod volume;

use clap::Subcommand;

/// Top-level volume CLI command.
#[derive(Debug, Subcommand)]
pub enum VolumeCommand {
    /// Create a new block volume
    #[command(after_help = "Examples:\n  \
            syfrah volume create pgdata --size 50 --project backend --org acme\n  \
            syfrah volume create redis-data --size 10 --project cache --org acme --env staging")]
    Create {
        /// Volume name (lowercase alphanumeric and hyphens, 3-63 chars)
        #[arg(allow_hyphen_values = true)]
        name: String,
        /// Size in gigabytes
        #[arg(long)]
        size: u64,
        /// Project the volume belongs to
        #[arg(long)]
        project: String,
        /// Organization the volume belongs to
        #[arg(long)]
        org: String,
        /// Environment (optional, for scoping)
        #[arg(long)]
        env: Option<String>,
    },
    /// List volumes
    #[command(after_help = "Examples:\n  \
            syfrah volume list\n  \
            syfrah volume list --project backend --org acme\n  \
            syfrah volume list --json")]
    List {
        /// Filter by project name
        #[arg(long)]
        project: Option<String>,
        /// Filter by organization name
        #[arg(long)]
        org: Option<String>,
        /// Filter by environment name
        #[arg(long)]
        env: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Get volume details
    #[command(after_help = "Examples:\n  \
            syfrah volume get pgdata\n  \
            syfrah volume get pgdata --project backend --json")]
    Get {
        /// Volume name
        name: String,
        /// Project the volume belongs to (auto-detected if name is unique)
        #[arg(long)]
        project: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a volume
    #[command(after_help = "Examples:\n  \
            syfrah volume delete pgdata --project backend\n  \
            syfrah volume delete pgdata --cascade")]
    Delete {
        /// Volume name
        name: String,
        /// Project the volume belongs to (auto-detected if name is unique)
        #[arg(long)]
        project: Option<String>,
        /// Also delete any snapshots derived from this volume
        #[arg(long)]
        cascade: bool,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Resize a volume (grow only)
    #[command(after_help = "Examples:\n  \
            syfrah volume resize pgdata --size 100\n  \
            syfrah volume resize pgdata --size 100 --project backend")]
    Resize {
        /// Volume name
        name: String,
        /// New size in gigabytes (must be larger than current size)
        #[arg(long)]
        size: u64,
        /// Project the volume belongs to (auto-detected if name is unique)
        #[arg(long)]
        project: Option<String>,
    },
    /// Update volume settings
    #[command(after_help = "Examples:\n  \
            syfrah volume update pgdata --deletion-protection\n  \
            syfrah volume update pgdata --no-deletion-protection")]
    Update {
        /// Volume name
        name: String,
        /// Project the volume belongs to (auto-detected if name is unique)
        #[arg(long)]
        project: Option<String>,
        /// Enable deletion protection
        #[arg(long, conflicts_with = "no_deletion_protection")]
        deletion_protection: bool,
        /// Disable deletion protection
        #[arg(long, conflicts_with = "deletion_protection")]
        no_deletion_protection: bool,
    },
}

/// Top-level storage CLI command (`syfrah storage ...`).
#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    /// Show ZeroFS binary version and path
    #[command(after_help = "Examples:\n  syfrah storage version")]
    Version,
    /// Run a health check against the S3 backend and cache subsystem
    #[command(after_help = "Examples:\n  \
            syfrah storage health\n  \
            syfrah storage health --json")]
    Health {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show storage status: connectivity, cache utilization, dirty bytes
    #[command(after_help = "Examples:\n  \
            syfrah storage status\n  \
            syfrah storage status --json")]
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Execute a storage CLI command.
pub async fn run_storage(cmd: StorageCommand) -> anyhow::Result<()> {
    match cmd {
        StorageCommand::Version => {
            let pinned = crate::binary::pinned_version();
            println!("zerofs pinned version: {pinned}");

            match crate::binary::resolve_binary(None) {
                Ok(path) => {
                    println!("zerofs binary: {}", path.display());
                    match crate::binary::check_version(&path) {
                        Ok(ver) => {
                            println!("zerofs disk version: {ver}");
                            if let Err(msg) = crate::binary::verify_version(&path) {
                                eprintln!("warning: {msg}");
                            }
                        }
                        Err(e) => {
                            eprintln!("warning: could not determine zerofs version: {e}");
                        }
                    }
                }
                Err(e) => {
                    println!("zerofs binary: not found ({e})");
                }
            }
            Ok(())
        }
        StorageCommand::Health { json } => health::run_health(json).await,
        StorageCommand::Status { json } => health::run_status(json).await,
    }
}

/// Execute a volume CLI command.
pub async fn run(cmd: VolumeCommand) -> anyhow::Result<()> {
    match cmd {
        VolumeCommand::Create {
            name,
            size,
            project,
            org,
            env,
        } => volume::run_create(&name, size, &project, &org, env.as_deref()).await,
        VolumeCommand::List {
            project,
            org,
            env,
            json,
        } => volume::run_list(project.as_deref(), org.as_deref(), env.as_deref(), json).await,
        VolumeCommand::Get {
            name,
            project,
            json,
        } => volume::run_get(&name, project.as_deref(), json).await,
        VolumeCommand::Delete {
            name,
            project,
            cascade,
            yes,
        } => volume::run_delete(&name, project.as_deref(), cascade, yes).await,
        VolumeCommand::Resize {
            name,
            size,
            project,
        } => volume::run_resize(&name, size, project.as_deref()).await,
        VolumeCommand::Update {
            name,
            project,
            deletion_protection,
            no_deletion_protection,
        } => {
            volume::run_update(
                &name,
                project.as_deref(),
                deletion_protection,
                no_deletion_protection,
            )
            .await
        }
    }
}
