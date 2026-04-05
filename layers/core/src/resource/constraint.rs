use std::collections::HashMap;

/// A value map from field names to their string values.
pub type FieldMap = HashMap<String, String>;

/// Cross-field validation constraint.
#[derive(Debug, Clone)]
pub enum Constraint {
    /// If `if_field` is present (optionally with a specific value),
    /// then `then_field` must also be present.
    Requires {
        if_field: &'static str,
        if_value: Option<&'static str>,
        then_field: &'static str,
        message: &'static str,
    },
    /// If `if_field` is present (optionally with a specific value),
    /// then `then_field` must NOT be present.
    Forbids {
        if_field: &'static str,
        if_value: Option<&'static str>,
        then_field: &'static str,
        message: &'static str,
    },
    /// Fields `a` and `b` cannot both be present.
    Conflicts {
        a: &'static str,
        b: &'static str,
        message: &'static str,
    },
    /// Exactly one of these fields must be present.
    OneOf {
        fields: &'static [&'static str],
        message: &'static str,
    },
    /// At least one of these fields must be present.
    AtLeastOne {
        fields: &'static [&'static str],
        message: &'static str,
    },
    /// Arbitrary validation function.
    Custom {
        name: &'static str,
        validate: fn(&FieldMap) -> Result<(), String>,
    },
}

impl Constraint {
    /// Validate a field map against this constraint.
    pub fn validate(&self, fields: &FieldMap) -> Result<(), String> {
        match self {
            Constraint::Requires {
                if_field,
                if_value,
                then_field,
                message,
            } => {
                let present = fields.get(*if_field);
                let triggered = match (present, if_value) {
                    (Some(v), Some(expected)) => v == expected,
                    (Some(_), None) => true,
                    _ => false,
                };
                if triggered && !fields.contains_key(*then_field) {
                    return Err(message.to_string());
                }
                Ok(())
            }
            Constraint::Forbids {
                if_field,
                if_value,
                then_field,
                message,
            } => {
                let present = fields.get(*if_field);
                let triggered = match (present, if_value) {
                    (Some(v), Some(expected)) => v == expected,
                    (Some(_), None) => true,
                    _ => false,
                };
                if triggered && fields.contains_key(*then_field) {
                    return Err(message.to_string());
                }
                Ok(())
            }
            Constraint::Conflicts { a, b, message } => {
                if fields.contains_key(*a) && fields.contains_key(*b) {
                    return Err(message.to_string());
                }
                Ok(())
            }
            Constraint::OneOf {
                fields: names,
                message,
            } => {
                let count = names.iter().filter(|n| fields.contains_key(**n)).count();
                if count != 1 {
                    return Err(message.to_string());
                }
                Ok(())
            }
            Constraint::AtLeastOne {
                fields: names,
                message,
            } => {
                let any = names.iter().any(|n| fields.contains_key(*n));
                if !any {
                    return Err(message.to_string());
                }
                Ok(())
            }
            Constraint::Custom { validate, .. } => validate(fields),
        }
    }
}
