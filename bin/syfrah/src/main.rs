use std::net::SocketAddr;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};

use syfrah_core::mesh::{Region, Zone};
use syfrah_fabric::cli;
use syfrah_fabric::daemon::{self, DaemonConfig};
use syfrah_state::cli::StateCommand;

mod update;

#[derive(Parser)]
#[command(
    name = "syfrah",
    about = "Syfrah — turn dedicated servers into a programmable cloud",
    version,
    after_help = "TIP: Generate shell completions with 'syfrah completions {bash,zsh,fish}'. \
                  Run 'syfrah completions --help' for setup instructions."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage the WireGuard fabric mesh
    Fabric {
        #[command(subcommand)]
        command: FabricCommand,
    },
    /// Manage virtual machines and compute resources
    Compute {
        #[command(subcommand)]
        command: syfrah_compute::cli::ComputeCommand,
    },
    /// Inspect and manage layer state databases
    State {
        #[command(subcommand)]
        command: StateCommand,
    },
    /// Generate shell completions for bash, zsh, or fish
    #[command(after_help = "\
Setup instructions:

  Bash:
    syfrah completions bash > /etc/bash_completion.d/syfrah

  Zsh:
    syfrah completions zsh > ~/.zfunc/_syfrah
    # Then add ~/.zfunc to your fpath in ~/.zshrc (before compinit):
    #   fpath=(~/.zfunc $fpath)
    #   autoload -Uz compinit && compinit

  Fish:
    syfrah completions fish > ~/.config/fish/completions/syfrah.fish

