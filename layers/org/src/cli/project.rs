//! `syfrah project create|list|delete` handlers.
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

pub async fn create(name: &str, org: &str) -> Result<()> {
    let req = OrgRequest::ProjectCreate {
        name: name.to_string(),
        org: org.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Project(project) => {
            println!(
                "Project '{}' created in organization '{}'.",
                project.name, project.org_id
            );
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn list(org: Option<&str>, json: bool) -> Result<()> {
    let req = OrgRequest::ProjectList {
        org: org.map(String::from),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::ProjectList(projects) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&projects)?);
                return Ok(());
            }

            if projects.is_empty() {
                if let Some(org_name) = org {
                    println!(
                        "No projects found in org '{org_name}'. Create one with: syfrah project create <name> --org {org_name}"
                    );
                } else {
                    println!(
                        "No projects found. Create one with: syfrah project create <name> --org <org>"
                    );
                }
                return Ok(());
            }

            println!("{:<30} {:<20} {:<20}", "NAME", "ORG", "CREATED");
            for p in &projects {
                let created = format_timestamp(p.created_at);
                println!("{:<30} {:<20} {:<20}", p.name, p.org_id, created);
            }
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

pub async fn delete(name: &str, org: &str, yes: bool) -> Result<()> {
    if !yes {
        eprint!("Delete project '{name}' from org '{org}'? This cannot be undone. [y/N] ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let req = OrgRequest::ProjectDelete {
        name: name.to_string(),
        org: org.to_string(),
    };
    let resp = send_org_request(&control_socket_path(), &req)
        .await
        .map_err(daemon_err)?;

    match resp {
        OrgResponse::Ok => {
            println!("Project '{name}' deleted from org '{org}'.");
            Ok(())
        }
        OrgResponse::Error(msg) => anyhow::bail!("{msg}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

fn format_timestamp(ts: u64) -> String {
    if ts == 0 {
        return "-".to_string();
    }
    let secs = ts;
    let days = secs / 86400;
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let mins = (remaining % 3600) / 60;
    let (year, month, day) = epoch_days_to_date(days);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{mins:02}")
}

fn epoch_days_to_date(days: u64) -> (u64, u64, u64) {
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
}
