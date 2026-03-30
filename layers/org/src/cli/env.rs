//! `syfrah env` subcommands.

use std::collections::HashMap;

use crate::cli::EnvCommand;
use crate::store::OrgStore;

/// Parse a TTL string like "48h", "1h", "3d" into seconds.
fn parse_ttl(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("TTL must not be empty");
    }

    let (num_str, unit) = if let Some(n) = s.strip_suffix('d') {
        (n, 86400u64)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600u64)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60u64)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1u64)
    } else {
        anyhow::bail!("TTL must end with a unit: s, m, h, or d (e.g. '48h', '3d')");
    };

    let num: u64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid TTL number: '{num_str}'"))?;
    if num == 0 {
        anyhow::bail!("TTL must be greater than 0");
    }

    Ok(num * unit)
}

/// Parse labels from "key=value" strings.
fn parse_labels(raw: &[String]) -> anyhow::Result<HashMap<String, String>> {
    let mut labels = HashMap::new();
    for entry in raw {
        let parts: Vec<&str> = entry.splitn(2, '=').collect();
        if parts.len() != 2 {
            anyhow::bail!("invalid label format: '{entry}'. Expected key=value");
        }
        labels.insert(parts[0].to_string(), parts[1].to_string());
    }
    Ok(labels)
}

pub fn run(cmd: EnvCommand) -> anyhow::Result<()> {
    let store = OrgStore::open().map_err(|e| anyhow::anyhow!("{e}"))?;

    match cmd {
        EnvCommand::Create {
            name,
            project,
            org,
            ttl,
            deletion_protection,
            labels,
        } => {
            let ttl_secs = match ttl {
                Some(t) => Some(parse_ttl(&t)?),
                None => None,
            };
            let labels = parse_labels(&labels)?;
            let env = store
                .create_env(&name, &project, &org, ttl_secs, deletion_protection, labels)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "Environment '{}' created in project '{}'.",
                env.name, env.project
            );
            if env.deletion_protection {
                println!("  Deletion protection: enabled");
            }
            if let Some(ttl) = env.ttl {
                println!("  TTL: {}s", ttl);
            }
            Ok(())
        }
        EnvCommand::List { project, org } => {
            let envs = store
                .list_envs(project.as_deref(), org.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if envs.is_empty() {
                println!("No environments found.");
            } else {
                println!(
                    "{:<25} {:<20} {:<15} {:<12} {:<10}",
                    "NAME", "PROJECT", "ORG", "PROTECTED", "TTL"
                );
                for e in envs {
                    let ttl_str = e
                        .ttl
                        .map(|t| format!("{}s", t))
                        .unwrap_or_else(|| "-".to_string());
                    let prot = if e.deletion_protection { "yes" } else { "no" };
                    println!(
                        "{:<25} {:<20} {:<15} {:<12} {:<10}",
                        e.name, e.project, e.org, prot, ttl_str
                    );
                }
            }
            Ok(())
        }
        EnvCommand::Update {
            name,
            project,
            org,
            deletion_protection,
            no_deletion_protection,
        } => {
            if !deletion_protection && !no_deletion_protection {
                anyhow::bail!(
                    "nothing to update. Use --deletion-protection or --no-deletion-protection"
                );
            }
            let enabled = deletion_protection;
            let env = store
                .update_env_protection(&name, &project, &org, enabled)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let status = if env.deletion_protection {
                "enabled"
            } else {
                "disabled"
            };
            println!(
                "Environment '{}': deletion protection {}.",
                env.name, status
            );
            Ok(())
        }
        EnvCommand::Destroy { name, project, org } => {
            store
                .delete_env(&name, &project, &org)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("Environment '{name}' destroyed.");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ttl_hours() {
        assert_eq!(parse_ttl("48h").unwrap(), 172800);
    }

    #[test]
    fn parse_ttl_days() {
        assert_eq!(parse_ttl("3d").unwrap(), 259200);
    }

    #[test]
    fn parse_ttl_minutes() {
        assert_eq!(parse_ttl("30m").unwrap(), 1800);
    }

    #[test]
    fn parse_ttl_invalid() {
        assert!(parse_ttl("").is_err());
        assert!(parse_ttl("0h").is_err());
        assert!(parse_ttl("abc").is_err());
    }

    #[test]
    fn parse_labels_valid() {
        let raw = vec!["region=eu-west".to_string(), "team=payments".to_string()];
        let labels = parse_labels(&raw).unwrap();
        assert_eq!(labels.get("region"), Some(&"eu-west".to_string()));
        assert_eq!(labels.get("team"), Some(&"payments".to_string()));
    }

    #[test]
    fn parse_labels_invalid() {
        let raw = vec!["invalid-no-equals".to_string()];
        assert!(parse_labels(&raw).is_err());
    }
}
