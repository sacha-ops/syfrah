//! CLI commands for `syfrah org ...`, `syfrah project ...`, `syfrah env ...`, `syfrah vpc ...`, and `syfrah subnet ...`.

pub mod env;
pub mod org;
pub mod project;
pub mod subnet;
pub mod vpc;

use clap::Subcommand;

/// Top-level org CLI command.
#[derive(Debug, Subcommand)]
pub enum OrgCommand {
    /// Create a new organization
    #[command(after_help = "Examples:\n  syfrah org create acme\n  syfrah org create my-company")]
    Create {
        /// Organization name (lowercase alphanumeric and hyphens, 3-63 chars)
        #[arg(allow_hyphen_values = true)]
        name: String,
    },
    /// List all organizations
    #[command(after_help = "Examples:\n  syfrah org list\n  syfrah org list --json")]
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete an organization
    #[command(after_help = "Examples:\n  syfrah org delete acme\n  syfrah org delete acme --yes")]
    Delete {
        /// Organization name
        name: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
}

/// Top-level project CLI command.
#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Create a new project under an organization
    Create {
        /// Project name (lowercase alphanumeric + hyphens, 3-63 chars)
        #[arg(allow_hyphen_values = true)]
        name: String,
        /// Organization this project belongs to
        #[arg(long)]
        org: String,
    },
    /// List projects
    List {
        /// Filter by organization
        #[arg(long)]
        org: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a project
    Delete {
        /// Project name
        name: String,
        /// Organization the project belongs to
        #[arg(long)]
        org: String,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

/// Top-level env CLI command.
#[derive(Debug, Subcommand)]
pub enum EnvCommand {
    /// Create a new environment
    Create {
        /// Environment name (lowercase alphanumeric + hyphens, 3-63 chars)
        #[arg(allow_hyphen_values = true)]
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
    /// Update environment settings (e.g. deletion protection)
    Update {
        /// Environment name
        name: String,
        /// Parent project name
        #[arg(long)]
        project: String,
        /// Parent organization name
        #[arg(long)]
        org: String,
        /// Enable deletion protection
        #[arg(long, conflicts_with = "no_deletion_protection")]
        deletion_protection: bool,
        /// Disable deletion protection
        #[arg(long, conflicts_with = "deletion_protection")]
        no_deletion_protection: bool,
    },
}

/// Top-level VPC CLI command.
#[derive(Debug, Subcommand)]
pub enum VpcCommand {
    /// Create a new VPC
    #[command(
        after_help = "Examples:\n  syfrah vpc create my-vpc --project backend --org acme\n  syfrah vpc create shared-net --org acme --shared --cidr 10.100.0.0/16"
    )]
    Create {
        /// VPC name (lowercase alphanumeric and hyphens, 3-63 chars)
        #[arg(allow_hyphen_values = true)]
        name: String,
        /// Organization the VPC belongs to
        #[arg(long)]
        org: String,
        /// Project the VPC belongs to (omit for shared VPCs)
        #[arg(long, conflicts_with = "shared")]
        project: Option<String>,
        /// Create a shared (org-level) VPC
        #[arg(long)]
        shared: bool,
        /// CIDR block for the VPC (default: 10.1.0.0/16 for project, 10.100.0.0/16 for shared)
        #[arg(long)]
        cidr: Option<String>,
    },
    /// List VPCs
    #[command(
        after_help = "Examples:\n  syfrah vpc list --org acme\n  syfrah vpc list --project backend --org acme --json"
    )]
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
    /// Delete a VPC
    #[command(
        after_help = "Examples:\n  syfrah vpc delete my-vpc --org acme\n  syfrah vpc delete my-vpc --org acme --yes"
    )]
    Delete {
        /// VPC name
        name: String,
        /// Organization the VPC belongs to
        #[arg(long)]
        org: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Attach a project to a shared VPC
    Attach {
        /// VPC name
        vpc: String,
        /// Project to attach (format: org/project)
        #[arg(long)]
        project: String,
    },
    /// Detach a project from a shared VPC
    Detach {
        /// VPC name
        vpc: String,
        /// Project to detach (format: org/project)
        #[arg(long)]
        project: String,
    },
    /// Create a peering between two VPCs
    #[command(after_help = "Examples:\n  syfrah vpc peer --from hub-vpc --to spoke-a-vpc")]
    Peer {
        /// Source VPC name
        #[arg(long)]
        from: String,
        /// Destination VPC name
        #[arg(long)]
        to: String,
    },
    /// Remove a peering between two VPCs
    #[command(after_help = "Examples:\n  syfrah vpc unpeer --from hub-vpc --to spoke-a-vpc")]
    Unpeer {
        /// Source VPC name
        #[arg(long)]
        from: String,
        /// Destination VPC name
        #[arg(long)]
        to: String,
    },
    /// List VPC peerings
    #[command(
        after_help = "Examples:\n  syfrah vpc peerings\n  syfrah vpc peerings --vpc hub-vpc\n  syfrah vpc peerings --json"
    )]
    Peerings {
        /// Filter by VPC name
        #[arg(long)]
        vpc: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Top-level subnet CLI command.
#[derive(Debug, Subcommand)]
pub enum SubnetCommand {
    /// Create a new subnet
    #[command(
        after_help = "Examples:\n  syfrah subnet create frontend --env production --project backend --org acme\n  syfrah subnet create database --env production --project backend --org acme --vpc my-vpc --cidr 10.1.2.0/24"
    )]
    Create {
        /// Subnet name (lowercase alphanumeric and hyphens, 3-63 chars)
        #[arg(allow_hyphen_values = true)]
        name: String,
        /// Environment the subnet belongs to
        #[arg(long)]
        env: String,
        /// Project the subnet belongs to
        #[arg(long)]
        project: String,
        /// Organization the subnet belongs to
        #[arg(long)]
        org: String,
        /// VPC to create the subnet in (default: project's default VPC, auto-created if needed)
        #[arg(long)]
        vpc: Option<String>,
        /// CIDR block for the subnet (default: auto-allocate next /24 within VPC)
        #[arg(long)]
        cidr: Option<String>,
    },
    /// List subnets
    #[command(
        after_help = "Examples:\n  syfrah subnet list --env production --project backend --org acme\n  syfrah subnet list --vpc my-vpc --json"
    )]
    List {
        /// Filter by environment name
        #[arg(long)]
        env: Option<String>,
        /// Filter by VPC name
        #[arg(long)]
        vpc: Option<String>,
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
    /// Delete a subnet
    #[command(
        after_help = "Examples:\n  syfrah subnet delete frontend --yes\n  syfrah subnet delete frontend --vpc my-vpc --yes"
    )]
    Delete {
        /// Subnet name
        name: String,
        /// VPC the subnet belongs to (auto-detected if omitted and name is unique)
        #[arg(long)]
        vpc: Option<String>,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
}

