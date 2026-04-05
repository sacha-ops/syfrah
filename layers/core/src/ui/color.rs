//! Color helpers using the `console` crate.
//!
//! All color formatting goes through these functions so we have
//! a single palette. Colors are applied via ANSI codes and
//! respect the `NO_COLOR` env var.

use console::style;

pub fn green(s: &str) -> String {
    style(s).green().to_string()
}

pub fn red(s: &str) -> String {
    style(s).red().to_string()
}

pub fn yellow(s: &str) -> String {
    style(s).yellow().to_string()
}

pub fn blue(s: &str) -> String {
    style(s).blue().to_string()
}

pub fn dim(s: &str) -> String {
    style(s).dim().to_string()
}

pub fn bold(s: &str) -> String {
    style(s).bold().to_string()
}

/// Color a status indicator based on its value.
pub fn status_color(status: &str) -> String {
    match status.to_lowercase().as_str() {
        "running" | "available" | "active" | "healthy" | "attached" | "pass" => {
            green(&format!("● {status}"))
        }
        "creating" | "pending" | "scheduling" | "starting" => blue(&format!("◌ {status}")),
        "stopped" | "decommissioned" => dim(&format!("■ {status}")),
        "notready" | "degraded" | "draining" => yellow(&format!("▲ {status}")),
        "failed" | "error" | "unreachable" => red(&format!("✖ {status}")),
        _ => format!("  {status}"),
    }
}

/// Color a status indicator without the icon (for tables where space is tight).
pub fn status_dot(status: &str) -> String {
    match status.to_lowercase().as_str() {
        "running" | "available" | "active" | "healthy" | "attached" => {
            format!("{} {status}", green("●"))
        }
        "creating" | "pending" | "scheduling" | "starting" => {
            format!("{} {status}", blue("◌"))
        }
        "stopped" | "decommissioned" => format!("{} {status}", dim("■")),
        "notready" | "degraded" | "draining" => format!("{} {status}", yellow("▲")),
        "failed" | "error" | "unreachable" => format!("{} {status}", red("✖")),
        _ => format!("  {status}"),
    }
}
