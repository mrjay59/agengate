//! Gateway management plane: provider/model/proxy catalog, proxy pools and
//! quota-aware free-model load balancing.
//!
//! The module is deliberately self-contained: it owns its own JSON state file
//! (`AGENTGATE_GATEWAY_STATE_PATH`, default `<config dir>/agentgate/gateway.json`)
//! so the existing CLI-bridge path and config file stay untouched. Runtime
//! callers reach the shared state through [`state`].

pub mod admin;
pub mod lb;
pub mod proxy;
pub mod store;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

pub use lb::{LbCandidate, LbSelection};
pub use store::{
    ComboCandidate, GatewayDocument, GatewayError, GatewayProvider, GatewayState, LbStrategy,
    ModelRecord, PoolConfig, PoolMember, ProxyConfig, ProxyMode, ProxyStrategy, StickyKey,
    DEFAULT_FREE_FALLBACK, DEFAULT_FREE_POOL,
};

static GLOBAL_STATE: OnceLock<Arc<GatewayState>> = OnceLock::new();

/// Default path for the persisted gateway catalog.
pub fn default_state_path() -> PathBuf {
    if let Ok(path) = std::env::var("AGENTGATE_GATEWAY_STATE_PATH") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|_| std::env::var("USERPROFILE").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("agentgate").join("gateway.json")
}

/// Process-wide gateway state (lazy, persisted on every mutation).
pub fn state() -> Arc<GatewayState> {
    GLOBAL_STATE
        .get_or_init(|| Arc::new(GatewayState::new(default_state_path())))
        .clone()
}

/// Install a custom state (tests / embedded usage). No-op once initialised.
pub fn try_init_state(custom: GatewayState) -> bool {
    GLOBAL_STATE.set(Arc::new(custom)).is_ok()
}
