use clap::{Arg, ArgAction, Command};

use super::operation::OperationSemantics;
use super::presentation::ColumnWidth;
use super::schema::{CliVisibility, FieldDef, FieldType, Mutability};
use super::ResourceDef;

/// Generate a complete clap [`Command`] from a [`ResourceDef`].
///
/// This is the core of the framework: one function that turns a declarative
/// resource definition into a fully-featured CLI subcommand with consistent
/// flags, help text, and validation — automatically.
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
    let desc = op.description;
    let mut cmd = Command::new(op.name).about(desc);

    // ── Standard args based on semantics ──

    match &op.semantics {
        OperationSemantics::Create => {
            // Positional <NAME> — always first
            cmd = cmd.arg(
                Arg::new("name")
                    .help(format!(
                        "{} name (lowercase alphanumeric and hyphens, 3-63 chars)",
                        def.identity.kind
                    ))
                    .required(true),
            );

            // Scope flags from parents
            cmd = add_scope_args(cmd, def, true);

            // Create-eligible fields from schema
            for field in &def.schema.fields {
                if field.mutability == Mutability::ReadOnly
                    || field.mutability == Mutability::Internal
                {
                    continue;
                }
                cmd = cmd.arg(field_to_arg(field, true));
            }

            // Operation-specific extra args
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

            // Only mutable fields
            for field in &def.schema.fields {
                if field.mutability == Mutability::Mutable {
                    cmd = cmd.arg(field_to_arg(field, false));
                }
            }
        }

        OperationSemantics::Action => {
            // Custom action args
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

    // ── Confirmation flag ──
    if op.confirmable && !matches!(op.semantics, OperationSemantics::Delete) {
        cmd = cmd.arg(yes_flag());
    }

    // ── Examples ──
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

/// Add scope flags (--org, --vpc, etc.) from the resource's parent refs.
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

/// Convert a FieldDef into a clap Arg.
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
        // env var support requires clap "env" feature — skip for now
        let _ = env;
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

/// The standard --json flag.
fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Output as JSON")
}

/// The standard --yes/-y flag.
fn yes_flag() -> Arg {
    Arg::new("yes")
        .long("yes")
        .short('y')
        .action(ArgAction::SetTrue)
        .help("Skip confirmation prompt")
}

/// Render a table from JSON values using the resource's presentation definition.
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
        let msg = table_def
            .empty_message
            .unwrap_or("No resources found.");
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
                .map(|item| extract_field(item, col.field).len())
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
        .map(|(col, &w)| format!("{:<width$}", col.header, width = w))
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
                let val = extract_field(item, col.field);
                format!("{:<width$}", val, width = w)
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
            let val = extract_field(item, field.field);
            println!("  {:<16} {}", format!("{}:", field.label), val);
        }
    }
}

fn extract_field(value: &serde_json::Value, field: &str) -> String {
    match value.get(field) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::Bool(b)) => if *b { "yes" } else { "no" }.to_string(),
        Some(serde_json::Value::Null) => "-".to_string(),
        Some(other) => other.to_string(),
        None => "-".to_string(),
    }
}
