use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use ipnet::Ipv4Net;
use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{
    Environment, EnvironmentId, NatGateway, NatGatewayId, NetworkInterface, Org, OrgId, PeeringId,
    PeeringStatus, Project, ProjectId, ResourceState, Route, RouteId, RouteOrigin, RouteStatus,
    RouteTable, RouteTableId, RouteTarget, SecurityGroup, SecurityGroupId, Subnet, SubnetId, Vpc,
    VpcAttachment, VpcId, VpcOwner, VpcPeering,
};
use crate::validation::validate_name;
use crate::vpc::{cidrs_overlap, parse_and_validate_cidr};

const TABLE: &str = "orgs";
const PROJECTS_TABLE: &str = "projects";
const ENVIRONMENTS_TABLE: &str = "environments";
const VPCS_TABLE: &str = "vpcs";
const VPC_ATTACHMENTS_TABLE: &str = "vpc_attachments";
const SUBNETS_TABLE: &str = "subnets";
const PEERINGS_TABLE: &str = "vpc_peerings";
const SECURITY_GROUPS_TABLE: &str = "security_groups";
const NICS_TABLE: &str = "network_interfaces";
const ROUTE_TABLES_TABLE: &str = "route_tables";
const ROUTES_TABLE: &str = "routes";
const SUBNET_ROUTE_ASSOC_TABLE: &str = "subnet_route_associations";
const NAT_GATEWAYS_TABLE: &str = "nat_gateways";
const VNI_COUNTER_TABLE: &str = "vni_counter";
const VNI_COUNTER_KEY: &str = "counter";
const VNI_START: u32 = 100;

/// Persistent store for organizations backed by redb.
pub struct OrgStore {
    db: LayerDb,
}

impl OrgStore {
    /// Create a new `OrgStore` with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    // ── Org operations ───────────────────────────────────────────────

    /// Create a new organization. Validates the name, checks for duplicates.
    pub fn create(&self, name: &str) -> Result<Org> {
        validate_name(name, "org")?;

        if self.db.exists(TABLE, name)? {
            return Err(OrgError::AlreadyExists(name.to_string()));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let org = Org {
            id: OrgId(format!("org-{name}")),
            name: name.to_string(),
            created_at: now,
        };

        self.db.set(TABLE, name, &org)?;
        Ok(org)
    }

    /// Get an organization by name. Returns `None` if it doesn't exist.
    pub fn get(&self, name: &str) -> Result<Option<Org>> {
        Ok(self.db.get(TABLE, name)?)
    }

    /// List all organizations.
    pub fn list(&self) -> Result<Vec<Org>> {
        let entries: Vec<(String, Org)> = self.db.list(TABLE)?;
        Ok(entries.into_iter().map(|(_, org)| org).collect())
    }

    /// Delete an organization by name. Fails if it has projects.
    pub fn delete(&self, name: &str) -> Result<()> {
        if !self.db.exists(TABLE, name)? {
            return Err(OrgError::NotFound(name.to_string()));
        }

        // Check for child VPCs (org-owned or project-owned).
        // list_vpcs_by_org already includes project-scoped VPCs.
        let org_id = OrgId(name.to_string());
        let all_vpcs = self.list_vpcs_by_org(&org_id)?;
        if !all_vpcs.is_empty() {
            return Err(OrgError::OrgHasVpcs {
                org: name.to_string(),
                count: all_vpcs.len(),
            });
        }

        // Check for child projects
        let projects = self.list_projects(name)?;
        if !projects.is_empty() {
            return Err(OrgError::OrgHasProjects(name.to_string()));
        }

        self.db.delete(TABLE, name)?;
        Ok(())
    }

    // ── Project operations ───────────────────────────────────────────

    /// Build the redb key for a project: "org_name/project_name".
    fn project_key(org: &str, project: &str) -> String {
        format!("{}/{}", org, project)
    }

    /// Create a new project within an organization.
    pub fn create_project(&self, org: &str, name: &str) -> Result<Project> {
        validate_name(name, "project")?;

        // Verify org exists
        if !self.db.exists(TABLE, org)? {
            return Err(OrgError::NotFound(org.to_string()));
        }

        let key = Self::project_key(org, name);
        if self.db.exists(PROJECTS_TABLE, &key)? {
            return Err(OrgError::ProjectAlreadyExists {
                org: org.to_string(),
                project: name.to_string(),
            });
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let project = Project {
            id: ProjectId(key.clone()),
            name: name.to_string(),
            org_id: OrgId(org.to_string()),
            created_at: now,
        };

        self.db.set(PROJECTS_TABLE, &key, &project)?;
        Ok(project)
    }

    /// Get a project by org and project name.
    pub fn get_project(&self, org: &str, name: &str) -> Result<Option<Project>> {
        let key = Self::project_key(org, name);
        Ok(self.db.get(PROJECTS_TABLE, &key)?)
    }

    /// List all projects in an organization.
    pub fn list_projects(&self, org: &str) -> Result<Vec<Project>> {
        let all: Vec<(String, Project)> = self.db.list(PROJECTS_TABLE)?;
        let prefix = format!("{}/", org);
        Ok(all
            .into_iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .map(|(_, project)| project)
            .collect())
    }

    /// Delete a project. Fails if it has any environments.
    pub fn delete_project(&self, org: &str, name: &str) -> Result<()> {
        let key = Self::project_key(org, name);

        if !self.db.exists(PROJECTS_TABLE, &key)? {
            return Err(OrgError::ProjectNotFound {
                org: org.to_string(),
                project: name.to_string(),
            });
        }

        // Check for child environments
        let envs = self.list_envs(org, name)?;
        if !envs.is_empty() {
            return Err(OrgError::ProjectHasEnvironments {
                org: org.to_string(),
                project: name.to_string(),
            });
        }

        self.db.delete(PROJECTS_TABLE, &key)?;
        Ok(())
    }

    // ── Environment operations ──────────────────────────────────────

    fn env_key(org: &str, project: &str, env: &str) -> String {
        format!("{org}/{project}/{env}")
    }

    /// Create an environment within a project.
    pub fn create_env(
        &self,
        org: &str,
        project: &str,
        name: &str,
        ttl: Option<u64>,
        deletion_protection: bool,
        labels: HashMap<String, String>,
    ) -> Result<Environment> {
        validate_name(name, "environment")?;

        // Verify project exists
        let project_key = Self::project_key(org, project);
        if !self.db.exists(PROJECTS_TABLE, &project_key)? {
            return Err(OrgError::ProjectNotFound {
                org: org.to_string(),
                project: project.to_string(),
            });
        }

        let env_key = Self::env_key(org, project, name);
        if self.db.exists(ENVIRONMENTS_TABLE, &env_key)? {
            return Err(OrgError::EnvAlreadyExists(name.to_string()));
        }

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let expires_at = ttl.map(|t| created_at + t);

        let env = Environment {
            id: EnvironmentId(env_key.clone()),
            name: name.to_string(),
            project_id: ProjectId(project_key),
            ttl,
            deletion_protection,
            labels,
            created_at,
            expires_at,
        };

        self.db.set(ENVIRONMENTS_TABLE, &env_key, &env)?;
        Ok(env)
    }

    /// Get an environment by org, project, and name.
    pub fn get_env(&self, org: &str, project: &str, name: &str) -> Result<Environment> {
        let key = Self::env_key(org, project, name);
        self.db
            .get::<Environment>(ENVIRONMENTS_TABLE, &key)?
            .ok_or_else(|| OrgError::EnvNotFound(name.to_string()))
    }

    /// List environments for a given org and project.
    pub fn list_envs(&self, org: &str, project: &str) -> Result<Vec<Environment>> {
        let prefix = format!("{org}/{project}/");
        Ok(self
            .db
            .list::<Environment>(ENVIRONMENTS_TABLE)?
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v)
            .collect())
    }

    /// Extend (or set) the TTL of an environment.
    ///
    /// The new TTL is measured from **now**, not from the original creation
    /// time, so `extend_env("acme", "backend", "ci", 7200)` always gives
    /// 2 hours from the current moment.
    pub fn extend_env(
        &self,
        org: &str,
        project: &str,
        name: &str,
        ttl: u64,
    ) -> Result<Environment> {
        let key = Self::env_key(org, project, name);

        let mut env = self
            .db
            .get::<Environment>(ENVIRONMENTS_TABLE, &key)?
            .ok_or_else(|| OrgError::EnvNotFound(name.to_string()))?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        env.ttl = Some(ttl);
        env.expires_at = Some(now + ttl);

        self.db.set(ENVIRONMENTS_TABLE, &key, &env)?;
        Ok(env)
    }

    /// Toggle deletion protection on an environment.
    pub fn update_env_protection(
        &self,
        org: &str,
        project: &str,
        name: &str,
        enabled: bool,
    ) -> Result<Environment> {
        let key = Self::env_key(org, project, name);

        let mut env = self
            .db
            .get::<Environment>(ENVIRONMENTS_TABLE, &key)?
            .ok_or_else(|| OrgError::EnvNotFound(name.to_string()))?;

        env.deletion_protection = enabled;
        self.db.set(ENVIRONMENTS_TABLE, &key, &env)?;
        Ok(env)
    }

    /// Delete an environment. Fails if deletion protection is enabled.
    pub fn delete_env(&self, org: &str, project: &str, name: &str) -> Result<()> {
        let key = Self::env_key(org, project, name);

        let env = self
            .db
            .get::<Environment>(ENVIRONMENTS_TABLE, &key)?
            .ok_or_else(|| OrgError::EnvNotFound(name.to_string()))?;

        if env.deletion_protection {
            return Err(OrgError::EnvProtected(name.to_string()));
        }

        self.db.delete(ENVIRONMENTS_TABLE, &key)?;
        Ok(())
    }

    // ── VPC operations ──────────────────────────────────────────────

    /// Validate a CIDR string. Accepts patterns like "10.0.0.0/16", "10.1.0.0/24".
    /// Collect parsed CIDRs of all VPCs belonging to the same org as `owner`.
    fn existing_cidrs_for_org(&self, owner: &VpcOwner) -> Result<Vec<Ipv4Net>> {
        let org_name = match owner {
            VpcOwner::Org(org_id) => org_id.0.clone(),
            VpcOwner::Project(proj_id) => proj_id
                .0
                .split('/')
                .next()
                .unwrap_or(&proj_id.0)
                .to_string(),
        };

        let all_vpcs = self.list_vpcs()?;
        Ok(all_vpcs
            .into_iter()
            .filter(|v| match &v.owner {
                VpcOwner::Org(oid) => oid.0 == org_name,
                VpcOwner::Project(pid) => pid.0.starts_with(&format!("{org_name}/")),
            })
            .filter_map(|v| v.cidr.parse::<Ipv4Net>().ok())
            .collect())
    }

    /// Allocate the next VNI. Starts at 100, monotonically increasing.
    ///
    /// Uses a single write transaction to read-then-increment the counter,
    /// preventing concurrent callers from observing the same value.
    fn next_vni(&self) -> Result<u32> {
        Ok(self
            .db
            .atomic_next_counter(VNI_COUNTER_TABLE, VNI_COUNTER_KEY, VNI_START)?)
    }

    /// Create a VPC. Validates the name and CIDR (RFC 1918, prefix 8-28,
    /// no host bits), checks for overlap with existing VPCs in the same org,
    /// allocates a VNI, and persists.
    ///
    /// Returns `OrgError::NotFound` if the org does not exist, or
    /// `OrgError::ProjectNotFound` if the project does not exist.
    pub fn create_vpc(&self, name: &str, cidr: &str, owner: VpcOwner, shared: bool) -> Result<Vpc> {
        validate_name(name, "vpc")?;

        // Full CIDR validation: format, RFC 1918, prefix bounds, host bits
        let net = parse_and_validate_cidr(cidr)?;

        // Validate that the parent org (and project, if applicable) exist.
        match &owner {
            VpcOwner::Org(org_id) => {
                if !self.db.exists(TABLE, &org_id.0)? {
                    return Err(OrgError::NotFound(org_id.0.clone()));
                }
            }
            VpcOwner::Project(proj_id) => {
                // Project IDs are "org/project"
                let parts: Vec<&str> = proj_id.0.splitn(2, '/').collect();
                let (org_name, project_name) = match parts.as_slice() {
                    [org, proj] => (*org, *proj),
                    _ => return Err(OrgError::NotFound(proj_id.0.clone())),
                };
                if !self.db.exists(TABLE, org_name)? {
                    return Err(OrgError::NotFound(org_name.to_string()));
                }
                let project_key = Self::project_key(org_name, project_name);
                if !self.db.exists(PROJECTS_TABLE, &project_key)? {
                    return Err(OrgError::ProjectNotFound {
                        org: org_name.to_string(),
                        project: project_name.to_string(),
                    });
                }
            }
        }

        if self.db.exists(VPCS_TABLE, name)? {
            return Err(OrgError::VpcAlreadyExists(name.to_string()));
        }

        // Check overlap with existing VPCs in the same org
        let existing = self.existing_cidrs_for_org(&owner)?;
        for existing_cidr in &existing {
            if cidrs_overlap(&net, existing_cidr) {
                return Err(OrgError::CidrOverlap {
                    new_cidr: net.to_string(),
                    existing_cidr: existing_cidr.to_string(),
                });
            }
        }

        let vni = self.next_vni()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let vpc = Vpc {
            id: VpcId(format!("vpc-{name}")),
            name: name.to_string(),
            cidr: cidr.to_string(),
            vni,
            owner,
            shared,
            created_at: now,
        };

        self.db.set(VPCS_TABLE, name, &vpc)?;

        // Auto-create the default security group for this VPC.
        self.create_default_sg(&vpc)?;

        // Auto-create the default route table for this VPC.
        self.create_default_route_table(&vpc)?;

        Ok(vpc)
    }

    /// Get a VPC by name.
    pub fn get_vpc(&self, name: &str) -> Result<Option<Vpc>> {
        Ok(self.db.get(VPCS_TABLE, name)?)
    }

    /// Get a VPC by its ID (e.g. "vpc-my-vpc").
    pub fn get_vpc_by_id(&self, vpc_id: &VpcId) -> Result<Option<Vpc>> {
        let all = self.list_vpcs()?;
        Ok(all.into_iter().find(|v| v.id == *vpc_id))
    }

    /// List all VPCs.
    pub fn list_vpcs(&self) -> Result<Vec<Vpc>> {
        let entries: Vec<(String, Vpc)> = self.db.list(VPCS_TABLE)?;
        Ok(entries.into_iter().map(|(_, vpc)| vpc).collect())
    }

    /// List VPCs owned by a specific project.
    pub fn list_vpcs_by_project(&self, project_id: &ProjectId) -> Result<Vec<Vpc>> {
        let all = self.list_vpcs()?;
        Ok(all
            .into_iter()
            .filter(|vpc| matches!(&vpc.owner, VpcOwner::Project(pid) if pid == project_id))
            .collect())
    }

    /// List VPCs owned by a specific org.
    ///
    /// Returns both org-level (shared) VPCs **and** project-scoped VPCs
    /// whose project belongs to the given org. Project IDs use the
    /// `{org_name}/{project_name}` convention.
    pub fn list_vpcs_by_org(&self, org_id: &OrgId) -> Result<Vec<Vpc>> {
        let prefix = format!("{}/", org_id.0);
        let all = self.list_vpcs()?;
        Ok(all
            .into_iter()
            .filter(|vpc| match &vpc.owner {
                VpcOwner::Org(oid) => oid == org_id,
                VpcOwner::Project(pid) => pid.0.starts_with(&prefix),
            })
            .collect())
    }

    /// Delete a VPC by name. Fails if it has active peerings.
    pub fn delete_vpc(&self, name: &str) -> Result<()> {
        if !self.db.exists(VPCS_TABLE, name)? {
            return Err(OrgError::VpcNotFound(name.to_string()));
        }

        // Check for active peerings
        let peerings = self.list_peerings_by_vpc(name)?;
        if !peerings.is_empty() {
            return Err(OrgError::VpcHasPeerings {
                name: name.to_string(),
                count: peerings.len(),
            });
        }

        self.db.delete(VPCS_TABLE, name)?;
        Ok(())
    }

    // ── VPC attachment operations ────────────────────────────────────

    /// Build a storage key for a VPC attachment.
    fn attachment_key(vpc_name: &str, project_id: &str) -> String {
        format!("{vpc_name}/{project_id}")
    }

    /// Attach a shared VPC to a project.
    pub fn attach_vpc(&self, vpc_name: &str, project_id: &str) -> Result<()> {
        let vpc = self
            .db
            .get::<Vpc>(VPCS_TABLE, vpc_name)?
            .ok_or_else(|| OrgError::VpcNotFound(vpc_name.to_string()))?;

        if !vpc.shared {
            return Err(OrgError::VpcNotShared(vpc_name.to_string()));
        }

        let key = Self::attachment_key(vpc_name, project_id);
        if self.db.exists(VPC_ATTACHMENTS_TABLE, &key)? {
            return Err(OrgError::VpcAlreadyAttached {
                vpc: vpc_name.to_string(),
                project: project_id.to_string(),
            });
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let attachment = VpcAttachment {
            vpc_name: vpc_name.to_string(),
            project_id: ProjectId(project_id.to_string()),
            attached_at: now,
        };

        self.db.set(VPC_ATTACHMENTS_TABLE, &key, &attachment)?;
        Ok(())
    }

    /// Detach a shared VPC from a project.
    pub fn detach_vpc(&self, vpc_name: &str, project_id: &str) -> Result<()> {
        if !self.db.exists(VPCS_TABLE, vpc_name)? {
            return Err(OrgError::VpcNotFound(vpc_name.to_string()));
        }

        let key = Self::attachment_key(vpc_name, project_id);
        if !self.db.exists(VPC_ATTACHMENTS_TABLE, &key)? {
            return Err(OrgError::VpcNotAttached {
                vpc: vpc_name.to_string(),
                project: project_id.to_string(),
            });
        }

        self.db.delete(VPC_ATTACHMENTS_TABLE, &key)?;
        Ok(())
    }

    /// List all projects attached to a VPC.
    pub fn list_attachments(&self, vpc_name: &str) -> Result<Vec<ProjectId>> {
        if !self.db.exists(VPCS_TABLE, vpc_name)? {
            return Err(OrgError::VpcNotFound(vpc_name.to_string()));
        }

        let prefix = format!("{vpc_name}/");
        let all: Vec<(String, VpcAttachment)> = self.db.list(VPC_ATTACHMENTS_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, a)| a.project_id)
            .collect())
    }

