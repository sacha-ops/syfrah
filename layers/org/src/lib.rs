pub mod api;
pub mod cli;
pub mod daemon;
pub mod error;
pub mod ipam;
pub mod placement;
pub mod store;
pub mod ttl;
pub mod types;
pub mod validation;
pub mod vpc;

pub use api::OrgHandler;
pub use cli::{EnvCommand, OrgCommand, ProjectCommand, SubnetCommand, VpcCommand};
pub use error::OrgError;
pub use ipam::{AllocationState, IpAllocation, IpamStore, SubnetBitmap};
pub use placement::PlacementStore;
pub use store::OrgStore;
pub use types::{
    Environment, EnvironmentId, Org, OrgId, PeeringId, PeeringStatus, PlacementAction, Project,
    ProjectId, Subnet, SubnetId, VmPlacement, Vpc, VpcAttachment, VpcId, VpcOwner, VpcPeering,
};
pub use validation::validate_name;
pub use vpc::{cidrs_overlap, parse_and_validate_cidr, validate_subnet_cidr, VpcStore};

#[cfg(test)]
mod tests;
