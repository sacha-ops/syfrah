//! Implementation of `syfrah env` subcommands.

use std::collections::HashMap;

use anyhow::{bail, Context};
use syfrah_state::LayerDb;

use crate::store::OrgStore;

// ── Duration parsing ────────────────────────────────────────────────

/// Parse a human-readable duration string into seconds.
///
/// Supported suffixes: `m` (minutes), `h` (hours), `d` (days).
/// Examples: `30m`, `2h`, `48h`, `7d`.
pub fn parse_duration(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("duration cannot be empty");
    }

    let (digits, suffix) = s.split_at(s.len() - 1);
    let value: u64 = digits.parse().with_context(|| {
        format!("invalid duration: '{s}' (expected a number followed by m, h, or d)")
    })?;

    if value == 0 {
        bail!("duration must be greater than zero");
    }

    match suffix {
        "m" => Ok(value * 60),
        "h" => Ok(value * 3600),
        "d" => Ok(value * 86400),
        _ => bail!(
            "invalid duration suffix '{suffix}' in '{s}'. Use m (minutes), h (hours), or d (days). Examples: 30m, 2h, 7d"
        ),
    }
}

/// Parse a `key=value` label string.
fn parse_label(s: &str) -> anyhow::Result<(String, String)> {
    let (key, value) = s
        .split_once('=')
        .with_context(|| format!("invalid label '{s}': expected KEY=VALUE format"))?;
    if key.is_empty() {
        bail!("label key cannot be empty in '{s}'");
    }
    Ok((key.to_string(), value.to_string()))
}

// ── Helpers ─────────────────────────────────────────────────────────

fn open_store() -> anyhow::Result<OrgStore> {
    let db = LayerDb::open("org").context("failed to open org database")?;
    Ok(OrgStore::new(db))
}

fn format_ttl(seconds: Option<u64>) -> String {
    match seconds {
        None => "-".to_string(),
        Some(s) if s >= 86400 && s.is_multiple_of(86400) => format!("{}d", s / 86400),
        Some(s) if s >= 3600 && s.is_multiple_of(3600) => format!("{}h", s / 3600),
        Some(s) if s >= 60 && s.is_multiple_of(60) => format!("{}m", s / 60),
        Some(s) => format!("{s}s"),
    }
}

fn format_labels(labels: &HashMap<String, String>) -> String {
    if labels.is_empty() {
        return "-".to_string();
    }
    let mut pairs: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
    pairs.sort();
    pairs.join(", ")
}

fn format_timestamp(epoch_secs: u64) -> String {
    // Simple human-readable UTC timestamp.
    let secs = epoch_secs;
    // Break into date components manually (avoid chrono dependency).
    // For a CLI display, we show seconds-since-epoch if we don't have
    // a lightweight formatter. Let's do a basic calculation.
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;

    // Compute year/month/day from days since epoch (1970-01-01).
    let (year, month, day) = days_to_ymd(days_since_epoch);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02} UTC")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ── Commands ────────────────────────────────────────────────────────

pub fn run_create(
    name: &str,
    project: &str,
    org: &str,
    ttl_str: Option<&str>,
    deletion_protection: bool,
    label_strs: &[String],
) -> anyhow::Result<()> {
    let ttl_seconds = match ttl_str {
        Some(s) => Some(parse_duration(s)?),
        None => None,
    };

    let mut labels = HashMap::new();
    for l in label_strs {
        let (k, v) = parse_label(l)?;
        labels.insert(k, v);
    }

    let store = open_store()?;
    let env = store
        .create_env(org, project, name, ttl_seconds, deletion_protection, labels)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Environment created: {}", env.name);
    println!("  Project:    {project}");
    println!("  Org:        {org}");
    if let Some(ttl) = env.ttl {
        println!("  TTL:        {}", format_ttl(Some(ttl)));
    }
    if env.deletion_protection {
        println!("  Protected:  yes");
    }
    if !env.labels.is_empty() {
        println!("  Labels:     {}", format_labels(&env.labels));
    }
    println!("  Created:    {}", format_timestamp(env.created_at));

    Ok(())
}

