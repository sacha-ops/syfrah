use anyhow::Result;
use clap::Command;

use syfrah_core::resource::{dispatch, generate_command, ResourceRegistry};

/// Build the resource registry with all known resources.
fn build_registry() -> ResourceRegistry {
    let mut registry = ResourceRegistry::new();

    // Hypervisor — the core resource
    registry.register(syfrah_hypervisor::handlers::registration());

    registry
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
