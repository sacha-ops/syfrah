//! Terminal UI system for Syfrah.
//!
//! A single `Ui` object controls all terminal output. No `println!` in business logic.
//!
//! # Modes
//!
//! - **Human** (default TTY): colors, icons, tables, spinners
//! - **Pipe** (stdout not a TTY): TSV, no color, no animation
//! - **JSON** (`--json`): structured JSON only
//! - **Quiet** (`--quiet`): data only, tab-separated

mod color;
mod confirm;
pub mod prompt;
mod spinner;
mod table;
mod time_fmt;

pub use color::*;
pub use confirm::*;
pub use prompt::*;
pub use spinner::*;
pub use table::*;
pub use time_fmt::*;

use crate::error::SyfrahError;
use std::io::{self, Write};

// ═══════════════════════════════════════════════════
// Output format
// ═══════════════════════════════════════════════════

/// How to format output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Human,
    Json,
    Quiet,
}

/// Color mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

// ═══════════════════════════════════════════════════
// Icons
// ═══════════════════════════════════════════════════

pub struct Icons;

impl Icons {
    pub fn ok() -> &'static str {
        "✓"
    }
    pub fn fail() -> &'static str {
        "✖"
    }
    pub fn warn() -> &'static str {
        "▲"
    }
    pub fn pending() -> &'static str {
        "◌"
    }
    pub fn active() -> &'static str {
        "●"
    }
    pub fn stopped() -> &'static str {
        "■"
    }
    pub fn download() -> &'static str {
        "↓"
    }
}

// ═══════════════════════════════════════════════════
// Ui — the central output controller
// ═══════════════════════════════════════════════════

/// The central UI controller. All output goes through this.
pub struct Ui {
    pub format: OutputFormat,
    pub color: ColorMode,
    is_tty: bool,
    term_width: u16,
}

impl Ui {
    /// Create from CLI flags.
    pub fn new(format: OutputFormat, color_str: &str) -> Self {
        let is_tty = console::Term::stderr().is_term();
        let color = match color_str {
            "always" => ColorMode::Always,
            "never" => ColorMode::Never,
            _ => ColorMode::Auto,
        };
        let term_width = terminal_size::terminal_size()
            .map(|(w, _)| w.0)
            .unwrap_or(80);

        Self {
            format,
            color,
            is_tty,
            term_width,
        }
    }

    /// Is color enabled?
    pub fn has_color(&self) -> bool {
        match self.color {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => self.is_tty,
        }
    }

    /// Terminal width.
    pub fn width(&self) -> u16 {
        self.term_width
    }

    /// Is this a human-interactive terminal?
    pub fn is_interactive(&self) -> bool {
        self.is_tty && self.format == OutputFormat::Human
    }

    // ── Steps ──────────────────────────────────────

    /// Print a completed step: "  ✓  message"
    pub fn step(&self, msg: &str) {
        if self.format == OutputFormat::Quiet {
            return;
        }
        if self.format == OutputFormat::Json {
            println!("{}", serde_json::json!({"event": "step", "message": msg}));
            return;
        }
        if self.has_color() {
            eprintln!("  {}  {}", color::green(Icons::ok()), msg);
        } else {
            eprintln!("  {}  {}", Icons::ok(), msg);
        }
    }

    /// Print a completed step with a right-aligned detail.
    pub fn step_detail(&self, msg: &str, detail: &str) {
        if self.format != OutputFormat::Human {
            self.step(msg);
            return;
        }
        let pad = (self.term_width as usize).saturating_sub(4 + msg.len() + detail.len() + 2);
        if self.has_color() {
            eprintln!(
                "  {}  {}{:>pad$}",
                color::green(Icons::ok()),
                msg,
                color::dim(detail),
                pad = pad,
            );
        } else {
            eprintln!("  {}  {}{:>pad$}", Icons::ok(), msg, detail, pad = pad);
        }
    }

