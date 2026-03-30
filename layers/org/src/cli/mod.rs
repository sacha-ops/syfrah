pub mod env;
pub mod org;
pub mod project;

pub use env::EnvCommand;
pub use org::OrgCommand;
pub use project::ProjectCommand;

/// Execute an org subcommand.
pub async fn run_org(cmd: OrgCommand) -> anyhow::Result<()> {
    org::run_org_command(&cmd)
}

/// Execute a project subcommand.
pub async fn run_project(cmd: ProjectCommand) -> anyhow::Result<()> {
    project::run_project_command(&cmd)
}

/// Execute an env subcommand.
pub async fn run_env(cmd: EnvCommand) -> anyhow::Result<()> {
    env::run_env_command(&cmd)
}