After installing, restart your shell or source the completions file.")]
    Completions {
        /// The shell to generate completions for
        shell: Shell,
    },
    /// Update syfrah to the latest release
    Update {
        /// Only check if an update is available, don't install
        #[arg(long)]
        check: bool,
        /// Skip automatic daemon restart (print manual instructions instead)
        #[arg(long)]
        no_restart: bool,
        /// Skip the confirmation prompt when the node has active peer connections
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum FabricCommand {
    /// Create a new mesh network
    Init {
        #[arg(long)]
        name: String,
        #[arg(long)]
        node_name: Option<String>,
        #[arg(long, default_value = "51820")]
        port: u16,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        /// Port for the peering protocol [default: WireGuard port + 1]
        #[arg(long)]
        peering_port: Option<u16>,
        /// Region label for this node [default: "default"]
        #[arg(long)]
        region: Option<String>,
        /// Zone label for this node (auto-incremented if not set)
        #[arg(long)]
        zone: Option<String>,
        /// Run daemon in foreground instead of backgrounding
        #[arg(long, short)]
        foreground: bool,
        /// Start peering with auto-accept PIN after init
        #[arg(long)]
        peering: bool,
    },
    /// Join an existing mesh
    Join {
        /// IP or IP:port of an existing node (default port: 51821)
        target: String,
        #[arg(long)]
        node_name: Option<String>,
        #[arg(long, default_value = "51820")]
        port: u16,
        #[arg(long)]
        endpoint: Option<SocketAddr>,
        /// Port for the peering protocol [default: WireGuard port + 1]
        #[arg(long)]
        peering_port: Option<u16>,
        /// PIN for auto-accept (skip manual approval)
        #[arg(long)]
        pin: Option<String>,
        /// Region label for this node [default: "default"]
        #[arg(long)]
        region: Option<String>,
        /// Zone label for this node (auto-incremented if not set)
        #[arg(long)]
        zone: Option<String>,
        /// Run daemon in foreground instead of backgrounding
        #[arg(long, short)]
        foreground: bool,
    },
    /// Start the daemon from saved state
    Start {
        /// Run daemon in foreground instead of backgrounding
        #[arg(long, short)]
        foreground: bool,
    },
    /// Stop the running daemon
    Stop,
    /// Show mesh and daemon status
    Status {
        /// Show config and metrics sections
        #[arg(long)]
        verbose: bool,
        /// Show the full mesh secret (masked by default)
        #[arg(long)]
        show_secret: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the event log
    Events {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Maximum number of events to display (most recent first)
        #[arg(long)]
        limit: Option<usize>,
        /// Only show events after this Unix timestamp
        #[arg(long)]
        since: Option<u64>,
    },
    /// Show the security audit log
    Audit {
        /// Only show events after this Unix timestamp
        #[arg(long)]
        since: Option<u64>,
        /// Filter by event type (e.g. peer.join.accepted, secret.rotated)
        #[arg(long, name = "type")]
        event_type: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Maximum number of entries to display (most recent first, default 20)
        #[arg(long)]
        limit: Option<usize>,
    },
    /// List and manage peers
    Peers {
        #[command(subcommand)]
        action: Option<PeersAction>,
        /// Output as JSON (for listing peers)
        #[arg(long)]
        json: bool,
        /// Group output by region/zone (tree view)
        #[arg(long)]
        topology: bool,
        /// Filter peers by region
        #[arg(long)]
        region: Option<String>,
        /// Filter peers by zone
        #[arg(long)]
        zone: Option<String>,
    },
    /// Show the mesh secret
    Token,
    /// Rotate the mesh secret
    Rotate {
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Leave the mesh, tear down interface, clear state
    Leave {
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Run diagnostic checks on the fabric
    Diagnose {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Filter diagnostics to a specific zone (e.g. eu-west/par-ovh)
        #[arg(long)]
        zone: Option<String>,
    },
    /// Reload config.toml without restarting the daemon
    Reload,
    /// Show mesh topology grouped by region and zone
    Topology {
        /// Filter to a single region
        #[arg(long)]
        region: Option<String>,
        /// Filter to a single zone
        #[arg(long)]
        zone: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Include per-node endpoint, handshake, and traffic
        #[arg(long)]
        verbose: bool,
    },
    /// Export metrics in Prometheus text format
    Metrics,
    /// Manage the systemd service
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Manage peering — accept/reject join requests
    Peering {
        #[command(subcommand)]
        action: PeeringAction,
    },
    /// Manage zone drain state for workload scheduling
    Zone {
        #[command(subcommand)]
        action: ZoneAction,
    },
    /// Print /etc/hosts entries for all mesh peers
    Hosts {
        /// Write entries directly to /etc/hosts (requires root)
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum PeeringAction {
    /// Start accepting join requests (non-interactive)
    Start {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        pin: Option<String>,
    },
    /// Stop accepting join requests
    Stop,
    /// List pending join requests
    List,
    /// Watch for join requests interactively
    Watch {
        /// PIN for auto-accept mode
        #[arg(long)]
        pin: Option<String>,
        /// Stay open to accept multiple join requests
        #[arg(long, name = "continuous")]
        continuous: bool,
    },
    /// Accept a join request
    Accept { request_id: String },
    /// Reject a join request
    Reject {
        request_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
enum PeersAction {
    /// Remove a peer from the mesh
    Remove {
        /// Peer node name or WireGuard public key
        name_or_key: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Update a peer's endpoint without rejoin
    Update {
        /// Peer node name or WireGuard public key
        name: String,
        /// New endpoint address (ip:port)
        #[arg(long)]
        endpoint: SocketAddr,
    },
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Install and enable the systemd service
    Install,
    /// Disable and remove the systemd service
    Uninstall,
    /// Show systemd service status
    Status,
}

#[derive(Subcommand)]
enum ZoneAction {
    /// Mark a zone as draining (stops new workload placement)
    Drain {
        /// Zone path in region/zone format (e.g. eu-west/par-ovh)
        zone_path: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Restore a draining zone to active
    Undrain {
        /// Zone path in region/zone format (e.g. eu-west/par-ovh)
        zone_path: String,
    },
    /// Show all zones with health and drain status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Maximum allowed length for mesh and node names.
const MAX_NAME_LEN: usize = 64;

/// Validate port configuration for fabric init/join.
fn validate_ports(port: u16, peering_port: Option<u16>) -> Result<u16> {
    // Port overflow: default peering port is port + 1, which overflows at 65535
    let resolved = match peering_port {
        Some(pp) => pp,
        None => {
            if port == 65535 {
                anyhow::bail!(
                    "Port 65535 cannot use default peering port (65536 overflows). \
                     Set --peering-port explicitly."
                );
            }
            port + 1
        }
    };

    // Port conflict: both ports must differ
    if resolved == port {
        anyhow::bail!("--peering-port must differ from --port (both are {port})");
    }

    // Privileged port warning (non-blocking)
    if port < 1024 {
        eprintln!("Warning: port {port} is privileged (< 1024). The daemon must run as root.");
    }
    if resolved < 1024 {
        eprintln!(
            "Warning: peering port {resolved} is privileged (< 1024). The daemon must run as root."
        );
    }

    Ok(resolved)
}

/// Validate name length for mesh and node names.
fn validate_name(label: &str, value: &str) -> Result<()> {
    if value.len() > MAX_NAME_LEN {
        anyhow::bail!(
            "{label} must be {MAX_NAME_LEN} characters or fewer (got {})",
            value.len()
        );
    }
    Ok(())
}

/// Validate and resolve `--region` / `--zone` CLI args using the typed
/// constructors from `syfrah-core`. Returns actionable error messages when
/// the input is invalid, and emits a warning when `--region` is omitted.
fn validate_region(raw: &Option<String>) -> Result<Option<String>> {
    match raw {
        None => {
            eprintln!("Warning: --region not specified. Using 'default'. Set --region for meaningful topology.");
            Ok(None)
        }
        Some(value) => {
            if Region::new(value).is_some() {
                Ok(Some(value.clone()))
            } else {
                // Produce an actionable suggestion depending on the failure mode.
                let suggestion = suggest_fix(value);
                anyhow::bail!("Region name '{value}' is invalid. {suggestion}");
            }
        }
    }
}

fn validate_zone(raw: &Option<String>) -> Result<Option<String>> {
    match raw {
        None => Ok(None),
        Some(value) => {
            if Zone::new(value).is_some() {
                Ok(Some(value.clone()))
            } else {
                let suggestion = suggest_fix(value);
                anyhow::bail!("Zone name '{value}' is invalid. {suggestion}");
            }
        }
    }
}

/// Generate a human-friendly fix suggestion for an invalid region/zone name.
fn suggest_fix(value: &str) -> String {
    use syfrah_core::mesh::MAX_REGION_ZONE_LENGTH;

    if value.len() > MAX_REGION_ZONE_LENGTH {
        return format!("Name too long (max {MAX_REGION_ZONE_LENGTH} characters).");
    }
    if value.is_empty() {
        return "Name must not be empty.".to_string();
    }
    // Check if it just has uppercase — suggest the lowercase version.
    let lowered = value.to_ascii_lowercase();
    if Region::new(&lowered).is_some() {
        return format!("Use lowercase: '{lowered}'.");
    }
    // Replace common bad chars with hyphens for a suggestion.
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if !trimmed.is_empty() && Region::new(trimmed).is_some() {
        return format!("Use alphanumeric + hyphens: '{trimmed}'.");
    }
    "Use lowercase alphanumeric characters and hyphens (e.g. 'eu-west').".to_string()
}

fn default_node_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "syfrah-node".into())
}

fn setup_logging(daemon_mode: bool) {
    let json_mode = std::env::var("SYFRAH_LOG_FORMAT")
        .map(|v| v == "json")
        .unwrap_or(false);

    if daemon_mode {
        let tuning = syfrah_fabric::config::load_tuning().unwrap_or_default();
        let log_max_bytes = tuning.log_max_size_mb * 1024 * 1024;

        let log_path = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".syfrah")
            .join("syfrah.log");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
        if let Ok(meta) = std::fs::metadata(&log_path) {
            if meta.len() > log_max_bytes {
                let old = log_path.with_extension("log.old");
                let _ = std::fs::rename(&log_path, &old);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o600));
                }
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("failed to open log file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600));
        }
        if json_mode {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .with_writer(file)
                .with_ansi(false)
                .json()
                .init();
        } else {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .with_writer(file)
                .with_ansi(false)
                .init();
        }
    } else if json_mode {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }
}

/// Spawn the daemon in background via double-fork + re-exec with `start --foreground`.
/// State must already be saved before calling this.
///
/// Uses the classic double-fork pattern so that:
/// 1. Parent forks -> intermediate child
/// 2. Intermediate forks -> grandchild (the daemon), then exits
/// 3. Parent reaps intermediate (no zombie)
/// 4. Grandchild calls setsid + exec, is reparented to init
#[cfg(unix)]
fn background_daemon() -> Result<()> {
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let exe = std::env::current_exe()?;

    // Open the log file for stderr so we can capture startup errors
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".syfrah");
    let _ = std::fs::create_dir_all(&log_dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o700));
    }
    let log_path = log_dir.join("syfrah.log");

    // First fork
    let pid1 = unsafe { libc::fork() };
    if pid1 < 0 {
        anyhow::bail!("fork failed");
    }
    if pid1 == 0 {
        // Intermediate child: setsid, fork again, then exit
        unsafe { libc::setsid() };

        let pid2 = unsafe { libc::fork() };
        if pid2 < 0 {
            std::process::exit(1);
        }
        if pid2 > 0 {
            // Intermediate exits immediately — grandchild becomes orphan
            std::process::exit(0);
        }

        // Grandchild: redirect stdio and exec the daemon
        unsafe {
            let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            if devnull >= 0 {
                libc::dup2(devnull, 0); // stdin
                libc::dup2(devnull, 1); // stdout
                if devnull > 2 {
                    libc::close(devnull);
                }
            }
            // Redirect stderr to log file
            if let Ok(log_cstr) = std::ffi::CString::new(log_path.to_string_lossy().as_bytes()) {
                let log_fd = libc::open(
                    log_cstr.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                    0o600,
                );
                if log_fd >= 0 {
                    libc::dup2(log_fd, 2); // stderr
                    if log_fd > 2 {
                        libc::close(log_fd);
                    }
                }
            }
        }

        // Exec the daemon — replaces current process
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe)
            .args(["fabric", "start", "--foreground"])
            .exec();
        // If exec fails, write error and exit
        eprintln!("exec failed: {err}");
        std::process::exit(1);
    }

    // Parent: wait for intermediate child to exit (reap it, no zombie)
    unsafe {
        let mut status: libc::c_int = 0;
        libc::waitpid(pid1, &mut status, 0);
    }

    // Wait for the grandchild daemon to start and write its PID file.
    // Poll up to 5s — the daemon needs time to open redb, set up WG, etc.
    let mut daemon_pid = None;
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(pid) = syfrah_fabric::store::daemon_running() {
            daemon_pid = Some(pid);
            break;
        }
    }
    match daemon_pid {
        Some(pid) => {
            println!("Daemon started (pid {pid}).");
        }
        None => {
            eprintln!("Warning: daemon may have failed to start. Check logs: ~/.syfrah/syfrah.log");
        }
    }
    println!("Run 'syfrah fabric status' to check.");
    Ok(())
}

#[cfg(not(unix))]
fn background_daemon() -> Result<()> {
    anyhow::bail!("background daemon is only supported on Unix. Use --foreground instead.");
}

#[tokio::main]
async fn main() {
    // Reset SIGPIPE to default behavior so piped commands (e.g.
    // `syfrah completions bash | head`) terminate cleanly instead of panicking.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    if let Err(e) = run().await {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = Cli::parse();

    match args.command {
        Commands::Fabric { command } => match command {
            FabricCommand::Init {
                name,
                node_name,
                port,
                endpoint,
                peering_port,
                region,
                zone,
                foreground,
                peering,
            } => {
                validate_name("Mesh name", &name)?;
                let resolved_node = node_name.unwrap_or_else(default_node_name);
                validate_name("Node name", &resolved_node)?;
                let peering_port = validate_ports(port, peering_port)?;
                let region = validate_region(&region)?;
                let zone = validate_zone(&zone)?;
                let config = DaemonConfig {
                    mesh_name: name,
                    node_name: resolved_node,
                    wg_listen_port: port,
                    public_endpoint: endpoint,
                    peering_port,
                    region,
                    zone,
                };
                if foreground {
                    setup_logging(true);
                    daemon::run_init(config).await
                } else {
                    daemon::setup_init(&config)?;
                    background_daemon()?;
                    if peering {
                        cli::init::wait_and_start_peering(endpoint, peering_port).await
                    } else {
                        Ok(())
                    }
                }
            }
            FabricCommand::Join {
                target,
                node_name,
                port,
                endpoint,
                peering_port,
                pin,
                region,
                zone,
                foreground,
            } => {
                let resolved_node = node_name.unwrap_or_else(default_node_name);
                validate_name("Node name", &resolved_node)?;
                let peering_port = validate_ports(port, peering_port)?;
                let region = validate_region(&region)?;
                let zone = validate_zone(&zone)?;
                let config = DaemonConfig {
                    mesh_name: String::new(),
                    node_name: resolved_node,
                    wg_listen_port: port,
                    public_endpoint: endpoint,
                    peering_port,
                    region,
                    zone,
                };
                // Parse target: "1.2.3.4" -> "1.2.3.4:51821", or "1.2.3.4:9999" as-is
                let target_addr: SocketAddr = if target.contains(':') {
                    target
                        .parse()
                        .map_err(|e| anyhow::anyhow!("invalid target address '{target}': {e}"))?
                } else {
                    format!("{target}:51821")
                        .parse()
                        .map_err(|e| anyhow::anyhow!("invalid target address '{target}': {e}"))?
                };
                if foreground {
                    setup_logging(true);
                    daemon::run_join(target_addr, config, pin).await
                } else {
                    daemon::setup_join(target_addr, &config, pin).await?;
                    background_daemon()
                }
            }
            FabricCommand::Start { foreground } => {
                if !foreground {
                    if let Some(pid) = syfrah_fabric::store::daemon_running() {
                        eprintln!(
                            "Daemon is already running (pid {pid}). Use 'syfrah fabric stop' first."
                        );
                        std::process::exit(1);
                    }
                }
                if foreground {
                    // Log to file when running as daemon (foreground or background)
                    setup_logging(true);
                    cli::start::run().await
                } else {
                    // Validate state can be loaded before spawning background daemon
                    syfrah_fabric::store::load().map_err(|_| syfrah_fabric::no_mesh_error())?;
                    background_daemon()
                }
            }
            FabricCommand::Stop => {
                setup_logging(false);
                cli::stop::run().await
            }
            FabricCommand::Status {
                verbose,
                show_secret,
                json,
            } => {
                setup_logging(false);
                cli::status::run(cli::status::StatusOpts {
                    verbose,
                    show_secret,
                    json,
                })
                .await
            }
            FabricCommand::Events { json, limit, since } => {
                setup_logging(false);
                cli::events::run(json, limit, since).await
            }
            FabricCommand::Audit {
                since,
                event_type,
                json,
                limit,
            } => {
                setup_logging(false);
                cli::audit::run(json, limit, since, event_type).await
            }
            FabricCommand::Peers {
                action,
                json,
                topology,
                region,
                zone,
            } => {
                setup_logging(false);
                match action {
                    None => {
                        cli::peers::run(cli::peers::PeersOpts {
                            json,
                            topology,
                            region,
                            zone,
                        })
                        .await
                    }
                    Some(PeersAction::Remove { name_or_key, yes }) => {
                        cli::peers_remove::run(name_or_key, yes).await
                    }
                    Some(PeersAction::Update { name, endpoint }) => {
                        cli::peers_update::run(name, endpoint).await
                    }
                }
            }
            FabricCommand::Token => {
                setup_logging(false);
                cli::token::run().await
            }
            FabricCommand::Rotate { yes } => {
                setup_logging(false);
                cli::rotate::run(yes).await
            }
            FabricCommand::Leave { yes } => {
                setup_logging(false);
                cli::leave::run(yes).await
            }
            FabricCommand::Diagnose { json, zone } => {
                setup_logging(false);
                cli::diagnose::run(json, zone).await
            }
            FabricCommand::Reload => {
                setup_logging(false);
                cli::reload::run().await
            }
            FabricCommand::Topology {
                region,
                zone,
                json,
                verbose,
            } => {
                setup_logging(false);
                cli::topology::run(cli::topology::TopologyOpts {
                    region,
                    zone,
                    json,
                    verbose,
                })
                .await
            }
            FabricCommand::Metrics => {
                setup_logging(false);
                cli::metrics::run().await
            }
            FabricCommand::Service { action } => {
                setup_logging(false);
                match action {
                    ServiceAction::Install => cli::service::install().await,
                    ServiceAction::Uninstall => cli::service::uninstall().await,
                    ServiceAction::Status => cli::service::status().await,
                }
            }
            FabricCommand::Zone { action } => {
                setup_logging(false);
                match action {
                    ZoneAction::Drain { zone_path, yes } => cli::zone::drain(&zone_path, yes).await,
                    ZoneAction::Undrain { zone_path } => cli::zone::undrain(&zone_path).await,
                    ZoneAction::Status { json } => cli::zone::status(json).await,
                }
            }
            FabricCommand::Hosts { apply } => {
                setup_logging(false);
                cli::hosts::run(cli::hosts::HostsOpts { apply }).await
            }
            FabricCommand::Peering { action } => {
                setup_logging(false);
                match action {
                    PeeringAction::Start { port, pin } => {
                        let port = port.unwrap_or(51821);
                        cli::peering::start(port, pin).await
                    }
                    PeeringAction::Stop => cli::peering::stop().await,
                    PeeringAction::List => cli::peering::list().await,
                    PeeringAction::Watch { pin, continuous } => {
                        cli::peering::watch(pin, continuous).await
                    }
                    PeeringAction::Accept { request_id } => cli::peering::accept(&request_id).await,
                    PeeringAction::Reject { request_id, reason } => {
                        cli::peering::reject(&request_id, reason).await
                    }
                }
            }
        },
        Commands::Compute { command } => syfrah_compute::cli::run(command).await,
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let mut buf = Vec::new();
            generate(shell, &mut cmd, "syfrah", &mut buf);
            // Use write_all to handle broken pipe gracefully (e.g. `syfrah completions bash | head`)
            use std::io::Write;
            let _ = std::io::stdout().lock().write_all(&buf);
            Ok(())
        }
        Commands::State { command } => syfrah_state::cli::run(command).await,
        Commands::Update {
            check,
            no_restart,
            force,
        } => {
            if check {
                update::check()?;
                Ok(())
            } else {
                update::run(no_restart, force)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_ports ───────────────────────────────────────────

    #[test]
    fn valid_default_peering_port() {
        let pp = validate_ports(51820, None).unwrap();
        assert_eq!(pp, 51821);
    }

    #[test]
    fn valid_explicit_peering_port() {
        let pp = validate_ports(51820, Some(9000)).unwrap();
        assert_eq!(pp, 9000);
    }

    #[test]
    fn port_overflow_rejected() {
        let err = validate_ports(65535, None).unwrap_err();
        assert!(
            err.to_string().contains("65536 overflows"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn port_65535_with_explicit_peering_ok() {
        let pp = validate_ports(65535, Some(65534)).unwrap();
        assert_eq!(pp, 65534);
    }

    #[test]
    fn same_port_rejected() {
        let err = validate_ports(8080, Some(8080)).unwrap_err();
        assert!(err.to_string().contains("must differ"), "unexpected: {err}");
    }

    // ── validate_name ────────────────────────────────────────────

    #[test]
    fn short_name_ok() {
        assert!(validate_name("Mesh name", "my-mesh").is_ok());
    }

    #[test]
    fn exactly_64_chars_ok() {
        let name = "a".repeat(64);
        assert!(validate_name("Mesh name", &name).is_ok());
    }

    #[test]
    fn name_65_chars_rejected() {
        let name = "a".repeat(65);
        let err = validate_name("Mesh name", &name).unwrap_err();
        assert!(
            err.to_string().contains("64 characters or fewer"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn node_name_too_long_rejected() {
        let name = "x".repeat(100);
        let err = validate_name("Node name", &name).unwrap_err();
        assert!(err.to_string().contains("Node name"), "unexpected: {err}");
    }

    // ── validate_region ───────────────────────────────────────────

    #[test]
    fn region_none_returns_none_with_warning() {
        // None region is allowed (falls back to "default")
        let result = validate_region(&None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn region_valid_lowercase() {
        let result = validate_region(&Some("eu-west".to_string())).unwrap();
        assert_eq!(result, Some("eu-west".to_string()));
    }

    #[test]
    fn region_uppercase_rejected_with_suggestion() {
        let err = validate_region(&Some("EU-WEST".to_string())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid"), "unexpected: {msg}");
        assert!(msg.contains("eu-west"), "should suggest lowercase: {msg}");
    }

    #[test]
    fn region_too_long_rejected() {
        let long = "a".repeat(65);
        let err = validate_region(&Some(long)).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("too long") || msg.contains("max 64"),
            "unexpected: {msg}"
        );
    }

    // ── validate_zone ─────────────────────────────────────────────

    #[test]
    fn zone_none_returns_none() {
        let result = validate_zone(&None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn zone_valid_lowercase() {
        let result = validate_zone(&Some("par-1".to_string())).unwrap();
        assert_eq!(result, Some("par-1".to_string()));
    }

    #[test]
    fn zone_with_spaces_rejected_with_suggestion() {
        let err = validate_zone(&Some("par 1".to_string())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid"), "unexpected: {msg}");
        assert!(msg.contains("par-1"), "should suggest hyphenated: {msg}");
    }

    #[test]
    fn zone_uppercase_rejected() {
        let err = validate_zone(&Some("ZONE-A".to_string())).unwrap_err();
        assert!(err.to_string().contains("invalid"), "unexpected: {err}");
    }

    // ── suggest_fix ───────────────────────────────────────────────

    #[test]
    fn suggest_fix_uppercase() {
        let s = suggest_fix("EU-WEST");
        assert!(s.contains("eu-west"), "unexpected: {s}");
    }

    #[test]
    fn suggest_fix_spaces() {
        let s = suggest_fix("par 1");
        assert!(s.contains("par-1"), "unexpected: {s}");
    }

    #[test]
    fn suggest_fix_too_long() {
        let long = "a".repeat(65);
        let s = suggest_fix(&long);
        assert!(s.contains("max 64"), "unexpected: {s}");
    }
}
