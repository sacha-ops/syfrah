//! Spinner and progress bar wrappers.

use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// Create a spinner with a message.
pub fn spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("  {spinner}  {msg}")
            .unwrap(),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

/// Finish a spinner with a success message.
pub fn finish_ok(pb: &ProgressBar, msg: &str) {
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("  ✓  {msg}")
            .unwrap(),
    );
    pb.finish_with_message(msg.to_string());
}

/// Finish a spinner with a success message + right-aligned detail.
pub fn finish_ok_detail(pb: &ProgressBar, msg: &str, detail: &str) {
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("  ✓  {msg}")
            .unwrap(),
    );
    pb.finish_with_message(format!("{msg:<40} {detail}"));
}

/// Finish a spinner with a failure message.
pub fn finish_fail(pb: &ProgressBar, msg: &str) {
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("  ✖  {msg}")
            .unwrap(),
    );
    pb.finish_with_message(msg.to_string());
}

/// Create a progress bar for downloads / long operations.
pub fn progress(msg: &str, total: u64) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("  ↓  {msg}  {bar:20.blue/dim}  {bytes}/{total_bytes}")
            .unwrap()
            .progress_chars("━╸─"),
    );
    pb.set_message(msg.to_string());
    pb
}

/// Finish a progress bar with a success message.
pub fn progress_finish(pb: &ProgressBar, msg: &str) {
    pb.set_style(ProgressStyle::default_bar().template("  ✓  {msg}").unwrap());
    pb.finish_with_message(msg.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_creates_without_panic() {
        let sp = spinner("Loading...");
        finish_ok(&sp, "Done");
    }

    #[test]
    fn spinner_finish_fail() {
        let sp = spinner("Trying...");
        finish_fail(&sp, "Failed");
    }

    #[test]
    fn progress_creates_without_panic() {
        let pb = progress("Downloading", 1000);
        pb.inc(500);
        progress_finish(&pb, "Downloaded");
    }

    #[test]
    fn finish_ok_detail_works() {
        let sp = spinner("Scheduling...");
        finish_ok_detail(&sp, "Scheduled on HYPERVISOR", "fsn1");
    }
}