    // ── Subnet operations ──────────────────────────────────────────

    /// Build the redb key for a subnet: "vpc_name/subnet_name".
    fn subnet_key(vpc_name: &str, subnet_name: &str) -> String {
        format!("{vpc_name}/{subnet_name}")
    }

    /// Compute the gateway address (.1) from a subnet CIDR.
    ///
    /// For example, "10.1.1.0/24" -> "10.1.1.1".
    fn compute_gateway(cidr: &Ipv4Net) -> std::net::Ipv4Addr {
        let network = cidr.network();
        let octets = network.octets();
        std::net::Ipv4Addr::new(octets[0], octets[1], octets[2], 1)
    }

    /// Auto-allocate the next available /24 within a VPC's CIDR that does
    /// not overlap with any existing subnets in that VPC.
    fn auto_allocate_subnet_cidr(vpc_cidr: &Ipv4Net, existing: &[Ipv4Net]) -> Result<Ipv4Net> {
        let vpc_octets = vpc_cidr.network().octets();
        let vpc_prefix = vpc_cidr.prefix_len();

        // Iterate over all possible /24 blocks within the VPC CIDR.
        // For a /16 (e.g., 10.1.0.0/16), iterate third octet 0..=255.
        // For a /8 (e.g., 10.0.0.0/8), iterate second octet 0..=255 and third 0..=255.
        // For a /24 (e.g., 10.1.1.0/24), only one candidate.
        match vpc_prefix {
            8 => {
                for o2 in vpc_octets[1]..=255u8 {
                    for o3 in 0..=255u8 {
                        let candidate =
                            Ipv4Net::new(std::net::Ipv4Addr::new(vpc_octets[0], o2, o3, 0), 24)
                                .unwrap();
                        if !existing.iter().any(|e| cidrs_overlap(&candidate, e)) {
                            return Ok(candidate);
                        }
                    }
                }
            }
            9..=16 => {
                // For /9 to /16, the second octet range is determined by the VPC.
                // We iterate over the third octet for each valid second octet.
                let net_start = u32::from(vpc_cidr.network());
                let net_end = u32::from(vpc_cidr.broadcast());
                let mut addr = net_start;
                while addr <= net_end {
                    let ip = std::net::Ipv4Addr::from(addr);
                    let candidate = Ipv4Net::new(ip, 24).unwrap();
                    // Ensure candidate is within VPC
                    if vpc_cidr.contains(&candidate.network())
                        && vpc_cidr.contains(&candidate.broadcast())
                        && !existing.iter().any(|e| cidrs_overlap(&candidate, e))
                    {
                        return Ok(candidate);
                    }
                    addr += 256; // next /24
                }
            }
            17..=24 => {
                let net_start = u32::from(vpc_cidr.network());
                let net_end = u32::from(vpc_cidr.broadcast());
                let mut addr = net_start;
                while addr <= net_end {
                    let ip = std::net::Ipv4Addr::from(addr);
                    if let Ok(candidate) = Ipv4Net::new(ip, 24) {
                        if u32::from(candidate.broadcast()) <= net_end
                            && !existing.iter().any(|e| cidrs_overlap(&candidate, e))
                        {
                            return Ok(candidate);
                        }
                    }
                    addr += 256;
                }
            }
            _ => {}
        }

        Err(OrgError::SubnetCidrExhausted(vpc_cidr.to_string()))
    }

    /// Create a subnet within a VPC.
    ///
    /// Validates:
    /// - VPC exists
    /// - Environment exists
    /// - Subnet name is unique within the VPC
    /// - If CIDR is provided, it must be a valid /24 (or other prefix) within the VPC range
    /// - If CIDR is not provided, auto-allocate the next available /24
    /// - No overlap with existing subnets in the same VPC
    ///
    /// Gateway is always computed as .1 of the subnet CIDR.
    pub fn create_subnet(
        &self,
        vpc_name: &str,
        env_id: &EnvironmentId,
        name: &str,
        cidr_opt: Option<&str>,
    ) -> Result<Subnet> {
        validate_name(name, "subnet")?;

        // Verify VPC exists
        let vpc = self
            .db
            .get::<Vpc>(VPCS_TABLE, vpc_name)?
            .ok_or_else(|| OrgError::VpcNotFound(vpc_name.to_string()))?;

        // Verify environment exists
        if !self.db.exists(ENVIRONMENTS_TABLE, &env_id.0)? {
            return Err(OrgError::EnvNotFound(env_id.0.clone()));
        }

        // Check subnet name uniqueness within VPC
        let key = Self::subnet_key(vpc_name, name);
        if self.db.exists(SUBNETS_TABLE, &key)? {
            return Err(OrgError::SubnetAlreadyExists {
                vpc: vpc_name.to_string(),
                subnet: name.to_string(),
            });
        }

        // Parse VPC CIDR
        let vpc_cidr: Ipv4Net = vpc
            .cidr
            .parse()
            .map_err(|_| OrgError::InvalidCidr(format!("VPC CIDR is invalid: {}", vpc.cidr)))?;

        // Get existing subnets in this VPC
        let existing_subnets = self.list_subnets(vpc_name)?;
        let existing_cidrs: Vec<Ipv4Net> = existing_subnets
            .iter()
            .filter_map(|s| s.cidr.parse::<Ipv4Net>().ok())
            .collect();

        // Determine subnet CIDR
        let subnet_cidr = match cidr_opt {
            Some(cidr_str) => {
                let net: Ipv4Net = cidr_str.parse().map_err(|_| {
                    OrgError::InvalidCidr(format!("'{cidr_str}': invalid CIDR format"))
                })?;

                // Verify the subnet CIDR is within the VPC range
                if !vpc_cidr.contains(&net.network()) || !vpc_cidr.contains(&net.broadcast()) {
                    return Err(OrgError::SubnetCidrOutOfRange {
                        cidr: net.to_string(),
                        vpc_cidr: vpc_cidr.to_string(),
                    });
                }

                // Check overlap with existing subnets
                for existing in &existing_cidrs {
                    if cidrs_overlap(&net, existing) {
                        return Err(OrgError::SubnetCidrOverlap {
                            new_cidr: net.to_string(),
                            existing_cidr: existing.to_string(),
                        });
                    }
                }

                net
            }
            None => Self::auto_allocate_subnet_cidr(&vpc_cidr, &existing_cidrs)?,
        };

        let gateway = Self::compute_gateway(&subnet_cidr);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let subnet = Subnet {
            id: SubnetId(key.clone()),
            name: name.to_string(),
            vpc_id: vpc.id.clone(),
            env_id: env_id.clone(),
            cidr: subnet_cidr.to_string(),
            gateway: gateway.to_string(),
            created_at: now,
        };

        self.db.set(SUBNETS_TABLE, &key, &subnet)?;

        // Auto-add system route for this subnet's CIDR in the VPC's default route table.
        if let Ok(Some(default_rt)) = self.get_default_route_table(&vpc.id) {
            let _ = self.add_system_route(&default_rt, &subnet.cidr);
        }

        Ok(subnet)
    }

    /// Get a subnet by VPC name and subnet name.
    pub fn get_subnet(&self, vpc_name: &str, subnet_name: &str) -> Result<Subnet> {
        let key = Self::subnet_key(vpc_name, subnet_name);
        self.db
            .get::<Subnet>(SUBNETS_TABLE, &key)?
            .ok_or_else(|| OrgError::SubnetNotFound {
                vpc: vpc_name.to_string(),
                subnet: subnet_name.to_string(),
            })
    }

