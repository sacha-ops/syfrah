pub mod api;
pub mod cli;
pub mod daemon;
pub mod discovery;
pub mod error;
pub mod hypervisor;
pub mod hypervisor_handler;
pub mod ipam;
pub mod nic;
pub mod placement;
pub mod sg_rules;
pub mod store;
pub mod ttl;
pub mod types;
pub mod validation;
pub mod vpc;

pub use api::{send_org_request, OrgLayerHandler, OrgRequest, OrgResponse, ResolvedSubnet};
pub use cli::{
    EnvCommand, HypervisorCommand, NatGwCommand, OrgCommand, ProjectCommand, RouteCommand,
    RouteTableAction, SgCommand, SubnetCommand, VpcCommand,
};
pub use error::OrgError;
pub use hypervisor::HypervisorStore;
pub use hypervisor_handler::HypervisorLayerHandler;
pub use ipam::{AllocationState, IpAllocation, IpamStore, SubnetBitmap};
pub use nic::NicStore;
pub use placement::PlacementStore;
pub use sg_rules::SgRuleStore;
pub use store::OrgStore;
pub use types::{
    AllocatableCapacity, CpuArchitecture, Direction, DiskType, Environment, EnvironmentId, GpuSpec,
    HardwareSpec, Hypervisor, HypervisorId, HypervisorState, HypervisorStatus, NatGateway,
    NatGatewayId, NetworkInterface, NicId, Org, OrgId, PeeringId, PeeringStatus, PlacementAction,
    PortRange, Project, ProjectId, Protocol, ResourceState, Route, RouteId, RouteOrigin,
    RouteStatus, RouteTable, RouteTableId, RouteTarget, RuleId, RuleSource, SecurityGroup,
    SecurityGroupId, SecurityGroupRule, Subnet, SubnetId, Taint, TaintEffect, VmPlacement, Vpc,
    VpcAttachment, VpcId, VpcOwner, VpcPeering,
};
pub use validation::validate_name;
pub use vpc::{cidrs_overlap, parse_and_validate_cidr, validate_subnet_cidr, VpcStore};

#[cfg(test)]
mod tests;
