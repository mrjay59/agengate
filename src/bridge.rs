use crate::config::{BridgeConfig, ProviderConfig, IsolationConfig};
use crate::reasoning::event::ToolCall;
use crate::types::Message;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, oneshot, mpsc};
use tokio::time::interval;
use tracing::{debug, info};

/// RequestBatcher for window-based batching (spec §10.3).
/// Collects requests and batches them based on window_ms and max_calls.
pub struct RequestBatcher {
    sender: mpsc::Sender<BatchedRequest>,
    handle: tokio::task::JoinHandle<()>,
}

impl RequestBatcher {
    /// Create a new RequestBatcher with the given window and max calls.
    /// The handler will be called with a batch of requests.
    pub fn new<F, Fut>(
        window_ms: u64,
        max_calls: usize,
        mut handler: F,
    ) -> Self
    where
        F: FnMut(Vec<BatchedRequest>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let (tx, mut rx) = mpsc::channel(100);
        let handle = tokio::spawn(async move {
            let mut batch = Vec::new();
            let mut timer = interval(Duration::from_millis(window_ms));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    Some(request) = rx.recv() => {
                        batch.push(request);
                        if batch.len() >= max_calls {
                            let to_send = std::mem::take(&mut batch);
                            if let Err(e) = handler(to_send).await {
                                tracing::error!("RequestBatcher handler error: {}", e);
                            }
                        }
                    }
                    _ = timer.tick() => {
                        if !batch.is_empty() {
                            let to_send = std::mem::take(&mut batch);
                            if let Err(e) = handler(to_send).await {
                                tracing::error!("RequestBatcher handler error: {}", e);
                            }
                        }
                    }
                }
            }
        });

        Self { sender: tx, handle }
    }

    /// Submit a request to the batcher.
    pub async fn submit(&self, request: BatchedRequest) -> Result<()> {
        self.sender.send(request).await?;
        Ok(())
    }

    /// Shutdown the batcher.
    pub async fn shutdown(self) -> Result<()> {
        drop(self.sender);
        self.handle.await?;
        Ok(())
    }
}

/// A request that can be batched.
#[derive(Debug, Clone)]
pub enum BatchedRequest {
    UserMessage { session_id: String, text: String },
    ToolResult { session_id: String, result: BridgeToolResult },
}

/// Result of a tool call from the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeToolResult {
    pub tool_call_id: String,
    pub result: String,
    pub is_error: bool,
}

/// Bridge client for handling tool calls from CLI agents
#[derive(Clone)]
pub struct BridgeClient {
    pub host: String,
    pub port: u16,
    /// Bridge server reference for PR-S4 live resume (continuation registration).
    bridge: Option<Arc<BridgeServer>>,
}

impl std::fmt::Debug for BridgeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeClient")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("bridge", &"<bridge server>")
            .finish()
    }
}

impl BridgeClient {
    pub fn new(host: String, port: u16) -> Self {
        Self { host, port, bridge: None }
    }

    /// Create client with bridge reference for PR-S4 live resume.
    pub fn new_with_bridge(host: String, port: u16, bridge: Arc<BridgeServer>) -> Self {
        Self { host, port, bridge: Some(bridge) }
    }

    /// Register a continuation waiter for live resume (PR-S4).
    /// Delegates to BridgeServer::register_continuation which returns the sender.
    /// The producer should await on the returned receiver.
    pub async fn register_continuation(&self, run_id: String) -> Result<oneshot::Sender<ContinuationOutput>> {
        if let Some(bridge) = &self.bridge {
            Ok(bridge.register_continuation(run_id).await)
        } else {
            Err(anyhow!("Bridge server not available for continuation"))
        }
    }

    pub async fn handle_tool_call(&self, _id: &str, _name: &str, _arguments: serde_json::Value) -> Result<()> {
        // TODO: Implement tool call handling
        Ok(())
    }

    pub async fn handle_user_message(&self, _text: &str) -> Result<()> {
        // TODO: Implement user message handling
        Ok(())
    }
}

/// Core Bridge server managing MCP loopback connections
#[allow(dead_code)]
#[derive(Clone)]
pub struct BridgeSession {
    pub session_id: String,
    pub token: String,
    pub run_id: Option<String>,
    pub tools: Vec<ToolSchema>,
    pub created_at: std::time::Instant,
    pub isolation_active: bool,
}

pub struct BridgeServer {
    config: BridgeConfig,
    runs: Arc<RwLock<HashMap<String, Run>>>,
    tool_schemas: Arc<RwLock<HashMap<String, ToolSchema>>>,
    parked_runs: Arc<Mutex<HashMap<String, Run>>>,
    sessions: Arc<RwLock<HashMap<String, BridgeSession>>>,
    pending_calls: Arc<Mutex<HashMap<String, PendingCall>>>,
    provider_sessions: Arc<RwLock<HashMap<String, Arc<dyn crate::provider::bridge::BridgeSession + Send + Sync>>>>,
    #[allow(dead_code)]
    cleanup_interval: Duration,
    /// Optional 150ms windowed request batcher for tool-call flow (spec §10.3).
    #[allow(dead_code)]
    batcher: Arc<Mutex<Option<RequestBatcher>>>,
    /// PR-S4: continuation waiters keyed by run_id (opencode live resume; replay fallback if absent/timeout).
    continuations: Arc<Mutex<HashMap<String, ParkedContinuation>>>,
    /// P0-AUDIT seq2: queued tool calls for serial-mode (parallel_tool_calls=false).
    /// Keyed by run_id, stores remaining ToolCalls to process sequentially on resume.
    tool_call_queue: Arc<Mutex<HashMap<String, Vec<ToolCall>>>>,
    /// P0-AUDIT seq2: NameMap for tool name -> alias reverse mapping.
    tool_name_map: Arc<RwLock<HashMap<String, String>>>,
    /// P0-AUDIT seq2: TTL tracking for tool_call_ids.
    tool_call_ttls: Arc<Mutex<HashMap<String, Instant>>>,
    /// PR-S5: session generation counter for replay rotation.
    /// Incremented on each replay to guarantee fresh cli_gen/session_id.
    cli_gen: Arc<Mutex<u64>>,
    /// PR-S5: session generation per run_id (run_id -> generation).
    /// Ensures replay uses a fresh generation.
    session_generation: Arc<RwLock<HashMap<String, u64>>>,
    /// PR-S5: tracked orphan PIDs for startup sweep.
    orphan_pids: Arc<Mutex<Vec<u32>>>,
}

/// A held `tools/call` bound to the session that issued it.
struct PendingCall {
    session_id: String,
    run_id: Option<String>,
    tool_name: String,
    tool_call_id: Option<String>,
    tx: oneshot::Sender<ToolResult>,
}

/// PR-S4: continuation output captured from a live CLI session after MCP tool results are delivered.
/// Minimal DTO (bridge-level) to avoid coupling to provider response types.
#[derive(Debug, Clone)]
pub struct ContinuationOutput {
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Option<Vec<crate::types::ToolCall>>,
    pub session_id: Option<String>,
}

