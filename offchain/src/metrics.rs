//! Lightweight, dependency-free observability (Prometheus text exposition).
//!
//! Design goals: (1) **no heavy deps** — counters/gauges/histograms are plain atomics, so
//! this compiles in every build and adds ~nothing to the binary; (2) **minimal hot-path
//! cost** — every record is one or a few relaxed atomic ops, no allocation, no locking;
//! (3) **fixed label sets** — label dimensions are small enums backed by arrays, so there
//! is no per-label map lookup. Call the free functions (`record_stage`, `inc_*`, `set_*`)
//! from anywhere; `render()` produces the `/metrics` body and `serve()` exposes it.
//!
//! Execution logic is never changed by instrumentation — these are additive observations.

use std::array;
use std::future::Future;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

// ── Label dimensions (fixed sets → array-indexed, lock-free) ──────────────────────────

/// Pipeline stage for latency histograms.
#[derive(Clone, Copy, Debug)]
pub enum Stage {
    Ingestion,
    Scanner,
    Sizing,
    Executor,
    PtbBuild,
    DryRun,
    Submit,
}
impl Stage {
    pub const ALL: [Stage; 7] = [
        Stage::Ingestion,
        Stage::Scanner,
        Stage::Sizing,
        Stage::Executor,
        Stage::PtbBuild,
        Stage::DryRun,
        Stage::Submit,
    ];
    fn as_str(self) -> &'static str {
        match self {
            Stage::Ingestion => "ingestion",
            Stage::Scanner => "scanner",
            Stage::Sizing => "sizing",
            Stage::Executor => "executor",
            Stage::PtbBuild => "ptb_build",
            Stage::DryRun => "dry_run",
            Stage::Submit => "submit",
        }
    }
}

/// RPC method for per-endpoint latency histograms.
#[derive(Clone, Copy, Debug)]
pub enum Rpc {
    GetObject,
    MultiGetObject,
    DryRun,
    DevInspect,
    GasPrice,
    LatestCheckpoint,
    ExecuteTx,
    QueryEvents,
    DynamicFields,
    Hermes,
    Other,
}
impl Rpc {
    pub const ALL: [Rpc; 11] = [
        Rpc::GetObject,
        Rpc::MultiGetObject,
        Rpc::DryRun,
        Rpc::DevInspect,
        Rpc::GasPrice,
        Rpc::LatestCheckpoint,
        Rpc::ExecuteTx,
        Rpc::QueryEvents,
        Rpc::DynamicFields,
        Rpc::Hermes,
        Rpc::Other,
    ];
    fn as_str(self) -> &'static str {
        match self {
            Rpc::GetObject => "get_object",
            Rpc::MultiGetObject => "multi_get_object",
            Rpc::DryRun => "dry_run",
            Rpc::DevInspect => "dev_inspect",
            Rpc::GasPrice => "gas_price",
            Rpc::LatestCheckpoint => "latest_checkpoint",
            Rpc::ExecuteTx => "execute_tx",
            Rpc::QueryEvents => "query_events",
            Rpc::DynamicFields => "dynamic_fields",
            Rpc::Hermes => "hermes",
            Rpc::Other => "other",
        }
    }
}

/// Reason a candidate was rejected (for the grouped counter).
#[derive(Clone, Copy, Debug)]
pub enum Reject {
    NotProfitable,
    BelowMinProfit,
    DryRunRevert,
    RiskGuard,
    Blacklisted,
    NoLiveRef,
    BuildError,
}
impl Reject {
    pub const ALL: [Reject; 7] = [
        Reject::NotProfitable,
        Reject::BelowMinProfit,
        Reject::DryRunRevert,
        Reject::RiskGuard,
        Reject::Blacklisted,
        Reject::NoLiveRef,
        Reject::BuildError,
    ];
    fn as_str(self) -> &'static str {
        match self {
            Reject::NotProfitable => "not_profitable",
            Reject::BelowMinProfit => "below_min_profit",
            Reject::DryRunRevert => "dry_run_revert",
            Reject::RiskGuard => "risk_guard",
            Reject::Blacklisted => "blacklisted_pool",
            Reject::NoLiveRef => "no_live_ref",
            Reject::BuildError => "build_error",
        }
    }
}

