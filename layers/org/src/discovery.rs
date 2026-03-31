//! Hypervisor auto-discovery — probes local hardware for KVM capability.
//!
//! On daemon startup, checks `/dev/kvm` and probes hardware specs from
//! `/proc/cpuinfo`, `/proc/meminfo`, `lsblk`, and `lspci`.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::hypervisor::HypervisorStore;
use crate::types::{
    AllocatableCapacity, CpuArchitecture, DiskType, GpuSpec, HardwareSpec, Hypervisor,
    HypervisorId, HypervisorState,
};

/// Check if KVM is available on this node.
pub fn kvm_available() -> bool {
    Path::new("/dev/kvm").exists()
}

/// Probe CPU information from /proc/cpuinfo.
fn probe_cpu() -> (String, u32, u32) {
    let content = match std::fs::read_to_string("/proc/cpuinfo") {
        Ok(c) => c,
        Err(_) => return ("Unknown CPU".to_string(), 1, 1),
    };

    let mut model = String::new();
    let mut physical_ids = std::collections::HashSet::new();
    let mut core_ids_per_socket: HashMap<String, std::collections::HashSet<String>> =
        HashMap::new();
    let mut logical_count: u32 = 0;
    let mut current_physical_id = String::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("model name") {
            if model.is_empty() {
                if let Some(val) = line.split(':').nth(1) {
                    model = val.trim().to_string();
                }
            }
        } else if line.starts_with("physical id") {
            if let Some(val) = line.split(':').nth(1) {
                current_physical_id = val.trim().to_string();
                physical_ids.insert(current_physical_id.clone());
            }
        } else if line.starts_with("core id") {
            if let Some(val) = line.split(':').nth(1) {
                core_ids_per_socket
                    .entry(current_physical_id.clone())
                    .or_default()
                    .insert(val.trim().to_string());
            }
        } else if line.starts_with("processor") {
            logical_count += 1;
        }
    }

    if model.is_empty() {
        model = "Unknown CPU".to_string();
    }

    let physical_cores: u32 = if core_ids_per_socket.is_empty() {
        // Fallback: assume logical = physical (no SMT info)
        logical_count
    } else {
        core_ids_per_socket.values().map(|s| s.len() as u32).sum()
    };

    if logical_count == 0 {
        logical_count = 1;
    }

    (model, physical_cores.max(1), logical_count)
}

/// Probe total memory from /proc/meminfo in GB.
fn probe_memory() -> (u32, u64) {
    let content = match std::fs::read_to_string("/proc/meminfo") {
        Ok(c) => c,
        Err(_) => return (1, 1024),
    };

    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(kb) = parts[1].parse::<u64>() {
                    let mb = kb / 1024;
                    let gb = (mb / 1024) as u32;
                    return (gb.max(1), mb);
                }
            }
        }
    }
    (1, 1024)
}