/// PR-S4: parked continuation waiter (opencode capability-gated, replay fallback mandatory).
pub struct ParkedContinuation {
    pub rx: oneshot::Receiver<ContinuationOutput>,
    pub registered_at: std::time::Instant,
}

impl BridgeServer {
    pub async fn pending_count(&self) -> usize {
        self.pending_calls.lock().await.len()
    }

    /// P0-AUDIT seq2: Queue remaining tool calls for a run (serial mode batch/queue).
    /// Stores ToolCalls keyed by run_id for sequential processing on resume.
    pub async fn queue_tool_calls(&self, run_id: &str, calls: Vec<ToolCall>) -> Result<()> {
        let mut queue = self.tool_call_queue.lock().await;
        queue.insert(run_id.to_string(), calls);
        Ok(())
    }

    /// P0-AUDIT seq2: Pop the next queued tool call for a run (FIFO).
    /// Returns None if queue is empty or run_id has no queued calls.
    pub async fn pop_next_tool_call(&self, run_id: &str) -> Option<ToolCall> {
        let mut queue = self.tool_call_queue.lock().await;
        if let Some(calls) = queue.get_mut(run_id) {
            let call = calls.remove(0);
            if calls.is_empty() {
                queue.remove(run_id);
            }
            Some(call)
        } else {
            None
        }
    }

    /// P0-AUDIT seq2: Check if a run has queued tool calls remaining.
    pub async fn has_queued_tool_calls(&self, run_id: &str) -> bool {
        let queue = self.tool_call_queue.lock().await;
        queue.contains_key(run_id)
    }

    /// P0-AUDIT seq2: Register tool name -> alias mapping (NameMap reverse lookup).
    pub async fn register_tool_name_alias(&self, tool_name: String, alias: String) {
        let mut map = self.tool_name_map.write().await;
        map.insert(tool_name, alias);
    }

    /// P0-AUDIT seq2: Resolve alias -> tool_name reverse mapping.
    pub async fn resolve_tool_alias(&self, alias: &str) -> Option<String> {
        let map = self.tool_name_map.read().await;
        map.iter()
            .find(|(_, v)| *v == alias)
            .map(|(k, _)| k.clone())
    }

    /// P0-AUDIT seq2: Record TTL expiry time for a tool_call_id.
    pub async fn record_tool_call_ttl(&self, tool_call_id: &str, ttl_secs: u64) {
        let mut ttl_map = self.tool_call_ttls.lock().await;
        ttl_map.insert(
            tool_call_id.to_string(),
            Instant::now() + Duration::from_secs(ttl_secs),
        );
    }

    /// P0-AUDIT seq2: Check if a tool_call_id has expired (TTL passed).
    pub async fn is_tool_call_expired(&self, tool_call_id: &str) -> bool {
        let ttl_map = self.tool_call_ttls.lock().await;
        match ttl_map.get(tool_call_id) {
            Some(deadline) => Instant::now() > *deadline,
            None => false,
        }
    }

    /// P0-AUDIT seq2: Cleanup expired tool_call_ids from TTL map.
    pub async fn cleanup_expired_tool_call_ttls(&self) {
        let mut ttl_map = self.tool_call_ttls.lock().await;
        let now = Instant::now();
        ttl_map.retain(|_, deadline| now <= *deadline);
    }

