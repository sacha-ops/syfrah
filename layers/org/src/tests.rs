//! Full hierarchy integration test: Org -> Project -> Environment lifecycle.
//! VM placement persistence tests. Security group rule persistence tests.
//! NIC store tests.

use std::collections::HashMap;

use crate::error::OrgError;
use crate::nic::NicStore;
use crate::placement::PlacementStore;
use crate::sg_rules::SgRuleStore;
use crate::store::OrgStore;
use crate::types::{
    Direction, NetworkInterface, NicId, PlacementAction, PortRange, Protocol, ResourceState,
    RuleId, RuleSource, SecurityGroupId, SecurityGroupRule, VmPlacement,
};

fn temp_store() -> (tempfile::TempDir, OrgStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("org-test.redb");
    let db = syfrah_state::LayerDb::open_at(&path).unwrap();
    (dir, OrgStore::new(db))
}

fn temp_placement_store() -> (tempfile::TempDir, PlacementStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("placement-test.redb");
    let db = syfrah_state::LayerDb::open_at(&path).unwrap();
    (dir, PlacementStore::new(db))
}

#[test]
fn org_project_env_lifecycle() {
    let (_dir, store) = temp_store();

    // --- Create org "acme" ---
    let acme = store.create("acme").unwrap();
    assert_eq!(acme.name, "acme");

    // --- Create project "backend" under acme ---
    let backend = store.create_project("acme", "backend").unwrap();
    assert_eq!(backend.name, "backend");

    // --- Create project "frontend" under acme ---
    let frontend = store.create_project("acme", "frontend").unwrap();
    assert_eq!(frontend.name, "frontend");

    // --- Create env "production" under backend (with deletion_protection) ---
    let production = store
        .create_env(
            "acme",
            "backend",
            "production",
            None,
            true, // deletion_protection
            HashMap::new(),
        )
        .unwrap();
    assert!(production.deletion_protection);

    // --- Create env "staging" under backend (with TTL 48h) ---
    let staging = store
        .create_env(
            "acme",
            "backend",
            "staging",
            Some(48 * 3600),
            false,
            HashMap::new(),
        )
        .unwrap();
    assert_eq!(staging.ttl, Some(48 * 3600));
    assert!(staging.expires_at.is_some());

    // --- Create env "dev" under frontend (with labels team=fe) ---
    let mut dev_labels = HashMap::new();
    dev_labels.insert("team".to_string(), "fe".to_string());
    let dev = store
        .create_env("acme", "frontend", "dev", None, false, dev_labels.clone())
        .unwrap();
    assert_eq!(dev.labels, dev_labels);

    // --- List all: verify 1 org, 2 projects, 3 envs ---
    let orgs = store.list().unwrap();
    assert_eq!(orgs.len(), 1, "expected 1 org");
    assert_eq!(orgs[0].name, "acme");

    let backend_projects = store.list_projects("acme").unwrap();
    assert_eq!(backend_projects.len(), 2, "expected 2 projects");

    let backend_envs = store.list_envs("acme", "backend").unwrap();
    assert_eq!(backend_envs.len(), 2, "expected 2 backend envs");

    let frontend_envs = store.list_envs("acme", "frontend").unwrap();
    assert_eq!(frontend_envs.len(), 1, "expected 1 frontend env");

    // --- Try delete project "backend" -> rejected (has envs) ---
    let err = store.delete_project("acme", "backend").unwrap_err();
    assert!(
        matches!(err, OrgError::ProjectHasEnvironments { .. }),
        "expected ProjectHasEnvironments, got: {err}"
    );

    // --- Delete env "staging" -> OK ---
    store.delete_env("acme", "backend", "staging").unwrap();

    // --- Try delete env "production" -> rejected (protected) ---
    let err = store
        .delete_env("acme", "backend", "production")
        .unwrap_err();
    assert!(
        matches!(err, OrgError::EnvProtected(_)),
        "expected EnvProtected, got: {err}"
    );

    // --- Unprotect "production", then delete -> OK ---
    store
        .update_env_protection("acme", "backend", "production", false)
        .unwrap();
    store.delete_env("acme", "backend", "production").unwrap();

    // --- Delete env "dev" -> OK ---
    store.delete_env("acme", "frontend", "dev").unwrap();

    // --- Delete project "backend" -> OK (no more envs) ---
    store.delete_project("acme", "backend").unwrap();

    // --- Delete project "frontend" -> OK ---
    store.delete_project("acme", "frontend").unwrap();

    // --- Delete org "acme" -> OK ---
    store.delete("acme").unwrap();

    // --- Verify all empty ---
    assert_eq!(store.list().unwrap().len(), 0, "orgs should be empty");
    assert_eq!(
        store.list_projects("acme").unwrap().len(),
        0,
        "projects should be empty"
    );
}