    /// Print a warning: "  ▲  message"
    pub fn warn(&self, msg: &str) {
        if self.format == OutputFormat::Json {
            println!("{}", serde_json::json!({"event": "warn", "message": msg}));
            return;
        }
        if self.has_color() {
            eprintln!("  {}  {}", color::yellow(Icons::warn()), msg);
        } else {
            eprintln!("  {}  {}", Icons::warn(), msg);
        }
    }

    /// Print an error from SyfrahError.
    pub fn error(&self, err: &SyfrahError) {
        if self.format == OutputFormat::Json {
            println!("{}", err.format_json());
            return;
        }
        if self.has_color() {
            eprintln!(
                "\n  {}  {}",
                color::red(Icons::fail()),
                color::red(&err.message)
            );
        } else {
            eprintln!("\n  {}  {}", Icons::fail(), err.message);
        }
        if let Some(suggestion) = &err.suggestion {
            eprintln!();
            if self.has_color() {
                eprintln!("  {}", color::dim(suggestion));
            } else {
                eprintln!("  {}", suggestion);
            }
        }
        eprintln!();
    }

    /// Print an error from a string.
    pub fn error_str(&self, msg: &str) {
        self.error(&SyfrahError::internal(msg));
    }

    // ── Info blocks ────────────────────────────────

    /// Print a title line.
    pub fn title(&self, title: &str) {
        if self.format != OutputFormat::Human {
            return;
        }
        eprintln!();
        eprintln!("  {}", title);
        eprintln!();
    }

    /// Print key-value pairs (detail view).
    pub fn info(&self, pairs: &[(&str, &str)]) {
        if self.format == OutputFormat::Json {
            let obj: serde_json::Map<String, serde_json::Value> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                .collect();
            println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
            return;
        }
        if self.format == OutputFormat::Quiet {
            for (k, v) in pairs {
                println!("{k}\t{v}");
            }
            return;
        }
        let max_label = pairs.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (label, value) in pairs {
            if self.has_color() {
                eprintln!(
                    "  {:<width$}  {}",
                    color::dim(label),
                    value,
                    width = max_label
                );
            } else {
                eprintln!("  {:<width$}  {}", label, value, width = max_label);
            }
        }
    }

    /// Print a suggestion / next step.
    pub fn next(&self, label: &str, cmd: &str) {
        if self.format != OutputFormat::Human {
            return;
        }
        eprintln!();
        if self.has_color() {
            eprintln!("  {}   {}", color::dim(label), cmd);
        } else {
            eprintln!("  {}   {}", label, cmd);
        }
        eprintln!();
    }

    // ── Empty states ───────────────────────────────

    /// Print an empty state with a hint command.
    pub fn empty(&self, msg: &str, hint_cmd: Option<&str>) {
        if self.format == OutputFormat::Json {
            println!("[]");
            return;
        }
        if self.format == OutputFormat::Quiet {
            return;
        }
        eprintln!();
        if self.has_color() {
            eprintln!("  {}", color::dim(msg));
        } else {
            eprintln!("  {}", msg);
        }
        if let Some(cmd) = hint_cmd {
            eprintln!();
            if self.has_color() {
                eprintln!("  {}", color::dim(cmd));
            } else {
                eprintln!("  {}", cmd);
            }
        }
        eprintln!();
    }

    // ── Summary ────────────────────────────────────

    /// Print a summary line below a table (dim).
    pub fn summary(&self, msg: &str) {
        if self.format != OutputFormat::Human {
            return;
        }
        eprintln!();
        if self.has_color() {
            eprintln!("  {}", color::dim(msg));
        } else {
            eprintln!("  {}", msg);
        }
    }

    // ── Raw ────────────────────────────────────────

    /// Print raw JSON (for --json mode data output).
    pub fn json(&self, value: &serde_json::Value) {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        );
    }

    /// Print a blank line.
    pub fn blank(&self) {
        if self.format == OutputFormat::Human {
            eprintln!();
        }
    }

    /// Flush stderr.
    pub fn flush(&self) {
        let _ = io::stderr().flush();
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self::new(OutputFormat::Human, "auto")
    }
}