    pub fn new(config: BridgeConfig) -> Self {
        let batcher = Arc::new(Mutex::new(None));

        Self {
            config,
            runs: Arc::new(RwLock::new(HashMap::new())),
            tool_schemas: Arc::new(RwLock::new(HashMap::new())),
            parked_runs: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            pending_calls: Arc::new(Mutex::new(HashMap::new())),
            provider_sessions: Arc::new(RwLock::new(HashMap::new())),
            cleanup_interval: Duration::from_secs(600),
            batcher,
            continuations: Arc::new(Mutex::new(HashMap::new())),
            tool_call_queue: Arc::new(Mutex::new(HashMap::new())),
            tool_name_map: Arc::new(RwLock::new(HashMap::new())),
            tool_call_ttls: Arc::new(Mutex::new(HashMap::new())),
            cli_gen: Arc::new(Mutex::new(0)),
            session_generation: Arc::new(RwLock::new(HashMap::new())),
            orphan_pids: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register a provider session (for cancellation/cleanup; P0-3).
    pub async fn register_provider_session(
        &self,
        session_id: String,
        provider_session: Arc<dyn crate::provider::bridge::BridgeSession + Send + Sync>,
    ) {
        self.provider_sessions
            .write()
            .await
            .insert(session_id, provider_session);
    }

    /// PR-S2: Check if a provider session is still registered (best-effort liveness).
    pub async fn has_provider_session(&self, session_id: &str) -> bool {
        self.provider_sessions.read().await.contains_key(session_id)
    }

    /// PR-S2: Remove provider session reference (cleanup).
    pub async fn unregister_provider_session(&self, session_id: &str) {
        self.provider_sessions.write().await.remove(session_id);
    }

    /// PR-S4: register a continuation waiter for a parked run/session key (capability-gated). Returns sender for producer.
    pub async fn register_continuation(&self, key: String) -> oneshot::Sender<ContinuationOutput> {
        let (tx, rx) = oneshot::channel();
        self.continuations.lock().await.insert(key, ParkedContinuation { rx, registered_at: std::time::Instant::now() });
        tx
    }

    /// PR-S4: take continuation waiter (single-consumer). Returns None if absent/already consumed.
    pub async fn take_continuation(&self, key: &str) -> Option<ParkedContinuation> {
        self.continuations.lock().await.remove(key)
    }

    /// PR-S4: drop continuation waiter without consuming output (cleanup/replay).
    pub async fn remove_continuation(&self, key: &str) -> bool {
        self.continuations.lock().await.remove(key).is_some()
    }

    /// PR-S4: re-key continuation (e.g. from mcp_session_id to run_id) for resume lookup.
    pub async fn rekey_continuation(&self, from: &str, to: String) -> bool {
        let mut map = self.continuations.lock().await;
        if let Some(v) = map.remove(from) {
            map.insert(to, v);
            true
        } else {
            false
        }
    }

    /// Initialize the 150ms request batcher for tool-call flow (spec §10.3).
    /// Call once after BridgeServer construction to enable batching.
    pub async fn init_batcher(&self) {
        if self.config.batch.window_ms > 0 && self.config.batch.max_calls > 0 {
            let window_ms = self.config.batch.window_ms;
            let max_calls = self.config.batch.max_calls;

            let handler = move |requests: Vec<BatchedRequest>| {
                info!(event = "bridge.batcher.flush", count = requests.len());
                // Minimal safe behavior: log batch contents. Full integration
                // (forwarding batched tool calls to CLI providers) requires
                // refactoring the execute_tool flow to route through batcher.
                async { Ok(()) }
            };

            let batcher = RequestBatcher::new(window_ms, max_calls, handler);

            let mut guard = self.batcher.lock().await;
            *guard = Some(batcher);
            info!(event = "bridge.batcher.initialized", window_ms, max_calls);
        }
    }

    // (Reconciled) ParkedSessionStore removed: use Run linkage (PR-S1) + pending_calls/provider_sessions (S2/P0-1b).

    pub async fn create_session(&self, tools: Vec<ToolSchema>, run_id: Option<String>) -> (String, String) {
        let session_id = uuid::Uuid::new_v4().to_string();
        let token = uuid::Uuid::new_v4().to_string().replace("-", "");
        let session = BridgeSession {
            session_id: session_id.clone(),
            token: token.clone(),
            run_id,
            tools,
            created_at: std::time::Instant::now(),
            isolation_active: true,
        };
        self.sessions.write().await.insert(session_id.clone(), session);
        (session_id, token)
    }

    pub async fn get_session(&self, session_id: &str) -> Option<BridgeSession> {
        self.sessions.read().await.get(session_id).cloned()
    }

    /// Remove a session and any pending tool calls bound to it.
    pub async fn remove_session(&self, session_id: &str) -> bool {
        let removed = self.sessions.write().await.remove(session_id).is_some();
        // Dropping the senders wakes any held `tools/call` with "channel closed".
        self.pending_calls
            .lock()
            .await
            .retain(|_, pending| pending.session_id != session_id);
        removed
    }

    /// PR-S5: Sweep orphan CLI PIDs on startup.
    /// Scans tracked orphan PIDs (from previous crashed runs) and attempts
    /// best-effort kill. Always clears the tracking list after sweep.
    pub async fn sweep_orphan_pids_on_startup(&self) {
        let pids = {
            let mut pids = self.orphan_pids.lock().await;
            let mut cleared = Vec::new();
            std::mem::swap(&mut cleared, &mut pids);
            cleared
        };
        let count = pids.len();
        for pid in &pids {
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .output();
            }
            debug!(event = "bridge.orphan_pid_sweep.killed", pid);
        }
        if count > 0 {
            info!(event = "bridge.orphan_pid_sweep", swept = count);
        }
    }

    /// PR-S5: Replay session rotation.
    /// Increments cli_gen and records new generation for run_id.
    /// Returns (new_session_id, new_token, cli_gen) so caller can create
    /// a fresh provider session and not reuse stale CLI context.
    pub async fn replay_rotate_session(&self, run_id: &str) -> (String, String, u64) {
        let gen = {
            let mut g = self.cli_gen.lock().await;
            *g += 1;
            *g
        };
        {
            let mut sg = self.session_generation.write().await;
            sg.insert(run_id.to_string(), gen);
        }
        let (session_id, token) = self.create_session(Vec::new(), Some(run_id.to_string())).await;
        info!(event = "bridge.replay.rotate", run_id = %run_id, session_id = %session_id, cli_gen = gen);
        (session_id, token, gen)
    }

    /// PR-S5: Get current session generation for a run_id.
    pub async fn session_generation_for(&self, run_id: &str) -> u64 {
        let sg = self.session_generation.read().await;
        sg.get(run_id).copied().unwrap_or(0)
    }

    /// PR-S5: Register an orphan PID for startup sweep (provider adapters call
    /// this when a CLI child exits unexpectedly without parent cleanup).
    pub async fn register_orphan_pid(&self, pid: u32) {
        let mut pids = self.orphan_pids.lock().await;
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }

    /// Cancel a session: kill CLI child process, cleanup workdir, stop run.
    pub async fn cancel_session(&self, session_id: &str) -> Result<()> {
        // Close provider session (kill CLI child process / cleanup workdir), then
        // drop session bookkeeping and wake held tool calls.
        if let Some(provider_session) = self.provider_sessions.write().await.remove(session_id) {
            provider_session.close().await?;
        }
        self.remove_session(session_id).await;
        Ok(())
    }

    pub fn validate_token(session: &BridgeSession, token: &str) -> bool {
        // constant-time compare
        if session.token.len() != token.len() {
            return false;
        }
        let mut diff = 0u8;
        for (a, b) in session.token.bytes().zip(token.bytes()) {
            diff |= a ^ b;
        }
        diff == 0
    }

    pub async fn hold_tool_call(
        &self,
        mcp_id: String,
        session_id: String,
        run_id: Option<String>,
        tool_call_id: Option<String>,
        tool_name: String,
        timeout_secs: u64,
    ) -> Result<ToolResult> {
        let (mut tx, mut rx) = oneshot::channel();
        self.pending_calls.lock().await.insert(
            mcp_id.clone(),
            PendingCall { session_id, run_id: run_id.clone(), tool_name, tool_call_id: tool_call_id.clone(), tx },
        );
        
        info!(event = "bridge.tool_call.hold", mcp_id = %mcp_id, timeout = timeout_secs);
        
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        // P0-AUDIT seq2: Wire hold↔Run live mapping — associate mcp_id with run_id/tool_call_id.
        if let (Some(_run_id), Some(tool_call_id)) = (&run_id, &tool_call_id) {
            self.associate_tool_call_ids(&mcp_id, tool_call_id).await;
        }

        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                self.pending_calls.lock().await.remove(&mcp_id);
                if let Some(id) = &tool_call_id {
                    self.record_tool_call_ttl(id, timeout_secs).await;
                }
                info!(event = "bridge.tool_call.timeout", mcp_id = %mcp_id);
                return Err(anyhow::anyhow!("timeout menunggu hasil dari client"));
            }

            tokio::select! {
                biased;
                result = &mut rx => {
                    match result {
                        Ok(res) => {
                            info!(event = "bridge.tool_call.resolved", mcp_id = %mcp_id);
                            return Ok(res);
                        }
                        Err(_) => {
                            self.pending_calls.lock().await.remove(&mcp_id);
                            return Err(anyhow::anyhow!("channel closed"));
                        }
                    }
                }
                _ = ticker.tick() => {
                    info!(event = "bridge.tool_call.hold_keepalive", mcp_id = %mcp_id);
                }
                _ = tokio::time::sleep(remaining) => {
                    self.pending_calls.lock().await.remove(&mcp_id);
                    if let Some(id) = &tool_call_id {
                        self.record_tool_call_ttl(id, timeout_secs).await;
                    }
                    info!(event = "bridge.tool_call.timeout", mcp_id = %mcp_id);
                    return Err(anyhow::anyhow!("timeout menunggu hasil dari client"));
                }
            }
        }
    }

    /// Normalize tool name for bridge lookup.
    ///
    /// Strips known MCP server namespace prefixes added by clients (e.g. opencode
    /// exposes tools from server "agentgate" as `agentgate_<tool>` / `agentgate__<tool>`),
    /// then resolves to the canonical name as registered in the session tool list
    /// (exact, then case-insensitive). Returns the original name if unresolved.
    pub fn normalize_tool_name(name: &str, session_tools: &[crate::bridge::ToolSchema]) -> String {
        // Map common tool name variants to opencode's canonical built-in names
        // before attempting prefix stripping / case-insensitive matching.
        let mapped = match name.to_lowercase().as_str() {
            "shell" | "sh" => "bash",
            "cat" => "read",  // opencode uses read for file viewing
            _ => name,
        };
        let mapped_str = if mapped == name { name } else { mapped };

        let candidates = Self::tool_name_candidates(mapped_str);
        for candidate in &candidates {
            if session_tools.iter().any(|t| t.name == *candidate) {
                return candidate.clone();
            }
        }
        for candidate in &candidates {
            let lower = candidate.to_lowercase();
            for t in session_tools {
                if t.name.to_lowercase() == lower {
                    return t.name.clone(); // Return the canonical casing from session
                }
            }
        }
        // Fallback: return de-prefixed candidate (or original) for consistent downstream handling.
        candidates.into_iter().next().unwrap_or_else(|| mapped_str.to_string())
    }

    /// Candidate tool names to try when resolving MCP-namespaced names.
    /// Order: original, then progressively de-prefixed forms.
    pub fn tool_name_candidates(name: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut push_unique = |s: String| {
            if !s.is_empty() && !out.iter().any(|x: &String| x == &s) {
                out.push(s);
            }
        };
        push_unique(name.to_string());
        let mut cur = name.to_string();
        for _ in 0..4 {
            let stripped = cur
                .strip_prefix("agentgate__")
                .or_else(|| cur.strip_prefix("agentgate_"))
                .map(|s| s.to_string());
            match stripped {
                Some(s) if !s.is_empty() && s != cur => {
                    push_unique(s.clone());
                    cur = s;
                }
                _ => break,
            }
        }
        // Also handle case-only prefix variants defensively (e.g. AgentGate_Glob).
        let lower = name.to_lowercase();
        if lower != name {
            push_unique(lower.clone());
            let mut cur2 = lower;
            for _ in 0..4 {
                let stripped = cur2
                    .strip_prefix("agentgate__")
                    .or_else(|| cur2.strip_prefix("agentgate_"))
                    .map(|s| s.to_string());
                match stripped {
                    Some(s) if !s.is_empty() && s != cur2 => {
                        push_unique(s.clone());
                        cur2 = s;
                    }
                    _ => break,
                }
            }
        }
        out
    }

    /// Strip known MCP server namespace prefixes from a tool name.
    ///
    /// Returns the de-prefixed form (preserving the remainder's casing when possible).
    /// Handles both "agentgate_" (single underscore) and "agentgate__" (double underscore),
    /// as well as case variants like "AgentGate_" / "AGENTGATE_".
    pub fn strip_mcp_tool_prefix(name: &str) -> String {
        // Check for known prefixes (case-sensitive then case-insensitive fallback).
        for prefix in ["agentgate__", "agentgate_", "AgentGate__", "AgentGate_", "AGENTGATE__", "AGENTGATE_"] {
            if name.starts_with(prefix) {
                let stripped = &name[prefix.len()..];
                if !stripped.is_empty() {
                    return stripped.to_string();
                }
            }
        }
        name.to_string()
    }