// ── VM Placement tests ──────────────────────────────────────────────

fn make_placement(vpc: &str, vm: &str, node: &str, subnet: &str) -> VmPlacement {
    VmPlacement {
        vpc_id: vpc.to_string(),
        vm_id: vm.to_string(),
        vm_mac: format!("02:00:0a:00:01:{:02x}", vm.len()),
        vm_ip: format!("10.0.1.{}", vm.len()),
        subnet_id: subnet.to_string(),
        hosting_node: node.to_string(),
        action: PlacementAction::Add,
        created_at: 1700000000,
    }
}

#[test]
fn create_placement() {
    let (_dir, store) = temp_placement_store();

    let p = make_placement("vpc-1", "vm-1", "fd00::1", "subnet-1");
    store.add_placement(&p).unwrap();

    let got = store.get_placement("vpc-1", "vm-1").unwrap();
    assert_eq!(got, Some(p));
}

#[test]
fn delete_placement() {
    let (_dir, store) = temp_placement_store();

    let p = make_placement("vpc-1", "vm-1", "fd00::1", "subnet-1");
    store.add_placement(&p).unwrap();

    store.remove_placement("vpc-1", "vm-1").unwrap();

    let got = store.get_placement("vpc-1", "vm-1").unwrap();
    assert!(got.is_none(), "placement should be gone after removal");

    // Removing again should error.
    let err = store.remove_placement("vpc-1", "vm-1").unwrap_err();
    assert!(
        matches!(err, OrgError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
}

#[test]
fn list_by_vpc() {
    let (_dir, store) = temp_placement_store();

    // Two VMs in vpc-1, one in vpc-2.
    store
        .add_placement(&make_placement("vpc-1", "vm-1", "fd00::1", "subnet-1"))
        .unwrap();
    store
        .add_placement(&make_placement("vpc-1", "vm-2", "fd00::2", "subnet-1"))
        .unwrap();
    store
        .add_placement(&make_placement("vpc-2", "vm-3", "fd00::1", "subnet-2"))
        .unwrap();

    let vpc1 = store.list_by_vpc("vpc-1").unwrap();
    assert_eq!(vpc1.len(), 2, "expected 2 placements in vpc-1");
    assert!(vpc1.iter().all(|p| p.vpc_id == "vpc-1"));

    let vpc2 = store.list_by_vpc("vpc-2").unwrap();
    assert_eq!(vpc2.len(), 1, "expected 1 placement in vpc-2");

    let vpc3 = store.list_by_vpc("vpc-3").unwrap();
    assert_eq!(vpc3.len(), 0, "expected 0 placements in vpc-3");
}

#[test]
fn list_by_node() {
    let (_dir, store) = temp_placement_store();

    // Two VMs on node fd00::1, one on fd00::2.
    store
        .add_placement(&make_placement("vpc-1", "vm-1", "fd00::1", "subnet-1"))
        .unwrap();
    store
        .add_placement(&make_placement("vpc-2", "vm-3", "fd00::1", "subnet-2"))
        .unwrap();
    store
        .add_placement(&make_placement("vpc-1", "vm-2", "fd00::2", "subnet-1"))
        .unwrap();

    let node1 = store.list_by_node("fd00::1").unwrap();
    assert_eq!(node1.len(), 2, "expected 2 placements on fd00::1");
    assert!(node1.iter().all(|p| p.hosting_node == "fd00::1"));

    let node2 = store.list_by_node("fd00::2").unwrap();
    assert_eq!(node2.len(), 1, "expected 1 placement on fd00::2");

    let node3 = store.list_by_node("fd00::99").unwrap();
    assert_eq!(node3.len(), 0, "expected 0 placements on fd00::99");
}

// ── Security Group Rule tests ──────────────────────────────────────

fn temp_sg_rule_store() -> (tempfile::TempDir, SgRuleStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sg-rule-test.redb");
    let db = syfrah_state::LayerDb::open_at(&path).unwrap();
    (dir, SgRuleStore::new(db))
}

fn make_rule(id: &str, sg_id: &str, direction: Direction, protocol: Protocol) -> SecurityGroupRule {
    SecurityGroupRule {
        id: RuleId(id.to_string()),
        sg_id: SecurityGroupId(sg_id.to_string()),
        direction,
        protocol,
        port_range: Some(PortRange { from: 80, to: 80 }),
        source: RuleSource::Cidr("10.0.0.0/16".to_string()),
        priority: 100,
        description: Some("test rule".to_string()),
    }
}

#[test]
fn add_rule() {
    let (_dir, store) = temp_sg_rule_store();

    let rule = make_rule("rule-1", "sg-1", Direction::Ingress, Protocol::Tcp);
    store.add_rule(&rule).unwrap();

    let rules = store.list_rules().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].id.0, "rule-1");
    assert_eq!(rules[0].sg_id.0, "sg-1");
}