/// Probe disk type and total size using lsblk.
fn probe_disk() -> (DiskType, u32) {
    let output = match std::process::Command::new("lsblk")
        .args(["-b", "-d", "-n", "-o", "NAME,SIZE,ROTA,TRAN"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return (DiskType::HDD, 0),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut total_bytes: u64 = 0;
    let mut disk_type = DiskType::HDD;

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let name = parts[0];
        // Skip loop, ram, etc.
        if name.starts_with("loop") || name.starts_with("ram") || name.starts_with("sr") {
            continue;
        }

        if let Ok(size) = parts[1].parse::<u64>() {
            total_bytes += size;
        }

        let rota = parts.get(2).unwrap_or(&"1");
        let tran = parts.get(3).unwrap_or(&"");

        if tran.contains("nvme") {
            disk_type = DiskType::NVMe;
        } else if *rota == "0" && disk_type != DiskType::NVMe {
            disk_type = DiskType::SSD;
        }
    }

    let gb = (total_bytes / (1024 * 1024 * 1024)) as u32;
    (disk_type, gb)
}

/// Probe GPU info using lspci (optional).
fn probe_gpu() -> Option<GpuSpec> {
    let output = match std::process::Command::new("lspci").output() {
        Ok(o) => o,
        Err(_) => return None,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut gpu_count: u32 = 0;
    let mut gpu_model = String::new();

    for line in stdout.lines() {
        // Look for NVIDIA or AMD GPU entries (VGA / 3D controller)
        let lower = line.to_lowercase();
        if (lower.contains("vga") || lower.contains("3d controller"))
            && (lower.contains("nvidia") || lower.contains("amd"))
        {
            gpu_count += 1;
            if gpu_model.is_empty() {
                // Extract model from after the colon
                if let Some(desc) = line.split(':').next_back() {
                    gpu_model = desc.trim().to_string();
                }
            }
        }
    }

    if gpu_count > 0 {
        Some(GpuSpec {
            model: gpu_model,
            vram_mb: 0, // Cannot reliably detect without nvidia-smi
            count: gpu_count,
        })
    } else {
        None
    }
}

/// Detect CPU architecture.
fn probe_architecture() -> CpuArchitecture {
    match std::env::consts::ARCH {
        "aarch64" => CpuArchitecture::Aarch64,
        _ => CpuArchitecture::X86_64,
    }
}

/// Probe all hardware specs from the local system.
pub fn probe_hardware() -> HardwareSpec {
    let (cpu_model, cores, threads) = probe_cpu();
    let (memory_gb, _memory_mb) = probe_memory();
    let (disk_type, disk_gb) = probe_disk();
    let gpu = probe_gpu();
    let architecture = probe_architecture();

    HardwareSpec {
        cpu_model,
        cpu_cores_physical: cores,
        cpu_threads_logical: threads,
        memory_gb,
        local_disk_type: disk_type,
        local_disk_gb: disk_gb,
        gpu,
        network_bandwidth_gbps: 1, // Safe default, can be overridden
        architecture,
    }
}

/// Compute allocatable capacity from hardware specs.
pub fn compute_capacity(hw: &HardwareSpec) -> AllocatableCapacity {
    let reserved_vcpus: u32 = 1;
    let reserved_memory_mb: u64 = 1024;
    let overcommit_cpu: f32 = 2.0;
    let overcommit_memory: f32 = 1.0;

    let physical_memory_mb = (hw.memory_gb as u64) * 1024;
    let allocatable_vcpus =
        ((hw.cpu_threads_logical as f32 * overcommit_cpu) as u32).saturating_sub(reserved_vcpus);
    let allocatable_memory_mb = ((physical_memory_mb as f64 * overcommit_memory as f64) as u64)
        .saturating_sub(reserved_memory_mb);

    AllocatableCapacity {
        physical_vcpus: hw.cpu_threads_logical,
        physical_memory_mb,
        allocatable_vcpus,
        allocatable_memory_mb,
        used_vcpus: 0,
        used_memory_mb: 0,
        available_vcpus: allocatable_vcpus,
        available_memory_mb: allocatable_memory_mb,
        reserved_vcpus,
        reserved_memory_mb,
        overcommit_cpu,
        overcommit_memory,
        local_total_gb: hw.local_disk_gb,
        local_used_gb: 0,
        local_allocatable_gb: hw.local_disk_gb.saturating_sub(20), // Reserve 20GB for OS
    }
}

/// Generate a ULID-style hypervisor ID.
fn generate_hypervisor_id() -> HypervisorId {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    // Simple unique ID: hv-{timestamp_hex}-{random_hex}
    let rand: u32 = (ts as u32) ^ std::process::id();
    HypervisorId(format!("hv-{ts:010x}{rand:08x}"))
}

/// Run hypervisor auto-discovery on daemon startup.
///
/// Returns `Some(hypervisor_name)` if a hypervisor was discovered/recovered,
/// `None` if this node is not KVM-capable.
pub fn discover_hypervisor(
    store: &HypervisorStore,
    node_name: &str,
    fabric_node_id: &str,
    region: &str,
    zone: &str,
    public_ip: &str,
    fabric_ipv6: &str,
) -> Option<String> {
    if !kvm_available() {
        info!("hypervisor: /dev/kvm not found, node is mesh-only");
        return None;
    }

    // Check if we already have a hypervisor for this fabric node
    if let Ok(Some(existing)) = store.get_by_fabric_node_id(fabric_node_id) {
        info!(
            name = %existing.name,
            id = %existing.id,
            "hypervisor: recovered existing record"
        );
        // Re-probe hardware and update
        let hw = probe_hardware();
        let capacity = compute_capacity(&hw);
        let mut updated = existing.clone();
        updated.hardware = hw;
        updated.capacity = capacity;
        updated.public_ip = public_ip.to_string();
        updated.fabric_ipv6 = fabric_ipv6.to_string();
        if let Err(e) = store.update(&updated) {
            warn!(error = %e, "hypervisor: failed to update hardware on restart");
        }
        return Some(existing.name);
    }

    // Also check by name (node_name is used as hypervisor name)
    if let Ok(Some(existing)) = store.get(node_name) {
        info!(
            name = %existing.name,
            id = %existing.id,
            "hypervisor: found existing record by name"
        );
        return Some(existing.name);
    }

    // New discovery: probe hardware and register
    info!("hypervisor: KVM detected, starting discovery");
    let hw = probe_hardware();
    info!(
        cpu = %hw.cpu_model,
        cores = hw.cpu_cores_physical,
        threads = hw.cpu_threads_logical,
        memory_gb = hw.memory_gb,
        disk_gb = hw.local_disk_gb,
        disk_type = %hw.local_disk_type,
        gpu = hw.gpu.is_some(),
        "hypervisor: hardware probed"
    );

    let capacity = compute_capacity(&hw);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let hv = Hypervisor {
        id: generate_hypervisor_id(),
        name: node_name.to_string(),
        region: region.to_string(),
        zone: zone.to_string(),
        state: HypervisorState::Registering,
        fabric_node_id: fabric_node_id.to_string(),
        public_ip: public_ip.to_string(),
        fabric_ipv6: fabric_ipv6.to_string(),
        hardware: hw,
        capacity,
        labels: HashMap::new(),
        taints: vec![],
        created_at: now,
    };

    match store.create(&hv) {
        Ok(()) => {
            info!(name = %hv.name, id = %hv.id, "hypervisor: registered in Registering state");
            // Transition to NotReady (probe complete)
            if let Err(e) = store.update_state(&hv.name, HypervisorState::NotReady) {
                warn!(error = %e, "hypervisor: failed to transition to NotReady");
            } else {
                info!(name = %hv.name, "hypervisor: transitioned to NotReady");
            }
            Some(hv.name)
        }
        Err(e) => {
            warn!(error = %e, "hypervisor: failed to register");
            None
        }
    }
}
