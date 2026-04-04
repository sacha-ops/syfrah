//! Shared TTY-aware formatting helpers for CLI output.

/// Print a section heading, styled if the terminal is a TTY.
pub fn print_heading(title: &str, is_tty: bool) {
    if is_tty {
        println!(
            "{}",
            console::Style::new().bold().underlined().apply_to(title)
        );
    } else {
        println!("{title}");
        println!("{}", "=".repeat(title.len()));
    }
}

/// Print a key-value pair, with the key bolded if the terminal is a TTY.
pub fn print_kv(key: &str, val: &str, is_tty: bool) {
    if is_tty {
        println!("  {}: {val}", console::Style::new().bold().apply_to(key));
    } else {
        println!("  {key}: {val}");
    }
}

/// Return the path to the daemon's control socket.
pub fn control_socket_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/root"))
        .join(".syfrah")
        .join("control.sock")
}

/// Build a user-friendly error when the daemon is unreachable.
pub fn daemon_connect_error(e: Box<dyn std::error::Error>) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot reach the syfrah daemon -- is it running?\n\
         Start it with: syfrah fabric init ...\n\n\
         Error: {e}"
    )
}

/// Return the current terminal width, falling back to 120 columns.
pub fn term_width() -> usize {
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(120)
}

/// Truncate a string to `max` characters, appending "..." if it exceeds the limit.
///
/// Uses `char_indices` to avoid panicking on multi-byte UTF-8 strings.
pub fn truncate(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        s.to_string()
    } else if max <= 3 {
        s.chars().take(max).collect()
    } else {
        let truncated: String = s.chars().take(max - 3).collect();
        format!("{truncated}...")
    }
}

/// Format a Unix timestamp as a human-readable date string.
pub fn format_timestamp(ts: u64) -> String {
    let secs = ts;
    let days = secs / 86400;
    let (year, month, day) = days_to_date(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(days: u64) -> (u64, u64, u64) {
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
