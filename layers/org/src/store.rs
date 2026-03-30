use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use ipnet::Ipv4Net;
use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{
    Environment, EnvironmentId, Org, OrgId, Project, ProjectId, Subnet, SubnetId, Vpc,
    VpcAttachment, VpcId, VpcOwner,
};
use crate::validation::validate_name;
use crate::vpc::{cidrs_overlap, parse_and_validate_cidr};

const TABLE: &str = "orgs";
const PROJECTS_TABLE: &str = "projects";
const ENVIRONMENTS_TABLE: &str = "environments";
const VPCS_TABLE: &str = "vpcs";
const SUBNETS_TABLE: &str = "subnets";
const VPC_ATTACHMENTS_TABLE: &str = "vpc_attachments";
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
    fn next_vni(&self) -> Result<u32> {
        let current: Option<u32> = self.db.get(VNI_COUNTER_TABLE, VNI_COUNTER_KEY)?;
        let vni = current.unwrap_or(VNI_START);
        self.db
            .set(VNI_COUNTER_TABLE, VNI_COUNTER_KEY, &(vni + 1))?;
        Ok(vni)
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

    /// Delete a VPC by name.
    pub fn delete_vpc(&self, name: &str) -> Result<()> {
        if !self.db.exists(VPCS_TABLE, name)? {
            return Err(OrgError::VpcNotFound(name.to_string()));
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

    // ── Subnet operations ───────────────────────────────────────────

    /// Create a subnet within a VPC.
    ///
    /// Validates the name, resolves the VPC (auto-creating a default VPC if
    /// the project has none and `--vpc` is omitted), auto-allocates a /24 CIDR
    /// if none is given, and persists the subnet.
    pub fn create_subnet(
        &self,
        name: &str,
        org: &str,
        project: &str,
        env: &str,
        vpc_name: Option<&str>,
        cidr: Option<&str>,
    ) -> Result<Subnet> {
        validate_name(name, "subnet")?;

        // Resolve environment
        let _ = self.get_env(org, project, env)?;
        let env_id = EnvironmentId(Self::env_key(org, project, env));

        // Resolve VPC: explicit or default
        let vpc = match vpc_name {
            Some(vn) => self
                .get_vpc(vn)?
                .ok_or_else(|| OrgError::VpcNotFound(vn.to_string()))?,
            None => self.ensure_default_vpc(org, project)?,
        };

        // Parse VPC CIDR for subnet allocation
        let vpc_cidr: Ipv4Net = vpc
            .cidr
            .parse()
            .map_err(|_| OrgError::InvalidCidr(format!("VPC CIDR '{}' is invalid", vpc.cidr)))?;

        // Get existing subnets in this VPC to check for duplicates and CIDR overlap
        let existing_subnets = self.list_subnets_by_vpc(&vpc.id)?;

        // Check name uniqueness within the VPC
        if existing_subnets.iter().any(|s| s.name == name) {
            return Err(OrgError::SubnetAlreadyExists {
                name: name.to_string(),
                vpc: vpc.name.clone(),
            });
        }

        let subnet_cidr = match cidr {
            Some(c) => {
                let net = parse_and_validate_cidr(c)?;
                // Verify the subnet CIDR fits within the VPC CIDR
                if !vpc_cidr.contains(&net.network()) || !vpc_cidr.contains(&net.broadcast()) {
                    return Err(OrgError::SubnetOutsideVpc {
                        subnet_cidr: net.to_string(),
                        vpc_cidr: vpc.cidr.clone(),
                    });
                }
                // Check overlap with existing subnets
                for existing in &existing_subnets {
                    if let Ok(existing_net) = existing.cidr.parse::<Ipv4Net>() {
                        if cidrs_overlap(&net, &existing_net) {
                            return Err(OrgError::CidrOverlap {
                                new_cidr: net.to_string(),
                                existing_cidr: existing_net.to_string(),
                            });
                        }
                    }
                }
                net
            }
            None => self.auto_allocate_subnet_cidr(&vpc_cidr, &existing_subnets)?,
        };

        // Gateway is always .1 of the subnet
        let gateway_octets = subnet_cidr.network().octets();
        let gateway =
            std::net::Ipv4Addr::new(gateway_octets[0], gateway_octets[1], gateway_octets[2], 1);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let subnet_id = SubnetId(format!("{}/{}/{}/{}", org, project, env, name));
        let subnet = Subnet {
            id: subnet_id.clone(),
            name: name.to_string(),
            vpc_id: vpc.id.clone(),
            env_id,
            cidr: subnet_cidr.to_string(),
            gateway: gateway.to_string(),
            created_at: now,
        };

        self.db.set(SUBNETS_TABLE, &subnet_id.0, &subnet)?;
        Ok(subnet)
    }

    /// Auto-allocate the next available /24 within the VPC's CIDR.
    fn auto_allocate_subnet_cidr(
        &self,
        vpc_cidr: &Ipv4Net,
        existing: &[Subnet],
    ) -> Result<Ipv4Net> {
        let existing_nets: Vec<Ipv4Net> = existing
            .iter()
            .filter_map(|s| s.cidr.parse::<Ipv4Net>().ok())
            .collect();

        let base = vpc_cidr.network().octets();
        // Iterate through possible /24 subnets within the VPC CIDR
        let vpc_prefix = vpc_cidr.prefix_len();
        if vpc_prefix > 24 {
            return Err(OrgError::InvalidCidr(
                "VPC CIDR is too small for /24 subnets".to_string(),
            ));
        }

        // Calculate the range of third-octet values
        let num_subnets = 1u32 << (24 - vpc_prefix);
        for i in 0..num_subnets {
            let third_octet_base = base[2] as u32 + i;
            if third_octet_base > 255 {
                break;
            }
            let candidate = Ipv4Net::new(
                std::net::Ipv4Addr::new(base[0], base[1], third_octet_base as u8, 0),
                24,
            )
            .unwrap();

            if !existing_nets.iter().any(|e| cidrs_overlap(&candidate, e)) {
                return Ok(candidate);
            }
        }

        Err(OrgError::CidrExhausted)
    }

    /// Ensure a default VPC exists for a project. If the project already has
    /// VPCs, return the first one. Otherwise, auto-create one.
    pub fn ensure_default_vpc(&self, org: &str, project: &str) -> Result<Vpc> {
        let project_id = ProjectId(format!("{org}/{project}"));
        let vpcs = self.list_vpcs_by_project(&project_id)?;
        if let Some(vpc) = vpcs.into_iter().next() {
            return Ok(vpc);
        }

        // Auto-create a default VPC
        let default_name = format!("{org}-{project}-default");
        let owner = VpcOwner::Project(project_id);

        // Auto-allocate CIDR by collecting existing CIDRs in the org
        let org_id = OrgId(org.to_string());
        let org_vpcs = self.list_vpcs_by_org(&org_id)?;
        let existing_cidrs: Vec<Ipv4Net> = org_vpcs
            .iter()
            .filter_map(|v| v.cidr.parse::<Ipv4Net>().ok())
            .collect();

        // Find available /16
        let cidr = self.auto_allocate_vpc_cidr(&existing_cidrs)?;

        let vni = self.next_vni()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let vpc = Vpc {
            id: VpcId(format!("vpc-{default_name}")),
            name: default_name.clone(),
            cidr: cidr.to_string(),
            vni,
            owner,
            shared: false,
            created_at: now,
        };

        self.db.set(VPCS_TABLE, &default_name, &vpc)?;
        Ok(vpc)
    }

    /// Find an available /16 in 10.0.0.0/8.
    fn auto_allocate_vpc_cidr(&self, existing: &[Ipv4Net]) -> Result<Ipv4Net> {
        for second_octet in 0..=255u8 {
            let candidate =
                Ipv4Net::new(std::net::Ipv4Addr::new(10, second_octet, 0, 0), 16).unwrap();
            if !existing.iter().any(|e| cidrs_overlap(&candidate, e)) {
                return Ok(candidate);
            }
        }
        Err(OrgError::CidrExhausted)
    }

    /// Get a subnet by its full ID (org/project/env/name).
    pub fn get_subnet(
        &self,
        org: &str,
        project: &str,
        env: &str,
        name: &str,
    ) -> Result<Option<Subnet>> {
        let key = format!("{org}/{project}/{env}/{name}");
        Ok(self.db.get(SUBNETS_TABLE, &key)?)
    }

    /// List all subnets, optionally filtered.
    pub fn list_subnets(
        &self,
        env_filter: Option<&str>,
        vpc_filter: Option<&str>,
        org: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<Subnet>> {
        let all: Vec<(String, Subnet)> = self.db.list(SUBNETS_TABLE)?;
        let mut subnets: Vec<Subnet> = all.into_iter().map(|(_, s)| s).collect();

        if let Some(env_name) = env_filter {
            subnets.retain(|s| {
                // env_id format: org/project/env
                s.env_id.0.split('/').nth(2) == Some(env_name)
            });
        }

        if let Some(vpc_name) = vpc_filter {
            // Resolve VPC name to VPC ID
            if let Ok(Some(vpc)) = self.get_vpc(vpc_name) {
                subnets.retain(|s| s.vpc_id == vpc.id);
            } else {
                return Ok(Vec::new());
            }
        }

        if let (Some(o), Some(p)) = (org, project) {
            let prefix = format!("{o}/{p}/");
            subnets.retain(|s| s.id.0.starts_with(&prefix));
        }

        Ok(subnets)
    }

    /// List all subnets belonging to a specific VPC.
    pub fn list_subnets_by_vpc(&self, vpc_id: &VpcId) -> Result<Vec<Subnet>> {
        let all: Vec<(String, Subnet)> = self.db.list(SUBNETS_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(_, s)| s.vpc_id == *vpc_id)
            .map(|(_, s)| s)
            .collect())
    }

    /// Delete a subnet by name within a VPC.
    pub fn delete_subnet_by_name(&self, name: &str, vpc_name: &str) -> Result<()> {
        let vpc = self
            .get_vpc(vpc_name)?
            .ok_or_else(|| OrgError::VpcNotFound(vpc_name.to_string()))?;

        let subnets = self.list_subnets_by_vpc(&vpc.id)?;
        let subnet =
            subnets
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| OrgError::SubnetNotFound {
                    name: name.to_string(),
                    vpc: vpc_name.to_string(),
                })?;

        self.db.delete(SUBNETS_TABLE, &subnet.id.0)?;
        Ok(())
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
}
