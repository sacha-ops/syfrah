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
