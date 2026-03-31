//! `syfrah sg attach|detach|list-attached|create|list|delete` handlers.
//!
//! All operations go through the daemon's control socket.

use std::path::PathBuf;

use anyhow::Result;

use crate::api::{send_org_request, OrgRequest, OrgResponse};

fn control_socket_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

fn daemon_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot reach the syfrah daemon — is it running?\n\
         Start it with: syfrah fabric init ...\n\n\
         Error: {e}"
    )
}

pub async fn run_create(name: &str, vpc: &str, description: &str) -> Result<()> {
    let req = OrgRequest::SgCreate {
        name: name.to_string(),
        vpc: vpc.to_string(),
        description: description.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Sg(sg) => {
            println!("Security group created: {}", sg.name);
            println!("  VPC:         {}", sg.vpc_id);
            println!(
                "  Description: {}",
                sg.description.as_deref().unwrap_or("-")
            );
            println!("  State:       {}", sg.state);
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_list(vpc: Option<&str>, json: bool) -> Result<()> {
    let req = OrgRequest::SgList {
        vpc: vpc.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::SgList(sgs) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&sgs)?);
                return Ok(());
            }

            if sgs.is_empty() {
                println!("No security groups found.");
                return Ok(());
            }

            println!("{:<25} {:<20} {:<10} DESCRIPTION", "NAME", "VPC", "STATE");
            println!("{}", "-".repeat(80));

            for sg in &sgs {
                println!(
                    "{:<25} {:<20} {:<10} {}",
                    sg.name,
                    sg.vpc_id,
                    sg.state,
                    sg.description.as_deref().unwrap_or("-"),
                );
            }

            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_delete(name: &str, vpc: &str) -> Result<()> {
    let req = OrgRequest::SgDelete {
        name: name.to_string(),
        vpc: vpc.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("Security group '{name}' deleted.");
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_attach(sg: &str, vm: Option<&str>, nic: Option<&str>) -> Result<()> {
    let req = OrgRequest::SgAttach {
        sg: sg.to_string(),
        vm: vm.map(String::from),
        nic: nic.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Nic(nic_record) => {
            let target = vm
                .map(|v| format!("VM '{v}'"))
                .or_else(|| nic.map(|n| format!("NIC '{n}'")))
                .unwrap_or_else(|| "target".to_string());
            println!("Security group '{sg}' attached to {target}.");
            println!(
                "  NIC '{}' now has {} security group(s). nftables refresh marked.",
                nic_record.name,
                nic_record.security_groups.len()
            );
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_detach(sg: &str, vm: Option<&str>, nic: Option<&str>) -> Result<()> {
    let req = OrgRequest::SgDetach {
        sg: sg.to_string(),
        vm: vm.map(String::from),
        nic: nic.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Nic(nic_record) => {
            let target = vm
                .map(|v| format!("VM '{v}'"))
                .or_else(|| nic.map(|n| format!("NIC '{n}'")))
                .unwrap_or_else(|| "target".to_string());
            println!("Security group '{sg}' detached from {target}.");
            println!(
                "  NIC '{}' now has {} security group(s). nftables refresh marked.",
                nic_record.name,
                nic_record.security_groups.len()
            );
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn run_list_attached(vm: Option<&str>, nic: Option<&str>, json: bool) -> Result<()> {
    let req = OrgRequest::SgListForNic {
        vm: vm.map(String::from),
        nic: nic.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::SgList(sgs) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&sgs)?);
                return Ok(());
            }

            if sgs.is_empty() {
                println!("No security groups attached.");
                return Ok(());
            }

            let target = vm
                .map(|v| format!("VM '{v}'"))
                .or_else(|| nic.map(|n| format!("NIC '{n}'")))
                .unwrap_or_else(|| "target".to_string());
            println!("Security groups attached to {target}:");
            println!();
            println!("{:<25} {:<20} DESCRIPTION", "NAME", "VPC");
            println!("{}", "-".repeat(70));

            for sg in &sgs {
                println!(
                    "{:<25} {:<20} {}",
                    sg.name,
                    sg.vpc_id,
                    sg.description.as_deref().unwrap_or("-"),
                );
            }

            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}
