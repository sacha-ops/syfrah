/// Errors that can occur in org operations.
#[derive(Debug, thiserror::Error)]
pub enum OrgError {
    #[error("org already exists: {0}")]
    AlreadyExists(String),

    #[error("org not found: {0}")]
    NotFound(String),

    #[error("org has projects and cannot be deleted: {0}")]
    OrgHasProjects(String),

    #[error("org has {count} VPC(s) — delete them first: {org}")]
    OrgHasVpcs { org: String, count: usize },

    #[error("project already exists: {project} in org {org}")]
    ProjectAlreadyExists { org: String, project: String },

    #[error("project not found: {project} in org {org}")]
    ProjectNotFound { org: String, project: String },

    #[error("project has environments and cannot be deleted: {project} in org {org}")]
    ProjectHasEnvironments { org: String, project: String },

    #[error("environment already exists: {0}")]
    EnvAlreadyExists(String),

    #[error("environment not found: {0}")]
    EnvNotFound(String),

    #[error("environment is protected from deletion: {0}")]
    EnvProtected(String),

    #[error("vpc already exists: {0}")]
    VpcAlreadyExists(String),

    #[error("vpc not found: {0}")]
    VpcNotFound(String),

    #[error("invalid {context} name: {reason}")]
    InvalidName { context: String, reason: String },

    #[error("invalid CIDR: {0}")]
    InvalidCidr(String),

    #[error("CIDR overlap: {new_cidr} overlaps with existing {existing_cidr}")]
    CidrOverlap {
        new_cidr: String,
        existing_cidr: String,
    },

    #[error("no available CIDR block in the auto-allocation range")]
    CidrExhausted,

    #[error("cannot delete vpc '{name}': has {count} active subnet(s)")]
    VpcHasSubnets { name: String, count: usize },

    #[error("cannot delete vpc '{name}': has {count} active peering(s)")]
    VpcHasPeerings { name: String, count: usize },

    #[error("cannot delete vpc '{name}': has {count} vm(s) in its subnets")]
    VpcHasVms { name: String, count: usize },

    #[error("cannot delete subnet '{name}': has {count} active VM(s)")]
    SubnetHasVms { name: String, count: usize },

    #[error("vpc is not shared: {0}")]
    VpcNotShared(String),

    #[error("project '{project}' is already attached to vpc '{vpc}'")]
    VpcAlreadyAttached { vpc: String, project: String },

    #[error("project '{project}' is not attached to vpc '{vpc}'")]
    VpcNotAttached { vpc: String, project: String },

    #[error("subnet already exists: {vpc}/{subnet}")]
    SubnetAlreadyExists { vpc: String, subnet: String },

    #[error("subnet not found: {vpc}/{subnet}")]
    SubnetNotFound { vpc: String, subnet: String },

    #[error("subnet CIDR {cidr} is outside VPC range {vpc_cidr}")]
    SubnetCidrOutOfRange { cidr: String, vpc_cidr: String },

    #[error("subnet CIDR {new_cidr} overlaps with existing subnet {existing_cidr}")]
    SubnetCidrOverlap {
        new_cidr: String,
        existing_cidr: String,
    },

    #[error("no available /24 block in VPC CIDR {0}")]
    SubnetCidrExhausted(String),

    #[error("subnet CIDR {subnet_cidr} is not within VPC CIDR {vpc_cidr}")]
    SubnetOutsideVpc {
        subnet_cidr: String,
        vpc_cidr: String,
    },

    #[error("subnet CIDR {new_cidr} overlaps with existing subnet {existing_cidr}")]
    SubnetOverlap {
        new_cidr: String,
        existing_cidr: String,
    },

    #[error("invalid subnet prefix length: expected /{min} to /{max}, got /{actual}")]
    SubnetPrefixLength { min: u8, max: u8, actual: u8 },

    #[error("cannot peer a VPC with itself")]
    SelfPeering,

    #[error("already peered")]
    DuplicatePeering,

    #[error("store error: {0}")]
    StoreError(String),
}

impl From<syfrah_state::StateError> for OrgError {
    fn from(e: syfrah_state::StateError) -> Self {
        OrgError::StoreError(e.to_string())
    }
}

/// Result type for org operations.
pub type Result<T> = std::result::Result<T, OrgError>;