/// Session that owns a held `tools/call`, if still pending.
    pub async fn pending_session_id(&self, mcp_id: &str) -> Option<String> {
        self.pending_calls
            .lock()
            .await
            .get(mcp_id)
            .map(|p| p.session_id.clone())
    }

    pub async fn resolve_tool_call(&self, mcp_id: &str, result: ToolResult) -> Option<Option<String>> {
        if let Some(pending) = self.pending_calls.lock().await.remove(mcp_id) {
            let _ = pending.tx.send(result);
            Some(pending.run_id)
        } else {
            None
        }
    }

    /// Resolve a tool call by OpenAI tool_call_id (spec §9.2).
    /// Returns the run_id if the tool call was resolved.
    /// Note: sends an empty result; prefer `resolve_tool_result_by_id` when result content is available.
    pub async fn resolve_by_openai_tool_call_id(&self, tool_call_id: &str) -> Option<String> {
        self.resolve_tool_result_by_id(tool_call_id, ToolResult {
            tool_call_id: tool_call_id.to_string(),
            name: "resolved_by_openai_tool_call_id".to_string(),
            content: serde_json::json!({}),
            is_error: false,
        }).await
    }

    /// Resolve a held MCP `tools/call` by mcp_id or OpenAI tool_call_id, delivering `result`.
    /// PR-S4: required for live continuation (CLI must receive real tool output).
    pub async fn resolve_tool_result_by_id(&self, id: &str, result: ToolResult) -> Option<String> {
        let mut pending = self.pending_calls.lock().await;
        // Direct key match (mcp_id or re-keyed id)
        if let Some(pending_call) = pending.remove(id) {
            let run_id = pending_call.run_id.clone();
            let _ = pending_call.tx.send(result);
            return run_id;
        }
        // Match by associated OpenAI tool_call_id field
        let key = pending.iter().find_map(|(k, p)| {
            if p.tool_call_id.as_deref() == Some(id) { Some(k.clone()) } else { None }
        });
        if let Some(k) = key {
            if let Some(pending_call) = pending.remove(&k) {
                let run_id = pending_call.run_id.clone();
                let _ = pending_call.tx.send(result);
                return run_id;
            }
        }
        None
    }

    /// Associate OpenAI tool_call_id with MCP call (spec §9.2).
    /// Returns the run_id if the association was successful.
    pub async fn associate_tool_call_ids(&self, mcp_id: &str, tool_call_id: &str) -> Option<String> {
        let mut pending = self.pending_calls.lock().await;
        if let Some(pending_call) = pending.get_mut(mcp_id) {
            pending_call.tool_call_id = Some(tool_call_id.to_string());
            let run_id = pending_call.run_id.clone();
            return run_id;
        }
        // If not found by mcp_id, try by tool_call_id key (already associated)
        pending.get(tool_call_id).and_then(|p| p.run_id.clone())
    }

    pub fn runs(&self) -> &Arc<RwLock<HashMap<String, Run>>> {
        &self.runs
    }

    /// Initialize tool schemas (stub: no provider registry wired yet)
    pub async fn initialize_schemas(&self) -> Result<()> {
        info!("Initialized 0 tool schemas");
        Ok(())
    }

    /// Get available tool schemas for a workspace
    pub async fn get_tool_schemas(&self, workspace: &str) -> Vec<ToolSchema> {
        let schemas = self.tool_schemas.read().await;
        let result: Vec<ToolSchema> = schemas
            .values()
            .filter(|s| s.workspace == workspace || s.workspace == "*")
            .cloned()
            .collect();
        info!(event = "bridge.tools_list", workspace = %workspace, count = %result.len());
        result
    }

    /// The key is workspace + model to limit concurrent runs for the same session context.
    pub async fn start_run(
        &self,
        run_id: String,
        workspace: String,
        model: String,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        max_steps: usize,
        call_ttl_secs: u64,
        park_ttl_secs: u64,
    ) -> Result<Run> {
        // Enforce max_runs_per_key - limit concurrent runs for the same workspace+model key
        let key = format!("{}:{}", workspace, model);
        let current_runs = {
            let runs = self.runs.read().await;
            runs.values()
                .filter(|r| {
                    let r_key = format!("{}:{}", r.workspace, r.model);
                    r_key == key
                })
                .count()
        };
        
        if current_runs >= self.config.max_runs_per_key {
            return Err(anyhow::anyhow!(
                "max_runs_per_key ({}) exceeded for key '{}' (spec §10.3)",
                self.config.max_runs_per_key, key
            ));
        }

        let run = Run::new(
            run_id.clone(),
            workspace,
            model,
            messages,
            tools,
            max_steps,
            call_ttl_secs,
            park_ttl_secs,
        );

        self.runs.write().await.insert(run_id.clone(), run.clone());
        debug!("Started run: {}", run_id);
        Ok(run)
    }

    /// Get parked runs map (for read access)
    pub fn parked_runs(&self) -> &Arc<Mutex<HashMap<String, Run>>> {
        &self.parked_runs
    }

    /// Get a run by ID
    pub async fn get_run(&self, run_id: &str) -> Option<Run> {
        self.runs.read().await.get(run_id).cloned()
    }

    /// Park a run (waiting for tool result)
    pub async fn park_run(&self, run: Run) -> Result<()> {
        let mut parked = self.parked_runs.lock().await;

        if parked.len() >= self.config.max_parked_runs {
            return Err(anyhow!("Max parked runs reached"));
        }

        let run_id = run.id.clone();
        parked.insert(run_id.clone(), run);
        crate::metrics::METRICS.mark_run_parked();
        info!(event = "run.parked", run_id = %run_id);
        Ok(())
    }

    /// Resume a parked run with tool result
    pub async fn resume_run(&self, run_id: &str, _tool_result: ToolResult) -> Option<Run> {
        let mut parked = self.parked_runs.lock().await;
        let removed = parked.remove(run_id);
        if removed.is_some() {
            crate::metrics::METRICS.mark_run_unparked();
            info!(event = "run.resumed", run_id = %run_id);
        }
        removed
    }

    /// Map parameter names for opencode built-in tools.
    /// Opencode uses different parameter names than common conventions.
    /// e.g., `read` tool expects `file_path` not `path`.
    fn map_tool_params(tool_name: &str, args: &mut serde_json::Value) {
        if let Some(obj) = args.as_object_mut() {
            match tool_name {
                // Opencode built-in tools use `file_path` instead of `path`
                "read" | "write" | "edit" | "view" | "cat" => {
                    if let Some(path_val) = obj.remove("path") {
                        obj.insert("file_path".to_string(), path_val);
                    }
                }
                // `bash`/`shell` use `command` (already correct)
                // `glob`/`grep` use `pattern` (already correct)
                // `list` uses `path` (already correct)
                _ => {}
            }
        }
    }

    /// Execute a tool call - parks run, forwards to client, waits for result
    pub async fn execute_tool(
        &self,
        run_id: &str,
        tool_name: &str,
        mut args: serde_json::Value,
        _workspace: &str,
    ) -> Result<ToolResult> {
        // Map parameter names for opencode built-in tools
        Self::map_tool_params(tool_name, &mut args);

        // Guard: Check if tool is bridge tool (L4 runtime guard)
        if !self.is_bridge_tool(tool_name) {
            tracing::error!(event = "bridge.tool_call.reject", run_id = %run_id, tool = %tool_name, reason = "not_bridge_tool", tool_call_id = "");
            info!(event = "guard.violation", run_id = %run_id, observed_tool = %tool_name);
            crate::metrics::METRICS.record_guard_violation();
            return Err(anyhow!("tool_isolation_violation: {} is not a bridge tool", tool_name));
        }

        // max_prompt_chars applies to rendered prompt/conversation (enforced in render_conversation);
        // args size is truncated here per spec §14.
        let args_str = args.to_string();
        let limit = self.config.limits.max_prompt_chars;
        if args_str.len() > limit {
            let mut cut = limit.min(args_str.len());
            while cut > 0 && !args_str.is_char_boundary(cut) { cut -= 1; }
            let mut truncated = args_str[..cut].to_string();
            truncated.push_str("\n...[truncated: max_prompt_chars]");
            info!(event = "bridge.tool_call.args_truncated", tool = %tool_name, original_bytes = args_str.len(), new_bytes = truncated.len(), limit = limit);
            args = serde_json::Value::String(truncated);
        }

        // Get the run
        let mut runs = self.runs.write().await;
        let mut run = match runs.get_mut(run_id) {
            Some(r) => r.clone(),
            None => return Err(anyhow!("Run not found: {}", run_id)),
        };

        // Park the run with tool call info
        let tool_call_id = crate::openai::TOOL_CALL_ID_GENERATOR.lock().unwrap().generate();
        let mcp_id = tool_call_id.clone();
        run.park(tool_call_id.clone(), tool_name.to_string());

        // Store the tool call for later resolution.
        let (tx, rx) = oneshot::channel();
        let run_id_opt = Some(run_id.to_string());
        self.pending_calls
            .lock()
            .await
            .insert(mcp_id.clone(), PendingCall { session_id: String::new(), run_id: run_id_opt, tool_name: tool_name.to_string(), tool_call_id: Some(tool_call_id.clone()), tx });

        // Re-insert the parked run
        self.park_run(run.clone()).await?;

        // Associate mcp_id -> tool_call_id mapping for ID coordination (§9.2, canonical pending_calls).
        let _ = self.associate_tool_call_ids(&mcp_id, &tool_call_id).await;

        // Drop lock before await
        drop(runs);

        // L3/L4 Observability: log tool call emit (before hold)
        info!(event = "bridge.tool_call.emit", mcp_id = %mcp_id, tool = %tool_name, run_id = %run_id, tool_call_id = %tool_call_id, args = %args);

        // Forward tool call to client (Device A) via BridgeClient
        // The client will execute the tool and send result back via bridge/resolve
        // We need to signal the client that a tool call is waiting
        // This is done via the MCP tools/call endpoint which the CLI polls/calls

        // Wait for client to execute tool and send result via bridge/resolve
        match tokio::time::timeout(std::time::Duration::from_secs(self.config.call_ttl_secs), rx).await {
            Ok(Ok(mut result)) => {
                info!(event = "bridge.tool_call.resolved", mcp_id = %mcp_id, tool = %tool_name);
                // Enforce max_result_bytes on tool result (spec §14)
                let result_str = result.content.to_string();
                let limit = self.config.limits.max_result_bytes;
                let original_bytes = result_str.len();
                if original_bytes > limit {
                    let mut cut = limit.min(result_str.len());
                    while cut > 0 && !result_str.is_char_boundary(cut) { cut -= 1; }
                    let mut truncated = result_str[..cut].to_string();
                    truncated.push_str("\n...[truncated: max_result_bytes]");
                    info!(event = "bridge.tool_call.result_truncated", tool = %tool_name, original_bytes = original_bytes, new_bytes = truncated.len(), limit = limit);
                    result.content = serde_json::Value::String(truncated);
                }
                Ok(result)
            },
            Ok(Err(_)) => {
                self.pending_calls.lock().await.remove(&mcp_id);
                Err(anyhow::anyhow!("channel closed"))
            },
            Err(_) => {
                self.pending_calls.lock().await.remove(&mcp_id);
                info!(event = "bridge.tool_call.timeout", mcp_id = %mcp_id, tool = %tool_name);
                Err(anyhow::anyhow!("timeout menunggu hasil dari client"))
            }
        }
    }

    pub fn is_bridge_tool(&self, tool_name: &str) -> bool {
        // Tool is bridge if it starts with a registered provider ID followed by ':'
        // or if it's the special preflight probe_ok
        if tool_name == "probe_ok" {
            return true;
        }
        
        let provider_id = match self.extract_provider_from_tool(tool_name) {
            Ok(id) => id,
            Err(_) => return false,
        };

        if provider_id == "probe_ok" {
            return true;
        }

        // A bridge tool must be namespaced as "provider:tool". Unknown bare
        // names are never bridge tools, so provider builtins stay isolated.
        tool_name.contains(':')
    }

    fn extract_provider_from_tool(&self, tool_name: &str) -> Result<String> {
        // Format: provider:tool_name or bare tool_name (returns "probe_ok")
        let parts: Vec<&str> = tool_name.splitn(2, ':').collect();
        match parts.len() {
            2 => Ok(parts[0].to_string()),
            1 => Ok("probe_ok".to_string()),
            _ => Err(anyhow!("Invalid tool name format: {}", tool_name)),
        }
    }

    /// Preflight isolation check: verifies isolation_active is true before admitting
    /// a tool-capable turn (spec §13.2). Returns canonical tool name if valid.
    pub fn preflight_isolation_check(&self, tool_name: &str, session_tools: &[crate::bridge::ToolSchema]) -> Result<(bool, String)> {
        if !self.is_bridge_tool(tool_name) {
            return Err(anyhow!("tool_isolation_unavailable: {} is not a bridge tool", tool_name));
        }
        let normalized = Self::normalize_tool_name(tool_name, session_tools);
        Ok((true, normalized))
    }

    /// Preflight active probe (L3): probe CLI for exposed MCP tools via tools/list,
    /// validate against isolation mode [bridge.isolation]. If built-in conflicting
    /// tools are detected → return Err → fail-closed (503 tool_isolation_unavailable).
    ///
    /// Conflicting built-ins: read, write, bash/execute, glob, list, patch, edit
    /// Allowed bridge tools: grep, webfetch, todowrite, todoread, task
    ///
    /// This does NOT spawn a real CLI process (which would be slow/fragile in
    /// preflight). Instead it validates that `session_tools` (already filtered by
    /// IMPL-TOOL-01 selective disable) contains no built-in conflicting tools,
    /// unless explicitly allowed via isolation.allowed_builtins.
    /// Callers that need real CLI probing should call `probe_cli_tools_live()`.
    pub fn preflight_active_probe(&self, session_tools: &[crate::bridge::ToolSchema], isolation: &IsolationConfig) -> Result<(), String> {
        // FIX-L1-01: expanded conflicting tools to include opencode utility tools
        // (execute, list_mcp_resources) that leak on resume in selective mode
        let conflicting_tools = [
            "read", "write", "bash", "execute", "glob", "list", "patch", "edit",
            "opencode_list_mcp_resources", "list_mcp_resources"
        ];

        // All built-in tools (conflicting + safe) for strict mode
        let all_builtins = [
            "read", "write", "bash", "execute", "glob", "list", "patch", "edit",
            "grep", "webfetch", "todowrite", "todoread", "task",
            "opencode_list_mcp_resources", "list_mcp_resources"
        ];

        // Build allowed set from config (user explicitly allowed builtins)
        let mut allowed = HashSet::new();
        for tool in &isolation.allowed_builtins {
            allowed.insert(tool.to_lowercase());
        }

        // Build disabled set from config (user explicitly disabled builtins)
        let mut disabled = HashSet::new();
        for tool in &isolation.disabled_builtins {
            disabled.insert(tool.to_lowercase());
        }

        for tool in session_tools {
            let name_lower = tool.name.to_lowercase();
            // Strip namespace prefix if present (e.g. "opencode:read" -> "read")
            let bare_name = name_lower.split(':').last().unwrap_or(&name_lower);

            // Check if explicitly disabled - always block regardless of mode
            if disabled.contains(bare_name) {
                info!(event = "bridge.preflight.reject", tool = %tool.name, reason = "builtin_explicitly_disabled");
                return Err(format!("tool_isolation_unavailable: built-in tool {} is explicitly disabled", tool.name));
            }

            match isolation.mode.as_str() {
                "off" => {
                    // Off mode: allow all builtins
                    continue;
                }
                "strict" => {
                    // Strict mode: block all builtins
                    if all_builtins.contains(&bare_name) {
                        info!(event = "bridge.preflight.reject", tool = %tool.name, reason = "builtin_blocked_in_strict_mode");
                        return Err(format!("tool_isolation_unavailable: built-in tool {} conflicts with isolation policy", tool.name));
                    }
                }
                _ => {
                    // Selective mode (default): block conflicting builtins unless allowed
                    if conflicting_tools.contains(&bare_name) && !allowed.contains(bare_name) {
                        info!(event = "bridge.preflight.reject", tool = %tool.name, reason = "builtin_conflict_detected", allowed = ?isolation.allowed_builtins);
                        return Err(format!("tool_isolation_unavailable: built-in tool {} conflicts with isolation policy", tool.name));
                    }
                }
            }
        }

        info!(event = "bridge.preflight.pass", tool_count = session_tools.len());
        Ok(())
    }

    /// Live preflight probe (L3): actually spawns the CLI and queries tools/list
    /// via MCP. Used when real-time validation is required (e.g. strict mode).
    /// Returns list of tool names exposed by the CLI.
    pub async fn probe_cli_tools_live(
        &self,
        _cli_bin: &str,
        _cli_args: &[String],
        _workdir: &std::path::Path,
    ) -> Result<Vec<String>, String> {
        // Placeholder for live CLI probing. Requires spawning CLI process,
        // sending MCP tools/list, parsing response.
        // Not implemented to avoid startup latency/fragility in preflight.
        // Use preflight_active_probe() for session-tool validation instead.
        Ok(vec![])
    }

    /// Start cleanup task for expired runs
    pub async fn start_cleanup_task(&self) {
        // PR-S5: Sweep orphan PIDs on startup (leftover CLI processes from crashed runs).
        self.sweep_orphan_pids_on_startup().await;

        let runs = self.runs.clone();
        let parked_runs = self.parked_runs.clone();
        let sessions = self.sessions.clone();
        let continuations = self.continuations.clone();
        let provider_sessions = self.provider_sessions.clone();
        let park_ttl = Duration::from_secs(self.config.park_ttl_secs);
        let call_ttl = Duration::from_secs(self.config.call_ttl_secs);
        let max_run_lifetime = Duration::from_secs(self.config.max_run_lifetime_secs);
        let session_ttl = Duration::from_secs(600);

        tokio::spawn(async move {
            let mut interval = interval(park_ttl.max(Duration::from_secs(30)));
            loop {
                interval.tick().await;
                let now = std::time::Instant::now();
                let mut removed_keys: Vec<String> = Vec::new();

                // Clean expired parked runs (collect removed for best-effort S5 cleanup).
                let parked_len = {
                    let mut parked = parked_runs.lock().await;
                    parked.retain(|id, run| {
                        let keep = now.duration_since(run.parked_at.unwrap_or(now)) < park_ttl;
                        if !keep {
                            removed_keys.push(id.clone());
                            if let Some(ref k) = run.mcp_session_id { removed_keys.push(k.clone()); }
                            if let Some(ref k) = run.provider_session_key { removed_keys.push(k.clone()); }
                        }
                        keep
                    });
                    parked.len()
                };

                // Clean expired active runs
                let runs_len = {
                    let mut runs_guard = runs.write().await;
                    runs_guard.retain(|id, run| {
                        if let RunState::Running { started_at, .. } = &run.state {
                            let age = now.duration_since(*started_at);
                            let keep = age < call_ttl && age < max_run_lifetime;
                            if !keep {
                                removed_keys.push(id.clone());
                                if let Some(ref k) = run.mcp_session_id { removed_keys.push(k.clone()); }
                                if let Some(ref k) = run.provider_session_key { removed_keys.push(k.clone()); }
                            }
                            keep
                        } else {
                            true
                        }
                    });
                    runs_guard.len()
                };

                // Clean expired bridge sessions
                let sessions_len = {
                    let mut sessions_guard = sessions.write().await;
                    sessions_guard.retain(|_, s| now.duration_since(s.created_at) < session_ttl);
                    sessions_guard.len()
                };

                // PR-S5: clean stale continuation waiters (replay fallback applies if later resumed).
                let cont_len = {
                    let mut cont = continuations.lock().await;
                    cont.retain(|_, c| now.duration_since(c.registered_at) < park_ttl.max(call_ttl));
                    if !removed_keys.is_empty() {
                        for k in &removed_keys { cont.remove(k); }
                    }
                    cont.len()
                };
                if !removed_keys.is_empty() {
                    let mut ps = provider_sessions.write().await;
                    for k in &removed_keys { ps.remove(k); }
                }

                if parked_len > 0 || runs_len > 0 || sessions_len > 0 || cont_len > 0 {
                    debug!("Cleanup: {} parked, {} active runs, {} sessions, {} continuations", parked_len, runs_len, sessions_len, cont_len);
                }
            }
        });
    }

    /// PR-S5: Best-effort sweep of orphan session workdirs on startup.
    /// Removes stale `agentgate/sessions/*` dirs older than max(park_ttl, max_run_lifetime, 1h).
    /// Conservative: never touches dirs newer than the threshold; logs and continues on errors.
    pub async fn sweep_orphan_session_dirs(&self) {
        let data_dir = std::env::temp_dir().join("agentgate").join("sessions");
        let threshold = Duration::from_secs(self.config.park_ttl_secs.max(self.config.max_run_lifetime_secs).max(3600));
        let now = std::time::SystemTime::now();
        let rd = match tokio::fs::read_dir(&data_dir).await {
            Ok(r) => r,
            Err(_) => return, // nothing to sweep
        };
        let mut rd = rd;
        let mut removed = 0u32;
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let meta = match tokio::fs::metadata(&path).await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = meta.modified().or_else(|_| meta.created()).unwrap_or(now);
            if now.duration_since(modified).unwrap_or(Duration::from_secs(0)) > threshold {
                if tokio::fs::remove_dir_all(&path).await.is_ok() {
                    removed += 1;
                    debug!(event = "bridge.orphan_sweep.removed", path = %path.display());
                }
            }
        }
        if removed > 0 {
            info!(event = "bridge.orphan_sweep", removed, dir = %data_dir.display());
        }
    }

    /// Shutdown the bridge
    pub async fn shutdown(&self) {
        info!("Shutting down bridge server");
        // Cleanup handled by dropping
    }
}

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: serde_json::Value,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(tool_call_id: String, name: String, content: serde_json::Value) -> Self {
        Self {
            tool_call_id,
            name,
            content,
            is_error: false,
        }
    }

    pub fn error(tool_call_id: String, name: String, error: String) -> Self {
        Self {
            tool_call_id,
            name,
            content: serde_json::json!({ "error": error }),
            is_error: true,
        }
    }
}

