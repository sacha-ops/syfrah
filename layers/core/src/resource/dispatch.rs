use clap::ArgMatches;
use std::collections::HashMap;
use std::io::{self, Write};

use super::cli_gen::{render_detail, render_table};
use super::constraint::FieldMap;
use super::operation::OutputKind;
use super::registry::{
    OperationRequest, OperationResponse, ResourceRegistration, ScopeValues, ValidatedRequest,
};
use super::ResourceDef;

/// Dispatch a parsed CLI invocation to the appropriate resource handler.
///
/// Pipeline:
/// 1. Extract raw values from clap matches
/// 2. Validate constraints → produce ValidatedRequest
/// 3. Handle confirmation prompt
/// 4. Call handler
/// 5. Render output
pub async fn dispatch(
    reg: &ResourceRegistration,
    op_name: &str,
    matches: &ArgMatches,
) -> anyhow::Result<()> {
    let def = &reg.def;
    let op = def
        .operations
        .iter()
        .find(|o| o.name == op_name)
        .ok_or_else(|| anyhow::anyhow!("unknown operation: {op_name}"))?;

    // ── 1. Extract raw values ──

    let name = matches
        .try_get_one::<String>("name")
        .ok()
        .flatten()
        .cloned();
    let json = matches
        .try_get_one::<bool>("json")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);
    let yes = matches
        .try_get_one::<bool>("yes")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);

    let scope = extract_scope(def, matches);
    let mut fields = extract_fields(def, op, matches);

    // ── 1b. Interactive prompts for missing required args ──
    //
    // If stdin is a TTY and required args are missing, prompt interactively
    // instead of returning an error. Non-TTY (pipe/script) still fails fast.

    if crate::ui::prompt::is_interactive() && !json {
        // Check operation-specific args
        for arg in &op.args {
            if arg.required && !fields.contains_key(arg.name) {
                if let super::operation::ArgSource::Custom(ref field_def) = arg.source {
                    if let Ok(Some(val)) = crate::ui::prompt::prompt_field(
                        arg.name,
                        arg.description,
                        &field_def.field_type,
                        field_def.default,
                    ) {
                        fields.insert(arg.name.to_string(), val);
                    }
                }
            }
        }

        // Check schema fields that are required for create
        if matches!(op.semantics, super::operation::OperationSemantics::Create) {
            for field in &def.schema.fields {
                if field.mutability != crate::resource::Mutability::ReadOnly
                    && field.mutability != crate::resource::Mutability::Internal
                    && field.default.is_none()
                    && !fields.contains_key(field.name)
                {
                    if let Ok(Some(val)) = crate::ui::prompt::prompt_field(
                        field.name,
                        field.description,
                        &field.field_type,
                        field.default,
                    ) {
                        fields.insert(field.name.to_string(), val);
                    }
                }
            }
        }
    }

    // ── 2. Validate constraints → ValidatedRequest ──

    for constraint in &op.constraints {
        constraint
            .validate(&fields)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    let raw_request = OperationRequest {
        operation: op_name.to_string(),
        name,
        scope,
        fields,
    };

    let validated = ValidatedRequest::from_raw(def.identity.kind, raw_request.clone());

    // ── 3. Confirmation prompt ──

    if op.confirmable && !yes {
        let resource_name = raw_request.name.as_deref().unwrap_or("this resource");
        let prompt = format!(
            "{} {} '{}'? This cannot be undone. [y/N] ",
            capitalize(op.name),
            def.identity.kind,
            resource_name
        );
        print!("{prompt}");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    // ── 4. Call handler with validated request ──

    let _ = &validated; // available for future use
    let response = (reg.handler)(raw_request).await?;

    // ── 5. Render output ──

    let force_json = json || matches!(op.output.kind, OutputKind::JsonOnly);

    match (&op.output.kind, force_json) {
        (_, true) => match &response {
            OperationResponse::Resource(v) => {
                println!("{}", serde_json::to_string_pretty(v)?);
            }
            OperationResponse::ResourceList(items) => {
                println!("{}", serde_json::to_string_pretty(items)?);
            }
            OperationResponse::Message(msg) => {
                println!("{}", serde_json::json!({"message": msg}));
            }
            OperationResponse::None => {}
        },
        (OutputKind::Resource, false) => {
            if let OperationResponse::Resource(v) = &response {
                render_detail(def, v);
            }
            if let Some(tpl) = op.output.success_message {
                let name_val: String = match &response {
                    OperationResponse::Resource(v) => extract_name(v).unwrap_or_default(),
                    _ => String::new(),
                };
                let rendered = tpl
                    .replace("{kind}", def.identity.kind)
                    .replace("{name}", &name_val);
                println!("{rendered}");
            }
        }
        (OutputKind::ResourceList, false) => {
            if let OperationResponse::ResourceList(items) = &response {
                render_table(def, items);
            }
        }
        (OutputKind::Message, false) => {
            if let OperationResponse::Message(msg) = &response {
                println!("{msg}");
            } else if let Some(tpl) = op.output.success_message {
                let rendered: String = tpl.replace("{kind}", def.identity.kind);
                println!("{rendered}");
            }
        }
        (OutputKind::None, false) | (OutputKind::JsonOnly, false) => {}
    }

    Ok(())
}

fn extract_scope(def: &ResourceDef, matches: &ArgMatches) -> ScopeValues {
    let mut scope = ScopeValues::default();
    for parent in &def.scope.parents {
        if let Some(val) = matches.get_one::<String>(parent.kind) {
            scope.set(parent.kind, val.clone());
        }
    }
    scope
}

fn extract_fields(
    def: &ResourceDef,
    op: &super::operation::OperationDef,
    matches: &ArgMatches,
) -> FieldMap {
    let mut fields = HashMap::new();

    // Schema fields
    for field in &def.schema.fields {
        if let Some(val) = matches.try_get_one::<String>(field.name).ok().flatten() {
            fields.insert(field.name.to_string(), val.clone());
        }
        if matches!(field.field_type, super::schema::FieldType::Flag)
            && matches
                .try_get_one::<bool>(field.name)
                .ok()
                .flatten()
                .copied()
                .unwrap_or(false)
        {
            fields.insert(field.name.to_string(), "true".to_string());
        }
    }

    // Operation-specific args
    for arg in &op.args {
        if let super::operation::ArgSource::Custom(field) = &arg.source {
            if let Some(val) = matches.try_get_one::<String>(arg.name).ok().flatten() {
                fields.insert(arg.name.to_string(), val.clone());
            }
            if matches!(field.field_type, super::schema::FieldType::Flag)
                && matches
                    .try_get_one::<bool>(arg.name)
                    .ok()
                    .flatten()
                    .copied()
                    .unwrap_or(false)
            {
                fields.insert(arg.name.to_string(), "true".to_string());
            }
        }
    }

    fields
}

fn extract_name(value: &serde_json::Value) -> Option<String> {
    value
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}
