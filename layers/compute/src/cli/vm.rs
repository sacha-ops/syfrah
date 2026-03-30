//! VM lifecycle CLI commands.
//!
//! Subcommands: create, list, get, start, stop, delete, reboot, resize.
//! Each handler communicates with the daemon via the control socket.

use std::path::PathBuf;

use clap::Subcommand;

use crate::control::{send_compute_request, ComputeRequest, ComputeResponse};

/// VM management subcommands.
#[derive(Debug, Subcommand)]
pub enum VmCommand {
    /// Create a new virtual machine
    #[command(
        after_help = "Examples:\n  syfrah compute vm create --name web-1 --image alpine-3.20\n  syfrah compute vm create --name web-1 --image alpine-3.20 --vcpus 4 --memory 4096 --ssh-key ~/.ssh/id_ed25519.pub"
    )]
    Create {
        /// Human-readable name for the VM
        #[arg(long)]
        name: String,
        /// Number of virtual CPUs
        #[arg(long = "vcpus", alias = "vcpu", default_value = "2")]
        vcpus: u32,
        /// Memory in megabytes
        #[arg(long, default_value = "2048")]
        memory: u32,
        /// Root filesystem image name (e.g. "ubuntu-24.04")
        #[arg(long)]
        image: String,
        /// Optional GPU PCI BDF address for VFIO passthrough
        #[arg(long)]
        gpu: Option<String>,
        /// TAP device name for networking
        #[arg(long)]
        tap: Option<String>,
        /// Path to SSH public key file to inject via cloud-init
        #[arg(long)]
        ssh_key: Option<PathBuf>,
        /// Disk size in MB (0 = use image default)
        #[arg(long)]
        disk_size: Option<u32>,
    },
    /// List all virtual machines
    #[command(after_help = "Examples:\n  syfrah compute vm list\n  syfrah compute vm list --json")]
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Get details of a virtual machine
    Get {
        /// VM name
        #[arg(value_name = "NAME")]
        id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Start (boot) a stopped virtual machine
    Start {
        /// VM name
        #[arg(value_name = "NAME")]
        id: String,
    },
    /// Stop a running virtual machine
    Stop {
        /// VM name
        #[arg(value_name = "NAME")]
        id: String,
        /// Force shutdown (kill) instead of graceful ACPI
        #[arg(long, short)]
        force: bool,
    },
    /// Delete a virtual machine and clean up all artifacts
    Delete {
        /// VM name
        #[arg(value_name = "NAME")]
        id: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Reboot a running virtual machine
    Reboot {
        /// VM name
        #[arg(value_name = "NAME")]
        id: String,
    },
    /// Hot-resize CPU and memory of a running virtual machine
    Resize {
        /// VM name
        #[arg(value_name = "NAME")]
        id: String,
        /// New number of virtual CPUs
        #[arg(long)]
        vcpus: Option<u32>,
        /// New memory in megabytes
        #[arg(long)]
        memory: Option<u32>,
    },
}

/// Execute a VM subcommand.
pub async fn run(cmd: VmCommand) -> anyhow::Result<()> {
    match cmd {
        VmCommand::Create {
            name,
            vcpus,
            memory,
            image,
            gpu,
            tap,
            ssh_key,
            disk_size,
        } => run_create(name, vcpus, memory, image, gpu, tap, ssh_key, disk_size).await,
        VmCommand::List { json } => run_list(json).await,
        VmCommand::Get { id, json } => run_get(id, json).await,
        VmCommand::Start { id } => run_start(id).await,
        VmCommand::Stop { id, force } => run_stop(id, force).await,
        VmCommand::Delete { id, yes } => run_delete(id, yes).await,
        VmCommand::Reboot { id } => run_reboot(id).await,
        VmCommand::Resize { id, vcpus, memory } => run_resize(id, vcpus, memory).await,
    }
}

// ---------------------------------------------------------------------------
// Control socket path
// ---------------------------------------------------------------------------

fn control_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Read and validate an SSH key file, returning the trimmed content.
pub(crate) fn read_ssh_key(path: &std::path::Path) -> anyhow::Result<String> {
    if !path.exists() {
        anyhow::bail!("SSH key file not found: {}", path.display());
    }
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read SSH key file: {e}"))?;
    let trimmed = content.trim_end().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("SSH key file is empty: {}", path.display());
    }
    Ok(trimmed)
}

