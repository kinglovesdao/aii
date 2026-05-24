# aii-metrics

Lightweight, dependency-free metrics for the AII node. Exposes counters
and gauges through a `Registry`, and renders them in Prometheus text format
via `Registry::render()`.

No HTTP server in this crate — embedders wire `render()` into the
process's existing HTTP stack (typically `aii-rpc`).

## Conventions

- Metric names use `snake_case`, prefixed with the subsystem (`block_*`,
  `tx_*`, `consensus_*`, `net_*`, `db_*`).
- Counters are monotonically non-decreasing.
- Gauges may go up or down.
- All registrations are thread-safe.
