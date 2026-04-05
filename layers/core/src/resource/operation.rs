use super::constraint::Constraint;
use super::schema::FieldDef;

/// An operation that can be performed on a resource.
#[derive(Debug, Clone)]
pub struct OperationDef {
    /// Operation name: "create", "list", "get", "delete", "peer", "drain", etc.
    pub name: &'static str,
    /// Help text
    pub description: &'static str,
    /// What kind of operation this is (affects automatic behavior)
    pub semantics: OperationSemantics,
    /// Arguments specific to this operation
    pub args: Vec<OperationArg>,
    /// Cross-field validation constraints
    pub constraints: Vec<Constraint>,
    /// Requires confirmation? (automatically adds --yes/-y)
    pub confirmable: bool,
    /// Output behavior
    pub output: OutputDef,
    /// Example invocations shown in --help
    pub examples: Vec<&'static str>,
}

/// The semantic category of an operation.
/// Affects automatic CLI generation behavior.
#[derive(Debug, Clone, PartialEq)]
pub enum OperationSemantics {
    /// Create a new resource. Auto: positional <NAME>, scope flags, create_fields.
    Create,
    /// Get a single resource by name/id. Auto: positional <NAME>, --json.
    Get,
    /// List resources. Auto: --json, scope filters.
    List,
    /// Delete a resource. Auto: positional <NAME>, --yes, confirmation prompt.
    Delete,
    /// Update/patch a resource. patch=true: partial update.
    Update { patch: bool },
    /// Custom action (peer, drain, attach, resize, rotate, etc.)
    Action,
}

/// An argument to an operation.
#[derive(Debug, Clone)]
pub struct OperationArg {
    /// Argument name (used as --flag or positional)
    pub name: &'static str,
    /// Help text
    pub description: &'static str,
    /// Is this argument required?
    pub required: bool,
    /// Where does the value come from?
    pub source: ArgSource,
}

/// Where an operation argument's definition comes from.
#[derive(Debug, Clone)]
pub enum ArgSource {
    /// Reuse a field from the resource schema (by name)
    FromSchema(&'static str),
    /// Custom field definition for this operation only
    Custom(FieldDef),
}

/// What the operation outputs.
#[derive(Debug, Clone)]
pub struct OutputDef {
    pub kind: OutputKind,
    /// Success message template. Supports {kind} and {name} placeholders.
    pub success_message: Option<&'static str>,
}

/// Output type — determines rendering behavior.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputKind {
    /// No output (silent success)
    None,
    /// A single resource (rendered as detail or JSON)
    Resource,
    /// A list of resources (rendered as table or JSON)
    ResourceList,
    /// A simple text message
    Message,
    /// JSON only — no table/detail rendering, always output JSON
    JsonOnly,
}

/// Typed filter for list operations.
#[derive(Debug, Clone)]
pub struct FilterDef {
    /// Filter name (used as --flag)
    pub name: &'static str,
    /// Field type for validation
    pub field_type: super::schema::FieldType,
    /// Help text
    pub description: &'static str,
}

// ── Convenience constructors ───────────────────────────────

impl OperationDef {
    /// Standard CRUD: create
    pub fn create() -> Self {
        Self {
            name: "create",
            description: "Create a new resource",
            semantics: OperationSemantics::Create,
            args: Vec::new(),
            constraints: Vec::new(),
            confirmable: false,
            output: OutputDef {
                kind: OutputKind::Resource,
                success_message: Some("{kind} '{name}' created."),
            },
            examples: Vec::new(),
        }
    }

    /// Standard CRUD: get
    pub fn get() -> Self {
        Self {
            name: "get",
            description: "Get resource details",
            semantics: OperationSemantics::Get,
            args: Vec::new(),
            constraints: Vec::new(),
            confirmable: false,
            output: OutputDef {
                kind: OutputKind::Resource,
                success_message: None,
            },
            examples: Vec::new(),
        }
    }

    /// Standard CRUD: list
    pub fn list() -> Self {
        Self {
            name: "list",
            description: "List resources",
            semantics: OperationSemantics::List,
            args: Vec::new(),
            constraints: Vec::new(),
            confirmable: false,
            output: OutputDef {
                kind: OutputKind::ResourceList,
                success_message: None,
            },
            examples: Vec::new(),
        }
    }

    /// Standard CRUD: delete
    pub fn delete() -> Self {
        Self {
            name: "delete",
            description: "Delete a resource",
            semantics: OperationSemantics::Delete,
            args: Vec::new(),
            constraints: Vec::new(),
            confirmable: true,
            output: OutputDef {
                kind: OutputKind::Message,
                success_message: Some("{kind} '{name}' deleted."),
            },
            examples: Vec::new(),
        }
    }

    /// Custom action
    pub fn action(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            semantics: OperationSemantics::Action,
            args: Vec::new(),
            constraints: Vec::new(),
            confirmable: false,
            output: OutputDef {
                kind: OutputKind::Message,
                success_message: None,
            },
            examples: Vec::new(),
        }
    }

    // ── Builder methods ──

    pub fn with_arg(mut self, arg: OperationArg) -> Self {
        self.args.push(arg);
        self
    }

    pub fn with_constraint(mut self, c: Constraint) -> Self {
        self.constraints.push(c);
        self
    }

    pub fn with_confirm(mut self) -> Self {
        self.confirmable = true;
        self
    }

    pub fn with_example(mut self, ex: &'static str) -> Self {
        self.examples.push(ex);
        self
    }

    pub fn with_output(mut self, kind: OutputKind) -> Self {
        self.output.kind = kind;
        self
    }

    pub fn with_success_message(mut self, msg: &'static str) -> Self {
        self.output.success_message = Some(msg);
        self
    }
}

impl OperationArg {
    pub fn required(name: &'static str, field: FieldDef) -> Self {
        Self {
            name,
            description: field.description,
            required: true,
            source: ArgSource::Custom(field),
        }
    }

    pub fn optional(name: &'static str, field: FieldDef) -> Self {
        Self {
            name,
            description: field.description,
            required: false,
            source: ArgSource::Custom(field),
        }
    }

    pub fn from_schema(field_name: &'static str) -> Self {
        Self {
            name: field_name,
            description: "",
            required: false,
            source: ArgSource::FromSchema(field_name),
        }
    }
}
