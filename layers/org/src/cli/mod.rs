//! CLI commands for `syfrah org ...`, `syfrah project ...`, and `syfrah env ...`.

pub mod env;
pub mod org;
pub mod project;

use clap::Subcommand;

/// Top-level org CLI command.
#[derive(Debug, Subcommand)]
pub enum OrgCommand {
    /// Create a new organization
    Create {
        /// Organization name (lowercase alphanumeric + hyphens, 3-63 chars)
        name: String,
    },
    /// List organizations
    List,
    /// Delete an organization (must have no projects)
    Delete {
        /// Organization name
        name: String,
    },
}

/// Top-level project CLI command.
#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Create a new project
    Create {
        /// Project name
        name: String,
        /// Organization the project belongs to
        #[arg(long)]
        org: String,
    },
    /// List projects
    List {
        /// Filter by organization
        #[arg(long)]
        org: Option<String>,
    },
    /// Delete a project (must have no environments)
    Delete {
        /// Project name
        name: String,
        /// Organization the project belongs to
        #[arg(long)]
        org: String,
    },
}

/// Top-level env CLI command.
#[derive(Debug, Subcommand)]
pub enum EnvCommand {
    /// Create a new environment
    Create {
        /// Environment name
        name: String,
        /// Project the environment belongs to
        #[arg(long)]
        project: String,
        /// Organization
        #[arg(long)]
        org: String,
        /// Auto-destroy after this duration (e.g. 48h, 1h, 3d)
        #[arg(long)]
        ttl: Option<String>,
        /// Enable deletion protection
        #[arg(long)]
        deletion_protection: bool,
        /// Label in key=value format (can be repeated)
        #[arg(long = "label", value_name = "KEY=VALUE")]
        labels: Vec<String>,
    },
    /// List environments
    List {
        /// Filter by project
        #[arg(long)]
        project: Option<String>,
        /// Filter by organization
        #[arg(long)]
        org: Option<String>,
    },
    /// Update an environment
    Update {
        /// Environment name
        name: String,
        /// Project the environment belongs to
        #[arg(long)]
        project: String,
        /// Organization
        #[arg(long)]
        org: String,
        /// Enable deletion protection
        #[arg(long, conflicts_with = "no_deletion_protection")]
        deletion_protection: bool,
        /// Disable deletion protection
        #[arg(long, conflicts_with = "deletion_protection")]
        no_deletion_protection: bool,
    },
    /// Destroy an environment
    Destroy {
        /// Environment name
        name: String,
        /// Project the environment belongs to
        #[arg(long)]
        project: String,
        /// Organization
        #[arg(long)]
        org: String,
    },
}

/// Execute an org CLI command.
pub async fn run_org(cmd: OrgCommand) -> anyhow::Result<()> {
    org::run(cmd)
}

/// Execute a project CLI command.
pub async fn run_project(cmd: ProjectCommand) -> anyhow::Result<()> {
    project::run(cmd)
}

/// Execute an env CLI command.
pub async fn run_env(cmd: EnvCommand) -> anyhow::Result<()> {
    env::run(cmd)
}
