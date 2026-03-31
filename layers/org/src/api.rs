//! Control socket types for the org layer.
//!
//! Follows the same pattern as `syfrah_compute::control`:
//! - `OrgRequest` / `OrgResponse` are the typed messages
//! - `OrgLayerHandler` adapts an `OrgStore` to the opaque `LayerHandler` trait
//! - `send_org_request` is the client-side helper used by CLI commands

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use syfrah_api::handler::LayerHandler;
use syfrah_api::{LayerRequest, LayerResponse};
use tokio::net::UnixStream;

use crate::sg_rules::SgRuleStore;
use crate::store::OrgStore;
use crate::types::{
    Direction, Environment, EnvironmentId, NetworkInterface, Org, OrgId, PeeringStatus, PortRange,
    Project, ProjectId, Protocol, RuleId, RuleSource, SecurityGroup, SecurityGroupRule, Subnet,
    Vpc, VpcOwner, VpcPeering,
};

// ---------------------------------------------------------------------------
// Request / Response enums
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub enum OrgRequest {
    // -- Org --
    OrgCreate {
        name: String,
    },
    OrgList,
    OrgDelete {
        name: String,
    },

    // -- Project --
    ProjectCreate {
        name: String,
        org: String,
    },
    ProjectList {
        org: Option<String>,
    },
    ProjectDelete {
        name: String,
        org: String,
    },

    // -- Environment --
    EnvCreate {
        name: String,
        project: String,
        org: String,
        ttl: Option<u64>,
        deletion_protection: bool,
        labels: HashMap<String, String>,
    },
    EnvList {
        project: Option<String>,
        org: Option<String>,
    },
    EnvDestroy {
        name: String,
        project: String,
        org: String,
    },
    EnvExtend {
        name: String,
        project: String,
        org: String,
        ttl_seconds: u64,
    },
    EnvUpdate {
        name: String,
        project: String,
        org: String,
        deletion_protection: Option<bool>,
    },

    // -- VPC --
    VpcCreate {
        name: String,
        org: String,
        project: Option<String>,
        shared: bool,
        cidr: Option<String>,
    },
    VpcList {
        org: Option<String>,
        project: Option<String>,
    },
    VpcDelete {
        name: String,
    },
    VpcAttach {
        vpc: String,
        project: String,
    },
    VpcDetach {
        vpc: String,
        project: String,
    },
    VpcPeer {
        from: String,
        to: String,
    },
    VpcUnpeer {
        from: String,
        to: String,
    },
    VpcPeeringsList {
        vpc: Option<String>,
    },

    // -- Subnet --
    SubnetCreate {
        name: String,
        env: String,
        project: String,
        org: String,
        vpc: Option<String>,
        cidr: Option<String>,
    },
    SubnetList {
        env: Option<String>,
        vpc: Option<String>,
        project: Option<String>,
        org: Option<String>,
    },
    SubnetDelete {
        name: String,
        vpc: Option<String>,
    },

    // -- Security Group --
    SgCreate {
        name: String,
        vpc: String,
        description: String,
    },
    SgList {
        vpc: Option<String>,
    },
    SgShow {
        name: String,
        vpc: Option<String>,
    },
    SgDelete {
        name: String,
        vpc: Option<String>,
    },
    SgAttach {
        sg: String,
        vm: Option<String>,
        nic: Option<String>,
    },
    SgDetach {
        sg: String,
        vm: Option<String>,
        nic: Option<String>,
    },
    SgListForNic {
        vm: Option<String>,
        nic: Option<String>,
    },

    // -- SG Rules --
    SgAddRule {
        sg: String,
        direction: String,
        protocol: String,
        port: Option<String>,
        source: Option<String>,
        source_sg: Option<String>,
        description: String,
        priority: u32,
    },
    SgRemoveRule {
        sg: String,
        rule_id: String,
    },
    SgListRules {
        sg: String,
    },
    SgCheck {
        vm: String,
        port: u16,
        protocol: String,
        source: Option<String>,
    },

    // -- Subnet resolution (used by compute layer) --
    SubnetResolve {
        subnet_name: Option<String>,
        env: Option<String>,
        project: Option<String>,
        org: Option<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum OrgResponse {
    Org(Org),
    OrgList(Vec<Org>),
    Project(Project),
    ProjectList(Vec<Project>),
    Env(Environment),
    EnvList(Vec<Environment>),
    Vpc(Vpc),
    VpcList(Vec<Vpc>),
    PeeringList(Vec<VpcPeering>),
    Subnet(Subnet),
    SubnetList(Vec<Subnet>),
    Sg(SecurityGroup),
    SgList(Vec<SecurityGroup>),
    Nic(NetworkInterface),
    SgRule(SecurityGroupRule),
    SgRuleList(Vec<SecurityGroupRule>),
    SgCheckResult {
        verdict: String,
        reason: String,
    },
    /// Resolved subnet info for VM placement (None = no subnet context).
    SubnetResolved(Option<ResolvedSubnet>),
    Ok,
    Error(String),
}

/// Minimal subnet info returned by SubnetResolve, matching `SubnetInfo` in compute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedSubnet {
    pub name: String,
    pub cidr: String,
    pub gateway: String,
    pub vpc_id: String,
    pub env_id: String,
}

// ---------------------------------------------------------------------------
// OrgLayerHandler — adapts OrgStore to LayerHandler
// ---------------------------------------------------------------------------

pub struct OrgLayerHandler {
    store: Arc<OrgStore>,
    sg_rule_store: Option<Arc<SgRuleStore>>,
}

impl OrgLayerHandler {
    pub fn new(store: Arc<OrgStore>) -> Self {
        Self {
            store,
            sg_rule_store: None,
        }
    }

    pub fn with_sg_rule_store(mut self, sg_rule_store: Arc<SgRuleStore>) -> Self {
        self.sg_rule_store = Some(sg_rule_store);
        self
    }
}

#[async_trait::async_trait]
impl LayerHandler for OrgLayerHandler {
    async fn handle(&self, request: Vec<u8>, _caller_uid: Option<u32>) -> Vec<u8> {
        let req: OrgRequest = match serde_json::from_slice(&request) {
            Ok(r) => r,
            Err(e) => {
                let resp = OrgResponse::Error(format!("invalid org request: {e}"));
                return serde_json::to_vec(&resp).unwrap_or_default();
            }
        };

        let resp = handle_org_request(&self.store, self.sg_rule_store.as_deref(), req);
        serde_json::to_vec(&resp).unwrap_or_default()
    }
}

const DEFAULT_PROJECT_CIDR: &str = "10.1.0.0/16";
const DEFAULT_SHARED_CIDR: &str = "10.100.0.0/16";

fn handle_org_request(
    store: &OrgStore,
    sg_rule_store: Option<&SgRuleStore>,
    req: OrgRequest,
) -> OrgResponse {
    match req {
        // -- Org --
        OrgRequest::OrgCreate { name } => match store.create(&name) {
            Ok(org) => OrgResponse::Org(org),
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::OrgList => match store.list() {
            Ok(orgs) => OrgResponse::OrgList(orgs),
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::OrgDelete { name } => match store.delete(&name) {
            Ok(()) => OrgResponse::Ok,
            Err(e) => OrgResponse::Error(e.to_string()),
        },

        // -- Project --
        OrgRequest::ProjectCreate { name, org } => match store.create_project(&org, &name) {
            Ok(project) => OrgResponse::Project(project),
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::ProjectList { org } => {
            let result = match org.as_deref() {
                Some(org_name) => store.list_projects(org_name),
                None => {
                    let orgs = match store.list() {
                        Ok(o) => o,
                        Err(e) => return OrgResponse::Error(e.to_string()),
                    };
                    let mut all = Vec::new();
                    for o in &orgs {
                        match store.list_projects(&o.name) {
                            Ok(projects) => all.extend(projects),
                            Err(e) => return OrgResponse::Error(e.to_string()),
                        }
                    }
                    Ok(all)
                }
            };
            match result {
                Ok(projects) => OrgResponse::ProjectList(projects),
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }
        OrgRequest::ProjectDelete { name, org } => match store.delete_project(&org, &name) {
            Ok(()) => OrgResponse::Ok,
            Err(e) => OrgResponse::Error(e.to_string()),
        },

        // -- Environment --
        OrgRequest::EnvCreate {
            name,
            project,
            org,
            ttl,
            deletion_protection,
            labels,
        } => match store.create_env(&org, &project, &name, ttl, deletion_protection, labels) {
            Ok(env) => OrgResponse::Env(env),
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::EnvList { project, org } => match (&org, &project) {
            (Some(o), Some(p)) => match store.list_envs(o, p) {
                Ok(envs) => OrgResponse::EnvList(envs),
                Err(e) => OrgResponse::Error(e.to_string()),
            },
            _ => OrgResponse::Error("specify org and project to list environments".to_string()),
        },
        OrgRequest::EnvDestroy { name, project, org } => {
            match store.delete_env(&org, &project, &name) {
                Ok(()) => OrgResponse::Ok,
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }
        OrgRequest::EnvExtend {
            name,
            project,
            org,
            ttl_seconds,
        } => match store.extend_env(&org, &project, &name, ttl_seconds) {
            Ok(env) => OrgResponse::Env(env),
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::EnvUpdate {
            name,
            project,
            org,
            deletion_protection,
        } => {
            if let Some(dp) = deletion_protection {
                match store.update_env_protection(&org, &project, &name, dp) {
                    Ok(env) => OrgResponse::Env(env),
                    Err(e) => OrgResponse::Error(e.to_string()),
                }
            } else {
                OrgResponse::Error("no update specified".to_string())
            }
        }

        // -- VPC --
        OrgRequest::VpcCreate {
            name,
            org,
            project,
            shared,
            cidr,
        } => {
            if !shared && project.is_none() {
                return OrgResponse::Error("--project is required for non-shared VPCs".to_string());
            }
            let (owner, cidr_str) = if shared {
                let owner = VpcOwner::Org(OrgId(org.clone()));
                let cidr_str = cidr.as_deref().unwrap_or(DEFAULT_SHARED_CIDR).to_string();
                (owner, cidr_str)
            } else {
                let p = project.as_deref().unwrap();
                let owner = VpcOwner::Project(ProjectId(format!("{org}/{p}")));
                let cidr_str = cidr.as_deref().unwrap_or(DEFAULT_PROJECT_CIDR).to_string();
                (owner, cidr_str)
            };
            match store.create_vpc(&name, &cidr_str, owner, shared) {
                Ok(vpc) => OrgResponse::Vpc(vpc),
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }
        OrgRequest::VpcList { org, project } => {
            let result = match (org.as_deref(), project.as_deref()) {
                (Some(org_name), Some(proj_name)) => {
                    let project_id = ProjectId(format!("{org_name}/{proj_name}"));
                    store.list_vpcs_by_project(&project_id)
                }
                (Some(org_name), None) => {
                    let org_id = OrgId(org_name.to_string());
                    store.list_vpcs_by_org(&org_id)
                }
                _ => store.list_vpcs(),
            };
            match result {
                Ok(vpcs) => OrgResponse::VpcList(vpcs),
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }
        OrgRequest::VpcDelete { name } => match store.delete_vpc(&name) {
            Ok(()) => OrgResponse::Ok,
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::VpcAttach { vpc, project } => match store.attach_vpc(&vpc, &project) {
            Ok(()) => OrgResponse::Ok,
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::VpcDetach { vpc, project } => match store.detach_vpc(&vpc, &project) {
            Ok(()) => OrgResponse::Ok,
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::VpcPeer { from, to } => match store.create_peering(&from, &to) {
            Ok(_peering) => OrgResponse::Ok,
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::VpcUnpeer { from, to } => match store.delete_peering(&from, &to) {
            Ok(()) => OrgResponse::Ok,
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::VpcPeeringsList { vpc } => {
            let result = match vpc.as_deref() {
                Some(name) => store.list_peerings_for_vpc(name),
                None => store.list_peerings().map(|all| {
                    all.into_iter()
                        .filter(|p| p.status == PeeringStatus::Active)
                        .collect()
                }),
            };
            match result {
                Ok(peerings) => {
                    // Resolve VPC names in the peering list for display
                    let enriched: Vec<VpcPeering> = peerings
                        .into_iter()
                        .map(|mut p| {
                            p.vpc_a = store.resolve_vpc_name(&p.vpc_a);
                            p.vpc_b = store.resolve_vpc_name(&p.vpc_b);
                            p
                        })
                        .collect();
                    OrgResponse::PeeringList(enriched)
                }
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }

        // -- Subnet --
        OrgRequest::SubnetCreate {
            name,
            env,
            project,
            org,
            vpc,
            cidr,
        } => {
            // Resolve VPC name
            let vpc_name = match vpc {
                Some(v) => v,
                None => {
                    // Ensure default VPC exists (same logic as VpcStore::ensure_default_vpc)
                    let default_name = format!("{org}-{project}-default");
                    match store.get_vpc(&default_name) {
                        Ok(Some(_)) => default_name,
                        Ok(None) => {
                            let owner = VpcOwner::Project(ProjectId(format!("{org}/{project}")));
                            match store.create_vpc(
                                &default_name,
                                DEFAULT_PROJECT_CIDR,
                                owner,
                                false,
                            ) {
                                Ok(vpc) => vpc.name,
                                Err(e) => return OrgResponse::Error(e.to_string()),
                            }
                        }
                        Err(e) => return OrgResponse::Error(e.to_string()),
                    }
                }
            };

            let env_id = EnvironmentId(format!("{org}/{project}/{env}"));
            match store.create_subnet(&vpc_name, &env_id, &name, cidr.as_deref()) {
                Ok(subnet) => OrgResponse::Subnet(subnet),
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }
        OrgRequest::SubnetList {
            env,
            vpc,
            project,
            org,
        } => {
            if let Some(vpc_name) = vpc.as_deref() {
                match store.list_subnets(vpc_name) {
                    Ok(subnets) => OrgResponse::SubnetList(subnets),
                    Err(e) => OrgResponse::Error(e.to_string()),
                }
            } else if let (Some(env_name), Some(proj), Some(org_name)) =
                (env.as_deref(), project.as_deref(), org.as_deref())
            {
                let env_id = EnvironmentId(format!("{org_name}/{proj}/{env_name}"));
                match store.list_subnets_by_env(&env_id) {
                    Ok(subnets) => OrgResponse::SubnetList(subnets),
                    Err(e) => OrgResponse::Error(e.to_string()),
                }
            } else if let (Some(proj), Some(org_name)) = (project.as_deref(), org.as_deref()) {
                let project_id = ProjectId(format!("{org_name}/{proj}"));
                let vpcs = match store.list_vpcs_by_project(&project_id) {
                    Ok(v) => v,
                    Err(e) => return OrgResponse::Error(e.to_string()),
                };
                let mut all_subnets = Vec::new();
                for v in &vpcs {
                    match store.list_subnets(&v.name) {
                        Ok(mut subs) => all_subnets.append(&mut subs),
                        Err(e) => return OrgResponse::Error(e.to_string()),
                    }
                }
                OrgResponse::SubnetList(all_subnets)
            } else {
                OrgResponse::Error(
                    "specify --vpc or --env/--project/--org to list subnets".to_string(),
                )
            }
        }
        OrgRequest::SubnetDelete { name, vpc } => {
            let vpc_name = match vpc {
                Some(v) => v,
                None => {
                    let matches = match store.find_subnets_by_name(&name) {
                        Ok(m) => m,
                        Err(e) => return OrgResponse::Error(e.to_string()),
                    };
                    match matches.len() {
                        0 => return OrgResponse::Error(format!("subnet '{name}' not found")),
                        1 => matches.into_iter().next().unwrap().0,
                        _ => {
                            let vpc_names: Vec<String> =
                                matches.into_iter().map(|(v, _)| v).collect();
                            return OrgResponse::Error(format!(
                                "subnet '{name}' exists in multiple VPCs: {}. Specify --vpc",
                                vpc_names.join(", ")
                            ));
                        }
                    }
                }
            };
            match store.delete_subnet(&vpc_name, &name) {
                Ok(()) => OrgResponse::Ok,
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }

        // -- Security Group --
        OrgRequest::SgCreate {
            name,
            vpc,
            description,
        } => match store.create_security_group(&name, &vpc, &description) {
            Ok(sg) => OrgResponse::Sg(sg),
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::SgList { vpc } => match store.list_security_groups(vpc.as_deref()) {
            Ok(sgs) => OrgResponse::SgList(sgs),
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::SgShow { name, vpc } => match store.get_security_group(&name, vpc.as_deref()) {
            Ok(Some(sg)) => OrgResponse::Sg(sg),
            Ok(None) => OrgResponse::Error(format!("security group '{name}' not found")),
            Err(e) => OrgResponse::Error(e.to_string()),
        },
        OrgRequest::SgDelete { name, vpc } => {
            match store.delete_security_group(&name, vpc.as_deref()) {
                Ok(()) => OrgResponse::Ok,
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }
        OrgRequest::SgAttach { sg, vm, nic } => {
            let sg_record = match store.find_sg_by_name(&sg) {
                Ok(Some(s)) => s,
                Ok(None) => return OrgResponse::Error(format!("security group not found: {sg}")),
                Err(e) => return OrgResponse::Error(e.to_string()),
            };
            let sg_key = format!("{}/{}", sg_record.vpc_id.0, sg_record.name);

            let nic_id = match resolve_nic(store, vm.as_deref(), nic.as_deref()) {
                Ok(id) => id,
                Err(e) => return OrgResponse::Error(e),
            };

            match store.attach_sg_to_nic(&sg_key, &nic_id) {
                Ok(updated_nic) => OrgResponse::Nic(updated_nic),
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }
        OrgRequest::SgDetach { sg, vm, nic } => {
            let sg_record = match store.find_sg_by_name(&sg) {
                Ok(Some(s)) => s,
                Ok(None) => return OrgResponse::Error(format!("security group not found: {sg}")),
                Err(e) => return OrgResponse::Error(e.to_string()),
            };
            let sg_key = format!("{}/{}", sg_record.vpc_id.0, sg_record.name);

            let nic_id = match resolve_nic(store, vm.as_deref(), nic.as_deref()) {
                Ok(id) => id,
                Err(e) => return OrgResponse::Error(e),
            };

            match store.detach_sg_from_nic(&sg_key, &nic_id) {
                Ok(updated_nic) => OrgResponse::Nic(updated_nic),
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }
        OrgRequest::SgListForNic { vm, nic } => {
            let nic_id = match resolve_nic(store, vm.as_deref(), nic.as_deref()) {
                Ok(id) => id,
                Err(e) => return OrgResponse::Error(e),
            };
            match store.list_sgs_for_nic(&nic_id) {
                Ok(sgs) => OrgResponse::SgList(sgs),
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }

        // -- SG Rules --
        OrgRequest::SgAddRule {
            sg,
            direction,
            protocol,
            port,
            source,
            source_sg,
            description,
            priority,
        } => {
            let rule_store = match sg_rule_store {
                Some(s) => s,
                None => return OrgResponse::Error("SG rule store not available".to_string()),
            };

            let sg_record = match store.find_sg_by_name(&sg) {
                Ok(Some(s)) => s,
                Ok(None) => return OrgResponse::Error(format!("security group not found: {sg}")),
                Err(e) => return OrgResponse::Error(e.to_string()),
            };

            let dir = match direction.as_str() {
                "ingress" => Direction::Ingress,
                "egress" => Direction::Egress,
                other => {
                    return OrgResponse::Error(format!(
                        "invalid direction: '{other}' (expected ingress or egress)"
                    ))
                }
            };

            let proto = match protocol.as_str() {
                "tcp" => Protocol::Tcp,
                "udp" => Protocol::Udp,
                "icmp" => Protocol::Icmp,
                "all" => Protocol::All,
                other => {
                    return OrgResponse::Error(format!(
                        "invalid protocol: '{other}' (expected tcp, udp, icmp, or all)"
                    ))
                }
            };

            let port_range = match port {
                Some(ref p) => match parse_port_range(p) {
                    Ok(pr) => Some(pr),
                    Err(e) => return OrgResponse::Error(e),
                },
                None => None,
            };

            let rule_source = match (source, source_sg) {
                (Some(cidr), None) => RuleSource::Cidr(cidr),
                (None, Some(sg_name)) => {
                    let sg_ref = match store.find_sg_by_name(&sg_name) {
                        Ok(Some(s)) => s,
                        Ok(None) => {
                            return OrgResponse::Error(format!(
                                "source security group not found: {sg_name}"
                            ))
                        }
                        Err(e) => return OrgResponse::Error(e.to_string()),
                    };
                    RuleSource::SecurityGroup(sg_ref.id)
                }
                (None, None) => RuleSource::Cidr("0.0.0.0/0".to_string()),
                (Some(_), Some(_)) => {
                    return OrgResponse::Error(
                        "cannot specify both --source and --source-sg".to_string(),
                    )
                }
            };

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let rule_id = {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                now.hash(&mut hasher);
                sg_record.id.0.hash(&mut hasher);
                description.hash(&mut hasher);
                RuleId(format!("rule-{:016x}", hasher.finish()))
            };

            let rule = SecurityGroupRule {
                id: rule_id,
                sg_id: sg_record.id.clone(),
                direction: dir,
                protocol: proto,
                port_range,
                source: rule_source,
                priority,
                description: Some(description),
            };

            match rule_store.add_rule(&rule) {
                Ok(()) => OrgResponse::SgRule(rule),
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }

        OrgRequest::SgRemoveRule { sg, rule_id } => {
            let rule_store = match sg_rule_store {
                Some(s) => s,
                None => return OrgResponse::Error("SG rule store not available".to_string()),
            };

            // Verify the SG exists
            match store.find_sg_by_name(&sg) {
                Ok(Some(_)) => {}
                Ok(None) => return OrgResponse::Error(format!("security group not found: {sg}")),
                Err(e) => return OrgResponse::Error(e.to_string()),
            };

            match rule_store.remove_rule(&rule_id) {
                Ok(()) => OrgResponse::Ok,
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }

        OrgRequest::SgListRules { sg } => {
            let rule_store = match sg_rule_store {
                Some(s) => s,
                None => return OrgResponse::Error("SG rule store not available".to_string()),
            };

            let sg_record = match store.find_sg_by_name(&sg) {
                Ok(Some(s)) => s,
                Ok(None) => return OrgResponse::Error(format!("security group not found: {sg}")),
                Err(e) => return OrgResponse::Error(e.to_string()),
            };

            match rule_store.list_rules_by_sg(&sg_record.id) {
                Ok(rules) => OrgResponse::SgRuleList(rules),
                Err(e) => OrgResponse::Error(e.to_string()),
            }
        }

        OrgRequest::SgCheck {
            vm,
            port,
            protocol,
            source,
        } => {
            let rule_store = match sg_rule_store {
                Some(s) => s,
                None => return OrgResponse::Error("SG rule store not available".to_string()),
            };

            let proto = match protocol.as_str() {
                "tcp" => Protocol::Tcp,
                "udp" => Protocol::Udp,
                "icmp" => Protocol::Icmp,
                "all" => Protocol::All,
                other => {
                    return OrgResponse::Error(format!(
                        "invalid protocol: '{other}' (expected tcp, udp, icmp, or all)"
                    ))
                }
            };

            // Resolve VM -> NIC -> SGs -> rules
            let nic = match store.find_nic_by_vm(&vm) {
                Ok(Some(n)) => n,
                Ok(None) => return OrgResponse::Error(format!("VM '{vm}' has no NIC")),
                Err(e) => return OrgResponse::Error(e.to_string()),
            };

            let mut all_rules = Vec::new();
            for sg_id in &nic.security_groups {
                match rule_store.list_rules_by_sg(sg_id) {
                    Ok(rules) => all_rules.extend(rules),
                    Err(e) => return OrgResponse::Error(e.to_string()),
                }
            }

            let source_ip = source.as_deref().unwrap_or("0.0.0.0");
            let (verdict, reason) = evaluate_rules(&all_rules, port, proto, source_ip);

            OrgResponse::SgCheckResult { verdict, reason }
        }

        // -- Subnet resolution (for compute layer) --
        OrgRequest::SubnetResolve {
            subnet_name,
            env,
            project,
            org,
        } => {
            let (env_name, project_name, org_name) = match (&env, &project, &org) {
                (Some(e), Some(p), Some(o)) => (e.as_str(), p.as_str(), o.as_str()),
                (None, None, None) => {
                    if subnet_name.is_some() {
                        return OrgResponse::Error(
                            "--subnet requires --env, --project, and --org".to_string(),
                        );
                    }
                    return OrgResponse::SubnetResolved(None);
                }
                _ => {
                    return OrgResponse::Error(
                        "--env, --project, and --org must all be specified together".to_string(),
                    );
                }
            };

            let env_id = EnvironmentId(format!("{org_name}/{project_name}/{env_name}"));
            let subnets = match store.list_subnets_by_env(&env_id) {
                Ok(s) => s,
                Err(e) => return OrgResponse::Error(e.to_string()),
            };

            match subnet_name {
                Some(name) => {
                    let subnet = subnets.into_iter().find(|s| s.name == name);
                    match subnet {
                        Some(s) => OrgResponse::SubnetResolved(Some(ResolvedSubnet {
                            name: s.name,
                            cidr: s.cidr,
                            gateway: s.gateway,
                            vpc_id: s.vpc_id.0,
                            env_id: s.env_id.0,
                        })),
                        None => OrgResponse::Error(format!(
                            "subnet '{name}' not found in environment '{env_name}'"
                        )),
                    }
                }
                None => match subnets.len() {
                    0 => {
                        OrgResponse::Error(format!("no subnet found for environment '{env_name}'"))
                    }
                    1 => {
                        let s = subnets.into_iter().next().unwrap();
                        OrgResponse::SubnetResolved(Some(ResolvedSubnet {
                            name: s.name,
                            cidr: s.cidr,
                            gateway: s.gateway,
                            vpc_id: s.vpc_id.0,
                            env_id: s.env_id.0,
                        }))
                    }
                    _ => {
                        let names: Vec<&str> = subnets.iter().map(|s| s.name.as_str()).collect();
                        OrgResponse::Error(format!(
                            "environment '{env_name}' has multiple subnets: {}. Specify --subnet",
                            names.join(", ")
                        ))
                    }
                },
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Client-side helper — send an org request to the daemon
// ---------------------------------------------------------------------------

/// Send an org request to the daemon via the Unix control socket.
pub async fn send_org_request(
    socket_path: &Path,
    req: &OrgRequest,
) -> Result<OrgResponse, Box<dyn std::error::Error>> {
    let payload = serde_json::to_vec(req)?;
    let envelope = LayerRequest::Org(payload);

    let mut stream = UnixStream::connect(socket_path).await?;
    syfrah_api::transport::write_message(&mut stream, &envelope).await?;
    let resp: LayerResponse = syfrah_api::transport::read_message(&mut stream).await?;

    match resp {
        LayerResponse::Org(data) => {
            let org_resp: OrgResponse = serde_json::from_slice(&data)?;
            Ok(org_resp)
        }
        LayerResponse::UnknownLayer(name) => Err(format!("unknown layer: {name}").into()),
        other => Err(format!("unexpected response variant: {other:?}").into()),
    }
}

/// Parse a port range string like "443" or "8000-9000".
fn parse_port_range(s: &str) -> std::result::Result<PortRange, String> {
    if let Some((from_s, to_s)) = s.split_once('-') {
        let from: u16 = from_s
            .parse()
            .map_err(|_| format!("invalid port range: {s}"))?;
        let to: u16 = to_s
            .parse()
            .map_err(|_| format!("invalid port range: {s}"))?;
        if from > to {
            return Err(format!("from port ({from}) must be <= to port ({to})"));
        }
        Ok(PortRange { from, to })
    } else {
        let port: u16 = s.parse().map_err(|_| format!("invalid port: {s}"))?;
        Ok(PortRange {
            from: port,
            to: port,
        })
    }
}

/// Evaluate security group rules to determine if traffic is allowed.
///
/// Returns `(verdict, reason)` where verdict is "ALLOWED" or "DENIED".
fn evaluate_rules(
    rules: &[SecurityGroupRule],
    port: u16,
    protocol: Protocol,
    source_ip: &str,
) -> (String, String) {
    let mut ingress: Vec<&SecurityGroupRule> = rules
        .iter()
        .filter(|r| r.direction == Direction::Ingress)
        .collect();
    ingress.sort_by_key(|r| r.priority);

    for rule in &ingress {
        if rule_matches(rule, port, &protocol, source_ip) {
            let desc = rule.description.as_deref().unwrap_or("").to_string();
            let reason = if desc.is_empty() {
                format!("rule {} (priority {})", rule.id, rule.priority)
            } else {
                format!("rule {} — {} (priority {})", rule.id, desc, rule.priority)
            };
            return ("ALLOWED".to_string(), reason);
        }
    }

    ("DENIED".to_string(), "no matching ingress rule".to_string())
}

/// Check if a single rule matches the given traffic.
fn rule_matches(rule: &SecurityGroupRule, port: u16, protocol: &Protocol, source_ip: &str) -> bool {
    // Protocol match.
    if rule.protocol != Protocol::All && rule.protocol != *protocol {
        return false;
    }

    // Port match.
    if let Some(ref pr) = rule.port_range {
        if port < pr.from || port > pr.to {
            return false;
        }
    }

    // Source match.
    match &rule.source {
        RuleSource::Cidr(cidr) if cidr == "0.0.0.0/0" => {}
        RuleSource::Cidr(cidr) => {
            if !cidr_contains(cidr, source_ip) {
                return false;
            }
        }
        RuleSource::SecurityGroup(_) => {
            // SG-ref source requires membership check; skip for CLI check.
        }
    }

    true
}

/// Simple CIDR containment check.
fn cidr_contains(cidr: &str, ip: &str) -> bool {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return false;
    }
    let net: std::net::Ipv4Addr = match parts[0].parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let prefix_len: u32 = match parts[1].parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let check: std::net::Ipv4Addr = match ip.parse() {
        Ok(a) => a,
        Err(_) => return false,
    };

    if prefix_len == 0 {
        return true;
    }
    if prefix_len > 32 {
        return false;
    }

    let mask = !0u32 << (32 - prefix_len);
    let net_bits = u32::from(net) & mask;
    let check_bits = u32::from(check) & mask;
    net_bits == check_bits
}

/// Resolve a NIC ID from either a VM name or a direct NIC ID.
fn resolve_nic(
    store: &OrgStore,
    vm: Option<&str>,
    nic: Option<&str>,
) -> std::result::Result<String, String> {
    if let Some(nic_id) = nic {
        return Ok(nic_id.to_string());
    }
    if let Some(vm_id) = vm {
        match store.find_nic_by_vm(vm_id) {
            Ok(Some(n)) => Ok(n.id.0),
            Ok(None) => Err(format!("VM '{vm_id}' has no NIC")),
            Err(e) => Err(e.to_string()),
        }
    } else {
        Err("either --vm or --nic must be specified".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, Arc<OrgStore>) {
        let dir = tempfile::tempdir().unwrap();
        let db = syfrah_state::LayerDb::open_at(&dir.path().join("org.redb")).unwrap();
        (dir, Arc::new(OrgStore::new(db)))
    }

    #[tokio::test]
    async fn handler_returns_error_for_invalid_request() {
        let (_dir, store) = temp_store();
        let handler = OrgLayerHandler::new(store);
        let resp_bytes = handler.handle(b"not valid json".to_vec(), None).await;
        let resp: OrgResponse = serde_json::from_slice(&resp_bytes).unwrap();
        match resp {
            OrgResponse::Error(msg) => assert!(msg.contains("invalid org request")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn org_create_via_handler() {
        let (_dir, store) = temp_store();
        let handler = OrgLayerHandler::new(store);

        let req = OrgRequest::OrgCreate {
            name: "acme".to_string(),
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let resp_bytes = handler.handle(payload, None).await;
        let resp: OrgResponse = serde_json::from_slice(&resp_bytes).unwrap();
        match resp {
            OrgResponse::Org(org) => assert_eq!(org.name, "acme"),
            other => panic!("expected Org, got {other:?}"),
        }
    }
}
