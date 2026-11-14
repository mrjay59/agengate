with open('src/config.rs', 'r') as f:
    content = f.read()

# Add ollama_stream_timeout to ProviderConfig
old = '''/// Ollama keep-alive duration for CLI mode (e.g. "10m").
    #[serde(default)]
    pub keep_alive: Option<String>,
    /// When true, expose tools to a CLI/model that lacks native tool calling by
    /// prompting for JSON tool calls and parsing them from the output.
    #[serde(default)]
    pub prompt_tools_fallback: bool,
}'''
new = '''/// Ollama keep-alive duration for CLI mode (e.g. "10m").
    #[serde(default)]
    pub keep_alive: Option<String>,
    /// Ollama stream timeout in seconds.
    #[serde(default)]
    pub ollama_stream_timeout: Option<u64>,
    /// When true, expose tools to a CLI/model that lacks native tool calling by
    /// prompting for JSON tool calls and parsing them from the output.
    #[serde(default)]
    pub prompt_tools_fallback: bool,
}'''
content = content.replace(old, new)

with open('src/config.rs', 'w') as f:
    f.write(content)
print('config.rs done')