/// Opportunity source kind (for the opportunities counter).
#[derive(Clone, Copy, Debug)]
pub enum Opp {
    Arb,
    Liquidation,
    Backrun,
}
impl Opp {
    pub const ALL: [Opp; 3] = [Opp::Arb, Opp::Liquidation, Opp::Backrun];
    fn as_str(self) -> &'static str {
        match self {
            Opp::Arb => "arb",
            Opp::Liquidation => "liquidation",
            Opp::Backrun => "backrun",
        }
    }
}

/// DEX label for the indexer's per-venue discovered-pool gauge.
#[derive(Clone, Copy, Debug)]
pub enum IndexDex {
    Cetus,
    Turbos,
    DeepBook,
    Other,
}
impl IndexDex {
    pub const ALL: [IndexDex; 4] = [
        IndexDex::Cetus,
        IndexDex::Turbos,
        IndexDex::DeepBook,
        IndexDex::Other,
    ];
    fn as_str(self) -> &'static str {
        match self {
            IndexDex::Cetus => "cetus",
            IndexDex::Turbos => "turbos",
            IndexDex::DeepBook => "deepbook",
            IndexDex::Other => "other",
        }
    }
}

// ── Primitive instruments ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct Counter(AtomicU64);
impl Counter {
    fn inc(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }
    fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct Gauge(AtomicI64);
impl Gauge {
    fn set(&self, v: i64) {
        self.0.store(v, Ordering::Relaxed);
    }
    fn add(&self, v: i64) {
        self.0.fetch_add(v, Ordering::Relaxed);
    }
    fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Latency histogram with fixed second-buckets. Stores per-bucket counts (non-cumulative),
/// total count, and the sum of observations in microseconds (integer, overflow-safe for
/// centuries). Rendered as cumulative `le` buckets per the Prometheus convention.
const BUCKETS_S: [f64; 12] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
];
struct Histogram {
    buckets: [AtomicU64; 12],
    count: AtomicU64,
    sum_micros: AtomicU64,
}
impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum_micros: AtomicU64::new(0),
        }
    }
}
impl Histogram {
    fn observe(&self, secs: f64) {
        let secs = if secs.is_finite() && secs > 0.0 {
            secs
        } else {
            0.0
        };
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_micros
            .fetch_add((secs * 1e6) as u64, Ordering::Relaxed);
        for (i, edge) in BUCKETS_S.iter().enumerate() {
            if secs <= *edge {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
                return; // count it in its bucket only; render makes them cumulative
            }
        }
        // > last finite bucket falls into +Inf only (count already incremented).
    }

    /// Render as `name{label,le=...}` cumulative buckets + `_sum` + `_count`.
    fn render(&self, out: &mut String, name: &str, label: &str) {
        let mut cumulative = 0u64;
        for (i, edge) in BUCKETS_S.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            out.push_str(&format!(
                "{name}_bucket{{{label}le=\"{edge}\"}} {cumulative}\n"
            ));
        }
        let total = self.count.load(Ordering::Relaxed);
        out.push_str(&format!("{name}_bucket{{{label}le=\"+Inf\"}} {total}\n"));
        let sum_s = self.sum_micros.load(Ordering::Relaxed) as f64 / 1e6;
        out.push_str(&format!(
            "{name}_sum{{{label_trim}}} {sum_s}\n",
            label_trim = label.trim_end_matches(',')
        ));
        out.push_str(&format!(
            "{name}_count{{{label_trim}}} {total}\n",
            label_trim = label.trim_end_matches(',')
        ));
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────────────

struct Metrics {
    stage_lat: [Histogram; 7],
    rpc_lat: [Histogram; 11],
    pools_updated: Counter,
    candidates_generated: Counter,
    candidates_rejected: [Counter; 7],
    dry_run_success: Counter,
    dry_run_failure: Counter,
    opportunities: [Counter; 3],
    tx_executed: Counter,
    tx_failed: Counter,
    predicted_net_mist: Counter, // monotonic — we only predict on profitable candidates
    realized_net_mist: Gauge,    // cumulative; may decrease (a lost race costs gas)
    cache_size: Gauge,
    tracked_pools: Gauge,
    indexed_obligations: Gauge,
    latest_checkpoint: Gauge,
    rpc_up: Gauge,
    discovered_total: Gauge,
    discovered_by_dex: [Gauge; 4],
}
impl Metrics {
    fn new() -> Self {
        Self {
            stage_lat: array::from_fn(|_| Histogram::default()),
            rpc_lat: array::from_fn(|_| Histogram::default()),
            pools_updated: Counter::default(),
            candidates_generated: Counter::default(),
            candidates_rejected: array::from_fn(|_| Counter::default()),
            dry_run_success: Counter::default(),
            dry_run_failure: Counter::default(),
            opportunities: array::from_fn(|_| Counter::default()),
            tx_executed: Counter::default(),
            tx_failed: Counter::default(),
            predicted_net_mist: Counter::default(),
            realized_net_mist: Gauge::default(),
            cache_size: Gauge::default(),
            tracked_pools: Gauge::default(),
            indexed_obligations: Gauge::default(),
            latest_checkpoint: Gauge::default(),
            rpc_up: Gauge::default(),
            discovered_total: Gauge::default(),
            discovered_by_dex: array::from_fn(|_| Gauge::default()),
        }
    }
}

fn m() -> &'static Metrics {
    static M: OnceLock<Metrics> = OnceLock::new();
    M.get_or_init(Metrics::new)
}

