//! Confirmation prompts for destructive operations.

use super::color;
use super::Icons;
use std::io::{self, Write};

/// Simple yes/no confirmation. Returns true if user confirms.
pub fn confirm(msg: &str) -> io::Result<bool> {
    eprint!("  {}  {} [y/N] ", color::yellow(Icons::warn()), msg);
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}

/// Type-to-confirm for critical destructive operations.
/// User must type the exact resource name.
pub fn confirm_destructive(kind: &str, name: &str, impact: &[(&str, &str)]) -> io::Result<bool> {
    eprintln!();
    eprintln!("  {}  Delete {kind} {name}?", color::yellow(Icons::warn()),);
    eprintln!();

    // Show impact
    if !impact.is_empty() {
        let max_label = impact.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (label, value) in impact {
            eprintln!(
                "  {:<width$}  {}",
                color::dim(label),
                value,
                width = max_label
            );
        }
        eprintln!();
    }

    eprintln!("  This action is permanent and cannot be undone.");
    eprint!("  Type \"{name}\" to confirm: ");
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    if input.trim() == name {
        Ok(true)
    } else {
        eprintln!();
        eprintln!("  Aborted.");
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_exist() {
        // Verify the module compiles and Icons are accessible
        let _ = Icons::warn();
        let _ = Icons::ok();
    }
}