#[test]
fn remove_rule() {
    let (_dir, store) = temp_sg_rule_store();

    let rule = make_rule("rule-1", "sg-1", Direction::Ingress, Protocol::Tcp);
    store.add_rule(&rule).unwrap();

    store.remove_rule("rule-1").unwrap();

    let rules = store.list_rules().unwrap();
    assert!(rules.is_empty(), "rules should be empty after removal");

    // Removing again should error.
    let err = store.remove_rule("rule-1").unwrap_err();
    assert!(
        matches!(err, OrgError::RuleNotFound(_)),
        "expected RuleNotFound, got: {err}"
    );
}

#[test]
fn list_rules_by_sg() {
    let (_dir, store) = temp_sg_rule_store();

    // Two rules in sg-1, one in sg-2.
    store
        .add_rule(&make_rule("r1", "sg-1", Direction::Ingress, Protocol::Tcp))
        .unwrap();
    store
        .add_rule(&make_rule("r2", "sg-1", Direction::Egress, Protocol::Udp))
        .unwrap();
    store
        .add_rule(&make_rule("r3", "sg-2", Direction::Ingress, Protocol::All))
        .unwrap();

    let sg1_rules = store
        .list_rules_by_sg(&SecurityGroupId("sg-1".to_string()))
        .unwrap();
    assert_eq!(sg1_rules.len(), 2, "expected 2 rules in sg-1");
    assert!(sg1_rules.iter().all(|r| r.sg_id.0 == "sg-1"));

    let sg2_rules = store
        .list_rules_by_sg(&SecurityGroupId("sg-2".to_string()))
        .unwrap();
    assert_eq!(sg2_rules.len(), 1, "expected 1 rule in sg-2");

    let sg3_rules = store
        .list_rules_by_sg(&SecurityGroupId("sg-3".to_string()))
        .unwrap();
    assert_eq!(sg3_rules.len(), 0, "expected 0 rules in sg-3");
}