pub fn run_list(project: Option<&str>, org: Option<&str>, json: bool) -> anyhow::Result<()> {
    let store = open_store()?;

    let (org_name, project_name) = match (org, project) {
        (Some(o), Some(p)) => (o, p),
        (None, None) => {
            bail!("specify --org and --project to list environments.\n\nUsage: syfrah env list --org <ORG> --project <PROJECT>");
        }
        (Some(_), None) => {
            bail!("--project is required when --org is specified.\n\nUsage: syfrah env list --org <ORG> --project <PROJECT>");
        }
        (None, Some(_)) => {
            bail!("--org is required when --project is specified.\n\nUsage: syfrah env list --org <ORG> --project <PROJECT>");
        }
    };

    let envs = store
        .list_envs(org_name, project_name)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&envs)?);
        return Ok(());
    }

    if envs.is_empty() {
        println!("No environments found in project '{project_name}' (org: {org_name}).");
        println!(
            "\nCreate one with: syfrah env create <name> --project {project_name} --org {org_name}"
        );
        return Ok(());
    }

    // Table output: NAME, PROJECT, TTL, PROTECTED, LABELS, CREATED
    println!(
        "{:<20} {:<15} {:<8} {:<10} {:<30} CREATED",
        "NAME", "PROJECT", "TTL", "PROTECTED", "LABELS"
    );
    println!("{}", "-".repeat(100));

    for env in &envs {
        let protected = if env.deletion_protection { "yes" } else { "no" };
        println!(
            "{:<20} {:<15} {:<8} {:<10} {:<30} {}",
            env.name,
            project_name,
            format_ttl(env.ttl),
            protected,
            format_labels(&env.labels),
            format_timestamp(env.created_at),
        );
    }

    Ok(())
}

pub fn run_destroy(name: &str, project: &str, org: &str, yes: bool) -> anyhow::Result<()> {
    if !yes {
        eprintln!(
            "This will permanently destroy environment '{name}' in project '{project}' (org: {org})."
        );
        eprintln!("Re-run with --yes to confirm.");
        std::process::exit(1);
    }

    let store = open_store()?;
    store
        .delete_env(org, project, name)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Environment destroyed: {name}");
    Ok(())
}

pub fn run_extend(name: &str, project: &str, org: &str, ttl_str: &str) -> anyhow::Result<()> {
    let ttl_seconds = parse_duration(ttl_str)?;

    let store = open_store()?;
    let env = store
        .extend_env(org, project, name, ttl_seconds)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Environment extended: {name}");
    println!("  New TTL:    {}", format_ttl(env.ttl));
    if let Some(expires) = env.expires_at {
        println!("  Expires:    {}", format_timestamp(expires));
    }

    Ok(())
}

