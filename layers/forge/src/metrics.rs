//! Prometheus metrics endpoint.
//!
//! Exposes GET /metrics in Prometheus text exposition format.
//! Metrics:
//! - forge_instances_total{state} — instance count by state
//! - forge_reconciliation_duration_seconds — last reconciliation duration
//! - forge_api_requests_total{method,path,status} — API request counter
//! - forge_node_cpu_used_ratio — CPU utilization ratio
//! - forge_node_memory_used_ratio — memory utilization ratio

use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Collected metrics for Prometheus exposition.
pub struct MetricsCollector {
    /// API request counters: (method, path, status_code) -> count.
    api_requests: Mutex<Vec<(String, String, u16, u64)>>,
    /// Last reconciliation duration in microseconds.
    pub last_reconcile_duration_us: AtomicU64,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            api_requests: Mutex::new(Vec::new()),
            last_reconcile_duration_us: AtomicU64::new(0),
        }
    }

    /// Record an API request.
    pub fn record_request(&self, method: &str, path: &str, status: u16) {
        let mut reqs = self.api_requests.lock().unwrap();
        // Find existing entry or create new.
        if let Some(entry) = reqs
            .iter_mut()
            .find(|(m, p, s, _)| m == method && p == path && *s == status)
        {
            entry.3 += 1;
        } else {
            reqs.push((method.to_string(), path.to_string(), status, 1));
        }
    }

    /// Render metrics in Prometheus text exposition format.
    pub fn render(
        &self,
        vm_count: usize,
        running_count: usize,
        stopped_count: usize,
        cpu_used_ratio: f64,
        memory_used_ratio: f64,
    ) -> String {
        let mut out = String::with_capacity(2048);

        // Instance counts by state
        let _ = writeln!(
            out,
            "# HELP forge_instances_total Number of instances by state."
        );
        let _ = writeln!(out, "# TYPE forge_instances_total gauge");
        let _ = writeln!(
            out,
            "forge_instances_total{{state=\"running\"}} {running_count}"
        );
        let _ = writeln!(
            out,
            "forge_instances_total{{state=\"stopped\"}} {stopped_count}"
        );
        let _ = writeln!(out, "forge_instances_total{{state=\"total\"}} {vm_count}");

        // Reconciliation duration
        let recon_us = self.last_reconcile_duration_us.load(Ordering::Relaxed);
        let recon_secs = recon_us as f64 / 1_000_000.0;
        let _ = writeln!(
            out,
            "# HELP forge_reconciliation_duration_seconds Duration of last reconciliation cycle."
        );
        let _ = writeln!(out, "# TYPE forge_reconciliation_duration_seconds gauge");
        let _ = writeln!(out, "forge_reconciliation_duration_seconds {recon_secs:.6}");

        // API request counts
        let reqs = self.api_requests.lock().unwrap();
        if !reqs.is_empty() {
            let _ = writeln!(
                out,
                "# HELP forge_api_requests_total Total API requests by method, path, and status."
            );
            let _ = writeln!(out, "# TYPE forge_api_requests_total counter");
            for (method, path, status, count) in reqs.iter() {
                let _ = writeln!(
                    out,
                    "forge_api_requests_total{{method=\"{method}\",path=\"{path}\",status=\"{status}\"}} {count}"
                );
            }
        }

        // CPU and memory ratios
        let _ = writeln!(
            out,
            "# HELP forge_node_cpu_used_ratio CPU utilization ratio (used/allocatable)."
        );
        let _ = writeln!(out, "# TYPE forge_node_cpu_used_ratio gauge");
        let _ = writeln!(out, "forge_node_cpu_used_ratio {cpu_used_ratio:.4}");

        let _ = writeln!(
            out,
            "# HELP forge_node_memory_used_ratio Memory utilization ratio (used/allocatable)."
        );
        let _ = writeln!(out, "# TYPE forge_node_memory_used_ratio gauge");
        let _ = writeln!(out, "forge_node_memory_used_ratio {memory_used_ratio:.4}");

        out
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_basic_metrics() {
        let collector = MetricsCollector::new();
        let output = collector.render(3, 2, 1, 0.5, 0.75);

        assert!(output.contains("forge_instances_total{state=\"running\"} 2"));
        assert!(output.contains("forge_instances_total{state=\"stopped\"} 1"));
        assert!(output.contains("forge_instances_total{state=\"total\"} 3"));
        assert!(output.contains("forge_node_cpu_used_ratio 0.5000"));
        assert!(output.contains("forge_node_memory_used_ratio 0.7500"));
        assert!(output.contains("forge_reconciliation_duration_seconds 0.000000"));
    }

    #[test]
    fn render_with_api_requests() {
        let collector = MetricsCollector::new();
        collector.record_request("GET", "/v1/hypervisor/health", 200);
        collector.record_request("GET", "/v1/hypervisor/health", 200);
        collector.record_request("POST", "/v1/instances", 201);

        let output = collector.render(0, 0, 0, 0.0, 0.0);
        assert!(output.contains("forge_api_requests_total{method=\"GET\",path=\"/v1/hypervisor/health\",status=\"200\"} 2"));
        assert!(output.contains(
            "forge_api_requests_total{method=\"POST\",path=\"/v1/instances\",status=\"201\"} 1"
        ));
    }

    #[test]
    fn record_reconcile_duration() {
        let collector = MetricsCollector::new();
        collector
            .last_reconcile_duration_us
            .store(1_500_000, Ordering::Relaxed);

        let output = collector.render(0, 0, 0, 0.0, 0.0);
        assert!(output.contains("forge_reconciliation_duration_seconds 1.500000"));
    }
}
