use serde::{Deserialize, Serialize};

/// Kebijakan ketika client tidak mengirim tools tapi model seharusnya bisa memanggil tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NoToolsPolicy {
    /// Lewati tool-calling, jawab chat biasa (default)
    #[default]
    #[serde(alias = "chat_only")]
    ChatOnly,
    /// Paksa attach tool schema dari config/workspace (bridge/native)
    #[serde(alias = "bridge")]
    Bridge,
    /// Tolak request dengan 400
    #[serde(alias = "reject")]
    Reject,
}

/// Mode izin untuk operasi file/shell oleh CLI agent.
/// Default: `read-only` (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    #[default]
    ReadOnly,
    AutoEdit,
    FullAuto,
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            PermissionMode::ReadOnly => "read-only",
            PermissionMode::AutoEdit => "auto-edit",
            PermissionMode::FullAuto => "full-auto",
        };
        write!(f, "{}", s)
    }
}

/// Cara menangani field `tools`/`tool_choice` yang dikirim client OpenAI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ClientToolsPolicy {
    /// Gantikan dengan tool schema agentgate (default)
    #[default]
    Bridge,
    /// Forward apa adanya ke upstream
    Passthrough,
    /// Abaikan tools client, gunakan config agentgate
    Ignore,
    /// Tolak request yang membawa `tools[]` (fail-closed, HTTP 501
    /// `tool_bridge_unsupported` — spec §7.6, §13.1, spec_tooling.md §56).
    Reject,
}

/// Kebijakan aktivitas tool-calling selama run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ToolActivityPolicy {
    #[default]
    Normal,
    ReadOnly,
    Disabled,
    Hide,
    Inline,
    Reasoning,
    Extension,
}
