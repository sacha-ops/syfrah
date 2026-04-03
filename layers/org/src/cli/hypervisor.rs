//! Hypervisor CLI subcommand handlers.
//!
//! All operations go through the daemon's control socket via the "hypervisor" layer.

use std::path::PathBuf;

use crate::hypervisor_handler::{send_hypervisor_request, HypervisorRequest, HypervisorResponse};

fn control_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

fn daemon_err(e: &dyn std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot reach the syfrah daemon — is it running?\n\
         Start it with: syfrah fabric init ...\n\n\
         Error: {e}"
    )
}

pub async fn run_list(
    region: Option<String>,
    zone: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let req = HypervisorRequest::List { region, zone };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::HypervisorList(list) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
                return Ok(());
            }
            if list.is_empty() {
                println!("No hypervisors found.");
                return Ok(());
            }
            println!(
                "{:<20} {:<12} {:<12} {:<15} {:<4} {:<16} {:<16}",
                "NAME", "REGION", "ZONE", "STATE", "VMs", "vCPU (used/tot)", "MEM (used/tot)"
            );
            for hv in &list {
                println!(
                    "{:<20} {:<12} {:<12} {:<15} {:<4} {}/{:<12} {}/{}",
                    hv.name,
                    hv.region,
                    hv.zone,
                    hv.state.to_string(),
                    0, // VM count not tracked here yet
                    hv.capacity.used_vcpus,
                    hv.capacity.allocatable_vcpus,
                    format_mb(hv.capacity.used_memory_mb),
                    format_mb(hv.capacity.allocatable_memory_mb),
                );
            }
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_get(name: String, json: bool) -> anyhow::Result<()> {
    let req = HypervisorRequest::Get { name };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Hypervisor(ref hv) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&hv)?);
                return Ok(());
            }
            println!("Hypervisor: {} ({})", hv.name, hv.id);
            println!("Region:     {}", hv.region);
            println!("Zone:       {}", hv.zone);
            println!("State:      {}", hv.state);
            println!("Fabric:     {}", hv.fabric_ipv6);
            println!("Public IP:  {}", hv.public_ip);
            println!();
            println!("Hardware:");
            println!(
                "  CPU:      {} ({} cores, {} threads)",
                hv.hardware.cpu_model,
                hv.hardware.cpu_cores_physical,
                hv.hardware.cpu_threads_logical
            );
            println!("  Memory:   {} GB", hv.hardware.memory_gb);
            println!(
                "  Disk:     {} GB {}",
                hv.hardware.local_disk_gb, hv.hardware.local_disk_type
            );
            if let Some(ref gpu) = hv.hardware.gpu {
                println!("  GPU:      {} x{}", gpu.model, gpu.count);
            }
            println!("  Arch:     {}", hv.hardware.architecture);
            println!();
            println!("Capacity:");
            println!(
                "  vCPUs:    {} used / {} allocatable",
                hv.capacity.used_vcpus, hv.capacity.allocatable_vcpus
            );
            println!(
                "  Memory:   {} used / {} allocatable",
                format_mb(hv.capacity.used_memory_mb),
                format_mb(hv.capacity.allocatable_memory_mb)
            );
            println!(
                "  Disk:     {} GB used / {} GB allocatable",
                hv.capacity.local_used_gb, hv.capacity.local_allocatable_gb
            );
            println!(
                "  Overcommit: CPU {:.1}x, Memory {:.1}x",
                hv.capacity.overcommit_cpu, hv.capacity.overcommit_memory
            );
            if !hv.labels.is_empty() {
                let labels: Vec<String> =
                    hv.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("\nLabels:     {}", labels.join(", "));
            }
            if !hv.taints.is_empty() {
                let taints: Vec<String> = hv.taints.iter().map(|t| t.to_string()).collect();
                println!("Taints:     {}", taints.join(", "));
            }
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_register(region: String, zone: String) -> anyhow::Result<()> {
    let req = HypervisorRequest::Register { region, zone };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Hypervisor(ref hv) => {
            println!("Hypervisor '{}' registered ({}).", hv.name, hv.id);
            Ok(())
        }
        HypervisorResponse::Ok => {
            println!("Hypervisor registered.");
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_enable(name: String) -> anyhow::Result<()> {
    let req = HypervisorRequest::Enable { name: name.clone() };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Ok => {
            println!("Hypervisor '{name}' enabled (Available).");
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_status() -> anyhow::Result<()> {
    let req = HypervisorRequest::Status;
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Hypervisor(ref hv) => {
            println!("Hypervisor: {} ({})", hv.name, hv.id);
            println!("State:      {}", hv.state);
            println!("Region:     {}", hv.region);
            println!("Zone:       {}", hv.zone);
            println!(
                "vCPUs:      {} used / {} allocatable",
                hv.capacity.used_vcpus, hv.capacity.allocatable_vcpus
            );
            println!(
                "Memory:     {} used / {} allocatable",
                format_mb(hv.capacity.used_memory_mb),
                format_mb(hv.capacity.allocatable_memory_mb)
            );
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_capacity() -> anyhow::Result<()> {
    let req = HypervisorRequest::Capacity;
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Hypervisor(ref hv) => {
            println!("Capacity for {} ({}):", hv.name, hv.state);
            println!();
            println!(
                "  Physical:   {} vCPUs, {} MB memory",
                hv.capacity.physical_vcpus, hv.capacity.physical_memory_mb
            );
            println!(
                "  Reserved:   {} vCPUs, {} MB memory",
                hv.capacity.reserved_vcpus, hv.capacity.reserved_memory_mb
            );
            println!(
                "  Overcommit: CPU {:.1}x, Memory {:.1}x",
                hv.capacity.overcommit_cpu, hv.capacity.overcommit_memory
            );
            println!(
                "  Allocatable: {} vCPUs, {} MB memory",
                hv.capacity.allocatable_vcpus, hv.capacity.allocatable_memory_mb
            );
            println!(
                "  Used:       {} vCPUs, {} MB memory",
                hv.capacity.used_vcpus, hv.capacity.used_memory_mb
            );
            println!(
                "  Available:  {} vCPUs, {} MB memory",
                hv.capacity.available_vcpus, hv.capacity.available_memory_mb
            );
            println!();
            println!(
                "  Disk:       {} GB total, {} GB used, {} GB allocatable",
                hv.capacity.local_total_gb,
                hv.capacity.local_used_gb,
                hv.capacity.local_allocatable_gb
            );
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_label_set(name: String, labels: Vec<(String, String)>) -> anyhow::Result<()> {
    let req = HypervisorRequest::LabelSet {
        name: name.clone(),
        labels,
    };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Ok => {
            println!("Labels updated on '{name}'.");
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_label_remove(name: String, keys: Vec<String>) -> anyhow::Result<()> {
    let req = HypervisorRequest::LabelRemove {
        name: name.clone(),
        keys,
    };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Ok => {
            println!("Labels removed from '{name}'.");
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_taint_add(name: String, taints: Vec<String>) -> anyhow::Result<()> {
    let req = HypervisorRequest::TaintAdd {
        name: name.clone(),
        taints,
    };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Ok => {
            println!("Taints added to '{name}'.");
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_taint_remove(name: String, keys: Vec<String>) -> anyhow::Result<()> {
    let req = HypervisorRequest::TaintRemove {
        name: name.clone(),
        keys,
    };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Ok => {
            println!("Taints removed from '{name}'.");
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_drain(name: String, force: bool) -> anyhow::Result<()> {
    let req = HypervisorRequest::Drain {
        name: name.clone(),
        force,
    };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Ok => {
            println!("Hypervisor '{name}' is now draining.");
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_activate(name: String) -> anyhow::Result<()> {
    let req = HypervisorRequest::Activate { name: name.clone() };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Ok => {
            println!("Hypervisor '{name}' activated (Available).");
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_maintenance(name: String, drain: bool) -> anyhow::Result<()> {
    let req = HypervisorRequest::Maintenance {
        name: name.clone(),
        drain,
    };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Ok => {
            println!("Hypervisor '{name}' is now in maintenance.");
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_decommission(name: String) -> anyhow::Result<()> {
    let req = HypervisorRequest::Decommission { name: name.clone() };
    let resp = send_hypervisor_request(&control_socket_path(), &req)
        .await
        .map_err(|e| daemon_err(&e))?;

    match resp {
        HypervisorResponse::Ok => {
            println!("Hypervisor '{name}' decommissioned.");
            Ok(())
        }
        HypervisorResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

fn format_mb(mb: u64) -> String {
    if mb >= 1024 {
        format!("{}G", mb / 1024)
    } else {
        format!("{}M", mb)
    }
}
