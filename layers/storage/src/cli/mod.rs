//! CLI commands for `syfrah volume ...` and `syfrah storage ...`.
//!
//! Provides subcommands for volume lifecycle management and storage
//! configuration. Each handler communicates with the daemon via the
//! control socket.

pub mod configure;
pub mod volume;

use clap::Subcommand;

/// Top-level volume CLI command.
#[derive(Debug, Subcommand)]
pub enum VolumeCommand {
    /// Create a new block volume
    #[command(after_help = "Examples:\n  \
            syfrah volume create pgdata --size 50 --project backend --org acme\n  \
            syfrah volume create redis-data --size 10 --project cache --org acme --env staging")]
    Create {
        /// Volume name (lowercase alphanumeric and hyphens, 3-63 chars)
        #[arg(allow_hyphen_values = true)]
        name: String,
        /// Size in gigabytes
        #[arg(long)]
        size: u64,
        /// Project the volume belongs to
        #[arg(long)]
        project: String,
        /// Organization the volume belongs to
        #[arg(long)]
        org: String,
        /// Environment (optional, for scoping)
        #[arg(long)]
        env: Option<String>,
    },
    /// List volumes
    #[command(after_help = "Examples:\n  \
            syfrah volume list\n  \
            syfrah volume list --project backend --org acme\n  \
            syfrah volume list --json")]
    List {
        /// Filter by project name
        #[arg(long)]
        project: Option<String>,
        /// Filter by organization name
        #[arg(long)]
        org: Option<String>,
        /// Filter by environment name
        #[arg(long)]
        env: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Get volume details
    #[command(after_help = "Examples:\n  \
            syfrah volume get pgdata\n  \
            syfrah volume get pgdata --project backend --json")]
    Get {
        /// Volume name
        name: String,
        /// Project the volume belongs to (auto-detected if name is unique)
        #[arg(long)]
        project: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a volume
    #[command(after_help = "Examples:\n  \
            syfrah volume delete pgdata --project backend\n  \
            syfrah volume delete pgdata --cascade")]
    Delete {
        /// Volume name
        name: String,
        /// Project the volume belongs to (auto-detected if name is unique)
        #[arg(long)]
        project: Option<String>,
        /// Also delete any snapshots derived from this volume
        #[arg(long)]
        cascade: bool,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Resize a volume (grow only)
    #[command(after_help = "Examples:\n  \
            syfrah volume resize pgdata --size 100\n  \
            syfrah volume resize pgdata --size 100 --project backend")]
    Resize {
        /// Volume name
        name: String,
        /// New size in gigabytes (must be larger than current size)
        #[arg(long)]
        size: u64,
        /// Project the volume belongs to (auto-detected if name is unique)
        #[arg(long)]
        project: Option<String>,
    },
    /// Update volume settings
    #[command(after_help = "Examples:\n  \
            syfrah volume update pgdata --deletion-protection\n  \
            syfrah volume update pgdata --no-deletion-protection")]
    Update {
        /// Volume name
        name: String,
        /// Project the volume belongs to (auto-detected if name is unique)
        #[arg(long)]
        project: Option<String>,
        /// Enable deletion protection
        #[arg(long, conflicts_with = "no_deletion_protection")]
        deletion_protection: bool,
        /// Disable deletion protection
        #[arg(long, conflicts_with = "deletion_protection")]
        no_deletion_protection: bool,
    },
}

/// Top-level storage CLI command (`syfrah storage ...`).
#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    /// Configure storage backend (S3 endpoint, bucket, cache)
    #[command(after_help = "Examples:\n  \
            syfrah storage configure --region eu-west \\\n    \
              --s3-endpoint https://s3.par.io.cloud.ovh.net \\\n    \
              --s3-bucket syfrah-storage --s3-access-key AKID --s3-secret-key SECRET \\\n    \
              --cache-disk-path /dev/nvme1n1 --cache-disk-size 200 --cache-memory-size 8\n  \
            syfrah storage configure --region eu-west \\\n    \
              --s3-endpoint https://s3.par.io.cloud.ovh.net \\\n    \
              --s3-bucket syfrah-storage --s3-access-key AKID --s3-secret-key SECRET \\\n    \
              --encryption-passphrase-file /path/to/passphrase\n  \
            syfrah storage configure --cache-disk-path /dev/nvme1n1 \\\n    \
              --cache-disk-size 200 --cache-memory-size 8")]
    Configure {
        /// Target region for this storage configuration
        #[arg(long)]
        region: Option<String>,
        /// S3-compatible endpoint URL (must start with https:// or http://)
        #[arg(long)]
        s3_endpoint: Option<String>,
        /// S3 bucket name
        #[arg(long)]
        s3_bucket: Option<String>,
        /// S3 access key (can also be set via SYFRAH_S3_ACCESS_KEY env var)
        #[arg(long, env = "SYFRAH_S3_ACCESS_KEY")]
        s3_access_key: Option<String>,
        /// S3 secret key (can also be set via SYFRAH_S3_SECRET_KEY env var)
        #[arg(long, env = "SYFRAH_S3_SECRET_KEY")]
        s3_secret_key: Option<String>,
        /// Path to local disk used for warm cache
        #[arg(long)]
        cache_disk_path: Option<String>,
        /// Maximum cache disk size in gigabytes
        #[arg(long)]
        cache_disk_size: Option<u32>,
        /// Maximum memory cache size in gigabytes
        #[arg(long)]
        cache_memory_size: Option<u32>,
        /// Encryption passphrase (stored locally at /etc/syfrah/storage-key, never replicated).
        /// Can also be set via SYFRAH_ENCRYPTION_PASSPHRASE env var.
        /// Prefer --encryption-passphrase-file to avoid exposing secrets in process listings.
        #[arg(long, env = "SYFRAH_ENCRYPTION_PASSPHRASE")]
        encryption_passphrase: Option<String>,
        /// Path to a file containing the encryption passphrase (alternative to --encryption-passphrase)
        #[arg(long, conflicts_with = "encryption_passphrase")]
        encryption_passphrase_file: Option<String>,
    },
}

/// Execute a storage CLI command.
pub async fn run_storage(cmd: StorageCommand) -> anyhow::Result<()> {
    match cmd {
        StorageCommand::Configure {
            region,
            s3_endpoint,
            s3_bucket,
            s3_access_key,
            s3_secret_key,
            cache_disk_path,
            cache_disk_size,
            cache_memory_size,
            encryption_passphrase,
            encryption_passphrase_file,
        } => {
            // Resolve passphrase: --encryption-passphrase-file takes a file path,
            // read its contents and use that as the passphrase.
            let encryption_passphrase = match (encryption_passphrase, encryption_passphrase_file) {
                (Some(p), _) => Some(p),
                (None, Some(path)) => {
                    let contents = std::fs::read_to_string(&path).map_err(|e| {
                        anyhow::anyhow!("failed to read encryption passphrase from '{path}': {e}")
                    })?;
                    let trimmed = contents.trim_end_matches('\n').to_string();
                    if trimmed.is_empty() {
                        anyhow::bail!("encryption passphrase file '{path}' is empty");
                    }
                    Some(trimmed)
                }
                (None, None) => None,
            };
            // Per-HV cache override: if only --cache-* flags provided, update cache only
            let has_s3 = s3_endpoint.is_some()
                || s3_bucket.is_some()
                || s3_access_key.is_some()
                || s3_secret_key.is_some()
                || region.is_some();
            let has_cache = cache_disk_path.is_some()
                || cache_disk_size.is_some()
                || cache_memory_size.is_some();

            if !has_s3 && has_cache {
                // Cache-only override
                let disk = cache_disk_path.ok_or_else(|| {
                    anyhow::anyhow!(
                        "--cache-disk-path is required for cache-only configuration.\n\n\
                         Usage: syfrah storage configure --cache-disk-path /dev/nvme1n1 \
                         --cache-disk-size <GB> --cache-memory-size <GB>"
                    )
                })?;
                let disk_size = cache_disk_size.ok_or_else(|| {
                    anyhow::anyhow!(
                        "--cache-disk-size is required for cache-only configuration.\n\n\
                         Usage: syfrah storage configure --cache-disk-path /dev/nvme1n1 \
                         --cache-disk-size <GB> --cache-memory-size <GB>"
                    )
                })?;
                let mem_size = cache_memory_size.ok_or_else(|| {
                    anyhow::anyhow!(
                        "--cache-memory-size is required for cache-only configuration.\n\n\
                         Usage: syfrah storage configure --cache-disk-path /dev/nvme1n1 \
                         --cache-disk-size <GB> --cache-memory-size <GB>"
                    )
                })?;
                return configure::run_configure_cache(&disk, disk_size, mem_size).await;
            }

            if !has_s3 && !has_cache {
                anyhow::bail!(
                    "no configuration flags provided.\n\n\
                     Full configuration:\n  \
                     syfrah storage configure --region <region> \\\n    \
                       --s3-endpoint <url> --s3-bucket <bucket> \\\n    \
                       --s3-access-key <key> --s3-secret-key <key>\n\n\
                     Cache-only override:\n  \
                     syfrah storage configure --cache-disk-path <path> \\\n    \
                       --cache-disk-size <GB> --cache-memory-size <GB>"
                );
            }

            // Full S3 configuration — require all S3 fields
            let region = region.ok_or_else(|| {
                anyhow::anyhow!(
                    "--region is required for storage configuration.\n\n\
                     Usage: syfrah storage configure --region <region> \
                     --s3-endpoint <url> --s3-bucket <bucket> \
                     --s3-access-key <key> --s3-secret-key <key>"
                )
            })?;
            let endpoint = s3_endpoint.ok_or_else(|| {
                anyhow::anyhow!(
                    "--s3-endpoint is required for storage configuration.\n\n\
                     Usage: syfrah storage configure --region {region} \
                     --s3-endpoint <url> --s3-bucket <bucket> \
                     --s3-access-key <key> --s3-secret-key <key>"
                )
            })?;
            let bucket = s3_bucket.ok_or_else(|| {
                anyhow::anyhow!(
                    "--s3-bucket is required for storage configuration.\n\n\
                     Usage: syfrah storage configure --region {region} \
                     --s3-endpoint {endpoint} --s3-bucket <bucket> \
                     --s3-access-key <key> --s3-secret-key <key>"
                )
            })?;
            let access_key = s3_access_key.ok_or_else(|| {
                anyhow::anyhow!(
                    "--s3-access-key is required for storage configuration.\n\n\
                     Usage: syfrah storage configure --region {region} \
                     --s3-endpoint {endpoint} --s3-bucket {bucket} \
                     --s3-access-key <key> --s3-secret-key <key>"
                )
            })?;
            let secret_key = s3_secret_key.ok_or_else(|| {
                anyhow::anyhow!(
                    "--s3-secret-key is required for storage configuration.\n\n\
                     Usage: syfrah storage configure --region {region} \
                     --s3-endpoint {endpoint} --s3-bucket {bucket} \
                     --s3-access-key {access_key} --s3-secret-key <key>"
                )
            })?;

            configure::run_configure(&configure::ConfigureParams {
                region: &region,
                s3_endpoint: &endpoint,
                s3_bucket: &bucket,
                s3_access_key: &access_key,
                s3_secret_key: &secret_key,
                cache_disk: cache_disk_path.as_deref(),
                cache_disk_size,
                cache_memory_size,
                encryption_passphrase: encryption_passphrase.as_deref(),
            })
            .await
        }
    }
}

/// Execute a volume CLI command.
pub async fn run(cmd: VolumeCommand) -> anyhow::Result<()> {
    match cmd {
        VolumeCommand::Create {
            name,
            size,
            project,
            org,
            env,
        } => volume::run_create(&name, size, &project, &org, env.as_deref()).await,
        VolumeCommand::List {
            project,
            org,
            env,
            json,
        } => volume::run_list(project.as_deref(), org.as_deref(), env.as_deref(), json).await,
        VolumeCommand::Get {
            name,
            project,
            json,
        } => volume::run_get(&name, project.as_deref(), json).await,
        VolumeCommand::Delete {
            name,
            project,
            cascade,
            yes,
        } => volume::run_delete(&name, project.as_deref(), cascade, yes).await,
        VolumeCommand::Resize {
            name,
            size,
            project,
        } => volume::run_resize(&name, size, project.as_deref()).await,
        VolumeCommand::Update {
            name,
            project,
            deletion_protection,
            no_deletion_protection,
        } => {
            volume::run_update(
                &name,
                project.as_deref(),
                deletion_protection,
                no_deletion_protection,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::StorageCommand;

    /// Helper to parse storage commands from an arg list.
    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: StorageCommand,
    }

    fn parse(args: &[&str]) -> StorageCommand {
        let full_args = std::iter::once("test").chain(args.iter().copied());
        TestCli::parse_from(full_args).cmd
    }

    #[test]
    fn configure_full_parse() {
        let cmd = parse(&[
            "configure",
            "--region",
            "eu-west",
            "--s3-endpoint",
            "https://s3.par.io.cloud.ovh.net",
            "--s3-bucket",
            "syfrah-storage",
            "--s3-access-key",
            "AKID",
            "--s3-secret-key",
            "SECRET",
            "--cache-disk-path",
            "/dev/nvme1n1",
            "--cache-disk-size",
            "200",
            "--cache-memory-size",
            "8",
        ]);
        match cmd {
            StorageCommand::Configure {
                region,
                s3_endpoint,
                s3_bucket,
                s3_access_key,
                s3_secret_key,
                cache_disk_path,
                cache_disk_size,
                cache_memory_size,
                encryption_passphrase,
                encryption_passphrase_file,
            } => {
                assert_eq!(region.as_deref(), Some("eu-west"));
                assert_eq!(
                    s3_endpoint.as_deref(),
                    Some("https://s3.par.io.cloud.ovh.net")
                );
                assert_eq!(s3_bucket.as_deref(), Some("syfrah-storage"));
                assert_eq!(s3_access_key.as_deref(), Some("AKID"));
                assert_eq!(s3_secret_key.as_deref(), Some("SECRET"));
                assert_eq!(cache_disk_path.as_deref(), Some("/dev/nvme1n1"));
                assert_eq!(cache_disk_size, Some(200));
                assert_eq!(cache_memory_size, Some(8));
                assert!(encryption_passphrase.is_none());
                assert!(encryption_passphrase_file.is_none());
            }
        }
    }

    #[test]
    fn configure_cache_only_parse() {
        let cmd = parse(&[
            "configure",
            "--cache-disk-path",
            "/dev/nvme1n1",
            "--cache-disk-size",
            "200",
            "--cache-memory-size",
            "8",
        ]);
        match cmd {
            StorageCommand::Configure {
                region,
                s3_endpoint,
                s3_bucket,
                cache_disk_path,
                cache_disk_size,
                cache_memory_size,
                ..
            } => {
                assert!(region.is_none());
                assert!(s3_endpoint.is_none());
                assert!(s3_bucket.is_none());
                assert_eq!(cache_disk_path.as_deref(), Some("/dev/nvme1n1"));
                assert_eq!(cache_disk_size, Some(200));
                assert_eq!(cache_memory_size, Some(8));
            }
        }
    }

    #[test]
    fn configure_with_encryption_passphrase() {
        let cmd = parse(&[
            "configure",
            "--region",
            "eu-west",
            "--s3-endpoint",
            "https://s3.example.com",
            "--s3-bucket",
            "bucket",
            "--s3-access-key",
            "AKID",
            "--s3-secret-key",
            "SECRET",
            "--encryption-passphrase",
            "my-secret",
        ]);
        match cmd {
            StorageCommand::Configure {
                encryption_passphrase,
                encryption_passphrase_file,
                ..
            } => {
                assert_eq!(encryption_passphrase.as_deref(), Some("my-secret"));
                assert!(encryption_passphrase_file.is_none());
            }
        }
    }

    #[test]
    fn configure_with_encryption_passphrase_file() {
        let cmd = parse(&[
            "configure",
            "--region",
            "eu-west",
            "--s3-endpoint",
            "https://s3.example.com",
            "--s3-bucket",
            "bucket",
            "--s3-access-key",
            "AKID",
            "--s3-secret-key",
            "SECRET",
            "--encryption-passphrase-file",
            "/tmp/secret.key",
        ]);
        match cmd {
            StorageCommand::Configure {
                encryption_passphrase,
                encryption_passphrase_file,
                ..
            } => {
                assert!(encryption_passphrase.is_none());
                assert_eq!(
                    encryption_passphrase_file.as_deref(),
                    Some("/tmp/secret.key")
                );
            }
        }
    }

    #[test]
    fn configure_no_flags() {
        let cmd = parse(&["configure"]);
        match cmd {
            StorageCommand::Configure {
                region,
                s3_endpoint,
                s3_bucket,
                s3_access_key,
                s3_secret_key,
                cache_disk_path,
                cache_disk_size,
                cache_memory_size,
                encryption_passphrase,
                encryption_passphrase_file,
            } => {
                assert!(region.is_none());
                assert!(s3_endpoint.is_none());
                assert!(s3_bucket.is_none());
                assert!(s3_access_key.is_none());
                assert!(s3_secret_key.is_none());
                assert!(cache_disk_path.is_none());
                assert!(cache_disk_size.is_none());
                assert!(cache_memory_size.is_none());
                assert!(encryption_passphrase.is_none());
                assert!(encryption_passphrase_file.is_none());
            }
        }
    }
}