    /// List all subnets in a VPC.
    pub fn list_subnets(&self, vpc_name: &str) -> Result<Vec<Subnet>> {
        let prefix = format!("{vpc_name}/");
        let all: Vec<(String, Subnet)> = self.db.list(SUBNETS_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, s)| s)
            .collect())
    }

    /// List all subnets belonging to a specific environment.
    pub fn list_subnets_by_env(&self, env_id: &EnvironmentId) -> Result<Vec<Subnet>> {
        let all: Vec<(String, Subnet)> = self.db.list(SUBNETS_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(_, s)| s.env_id == *env_id)
            .map(|(_, s)| s)
            .collect())
    }

    /// Find all subnets with a given name across all VPCs.
    ///
    /// Returns a list of `(vpc_name, Subnet)` pairs. Useful for resolving a
    /// subnet name when the VPC is not specified.
    pub fn find_subnets_by_name(&self, subnet_name: &str) -> Result<Vec<(String, Subnet)>> {
        let all: Vec<(String, Subnet)> = self.db.list(SUBNETS_TABLE)?;
        let mut matches = Vec::new();
        for (key, subnet) in all {
            if subnet.name == subnet_name {
                // key is "vpc_name/subnet_name"; extract vpc_name
                let vpc_name = key.split('/').next().unwrap_or(&key).to_string();
                matches.push((vpc_name, subnet));
            }
        }
        Ok(matches)
    }

    /// Delete a subnet by VPC name and subnet name.
    pub fn delete_subnet(&self, vpc_name: &str, subnet_name: &str) -> Result<()> {
        let key = Self::subnet_key(vpc_name, subnet_name);
        let subnet: Subnet =
            self.db
                .get(SUBNETS_TABLE, &key)?
                .ok_or_else(|| OrgError::SubnetNotFound {
                    vpc: vpc_name.to_string(),
                    subnet: subnet_name.to_string(),
                })?;

        // Remove the system route for this subnet's CIDR from the default route table.
        if let Ok(Some(default_rt)) = self.get_default_route_table(&subnet.vpc_id) {
            let _ = self.remove_route_force(&default_rt.id, &subnet.cidr);
        }

        // Remove any route table association for this subnet.
        let _ = self.disassociate_subnet_route_table(&subnet.id);

        self.db.delete(SUBNETS_TABLE, &key)?;
        Ok(())
    }

    // ── VPC Peering operations ──────────────────────────────────────

    /// Normalize a peering key by sorting the two VPC names alphabetically.
    /// This ensures "A/B" and "B/A" resolve to the same peering.
    fn peering_key(vpc_a: &str, vpc_b: &str) -> (String, String, String) {
        let (a, b) = if vpc_a <= vpc_b {
            (vpc_a.to_string(), vpc_b.to_string())
        } else {
            (vpc_b.to_string(), vpc_a.to_string())
        };
        let key = format!("{a}/{b}");
        (key, a, b)
    }

    /// Create a peering between two VPCs.
    ///
    /// Both VPCs must exist, self-peering is rejected, and duplicate
    /// peerings are rejected. The key is normalized so that order of
    /// vpc_a/vpc_b does not matter.
    pub fn create_peering(&self, vpc_a: &str, vpc_b: &str) -> Result<VpcPeering> {
        // Reject self-peering
        if vpc_a == vpc_b {
            return Err(OrgError::SelfPeeringRejected(vpc_a.to_string()));
        }

        // Verify both VPCs exist
        if !self.db.exists(VPCS_TABLE, vpc_a)? {
            return Err(OrgError::VpcNotFound(vpc_a.to_string()));
        }
        if !self.db.exists(VPCS_TABLE, vpc_b)? {
            return Err(OrgError::VpcNotFound(vpc_b.to_string()));
        }

        let (key, a, b) = Self::peering_key(vpc_a, vpc_b);

        // Reject duplicate
        if self.db.exists(PEERINGS_TABLE, &key)? {
            return Err(OrgError::PeeringAlreadyExists { vpc_a: a, vpc_b: b });
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let peering = VpcPeering {
            id: PeeringId(format!("peer-{key}")),
            vpc_a: a,
            vpc_b: b,
            status: PeeringStatus::Active,
            created_at: now,
        };

        self.db.set(PEERINGS_TABLE, &key, &peering)?;
        Ok(peering)
    }

    /// Delete (remove) a peering between two VPCs.
    pub fn delete_peering(&self, vpc_a: &str, vpc_b: &str) -> Result<()> {
        let (key, a, b) = Self::peering_key(vpc_a, vpc_b);

        if !self.db.exists(PEERINGS_TABLE, &key)? {
            return Err(OrgError::PeeringNotFound { vpc_a: a, vpc_b: b });
        }

        self.db.delete(PEERINGS_TABLE, &key)?;
        Ok(())
    }

    /// List all peerings.
    pub fn list_peerings(&self) -> Result<Vec<VpcPeering>> {
        let entries: Vec<(String, VpcPeering)> = self.db.list(PEERINGS_TABLE)?;
        Ok(entries.into_iter().map(|(_, p)| p).collect())
    }

    /// List peerings that involve a specific VPC.
    pub fn list_peerings_by_vpc(&self, vpc_name: &str) -> Result<Vec<VpcPeering>> {
        let all = self.list_peerings()?;
        Ok(all
            .into_iter()
            .filter(|p| p.vpc_a == vpc_name || p.vpc_b == vpc_name)
            .collect())
    }

    /// List active peerings filtered by a specific VPC name (alias for CLI).
    pub fn list_peerings_for_vpc(&self, vpc_name: &str) -> Result<Vec<VpcPeering>> {
        self.list_peerings_by_vpc(vpc_name)
    }

    /// Resolve a VPC name from its stored string. Since peerings now store
    /// VPC names directly, this is an identity function.
    pub fn resolve_vpc_name(&self, vpc_name: &str) -> String {
        vpc_name.to_string()
    }

    /// Get a specific peering between two VPCs, if it exists.
    pub fn get_peering(&self, vpc_a: &str, vpc_b: &str) -> Result<Option<VpcPeering>> {
        let (key, _, _) = Self::peering_key(vpc_a, vpc_b);
        Ok(self.db.get(PEERINGS_TABLE, &key)?)
    }

    // ── Security Group operations ──────────────────────────────────

    /// Build the redb key for a security group: "vpc_id/sg_name".
    fn sg_key(vpc_id: &VpcId, name: &str) -> String {
        format!("{}/{}", vpc_id.0, name)
    }

    /// Create a security group within a VPC.
    pub fn create_sg(
        &self,
        name: &str,
        vpc_id: &VpcId,
        description: Option<&str>,
    ) -> Result<SecurityGroup> {
        validate_name(name, "security group")?;

        let key = Self::sg_key(vpc_id, name);
        if self.db.exists(SECURITY_GROUPS_TABLE, &key)? {
            return Err(OrgError::SgAlreadyExists(name.to_string()));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let sg = SecurityGroup {
            id: SecurityGroupId(format!("sg-{}", name)),
            name: name.to_string(),
            vpc_id: vpc_id.clone(),
            description: description.map(|s| s.to_string()),
            is_default: false,
            state: ResourceState::Active,
            created_at: now,
        };

        self.db.set(SECURITY_GROUPS_TABLE, &key, &sg)?;
        Ok(sg)
    }

    /// Create the default security group for a VPC. Called automatically
    /// during VPC creation. The default SG cannot be deleted.
    pub fn create_default_sg(&self, vpc: &Vpc) -> Result<SecurityGroup> {
        let key = Self::sg_key(&vpc.id, "default");

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let sg = SecurityGroup {
            id: SecurityGroupId(format!("sg-default-{}", vpc.name)),
            name: "default".to_string(),
            vpc_id: vpc.id.clone(),
            description: Some(format!("Default security group for VPC {}", vpc.name)),
            is_default: true,
            state: ResourceState::Active,
            created_at: now,
        };

        self.db.set(SECURITY_GROUPS_TABLE, &key, &sg)?;
        Ok(sg)
    }

    /// Get a security group by VPC ID and name.
    pub fn get_sg(&self, vpc_id: &VpcId, name: &str) -> Result<Option<SecurityGroup>> {
        let key = Self::sg_key(vpc_id, name);
        Ok(self.db.get(SECURITY_GROUPS_TABLE, &key)?)
    }

    /// List all security groups.
    pub fn list_sgs(&self) -> Result<Vec<SecurityGroup>> {
        let entries: Vec<(String, SecurityGroup)> = self.db.list(SECURITY_GROUPS_TABLE)?;
        Ok(entries.into_iter().map(|(_, sg)| sg).collect())
    }

    /// List security groups belonging to a specific VPC.
    pub fn list_sgs_by_vpc(&self, vpc_id: &VpcId) -> Result<Vec<SecurityGroup>> {
        let prefix = format!("{}/", vpc_id.0);
        let all: Vec<(String, SecurityGroup)> = self.db.list(SECURITY_GROUPS_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, sg)| sg)
            .collect())
    }

    /// Delete a security group. Fails if the SG is the default for its VPC.
    pub fn delete_sg(&self, vpc_id: &VpcId, name: &str) -> Result<()> {
        let key = Self::sg_key(vpc_id, name);

        let sg: SecurityGroup = self
            .db
            .get(SECURITY_GROUPS_TABLE, &key)?
            .ok_or_else(|| OrgError::SgNotFound(name.to_string()))?;

        if sg.is_default {
            return Err(OrgError::SgIsDefault(name.to_string()));
        }

        self.db.delete(SECURITY_GROUPS_TABLE, &key)?;
        Ok(())
    }

    // ── Security Group CLI helpers (VPC-name based) ────────────────

    /// Create a security group by VPC name (CLI convenience wrapper).
    pub fn create_security_group(
        &self,
        name: &str,
        vpc_name: &str,
        description: &str,
    ) -> Result<SecurityGroup> {
        let vpc = match self.get_vpc(vpc_name)? {
            Some(v) => v,
            None => return Err(OrgError::NotFound(format!("VPC '{vpc_name}'"))),
        };
        self.create_sg(name, &vpc.id, Some(description))
    }

    /// List security groups, optionally filtered by VPC name.
    pub fn list_security_groups(&self, vpc_name: Option<&str>) -> Result<Vec<SecurityGroup>> {
        if let Some(vname) = vpc_name {
            let vpc = match self.get_vpc(vname)? {
                Some(v) => v,
                None => return Err(OrgError::NotFound(format!("VPC '{vname}'"))),
            };
            self.list_sgs_by_vpc(&vpc.id)
        } else {
            self.list_sgs()
        }
    }

    /// Get a security group by name. Searches all VPCs or a specific one.
    pub fn get_security_group(
        &self,
        name: &str,
        vpc_name: Option<&str>,
    ) -> Result<Option<SecurityGroup>> {
        if let Some(vname) = vpc_name {
            let vpc = match self.get_vpc(vname)? {
                Some(v) => v,
                None => return Err(OrgError::NotFound(format!("VPC '{vname}'"))),
            };
            self.get_sg(&vpc.id, name)
        } else {
            let all = self.list_sgs()?;
            let matches: Vec<SecurityGroup> =
                all.into_iter().filter(|sg| sg.name == name).collect();
            match matches.len() {
                0 => Ok(None),
                1 => Ok(Some(matches.into_iter().next().unwrap())),
                _ => Err(OrgError::Ambiguous(format!(
                    "security group '{name}' exists in multiple VPCs — specify --vpc"
                ))),
            }
        }
    }

    /// Delete a security group by name.
    pub fn delete_security_group(&self, name: &str, vpc_name: Option<&str>) -> Result<()> {
        let sg = match self.get_security_group(name, vpc_name)? {
            Some(sg) => sg,
            None => {
                return Err(OrgError::NotFound(format!("security group '{name}'")));
            }
        };

        if sg.is_default {
            return Err(OrgError::CannotDelete(
                "cannot delete the default security group".to_string(),
            ));
        }

        self.delete_sg(&sg.vpc_id, name)
    }

    // ── Route Table operations ──────────────────────────────────────

    /// Build the redb key for a route table: "vpc_id/table_name".
    fn route_table_key(vpc_id: &VpcId, name: &str) -> String {
        format!("{}/{}", vpc_id.0, name)
    }

    /// Build the redb key for a route: "table_id/destination".
    fn route_key(table_id: &RouteTableId, destination: &str) -> String {
        format!("{}/{}", table_id.0, destination)
    }

    /// Create the default route table for a VPC. Called automatically
    /// during VPC creation.
    pub fn create_default_route_table(&self, vpc: &Vpc) -> Result<RouteTable> {
        let key = Self::route_table_key(&vpc.id, "default");

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let table = RouteTable {
            id: RouteTableId(format!("rtb-default-{}", vpc.name)),
            name: "default".to_string(),
            vpc_id: vpc.id.clone(),
            is_default: true,
            state: ResourceState::Active,
            created_at: now,
        };

        self.db.set(ROUTE_TABLES_TABLE, &key, &table)?;

        // Add the VPC CIDR local route as a system route.
        self.add_system_route(&table, &vpc.cidr)?;

        Ok(table)
    }

    /// Create a named route table within a VPC.
    pub fn create_route_table(&self, name: &str, vpc_id: &VpcId) -> Result<RouteTable> {
        validate_name(name, "route table")?;

        let key = Self::route_table_key(vpc_id, name);
        if self.db.exists(ROUTE_TABLES_TABLE, &key)? {
            return Err(OrgError::RouteTableAlreadyExists(name.to_string()));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let table = RouteTable {
            id: RouteTableId(format!("rtb-{name}")),
            name: name.to_string(),
            vpc_id: vpc_id.clone(),
            is_default: false,
            state: ResourceState::Active,
            created_at: now,
        };

        self.db.set(ROUTE_TABLES_TABLE, &key, &table)?;
        Ok(table)
    }

    /// Get a route table by VPC ID and name.
    pub fn get_route_table(&self, vpc_id: &VpcId, name: &str) -> Result<Option<RouteTable>> {
        let key = Self::route_table_key(vpc_id, name);
        Ok(self.db.get(ROUTE_TABLES_TABLE, &key)?)
    }

    /// Get the default route table for a VPC.
    pub fn get_default_route_table(&self, vpc_id: &VpcId) -> Result<Option<RouteTable>> {
        self.get_route_table(vpc_id, "default")
    }

    /// List all route tables.
    pub fn list_route_tables(&self) -> Result<Vec<RouteTable>> {
        let entries: Vec<(String, RouteTable)> = self.db.list(ROUTE_TABLES_TABLE)?;
        Ok(entries.into_iter().map(|(_, t)| t).collect())
    }

    /// List route tables belonging to a specific VPC.
    pub fn list_route_tables_by_vpc(&self, vpc_id: &VpcId) -> Result<Vec<RouteTable>> {
        let prefix = format!("{}/", vpc_id.0);
        let all: Vec<(String, RouteTable)> = self.db.list(ROUTE_TABLES_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, t)| t)
            .collect())
    }

    /// Delete a route table. Fails if it is the default table or has associated subnets.
    pub fn delete_route_table(&self, vpc_id: &VpcId, name: &str) -> Result<()> {
        let key = Self::route_table_key(vpc_id, name);

        let table: RouteTable = self
            .db
            .get(ROUTE_TABLES_TABLE, &key)?
            .ok_or_else(|| OrgError::RouteTableNotFound(name.to_string()))?;

        if table.is_default {
            return Err(OrgError::CannotDeleteDefaultRouteTable);
        }

        // Check if any subnets are associated with this table.
        let assoc_count = self.count_subnets_for_route_table(&table.id)?;
        if assoc_count > 0 {
            return Err(OrgError::RouteTableHasSubnets {
                name: name.to_string(),
                count: assoc_count,
            });
        }

        // Delete all routes in this table.
        let routes = self.list_routes_by_table(&table.id)?;
        for route in &routes {
            let rkey = Self::route_key(&table.id, &route.destination);
            self.db.delete(ROUTES_TABLE, &rkey)?;
        }

        self.db.delete(ROUTE_TABLES_TABLE, &key)?;
        Ok(())
    }

    /// Delete a route table by VPC name (CLI convenience wrapper).
    pub fn delete_route_table_by_vpc_name(&self, vpc_name: &str, table_name: &str) -> Result<()> {
        let vpc = self
            .get_vpc(vpc_name)?
            .ok_or_else(|| OrgError::VpcNotFound(vpc_name.to_string()))?;
        self.delete_route_table(&vpc.id, table_name)
    }

    /// Create a route table by VPC name (CLI convenience wrapper).
    pub fn create_route_table_by_vpc_name(&self, name: &str, vpc_name: &str) -> Result<RouteTable> {
        let vpc = self
            .get_vpc(vpc_name)?
            .ok_or_else(|| OrgError::VpcNotFound(vpc_name.to_string()))?;
        self.create_route_table(name, &vpc.id)
    }

    /// List route tables by VPC name (CLI convenience wrapper).
    pub fn list_route_tables_by_vpc_name(&self, vpc_name: Option<&str>) -> Result<Vec<RouteTable>> {
        match vpc_name {
            Some(vname) => {
                let vpc = self
                    .get_vpc(vname)?
                    .ok_or_else(|| OrgError::VpcNotFound(vname.to_string()))?;
                self.list_route_tables_by_vpc(&vpc.id)
            }
            None => self.list_route_tables(),
        }
    }

    /// Count subnets associated with a specific route table.
    fn count_subnets_for_route_table(&self, table_id: &RouteTableId) -> Result<usize> {
        let entries: Vec<(String, String)> = self.db.list(SUBNET_ROUTE_ASSOC_TABLE)?;
        Ok(entries
            .into_iter()
            .filter(|(_, tid)| *tid == table_id.0)
            .count())
    }

    // ── Route operations ──────────────────────────────────────────────

    /// Add a system route (auto-created, undeletable).
    pub fn add_system_route(&self, table: &RouteTable, destination: &str) -> Result<Route> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let route = Route {
            id: RouteId(format!(
                "rt-sys-{}-{}",
                table.name,
                destination.replace('/', "_")
            )),
            route_table_id: table.id.clone(),
            destination: destination.to_string(),
            target: RouteTarget::Local,
            origin: RouteOrigin::System,
            status: RouteStatus::Active,
            priority: 0,
            created_at: now,
        };

        let key = Self::route_key(&table.id, destination);
        self.db.set(ROUTES_TABLE, &key, &route)?;
        Ok(route)
    }

    /// Add a user-created route.
    pub fn add_route(
        &self,
        table_id: &RouteTableId,
        destination: &str,
        target: RouteTarget,
        priority: Option<u32>,
    ) -> Result<Route> {
        let key = Self::route_key(table_id, destination);
        if self.db.exists(ROUTES_TABLE, &key)? {
            return Err(OrgError::RouteAlreadyExists(destination.to_string()));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let status = match &target {
            RouteTarget::Blackhole => RouteStatus::Active,
            RouteTarget::Local => RouteStatus::Active,
            _ => RouteStatus::Active,
        };

        let route = Route {
            id: RouteId(format!("rt-{}", destination.replace('/', "_"))),
            route_table_id: table_id.clone(),
            destination: destination.to_string(),
            target,
            origin: RouteOrigin::User,
            status,
            priority: priority.unwrap_or(100),
            created_at: now,
        };

        self.db.set(ROUTES_TABLE, &key, &route)?;
        Ok(route)
    }

    /// Add a propagated route (auto-created from peering).
    pub fn add_propagated_route(
        &self,
        table_id: &RouteTableId,
        destination: &str,
        target: RouteTarget,
    ) -> Result<Route> {
        let key = Self::route_key(table_id, destination);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let route = Route {
            id: RouteId(format!("rt-prop-{}", destination.replace('/', "_"))),
            route_table_id: table_id.clone(),
            destination: destination.to_string(),
            target,
            origin: RouteOrigin::Propagated,
            status: RouteStatus::Active,
            priority: 50,
            created_at: now,
        };

        self.db.set(ROUTES_TABLE, &key, &route)?;
        Ok(route)
    }

    /// Remove a route. Fails if the route is system or propagated origin.
    pub fn remove_route(&self, table_id: &RouteTableId, destination: &str) -> Result<()> {
        let key = Self::route_key(table_id, destination);

        let route: Route = self
            .db
            .get(ROUTES_TABLE, &key)?
            .ok_or_else(|| OrgError::RouteNotFound(destination.to_string()))?;

        match route.origin {
            RouteOrigin::System => {
                return Err(OrgError::CannotDeleteSystemRoute);
            }
            RouteOrigin::Propagated => {
                return Err(OrgError::CannotDeletePropagatedRoute);
            }
            RouteOrigin::User => {}
        }

        self.db.delete(ROUTES_TABLE, &key)?;
        Ok(())
    }

    /// Force-remove a route regardless of origin (used internally for cleanup).
    pub fn remove_route_force(&self, table_id: &RouteTableId, destination: &str) -> Result<()> {
        let key = Self::route_key(table_id, destination);
        self.db.delete(ROUTES_TABLE, &key)?;
        Ok(())
    }

    /// List all routes in a specific route table.
    pub fn list_routes_by_table(&self, table_id: &RouteTableId) -> Result<Vec<Route>> {
        let prefix = format!("{}/", table_id.0);
        let all: Vec<(String, Route)> = self.db.list(ROUTES_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, r)| r)
            .collect())
    }

    /// List all routes across all tables in a VPC.
    pub fn list_routes_by_vpc(&self, vpc_id: &VpcId) -> Result<Vec<Route>> {
        let tables = self.list_route_tables_by_vpc(vpc_id)?;
        let mut routes = Vec::new();
        for table in &tables {
            routes.extend(self.list_routes_by_table(&table.id)?);
        }
        Ok(routes)
    }

    /// Get a specific route by table ID and destination.
    pub fn get_route(&self, table_id: &RouteTableId, destination: &str) -> Result<Option<Route>> {
        let key = Self::route_key(table_id, destination);
        Ok(self.db.get(ROUTES_TABLE, &key)?)
    }

    /// Update a route's status field.
    pub fn update_route_status(
        &self,
        table_id: &RouteTableId,
        destination: &str,
        status: RouteStatus,
    ) -> Result<()> {
        let key = Self::route_key(table_id, destination);
        let mut route: Route = self
            .db
            .get(ROUTES_TABLE, &key)?
            .ok_or_else(|| OrgError::RouteNotFound(destination.to_string()))?;
        route.status = status;
        self.db.set(ROUTES_TABLE, &key, &route)?;
        Ok(())
    }

    // ── Subnet-RouteTable association ──────────────────────────────────

    /// Associate a subnet with a route table.
    pub fn associate_subnet_route_table(
        &self,
        subnet_id: &SubnetId,
        table_id: &RouteTableId,
    ) -> Result<()> {
        self.db
            .set(SUBNET_ROUTE_ASSOC_TABLE, &subnet_id.0, &table_id.0)?;
        Ok(())
    }

    /// Disassociate a subnet from its custom route table (reverts to default).
    pub fn disassociate_subnet_route_table(&self, subnet_id: &SubnetId) -> Result<()> {
        self.db.delete(SUBNET_ROUTE_ASSOC_TABLE, &subnet_id.0)?;
        Ok(())
    }

    /// Get the route table ID for a subnet (None means use default).
    pub fn get_subnet_route_table_id(&self, subnet_id: &SubnetId) -> Result<Option<RouteTableId>> {
        let val: Option<String> = self.db.get(SUBNET_ROUTE_ASSOC_TABLE, &subnet_id.0)?;
        Ok(val.map(RouteTableId))
    }

    /// Resolve the effective route table for a subnet — explicit association or VPC default.
    pub fn resolve_subnet_route_table(&self, subnet: &Subnet) -> Result<RouteTable> {
        if let Some(table_id) = self.get_subnet_route_table_id(&subnet.id)? {
            // Find the table by scanning (table_id is the value, not key).
            let tables = self.list_route_tables_by_vpc(&subnet.vpc_id)?;
            for t in tables {
                if t.id == table_id {
                    return Ok(t);
                }
            }
        }
        // Fall back to default.
        self.get_default_route_table(&subnet.vpc_id)?
            .ok_or_else(|| OrgError::RouteTableNotFound("default".to_string()))
    }

    // ── NAT Gateway operations ─────────────────────────────────────

    /// Key format for NAT gateway entries: "vpc_id/name".
    fn nat_gw_key(vpc_id: &VpcId, name: &str) -> String {
        format!("{}/{name}", vpc_id.0)
    }

    /// Create a new NAT Gateway. Starts in Pending state.
    pub fn create_nat_gw(
        &self,
        name: &str,
        vpc_id: &VpcId,
        subnet_id: &SubnetId,
        public_ip: &str,
    ) -> Result<NatGateway> {
        validate_name(name, "nat-gw")?;

        let key = Self::nat_gw_key(vpc_id, name);
        if self.db.exists(NAT_GATEWAYS_TABLE, &key)? {
            return Err(OrgError::NatGwAlreadyExists(name.to_string()));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let gw = NatGateway {
            id: NatGatewayId(format!("nat-{name}")),
            name: name.to_string(),
            vpc_id: vpc_id.clone(),
            subnet_id: subnet_id.clone(),
            public_ip: public_ip.to_string(),
            state: ResourceState::Pending,
            created_at: now,
        };

        self.db.set(NAT_GATEWAYS_TABLE, &key, &gw)?;
        Ok(gw)
    }

    /// Get a NAT Gateway by VPC ID and name.
    pub fn get_nat_gw(&self, vpc_id: &VpcId, name: &str) -> Result<Option<NatGateway>> {
        let key = Self::nat_gw_key(vpc_id, name);
        Ok(self.db.get(NAT_GATEWAYS_TABLE, &key)?)
    }

    /// Get a NAT Gateway by name alone (scans all VPCs). Returns error if ambiguous.
    pub fn get_nat_gw_by_name(&self, name: &str) -> Result<Option<NatGateway>> {
        let all = self.list_nat_gws()?;
        let matches: Vec<_> = all.into_iter().filter(|g| g.name == name).collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches.into_iter().next().unwrap())),
            n => Err(OrgError::Ambiguous(format!(
                "nat-gw name '{name}' exists in {n} VPCs — specify --vpc"
            ))),
        }
    }

    /// List all NAT Gateways.
    pub fn list_nat_gws(&self) -> Result<Vec<NatGateway>> {
        let entries: Vec<(String, NatGateway)> = self.db.list(NAT_GATEWAYS_TABLE)?;
        Ok(entries.into_iter().map(|(_, g)| g).collect())
    }

    /// List NAT Gateways belonging to a specific VPC.
    pub fn list_nat_gws_by_vpc(&self, vpc_id: &VpcId) -> Result<Vec<NatGateway>> {
        let prefix = format!("{}/", vpc_id.0);
        let all: Vec<(String, NatGateway)> = self.db.list(NAT_GATEWAYS_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, g)| g)
            .collect())
    }

    /// List NAT Gateways by VPC name (CLI convenience wrapper).
    pub fn list_nat_gws_by_vpc_name(&self, vpc_name: Option<&str>) -> Result<Vec<NatGateway>> {
        match vpc_name {
            Some(vname) => {
                let vpc = self
                    .get_vpc(vname)?
                    .ok_or_else(|| OrgError::VpcNotFound(vname.to_string()))?;
                self.list_nat_gws_by_vpc(&vpc.id)
            }
            None => self.list_nat_gws(),
        }
    }

    /// Update the state of a NAT Gateway.
    pub fn update_nat_gw_state(
        &self,
        vpc_id: &VpcId,
        name: &str,
        state: ResourceState,
    ) -> Result<NatGateway> {
        let key = Self::nat_gw_key(vpc_id, name);
        let mut gw: NatGateway = self
            .db
            .get(NAT_GATEWAYS_TABLE, &key)?
            .ok_or_else(|| OrgError::NatGwNotFound(name.to_string()))?;
        gw.state = state;
        self.db.set(NAT_GATEWAYS_TABLE, &key, &gw)?;
        Ok(gw)
    }

    /// Delete a NAT Gateway record from the store.
    pub fn delete_nat_gw(&self, vpc_id: &VpcId, name: &str) -> Result<()> {
        let key = Self::nat_gw_key(vpc_id, name);
        if !self.db.exists(NAT_GATEWAYS_TABLE, &key)? {
            return Err(OrgError::NatGwNotFound(name.to_string()));
        }
        self.db.delete(NAT_GATEWAYS_TABLE, &key)?;
        Ok(())
    }

    /// Check if any routes in the VPC reference the given NAT Gateway name.
    pub fn routes_referencing_nat_gw(
        &self,
        vpc_id: &VpcId,
        nat_gw_name: &str,
    ) -> Result<Vec<Route>> {
        let routes = self.list_routes_by_vpc(vpc_id)?;
        Ok(routes
            .into_iter()
            .filter(|r| matches!(&r.target, RouteTarget::NatGateway(n) if n == nat_gw_name))
            .collect())
    }

    // ── NIC operations (convenience wrappers for attach/detach) ────

    /// List NICs that have a specific security group attached.
    pub fn list_nics_by_sg(&self, sg_id: &SecurityGroupId) -> Result<Vec<NetworkInterface>> {
        let entries: Vec<(String, NetworkInterface)> = self.db.list(NICS_TABLE)?;
        Ok(entries
            .into_iter()
            .filter(|(_, nic)| nic.security_groups.contains(sg_id))
            .map(|(_, nic)| nic)
            .collect())
    }

    /// Create a NIC in the store.
    pub fn create_nic(&self, nic: &NetworkInterface) -> Result<()> {
        if self.db.exists(NICS_TABLE, &nic.id.0)? {
            return Err(OrgError::NicAlreadyExists(nic.id.0.clone()));
        }
        self.db.set(NICS_TABLE, &nic.id.0, nic)?;
        Ok(())
    }

    /// Get a NIC by its ID.
    pub fn get_nic(&self, nic_id: &str) -> Result<Option<NetworkInterface>> {
        Ok(self.db.get(NICS_TABLE, nic_id)?)
    }

    /// Delete a NIC by its ID.
    pub fn delete_nic(&self, nic_id: &str) -> Result<()> {
        let existed = self.db.delete(NICS_TABLE, nic_id)?;
        if !existed {
            return Err(OrgError::NicNotFound(nic_id.to_string()));
        }
        Ok(())
    }

    /// List all NICs in a given VPC.
    pub fn list_nics_by_vpc(&self, vpc_id: &str) -> Result<Vec<NetworkInterface>> {
        let entries: Vec<(String, NetworkInterface)> = self.db.list(NICS_TABLE)?;
        Ok(entries
            .into_iter()
            .filter(|(_, nic)| nic.vpc_id == vpc_id && nic.state != ResourceState::Deleted)
            .map(|(_, nic)| nic)
            .collect())
    }

    /// Find the primary NIC for a given VM.
    pub fn find_nic_by_vm(&self, vm_id: &str) -> Result<Option<NetworkInterface>> {
        let entries: Vec<(String, NetworkInterface)> = self.db.list(NICS_TABLE)?;
        Ok(entries
            .into_iter()
            .find(|(_, nic)| nic.vm_id.as_deref() == Some(vm_id))
            .map(|(_, nic)| nic))
    }

    /// Find a security group by name across all VPCs.
    pub fn find_sg_by_name(&self, name: &str) -> Result<Option<SecurityGroup>> {
        let all = self.list_sgs()?;
        let matches: Vec<SecurityGroup> = all.into_iter().filter(|sg| sg.name == name).collect();
        match matches.len() {
            0 => Ok(None),
            _ => Ok(Some(matches.into_iter().next().unwrap())),
        }
    }

    /// Attach a security group to a NIC. Validates VPC match.
    pub fn attach_sg_to_nic(&self, sg_key: &str, nic_id: &str) -> Result<NetworkInterface> {
        let sg: SecurityGroup = self
            .db
            .get(SECURITY_GROUPS_TABLE, sg_key)?
            .ok_or_else(|| OrgError::SgNotFound(sg_key.to_string()))?;

        let mut nic: NetworkInterface = self
            .db
            .get(NICS_TABLE, nic_id)?
            .ok_or_else(|| OrgError::NicNotFound(nic_id.to_string()))?;

        if sg.vpc_id.0 != nic.vpc_id {
            return Err(OrgError::SgVpcMismatch {
                sg: sg.name.clone(),
                sg_vpc: sg.vpc_id.0.clone(),
                nic_vpc: nic.vpc_id.clone(),
            });
        }

        if nic.security_groups.contains(&sg.id) {
            return Err(OrgError::SgAlreadyAttached {
                sg: sg.name.clone(),
                nic: nic.name.clone(),
            });
        }

        nic.security_groups.push(sg.id);
        self.db.set(NICS_TABLE, nic_id, &nic)?;
        Ok(nic)
    }

    /// Detach a security group from a NIC.
    pub fn detach_sg_from_nic(&self, sg_key: &str, nic_id: &str) -> Result<NetworkInterface> {
        let sg: SecurityGroup = self
            .db
            .get(SECURITY_GROUPS_TABLE, sg_key)?
            .ok_or_else(|| OrgError::SgNotFound(sg_key.to_string()))?;

        let mut nic: NetworkInterface = self
            .db
            .get(NICS_TABLE, nic_id)?
            .ok_or_else(|| OrgError::NicNotFound(nic_id.to_string()))?;

        if !nic.security_groups.contains(&sg.id) {
            return Err(OrgError::SgNotAttached {
                sg: sg.name.clone(),
                nic: nic.name.clone(),
            });
        }

        if nic.security_groups.len() <= 1 {
            return Err(OrgError::LastSgDetach {
                nic: nic.name.clone(),
            });
        }

        nic.security_groups.retain(|id| id != &sg.id);
        self.db.set(NICS_TABLE, nic_id, &nic)?;
        Ok(nic)
    }

    /// List security groups attached to a NIC.
    ///
    /// The NIC stores `SecurityGroupId` values (e.g. `"sg-web-sg"`), but the
    /// SECURITY_GROUPS_TABLE is keyed by `"vpc_id/sg_name"`. We therefore scan
    /// all SGs and match by ID rather than using `sg_id.0` as a DB key.
    pub fn list_sgs_for_nic(&self, nic_id: &str) -> Result<Vec<SecurityGroup>> {
        let nic: NetworkInterface = self
            .db
            .get(NICS_TABLE, nic_id)?
            .ok_or_else(|| OrgError::NicNotFound(nic_id.to_string()))?;

        let all_sgs = self.list_sgs()?;
        let sgs: Vec<SecurityGroup> = all_sgs
            .into_iter()
            .filter(|sg| nic.security_groups.contains(&sg.id))
            .collect();
        Ok(sgs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, OrgStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("org-test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, OrgStore::new(db))
    }

    /// Helper: create an org and project so env tests can focus on environments.
    fn setup_org_and_project(store: &OrgStore) {
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
    }

    // ── Org tests ───────────────────────────────────────────────────

    #[test]
    fn create_org() {
        let (_dir, store) = temp_store();
        let org = store.create("acme").unwrap();
        assert_eq!(org.name, "acme");
        assert_eq!(org.id.0, "org-acme");
        assert!(org.created_at > 0);
    }

    #[test]
    fn duplicate_name_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        let err = store.create("acme").unwrap_err();
        assert!(matches!(err, OrgError::AlreadyExists(_)));
    }

    #[test]
    fn invalid_name_rejected() {
        let (_dir, store) = temp_store();

        assert!(matches!(
            store.create("my org").unwrap_err(),
            OrgError::InvalidName { .. }
        ));
        assert!(matches!(
            store.create("Acme").unwrap_err(),
            OrgError::InvalidName { .. }
        ));
        assert!(matches!(
            store.create("org@1").unwrap_err(),
            OrgError::InvalidName { .. }
        ));
        assert!(matches!(
            store.create("ab").unwrap_err(),
            OrgError::InvalidName { .. }
        ));
        assert!(matches!(
            store.create(&"x".repeat(64)).unwrap_err(),
            OrgError::InvalidName { .. }
        ));
    }

    #[test]
    fn delete_org() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.delete("acme").unwrap();
        assert!(store.get("acme").unwrap().is_none());
    }

    #[test]
    fn delete_org_with_org_vpc_fails() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store
            .create_vpc(
                "shared-net",
                "10.0.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();
        let err = store.delete("acme").unwrap_err();
        assert!(matches!(err, OrgError::OrgHasVpcs { count: 1, .. }));
    }

    #[test]
    fn delete_org_with_project_vpc_fails() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
        store
            .create_vpc(
                "proj-net",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();
        let err = store.delete("acme").unwrap_err();
        assert!(matches!(err, OrgError::OrgHasVpcs { count: 1, .. }));
    }

    #[test]
    fn delete_nonexistent_fails() {
        let (_dir, store) = temp_store();
        let err = store.delete("ghost").unwrap_err();
        assert!(matches!(err, OrgError::NotFound(_)));
    }

    #[test]
    fn list_orgs() {
        let (_dir, store) = temp_store();
        store.create("alpha").unwrap();
        store.create("beta").unwrap();
        store.create("gamma").unwrap();

        let orgs = store.list().unwrap();
        assert_eq!(orgs.len(), 3);
    }

    #[test]
    fn get_nonexistent() {
        let (_dir, store) = temp_store();
        assert!(store.get("does-not-exist").unwrap().is_none());
    }

    // ── Project tests ───────────────────────────────────────────────

    #[test]
    fn create_project_succeeds_with_valid_org() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();

        let project = store.create_project("acme", "backend").unwrap();
        assert_eq!(project.name, "backend");
        assert_eq!(project.org_id, OrgId("acme".to_string()));

        let fetched = store.get_project("acme", "backend").unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().name, "backend");
    }

    #[test]
    fn duplicate_project_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        let err = store.create_project("acme", "backend").unwrap_err();
        assert!(matches!(err, OrgError::ProjectAlreadyExists { .. }));
    }

    #[test]
    fn delete_project_succeeds_when_empty() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        store.delete_project("acme", "backend").unwrap();
        assert!(store.get_project("acme", "backend").unwrap().is_none());
    }

    #[test]
    fn org_with_projects_cannot_be_deleted() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        let err = store.delete("acme").unwrap_err();
        assert!(matches!(err, OrgError::OrgHasProjects(_)));
    }

    #[test]
    fn create_project_fails_without_org() {
        let (_dir, store) = temp_store();
        let err = store.create_project("nonexistent", "backend").unwrap_err();
        assert!(matches!(err, OrgError::NotFound(_)));
    }

    // ── Environment tests ───────────────────────────────────────────

    #[test]
    fn create_env_basic() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let env = store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();

        assert_eq!(env.name, "staging");
        assert_eq!(env.project_id.0, "acme/backend");
        assert!(env.created_at > 0);
        assert_eq!(env.ttl, None);
        assert!(!env.deletion_protection);
        assert!(env.labels.is_empty());
        assert_eq!(env.expires_at, None);
    }

    #[test]
    fn duplicate_env_rejected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();

        let err = store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap_err();

        assert!(matches!(err, OrgError::EnvAlreadyExists(ref n) if n == "staging"));
    }

    #[test]
    fn with_ttl() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let ttl_seconds = 3600;
        let env = store
            .create_env(
                "acme",
                "backend",
                "ephemeral",
                Some(ttl_seconds),
                false,
                HashMap::new(),
            )
            .unwrap();

        assert_eq!(env.ttl, Some(ttl_seconds));
        assert!(env.expires_at.is_some());
        assert_eq!(env.expires_at.unwrap(), env.created_at + ttl_seconds);
    }

    #[test]
    fn with_labels() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let mut labels = HashMap::new();
        labels.insert("region".to_string(), "eu-west".to_string());
        labels.insert("team".to_string(), "payments".to_string());

        let env = store
            .create_env("acme", "backend", "production", None, false, labels.clone())
            .unwrap();

        assert_eq!(env.labels, labels);

        let retrieved = store.get_env("acme", "backend", "production").unwrap();
        assert_eq!(retrieved.labels, labels);
    }

    #[test]
    fn with_deletion_protection() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let env = store
            .create_env("acme", "backend", "production", None, true, HashMap::new())
            .unwrap();

        assert!(env.deletion_protection);

        let retrieved = store.get_env("acme", "backend", "production").unwrap();
        assert!(retrieved.deletion_protection);
    }

    #[test]
    fn delete_env_succeeds_when_not_protected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();

        store.delete_env("acme", "backend", "staging").unwrap();

        let err = store.get_env("acme", "backend", "staging").unwrap_err();
        assert!(matches!(err, OrgError::EnvNotFound(_)));
    }

    #[test]
    fn delete_env_fails_when_protected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store
            .create_env("acme", "backend", "production", None, true, HashMap::new())
            .unwrap();

        let err = store
            .delete_env("acme", "backend", "production")
            .unwrap_err();
        assert!(matches!(err, OrgError::EnvProtected(ref n) if n == "production"));
    }

    #[test]
    fn list_by_project() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store.create_project("acme", "frontend").unwrap();

        store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();
        store
            .create_env("acme", "backend", "production", None, true, HashMap::new())
            .unwrap();
        store
            .create_env("acme", "frontend", "staging", None, false, HashMap::new())
            .unwrap();

        let backend_envs = store.list_envs("acme", "backend").unwrap();
        assert_eq!(backend_envs.len(), 2);

        let frontend_envs = store.list_envs("acme", "frontend").unwrap();
        assert_eq!(frontend_envs.len(), 1);
        assert_eq!(frontend_envs[0].name, "staging");
    }

    #[test]
    fn create_env_requires_project() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();

        let err = store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap_err();
        assert!(matches!(err, OrgError::ProjectNotFound { .. }));
    }

    #[test]
    fn delete_env_not_found() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let err = store
            .delete_env("acme", "backend", "nonexistent")
            .unwrap_err();
        assert!(matches!(err, OrgError::EnvNotFound(_)));
    }

    #[test]
    fn delete_project_with_envs_rejected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();

        let err = store.delete_project("acme", "backend").unwrap_err();
        assert!(matches!(err, OrgError::ProjectHasEnvironments { .. }));
    }

    // ── VPC tests ───────────────────────────────────────────────────

    #[test]
    fn create_vpc_succeeds() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "default",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        assert_eq!(vpc.name, "default");
        assert_eq!(vpc.id.0, "vpc-default");
        assert_eq!(vpc.cidr, "10.1.0.0/16");
        assert_eq!(vpc.vni, 100);
        assert!(!vpc.shared);
        assert!(vpc.created_at > 0);

        let fetched = store.get_vpc("default").unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().name, "default");
    }

    #[test]
    fn vni_increments() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc1 = store
            .create_vpc(
                "vpc-one",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();
        let vpc2 = store
            .create_vpc(
                "vpc-two",
                "10.2.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        assert_eq!(vpc1.vni, 100);
        assert_eq!(vpc2.vni, 101);
    }

    #[test]
    fn vni_unique_and_sequential_for_five_vpcs() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);

        let mut vnis = Vec::new();
        for i in 0..5u8 {
            let vpc = store
                .create_vpc(
                    &format!("vpc-{i}"),
                    &format!("10.{i}.0.0/16"),
                    VpcOwner::Project(ProjectId("acme/backend".to_string())),
                    false,
                )
                .unwrap();
            vnis.push(vpc.vni);
        }

        assert_eq!(vnis, vec![100, 101, 102, 103, 104]);
    }

    #[test]
    fn duplicate_vpc_name_rejected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        store
            .create_vpc(
                "default",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let err = store
            .create_vpc(
                "default",
                "10.2.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::VpcAlreadyExists(_)));
    }

    #[test]
    fn delete_vpc_succeeds() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        store
            .create_vpc(
                "default",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        store.delete_vpc("default").unwrap();
        assert!(store.get_vpc("default").unwrap().is_none());
    }

    #[test]
    fn delete_vpc_not_found() {
        let (_dir, store) = temp_store();
        let err = store.delete_vpc("ghost").unwrap_err();
        assert!(matches!(err, OrgError::VpcNotFound(_)));
    }

    #[test]
    fn list_vpcs_returns_all() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
        store.create_project("acme", "frontend").unwrap();
        store
            .create_vpc(
                "vpc-one",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();
        store
            .create_vpc(
                "vpc-two",
                "10.2.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();
        store
            .create_vpc(
                "vpc-three",
                "10.3.0.0/16",
                VpcOwner::Project(ProjectId("acme/frontend".to_string())),
                false,
            )
            .unwrap();

        let all = store.list_vpcs().unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn list_vpcs_by_project_filters() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
        store.create_project("acme", "frontend").unwrap();
        let pid = ProjectId("acme/backend".to_string());

        store
            .create_vpc(
                "vpc-one",
                "10.1.0.0/16",
                VpcOwner::Project(pid.clone()),
                false,
            )
            .unwrap();
        store
            .create_vpc(
                "vpc-two",
                "10.2.0.0/16",
                VpcOwner::Project(ProjectId("acme/frontend".to_string())),
                false,
            )
            .unwrap();
        store
            .create_vpc(
                "vpc-shared",
                "10.100.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();

        let by_project = store.list_vpcs_by_project(&pid).unwrap();
        assert_eq!(by_project.len(), 1);
        assert_eq!(by_project[0].name, "vpc-one");
    }

    #[test]
    fn list_vpcs_by_org_filters() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
        let oid = OrgId("acme".to_string());

        store
            .create_vpc(
                "vpc-one",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();
        store
            .create_vpc(
                "vpc-shared",
                "10.100.0.0/16",
                VpcOwner::Org(oid.clone()),
                true,
            )
            .unwrap();

        let by_org = store.list_vpcs_by_org(&oid).unwrap();
        assert_eq!(by_org.len(), 2);
        let names: Vec<&str> = by_org.iter().map(|v| v.name.as_str()).collect();
        assert!(
            names.contains(&"vpc-one"),
            "should include project-scoped VPC"
        );
        assert!(
            names.contains(&"vpc-shared"),
            "should include org-level VPC"
        );
    }

    #[test]
    fn invalid_cidr_rejected() {
        let (_dir, store) = temp_store();

        let err = store
            .create_vpc(
                "bad-cidr",
                "not-a-cidr",
                VpcOwner::Project(ProjectId("x".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::InvalidCidr(_)));

        let err = store
            .create_vpc(
                "bad-cidr",
                "10.0.0/16",
                VpcOwner::Project(ProjectId("x".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::InvalidCidr(_)));

        let err = store
            .create_vpc(
                "bad-cidr",
                "10.0.0.0/33",
                VpcOwner::Project(ProjectId("x".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::InvalidCidr(_)));
    }

    #[test]
    fn non_private_cidr_rejected() {
        let (_dir, store) = temp_store();

        let err = store
            .create_vpc(
                "pub-vpc",
                "8.8.8.0/24",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::InvalidCidr(_)));

        let err = store
            .create_vpc(
                "pub-vpc",
                "1.0.0.0/8",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::InvalidCidr(_)));
    }

    #[test]
    fn extreme_prefix_rejected() {
        let (_dir, store) = temp_store();

        // Too small (< 8)
        let err = store
            .create_vpc(
                "huge-vpc",
                "10.0.0.0/7",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::InvalidCidr(_)));

        // Too large (> 28)
        let err = store
            .create_vpc(
                "tiny-vpc",
                "10.0.0.0/29",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::InvalidCidr(_)));
    }

    #[test]
    fn overlapping_cidr_in_same_org_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();

        store
            .create_vpc(
                "vpc-one",
                "10.1.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                false,
            )
            .unwrap();

        let err = store
            .create_vpc(
                "vpc-two",
                "10.1.0.0/24",
                VpcOwner::Org(OrgId("acme".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::CidrOverlap { .. }));
    }

    #[test]
    fn same_cidr_different_orgs_ok() {
        let (_dir, store) = temp_store();
        store.create("alpha").unwrap();
        store.create("beta").unwrap();

        store
            .create_vpc(
                "vpc-alpha",
                "10.1.0.0/16",
                VpcOwner::Org(OrgId("alpha".to_string())),
                false,
            )
            .unwrap();

        let vpc2 = store
            .create_vpc(
                "vpc-beta",
                "10.1.0.0/16",
                VpcOwner::Org(OrgId("beta".to_string())),
                false,
            )
            .unwrap();
        assert_eq!(vpc2.cidr, "10.1.0.0/16");
    }

    #[test]
    fn project_cidr_overlap_within_org_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();

        store
            .create_vpc(
                "proj-vpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let err = store
            .create_vpc(
                "org-vpc",
                "10.1.5.0/24",
                VpcOwner::Org(OrgId("acme".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::CidrOverlap { .. }));
    }

    #[test]
    fn create_vpc_fails_when_org_not_found() {
        let (_dir, store) = temp_store();
        let err = store
            .create_vpc(
                "my-vpc",
                "10.1.0.0/16",
                VpcOwner::Org(OrgId("nonexistent".to_string())),
                true,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::NotFound(ref name) if name == "nonexistent"));
    }

    #[test]
    fn create_vpc_fails_when_project_org_not_found() {
        let (_dir, store) = temp_store();
        let err = store
            .create_vpc(
                "my-vpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("ghost-org/backend".to_string())),
                false,
            )
            .unwrap_err();
        assert!(matches!(err, OrgError::NotFound(ref name) if name == "ghost-org"));
    }

    #[test]
    fn create_vpc_fails_when_project_not_found() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        let err = store
            .create_vpc(
                "my-vpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/nonexistent".to_string())),
                false,
            )
            .unwrap_err();
        assert!(
            matches!(err, OrgError::ProjectNotFound { ref org, ref project } if org == "acme" && project == "nonexistent")
        );
    }

    // ── VPC attachment tests ─────────────────────────────────────────

    #[test]
    fn attach_vpc_succeeds() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store
            .create_vpc(
                "shared-vpc",
                "10.100.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();

        store.attach_vpc("shared-vpc", "acme/backend").unwrap();
        let attachments = store.list_attachments("shared-vpc").unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].0, "acme/backend");
    }

    #[test]
    fn attach_non_shared_vpc_rejected() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        store
            .create_vpc(
                "private-vpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let err = store
            .attach_vpc("private-vpc", "acme/frontend")
            .unwrap_err();
        assert!(matches!(err, OrgError::VpcNotShared(_)));
    }

    #[test]
    fn attach_duplicate_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store
            .create_vpc(
                "shared-vpc",
                "10.100.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();

        store.attach_vpc("shared-vpc", "acme/backend").unwrap();
        let err = store.attach_vpc("shared-vpc", "acme/backend").unwrap_err();
        assert!(matches!(err, OrgError::VpcAlreadyAttached { .. }));
    }

    #[test]
    fn detach_vpc_succeeds() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store
            .create_vpc(
                "shared-vpc",
                "10.100.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();

        store.attach_vpc("shared-vpc", "acme/backend").unwrap();
        store.detach_vpc("shared-vpc", "acme/backend").unwrap();
        let attachments = store.list_attachments("shared-vpc").unwrap();
        assert!(attachments.is_empty());
    }

    #[test]
    fn detach_not_attached_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store
            .create_vpc(
                "shared-vpc",
                "10.100.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();

        let err = store.detach_vpc("shared-vpc", "acme/backend").unwrap_err();
        assert!(matches!(err, OrgError::VpcNotAttached { .. }));
    }

    #[test]
    fn attach_then_detach_across_store_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("org-persist.redb");

        // First store instance: create org, VPC and attach
        {
            let db = LayerDb::open_at(&path).unwrap();
            let store = OrgStore::new(db);
            store.create("acme").unwrap();
            store
                .create_vpc(
                    "shared-vpc",
                    "10.100.0.0/16",
                    VpcOwner::Org(OrgId("acme".to_string())),
                    true,
                )
                .unwrap();
            store.attach_vpc("shared-vpc", "acme/backend").unwrap();
        }

        // Second store instance: detach should find the attachment
        {
            let db = LayerDb::open_at(&path).unwrap();
            let store = OrgStore::new(db);
            store.detach_vpc("shared-vpc", "acme/backend").unwrap();
            let attachments = store.list_attachments("shared-vpc").unwrap();
            assert!(attachments.is_empty());
        }
    }

    #[test]
    fn list_attachments_multiple() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store
            .create_vpc(
                "shared-vpc",
                "10.100.0.0/16",
                VpcOwner::Org(OrgId("acme".to_string())),
                true,
            )
            .unwrap();

        store.attach_vpc("shared-vpc", "acme/backend").unwrap();
        store.attach_vpc("shared-vpc", "acme/frontend").unwrap();

        let attachments = store.list_attachments("shared-vpc").unwrap();
        assert_eq!(attachments.len(), 2);
    }

    // ── Subnet tests ────────────────────────────────────────────────

    /// Helper: set up org, project, env, and VPC for subnet tests.
    fn setup_for_subnet(store: &OrgStore) -> (String, EnvironmentId) {
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
        store
            .create_env("acme", "backend", "production", None, false, HashMap::new())
            .unwrap();
        store
            .create_vpc(
                "default",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();
        (
            "default".to_string(),
            EnvironmentId("acme/backend/production".to_string()),
        )
    }

    #[test]
    fn create_subnet_succeeds_with_gateway() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        let subnet = store
            .create_subnet(&vpc_name, &env_id, "frontend", Some("10.1.1.0/24"))
            .unwrap();

        assert_eq!(subnet.name, "frontend");
        assert_eq!(subnet.cidr, "10.1.1.0/24");
        assert_eq!(subnet.gateway, "10.1.1.1");
        assert_eq!(subnet.vpc_id.0, "vpc-default");
        assert_eq!(subnet.env_id, env_id);
        assert!(subnet.created_at > 0);
    }

    #[test]
    fn auto_cidr_allocation_first_subnet() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        let subnet = store
            .create_subnet(&vpc_name, &env_id, "frontend", None)
            .unwrap();

        // First auto-allocated /24 within 10.1.0.0/16 should be 10.1.0.0/24
        assert_eq!(subnet.cidr, "10.1.0.0/24");
        assert_eq!(subnet.gateway, "10.1.0.1");
    }

    #[test]
    fn sequential_cidrs_auto_allocation() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        let s1 = store
            .create_subnet(&vpc_name, &env_id, "subnet-aaa", None)
            .unwrap();
        let s2 = store
            .create_subnet(&vpc_name, &env_id, "subnet-bbb", None)
            .unwrap();

        assert_eq!(s1.cidr, "10.1.0.0/24");
        assert_eq!(s2.cidr, "10.1.1.0/24");
    }

    #[test]
    fn custom_cidr_accepted() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        let subnet = store
            .create_subnet(&vpc_name, &env_id, "database", Some("10.1.50.0/24"))
            .unwrap();

        assert_eq!(subnet.cidr, "10.1.50.0/24");
        assert_eq!(subnet.gateway, "10.1.50.1");
    }

    #[test]
    fn gateway_is_dot_1() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        let cases = vec![
            ("sub-aaa", "10.1.0.0/24", "10.1.0.1"),
            ("sub-bbb", "10.1.100.0/24", "10.1.100.1"),
            ("sub-ccc", "10.1.255.0/24", "10.1.255.1"),
        ];

        for (name, cidr, expected_gw) in cases {
            let subnet = store
                .create_subnet(&vpc_name, &env_id, name, Some(cidr))
                .unwrap();
            assert_eq!(
                subnet.gateway, expected_gw,
                "gateway for {} should be {}",
                cidr, expected_gw
            );
        }
    }

    #[test]
    fn subnet_duplicate_name_rejected() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        store
            .create_subnet(&vpc_name, &env_id, "frontend", Some("10.1.1.0/24"))
            .unwrap();

        let err = store
            .create_subnet(&vpc_name, &env_id, "frontend", Some("10.1.2.0/24"))
            .unwrap_err();
        assert!(matches!(err, OrgError::SubnetAlreadyExists { .. }));
    }

    #[test]
    fn subnet_cidr_out_of_range_rejected() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        // VPC is 10.1.0.0/16, so 10.2.0.0/24 is out of range
        let err = store
            .create_subnet(&vpc_name, &env_id, "bad-subnet", Some("10.2.0.0/24"))
            .unwrap_err();
        assert!(matches!(err, OrgError::SubnetCidrOutOfRange { .. }));
    }

    #[test]
    fn subnet_cidr_overlap_rejected() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        store
            .create_subnet(&vpc_name, &env_id, "frontend", Some("10.1.1.0/24"))
            .unwrap();

        let err = store
            .create_subnet(&vpc_name, &env_id, "overlap", Some("10.1.1.0/24"))
            .unwrap_err();
        assert!(matches!(err, OrgError::SubnetCidrOverlap { .. }));
    }

    #[test]
    fn subnet_vpc_not_found_rejected() {
        let (_dir, store) = temp_store();
        let env_id = EnvironmentId("acme/backend/production".to_string());

        let err = store
            .create_subnet("nonexistent", &env_id, "frontend", None)
            .unwrap_err();
        assert!(matches!(err, OrgError::VpcNotFound(_)));
    }

    #[test]
    fn subnet_env_not_found_rejected() {
        let (_dir, store) = temp_store();
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
        store
            .create_vpc(
                "default",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let bad_env = EnvironmentId("acme/backend/nonexistent".to_string());
        let err = store
            .create_subnet("default", &bad_env, "frontend", None)
            .unwrap_err();
        assert!(matches!(err, OrgError::EnvNotFound(_)));
    }

    #[test]
    fn get_subnet_succeeds() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        store
            .create_subnet(&vpc_name, &env_id, "frontend", Some("10.1.1.0/24"))
            .unwrap();

        let subnet = store.get_subnet("default", "frontend").unwrap();
        assert_eq!(subnet.name, "frontend");
        assert_eq!(subnet.cidr, "10.1.1.0/24");
    }

    #[test]
    fn get_subnet_not_found() {
        let (_dir, store) = temp_store();
        let err = store.get_subnet("default", "ghost").unwrap_err();
        assert!(matches!(err, OrgError::SubnetNotFound { .. }));
    }

    #[test]
    fn list_subnets_by_vpc() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        store
            .create_subnet(&vpc_name, &env_id, "frontend", Some("10.1.1.0/24"))
            .unwrap();
        store
            .create_subnet(&vpc_name, &env_id, "database", Some("10.1.2.0/24"))
            .unwrap();

        let subnets = store.list_subnets("default").unwrap();
        assert_eq!(subnets.len(), 2);
    }

    #[test]
    fn list_subnets_by_env_filters() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        // Create second env
        store
            .create_env("acme", "backend", "staging", None, false, HashMap::new())
            .unwrap();
        let staging_env = EnvironmentId("acme/backend/staging".to_string());

        store
            .create_subnet(&vpc_name, &env_id, "prod-sub", Some("10.1.1.0/24"))
            .unwrap();
        store
            .create_subnet(&vpc_name, &staging_env, "stg-sub", Some("10.1.2.0/24"))
            .unwrap();

        let prod_subs = store.list_subnets_by_env(&env_id).unwrap();
        assert_eq!(prod_subs.len(), 1);
        assert_eq!(prod_subs[0].name, "prod-sub");

        let stg_subs = store.list_subnets_by_env(&staging_env).unwrap();
        assert_eq!(stg_subs.len(), 1);
        assert_eq!(stg_subs[0].name, "stg-sub");
    }

    #[test]
    fn delete_subnet_succeeds() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        store
            .create_subnet(&vpc_name, &env_id, "frontend", Some("10.1.1.0/24"))
            .unwrap();

        store.delete_subnet("default", "frontend").unwrap();

        let err = store.get_subnet("default", "frontend").unwrap_err();
        assert!(matches!(err, OrgError::SubnetNotFound { .. }));
    }

    #[test]
    fn delete_subnet_not_found() {
        let (_dir, store) = temp_store();
        let err = store.delete_subnet("default", "ghost").unwrap_err();
        assert!(matches!(err, OrgError::SubnetNotFound { .. }));
    }

    #[test]
    fn auto_allocation_skips_used_cidr() {
        let (_dir, store) = temp_store();
        let (vpc_name, env_id) = setup_for_subnet(&store);

        // Manually occupy 10.1.0.0/24
        store
            .create_subnet(&vpc_name, &env_id, "first", Some("10.1.0.0/24"))
            .unwrap();

        // Auto-allocate should skip to 10.1.1.0/24
        let subnet = store
            .create_subnet(&vpc_name, &env_id, "second", None)
            .unwrap();
        assert_eq!(subnet.cidr, "10.1.1.0/24");
    }

    // ── Peering tests ───────────────────────────────────────────────

    /// Helper: create an org with two VPCs for peering tests.
    fn setup_for_peering(store: &OrgStore) -> (String, String) {
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
        let owner = VpcOwner::Project(ProjectId("acme/backend".to_string()));
        store
            .create_vpc("vpc-alpha", "10.1.0.0/16", owner.clone(), false)
            .unwrap();
        store
            .create_vpc("vpc-beta", "10.2.0.0/16", owner, false)
            .unwrap();
        ("vpc-alpha".to_string(), "vpc-beta".to_string())
    }

    #[test]
    fn create_peering_succeeds() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        let peering = store.create_peering(&a, &b).unwrap();
        assert_eq!(peering.vpc_a, "vpc-alpha");
        assert_eq!(peering.vpc_b, "vpc-beta");
        assert_eq!(peering.status, PeeringStatus::Active);
        assert!(peering.created_at > 0);
    }

    #[test]
    fn create_peering_reverse_order_normalizes() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        // Create with reversed order
        let peering = store.create_peering(&b, &a).unwrap();
        // Key is normalized alphabetically
        assert_eq!(peering.vpc_a, "vpc-alpha");
        assert_eq!(peering.vpc_b, "vpc-beta");
    }

    #[test]
    fn create_peering_duplicate_rejected() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        store.create_peering(&a, &b).unwrap();
        let err = store.create_peering(&a, &b).unwrap_err();
        assert!(matches!(err, OrgError::PeeringAlreadyExists { .. }));
    }

    #[test]
    fn create_peering_duplicate_reversed_rejected() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        store.create_peering(&a, &b).unwrap();
        // Reversed order should also be rejected
        let err = store.create_peering(&b, &a).unwrap_err();
        assert!(matches!(err, OrgError::PeeringAlreadyExists { .. }));
    }

    #[test]
    fn create_peering_self_rejected() {
        let (_dir, store) = temp_store();
        let (a, _) = setup_for_peering(&store);

        let err = store.create_peering(&a, &a).unwrap_err();
        assert!(matches!(err, OrgError::SelfPeeringRejected(_)));
    }

    #[test]
    fn create_peering_vpc_not_found() {
        let (_dir, store) = temp_store();
        let (a, _) = setup_for_peering(&store);

        let err = store.create_peering(&a, "ghost-vpc").unwrap_err();
        assert!(matches!(err, OrgError::VpcNotFound(_)));
    }

    #[test]
    fn delete_peering_succeeds() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        store.create_peering(&a, &b).unwrap();
        store.delete_peering(&a, &b).unwrap();

        let result = store.get_peering(&a, &b).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delete_peering_not_found() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        let err = store.delete_peering(&a, &b).unwrap_err();
        assert!(matches!(err, OrgError::PeeringNotFound { .. }));
    }

    #[test]
    fn list_peerings_returns_all() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        // Add a third VPC
        let owner = VpcOwner::Project(ProjectId("acme/backend".to_string()));
        store
            .create_vpc("vpc-gamma", "10.3.0.0/16", owner, false)
            .unwrap();

        store.create_peering(&a, &b).unwrap();
        store.create_peering(&a, "vpc-gamma").unwrap();

        let peerings = store.list_peerings().unwrap();
        assert_eq!(peerings.len(), 2);
    }

    #[test]
    fn list_peerings_by_vpc_filters() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        let owner = VpcOwner::Project(ProjectId("acme/backend".to_string()));
        store
            .create_vpc("vpc-gamma", "10.3.0.0/16", owner, false)
            .unwrap();

        store.create_peering(&a, &b).unwrap();
        store.create_peering(&a, "vpc-gamma").unwrap();

        // vpc-alpha is in both peerings
        let alpha_peerings = store.list_peerings_by_vpc(&a).unwrap();
        assert_eq!(alpha_peerings.len(), 2);

        // vpc-beta is in one peering
        let beta_peerings = store.list_peerings_by_vpc(&b).unwrap();
        assert_eq!(beta_peerings.len(), 1);

        // vpc-gamma is in one peering
        let gamma_peerings = store.list_peerings_by_vpc("vpc-gamma").unwrap();
        assert_eq!(gamma_peerings.len(), 1);
    }

    #[test]
    fn peering_status_lifecycle() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        // Create -> Active
        let peering = store.create_peering(&a, &b).unwrap();
        assert_eq!(peering.status, PeeringStatus::Active);

        // Delete -> gone
        store.delete_peering(&a, &b).unwrap();
        let result = store.get_peering(&a, &b).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_peering_succeeds() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        store.create_peering(&a, &b).unwrap();

        let peering = store.get_peering(&a, &b).unwrap();
        assert!(peering.is_some());
        let p = peering.unwrap();
        assert_eq!(p.vpc_a, "vpc-alpha");
        assert_eq!(p.vpc_b, "vpc-beta");
    }

    #[test]
    fn get_peering_reversed_order() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        store.create_peering(&a, &b).unwrap();

        // Reversed order should still find the peering
        let peering = store.get_peering(&b, &a).unwrap();
        assert!(peering.is_some());
    }

    #[test]
    fn delete_vpc_blocked_by_peering() {
        let (_dir, store) = temp_store();
        let (a, b) = setup_for_peering(&store);

        store.create_peering(&a, &b).unwrap();

        let err = store.delete_vpc(&a).unwrap_err();
        assert!(matches!(err, OrgError::VpcHasPeerings { .. }));
    }

    // ── Security Group tests ───────────────────────────────────────

    /// Helper: create an org, project, and VPC — returns the VpcId.
    fn setup_vpc_for_sg(store: &OrgStore) -> VpcId {
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
        let owner = VpcOwner::Project(ProjectId("acme/backend".to_string()));
        let vpc = store
            .create_vpc("myvpc", "10.1.0.0/16", owner, false)
            .unwrap();
        vpc.id
    }

    #[test]
    fn create_sg() {
        let (_dir, store) = temp_store();
        let vpc_id = setup_vpc_for_sg(&store);

        let sg = store.create_sg("web", &vpc_id, Some("Web tier")).unwrap();
        assert_eq!(sg.name, "web");
        assert_eq!(sg.vpc_id, vpc_id);
        assert_eq!(sg.description, Some("Web tier".to_string()));
        assert!(!sg.is_default);
        assert_eq!(sg.state, ResourceState::Active);
        assert!(sg.created_at > 0);
    }

    #[test]
    fn duplicate_sg_rejected() {
        let (_dir, store) = temp_store();
        let vpc_id = setup_vpc_for_sg(&store);

        store.create_sg("web", &vpc_id, None).unwrap();
        let err = store.create_sg("web", &vpc_id, None).unwrap_err();
        assert!(matches!(err, OrgError::SgAlreadyExists(_)));
    }

    #[test]
    fn delete_sg() {
        let (_dir, store) = temp_store();
        let vpc_id = setup_vpc_for_sg(&store);

        store.create_sg("web", &vpc_id, None).unwrap();
        store.delete_sg(&vpc_id, "web").unwrap();

        let result = store.get_sg(&vpc_id, "web").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_sgs_by_vpc() {
        let (_dir, store) = temp_store();
        let vpc_id = setup_vpc_for_sg(&store);

        store.create_sg("web", &vpc_id, None).unwrap();
        store.create_sg("database", &vpc_id, None).unwrap();

        let sgs = store.list_sgs_by_vpc(&vpc_id).unwrap();
        // 2 user-created + 1 default = 3
        assert_eq!(sgs.len(), 3);
        let names: Vec<&str> = sgs.iter().map(|sg| sg.name.as_str()).collect();
        assert!(names.contains(&"web"));
        assert!(names.contains(&"database"));
        assert!(names.contains(&"default"));
    }

    #[test]
    fn default_sg_auto_created() {
        let (_dir, store) = temp_store();
        let vpc_id = setup_vpc_for_sg(&store);

        let default_sg = store.get_sg(&vpc_id, "default").unwrap();
        assert!(default_sg.is_some());
        let sg = default_sg.unwrap();
        assert!(sg.is_default);
        assert_eq!(sg.name, "default");
        assert_eq!(sg.vpc_id, vpc_id);
        assert!(sg.description.unwrap().contains("myvpc"));
    }

    #[test]
    fn default_sg_undeletable() {
        let (_dir, store) = temp_store();
        let vpc_id = setup_vpc_for_sg(&store);

        let err = store.delete_sg(&vpc_id, "default").unwrap_err();
        assert!(matches!(err, OrgError::SgIsDefault(_)));
    }

    // ── Route Table tests ─────────────────────────────────────────

    #[test]
    fn default_route_table_auto_created() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let default_rt = store.get_default_route_table(&vpc.id).unwrap();
        assert!(default_rt.is_some());
        let rt = default_rt.unwrap();
        assert!(rt.is_default);
        assert_eq!(rt.name, "default");
        assert_eq!(rt.vpc_id, vpc.id);
    }

    #[test]
    fn default_route_table_has_vpc_cidr_route() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let rt = store.get_default_route_table(&vpc.id).unwrap().unwrap();
        let routes = store.list_routes_by_table(&rt.id).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].destination, "10.1.0.0/16");
        assert_eq!(routes[0].target, RouteTarget::Local);
        assert_eq!(routes[0].origin, RouteOrigin::System);
        assert_eq!(routes[0].priority, 0);
    }

    #[test]
    fn create_route_table() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let rt = store.create_route_table("custom", &vpc.id).unwrap();
        assert_eq!(rt.name, "custom");
        assert!(!rt.is_default);
        assert_eq!(rt.vpc_id, vpc.id);
    }

    #[test]
    fn list_route_tables_by_vpc() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        store.create_route_table("custom-a", &vpc.id).unwrap();
        store.create_route_table("custom-b", &vpc.id).unwrap();

        let tables = store.list_route_tables_by_vpc(&vpc.id).unwrap();
        assert_eq!(tables.len(), 3); // default + 2 custom
    }

    #[test]
    fn cannot_delete_default_route_table() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let err = store.delete_route_table(&vpc.id, "default").unwrap_err();
        assert!(matches!(err, OrgError::CannotDeleteDefaultRouteTable));
    }

    #[test]
    fn delete_route_table_succeeds() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        store.create_route_table("ephemeral", &vpc.id).unwrap();
        store.delete_route_table(&vpc.id, "ephemeral").unwrap();

        let tables = store.list_route_tables_by_vpc(&vpc.id).unwrap();
        assert_eq!(tables.len(), 1); // only default remains
    }

    #[test]
    fn add_user_route_and_list() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let rt = store.get_default_route_table(&vpc.id).unwrap().unwrap();

        let route = store
            .add_route(&rt.id, "10.99.0.0/24", RouteTarget::Blackhole, None)
            .unwrap();
        assert_eq!(route.origin, RouteOrigin::User);
        assert_eq!(route.priority, 100);

        let routes = store.list_routes_by_table(&rt.id).unwrap();
        assert_eq!(routes.len(), 2); // VPC CIDR system route + user route
    }

    #[test]
    fn cannot_delete_system_route() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let rt = store.get_default_route_table(&vpc.id).unwrap().unwrap();
        let err = store.remove_route(&rt.id, "10.1.0.0/16").unwrap_err();
        assert!(matches!(err, OrgError::CannotDeleteSystemRoute));
    }

    #[test]
    fn delete_user_route_succeeds() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let rt = store.get_default_route_table(&vpc.id).unwrap().unwrap();
        store
            .add_route(&rt.id, "10.99.0.0/24", RouteTarget::Blackhole, None)
            .unwrap();
        store.remove_route(&rt.id, "10.99.0.0/24").unwrap();

        let routes = store.list_routes_by_table(&rt.id).unwrap();
        assert_eq!(routes.len(), 1); // only system route remains
    }

    #[test]
    fn subnet_creation_adds_local_route() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        // Create an environment for the subnet.
        store
            .create_env("acme", "backend", "production", None, false, HashMap::new())
            .unwrap();

        let env_id = EnvironmentId("acme/backend/production".to_string());
        store
            .create_subnet("myvpc", &env_id, "web", Some("10.1.1.0/24"))
            .unwrap();

        let rt = store.get_default_route_table(&vpc.id).unwrap().unwrap();
        let routes = store.list_routes_by_table(&rt.id).unwrap();
        // Should have: VPC CIDR route + subnet CIDR route
        assert_eq!(routes.len(), 2);
        let subnet_route = routes.iter().find(|r| r.destination == "10.1.1.0/24");
        assert!(subnet_route.is_some());
        assert_eq!(subnet_route.unwrap().origin, RouteOrigin::System);
    }

    #[test]
    fn subnet_deletion_removes_local_route() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        store
            .create_env("acme", "backend", "production", None, false, HashMap::new())
            .unwrap();

        let env_id = EnvironmentId("acme/backend/production".to_string());
        store
            .create_subnet("myvpc", &env_id, "web", Some("10.1.1.0/24"))
            .unwrap();

        store.delete_subnet("myvpc", "web").unwrap();

        let rt = store.get_default_route_table(&vpc.id).unwrap().unwrap();
        let routes = store.list_routes_by_table(&rt.id).unwrap();
        // Only VPC CIDR route should remain.
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].destination, "10.1.0.0/16");
    }

    #[test]
    fn subnet_route_table_association() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        store
            .create_env("acme", "backend", "production", None, false, HashMap::new())
            .unwrap();

        let env_id = EnvironmentId("acme/backend/production".to_string());
        let subnet = store
            .create_subnet("myvpc", &env_id, "web", Some("10.1.1.0/24"))
            .unwrap();

        // Default: resolves to default route table.
        let resolved = store.resolve_subnet_route_table(&subnet).unwrap();
        assert!(resolved.is_default);

        // Create custom table and associate.
        let custom = store.create_route_table("custom", &vpc.id).unwrap();
        store
            .associate_subnet_route_table(&subnet.id, &custom.id)
            .unwrap();

        let resolved = store.resolve_subnet_route_table(&subnet).unwrap();
        assert_eq!(resolved.name, "custom");

        // Disassociate: back to default.
        store.disassociate_subnet_route_table(&subnet.id).unwrap();
        let resolved = store.resolve_subnet_route_table(&subnet).unwrap();
        assert!(resolved.is_default);
    }

    #[test]
    fn cannot_delete_route_table_with_subnets() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        store
            .create_env("acme", "backend", "production", None, false, HashMap::new())
            .unwrap();

        let env_id = EnvironmentId("acme/backend/production".to_string());
        let subnet = store
            .create_subnet("myvpc", &env_id, "web", Some("10.1.1.0/24"))
            .unwrap();

        let custom = store.create_route_table("custom", &vpc.id).unwrap();
        store
            .associate_subnet_route_table(&subnet.id, &custom.id)
            .unwrap();

        let err = store.delete_route_table(&vpc.id, "custom").unwrap_err();
        assert!(matches!(err, OrgError::RouteTableHasSubnets { .. }));
    }

    #[test]
    fn cannot_delete_propagated_route() {
        let (_dir, store) = temp_store();
        setup_org_and_project(&store);
        let vpc = store
            .create_vpc(
                "myvpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();

        let rt = store.get_default_route_table(&vpc.id).unwrap().unwrap();
        store
            .add_propagated_route(
                &rt.id,
                "10.2.0.0/16",
                RouteTarget::VpcPeering("peering-123".to_string()),
            )
            .unwrap();

        let err = store.remove_route(&rt.id, "10.2.0.0/16").unwrap_err();
        assert!(matches!(err, OrgError::CannotDeletePropagatedRoute));
    }

    // ── NAT Gateway tests ──────────────────────────────────────────

    fn setup_vpc_and_subnet(store: &OrgStore) -> (Vpc, Subnet) {
        store.create("acme").unwrap();
        store.create_project("acme", "backend").unwrap();
        store
            .create_env("acme", "backend", "production", None, false, HashMap::new())
            .unwrap();
        let vpc = store
            .create_vpc(
                "test-vpc",
                "10.1.0.0/16",
                VpcOwner::Project(ProjectId("acme/backend".to_string())),
                false,
            )
            .unwrap();
        let env_id = EnvironmentId("acme/backend/production".to_string());
        let subnet = store
            .create_subnet("test-vpc", &env_id, "frontend", Some("10.1.1.0/24"))
            .unwrap();
        (vpc, subnet)
    }

    #[test]
    fn test_create_nat_gw() {
        let (_dir, store) = temp_store();
        let (vpc, subnet) = setup_vpc_and_subnet(&store);

        let gw = store
            .create_nat_gw("main-gw", &vpc.id, &subnet.id, "1.2.3.4")
            .unwrap();
        assert_eq!(gw.name, "main-gw");
        assert_eq!(gw.vpc_id, vpc.id);
        assert_eq!(gw.subnet_id, subnet.id);
        assert_eq!(gw.public_ip, "1.2.3.4");
    }

    #[test]
    fn test_nat_gw_state_pending() {
        let (_dir, store) = temp_store();
        let (vpc, subnet) = setup_vpc_and_subnet(&store);

        let gw = store
            .create_nat_gw("gw1", &vpc.id, &subnet.id, "1.2.3.4")
            .unwrap();
        assert_eq!(gw.state, ResourceState::Pending);
    }

    #[test]
    fn test_nat_gw_state_active() {
        let (_dir, store) = temp_store();
        let (vpc, subnet) = setup_vpc_and_subnet(&store);

        store
            .create_nat_gw("gw1", &vpc.id, &subnet.id, "1.2.3.4")
            .unwrap();
        let gw = store
            .update_nat_gw_state(&vpc.id, "gw1", ResourceState::Active)
            .unwrap();
        assert_eq!(gw.state, ResourceState::Active);
    }

    #[test]
    fn test_nat_gw_state_failed() {
        let (_dir, store) = temp_store();
        let (vpc, subnet) = setup_vpc_and_subnet(&store);

        store
            .create_nat_gw("gw1", &vpc.id, &subnet.id, "1.2.3.4")
            .unwrap();
        let gw = store
            .update_nat_gw_state(&vpc.id, "gw1", ResourceState::Failed)
            .unwrap();
        assert_eq!(gw.state, ResourceState::Failed);
    }

    #[test]
    fn test_nat_gw_public_ip_resolved() {
        let (_dir, store) = temp_store();
        let (vpc, subnet) = setup_vpc_and_subnet(&store);

        let gw = store
            .create_nat_gw("gw1", &vpc.id, &subnet.id, "203.0.113.5")
            .unwrap();
        assert_eq!(gw.public_ip, "203.0.113.5");
    }

    #[test]
    fn test_list_nat_gws_by_vpc() {
        let (_dir, store) = temp_store();
        let (vpc, subnet) = setup_vpc_and_subnet(&store);

        store
            .create_nat_gw("gw1", &vpc.id, &subnet.id, "1.2.3.4")
            .unwrap();
        store
            .create_nat_gw("gw2", &vpc.id, &subnet.id, "1.2.3.5")
            .unwrap();

        let gws = store.list_nat_gws_by_vpc(&vpc.id).unwrap();
        assert_eq!(gws.len(), 2);
    }

    #[test]
    fn test_delete_nat_gw() {
        let (_dir, store) = temp_store();
        let (vpc, subnet) = setup_vpc_and_subnet(&store);

        store
            .create_nat_gw("gw1", &vpc.id, &subnet.id, "1.2.3.4")
            .unwrap();
        store.delete_nat_gw(&vpc.id, "gw1").unwrap();

        let gw = store.get_nat_gw(&vpc.id, "gw1").unwrap();
        assert!(gw.is_none());
    }

    #[test]
    fn test_nat_gw_duplicate_rejected() {
        let (_dir, store) = temp_store();
        let (vpc, subnet) = setup_vpc_and_subnet(&store);

        store
            .create_nat_gw("gw1", &vpc.id, &subnet.id, "1.2.3.4")
            .unwrap();
        let err = store
            .create_nat_gw("gw1", &vpc.id, &subnet.id, "1.2.3.5")
            .unwrap_err();
        assert!(matches!(err, OrgError::NatGwAlreadyExists(_)));
    }

    #[test]
    fn test_routes_referencing_nat_gw() {
        let (_dir, store) = temp_store();
        let (vpc, subnet) = setup_vpc_and_subnet(&store);

        store
            .create_nat_gw("gw1", &vpc.id, &subnet.id, "1.2.3.4")
            .unwrap();

        let rt = store.get_default_route_table(&vpc.id).unwrap().unwrap();
        store
            .add_route(
                &rt.id,
                "0.0.0.0/0",
                RouteTarget::NatGateway("gw1".to_string()),
                None,
            )
            .unwrap();

        let refs = store.routes_referencing_nat_gw(&vpc.id, "gw1").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].destination, "0.0.0.0/0");
    }
}
