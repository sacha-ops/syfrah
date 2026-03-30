//! Rich terminal feedback helpers: spinners, styled output, TTY detection.
//!
//! When stdout is not a terminal (piping, CI, docker exec), all output falls
//! back to plain text with no ANSI escape codes and no spinners.
//!
//! Color output respects:
//! - `--color=always|auto|never` CLI flag (via [`set_color_mode`])
//! - `NO_COLOR` env var (disables color when set)
//! - `FORCE_COLOR` env var (forces color when set)
//! - TTY detection (auto mode, the default)

use console::Style;
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// Color mode set by the `--color` CLI flag.
///
/// 0 = auto (default), 1 = always, 2 = never
static COLOR_MODE: AtomicU8 = AtomicU8::new(0);

const COLOR_AUTO: u8 = 0;
const COLOR_ALWAYS: u8 = 1;
const COLOR_NEVER: u8 = 2;

/// Set the global color mode. Call once from CLI arg parsing.
pub fn set_color_mode(mode: &str) {
    let val = match mode {
        "always" => COLOR_ALWAYS,
        "never" => COLOR_NEVER,
        _ => COLOR_AUTO,
    };
    COLOR_MODE.store(val, Ordering::Relaxed);
    // Propagate to the `console` crate so Style::apply_to respects the setting.
    match val {
        COLOR_ALWAYS => {
            console::set_colors_enabled(true);
            console::set_colors_enabled_stderr(true);
        }
        COLOR_NEVER => {
            console::set_colors_enabled(false);
            console::set_colors_enabled_stderr(false);
        }
        _ => {} // auto: let console decide
    }
}

/// Returns `true` when stdout is connected to a real terminal.
///
/// This is the raw TTY check — used for interactive features like spinners and
/// confirmation prompts that depend on a real terminal regardless of color
/// settings.
pub fn is_tty() -> bool {
    console::Term::stdout().is_term()
}

/// Returns `true` when colored/styled output should be used.
///
/// Checks (in priority order):
/// 1. `--color=always` / `--color=never` CLI flag
/// 2. `FORCE_COLOR` env var (any non-empty value enables color)
/// 3. `NO_COLOR` env var (any non-empty value disables color)
/// 4. TTY detection (color if stdout is a terminal)
pub fn use_color() -> bool {
    match COLOR_MODE.load(Ordering::Relaxed) {
        COLOR_ALWAYS => true,
        COLOR_NEVER => false,
        _ => {
            // FORCE_COLOR takes priority over NO_COLOR
            if std::env::var_os("FORCE_COLOR").is_some_and(|v| !v.is_empty()) {
                return true;
            }
            if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
                return false;
            }
            is_tty()
        }
    }
}

/// Create a cyan spinner with the given message.
///
/// In non-TTY mode returns a hidden progress bar that produces no output.
pub fn spinner(msg: &str) -> ProgressBar {
    if !is_tty() {
        let pb = ProgressBar::hidden();
        pb.set_message(msg.to_string());
        return pb;
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("  {spinner:.cyan} {msg}")
            .expect("valid template"),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

/// Finish a spinner with a green checkmark and success message.
pub fn step_ok(pb: &ProgressBar, msg: &str) {
    if use_color() {
        let green = Style::new().green();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("  {msg}")
                .expect("valid template"),
        );
        pb.finish_with_message(format!("{} {msg}", green.apply_to("\u{2713}")));
    } else {
        pb.finish_and_clear();
        println!("  OK: {msg}");
    }
}

/// Finish a spinner with a red cross and failure message.
pub fn step_fail(pb: &ProgressBar, msg: &str) {
    if use_color() {
        let red = Style::new().red();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("  {msg}")
                .expect("valid template"),
        );
        pb.finish_with_message(format!("{} {msg}", red.apply_to("\u{2717}")));
    } else {
        pb.finish_and_clear();
        eprintln!("  FAIL: {msg}");
    }
}

/// Print a styled key-value line (bold key when color is enabled, plain otherwise).
pub fn info_line(key: &str, value: &str) {
    if use_color() {
        let bold = Style::new().bold();
        println!("  {}: {value}", bold.apply_to(key));
    } else {
        println!("  {key}: {value}");
    }
}

/// Return the current terminal width, falling back to 120 columns.
fn term_width() -> usize {
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(120)
}

