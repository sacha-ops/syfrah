//! CLI commands for `syfrah org ...`, `syfrah project ...`, `syfrah env ...`, `syfrah vpc ...`, `syfrah subnet ...`, and `syfrah route ...`.

pub mod env;
pub mod nat_gw;
pub mod org;
pub mod project;
pub mod route;
pub mod sg;
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

/// Top-level route CLI command.
#[derive(Debug, Subcommand)]
pub enum RouteCommand {
    /// Manage route tables
    Table {
        #[command(subcommand)]
        action: RouteTableAction,
    },
    /// List routes in a VPC
    #[command(
        after_help = "Examples:\n  syfrah route list --vpc my-vpc\n  syfrah route list --vpc my-vpc --table default --json"
    )]
    List {
        /// VPC name
        #[arg(long)]
        vpc: Option<String>,
        /// Route table name (default: all tables in VPC)
        #[arg(long)]
        table: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Add a route to a route table
    #[command(
        after_help = "Examples:\n  syfrah route add --vpc my-vpc --destination 10.99.0.0/24 --target blackhole\n  syfrah route add --vpc my-vpc --destination 0.0.0.0/0 --target nat-gw:my-nat"
    )]
    Add {
        /// VPC name
        #[arg(long)]
        vpc: String,
        /// Destination CIDR
        #[arg(long)]
        destination: String,
        /// Target: local, blackhole, nat-gw:<name>, peering:<name>
        #[arg(long)]
        target: String,
        /// Route table name (default: "default")
        #[arg(long)]
        table: Option<String>,
        /// Priority (lower = evaluated first, default: 100)
        #[arg(long)]
        priority: Option<u32>,
    },
    /// Delete a route from a route table
    #[command(
        after_help = "Examples:\n  syfrah route delete --vpc my-vpc --destination 10.99.0.0/24"
    )]
    Delete {
        /// VPC name
        #[arg(long)]
        vpc: String,
        /// Destination CIDR to remove
        #[arg(long)]
        destination: String,
        /// Route table name (default: "default")
        #[arg(long)]
        table: Option<String>,
    },
}