#[test]
fn default_rules_created() {
    let (_dir, store) = temp_sg_rule_store();

    let sg_id = SecurityGroupId("sg-default".to_string());
    let rules = store.create_default_rules(&sg_id, "10.0.0.0/16").unwrap();

    assert_eq!(rules.len(), 2, "expected 2 default rules");

    // SSH rule
    let ssh = rules.iter().find(|r| r.protocol == Protocol::Tcp).unwrap();
    assert_eq!(ssh.direction, Direction::Ingress);
    assert_eq!(ssh.port_range, Some(PortRange { from: 22, to: 22 }));
    assert_eq!(ssh.source, RuleSource::Cidr("10.0.0.0/16".to_string()));

    // ICMP rule
    let icmp = rules.iter().find(|r| r.protocol == Protocol::Icmp).unwrap();
    assert_eq!(icmp.direction, Direction::Ingress);
    assert!(icmp.port_range.is_none());
    assert_eq!(icmp.source, RuleSource::Cidr("10.0.0.0/16".to_string()));

    // Verify they are persisted
    let persisted = store.list_rules_by_sg(&sg_id).unwrap();
    assert_eq!(persisted.len(), 2, "default rules should be persisted");
}

#[test]
fn invalid_port_rejected() {
    let (_dir, store) = temp_sg_rule_store();

    // Port 0 is invalid
    let mut rule = make_rule("r-bad", "sg-1", Direction::Ingress, Protocol::Tcp);
    rule.port_range = Some(PortRange { from: 0, to: 80 });

    let err = store.add_rule(&rule).unwrap_err();
    assert!(
        matches!(err, OrgError::InvalidPortRange { .. }),
        "expected InvalidPortRange, got: {err}"
    );

    // from > to is invalid
    rule.port_range = Some(PortRange { from: 8080, to: 80 });
    let err = store.add_rule(&rule).unwrap_err();
    assert!(
        matches!(err, OrgError::InvalidPortRange { .. }),
        "expected InvalidPortRange, got: {err}"
    );
}

#[test]
fn duplicate_rule_handling() {
    let (_dir, store) = temp_sg_rule_store();

    let rule = make_rule("rule-dup", "sg-1", Direction::Ingress, Protocol::Tcp);
    store.add_rule(&rule).unwrap();

    // Adding the same rule_id again should fail
    let err = store.add_rule(&rule).unwrap_err();
    assert!(
        matches!(err, OrgError::RuleAlreadyExists(_)),
        "expected RuleAlreadyExists, got: {err}"
    );

    // Only one rule should exist
    let rules = store.list_rules().unwrap();
    assert_eq!(
        rules.len(),
        1,
        "should have exactly 1 rule, not a duplicate"
    );
}

// ── NIC tests ──────────────────────────────────────────────────────

fn temp_nic_store() -> (tempfile::TempDir, NicStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nic-test.redb");
    let db = syfrah_state::LayerDb::open_at(&path).unwrap();
    (dir, NicStore::new(db))
}

fn make_nic(id: &str, vm_id: Option<&str>, subnet: &str, vpc: &str) -> NetworkInterface {
    NetworkInterface {
        id: NicId(id.to_string()),
        name: format!("nic-{id}"),
        vm_id: vm_id.map(|s| s.to_string()),
        subnet_id: subnet.to_string(),
        vpc_id: vpc.to_string(),
        private_ip: "10.0.1.10".to_string(),
        mac: "02:00:0a:00:01:0a".to_string(),
        security_groups: vec![],
        state: ResourceState::Active,
        created_at: 1700000000,
    }
}

#[test]
fn create_and_get_nic() {
    let (_dir, store) = temp_nic_store();

    let nic = make_nic("nic-1", Some("vm-1"), "subnet-1", "vpc-1");
    store.create_nic(&nic).unwrap();

    let got = store.get_nic("nic-1").unwrap();
    assert_eq!(got, Some(nic));
}

#[test]
fn create_nic_duplicate_rejected() {
    let (_dir, store) = temp_nic_store();

    let nic = make_nic("nic-1", Some("vm-1"), "subnet-1", "vpc-1");
    store.create_nic(&nic).unwrap();

    let err = store.create_nic(&nic).unwrap_err();
    assert!(
        matches!(err, OrgError::NicAlreadyExists(_)),
        "expected NicAlreadyExists, got: {err}"
    );
}

