//! Minimal Prometheus-compatible metrics for agentgate (spec_tooling.md §15).
//!
//! Design: no external crate — counters/gauges are `AtomicU64`, the two
//! histograms are `sum + count` accumulators. Exposed at `GET /_metrics`
//! as Prometheus text format.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct Metrics {
    /// Runs currently executing (gated by classification).
    pub runs_active: AtomicU64,
    /// Runs currently Parked waiting for the client resume request.
    pub runs_parked: AtomicU64,
    /// Bridge keepalive ticks emitted.
    pub bridge_keepalive_ticks: AtomicU64,
    /// Resume attempts that succeeded.
    pub resume_ok: AtomicU64,
    /// Resume attempts served via replay fallback.
    pub resume_replay: AtomicU64,
    /// Resume attempts rejected with a conflict.
    pub resume_conflict: AtomicU64,
    /// Non-bridge tool observed (guard trip).
    pub guard_violations: AtomicU64,
    /// Successful CLI exits observed.
    pub cli_exit_ok: AtomicU64,
    /// Failed CLI exits observed.
    pub cli_exit_error: AtomicU64,
    /// Total time spent waiting for client tool results (histogram, sum).
    pub tool_wait_ms_sum: AtomicU64,
    /// Number of held tool results observed (histogram, count).
    pub tool_wait_ms_count: AtomicU64,
    /// Total time spent acquiring provider semaphore permits (histogram, sum).
    pub semaphore_wait_ms_sum: AtomicU64,
    /// Number of semaphore acquisition observations.
    pub semaphore_wait_ms_count: AtomicU64,
    /// Sum of time-to-first-token / first byte, ms.
    pub ttft_ms_sum: AtomicU64,
    /// Count of ttft observations.
    pub ttft_ms_count: AtomicU64,
    /// Active permit holders per provider (gauge map: provider -> count).
    pub provider_concurrency: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Total HTTP requests by method, path, status.
    pub http_requests_total: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Total provider calls by provider, model, status.
    pub provider_calls_total: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Total bridge sessions by provider, status.
    pub bridge_sessions_total: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Total bridge continuations by provider, status.
    pub bridge_continuations_total: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Model cache hits by provider.
    pub model_cache_hits_total: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Model cache misses by provider.
    pub model_cache_misses_total: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Tools injected by provider, model_type (free/paid).
    pub tool_injected_total: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Errors by component, error_type.
    pub errors_total: std::sync::Mutex<std::collections::HashMap<String, u64>>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            runs_active: AtomicU64::new(0),
            runs_parked: AtomicU64::new(0),
            bridge_keepalive_ticks: AtomicU64::new(0),
            resume_ok: AtomicU64::new(0),
            resume_replay: AtomicU64::new(0),
            resume_conflict: AtomicU64::new(0),
            guard_violations: AtomicU64::new(0),
            cli_exit_ok: AtomicU64::new(0),
            cli_exit_error: AtomicU64::new(0),
            tool_wait_ms_sum: AtomicU64::new(0),
            tool_wait_ms_count: AtomicU64::new(0),
            semaphore_wait_ms_sum: AtomicU64::new(0),
            semaphore_wait_ms_count: AtomicU64::new(0),
            ttft_ms_sum: AtomicU64::new(0),
            ttft_ms_count: AtomicU64::new(0),
            provider_concurrency: std::sync::Mutex::new(std::collections::HashMap::new()),
            http_requests_total: std::sync::Mutex::new(std::collections::HashMap::new()),
            provider_calls_total: std::sync::Mutex::new(std::collections::HashMap::new()),
            bridge_sessions_total: std::sync::Mutex::new(std::collections::HashMap::new()),
            bridge_continuations_total: std::sync::Mutex::new(std::collections::HashMap::new()),
            model_cache_hits_total: std::sync::Mutex::new(std::collections::HashMap::new()),
            model_cache_misses_total: std::sync::Mutex::new(std::collections::HashMap::new()),
            tool_injected_total: std::sync::Mutex::new(std::collections::HashMap::new()),
            errors_total: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn dec(gauge: &AtomicU64) {
        // Saturate at zero instead of wrapping.
        loop {
            let cur = gauge.load(Ordering::Relaxed);
            if cur == 0 {
                return;
            }
            if gauge.compare_exchange_weak(cur, cur - 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                return;
            }
        }
    }

    pub fn mark_run_started(&self) {
        self.runs_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn mark_run_ended(&self) {
        Self::dec(&self.runs_active);
    }

    pub fn mark_run_parked(&self) {
        self.runs_parked.fetch_add(1, Ordering::Relaxed);
    }

    pub fn mark_run_unparked(&self) {
        Self::dec(&self.runs_parked);
    }

    pub fn record_resume(&self, result: &str) {
        match result {
            "ok" => self.resume_ok.fetch_add(1, Ordering::Relaxed),
            "replay" => self.resume_replay.fetch_add(1, Ordering::Relaxed),
            "conflict" => self.resume_conflict.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
    }

    pub fn record_guard_violation(&self) {
        self.guard_violations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cli_exit(&self, success: bool) {
        if success {
            self.cli_exit_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            self.cli_exit_error.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn observe_tool_wait(&self, ms: u64) {
        self.tool_wait_ms_sum.fetch_add(ms, Ordering::Relaxed);
        self.tool_wait_ms_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_semaphore_wait(&self, ms: u64) {
        self.semaphore_wait_ms_sum.fetch_add(ms, Ordering::Relaxed);
        self.semaphore_wait_ms_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_ttft(&self, ms: u64) {
        self.ttft_ms_sum.fetch_add(ms, Ordering::Relaxed);
        self.ttft_ms_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_provider_concurrency(&self, provider: &str, delta: i64) {
        let key = format!("provider=\"{}\"", provider);
        let mut map = self.provider_concurrency.lock().unwrap();
        let v = map.entry(key).or_insert(0);
        *v = (*v as i64 + delta).max(0) as u64;
    }

    pub fn record_bridge_keepalive(&self) {
        self.bridge_keepalive_ticks.fetch_add(1, Ordering::Relaxed);
    }

    // New granular metrics methods
    fn increment_map(map: &std::sync::Mutex<std::collections::HashMap<String, u64>>, key: &str) {
        map.lock().unwrap().entry(key.to_string()).and_modify(|v| *v += 1).or_insert(1);
    }

    pub fn record_http_request(&self, method: &str, path: &str, status: u16) {
        let key = format!("method=\"{}\",path=\"{}\",status=\"{}\"", method, path, status);
        Self::increment_map(&self.http_requests_total, &key);
    }

    pub fn record_provider_call(&self, provider: &str, model: &str, success: bool) {
        let status = if success { "ok" } else { "error" };
        let key = format!("provider=\"{}\",model=\"{}\",status=\"{}\"", provider, model, status);
        Self::increment_map(&self.provider_calls_total, &key);
    }

    pub fn record_bridge_session(&self, provider: &str, success: bool) {
        let status = if success { "ok" } else { "error" };
        let key = format!("provider=\"{}\",status=\"{}\"", provider, status);
        Self::increment_map(&self.bridge_sessions_total, &key);
    }

    pub fn record_bridge_continuation(&self, provider: &str, success: bool) {
        let status = if success { "ok" } else { "error" };
        let key = format!("provider=\"{}\",status=\"{}\"", provider, status);
        Self::increment_map(&self.bridge_continuations_total, &key);
    }

    pub fn record_model_cache_hit(&self, provider: &str) {
        let key = format!("provider=\"{}\"", provider);
        Self::increment_map(&self.model_cache_hits_total, &key);
    }

    pub fn record_model_cache_miss(&self, provider: &str) {
        let key = format!("provider=\"{}\"", provider);
        Self::increment_map(&self.model_cache_misses_total, &key);
    }

    pub fn record_tool_injected(&self, provider: &str, is_free_model: bool) {
        let model_type = if is_free_model { "free" } else { "paid" };
        let key = format!("provider=\"{}\",model_type=\"{}\"", provider, model_type);
        Self::increment_map(&self.tool_injected_total, &key);
    }

    pub fn record_error(&self, component: &str, error_type: &str) {
        let key = format!("component=\"{}\",error_type=\"{}\"", component, error_type);
        Self::increment_map(&self.errors_total, &key);
    }

    fn ms(v: u64) -> f64 {
        v as f64 / 1000.0
    }

    pub fn render_prometheus(&self) -> String {
        fn line(out: &mut String, metric: &str, value: f64) {
            out.push_str(metric);
            out.push(' ');
            out.push_str(&value.to_string());
            out.push('\n');
        }
        let mut out = String::with_capacity(1024);
        line(&mut out, "agentgate_runs_active", self.runs_active.load(Ordering::Relaxed) as f64);
        line(&mut out, "agentgate_runs_parked", self.runs_parked.load(Ordering::Relaxed) as f64);
        line(&mut out, "agentgate_bridge_keepalive_ticks_total", self.bridge_keepalive_ticks.load(Ordering::Relaxed) as f64);
        
        // resume_total with labels per spec
        let resume_ok = self.resume_ok.load(Ordering::Relaxed) as f64;
        let resume_replay = self.resume_replay.load(Ordering::Relaxed) as f64;
        let resume_conflict = self.resume_conflict.load(Ordering::Relaxed) as f64;
        out.push_str("agentgate_resume_total{result=\"ok\"} ");
        out.push_str(&resume_ok.to_string());
        out.push('\n');
        out.push_str("agentgate_resume_total{result=\"replay\"} ");
        out.push_str(&resume_replay.to_string());
        out.push('\n');
        out.push_str("agentgate_resume_total{result=\"conflict\"} ");
        out.push_str(&resume_conflict.to_string());
        out.push('\n');
        
        line(&mut out, "agentgate_guard_violations_total", self.guard_violations.load(Ordering::Relaxed) as f64);
        
        // cli_exit_total with labels per spec
        let cli_exit_ok = self.cli_exit_ok.load(Ordering::Relaxed) as f64;
        let cli_exit_error = self.cli_exit_error.load(Ordering::Relaxed) as f64;
        out.push_str("agentgate_cli_exit_total{code=\"0\"} ");
        out.push_str(&cli_exit_ok.to_string());
        out.push('\n');
        out.push_str("agentgate_cli_exit_total{code=\"1\"} ");
        out.push_str(&cli_exit_error.to_string());
        out.push('\n');
        
        line(&mut out,
            "agentgate_tool_wait_seconds_sum",
            Self::ms(self.tool_wait_ms_sum.load(Ordering::Relaxed)),
        );
        line(&mut out,
            "agentgate_tool_wait_seconds_count",
            self.tool_wait_ms_count.load(Ordering::Relaxed) as f64,
        );
        line(&mut out,
            "agentgate_ttft_seconds_sum",
            Self::ms(self.ttft_ms_sum.load(Ordering::Relaxed)),
        );
        line(&mut out,
            "agentgate_ttft_seconds_count",
            self.ttft_ms_count.load(Ordering::Relaxed) as f64,
        );
        line(&mut out,
            "agentgate_semaphore_wait_seconds_sum",
            Self::ms(self.semaphore_wait_ms_sum.load(Ordering::Relaxed)),
        );
        line(&mut out,
            "agentgate_semaphore_wait_seconds_count",
            self.semaphore_wait_ms_count.load(Ordering::Relaxed) as f64,
        );
        out
    }

    fn render_map_metrics(&self, name: &str, map: &std::sync::Mutex<std::collections::HashMap<String, u64>>) -> String {
        let mut out = String::new();
        for (key, value) in map.lock().unwrap().iter() {
            out.push_str(name);
            out.push('{');
            out.push_str(key);
            out.push_str("} ");
            out.push_str(&value.to_string());
            out.push('\n');
        }
        out
    }

    pub fn render_prometheus_full(&self) -> String {
        let mut out = self.render_prometheus();
        out.push_str(&self.render_map_metrics("agentgate_http_requests_total", &self.http_requests_total));
        out.push_str(&self.render_map_metrics("agentgate_provider_calls_total", &self.provider_calls_total));
        out.push_str(&self.render_map_metrics("agentgate_bridge_sessions_total", &self.bridge_sessions_total));
        out.push_str(&self.render_map_metrics("agentgate_bridge_continuations_total", &self.bridge_continuations_total));
        out.push_str(&self.render_map_metrics("agentgate_model_cache_hits_total", &self.model_cache_hits_total));
        out.push_str(&self.render_map_metrics("agentgate_model_cache_misses_total", &self.model_cache_misses_total));
        out.push_str(&self.render_map_metrics("agentgate_tool_injected_total", &self.tool_injected_total));
        out.push_str(&self.render_map_metrics("agentgate_provider_concurrency", &self.provider_concurrency));
        out.push_str(&self.render_map_metrics("agentgate_errors_total", &self.errors_total));
        out
    }
}

pub static METRICS: std::sync::OnceLock<Metrics> = std::sync::OnceLock::new();

pub fn init_metrics() -> &'static Metrics {
    METRICS.get_or_init(Metrics::new)
}