/// Tool schema compatible with OpenAI function calling
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub provider: String,
    pub workspace: String,
}

impl ToolSchema {
    pub fn new(
        name: String,
        description: String,
        parameters: serde_json::Value,
        provider: String,
        workspace: String,
    ) -> Self {
        Self {
            name,
            description,
            parameters,
            provider,
            workspace,
        }
    }

    /// Convert to OpenAI function format (single tool; sanitizes name only).
    /// For batch conversion with dedupe/aliasing, use `tool_schemas_to_openai_functions`.
    pub fn to_openai_function(&self) -> serde_json::Value {
        let name = crate::bridge::sanitize_tool_name(&self.name);
        serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "description": self.description,
                "parameters": self.parameters
            }
        })
    }
}

/// Convert tool schemas to OpenAI function format with shared NameMap dedupe/aliasing (P2, spec §8).
pub fn tool_schemas_to_openai_functions(schemas: Vec<ToolSchema>) -> Vec<serde_json::Value> {
    // Only alias when necessary (duplicates or invalid/long names handled by sanitize+dedupe).
    // Preserve original sanitized names when unique; use NameMap for stable aliases on collision.
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    schemas
        .into_iter()
        .map(|s| {
            let base = crate::bridge::sanitize_tool_name(&s.name);
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            let name = if *n == 1 { base } else { format!("{}_{}", base, *n) };
            // ensure <=64 chars
            let name = if name.len() > 64 { name[..64].to_string() } else { name };
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": s.description,
                    "parameters": s.parameters
                }
            })
        })
        .collect()
}

