use clap::{Arg, ArgAction, Command};

use super::operation::OperationSemantics;
use super::presentation::{Align, ColumnWidth, DisplayFormat};
use super::schema::{CliVisibility, FieldDef, FieldType, Mutability};
use super::ResourceDef;

/// Generate a complete clap [`Command`] from a [`ResourceDef`].
pub fn generate_command(def: &ResourceDef) -> Command {
    let mut cmd = Command::new(def.identity.cli_name).about(def.identity.description);

    for alias in def.identity.aliases {
        cmd = cmd.visible_alias(alias);
    }

    for op in &def.operations {
        cmd = cmd.subcommand(generate_operation(def, op));
    }

    cmd
}

/// Generate a clap Command for a single operation.
fn generate_operation(def: &ResourceDef, op: &super::operation::OperationDef) -> Command {
    let mut cmd = Command::new(op.name).about(op.description);

    match &op.semantics {
        OperationSemantics::Create => {
            cmd = cmd.arg(
                Arg::new("name")
                    .help(format!(
                        "{} name (lowercase alphanumeric and hyphens, 3-63 chars)",
                        def.identity.kind
                    ))
                    .required(true),
            );
            cmd = add_scope_args(cmd, def, true);

            for field in &def.schema.fields {
                if field.mutability == Mutability::ReadOnly
                    || field.mutability == Mutability::Internal
                {
                    continue;
                }
                cmd = cmd.arg(field_to_arg(field, true));
            }

            for arg in &op.args {
                if let super::operation::ArgSource::Custom(field) = &arg.source {
                    cmd = cmd.arg(field_to_arg(field, arg.required));
                }
            }
        }

        OperationSemantics::Get => {
            cmd = cmd
                .arg(Arg::new("name").help("Resource name or ID").required(true))
                .arg(json_flag());
            cmd = add_scope_args(cmd, def, false);
        }

        OperationSemantics::List => {
            cmd = cmd.arg(json_flag());
            cmd = add_scope_args(cmd, def, false);

            // Add typed filters from the operation args
            for arg in &op.args {
                match &arg.source {
                    super::operation::ArgSource::Custom(field) => {
                        cmd = cmd.arg(field_to_arg(field, arg.required));
                    }
                    super::operation::ArgSource::FromSchema(name) => {
                        if let Some(field) = def.schema.fields.iter().find(|f| f.name == *name) {
                            cmd = cmd.arg(field_to_arg(field, arg.required));
                        }
                    }
                }
            }
        }

        OperationSemantics::Delete => {
            cmd = cmd
                .arg(Arg::new("name").help("Resource name or ID").required(true))
                .arg(yes_flag());
            cmd = add_scope_args(cmd, def, false);
        }

        OperationSemantics::Update { .. } => {
            cmd = cmd.arg(Arg::new("name").help("Resource name or ID").required(true));
            cmd = add_scope_args(cmd, def, false);

            for field in &def.schema.fields {
                if field.mutability == Mutability::Mutable {
                    cmd = cmd.arg(field_to_arg(field, false));
                }
            }

            for arg in &op.args {
                if let super::operation::ArgSource::Custom(field) = &arg.source {
                    cmd = cmd.arg(field_to_arg(field, arg.required));
                }
            }
        }

        OperationSemantics::Action => {
            for arg in &op.args {
                match &arg.source {
                    super::operation::ArgSource::Custom(field) => {
                        cmd = cmd.arg(field_to_arg(field, arg.required));
                    }
                    super::operation::ArgSource::FromSchema(name) => {
                        if let Some(field) = def.schema.fields.iter().find(|f| f.name == *name) {
                            cmd = cmd.arg(field_to_arg(field, arg.required));
                        }
                    }
                }
            }
        }
    }

    // Confirmation flag (delete already adds it above)
    if op.confirmable && !matches!(op.semantics, OperationSemantics::Delete) {
        cmd = cmd.arg(yes_flag());
    }

    // Examples in after_help
    if !op.examples.is_empty() {
        let examples = op
            .examples
            .iter()
            .map(|e| format!("  {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        cmd = cmd.after_help(format!("Examples:\n{examples}"));
    }

    cmd
}

fn add_scope_args(mut cmd: Command, def: &ResourceDef, for_create: bool) -> Command {
    for parent in &def.scope.parents {
        let required = if for_create {
            parent.required_on_create
        } else {
            parent.required_on_resolve
        };

        let mut arg = Arg::new(parent.kind)
            .long(parent.flag.trim_start_matches('-'))
            .help(parent.description);

        if required {
            arg = arg.required(true);
        }

        cmd = cmd.arg(arg);
    }
    cmd
}

fn field_to_arg(field: &FieldDef, required: bool) -> Arg {
    let mut arg = Arg::new(field.name)
        .long(field.name)
        .help(field.description);

    if let Some(short) = field.short {
        arg = arg.short(short);
    }

    if let Some(default) = field.default {
        arg = arg.default_value(default);
    }

    if let Some(env) = field.env_var {
        arg = arg.env(env);
    }

    match &field.field_type {
        FieldType::Flag => {
            arg = arg.action(ArgAction::SetTrue);
        }
        FieldType::Enum(e) => {
            arg = arg.value_parser(
                e.values
                    .iter()
                    .map(|s| clap::builder::PossibleValue::new(*s))
                    .collect::<Vec<_>>(),
            );
            if let Some(default) = e.default {
                arg = arg.default_value(default);
            }
        }
        FieldType::KeyValue => {
            arg = arg.action(ArgAction::Append);
        }
        _ => {
            if required && field.default.is_none() {
                arg = arg.required(true);
            }
        }
    }

    match field.visibility {
        CliVisibility::Hidden => {
            arg = arg.hide(true);
        }
        CliVisibility::Advanced => {
            arg = arg.hide_short_help(true);
        }
        CliVisibility::Normal => {}
    }

    arg
}

fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Output as JSON")
}

fn yes_flag() -> Arg {
    Arg::new("yes")
        .long("yes")
        .short('y')
        .action(ArgAction::SetTrue)
        .help("Skip confirmation prompt")
}

// ═══════════════════════════════════════════════════
// Rendering
// ═══════════════════════════════════════════════════

/// Render a table from JSON values using the resource's table definition.
pub fn render_table(def: &ResourceDef, items: &[serde_json::Value]) {
    let table_def = match &def.presentation.table {
        Some(t) => t,
        None => {
            for item in items {
                println!("{}", serde_json::to_string_pretty(item).unwrap_or_default());
            }
            return;
        }
    };

    if items.is_empty() {
        let msg = table_def.empty_message.unwrap_or("No resources found.");
        println!("{msg}");
        return;
    }

    // Calculate column widths
    let widths: Vec<usize> = table_def
        .columns
        .iter()
        .map(|col| {
            let header_len = col.header.len();
            let max_data_len = items
                .iter()
                .map(|item| format_value(item, col.field, &col.format).len())
                .max()
                .unwrap_or(0);
            let content_width = header_len.max(max_data_len);

            match col.width {
                ColumnWidth::Auto => content_width,
                ColumnWidth::Fixed(w) => w,
                ColumnWidth::Min(m) => content_width.max(m),
                ColumnWidth::Max(m) => content_width.min(m),
            }
        })
        .collect();

    // Print header
    let header: String = table_def
        .columns
        .iter()
        .zip(&widths)
        .map(|(col, &w)| match col.align {
            Align::Left => format!("{:<width$}", col.header, width = w),
            Align::Right => format!("{:>width$}", col.header, width = w),
        })
        .collect::<Vec<_>>()
        .join("  ");
    println!("{header}");
    println!("{}", "-".repeat(header.len()));

    // Print rows
    for item in items {
        let row: String = table_def
            .columns
            .iter()
            .zip(&widths)
            .map(|(col, &w)| {
                let val = format_value(item, col.field, &col.format);
                match col.align {
                    Align::Left => format!("{:<width$}", val, width = w),
                    Align::Right => format!("{:>width$}", val, width = w),
                }
            })
            .collect::<Vec<_>>()
            .join("  ");
        println!("{row}");
    }
}

/// Render a detail view from a JSON value.
pub fn render_detail(def: &ResourceDef, item: &serde_json::Value) {
    let detail_def = match &def.presentation.detail {
        Some(d) => d,
        None => {
            println!("{}", serde_json::to_string_pretty(item).unwrap_or_default());
            return;
        }
    };

    for section in &detail_def.sections {
        if let Some(title) = section.title {
            println!("\n{title}");
            println!("{}", "=".repeat(title.len()));
        }
        for field in &section.fields {
            let val = format_value(item, field.field, &field.format);
            println!("  {:<16} {}", format!("{}:", field.label), val);
        }
    }
}

/// Format a JSON value according to a DisplayFormat.
pub fn format_value(value: &serde_json::Value, field: &str, format: &DisplayFormat) -> String {
    let raw = match value.get(field) {
        Some(v) => v,
        None => return "-".to_string(),
    };

    match format {
        DisplayFormat::Plain => match raw {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => "-".to_string(),
            other => other.to_string(),
        },

        DisplayFormat::YesNo => match raw {
            serde_json::Value::Bool(b) => if *b { "yes" } else { "no" }.to_string(),
            serde_json::Value::String(s) => {
                if s == "true" || s == "yes" {
                    "yes".to_string()
                } else {
                    "no".to_string()
                }
            }
            _ => "-".to_string(),
        },

        DisplayFormat::Bytes => {
            let bytes = raw.as_u64().or_else(|| raw.as_f64().map(|f| f as u64));
            match bytes {
                Some(b) if b >= 1_073_741_824 => format!("{:.1} GiB", b as f64 / 1_073_741_824.0),
                Some(b) if b >= 1_048_576 => format!("{:.1} MiB", b as f64 / 1_048_576.0),
                Some(b) if b >= 1024 => format!("{:.1} KiB", b as f64 / 1024.0),
                Some(b) => format!("{b} B"),
                None => raw.to_string(),
            }
        }

        DisplayFormat::Duration => {
            let secs = raw.as_u64().or_else(|| raw.as_f64().map(|f| f as u64));
            match secs {
                Some(s) if s >= 86400 => format!("{}d {}h", s / 86400, (s % 86400) / 3600),
                Some(s) if s >= 3600 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
                Some(s) if s >= 60 => format!("{}m {}s", s / 60, s % 60),
                Some(s) => format!("{s}s"),
                None => raw.to_string(),
            }
        }

        DisplayFormat::Timestamp => {
            let ts = raw.as_u64().or_else(|| raw.as_f64().map(|f| f as u64));
            match ts {
                Some(epoch) => {
                    // Simple UTC formatting without chrono dependency
                    // Days since epoch → approximate date
                    let secs = epoch;
                    let days = secs / 86400;
                    let remaining = secs % 86400;
                    let hours = remaining / 3600;
                    let minutes = (remaining % 3600) / 60;

                    // Approximate year/month/day from days since epoch
                    let (year, month, day) = days_to_date(days);
                    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02} UTC")
                }
                None => match raw.as_str() {
                    Some(s) => s.to_string(),
                    None => raw.to_string(),
                },
            }
        }

        DisplayFormat::Status => match raw.as_str() {
            Some(s) => s.to_string(),
            _ => raw.to_string(),
        },

        DisplayFormat::Masked => match raw.as_str() {
            Some(s) if s.len() > 4 => {
                let suffix = &s[s.len() - 4..];
                format!("****...{suffix}")
            }
            Some(_) => "****".to_string(),
            _ => "****".to_string(),
        },
    }
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Simplified algorithm — accurate for 1970-2099
    let mut y = 1970;
    let mut remaining = days;

    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }

    let months: [u64; 12] = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut m = 0;
    for (i, &days_in_month) in months.iter().enumerate() {
        if remaining < days_in_month {
            m = i as u64 + 1;
            break;
        }
        remaining -= days_in_month;
    }
    if m == 0 {
        m = 12;
    }

    (y, m, remaining + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}