/// Route table management subcommands.
#[derive(Debug, Subcommand)]
pub enum RouteTableAction {
    /// Create a new route table
    Create {
        /// Route table name
        name: String,
        /// VPC the route table belongs to
        #[arg(long)]
        vpc: String,
    },
    /// List route tables
    List {
        /// Filter by VPC name
        #[arg(long)]
        vpc: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a route table
    Delete {
        /// Route table name
        name: String,
        /// VPC the route table belongs to
        #[arg(long)]
        vpc: Option<String>,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Associate a route table with a subnet
    Associate {
        /// Route table name
        table: String,
        /// Subnet to associate
        #[arg(long)]
        subnet: String,
    },
    /// Disassociate a subnet from its custom route table (reverts to default)
    Disassociate {
        /// Subnet to disassociate
        #[arg(long)]
        subnet: String,
    },
}

/// Top-level NAT Gateway CLI command.
#[derive(Debug, Subcommand)]
pub enum NatGwCommand {
    /// Create a new NAT gateway
    #[command(
        after_help = "Examples:\n  syfrah nat-gw create main-gw --vpc acme-backend-default --subnet frontend"
    )]
    Create {
        /// NAT gateway name (lowercase alphanumeric and hyphens, 3-63 chars)
        #[arg(allow_hyphen_values = true)]
        name: String,
        /// VPC the NAT gateway belongs to
        #[arg(long)]
        vpc: String,
        /// Subnet to place the NAT gateway in
        #[arg(long)]
        subnet: String,
    },
    /// List NAT gateways
    #[command(
        after_help = "Examples:\n  syfrah nat-gw list --vpc acme-backend-default\n  syfrah nat-gw list --json"
    )]
    List {
        /// Filter by VPC name
        #[arg(long)]
        vpc: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show NAT gateway details
    #[command(after_help = "Examples:\n  syfrah nat-gw show main-gw")]
    Show {
        /// NAT gateway name
        name: String,
    },
    /// Delete a NAT gateway
    #[command(after_help = "Examples:\n  syfrah nat-gw delete main-gw --yes")]
    Delete {
        /// NAT gateway name
        name: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
}

/// Top-level SG CLI command.
#[derive(Debug, Subcommand)]
pub enum SgCommand {
    /// Create a new security group
    #[command(
        after_help = "Examples:\n  syfrah sg create web-sg --vpc my-vpc\n  syfrah sg create db-sg --vpc my-vpc --description \"Database tier\""
    )]
    Create {
        /// Security group name (lowercase alphanumeric and hyphens, 3-63 chars)
        #[arg(allow_hyphen_values = true)]
        name: String,
        /// VPC the security group belongs to
        #[arg(long)]
        vpc: String,
        /// Description of the security group
        #[arg(long)]
        description: Option<String>,
    },
    /// List security groups
    #[command(
        after_help = "Examples:\n  syfrah sg list --vpc my-vpc\n  syfrah sg list --vpc my-vpc --json"
    )]
    List {
        /// Filter by VPC name
        #[arg(long)]
        vpc: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show security group details (rules + attached VMs)
    #[command(
        after_help = "Examples:\n  syfrah sg show web-sg\n  syfrah sg show web-sg --vpc my-vpc"
    )]
    Show {
        /// Security group name
        name: String,
        /// VPC the security group belongs to (auto-detected if name is unique)
        #[arg(long)]
        vpc: Option<String>,
    },
    /// Delete a security group
    #[command(
        after_help = "Examples:\n  syfrah sg delete web-sg --yes\n  syfrah sg delete web-sg --vpc my-vpc --yes"
    )]
    Delete {
        /// Security group name
        name: String,
        /// VPC the security group belongs to (auto-detected if name is unique)
        #[arg(long)]
        vpc: Option<String>,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Attach a security group to a VM (via its primary NIC)
    #[command(
        after_help = "Examples:\n  syfrah sg attach web-sg --vm web-1\n  syfrah sg attach web-sg --nic nic-web-1"
    )]
    Attach {
        /// Security group name
        sg: String,
        /// VM name (resolves to primary NIC)
        #[arg(long, conflicts_with = "nic")]
        vm: Option<String>,
        /// NIC ID (direct)
        #[arg(long, conflicts_with = "vm")]
        nic: Option<String>,
    },
    /// Detach a security group from a VM (via its primary NIC)
    #[command(
        after_help = "Examples:\n  syfrah sg detach web-sg --vm web-1\n  syfrah sg detach web-sg --nic nic-web-1"
    )]
    Detach {
        /// Security group name
        sg: String,
        /// VM name (resolves to primary NIC)
        #[arg(long, conflicts_with = "nic")]
        vm: Option<String>,
        /// NIC ID (direct)
        #[arg(long, conflicts_with = "vm")]
        nic: Option<String>,
    },
    /// List security groups attached to a VM or NIC
    #[command(
        name = "list-attached",
        after_help = "Examples:\n  syfrah sg list-attached --vm web-1\n  syfrah sg list-attached --nic nic-web-1"
    )]
    ListAttached {
        /// VM name (resolves to primary NIC)
        #[arg(long, conflicts_with = "nic")]
        vm: Option<String>,
        /// NIC ID (direct)
        #[arg(long, conflicts_with = "vm")]
        nic: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Add a firewall rule to a security group
    #[command(
        name = "add-rule",
        after_help = "Examples:\n  \
            syfrah sg add-rule web-sg --direction ingress --protocol tcp --port 443 --source 0.0.0.0/0\n  \
            syfrah sg add-rule db-sg --direction ingress --protocol tcp --port 5432 --source-sg web-sg\n  \
            syfrah sg add-rule app-sg --direction egress --protocol tcp --port 8000-9000 --source 10.0.0.0/8"
    )]
    AddRule {
        /// Security group name
        sg: String,
        /// Rule direction: ingress or egress
        #[arg(long)]
        direction: String,
        /// Protocol: tcp, udp, icmp, or all
        #[arg(long)]
        protocol: String,
        /// Port number (e.g. 443) or range (e.g. 8000-9000)
        #[arg(long)]
        port: Option<String>,
        /// Source/destination as CIDR (e.g. 0.0.0.0/0)
        #[arg(long, conflicts_with = "source_sg")]
        source: Option<String>,
        /// Source/destination as security group name
        #[arg(long, conflicts_with = "source")]
        source_sg: Option<String>,
        /// Rule description
        #[arg(long)]
        description: Option<String>,
        /// Priority (lower = evaluated first, default: 100)
        #[arg(long)]
        priority: Option<u32>,
    },
    /// Remove a rule from a security group
    #[command(
        name = "remove-rule",
        after_help = "Examples:\n  syfrah sg remove-rule web-sg --rule-id rule-abc123"
    )]
    RemoveRule {
        /// Security group name
        sg: String,
        /// Rule ID to remove
        #[arg(long)]
        rule_id: String,
    },
    /// List rules in a security group
    #[command(
        name = "rules",
        after_help = "Examples:\n  syfrah sg rules web-sg\n  syfrah sg rules web-sg --json"
    )]
    Rules {
        /// Security group name
        sg: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Check if traffic would be allowed or denied by a VM's security groups
    #[command(
        name = "check",
        after_help = "Examples:\n  \
            syfrah sg check --vm web-1 --port 443 --protocol tcp\n  \
            syfrah sg check --vm web-1 --port 22 --protocol tcp --source 10.0.0.1"
    )]
    Check {
        /// VM name to evaluate
        #[arg(long)]
        vm: String,
        /// Port to check
        #[arg(long)]
        port: u16,
        /// Protocol: tcp, udp, icmp
        #[arg(long, default_value = "tcp")]
        protocol: String,
        /// Source IP address to check (default: 0.0.0.0 = any)
        #[arg(long)]
        source: Option<String>,
    },
}

