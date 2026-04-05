/// How a resource is scoped in the hierarchy.
#[derive(Debug, Clone)]
pub struct ScopeDef {
    /// Parent resources this resource belongs to.
    pub parents: Vec<ParentRef>,
    /// How name uniqueness is enforced.
    pub uniqueness: UniquenessScope,
}

/// A reference to a parent resource that scopes this one.
#[derive(Debug, Clone)]
pub struct ParentRef {
    /// Parent resource kind, e.g. "vpc", "org"
    pub kind: &'static str,
    /// CLI flag name, e.g. "--vpc", "--org"
    pub flag: &'static str,
    /// Required when creating this resource?
    pub required_on_create: bool,
    /// Required when getting/deleting? (false = auto-resolve if unambiguous)
    pub required_on_resolve: bool,
    /// Help text for this parent flag
    pub description: &'static str,
}

/// Name uniqueness scope.
#[derive(Debug, Clone)]
pub enum UniquenessScope {
    /// Name is globally unique (e.g. org names)
    Global,
    /// Name is unique within a single parent (e.g. subnet within vpc)
    WithinParent(&'static str),
    /// Name is unique within a combination of parents
    WithinComposite(&'static [&'static str]),
}

impl ScopeDef {
    /// Global scope — no parents, globally unique names.
    pub fn global() -> Self {
        Self {
            parents: Vec::new(),
            uniqueness: UniquenessScope::Global,
        }
    }

    /// Scoped to a single parent resource.
    pub fn within(parent_kind: &'static str, flag: &'static str, desc: &'static str) -> Self {
        Self {
            parents: vec![ParentRef {
                kind: parent_kind,
                flag,
                required_on_create: true,
                required_on_resolve: false,
                description: desc,
            }],
            uniqueness: UniquenessScope::WithinParent(parent_kind),
        }
    }
}
