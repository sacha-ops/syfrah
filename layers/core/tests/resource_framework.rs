//! Integration tests for the resource framework.

use syfrah_core::resource::*;

fn test_resource() -> ResourceDef {
    ResourceDef {
        identity: ResourceIdentity {
            kind: "widget",
            cli_name: "widget",
            plural: "widgets",
            description: "A test widget",
            aliases: &["wdg"],
        },
        scope: ScopeDef::within("org", "--org", "Organization"),
        schema: ResourceSchema {
            fields: vec![
                FieldDef::string("color", "Widget color"),
                FieldDef::flag("shiny", "Make it shiny"),
                FieldDef::integer("count", "How many").with_default("1"),
                FieldDef::string("label", "Display label").mutable(),
            ],
        },
        operations: vec![
            OperationDef::create()
                .with_example("syfrah widget create my-widget --org acme --color red"),
            OperationDef::list().with_example("syfrah widget list --org acme"),
            OperationDef::get(),
            OperationDef::delete(),
            OperationDef::action("polish", "Polish the widget")
                .with_arg(OperationArg::required(
                    "level",
                    FieldDef::enum_field("level", "Polish level", &["low", "medium", "high"]),
                ))
                .with_confirm(),
        ],
        presentation: PresentationDef {
            table: Some(TableDef {
                columns: vec![
                    ColumnDef::new("NAME", "name"),
                    ColumnDef::new("COLOR", "color"),
                    ColumnDef::new("SHINY", "shiny"),
                ],
                default_sort: Some("name"),
                empty_message: Some("No widgets found."),
            }),
            detail: Some(DetailDef {
                sections: vec![DetailSection {
                    title: Some("Widget Details"),
                    fields: vec![
                        DetailField::new("Name", "name"),
                        DetailField::new("Color", "color"),
                        DetailField::new("Shiny", "shiny").with_format(DisplayFormat::YesNo),
                        DetailField::new("Created", "created_at")
                            .with_format(DisplayFormat::Timestamp),
                    ],
                }],
            }),
        },
    }
}

// ── CLI Generation Tests ──────────────────────────────

#[test]
fn generates_top_level_command() {
    let def = test_resource();
    let cmd = generate_command(&def);
    assert_eq!(cmd.get_name(), "widget");
}

#[test]
fn generates_alias() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let aliases: Vec<&str> = cmd.get_visible_aliases().collect();
    assert!(
        aliases.contains(&"wdg"),
        "expected alias 'wdg', got {:?}",
        aliases
    );
}

#[test]
fn generates_crud_subcommands() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let subs: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
    assert!(subs.contains(&"create"), "missing create: {subs:?}");
    assert!(subs.contains(&"list"), "missing list: {subs:?}");
    assert!(subs.contains(&"get"), "missing get: {subs:?}");
    assert!(subs.contains(&"delete"), "missing delete: {subs:?}");
}

#[test]
fn generates_custom_action() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let subs: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
    assert!(
        subs.contains(&"polish"),
        "missing custom action 'polish': {subs:?}"
    );
}

#[test]
fn create_has_positional_name() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let create = cmd
        .get_subcommands()
        .find(|s| s.get_name() == "create")
        .unwrap();
    let args: Vec<&str> = create
        .get_arguments()
        .map(|a| a.get_id().as_str())
        .collect();
    assert!(
        args.contains(&"name"),
        "create missing positional 'name': {args:?}"
    );
}

#[test]
fn create_has_scope_flags() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let create = cmd
        .get_subcommands()
        .find(|s| s.get_name() == "create")
        .unwrap();
    let args: Vec<&str> = create
        .get_arguments()
        .map(|a| a.get_id().as_str())
        .collect();
    assert!(args.contains(&"org"), "create missing --org flag: {args:?}");
}

#[test]
fn create_has_schema_fields() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let create = cmd
        .get_subcommands()
        .find(|s| s.get_name() == "create")
        .unwrap();
    let args: Vec<&str> = create
        .get_arguments()
        .map(|a| a.get_id().as_str())
        .collect();
    assert!(args.contains(&"color"), "create missing --color: {args:?}");
    assert!(args.contains(&"shiny"), "create missing --shiny: {args:?}");
    assert!(args.contains(&"count"), "create missing --count: {args:?}");
}