/// Execute an org CLI command.
pub async fn run(cmd: OrgCommand) -> anyhow::Result<()> {
    match cmd {
        OrgCommand::Create { name } => org::run_create(name).await,
        OrgCommand::List { json } => org::run_list(json).await,
        OrgCommand::Delete { name, yes } => org::run_delete(name, yes).await,
    }
}

/// Execute a project CLI command.
pub async fn run_project(cmd: ProjectCommand) -> anyhow::Result<()> {
    match cmd {
        ProjectCommand::Create { name, org } => project::create(&name, &org).await,
        ProjectCommand::List { org, json } => project::list(org.as_deref(), json).await,
        ProjectCommand::Delete { name, org, yes } => project::delete(&name, &org, yes).await,
    }
}

/// Execute an env CLI command.
pub async fn run_env(cmd: EnvCommand) -> anyhow::Result<()> {
    match cmd {
        EnvCommand::Create {
            name,
            project,
            org,
            ttl,
            deletion_protection,
            labels,
        } => {
            env::run_create(
                &name,
                &project,
                &org,
                ttl.as_deref(),
                deletion_protection,
                &labels,
            )
            .await
        }
        EnvCommand::List { project, org, json } => {
            env::run_list(project.as_deref(), org.as_deref(), json).await
        }
        EnvCommand::Destroy {
            name,
            project,
            org,
            yes,
        } => env::run_destroy(&name, &project, &org, yes).await,
        EnvCommand::Extend {
            name,
            project,
            org,
            ttl,
        } => env::run_extend(&name, &project, &org, &ttl).await,
        EnvCommand::Update {
            name,
            project,
            org,
            deletion_protection,
            no_deletion_protection,
        } => {
            env::run_update(
                &name,
                &project,
                &org,
                deletion_protection,
                no_deletion_protection,
            )
            .await
        }
    }
}

/// Execute a VPC CLI command.
pub async fn run_vpc(cmd: VpcCommand) -> anyhow::Result<()> {
    match cmd {
        VpcCommand::Create {
            name,
            org,
            project,
            shared,
            cidr,
        } => vpc::run_create(&name, &org, project.as_deref(), shared, cidr.as_deref()).await,
        VpcCommand::List { project, org, json } => {
            vpc::run_list(org.as_deref(), project.as_deref(), json).await
        }
        VpcCommand::Delete { name, org, yes } => vpc::run_delete(&name, &org, yes).await,
        VpcCommand::Attach { vpc: v, project } => vpc::run_attach(&v, &project).await,
        VpcCommand::Detach { vpc: v, project } => vpc::run_detach(&v, &project).await,
        VpcCommand::Peer { from, to } => vpc::run_peer(&from, &to).await,
        VpcCommand::Unpeer { from, to } => vpc::run_unpeer(&from, &to).await,
        VpcCommand::Peerings { vpc, json } => vpc::run_peerings(vpc.as_deref(), json).await,
    }
}

