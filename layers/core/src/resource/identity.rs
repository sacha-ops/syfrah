/// Resource identity — who is this resource?
#[derive(Debug, Clone)]
pub struct ResourceIdentity {
    /// Internal kind key, e.g. "vpc"
    pub kind: &'static str,
    /// What the user types in the CLI, e.g. "vpc"
    pub cli_name: &'static str,
    /// Plural form for messages, e.g. "vpcs"
    pub plural: &'static str,
    /// Human description for help text
    pub description: &'static str,
    /// Alternative names accepted by the CLI, e.g. ["network"]
    pub aliases: &'static [&'static str],
}
