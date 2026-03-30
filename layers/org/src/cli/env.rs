use clap::Subcommand;

/// Environment management commands.
#[derive(Subcommand)]
pub enum EnvCommand {
    /// Extend an environment's TTL
    Extend {
        /// Environment name
        name: String,
        /// Project name
        #[arg(long)]
        project: String,
        /// Organization name
        #[arg(long)]
        org: String,
        /// Additional TTL duration (e.g. 24h, 7d, 30m)
        #[arg(long)]
        ttl: String,
    },
}

/// Parse a human-readable duration string into seconds.
///
/// Supported suffixes: `s` (seconds), `m` (minutes), `h` (hours), `d` (days).
/// Plain numbers are treated as seconds.
pub fn parse_duration_secs(input: &str) -> anyhow::Result<u64> {
    let input = input.trim();
    if input.is_empty() {
        anyhow::bail!("empty duration");
    }

    let (num_str, multiplier) = if let Some(n) = input.strip_suffix('d') {
        (n, 86400u64)
    } else if let Some(n) = input.strip_suffix('h') {
        (n, 3600u64)
    } else if let Some(n) = input.strip_suffix('m') {
        (n, 60u64)
    } else if let Some(n) = input.strip_suffix('s') {
        (n, 1u64)
    } else {
        (input, 1u64)
    };

    let value: u64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid duration: '{input}'"))?;

    Ok(value * multiplier)
}

/// Execute an env subcommand.
pub fn run_env_command(cmd: &EnvCommand) -> anyhow::Result<()> {
    match cmd {
        EnvCommand::Extend {
            name,
            project,
            org,
            ttl,
        } => {
            let additional_secs = parse_duration_secs(ttl)?;
            let store = crate::store::OrgStore::open()
                .map_err(|e| anyhow::anyhow!("failed to open org store: {e}"))?;
            let updated = crate::ttl::extend_env(&store, org, project, name, additional_secs)
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            let hours = additional_secs / 3600;
            let mins = (additional_secs % 3600) / 60;
            let duration_str = if hours > 0 && mins > 0 {
                format!("{hours}h{mins}m")
            } else if hours > 0 {
                format!("{hours}h")
            } else if mins > 0 {
                format!("{mins}m")
            } else {
                format!("{additional_secs}s")
            };

            println!(
                "Extended environment '{}' by {}. New expiry: {}",
                name,
                duration_str,
                updated.expires_at.unwrap_or(0)
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration_secs("24h").unwrap(), 86400);
    }

    #[test]
    fn parse_duration_days() {
        assert_eq!(parse_duration_secs("7d").unwrap(), 604800);
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration_secs("30m").unwrap(), 1800);
    }

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration_secs("120s").unwrap(), 120);
    }

    #[test]
    fn parse_duration_plain_number() {
        assert_eq!(parse_duration_secs("3600").unwrap(), 3600);
    }

    #[test]
    fn parse_duration_empty_errors() {
        assert!(parse_duration_secs("").is_err());
    }

    #[test]
    fn parse_duration_invalid_errors() {
        assert!(parse_duration_secs("abc").is_err());
    }
}