#[test]
fn list_nics_by_vm() {
    let (_dir, store) = temp_nic_store();

    store
        .create_nic(&make_nic("nic-1", Some("vm-1"), "subnet-1", "vpc-1"))
        .unwrap();
    store
        .create_nic(&make_nic("nic-2", Some("vm-1"), "subnet-2", "vpc-1"))
        .unwrap();
    store
        .create_nic(&make_nic("nic-3", Some("vm-2"), "subnet-1", "vpc-1"))
        .unwrap();

    let vm1_nics = store.list_nics_by_vm("vm-1").unwrap();
    assert_eq!(vm1_nics.len(), 2, "expected 2 NICs for vm-1");
    assert!(vm1_nics.iter().all(|n| n.vm_id.as_deref() == Some("vm-1")));

    let vm2_nics = store.list_nics_by_vm("vm-2").unwrap();
    assert_eq!(vm2_nics.len(), 1, "expected 1 NIC for vm-2");

    let vm3_nics = store.list_nics_by_vm("vm-99").unwrap();
    assert_eq!(vm3_nics.len(), 0, "expected 0 NICs for vm-99");
}

#[test]
fn list_nics_by_subnet() {
    let (_dir, store) = temp_nic_store();

    store
        .create_nic(&make_nic("nic-1", Some("vm-1"), "subnet-1", "vpc-1"))
        .unwrap();
    store
        .create_nic(&make_nic("nic-2", Some("vm-2"), "subnet-1", "vpc-1"))
        .unwrap();
    store
        .create_nic(&make_nic("nic-3", Some("vm-3"), "subnet-2", "vpc-1"))
        .unwrap();

    let s1 = store.list_nics_by_subnet("subnet-1").unwrap();
    assert_eq!(s1.len(), 2, "expected 2 NICs in subnet-1");

    let s2 = store.list_nics_by_subnet("subnet-2").unwrap();
    assert_eq!(s2.len(), 1, "expected 1 NIC in subnet-2");
}

#[test]
fn attach_sg_to_nic() {
    let (_dir, store) = temp_nic_store();

    store
        .create_nic(&make_nic("nic-1", Some("vm-1"), "subnet-1", "vpc-1"))
        .unwrap();

    let sg = SecurityGroupId("sg-default".to_string());
    store.attach_sg_to_nic("nic-1", &sg).unwrap();

    let nic = store.get_nic("nic-1").unwrap().unwrap();
    assert_eq!(nic.security_groups, vec![sg.clone()]);

    // Attaching the same SG again is a no-op.
    store.attach_sg_to_nic("nic-1", &sg).unwrap();
    let nic = store.get_nic("nic-1").unwrap().unwrap();
    assert_eq!(nic.security_groups.len(), 1, "SG should not be duplicated");
}

#[test]
fn detach_sg_from_nic() {
    let (_dir, store) = temp_nic_store();

    let mut nic = make_nic("nic-1", Some("vm-1"), "subnet-1", "vpc-1");
    let sg = SecurityGroupId("sg-default".to_string());
    nic.security_groups.push(sg.clone());
    store.create_nic(&nic).unwrap();

    store.detach_sg_from_nic("nic-1", &sg).unwrap();

    let nic = store.get_nic("nic-1").unwrap().unwrap();
    assert!(nic.security_groups.is_empty(), "SG should be detached");

    // Detaching again should error.
    let err = store.detach_sg_from_nic("nic-1", &sg).unwrap_err();
    assert!(
        matches!(err, OrgError::SgNotAttached { .. }),
        "expected SgNotAttached, got: {err}"
    );
}

#[test]
fn delete_nic() {
    let (_dir, store) = temp_nic_store();

    store
        .create_nic(&make_nic("nic-1", Some("vm-1"), "subnet-1", "vpc-1"))
        .unwrap();

    store.delete_nic("nic-1").unwrap();

    let nic = store.get_nic("nic-1").unwrap().unwrap();
    assert_eq!(nic.state, ResourceState::Deleted, "NIC should be Deleted");

    // Deleting a non-existent NIC should error.
    let err = store.delete_nic("nic-999").unwrap_err();
    assert!(
        matches!(err, OrgError::NicNotFound(_)),
        "expected NicNotFound, got: {err}"
    );
}