// ── Public facade (call these from the pipeline) ──────────────────────────────────────

pub fn record_stage(stage: Stage, secs: f64) {
    m().stage_lat[stage as usize].observe(secs);
}
pub fn record_rpc(rpc: Rpc, secs: f64) {
    m().rpc_lat[rpc as usize].observe(secs);
}
pub fn inc_pools_updated(n: u64) {
    m().pools_updated.inc(n);
}
pub fn inc_candidates(n: u64) {
    m().candidates_generated.inc(n);
}
pub fn inc_rejected(r: Reject) {
    m().candidates_rejected[r as usize].inc(1);
}
pub fn inc_dry_run(success: bool) {
    if success {
        m().dry_run_success.inc(1);
    } else {
        m().dry_run_failure.inc(1);
    }
}
pub fn inc_opportunity(kind: Opp) {
    m().opportunities[kind as usize].inc(1);
}
pub fn inc_tx(success: bool) {
    if success {
        m().tx_executed.inc(1);
    } else {
        m().tx_failed.inc(1);
    }
}
pub fn add_predicted_net(mist: i128) {
    m().predicted_net_mist.inc(mist.max(0) as u64);
}
pub fn add_realized_net(mist: i128) {
    m().realized_net_mist.add(mist as i64);
}
pub fn set_cache_size(n: usize) {
    m().cache_size.set(n as i64);
}
pub fn set_tracked_pools(n: usize) {
    m().tracked_pools.set(n as i64);
}
pub fn set_indexed_obligations(n: usize) {
    m().indexed_obligations.set(n as i64);
}
pub fn set_latest_checkpoint(seq: u64) {
    m().latest_checkpoint.set(seq as i64);
}
pub fn set_rpc_up(up: bool) {
    m().rpc_up.set(i64::from(up));
}
pub fn set_discovered_total(n: usize) {
    m().discovered_total.set(n as i64);
}
pub fn set_dex_pools(dex: IndexDex, n: usize) {
    m().discovered_by_dex[dex as usize].set(n as i64);
}

/// RAII timer: records the elapsed time into `stage`'s histogram when dropped. Use as
/// `let _t = metrics::stage_timer(Stage::PtbBuild);`.
pub struct StageTimer {
    stage: Stage,
    start: Instant,
}
impl Drop for StageTimer {
    fn drop(&mut self) {
        record_stage(self.stage, self.start.elapsed().as_secs_f64());
    }
}
#[must_use]
pub fn stage_timer(stage: Stage) -> StageTimer {
    StageTimer {
        stage,
        start: Instant::now(),
    }
}

/// Time an async RPC call, recording its latency under `rpc`. Returns the future's output
/// unchanged — drop-in wrapper: `metrics::time_rpc(Rpc::DryRun, client.read_api().dry_run(..)).await`.
pub async fn time_rpc<F, T>(rpc: Rpc, fut: F) -> T
where
    F: Future<Output = T>,
{
    let start = Instant::now();
    let out = fut.await;
    record_rpc(rpc, start.elapsed().as_secs_f64());
    out
}

