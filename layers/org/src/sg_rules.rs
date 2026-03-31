//! Security group rule persistence — stores SG rules in redb.
//!
//! Backed by a redb table `sg_rules` with key `rule_id`.

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{
    Direction, PortRange, Protocol, RuleId, RuleSource, SecurityGroupId, SecurityGroupRule,
};

const TABLE: &str = "sg_rules";

/// Persistent store for security group rules backed by redb.
pub struct SgRuleStore {
    db: LayerDb,
}

impl SgRuleStore {
    /// Create a new `SgRuleStore` with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Validate a security group rule before persisting.
    fn validate(rule: &SecurityGroupRule) -> Result<()> {
        if let Some(ref pr) = rule.port_range {
            if pr.from == 0 || pr.to == 0 {
                return Err(OrgError::InvalidPortRange {
                    reason: "port must be between 1 and 65535".to_string(),
                });
            }
            if pr.from > pr.to {
                return Err(OrgError::InvalidPortRange {
                    reason: format!("from port ({}) must be <= to port ({})", pr.from, pr.to),
                });
            }
        }

        // If a port range is specified, protocol must not be All or Icmp
        // (ICMP doesn't use ports in the traditional sense, but we allow it
        // for type/code encoding; protocol All with ports is ambiguous).
        if rule.port_range.is_some() && rule.protocol == Protocol::Icmp {
            // ICMP with port range is acceptable (type/code), but we validate range
        }

        Ok(())
    }

    /// Add a rule. Validates port range. Returns error if rule_id already exists.
    pub fn add_rule(&self, rule: &SecurityGroupRule) -> Result<()> {
        Self::validate(rule)?;

        if self.db.exists(TABLE, &rule.id.0)? {
            return Err(OrgError::RuleAlreadyExists(rule.id.0.clone()));
        }

        self.db.set(TABLE, &rule.id.0, rule)?;
        Ok(())
    }

    /// Remove a rule by its ID. Returns error if it does not exist.
    pub fn remove_rule(&self, rule_id: &str) -> Result<()> {
        let existed = self.db.delete(TABLE, rule_id)?;
        if !existed {
            return Err(OrgError::RuleNotFound(rule_id.to_string()));
        }
        Ok(())
    }

    /// List all rules.
    pub fn list_rules(&self) -> Result<Vec<SecurityGroupRule>> {
        let entries: Vec<(String, SecurityGroupRule)> = self.db.list(TABLE)?;
        Ok(entries.into_iter().map(|(_, r)| r).collect())
    }

    /// List all rules belonging to a specific security group.
    pub fn list_rules_by_sg(&self, sg_id: &SecurityGroupId) -> Result<Vec<SecurityGroupRule>> {
        let entries: Vec<(String, SecurityGroupRule)> = self.db.list(TABLE)?;
        Ok(entries
            .into_iter()
            .filter(|(_, r)| r.sg_id == *sg_id)
            .map(|(_, r)| r)
            .collect())
    }

    /// Create the default rules for a security group (called when a default SG
    /// is created for a VPC).
    ///
    /// Default rules:
    /// - Ingress TCP 22 from VPC CIDR (SSH)
    /// - Ingress ICMP from VPC CIDR (ping)
    /// - No explicit egress (allow all by default)
    pub fn create_default_rules(
        &self,
        sg_id: &SecurityGroupId,
        vpc_cidr: &str,
    ) -> Result<Vec<SecurityGroupRule>> {
        let ssh_rule = SecurityGroupRule {
            id: RuleId(format!("{}-default-ssh", sg_id.0)),
            sg_id: sg_id.clone(),
            direction: Direction::Ingress,
            protocol: Protocol::Tcp,
            port_range: Some(PortRange { from: 22, to: 22 }),
            source: RuleSource::Cidr(vpc_cidr.to_string()),
            priority: 100,
            description: Some("Allow SSH from VPC CIDR".to_string()),
        };

        let icmp_rule = SecurityGroupRule {
            id: RuleId(format!("{}-default-icmp", sg_id.0)),
            sg_id: sg_id.clone(),
            direction: Direction::Ingress,
            protocol: Protocol::Icmp,
            port_range: None,
            source: RuleSource::Cidr(vpc_cidr.to_string()),
            priority: 100,
            description: Some("Allow ICMP from VPC CIDR".to_string()),
        };

        self.add_rule(&ssh_rule)?;
        self.add_rule(&icmp_rule)?;

        Ok(vec![ssh_rule, icmp_rule])
    }
}
