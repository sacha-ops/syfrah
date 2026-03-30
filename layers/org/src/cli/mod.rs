//! CLI commands for `syfrah env ...`.
//!
//! Provides subcommands for environment lifecycle management within the
//! organization hierarchy.

pub mod env;

use clap::Subcommand;

/// Top-level env CLI command.
#[derive(Debug, Subcommand)]
pub enum EnvCommand {
    /// Create a new environment
    Create {
        /// Environment name (lowercase alphanumeric, hyphens, forward slashes, 3-63 chars)
        name: String,

        /// Parent project name
        #[arg(long)]
        project: String,

        /// Parent organization name
        #[arg(long)]
        org: String,

        /// Time-to-live before auto-destroy (e.g. 30m, 2h, 48h, 7d)
        #[arg(long)]
        ttl: Option<String>,

        /// Enable deletion protection
        #[arg(long)]
        deletion_protection: bool,

        /// Labels as key=value pairs (repeatable)
        #[arg(long = "label", value_name = "KEY=VALUE")]
        labels: Vec<String>,
    },

    /// List environments
    List {
        /// Filter by project name
        #[arg(long)]
        project: Option<String>,

        /// Filter by organization name
        #[arg(long)]
        org: Option<String>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Destroy an environment
    Destroy {
        /// Environment name
        name: String,

        /// Parent project name
        #[arg(long)]
        project: String,

        /// Parent organization name
        #[arg(long)]
        org: String,

        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },

    /// Extend the TTL of an environment
    Extend {
        /// Environment name
        name: String,

        /// Parent project name
        #[arg(long)]
        project: String,

        /// Parent organization name
        #[arg(long)]
        org: String,

        /// New time-to-live from now (e.g. 30m, 2h, 48h, 7d)
        #[arg(long)]
        ttl: String,
    },
}

/// Execute an env CLI command.
pub fn run(cmd: EnvCommand) -> anyhow::Result<()> {
    match cmd {
        EnvCommand::Create {
            name,
            project,
            org,
            ttl,
            deletion_protection,
            labels,
        } => env::run_create(
            &name,
            &project,
            &org,
            ttl.as_deref(),
            deletion_protection,
            &labels,
        ),
        EnvCommand::List { project, org, json } => {
            env::run_list(project.as_deref(), org.as_deref(), json)
        }
        EnvCommand::Destroy {
            name,
            project,
            org,
            yes,
        } => env::run_destroy(&name, &project, &org, yes),
        EnvCommand::Extend {
            name,
            project,
            org,
            ttl,
        } => env::run_extend(&name, &project, &org, &ttl),
    }
}