/// Execute a subnet CLI command.
pub async fn run_subnet(cmd: SubnetCommand) -> anyhow::Result<()> {
    match cmd {
        SubnetCommand::Create {
            name,
            env,
            project,
            org,
            vpc,
            cidr,
        } => subnet::run_create(&name, &env, &project, &org, vpc.as_deref(), cidr.as_deref()).await,
        SubnetCommand::List {
            env,
            vpc,
            project,
            org,
            json,
        } => {
            subnet::run_list(
                env.as_deref(),
                vpc.as_deref(),
                project.as_deref(),
                org.as_deref(),
                json,
            )
            .await
        }
        SubnetCommand::Delete { name, vpc, yes } => {
            subnet::run_delete(&name, vpc.as_deref(), yes).await
        }
    }
}

/// Execute a security group CLI command.
pub async fn run_sg(cmd: SgCommand) -> anyhow::Result<()> {
    match cmd {
        SgCommand::Create {
            name,
            vpc,
            description,
        } => sg::run_create(&name, &vpc, description.as_deref().unwrap_or("")).await,
        SgCommand::List { vpc, json } => sg::run_list(vpc.as_deref(), json).await,
        SgCommand::Show { name, vpc } => sg::run_show(&name, vpc.as_deref()).await,
        SgCommand::Delete { name, vpc, yes } => sg::run_delete(&name, vpc.as_deref(), yes).await,
        SgCommand::Attach { sg, vm, nic } => {
            sg::run_attach(&sg, vm.as_deref(), nic.as_deref()).await
        }
        SgCommand::Detach { sg, vm, nic } => {
            sg::run_detach(&sg, vm.as_deref(), nic.as_deref()).await
        }
        SgCommand::ListAttached { vm, nic, json } => {
            sg::run_list_attached(vm.as_deref(), nic.as_deref(), json).await
        }
        SgCommand::AddRule {
            sg,
            direction,
            protocol,
            port,
            source,
            source_sg,
            description,
            priority,
        } => {
            sg::run_add_rule(
                &sg,
                &direction,
                &protocol,
                port.as_deref(),
                source.as_deref(),
                source_sg.as_deref(),
                description.as_deref(),
                priority,
            )
            .await
        }
        SgCommand::RemoveRule { sg, rule_id } => sg::run_remove_rule(&sg, &rule_id).await,
        SgCommand::Rules { sg, json } => sg::run_rules(&sg, json).await,
        SgCommand::Check {
            vm,
            port,
            protocol,
            source,
        } => sg::run_check(&vm, port, &protocol, source.as_deref()).await,
    }
}

/// Execute a NAT gateway CLI command.
pub async fn run_nat_gw(cmd: NatGwCommand) -> anyhow::Result<()> {
    match cmd {
        NatGwCommand::Create { name, vpc, subnet } => {
            nat_gw::run_create(&name, &vpc, &subnet).await
        }
        NatGwCommand::List { vpc, json } => nat_gw::run_list(vpc.as_deref(), json).await,
        NatGwCommand::Show { name } => nat_gw::run_show(&name).await,
        NatGwCommand::Delete { name, yes } => nat_gw::run_delete(&name, yes).await,
    }
}

/// Execute a route CLI command.
pub async fn run_route(cmd: RouteCommand) -> anyhow::Result<()> {
    match cmd {
        RouteCommand::Table { action } => match action {
            RouteTableAction::Create { name, vpc } => route::run_table_create(&name, &vpc).await,
            RouteTableAction::List { vpc, json } => {
                route::run_table_list(vpc.as_deref(), json).await
            }
            RouteTableAction::Delete { name, vpc, yes } => {
                route::run_table_delete(&name, vpc.as_deref(), yes).await
            }
            RouteTableAction::Associate { table, subnet } => {
                route::run_table_associate(&table, &subnet).await
            }
            RouteTableAction::Disassociate { subnet } => {
                route::run_table_disassociate(&subnet).await
            }
        },
        RouteCommand::List { vpc, table, json } => {
            route::run_list(vpc.as_deref(), table.as_deref(), json).await
        }
        RouteCommand::Add {
            vpc,
            destination,
            target,
            table,
            priority,
        } => route::run_add(&vpc, &destination, &target, table.as_deref(), priority).await,
        RouteCommand::Delete {
            vpc,
            destination,
            table,
        } => route::run_delete(&vpc, &destination, table.as_deref()).await,
    }
}
