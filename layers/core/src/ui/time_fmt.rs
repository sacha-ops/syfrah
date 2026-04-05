//! Relative time formatting.
//!
//! "2 hours ago", "just now", "3 days ago"

use std::time::{SystemTime, UNIX_EPOCH};

/// Format a unix timestamp as relative time ("2 hours ago", "just now").
pub fn relative(epoch_secs: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if epoch_secs > now {
        return "just now".to_string();
    }

    let diff = now - epoch_secs;

    match diff {
        0..=59 => "just now".to_string(),
        60..=119 => "1 minute ago".to_string(),
        120..=3599 => format!("{} minutes ago", diff / 60),
        3600..=7199 => "1 hour ago".to_string(),
        7200..=86399 => format!("{} hours ago", diff / 3600),
        86400..=172799 => "1 day ago".to_string(),
        172800..=2591999 => format!("{} days ago", diff / 86400),
        2592000..=5183999 => "1 month ago".to_string(),
        _ => format!("{} months ago", diff / 2592000),
    }
}

/// Format seconds as a human duration: "2h 15m", "45s", "3d 1h".
pub fn duration(secs: u64) -> String {
    match secs {
        0 => "0s".to_string(),
        1..=59 => format!("{secs}s"),
        60..=3599 => {
            let m = secs / 60;
            let s = secs % 60;
            if s == 0 {
                format!("{m}m")
            } else {
                format!("{m}m {s}s")
            }
        }
        3600..=86399 => {
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            if m == 0 {
                format!("{h}h")
            } else {
                format!("{h}h {m}m")
            }
        }
        _ => {
            let d = secs / 86400;
            let h = (secs % 86400) / 3600;
            if h == 0 {
                format!("{d}d")
            } else {
                format!("{d}d {h}h")
            }
        }
    }
}

/// Format bytes as human readable: "1.2 GiB", "45 MiB", "128 B".
pub fn bytes(b: u64) -> String {
    match b {
        0 => "0 B".to_string(),
        1..=1023 => format!("{b} B"),
        1024..=1_048_575 => format!("{:.1} KiB", b as f64 / 1024.0),
        1_048_576..=1_073_741_823 => format!("{:.1} MiB", b as f64 / 1_048_576.0),
        _ => format!("{:.1} GiB", b as f64 / 1_073_741_824.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_just_now() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(relative(now), "just now");
        assert_eq!(relative(now + 100), "just now"); // future
    }

    #[test]
    fn relative_minutes() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(relative(now - 120), "2 minutes ago");
        assert_eq!(relative(now - 60), "1 minute ago");
    }

    #[test]
    fn relative_hours() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(relative(now - 3600), "1 hour ago");
        assert_eq!(relative(now - 7200), "2 hours ago");
    }

    #[test]
    fn relative_days() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(relative(now - 86400), "1 day ago");
        assert_eq!(relative(now - 172800), "2 days ago");
    }

    #[test]
    fn duration_seconds() {
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(45), "45s");
    }

    #[test]
    fn duration_minutes() {
        assert_eq!(duration(60), "1m");
        assert_eq!(duration(90), "1m 30s");
        assert_eq!(duration(300), "5m");
    }

    #[test]
    fn duration_hours() {
        assert_eq!(duration(3600), "1h");
        assert_eq!(duration(5400), "1h 30m");
        assert_eq!(duration(8100), "2h 15m");
    }

    #[test]
    fn duration_days() {
        assert_eq!(duration(86400), "1d");
        assert_eq!(duration(90000), "1d 1h");
    }

    #[test]
    fn bytes_formatting() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1_048_576), "1.0 MiB");
        assert_eq!(bytes(1_073_741_824), "1.0 GiB");
    }
}