/// Convert disk_size: 0 means use image default (None).
pub(crate) fn normalize_disk_size(disk_size: Option<u32>) -> Option<u32> {
    match disk_size {
        Some(0) => None,
        other => other,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_create(
    name: String,
    vcpus: u32,
    memory: u32,
    image: String,
    gpu: Option<String>,
    tap: Option<String>,
    ssh_key_path: Option<PathBuf>,
    disk_size: Option<u32>,
) -> anyhow::Result<()> {
    let ssh_key = match ssh_key_path {
        Some(ref path) => Some(read_ssh_key(path)?),
        None => None,
    };
    let disk_size_mb = normalize_disk_size(disk_size);

    let req = ComputeRequest::CreateVm {
        name,
        vcpus,
        memory_mb: memory,
        image,
        gpu_bdf: gpu,
        tap,
        ssh_key: ssh_key.clone(),
        disk_size_mb,
    };
    let resp = send_compute_request(&control_socket_path(), &req)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
            )
        })?;

    match resp {
        ComputeResponse::Vm(v) => {
            let vm_name = v.get("id").and_then(|n| n.as_str()).unwrap_or("?");
            let vm_image = v.get("image").and_then(|i| i.as_str()).unwrap_or("?");
            let vm_vcpus = v.get("vcpus").and_then(|c| c.as_u64()).unwrap_or(0);
            let vm_memory = v.get("memory_mb").and_then(|m| m.as_u64()).unwrap_or(0);
            println!("VM created: {vm_name} ({vm_image}, {vm_vcpus} vCPU, {vm_memory} MB)");
            if ssh_key.is_some() {
                let mesh_ip = v
                    .get("mesh_ipv6")
                    .and_then(|ip| ip.as_str())
                    .unwrap_or("<mesh-ipv6>");
                let user = v
                    .get("default_username")
                    .and_then(|u| u.as_str())
                    .unwrap_or("ubuntu");
                println!("SSH: ssh {user}@{mesh_ip}");
            } else {
                println!("Note: no SSH key provided — configure access manually");
            }
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            anyhow::bail!("{msg}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_list(json: bool) -> anyhow::Result<()> {
    let req = ComputeRequest::ListVms;
    let resp = match send_compute_request(&control_socket_path(), &req).await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>");
            if json {
                super::json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
    };

    match resp {
        ComputeResponse::VmList(vms) => {
            if json {
                let json_str = serde_json::to_string_pretty(&vms)?;
                println!("{json_str}");
            } else {
                let header = format!(
                    "{:<20} {:<20} {:<12} {:<12} {:<6} {:<10} {:<10}",
                    "NAME", "IMAGE", "PHASE", "RUNTIME", "vCPUs", "MEMORY", "UPTIME"
                );
                if console::Term::stdout().is_term() {
                    println!("{}", console::Style::new().bold().apply_to(&header));
                } else {
                    println!("{header}");
                }
                println!("{}", "-".repeat(90));
                if vms.is_empty() {
                    println!("(no VMs)");
                } else {
                    for vm in &vms {
                        let name = vm.get("id").and_then(|n| n.as_str()).unwrap_or("?");
                        let image = vm.get("image").and_then(|i| i.as_str()).unwrap_or("");
                        let phase = vm.get("phase").and_then(|p| p.as_str()).unwrap_or("?");
                        let runtime = vm.get("runtime").and_then(|r| r.as_str()).unwrap_or("-");
                        let vcpus = vm.get("vcpus").and_then(|v| v.as_u64()).unwrap_or(0);
                        let memory = vm.get("memory_mb").and_then(|m| m.as_u64()).unwrap_or(0);
                        let uptime = vm
                            .get("uptime_secs")
                            .and_then(|u| u.as_u64())
                            .map(format_uptime)
                            .unwrap_or_else(|| "-".to_string());
                        println!("{name:<20} {image:<20} {phase:<12} {runtime:<12} {vcpus:<6} {memory:<10} {uptime:<10}");
                    }
                }
            }
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            if json {
                super::json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
        _ => {
            if json {
                super::json_error_exit("unexpected response from daemon");
            }
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_get(id: String, json: bool) -> anyhow::Result<()> {
    let req = ComputeRequest::GetVm { id };
    let resp = match send_compute_request(&control_socket_path(), &req).await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>");
            if json {
                super::json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
    };

    match resp {
        ComputeResponse::Vm(v) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                let name = v.get("id").and_then(|n| n.as_str()).unwrap_or("?");
                let image = v.get("image").and_then(|i| i.as_str()).unwrap_or("");
                let phase = v.get("phase").and_then(|p| p.as_str()).unwrap_or("?");
                let runtime = v.get("runtime").and_then(|r| r.as_str()).unwrap_or("-");
                let vcpus = v.get("vcpus").and_then(|v| v.as_u64()).unwrap_or(0);
                let memory = v.get("memory_mb").and_then(|m| m.as_u64()).unwrap_or(0);
                let uptime = v
                    .get("uptime_secs")
                    .and_then(|u| u.as_u64())
                    .map(format_uptime)
                    .unwrap_or_else(|| "-".to_string());
                println!("VM Details");
                println!("  Name:      {name}");
                println!("  Image:     {image}");
                println!("  Phase:     {phase}");
                println!("  Runtime:   {runtime}");
                println!("  vCPUs:     {vcpus}");
                println!("  Memory:    {memory} MB");
                println!("  Uptime:    {uptime}");
            }
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            if json {
                super::json_error_exit(&msg);
            }
            anyhow::bail!("{msg}");
        }
        _ => {
            if json {
                super::json_error_exit("unexpected response from daemon");
            }
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_start(id: String) -> anyhow::Result<()> {
    let req = ComputeRequest::StartVm { id: id.clone() };
    let resp = send_compute_request(&control_socket_path(), &req)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
            )
        })?;

    match resp {
        ComputeResponse::Vm(v) => {
            let phase = v.get("phase").and_then(|p| p.as_str()).unwrap_or("?");
            println!("VM {id}: {phase}");
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            anyhow::bail!("{msg}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_stop(id: String, force: bool) -> anyhow::Result<()> {
    let req = ComputeRequest::StopVm {
        id: id.clone(),
        force,
    };
    let resp = send_compute_request(&control_socket_path(), &req)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
            )
        })?;

    match resp {
        ComputeResponse::Vm(v) => {
            let phase = v.get("phase").and_then(|p| p.as_str()).unwrap_or("?");
            println!("VM {id}: {phase}");
            Ok(())
        }
        ComputeResponse::Ok => {
            println!("VM {id}: Stopped");
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            anyhow::bail!("{msg}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_delete(id: String, yes: bool) -> anyhow::Result<()> {
    // Check that the VM exists before prompting for confirmation.
    let get_req = ComputeRequest::GetVm { id: id.clone() };
    let get_resp = send_compute_request(&control_socket_path(), &get_req)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
            )
        })?;

    match get_resp {
        ComputeResponse::Vm(_) => {} // VM exists, proceed
        ComputeResponse::Error(msg) => {
            anyhow::bail!("{msg}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }

    if !yes {
        eprint!("Delete VM {id}? This cannot be undone. [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();
        if answer != "y" && answer != "Y" {
            eprintln!("Aborted.");
            std::process::exit(1);
        }
    }

    let req = ComputeRequest::DeleteVm {
        id: id.clone(),
        retain_disk: false,
    };
    let resp = send_compute_request(&control_socket_path(), &req)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
            )
        })?;

    match resp {
        ComputeResponse::Ok => {
            println!("VM {id}: deleted");
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            anyhow::bail!("{msg}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_reboot(id: String) -> anyhow::Result<()> {
    let req = ComputeRequest::RebootVm { id: id.clone() };
    let resp = send_compute_request(&control_socket_path(), &req)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
            )
        })?;

    match resp {
        ComputeResponse::Vm(v) => {
            let phase = v.get("phase").and_then(|p| p.as_str()).unwrap_or("?");
            println!("VM {id}: {phase}");
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            anyhow::bail!("{msg}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

async fn run_resize(id: String, vcpus: Option<u32>, memory: Option<u32>) -> anyhow::Result<()> {
    if vcpus.is_none() && memory.is_none() {
        anyhow::bail!("at least one of --vcpus or --memory must be provided");
    }

    let req = ComputeRequest::ResizeVm {
        id: id.clone(),
        vcpus,
        memory_mb: memory,
    };
    let resp = send_compute_request(&control_socket_path(), &req)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to connect to daemon: {e}\n\nIs the daemon running? Initialize with: syfrah fabric init --name <mesh-name>"
            )
        })?;

    match resp {
        ComputeResponse::Vm(v) => {
            let phase = v.get("phase").and_then(|p| p.as_str()).unwrap_or("?");
            println!("VM {id}: {phase}");
            Ok(())
        }
        ComputeResponse::Error(msg) => {
            anyhow::bail!("{msg}");
        }
        _ => {
            anyhow::bail!("unexpected response from daemon");
        }
    }
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    /// Helper to parse VM commands from an arg list.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: VmCommand,
    }

    fn parse(args: &[&str]) -> VmCommand {
        let full_args = std::iter::once("test").chain(args.iter().copied());
        TestCli::parse_from(full_args).cmd
    }

    #[test]
    fn parse_create_minimal() {
        let cmd = parse(&["create", "--name", "test-vm", "--image", "ubuntu-24.04"]);
        match cmd {
            VmCommand::Create {
                name,
                vcpus,
                memory,
                image,
                gpu,
                tap,
                ssh_key,
                disk_size,
            } => {
                assert_eq!(name, "test-vm");
                assert_eq!(vcpus, 2); // default
                assert_eq!(memory, 2048); // default
                assert_eq!(image, "ubuntu-24.04");
                assert!(gpu.is_none());
                assert!(tap.is_none());
                assert!(ssh_key.is_none());
                assert!(disk_size.is_none());
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn parse_create_full() {
        let cmd = parse(&[
            "create",
            "--name",
            "gpu-vm",
            "--vcpu",
            "8",
            "--memory",
            "16384",
            "--image",
            "ubuntu-24.04",
            "--gpu",
            "0000:01:00.0",
            "--tap",
            "tap0",
        ]);
        match cmd {
            VmCommand::Create {
                name,
                vcpus,
                memory,
                image,
                gpu,
                tap,
                ssh_key,
                disk_size,
            } => {
                assert_eq!(name, "gpu-vm");
                assert_eq!(vcpus, 8);
                assert_eq!(memory, 16384);
                assert_eq!(image, "ubuntu-24.04");
                assert_eq!(gpu.as_deref(), Some("0000:01:00.0"));
                assert_eq!(tap.as_deref(), Some("tap0"));
                assert!(ssh_key.is_none());
                assert!(disk_size.is_none());
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn parse_create_vcpu_alias() {
        // --vcpu (singular) is now the alias for --vcpus
        let cmd = parse(&[
            "create", "--name", "alias-vm", "--vcpu", "4", "--memory", "1024", "--image", "alpine",
        ]);
        match cmd {
            VmCommand::Create { vcpus, .. } => assert_eq!(vcpus, 4),
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn parse_list() {
        let cmd = parse(&["list"]);
        assert!(matches!(cmd, VmCommand::List { json: false }));
    }

    #[test]
    fn parse_list_json() {
        let cmd = parse(&["list", "--json"]);
        assert!(matches!(cmd, VmCommand::List { json: true }));
    }

    #[test]
    fn parse_get() {
        let cmd = parse(&["get", "vm-123"]);
        match cmd {
            VmCommand::Get { id, json } => {
                assert_eq!(id, "vm-123");
                assert!(!json);
            }
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn parse_get_json() {
        let cmd = parse(&["get", "vm-456", "--json"]);
        match cmd {
            VmCommand::Get { id, json } => {
                assert_eq!(id, "vm-456");
                assert!(json);
            }
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn parse_start() {
        let cmd = parse(&["start", "vm-789"]);
        assert!(matches!(cmd, VmCommand::Start { id } if id == "vm-789"));
    }

    #[test]
    fn parse_stop() {
        let cmd = parse(&["stop", "vm-abc"]);
        match cmd {
            VmCommand::Stop { id, force } => {
                assert_eq!(id, "vm-abc");
                assert!(!force);
            }
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[test]
    fn parse_stop_force() {
        let cmd = parse(&["stop", "--force", "vm-abc"]);
        match cmd {
            VmCommand::Stop { id, force } => {
                assert_eq!(id, "vm-abc");
                assert!(force);
            }
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    #[test]
    fn parse_delete() {
        let cmd = parse(&["delete", "vm-del"]);
        match cmd {
            VmCommand::Delete { id, yes } => {
                assert_eq!(id, "vm-del");
                assert!(!yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn parse_delete_yes() {
        let cmd = parse(&["delete", "--yes", "vm-del"]);
        match cmd {
            VmCommand::Delete { id, yes } => {
                assert_eq!(id, "vm-del");
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn parse_reboot() {
        let cmd = parse(&["reboot", "vm-reboot"]);
        assert!(matches!(cmd, VmCommand::Reboot { id } if id == "vm-reboot"));
    }

    #[test]
    fn parse_resize_vcpus_only() {
        let cmd = parse(&["resize", "vm-resize", "--vcpus", "4"]);
        match cmd {
            VmCommand::Resize { id, vcpus, memory } => {
                assert_eq!(id, "vm-resize");
                assert_eq!(vcpus, Some(4));
                assert!(memory.is_none());
            }
            other => panic!("expected Resize, got {other:?}"),
        }
    }

    #[test]
    fn parse_resize_memory_only() {
        let cmd = parse(&["resize", "vm-resize", "--memory", "8192"]);
        match cmd {
            VmCommand::Resize { id, vcpus, memory } => {
                assert_eq!(id, "vm-resize");
                assert!(vcpus.is_none());
                assert_eq!(memory, Some(8192));
            }
            other => panic!("expected Resize, got {other:?}"),
        }
    }

    #[test]
    fn parse_resize_both() {
        let cmd = parse(&["resize", "vm-resize", "--vcpus", "8", "--memory", "16384"]);
        match cmd {
            VmCommand::Resize { id, vcpus, memory } => {
                assert_eq!(id, "vm-resize");
                assert_eq!(vcpus, Some(8));
                assert_eq!(memory, Some(16384));
            }
            other => panic!("expected Resize, got {other:?}"),
        }
    }

    // -- SSH key tests --------------------------------------------------------

    #[test]
    fn ssh_key_valid_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "ssh-ed25519 AAAA... user@host\n").unwrap();
        let result = read_ssh_key(tmp.path());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "ssh-ed25519 AAAA... user@host");
    }

    #[test]
    fn ssh_key_missing_file() {
        let result = read_ssh_key(std::path::Path::new("/nonexistent/key.pub"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("SSH key file not found"));
    }

    #[test]
    fn ssh_key_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();
        let result = read_ssh_key(tmp.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("SSH key file is empty"));
    }

    #[test]
    fn ssh_key_trailing_newline_trimmed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "ssh-rsa AAAA...\n\n\n").unwrap();
        let result = read_ssh_key(tmp.path());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "ssh-rsa AAAA...");
    }

    // -- Disk size tests ------------------------------------------------------

    #[test]
    fn disk_size_4096() {
        assert_eq!(normalize_disk_size(Some(4096)), Some(4096));
    }

    #[test]
    fn disk_size_zero_becomes_none() {
        assert_eq!(normalize_disk_size(Some(0)), None);
    }

    #[test]
    fn disk_size_none_stays_none() {
        assert_eq!(normalize_disk_size(None), None);
    }

    // -- Parse with ssh-key and disk-size flags -------------------------------

    #[test]
    fn parse_create_with_ssh_key_and_disk_size() {
        let cmd = parse(&[
            "create",
            "--name",
            "web-1",
            "--image",
            "ubuntu-24.10",
            "--ssh-key",
            "/tmp/id_ed25519.pub",
            "--disk-size",
            "8192",
        ]);
        match cmd {
            VmCommand::Create {
                name,
                ssh_key,
                disk_size,
                ..
            } => {
                assert_eq!(name, "web-1");
                assert_eq!(ssh_key, Some(PathBuf::from("/tmp/id_ed25519.pub")));
                assert_eq!(disk_size, Some(8192));
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resize_requires_at_least_one_option() {
        let result = run_resize("vm-test".to_string(), None, None).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("--vcpus or --memory"), "unexpected: {msg}");
    }
}
