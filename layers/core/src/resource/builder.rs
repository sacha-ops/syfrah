//! Fluent builder API for constructing ResourceDefs ergonomically.

use super::identity::ResourceIdentity;
use super::operation::OperationDef;
use super::presentation::{
    ColumnDef, DetailDef, DetailField, DetailSection, PresentationDef, TableDef,
};
use super::schema::{FieldDef, ResourceSchema};
use super::scope::{ParentRef, ScopeDef, UniquenessScope};
use super::ResourceDef;

/// Entry point: `ResourceDef::build("vpc", "Virtual Private Cloud")`
impl ResourceDef {
    pub fn build(kind: &'static str, description: &'static str) -> ResourceDefBuilder {
        ResourceDefBuilder {
            kind,
            cli_name: kind,
            plural: "",
            description,
            aliases: Vec::new(),
            parents: Vec::new(),
            uniqueness: UniquenessScope::Global,
            fields: Vec::new(),
            operations: Vec::new(),
            columns: Vec::new(),
            detail_sections: Vec::new(),
            table_empty_message: None,
            table_default_sort: None,
        }
    }
}

pub struct ResourceDefBuilder {
    kind: &'static str,
    cli_name: &'static str,
    plural: &'static str,
    description: &'static str,
    aliases: Vec<&'static str>,
    parents: Vec<ParentRef>,
    uniqueness: UniquenessScope,
    fields: Vec<FieldDef>,
    operations: Vec<OperationDef>,
    columns: Vec<ColumnDef>,
    detail_sections: Vec<DetailSection>,
    table_empty_message: Option<&'static str>,
    table_default_sort: Option<&'static str>,
}

impl ResourceDefBuilder {
    // ── Identity ──

    pub fn cli_name(mut self, name: &'static str) -> Self {
        self.cli_name = name;
        self
    }

    pub fn plural(mut self, plural: &'static str) -> Self {
        self.plural = plural;
        self
    }

    pub fn alias(mut self, alias: &'static str) -> Self {
        self.aliases.push(alias);
        self
    }

    // ── Scope ──

    pub fn scope_global(mut self) -> Self {
        self.uniqueness = UniquenessScope::Global;
        self
    }

    pub fn parent(
        mut self,
        kind: &'static str,
        flag: &'static str,
        description: &'static str,
    ) -> Self {
        self.parents.push(ParentRef {
            kind,
            flag,
            required_on_create: true,
            required_on_resolve: false,
            description,
        });
        self.uniqueness = UniquenessScope::WithinParent(kind);
        self
    }

    pub fn parent_required(
        mut self,
        kind: &'static str,
        flag: &'static str,
        description: &'static str,
    ) -> Self {
        self.parents.push(ParentRef {
            kind,
            flag,
            required_on_create: true,
            required_on_resolve: true,
            description,
        });
        self.uniqueness = UniquenessScope::WithinParent(kind);
        self
    }

    // ── Schema fields ──

    pub fn field(mut self, field: FieldDef) -> Self {
        self.fields.push(field);
        self
    }

    // ── Operations ──

    pub fn create(mut self) -> Self {
        self.operations.push(OperationDef::create());
        self
    }

    pub fn list(mut self) -> Self {
        self.operations.push(OperationDef::list());
        self
    }

    pub fn get(mut self) -> Self {
        self.operations.push(OperationDef::get());
        self
    }

    pub fn delete(mut self) -> Self {
        self.operations.push(OperationDef::delete());
        self
    }

    pub fn crud(self) -> Self {
        self.create().list().get().delete()
    }

    pub fn action(mut self, name: &'static str, description: &'static str) -> Self {
        self.operations
            .push(OperationDef::action(name, description));
        self
    }

    /// Modify the last added operation.
    pub fn op(mut self, f: impl FnOnce(OperationDef) -> OperationDef) -> Self {
        if let Some(last) = self.operations.pop() {
            self.operations.push(f(last));
        }
        self
    }

    // ── Presentation: table ──

    pub fn column(mut self, header: &'static str, field: &'static str) -> Self {
        self.columns.push(ColumnDef::new(header, field));
        self
    }

    pub fn column_def(mut self, col: ColumnDef) -> Self {
        self.columns.push(col);
        self
    }

    pub fn empty_message(mut self, msg: &'static str) -> Self {
        self.table_empty_message = Some(msg);
        self
    }

    pub fn sort_by(mut self, field: &'static str) -> Self {
        self.table_default_sort = Some(field);
        self
    }

    // ── Presentation: detail ──

    pub fn detail_section(mut self, title: Option<&'static str>, fields: Vec<DetailField>) -> Self {
        self.detail_sections.push(DetailSection { title, fields });
        self
    }

    // ── Build ──

    pub fn done(self) -> ResourceDef {
        let plural = if self.plural.is_empty() {
            // Auto-pluralize: kind + "s"
            // Leaked intentionally — these are static program-lifetime strings
            let s = format!("{}s", self.kind);
            Box::leak(s.into_boxed_str()) as &'static str
        } else {
            self.plural
        };

        let aliases: &'static [&'static str] = if self.aliases.is_empty() {
            &[]
        } else {
            let boxed: Box<[&'static str]> = self.aliases.into_boxed_slice();
            Box::leak(boxed)
        };

        ResourceDef {
            identity: ResourceIdentity {
                kind: self.kind,
                cli_name: self.cli_name,
                plural,
                description: self.description,
                aliases,
            },
            scope: ScopeDef {
                parents: self.parents,
                uniqueness: self.uniqueness,
            },
            schema: ResourceSchema {
                fields: self.fields,
            },
            operations: self.operations,
            presentation: PresentationDef {
                table: if self.columns.is_empty() {
                    None
                } else {
                    Some(TableDef {
                        columns: self.columns,
                        default_sort: self.table_default_sort,
                        empty_message: self.table_empty_message,
                    })
                },
                detail: if self.detail_sections.is_empty() {
                    None
                } else {
                    Some(DetailDef {
                        sections: self.detail_sections,
                    })
                },
            },
        }
    }
}
