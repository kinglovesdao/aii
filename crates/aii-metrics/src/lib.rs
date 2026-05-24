//! # aii-metrics
//!
//! Lock-protected metric registry rendered in Prometheus text format.
//!
//! ## Public API
//! - [`Registry`] — owns counters and gauges; thread-safe; `render()`
//!   serialises in Prometheus format
//! - [`Counter`] / [`Gauge`] — atomic-ish handles returned from
//!   `Registry::counter` / `Registry::gauge`

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

/// Monotonically non-decreasing counter.
#[derive(Debug, Clone)]
pub struct Counter(Arc<AtomicU64>);

impl Counter {
    /// Increment by 1.
    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment by `n`.
    pub fn inc_by(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    /// Read the current value.
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Signed gauge (may go up or down).
#[derive(Debug, Clone)]
pub struct Gauge(Arc<AtomicI64>);

impl Gauge {
    /// Set the gauge to `v`.
    pub fn set(&self, v: i64) {
        self.0.store(v, Ordering::Relaxed);
    }

    /// Add `delta` (negative subtracts).
    pub fn add(&self, delta: i64) {
        self.0.fetch_add(delta, Ordering::Relaxed);
    }

    /// Read the current value.
    pub fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
enum Metric {
    Counter { c: Counter, help: &'static str },
    Gauge { g: Gauge, help: &'static str },
}

/// Process-wide metric registry. Cheap to clone (shares state via `Arc`).
#[derive(Debug, Clone, Default)]
pub struct Registry {
    inner: Arc<RwLock<BTreeMap<&'static str, Metric>>>,
}

impl Registry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or return the existing) counter named `name`.
    ///
    /// If `name` is already registered as a gauge, this returns a fresh
    /// counter (overwriting) — callers should pick stable names.
    pub fn counter(&self, name: &'static str, help: &'static str) -> Counter {
        let mut g = self.inner.write();
        if let Some(Metric::Counter { c, .. }) = g.get(name) {
            return c.clone();
        }
        let c = Counter(Arc::new(AtomicU64::new(0)));
        g.insert(name, Metric::Counter { c: c.clone(), help });
        c
    }

    /// Register (or return the existing) gauge named `name`.
    pub fn gauge(&self, name: &'static str, help: &'static str) -> Gauge {
        let mut g = self.inner.write();
        if let Some(Metric::Gauge { g: existing, .. }) = g.get(name) {
            return existing.clone();
        }
        let gg = Gauge(Arc::new(AtomicI64::new(0)));
        g.insert(
            name,
            Metric::Gauge {
                g: gg.clone(),
                help,
            },
        );
        gg
    }

    /// Render all metrics in Prometheus text exposition format.
    pub fn render(&self) -> String {
        let snapshot: Vec<(&'static str, Metric)> = {
            let g = self.inner.read();
            g.iter().map(|(k, v)| (*k, v.clone())).collect()
        };
        let mut out = String::new();
        for (name, m) in &snapshot {
            match m {
                Metric::Counter { c, help } => {
                    out.push_str(&format!("# HELP {name} {help}\n"));
                    out.push_str(&format!("# TYPE {name} counter\n"));
                    out.push_str(&format!("{name} {}\n", c.get()));
                }
                Metric::Gauge { g, help } => {
                    out.push_str(&format!("# HELP {name} {help}\n"));
                    out.push_str(&format!("# TYPE {name} gauge\n"));
                    out.push_str(&format!("{name} {}\n", g.get()));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_increments() {
        let r = Registry::new();
        let c = r.counter("block_total", "Blocks observed");
        c.inc();
        c.inc_by(4);
        assert_eq!(c.get(), 5);
    }

    #[test]
    fn gauge_set_and_add() {
        let r = Registry::new();
        let g = r.gauge("peer_count", "Connected peers");
        g.set(10);
        g.add(-3);
        assert_eq!(g.get(), 7);
    }

    #[test]
    fn counter_handle_is_shared() {
        let r = Registry::new();
        let c1 = r.counter("tx_total", "Transactions accepted");
        let c2 = r.counter("tx_total", "Transactions accepted");
        c1.inc();
        assert_eq!(c2.get(), 1);
    }

    #[test]
    fn render_emits_help_type_and_value() {
        let r = Registry::new();
        let c = r.counter("block_total", "Blocks observed");
        c.inc_by(3);
        let txt = r.render();
        assert!(txt.contains("# HELP block_total Blocks observed"));
        assert!(txt.contains("# TYPE block_total counter"));
        assert!(txt.contains("block_total 3"));
    }

    #[test]
    fn render_includes_both_kinds() {
        let r = Registry::new();
        r.counter("c1", "c1 help").inc();
        r.gauge("g1", "g1 help").set(42);
        let txt = r.render();
        assert!(txt.contains("c1 1"));
        assert!(txt.contains("g1 42"));
    }

    #[test]
    fn registry_is_send_sync_and_clone() {
        fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
        assert_send_sync_clone::<Registry>();
        assert_send_sync_clone::<Counter>();
        assert_send_sync_clone::<Gauge>();
    }
}