/// Execute an org CLI command.
pub fn run(cmd: OrgCommand) -> anyhow::Result<()> {
    match cmd {
        OrgCommand::Create { name } => org::run_create(name),
        OrgCommand::List { json } => org::run_list(json),
        OrgCommand::Delete { name, yes } => org::run_delete(name, yes),
    }
}

/// Execute a project CLI command.
pub fn run_project(cmd: ProjectCommand) -> anyhow::Result<()> {
    match cmd {
        ProjectCommand::Create { name, org } => project::create(&name, &org),
        ProjectCommand::List { org, json } => project::list(org.as_deref(), json),
        ProjectCommand::Delete { name, org, yes } => project::delete(&name, &org, yes),
    }
}

/// Execute an env CLI command.
pub fn run_env(cmd: EnvCommand) -> anyhow::Result<()> {
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
        EnvCommand::Update {
            name,
            project,
            org,
            deletion_protection,
            no_deletion_protection,
        } => env::run_update(
            &name,
            &project,
            &org,
            deletion_protection,
            no_deletion_protection,
        ),
    }
}

/// Execute a VPC CLI command.
pub fn run_vpc(cmd: VpcCommand) -> anyhow::Result<()> {
    match cmd {
        VpcCommand::Create {
            name,
            org,
            project,
            shared,
            cidr,
        } => vpc::run_create(&name, &org, project.as_deref(), shared, cidr.as_deref()),
        VpcCommand::List { project, org, json } => {
            vpc::run_list(org.as_deref(), project.as_deref(), json)
        }
        VpcCommand::Delete { name, org, yes } => vpc::run_delete(&name, &org, yes),
        VpcCommand::Attach { vpc: v, project } => vpc::run_attach(&v, &project),
        VpcCommand::Detach { vpc: v, project } => vpc::run_detach(&v, &project),
        VpcCommand::Peer { from, to } => vpc::run_peer(&from, &to),
        VpcCommand::Unpeer { from, to } => vpc::run_unpeer(&from, &to),
        VpcCommand::Peerings { vpc, json } => vpc::run_peerings(vpc.as_deref(), json),
    }
}

/// Execute a subnet CLI command.
pub fn run_subnet(cmd: SubnetCommand) -> anyhow::Result<()> {
    match cmd {
        SubnetCommand::Create {
            name,
            env,
            project,
            org,
            vpc,
            cidr,
        } => subnet::run_create(&name, &env, &project, &org, vpc.as_deref(), cidr.as_deref()),
        SubnetCommand::List {
            env,
            vpc,
            project,
            org,
            json,
        } => subnet::run_list(
            env.as_deref(),
            vpc.as_deref(),
            project.as_deref(),
            org.as_deref(),
            json,
        ),
        SubnetCommand::Delete { name, vpc, yes } => subnet::run_delete(&name, vpc.as_deref(), yes),
    }
}