/// Print a styled heading.
///
/// The separator / underline width is capped at the terminal width so it
/// doesn't wrap on narrow terminals (see issue #210).
pub fn heading(text: &str) {
    let width = text.len().min(term_width());
    if use_color() {
        let bold = Style::new().bold().underlined();
        println!("{}", bold.apply_to(text));
    } else {
        println!("{text}");
        println!("{}", "=".repeat(width));
    }
}

/// Print a green success line (used for final summaries).
pub fn success(msg: &str) {
    if use_color() {
        let green = Style::new().green().bold();
        println!("{}", green.apply_to(msg));
    } else {
        println!("{msg}");
    }
}

/// Prompt the user for y/n confirmation. Returns `true` if they accept.
///
/// In non-TTY mode always returns `false` (non-interactive defaults to no).
/// Callers that need unattended confirmation should use an explicit flag
/// (e.g. `--force`) instead of relying on the prompt.
pub fn confirm(prompt: &str) -> bool {
    use std::io::Write;
    if !is_tty() {
        return false;
    }
    let yellow = Style::new().yellow();
    eprint!("{} [y/N] ", yellow.apply_to(prompt));
    let _ = std::io::stderr().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Print a yellow warning line.
pub fn warn(msg: &str) {
    if use_color() {
        let yellow = Style::new().yellow();
        eprintln!("{}", yellow.apply_to(msg));
    } else {
        eprintln!("WARNING: {msg}");
    }
}

// ── Box-drawing section helpers ──────────────────────────────────────

const BOX_WIDTH: usize = 50;

/// Print the top border of a section box with an optional title.
pub fn box_top(title: &str) {
    if use_color() {
        let bold = Style::new().bold();
        if title.is_empty() {
            println!("\u{256d}{}\u{256e}", "\u{2500}".repeat(BOX_WIDTH));
        } else {
            let label = format!(" {} ", title);
            let rest = BOX_WIDTH.saturating_sub(label.len() + 1);
            println!(
                "\u{256d}\u{2500}{}{}\u{256e}",
                bold.apply_to(&label),
                "\u{2500}".repeat(rest)
            );
        }
    } else if title.is_empty() {
        println!("{}", "-".repeat(BOX_WIDTH + 2));
    } else {
        println!(
            "-- {} {}",
            title,
            "-".repeat(BOX_WIDTH.saturating_sub(title.len() + 4))
        );
    }
}

/// Print a line inside a box section.
pub fn box_row(text: &str) {
    if use_color() {
        let pad = BOX_WIDTH.saturating_sub(console::measure_text_width(text) + 1);
        println!("\u{2502} {text}{}\u{2502}", " ".repeat(pad));
    } else {
        println!("  {text}");
    }
}

/// Print the bottom border of a section box.
pub fn box_bottom() {
    if use_color() {
        println!("\u{2570}{}\u{256f}", "\u{2500}".repeat(BOX_WIDTH));
    } else {
        println!("{}", "-".repeat(BOX_WIDTH + 2));
    }
}

/// Print a health status line (outside a box). Green check or red cross.
pub fn health_ok(msg: &str) {
    if use_color() {
        let green = Style::new().green().bold();
        println!("  {} {msg}", green.apply_to("\u{25cf}"));
    } else {
        println!("  [OK] {msg}");
    }
}

/// Print a health problem line. Red cross in TTY, plain in non-TTY.
pub fn health_bad(msg: &str) {
    if use_color() {
        let red = Style::new().red().bold();
        println!("  {} {msg}", red.apply_to("\u{2717}"));
    } else {
        println!("  [!!] {msg}");
    }
}

/// Mask a secret string, showing only the prefix tag and last 4 chars.
pub fn mask_secret(secret: &str) -> String {
    if secret.len() <= 8 {
        return "****".to_string();
    }
    // Try to find the prefix like "syf_sk_"
    let prefix_end = secret
        .find('_')
        .and_then(|i| secret[i + 1..].find('_').map(|j| i + 1 + j + 1))
        .unwrap_or(0);
    let last4 = &secret[secret.len() - 4..];
    if prefix_end > 0 {
        format!("{}****...{}", &secret[..prefix_end], last4)
    } else {
        format!("****...{}", last4)
    }
}

/// Print a styled pass/fail check line for diagnostics.
pub fn check_pass(name: &str) {
    if use_color() {
        let green = Style::new().green();
        println!("  {} {name}", green.apply_to("\u{2713}"));
    } else {
        println!("  [PASS] {name}");
    }
}

/// Print a styled fail check line for diagnostics.
pub fn check_fail(name: &str, detail: &str) {
    if use_color() {
        let red = Style::new().red();
        println!("  {} {name}: {detail}", red.apply_to("\u{2717}"));
    } else {
        println!("  [FAIL] {name}: {detail}");
    }
}

/// Print a styled join request card for peering watch.
pub fn join_request_card(node_name: &str, endpoint: &str, wg_key_prefix: &str) {
    if use_color() {
        let cyan = Style::new().cyan();
        let bold = Style::new().bold();
        println!(
            "\n  {} Join request from {}",
            cyan.apply_to("\u{250c}\u{2500}"),
            bold.apply_to(node_name)
        );
        println!("  {}  Endpoint: {endpoint}", cyan.apply_to("\u{2502}"));
        println!("  {}  WG key:   {wg_key_prefix}", cyan.apply_to("\u{2502}"));
        print!("  {} Accept? [Y/n] ", cyan.apply_to("\u{2514}\u{2500}"));
    } else {
        println!("\nJoin request from {node_name} ({endpoint})");
        println!("  WG pubkey: {wg_key_prefix}");
        print!("  Accept? [Y/n] ");
    }
}

/// Print a peering-active banner.
///
/// When `continuous` is true (`--watch` mode), shows "Press Ctrl+C to stop."
/// In default mode, just says "Waiting for join request..." since it exits
/// after the first accept/reject.
pub fn peering_banner(port: u16, pin: Option<&str>, continuous: bool) {
    if use_color() {
        let green = Style::new().green();
        println!(
            "  {} Peering active on port {port}",
            green.apply_to("\u{2713}")
        );
        if let Some(p) = pin {
            println!("  Mode: auto-accept with PIN");
            println!("  Nodes can join with: syfrah fabric join <this-ip> --pin {p}");
        } else if continuous {
            println!("  Mode: manual approval (you will be prompted for each join request)");
            println!("  Press Ctrl+C to stop.");
        } else {
            println!("  Waiting for join request...");
        }
        println!();
    } else {
        println!("Peering active on port {port}");
        if let Some(p) = pin {
            println!("Mode: auto-accept with PIN");
            println!("Nodes can join with: syfrah fabric join <this-ip> --pin {p}");
        } else if continuous {
            println!("Mode: manual approval");
            println!("Press Ctrl+C to stop.");
        } else {
            println!("Waiting for join request...");
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_returns_progress_bar() {
        // In test (non-TTY), should return hidden bar
        let pb = spinner("test message");
        step_ok(&pb, "done");
    }

    #[test]
    fn step_fail_does_not_panic() {
        let pb = spinner("failing");
        step_fail(&pb, "something broke");
    }

    #[test]
    fn info_line_does_not_panic() {
        info_line("Key", "Value");
    }

    #[test]
    fn heading_does_not_panic() {
        heading("Test Heading");
    }

    #[test]
    fn check_pass_fail_do_not_panic() {
        check_pass("something works");
        check_fail("something broke", "details here");
    }

    #[test]
    fn box_drawing_does_not_panic() {
        box_top("Test");
        box_row("some content");
        box_bottom();
        box_top("");
        box_bottom();
    }

    #[test]
    fn health_lines_do_not_panic() {
        health_ok("Daemon running");
        health_bad("Interface down");
    }

    #[test]
    fn mask_secret_hides_middle() {
        let masked = mask_secret("syf_sk_Gegx27CfeNjXiK3ABQZQ1YBk7NpXCunu3eytQYsTkvd1");
        assert!(masked.starts_with("syf_sk_"));
        assert!(masked.ends_with("kvd1"));
        assert!(masked.contains("****"));
        assert!(!masked.contains("Gegx27"));
    }

    #[test]
    fn mask_secret_short_input() {
        assert_eq!(mask_secret("short"), "****");
    }

    #[test]
    fn set_color_mode_always() {
        set_color_mode("always");
        assert!(use_color());
        // Reset to auto for other tests
        set_color_mode("auto");
    }

    #[test]
    fn set_color_mode_never() {
        set_color_mode("never");
        assert!(!use_color());
        // Reset to auto for other tests
        set_color_mode("auto");
    }

    #[test]
    fn set_color_mode_auto_non_tty() {
        set_color_mode("auto");
        // In tests we're not on a TTY, so use_color() returns false
        // (unless FORCE_COLOR or NO_COLOR are set in the env)
        // Just verify it doesn't panic
        let _ = use_color();
    }
}
