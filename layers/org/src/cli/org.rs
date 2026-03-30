//! Org subcommand handlers.
//!
//! All operations are local (redb), no daemon needed.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::store::OrgStore;
use crate::types::{validate_org_name, Org};

/// Create a new organization.
pub fn run_create(name: String) -> anyhow::Result<()> {
    if let Err(e) = validate_org_name(&name) {
        anyhow::bail!("{e}");
    }

    let store = OrgStore::open().map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let org = Org {
        name: name.clone(),
        created_at: now,
    };

    match store.create(&org) {
        Ok(()) => {
            println!("Organization '{name}' created.");
            Ok(())
        }
        Err(crate::store::OrgStoreError::AlreadyExists(_)) => {
            anyhow::bail!("org '{name}' already exists");
        }
        Err(e) => {
            anyhow::bail!("failed to create org: {e}");
        }
    }
}

/// List all organizations.
pub fn run_list(json: bool) -> anyhow::Result<()> {
    let store = OrgStore::open().map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;

    let orgs = store
        .list()
        .map_err(|e| anyhow::anyhow!("failed to list orgs: {e}"))?;

    if json {
        let json_str = serde_json::to_string_pretty(&orgs)?;
        println!("{json_str}");
        return Ok(());
    }

    if orgs.is_empty() {
        println!("(no organizations)");
        return Ok(());
    }

    let tw = term_width();
    let header = format!("{:<30} {:<20}", "NAME", "CREATED");
    if console::Term::stdout().is_term() {
        let truncated = &header[..header.len().min(tw)];
        println!("{}", console::Style::new().bold().apply_to(truncated));
    } else {
        println!("{}", &header[..header.len().min(tw)]);
    }
    println!("{}", "-".repeat(50.min(tw)));

    for org in &orgs {
        let created = format_timestamp(org.created_at);
        let row = format!("{:<30} {:<20}", org.name, created);
        println!("{}", &row[..row.len().min(tw)]);
    }

    Ok(())
}

/// Delete an organization.
pub fn run_delete(name: String, yes: bool) -> anyhow::Result<()> {
    let store = OrgStore::open().map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;

    // Check it exists before prompting
    match store.get(&name) {
        Ok(Some(_)) => {}
        Ok(None) => anyhow::bail!("org '{name}' not found"),
        Err(e) => anyhow::bail!("failed to look up org: {e}"),
    }

    if !yes {
        eprint!("Delete organization '{name}'? This cannot be undone. [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        let answer = answer.trim();
        if answer != "y" && answer != "Y" {
            eprintln!("Aborted.");
            std::process::exit(1);
        }
    }

    match store.delete(&name) {
        Ok(()) => {
            println!("Organization '{name}' deleted.");
            Ok(())
        }
        Err(crate::store::OrgStoreError::NotFound(_)) => {
            anyhow::bail!("org '{name}' not found");
        }
        Err(e) => {
            anyhow::bail!("failed to delete org: {e}");
        }
    }
}

/// Return the current terminal width, falling back to 120 columns.
fn term_width() -> usize {
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(120)
}

/// Format a Unix timestamp as a human-readable date string.
fn format_timestamp(ts: u64) -> String {
    let secs = ts;
    let days = secs / 86400;
    // Simple date calculation from days since epoch
    let (year, month, day) = days_to_date(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::super::OrgCommand;

    /// Helper to parse org commands from an arg list.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: OrgCommand,
    }

    fn parse(args: &[&str]) -> OrgCommand {
        let full_args = std::iter::once("test").chain(args.iter().copied());
        TestCli::parse_from(full_args).cmd
    }

    #[test]
    fn org_create_parse() {
        let cmd = parse(&["create", "acme"]);
        match cmd {
            OrgCommand::Create { name } => {
                assert_eq!(name, "acme");
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn org_delete_parse() {
        let cmd = parse(&["delete", "acme", "--yes"]);
        match cmd {
            OrgCommand::Delete { name, yes } => {
                assert_eq!(name, "acme");
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn org_delete_parse_no_yes() {
        let cmd = parse(&["delete", "acme"]);
        match cmd {
            OrgCommand::Delete { name, yes } => {
                assert_eq!(name, "acme");
                assert!(!yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn org_delete_parse_short_y() {
        let cmd = parse(&["delete", "-y", "acme"]);
        match cmd {
            OrgCommand::Delete { name, yes } => {
                assert_eq!(name, "acme");
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn org_list_parse() {
        let cmd = parse(&["list"]);
        assert!(matches!(cmd, OrgCommand::List { json: false }));
    }

    #[test]
    fn org_list_parse_json() {
        let cmd = parse(&["list", "--json"]);
        assert!(matches!(cmd, OrgCommand::List { json: true }));
    }
}