pub fn run_update(
    name: &str,
    project: &str,
    org: &str,
    deletion_protection: bool,
    no_deletion_protection: bool,
) -> anyhow::Result<()> {
    let store = open_store()?;

    if deletion_protection {
        let env = store
            .update_env_protection(org, project, name, true)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!(
            "Deletion protection enabled for environment '{}'.",
            env.name
        );
    } else if no_deletion_protection {
        let env = store
            .update_env_protection(org, project, name, false)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!(
            "Deletion protection disabled for environment '{}'.",
            env.name
        );
    } else {
        anyhow::bail!(
            "specify --deletion-protection or --no-deletion-protection to update the environment"
        );
    }

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_create_parse() {
        // Verify duration parsing for all supported suffixes
        assert_eq!(parse_duration("30m").unwrap(), 1800);
        assert_eq!(parse_duration("2h").unwrap(), 7200);
        assert_eq!(parse_duration("48h").unwrap(), 172800);
        assert_eq!(parse_duration("7d").unwrap(), 604800);

        // Verify label parsing
        let (k, v) = parse_label("region=eu-west").unwrap();
        assert_eq!(k, "region");
        assert_eq!(v, "eu-west");

        // Verify label with empty value
        let (k, v) = parse_label("tag=").unwrap();
        assert_eq!(k, "tag");
        assert_eq!(v, "");

        // Verify label without = fails
        assert!(parse_label("invalid").is_err());

        // Verify empty key fails
        assert!(parse_label("=value").is_err());
    }

    #[test]
    fn env_destroy_parse() {
        // With --yes the command should proceed; without it, it exits.
        // We test the flag parsing via clap derive.
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: crate::cli::EnvCommand,
        }

        // Parse with --yes
        let cli = TestCli::parse_from([
            "test",
            "destroy",
            "staging",
            "--project",
            "backend",
            "--org",
            "acme",
            "--yes",
        ]);

        match cli.cmd {
            crate::cli::EnvCommand::Destroy {
                name,
                project,
                org,
                yes,
            } => {
                assert_eq!(name, "staging");
                assert_eq!(project, "backend");
                assert_eq!(org, "acme");
                assert!(yes);
            }
            _ => panic!("expected Destroy command"),
        }

        // Parse without --yes
        let cli = TestCli::parse_from([
            "test",
            "destroy",
            "staging",
            "--project",
            "backend",
            "--org",
            "acme",
        ]);

        match cli.cmd {
            crate::cli::EnvCommand::Destroy { yes, .. } => {
                assert!(!yes);
            }
            _ => panic!("expected Destroy command"),
        }
    }

    #[test]
    fn env_extend_parse() {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: crate::cli::EnvCommand,
        }

        let cli = TestCli::parse_from([
            "test",
            "extend",
            "ci-run",
            "--project",
            "backend",
            "--org",
            "acme",
            "--ttl",
            "24h",
        ]);

        match cli.cmd {
            crate::cli::EnvCommand::Extend {
                name,
                project,
                org,
                ttl,
            } => {
                assert_eq!(name, "ci-run");
                assert_eq!(project, "backend");
                assert_eq!(org, "acme");
                assert_eq!(ttl, "24h");
                // Verify the duration parses correctly
                assert_eq!(parse_duration(&ttl).unwrap(), 86400);
            }
            _ => panic!("expected Extend command"),
        }
    }

    #[test]
    fn duration_parse_invalid() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("0h").is_err());
    }

    #[test]
    fn format_ttl_display() {
        assert_eq!(format_ttl(None), "-");
        assert_eq!(format_ttl(Some(60)), "1m");
        assert_eq!(format_ttl(Some(3600)), "1h");
        assert_eq!(format_ttl(Some(86400)), "1d");
        assert_eq!(format_ttl(Some(172800)), "2d");
        assert_eq!(format_ttl(Some(90)), "90s");
    }

    #[test]
    fn label_formatting() {
        let mut labels = HashMap::new();
        assert_eq!(format_labels(&labels), "-");

        labels.insert("region".to_string(), "eu-west".to_string());
        assert_eq!(format_labels(&labels), "region=eu-west");

        labels.insert("team".to_string(), "payments".to_string());
        // Sorted output
        assert_eq!(format_labels(&labels), "region=eu-west, team=payments");
    }

    #[test]
    fn env_create_all_flags_parsed() {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(subcommand)]
            cmd: crate::cli::EnvCommand,
        }

        let cli = TestCli::parse_from([
            "test",
            "create",
            "production",
            "--project",
            "backend",
            "--org",
            "acme",
            "--ttl",
            "48h",
            "--deletion-protection",
            "--label",
            "region=eu-west",
            "--label",
            "team=payments",
        ]);

        match cli.cmd {
            crate::cli::EnvCommand::Create {
                name,
                project,
                org,
                ttl,
                deletion_protection,
                labels,
            } => {
                assert_eq!(name, "production");
                assert_eq!(project, "backend");
                assert_eq!(org, "acme");
                assert_eq!(ttl.as_deref(), Some("48h"));
                assert!(deletion_protection);
                assert_eq!(labels.len(), 2);
                assert!(labels.contains(&"region=eu-west".to_string()));
                assert!(labels.contains(&"team=payments".to_string()));
            }
            _ => panic!("expected Create command"),
        }
    }
}