// ── Exposition ────────────────────────────────────────────────────────────────────────

/// Render the full registry in Prometheus text exposition format (the `/metrics` body).
pub fn render() -> String {
    let m = m();
    let mut s = String::with_capacity(4096);

    // Latency histograms.
    s.push_str("# HELP searcher_stage_latency_seconds Per-stage pipeline latency.\n");
    s.push_str("# TYPE searcher_stage_latency_seconds histogram\n");
    for stage in Stage::ALL {
        m.stage_lat[stage as usize].render(
            &mut s,
            "searcher_stage_latency_seconds",
            &format!("stage=\"{}\",", stage.as_str()),
        );
    }
    s.push_str("# HELP searcher_rpc_latency_seconds Per-endpoint RPC latency.\n");
    s.push_str("# TYPE searcher_rpc_latency_seconds histogram\n");
    for rpc in Rpc::ALL {
        m.rpc_lat[rpc as usize].render(
            &mut s,
            "searcher_rpc_latency_seconds",
            &format!("method=\"{}\",", rpc.as_str()),
        );
    }

    // Counters.
    counter(
        &mut s,
        "searcher_pools_updated_total",
        "Pool snapshots upserted.",
        m.pools_updated.get(),
    );
    counter(
        &mut s,
        "searcher_candidates_generated_total",
        "Opportunities produced by all sources.",
        m.candidates_generated.get(),
    );
    s.push_str("# HELP searcher_candidates_rejected_total Candidates rejected, by reason.\n");
    s.push_str("# TYPE searcher_candidates_rejected_total counter\n");
    for r in Reject::ALL {
        s.push_str(&format!(
            "searcher_candidates_rejected_total{{reason=\"{}\"}} {}\n",
            r.as_str(),
            m.candidates_rejected[r as usize].get()
        ));
    }
    s.push_str("# HELP searcher_dry_run_total Dry-runs by result.\n# TYPE searcher_dry_run_total counter\n");
    s.push_str(&format!(
        "searcher_dry_run_total{{result=\"success\"}} {}\n",
        m.dry_run_success.get()
    ));
    s.push_str(&format!(
        "searcher_dry_run_total{{result=\"failure\"}} {}\n",
        m.dry_run_failure.get()
    ));
    s.push_str("# HELP searcher_opportunities_total Opportunities by source kind.\n# TYPE searcher_opportunities_total counter\n");
    for o in Opp::ALL {
        s.push_str(&format!(
            "searcher_opportunities_total{{kind=\"{}\"}} {}\n",
            o.as_str(),
            m.opportunities[o as usize].get()
        ));
    }
    counter(
        &mut s,
        "searcher_tx_executed_total",
        "Transactions submitted+landed.",
        m.tx_executed.get(),
    );
    counter(
        &mut s,
        "searcher_tx_failed_total",
        "Transactions that failed to land.",
        m.tx_failed.get(),
    );
    counter(
        &mut s,
        "searcher_predicted_net_mist_total",
        "Cumulative predicted net (MIST).",
        m.predicted_net_mist.get(),
    );

    // Gauges.
    gauge(
        &mut s,
        "searcher_realized_net_mist",
        "Cumulative realized net (MIST; may be negative).",
        m.realized_net_mist.get(),
    );
    gauge(
        &mut s,
        "searcher_cache_size",
        "Pools in the local cache.",
        m.cache_size.get(),
    );
    gauge(
        &mut s,
        "searcher_tracked_pools",
        "Configured tracked pools.",
        m.tracked_pools.get(),
    );
    gauge(
        &mut s,
        "searcher_indexed_obligations",
        "Obligations in the liquidation index.",
        m.indexed_obligations.get(),
    );
    gauge(
        &mut s,
        "searcher_latest_checkpoint",
        "Latest checkpoint sequence seen.",
        m.latest_checkpoint.get(),
    );
    gauge(
        &mut s,
        "searcher_rpc_up",
        "RPC reachability (1=up, 0=down).",
        m.rpc_up.get(),
    );
    gauge(
        &mut s,
        "searcher_discovered_pools",
        "Pools discovered by the indexer (candidate set across all DEXs).",
        m.discovered_total.get(),
    );
    s.push_str("# HELP searcher_discovered_pools_by_dex Active (quotable, kept) pools per DEX.\n");
    s.push_str("# TYPE searcher_discovered_pools_by_dex gauge\n");
    for d in IndexDex::ALL {
        s.push_str(&format!(
            "searcher_discovered_pools_by_dex{{dex=\"{}\"}} {}\n",
            d.as_str(),
            m.discovered_by_dex[d as usize].get()
        ));
    }

    // Derived: realized / predicted (capture leakage). Only when predicted > 0.
    let pred = m.predicted_net_mist.get();
    if pred > 0 {
        let ratio = m.realized_net_mist.get() as f64 / pred as f64;
        s.push_str(
            "# HELP searcher_realized_vs_predicted Realized / predicted net (<1 = leakage).\n",
        );
        s.push_str("# TYPE searcher_realized_vs_predicted gauge\n");
        s.push_str(&format!("searcher_realized_vs_predicted {ratio}\n"));
    }
    s
}