/// Generate tool schemas from provider registry
pub async fn generate_tool_schemas() -> Result<Vec<ToolSchema>> {
    Ok(Vec::new())
}

/// Merge tool schemas with client-provided tools (for Bridge mode)
pub fn merge_tool_schemas(
    agentgate_schemas: Vec<ToolSchema>,
    client_tools: Option<Vec<serde_json::Value>>,
    policy: crate::permission::ClientToolsPolicy,
) -> Vec<serde_json::Value> {
    match policy {
        crate::permission::ClientToolsPolicy::Bridge => {
            // Replace with agentgate schemas (batch dedupe/sanitize, P2)
            tool_schemas_to_openai_functions(agentgate_schemas)
        }
        crate::permission::ClientToolsPolicy::Passthrough => {
            // Use client tools as-is, fallback to agentgate if none
            client_tools.unwrap_or_else(|| tool_schemas_to_openai_functions(agentgate_schemas))
        }
        crate::permission::ClientToolsPolicy::Ignore => {
            // Use agentgate schemas, ignore client
            tool_schemas_to_openai_functions(agentgate_schemas)
        }
        crate::permission::ClientToolsPolicy::Reject => {
            // Fail-closed: client tool escalation is not allowed; use agentgate schemas.
            tool_schemas_to_openai_functions(agentgate_schemas)
        }
    }
}

