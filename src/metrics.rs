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
    /// Sum of time-to-first-token / first byte, ms.
    pub ttft_ms_sum: AtomicU64,
    /// Count of ttft observations.
    pub ttft_ms_count: AtomicU64,
}

impl Metrics {
    pub const fn new() -> Self {
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
            ttft_ms_sum: AtomicU64::new(0),
            ttft_ms_count: AtomicU64::new(0),
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

    pub fn observe_ttft(&self, ms: u64) {
        self.ttft_ms_sum.fetch_add(ms, Ordering::Relaxed);
        self.ttft_ms_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_bridge_keepalive(&self) {
        self.bridge_keepalive_ticks.fetch_add(1, Ordering::Relaxed);
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
        out
    }
}

pub static METRICS: Metrics = Metrics::new();


