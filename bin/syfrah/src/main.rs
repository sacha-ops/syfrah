use anyhow::Result;
use clap::Command;
use std::future::Future;
use std::pin::Pin;

use syfrah_core::resource::{
    dispatch, generate_command, ColumnDef, DetailDef, DetailField, DetailSection, HandlerFn,
    OperationDef, OperationRequest, OperationResponse, PresentationDef, ResourceDef,
    ResourceIdentity, ResourceRegistration, ResourceRegistry, ResourceSchema, ScopeDef, TableDef,
};

/// Build the resource registry with all known resources.
fn build_registry() -> ResourceRegistry {
    let mut registry = ResourceRegistry::new();
    registry.register(ping_resource());
    registry
}

/// A minimal test resource to validate the framework end-to-end.
fn ping_resource() -> ResourceRegistration {
    let def = ResourceDef {
        identity: ResourceIdentity {
            kind: "ping",
            cli_name: "ping",
            plural: "pings",
            description: "Test resource for framework validation",
            aliases: &[],
        },
        scope: ScopeDef::global(),
        schema: ResourceSchema::new(),
        operations: vec![
            OperationDef::list()
                .with_example("syfrah ping list")
                .with_example("syfrah ping list --json"),
            OperationDef::get().with_example("syfrah ping get hello"),
        ],
        presentation: PresentationDef {
            table: Some(TableDef {
                columns: vec![
                    ColumnDef::new("NAME", "name"),
                    ColumnDef::new("STATUS", "status"),
                ],
                default_sort: None,
                empty_message: Some("No pings found."),
            }),
            detail: Some(DetailDef {
                sections: vec![DetailSection {
                    title: None,
                    fields: vec![
                        DetailField::new("Name", "name"),
                        DetailField::new("Status", "status"),
                    ],
                }],
            }),
        },
    };

    let handler: HandlerFn = Box::new(
        |req: OperationRequest| -> Pin<Box<dyn Future<Output = Result<OperationResponse>> + Send>> {
            Box::pin(async move {
                match req.operation.as_str() {
                    "list" => Ok(OperationResponse::ResourceList(vec![
                        serde_json::json!({"name": "pong", "status": "ok"}),
                    ])),
                    "get" => {
                        let name = req.name.unwrap_or_else(|| "unknown".to_string());
                        Ok(OperationResponse::Resource(
                            serde_json::json!({"name": name, "status": "ok"}),
                        ))
                    }
                    _ => Ok(OperationResponse::Message("unknown operation".to_string())),
                }
            })
        },
    );

    ResourceRegistration { def, handler }
}

#[tokio::main]
async fn main() -> Result<()> {
    let registry = build_registry();

    let mut app = Command::new("syfrah")
        .about("Syfrah — turn dedicated servers into a programmable cloud")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .arg_required_else_help(true);

    for reg in registry.iter() {
        app = app.subcommand(generate_command(&reg.def));
    }

    let matches = app.get_matches();

    if let Some((sub_name, sub_matches)) = matches.subcommand() {
        if let Some(reg) = registry.find(sub_name) {
            if let Some((op_name, op_matches)) = sub_matches.subcommand() {
                return dispatch(reg, op_name, op_matches).await;
            } else {
                anyhow::bail!("specify a subcommand. Run 'syfrah {sub_name} --help' for details.");
            }
        } else {
            anyhow::bail!("unknown command: {sub_name}");
        }
    }

    Ok(())
}
