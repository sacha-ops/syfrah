//! CLI commands for `syfrah sg ...`.
//!
//! Provides subcommands for security group rule management:
//! - `sg add-rule` — add a firewall rule to a security group
//! - `sg remove-rule` — remove a rule by ID
//! - `sg rules` — list all rules in a security group

pub mod sg;

pub use sg::SgCommand;
