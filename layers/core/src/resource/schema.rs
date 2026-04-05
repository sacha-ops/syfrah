/// Schema — the fields a resource has.
#[derive(Debug, Clone)]
pub struct ResourceSchema {
    pub fields: Vec<FieldDef>,
}

/// A single field definition.
#[derive(Debug, Clone)]
pub struct FieldDef {
    /// Field name (used as CLI flag: --name)
    pub name: &'static str,
    /// Help text
    pub description: &'static str,
    /// Type determines validation and clap arg type
    pub field_type: FieldType,
    /// When can this field be set?
    pub mutability: Mutability,
    /// Short flag, e.g. 'n' for -n
    pub short: Option<char>,
    /// Default value (displayed in help)
    pub default: Option<&'static str>,
    /// Environment variable override
    pub env_var: Option<&'static str>,
    /// Visibility in --help
    pub visibility: CliVisibility,
}

/// Field data type — determines parsing, validation, and clap arg config.
#[derive(Debug, Clone)]
pub enum FieldType {
    String,
    Cidr,
    Port,
    SizeGb,
    SizeMb,
    Duration,
    KeyValue,
    Flag,
    Enum(EnumDef),
    Path,
    IpAddr,
    Integer,
    /// Reference to another resource, resolved by name or ID.
    ResourceRef(RefDef),
}

/// Enum field with allowed values.
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub values: &'static [&'static str],
    pub default: Option<&'static str>,
}

/// Reference to another resource.
#[derive(Debug, Clone)]
pub struct RefDef {
    pub target_kind: &'static str,
    pub resolution: RefResolution,
}

/// How a resource reference is resolved.
#[derive(Debug, Clone)]
pub enum RefResolution {
    ByName,
    ById,
    ByNameOrId,
}

/// When can a field be set or changed?
#[derive(Debug, Clone, PartialEq)]
pub enum Mutability {
    /// Set at creation, immutable after (e.g. cidr)
    CreateOnly,
    /// Can be updated via patch (e.g. description, labels)
    Mutable,
    /// Computed by the system, never set by user (e.g. id, created_at)
    ReadOnly,
    /// Internal field, never exposed in CLI
    Internal,
}

/// Controls visibility in CLI help output.
#[derive(Debug, Clone, PartialEq)]
pub enum CliVisibility {
    /// Shown in default --help
    Normal,
    /// Only shown in --help (verbose), hidden from -h (short)
    Advanced,
    /// Never shown in help
    Hidden,
}

impl FieldDef {
    pub fn string(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            field_type: FieldType::String,
            mutability: Mutability::CreateOnly,
            short: None,
            default: None,
            env_var: None,
            visibility: CliVisibility::Normal,
        }
    }

    pub fn cidr(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            field_type: FieldType::Cidr,
            ..Self::string(name, description)
        }
    }

    pub fn flag(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            field_type: FieldType::Flag,
            ..Self::string(name, description)
        }
    }

    pub fn size_gb(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            field_type: FieldType::SizeGb,
            ..Self::string(name, description)
        }
    }

    pub fn integer(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            field_type: FieldType::Integer,
            ..Self::string(name, description)
        }
    }

    pub fn resource_ref(
        name: &'static str,
        description: &'static str,
        target_kind: &'static str,
    ) -> Self {
        Self {
            name,
            description,
            field_type: FieldType::ResourceRef(RefDef {
                target_kind,
                resolution: RefResolution::ByNameOrId,
            }),
            ..Self::string(name, description)
        }
    }

    pub fn enum_field(
        name: &'static str,
        description: &'static str,
        values: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            description,
            field_type: FieldType::Enum(EnumDef {
                values,
                default: None,
            }),
            ..Self::string(name, description)
        }
    }

    pub fn mutable(mut self) -> Self {
        self.mutability = Mutability::Mutable;
        self
    }

    pub fn with_default(mut self, default: &'static str) -> Self {
        self.default = Some(default);
        self
    }

    pub fn with_short(mut self, short: char) -> Self {
        self.short = Some(short);
        self
    }

    pub fn advanced(mut self) -> Self {
        self.visibility = CliVisibility::Advanced;
        self
    }

    pub fn hidden(mut self) -> Self {
        self.visibility = CliVisibility::Hidden;
        self
    }

    pub fn with_env(mut self, var: &'static str) -> Self {
        self.env_var = Some(var);
        self
    }
}

impl ResourceSchema {
    pub fn new() -> Self {
        Self { fields: Vec::new() }
    }
}

impl Default for ResourceSchema {
    fn default() -> Self {
        Self::new()
    }
}