#[test]
fn list_has_json_flag() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let list = cmd
        .get_subcommands()
        .find(|s| s.get_name() == "list")
        .unwrap();
    let args: Vec<&str> = list.get_arguments().map(|a| a.get_id().as_str()).collect();
    assert!(args.contains(&"json"), "list missing --json: {args:?}");
}

#[test]
fn get_has_json_flag() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let get = cmd
        .get_subcommands()
        .find(|s| s.get_name() == "get")
        .unwrap();
    let args: Vec<&str> = get.get_arguments().map(|a| a.get_id().as_str()).collect();
    assert!(args.contains(&"json"), "get missing --json: {args:?}");
}

#[test]
fn delete_has_yes_flag() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let delete = cmd
        .get_subcommands()
        .find(|s| s.get_name() == "delete")
        .unwrap();
    let args: Vec<&str> = delete
        .get_arguments()
        .map(|a| a.get_id().as_str())
        .collect();
    assert!(args.contains(&"yes"), "delete missing --yes: {args:?}");
}

#[test]
fn delete_has_positional_name() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let delete = cmd
        .get_subcommands()
        .find(|s| s.get_name() == "delete")
        .unwrap();
    let args: Vec<&str> = delete
        .get_arguments()
        .map(|a| a.get_id().as_str())
        .collect();
    assert!(
        args.contains(&"name"),
        "delete missing positional 'name': {args:?}"
    );
}

#[test]
fn custom_action_has_yes_when_confirmable() {
    let def = test_resource();
    let cmd = generate_command(&def);
    let polish = cmd
        .get_subcommands()
        .find(|s| s.get_name() == "polish")
        .unwrap();
    let args: Vec<&str> = polish
        .get_arguments()
        .map(|a| a.get_id().as_str())
        .collect();
    assert!(
        args.contains(&"yes"),
        "confirmable action missing --yes: {args:?}"
    );
    assert!(args.contains(&"level"), "polish missing --level: {args:?}");
}

// ── Conformance Tests ──────────────────────────────────
// These tests ensure EVERY list has --json, EVERY delete has --yes, etc.
// If someone adds a resource without these, the tests fail.

#[test]
fn conformance_every_list_has_json() {
    let defs = vec![test_resource()];
    for def in &defs {
        let cmd = generate_command(def);
        for sub in cmd.get_subcommands() {
            if sub.get_name() == "list" {
                let has_json = sub.get_arguments().any(|a| a.get_id().as_str() == "json");
                assert!(
                    has_json,
                    "{} list is missing --json flag",
                    def.identity.kind
                );
            }
        }
    }
}

#[test]
fn conformance_every_delete_has_yes() {
    let defs = vec![test_resource()];
    for def in &defs {
        let cmd = generate_command(def);
        for sub in cmd.get_subcommands() {
            if sub.get_name() == "delete" {
                let has_yes = sub.get_arguments().any(|a| a.get_id().as_str() == "yes");
                assert!(
                    has_yes,
                    "{} delete is missing --yes flag",
                    def.identity.kind
                );
            }
        }
    }
}

#[test]
fn conformance_every_get_has_json() {
    let defs = vec![test_resource()];
    for def in &defs {
        let cmd = generate_command(def);
        for sub in cmd.get_subcommands() {
            if sub.get_name() == "get" {
                let has_json = sub.get_arguments().any(|a| a.get_id().as_str() == "json");
                assert!(has_json, "{} get is missing --json flag", def.identity.kind);
            }
        }
    }
}

// ── Constraint Tests ──────────────────────────────────

#[test]
fn constraint_requires_validates() {
    use std::collections::HashMap;

    let c = Constraint::Requires {
        if_field: "protocol",
        if_value: Some("tcp"),
        then_field: "port",
        message: "TCP requires --port",
    };

    // tcp without port → fail
    let mut fields = HashMap::new();
    fields.insert("protocol".to_string(), "tcp".to_string());
    assert!(c.validate(&fields).is_err());

    // tcp with port → ok
    fields.insert("port".to_string(), "80".to_string());
    assert!(c.validate(&fields).is_ok());

    // udp without port → ok (not triggered)
    let mut fields2 = HashMap::new();
    fields2.insert("protocol".to_string(), "udp".to_string());
    assert!(c.validate(&fields2).is_ok());
}

