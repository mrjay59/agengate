use serde::{Deserialize, Serialize};

/// Kebijakan ketika client tidak mengirim tools tapi model seharusnya bisa memanggil tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NoToolsPolicy {
    /// Lewati tool-calling, jawab chat biasa (default)
    #[default]
    ChatOnly,
    /// Paksa attach tool schema dari config/workspace (bridge/native)
    Bridge,
    /// Tolak request dengan 400
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