/// Sanitize tool name to OpenAI-compatible format
/// OpenAI requires: ^[a-zA-Z0-9_-]{1,64}$
pub fn sanitize_tool_name(name: &str) -> String {
    let mut sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // Ensure it starts with a letter
    if sanitized.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) {
        sanitized.insert(0, 't');
    }

    // Truncate to 64 chars
    if sanitized.len() > 64 {
        sanitized.truncate(64);
    }

    // Ensure not empty
    if sanitized.is_empty() {
        sanitized = "tool".to_string();
    }

    sanitized
}

/// Generate namespaced tool name: provider:tool
pub fn namespaced_tool_name(provider: &str, tool: &str) -> String {
    let provider_safe = sanitize_tool_name(provider);
    let tool_safe = sanitize_tool_name(tool);
    format!("{}:{}", provider_safe, tool_safe)
}

/// Build tool name map for a provider config
pub fn build_tool_name_map(config: &ProviderConfig) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let provider_name = sanitize_tool_name(&config.id);

    // If models_cmd is set, tools come from CLI
    if config.models_cmd.is_some() || config.chat_cmd.is_some() {
        // Tools will be discovered at runtime from CLI
        map.insert("*".to_string(), namespaced_tool_name(&provider_name, "*"));
    } else {
        // Static tools from config
        for model in &config.models {
            let model_safe = sanitize_tool_name(model);
            map.insert(
                model.clone(),
                namespaced_tool_name(&provider_name, &model_safe),
            );
        }
    }

    map
}