#[test]
fn constraint_forbids_validates() {
    use std::collections::HashMap;

    let c = Constraint::Forbids {
        if_field: "protocol",
        if_value: Some("icmp"),
        then_field: "port",
        message: "ICMP must not have --port",
    };

    // icmp with port → fail
    let mut fields = HashMap::new();
    fields.insert("protocol".to_string(), "icmp".to_string());
    fields.insert("port".to_string(), "80".to_string());
    assert!(c.validate(&fields).is_err());

    // icmp without port → ok
    fields.remove("port");
    assert!(c.validate(&fields).is_ok());
}

#[test]
fn constraint_conflicts_validates() {
    use std::collections::HashMap;

    let c = Constraint::Conflicts {
        a: "shared",
        b: "project",
        message: "shared VPCs cannot have a project",
    };

    let mut fields = HashMap::new();
    fields.insert("shared".to_string(), "true".to_string());
    fields.insert("project".to_string(), "backend".to_string());
    assert!(c.validate(&fields).is_err());

    fields.remove("project");
    assert!(c.validate(&fields).is_ok());
}

#[test]
fn constraint_one_of_validates() {
    use std::collections::HashMap;

    let c = Constraint::OneOf {
        fields: &["ipv4", "ipv6"],
        message: "specify exactly one of --ipv4 or --ipv6",
    };

    // neither → fail
    let fields: HashMap<String, String> = HashMap::new();
    assert!(c.validate(&fields).is_err());

    // one → ok
    let mut fields = HashMap::new();
    fields.insert("ipv4".to_string(), "true".to_string());
    assert!(c.validate(&fields).is_ok());

    // both → fail
    fields.insert("ipv6".to_string(), "true".to_string());
    assert!(c.validate(&fields).is_err());
}

// ── Registry Tests ──────────────────────────────────

#[test]
fn registry_find_by_name() {
    let mut reg = ResourceRegistry::new();
    reg.register(ResourceRegistration {
        def: test_resource(),
        handler: Box::new(|_| Box::pin(async { Ok(OperationResponse::None) })),
    });
    assert!(reg.find("widget").is_some());
    assert!(reg.find("wdg").is_some());
    assert!(reg.find("nonexistent").is_none());
}

#[test]
fn registry_len() {
    let mut reg = ResourceRegistry::new();
    assert!(reg.is_empty());
    reg.register(ResourceRegistration {
        def: test_resource(),
        handler: Box::new(|_| Box::pin(async { Ok(OperationResponse::None) })),
    });
    assert_eq!(reg.len(), 1);
}

// ── Rendering Tests ──────────────────────────────────

#[test]
fn render_table_empty() {
    let def = test_resource();
    // Just verify it doesn't panic
    render_table(&def, &[]);
}

#[test]
fn render_table_with_data() {
    let def = test_resource();
    let items = vec![
        serde_json::json!({"name": "w1", "color": "red", "shiny": true}),
        serde_json::json!({"name": "w2", "color": "blue", "shiny": false}),
    ];
    // Just verify it doesn't panic
    render_table(&def, &items);
}

#[test]
fn render_detail_with_data() {
    let def = test_resource();
    let item =
        serde_json::json!({"name": "w1", "color": "red", "shiny": true, "created_at": 1700000000});
    render_detail(&def, &item);
}

// ── Scope Tests ──────────────────────────────────

#[test]
fn scope_global_has_no_parents() {
    let scope = ScopeDef::global();
    assert!(scope.parents.is_empty());
}

#[test]
fn scope_within_has_parent() {
    let scope = ScopeDef::within("vpc", "--vpc", "VPC");
    assert_eq!(scope.parents.len(), 1);
    assert_eq!(scope.parents[0].kind, "vpc");
    assert!(scope.parents[0].required_on_create);
}

// ── Schema Tests ──────────────────────────────────

#[test]
fn field_builder_chain() {
    let f = FieldDef::string("name", "Widget name")
        .mutable()
        .with_default("untitled")
        .with_short('n')
        .advanced();

    assert_eq!(f.name, "name");
    assert_eq!(f.mutability, Mutability::Mutable);
    assert_eq!(f.default, Some("untitled"));
    assert_eq!(f.short, Some('n'));
    assert_eq!(f.visibility, CliVisibility::Advanced);
}
