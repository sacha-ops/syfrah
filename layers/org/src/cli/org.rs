//! `syfrah org create|list|delete` handlers.

use anyhow::Result;

use crate::store;
use crate::types::validate_name;

pub fn create(name: &str) -> Result<()> {
    validate_name(name)?;
    let db = store::open()?;
    let org = store::create_org(&db, name)?;
    println!("Organization '{}' created.", org.name);
    Ok(())
}

pub fn list(json: bool) -> Result<()> {
    let db = store::open()?;
    let orgs = store::list_orgs(&db)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&orgs)?);
        return Ok(());
    }

    if orgs.is_empty() {
        println!("No organizations found. Create one with: syfrah org create <name>");
        return Ok(());
    }

    println!("{:<30} {:<20}", "NAME", "CREATED");
    for org in &orgs {
        let created = format_timestamp(org.created_at);
        println!("{:<30} {:<20}", org.name, created);
    }
    Ok(())
}

pub fn delete(name: &str, yes: bool) -> Result<()> {
    if !yes {
        eprint!("Delete organization '{name}'? This cannot be undone. [y/N] ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }
    let db = store::open()?;
    store::delete_org(&db, name)?;
    println!("Organization '{name}' deleted.");
    Ok(())
}

fn format_timestamp(ts: u64) -> String {
    if ts == 0 {
        return "-".to_string();
    }
    // Simple ISO-ish format without pulling in chrono
    let secs = ts;
    let days = secs / 86400;
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let mins = (remaining % 3600) / 60;

    // Approximate date from epoch days (good enough for display)
    let (year, month, day) = epoch_days_to_date(days);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{mins:02}")
}

fn epoch_days_to_date(days: u64) -> (u64, u64, u64) {
    // Civil date from epoch days (Algorithms by Howard Hinnant)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
}
