//! Interactive prompts for missing CLI arguments.

use dialoguer::{Confirm, Input, Select};

use crate::resource::FieldType;

type PromptResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Prompt for a string value with optional default.
pub fn prompt_string(label: &str, default: Option<&str>) -> PromptResult<String> {
    let mut input: Input<String> = Input::new().with_prompt(format!("  {label}"));
    if let Some(d) = default {
        input = input.default(d.to_string());
    }
    Ok(input.interact_text()?)
}

/// Prompt for an integer value with optional default.
pub fn prompt_integer(label: &str, default: Option<&str>) -> PromptResult<String> {
    let input: Input<String> = Input::new()
        .with_prompt(format!("  {label}"))
        .default(default.unwrap_or("0").to_string());
    Ok(input.interact_text()?)
}

/// Prompt for a yes/no flag.
pub fn prompt_flag(label: &str, default: bool) -> PromptResult<bool> {
    Ok(Confirm::new()
        .with_prompt(format!("  {label}"))
        .default(default)
        .interact()?)
}

/// Prompt for a selection from a list of options.
pub fn prompt_enum(label: &str, options: &[&str], default: Option<usize>) -> PromptResult<String> {
    let mut select = Select::new().with_prompt(format!("  {label}"));
    for opt in options {
        select = select.item(opt);
    }
    if let Some(d) = default {
        select = select.default(d);
    }
    let idx = select.interact()?;
    Ok(options[idx].to_string())
}

/// Prompt for a value based on its FieldType.
pub fn prompt_field(
    name: &str,
    _description: &str,
    field_type: &FieldType,
    default: Option<&str>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match field_type {
        FieldType::String
        | FieldType::Cidr
        | FieldType::IpAddr
        | FieldType::Path
        | FieldType::Duration
        | FieldType::KeyValue
        | FieldType::ResourceRef(_) => {
            let val = prompt_string(name, default)?;
            if val.is_empty() {
                Ok(None)
            } else {
                Ok(Some(val))
            }
        }
        FieldType::Integer | FieldType::Port | FieldType::SizeGb | FieldType::SizeMb => {
            let val = prompt_integer(name, default)?;
            Ok(Some(val))
        }
        FieldType::Flag => {
            let val = prompt_flag(name, false)?;
            if val {
                Ok(Some("true".to_string()))
            } else {
                Ok(None)
            }
        }
        FieldType::Enum(e) => {
            let default_idx = e
                .default
                .and_then(|d| e.values.iter().position(|v| *v == d));
            let val = prompt_enum(name, e.values, default_idx)?;
            Ok(Some(val))
        }
    }
}

/// Check if stdin is a TTY (interactive terminal).
pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_interactive_no_panic() {
        let _ = is_interactive();
    }
}