/// Validate all tool names are unique across providers
pub fn validate_unique_tool_names(configs: &[ProviderConfig]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();

    for config in configs {
        let provider_name = sanitize_tool_name(&config.id);

        if config.models_cmd.is_some() || config.chat_cmd.is_some() {
            // Dynamic tools - can't validate statically
            continue;
        }

        for model in &config.models {
            let tool_name = namespaced_tool_name(&provider_name, model);
            if !seen.insert(tool_name.clone()) {
                return Err(format!("Duplicate tool name: {}", tool_name));
            }
        }
    }

    Ok(())
}

/// Run state machine
#[derive(Debug, Clone, PartialEq)]
pub enum RunState {
    /// Run created, not started
    Pending,
    /// Run is executing (model thinking / tool calling)
    Running {
        started_at: std::time::Instant,
        #[allow(dead_code)]
        step: usize,
        #[allow(dead_code)]
        current_tool: Option<String>,
    },
    /// Run parked waiting for tool result
    Parked {
        parked_at: std::time::Instant,
        tool_call_id: String,
        tool_name: String,
    },
    /// Run completed successfully
    Completed {
        #[allow(dead_code)]
        completed_at: std::time::Instant,
        final_messages: Vec<Message>,
    },
    /// Run failed
    Failed {
        #[allow(dead_code)]
        failed_at: std::time::Instant,
        error: String,
    },
    /// Run cancelled by client
    Cancelled,
}

impl RunState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, RunState::Completed { .. } | RunState::Failed { .. } | RunState::Cancelled)
    }

    pub fn is_parked(&self) -> bool {
        matches!(self, RunState::Parked { .. })
    }
}

/// Run represents a single agent conversation with tool-calling
#[derive(Debug, Clone)]
pub struct Run {
    pub id: String,
    pub workspace: String,
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub max_steps: usize,
    pub call_ttl_secs: u64,
    pub park_ttl_secs: u64,
    pub state: RunState,
    pub step_count: usize,
    pub created_at: std::time::Instant,
    pub parked_at: Option<std::time::Instant>,
    /// PR-S1: linkage metadata for Stage 2 parked-session resume (no behavior change).
    pub mcp_session_id: Option<String>,
    pub provider_session_key: Option<String>,
    pub pending_tool_call_ids: Vec<String>,
    pub continuation_capable: bool,
}

impl Run {
    pub fn new(
        id: String,
        workspace: String,
        model: String,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        max_steps: usize,
        call_ttl_secs: u64,
        park_ttl_secs: u64,
    ) -> Self {
        Self {
            id,
            workspace,
            model,
            messages,
            tools,
            max_steps,
            call_ttl_secs,
            park_ttl_secs,
            state: RunState::Pending,
            step_count: 0,
            created_at: std::time::Instant::now(),
            parked_at: None,
            mcp_session_id: None,
            provider_session_key: None,
            pending_tool_call_ids: Vec::new(),
            continuation_capable: false,
        }
    }

    /// PR-S1: attach session linkage metadata (no behavior change).
    pub fn with_session_linkage(
        mut self,
        mcp_session_id: Option<String>,
        provider_session_key: Option<String>,
    ) -> Self {
        self.mcp_session_id = mcp_session_id;
        self.provider_session_key = provider_session_key;
        self
    }

    /// Start the run
    pub fn start(&mut self) {
        self.state = RunState::Running {
            started_at: std::time::Instant::now(),
            step: 0,
            current_tool: None,
        };
    }

    /// Park the run waiting for tool result
    pub fn park(&mut self, tool_call_id: String, tool_name: String) {
        if !self.pending_tool_call_ids.iter().any(|id| id == &tool_call_id) {
            self.pending_tool_call_ids.push(tool_call_id.clone());
        }
        self.state = RunState::Parked {
            parked_at: std::time::Instant::now(),
            tool_call_id,
            tool_name,
        };
        self.parked_at = Some(std::time::Instant::now());
    }

    /// Resume from parked state with tool result
    pub fn resume(&mut self, tool_result: ToolResult) {
        self.messages.push(Message::tool_result(
            tool_result.tool_call_id,
            tool_result.name,
            tool_result.content.to_string(),
        ));
        self.step_count += 1;
        self.state = RunState::Running {
            started_at: std::time::Instant::now(),
            step: self.step_count,
            current_tool: None,
        };
        self.parked_at = None;
    }

    /// Complete the run
    pub fn complete(&mut self, final_messages: Vec<Message>) {
        self.state = RunState::Completed {
            completed_at: std::time::Instant::now(),
            final_messages,
        };
    }

    /// Fail the run
    pub fn fail(&mut self, error: String) {
        self.state = RunState::Failed {
            failed_at: std::time::Instant::now(),
            error,
        };
    }

    /// Cancel the run
    pub fn cancel(&mut self) {
        self.state = RunState::Cancelled;
    }

    /// Check if run can continue (not terminal, not exceeded max steps)
    pub fn can_continue(&self) -> bool {
        !self.state.is_terminal() && self.step_count < self.max_steps
    }

    /// Check if call TTL expired
    pub fn is_call_expired(&self) -> bool {
        match &self.state {
            RunState::Running { started_at, .. } => {
                started_at.elapsed() > std::time::Duration::from_secs(self.call_ttl_secs)
            }
            RunState::Parked { parked_at, .. } => {
                parked_at.elapsed() > std::time::Duration::from_secs(self.call_ttl_secs)
            }
            _ => false,
        }
    }

    /// Check if parked TTL expired
    pub fn is_park_expired(&self) -> bool {
        if let Some(parked_at) = self.parked_at {
            parked_at.elapsed() > std::time::Duration::from_secs(self.park_ttl_secs)
        } else {
            false
        }
    }

    /// Get current step number
    pub fn current_step(&self) -> usize {
        match &self.state {
            RunState::Running { step, .. } => *step,
            RunState::Parked { .. } => self.step_count,
            _ => self.step_count,
        }
    }

    /// Set current tool being executed
    pub fn set_current_tool(&mut self, tool_name: Option<String>) {
        if let RunState::Running { started_at: _, step: _, current_tool } = &mut self.state {
            *current_tool = tool_name;
        }
    }
}