fn counter(s: &mut String, name: &str, help: &str, v: u64) {
    s.push_str(&format!(
        "# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}\n"
    ));
}
fn gauge(s: &mut String, name: &str, help: &str, v: i64) {
    s.push_str(&format!(
        "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {v}\n"
    ));
}

/// Serve `render()` at `GET /metrics` over a minimal HTTP/1.1 responder (no web framework).
/// Binds `addr` (e.g. `0.0.0.0:9100`) and serves until the process exits.
pub async fn serve(addr: &str) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "metrics: /metrics endpoint listening");
    loop {
        let (mut sock, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await; // drain the request line/headers (ignored)
            let body = render();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_gauges_and_render() {
        inc_pools_updated(3);
        inc_candidates(2);
        inc_rejected(Reject::BelowMinProfit);
        inc_dry_run(true);
        inc_dry_run(false);
        inc_opportunity(Opp::Liquidation);
        inc_tx(true);
        add_predicted_net(1_000);
        add_realized_net(-250);
        set_cache_size(10);
        set_tracked_pools(12);
        set_indexed_obligations(5);
        set_latest_checkpoint(42);
        set_rpc_up(true);

        let out = render();
        // Type declarations present.
        assert!(out.contains("# TYPE searcher_pools_updated_total counter"));
        assert!(out.contains("# TYPE searcher_stage_latency_seconds histogram"));
        assert!(out.contains("searcher_candidates_rejected_total{reason=\"below_min_profit\"} 1"));
        assert!(out.contains("searcher_dry_run_total{result=\"success\"} 1"));
        assert!(out.contains("searcher_dry_run_total{result=\"failure\"} 1"));
        assert!(out.contains("searcher_opportunities_total{kind=\"liquidation\"} 1"));
        assert!(out.contains("searcher_latest_checkpoint 42"));
        assert!(out.contains("searcher_rpc_up 1"));
        // Derived ratio: realized(-250)/predicted(1000) = -0.25.
        assert!(out.contains("searcher_realized_vs_predicted -0.25"));
    }

    #[test]
    fn histogram_buckets_are_cumulative() {
        let h = Histogram::default();
        h.observe(0.002); // <= 0.0025
        h.observe(0.2); // <= 0.25
        h.observe(10.0); // +Inf only
        let mut s = String::new();
        h.render(&mut s, "t_lat", "stage=\"x\",");
        // le=0.0025 should already include the first observation.
        assert!(s.contains("t_lat_bucket{stage=\"x\",le=\"0.0025\"} 1"));
        // le=0.25 cumulative includes the 0.002 and 0.2 ⇒ 2.
        assert!(s.contains("t_lat_bucket{stage=\"x\",le=\"0.25\"} 2"));
        // +Inf includes all three.
        assert!(s.contains("t_lat_bucket{stage=\"x\",le=\"+Inf\"} 3"));
        assert!(s.contains("t_lat_count{stage=\"x\"} 3"));
    }

    #[tokio::test]
    async fn time_rpc_records_and_returns() {
        let v = time_rpc(Rpc::Other, async { 7u32 }).await;
        assert_eq!(v, 7);
    }
}
