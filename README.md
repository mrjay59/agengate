# AgentGate

Gateway Provider untuk CLI Agent (OpenAI-compatible).

## Overview

AgentGate adalah layanan middleware yang menjembatani aplikasi dengan berbagai provider CLI agent seperti OpenCode, Copilot CLI, Gemini CLI, dan lain-lain.

## Project Structure

```
agentgate/
├── src/
│   ├── main.rs      # Entry point & CLI commands
│   ├── lib.rs       # Library exports
│   ├── api.rs       # HTTP API endpoints (axum)
│   ├── cli.rs       # CLI execution logic
│   ├── config.rs    # Configuration loading
│   ├── error.rs     # Error handling
│   ├── provider.rs  # Provider trait & CLI provider impl
│   ├── service.rs   # Service registry
│   └── types.rs     # Data types
├── tests/
│   └── provider_test.rs
├── Cargo.toml
├── agentgate.example.toml
├── README.md
└── plan.md
```

## Quick Start

### Build
```bash
cargo build --release
```

### Initialize config
```bash
./target/release/agentgate init
```

### List providers
```bash
./target/release/agentgate providers
```

### List models
```bash
./target/release/agentgate models
```

### Run server
```bash
./target/release/agentgate serve --host 127.0.0.1 --port 8787
```

### API Endpoints

- `GET /v1/models` - List available models
- `POST /v1/chat/completions` - Chat completion (OpenAI-compatible)
- `GET /health` - Health check

## Features

- OpenAI-compatible API
- Auto-detection of CLI tools via PATH
- Multi-provider support (OpenCode, GitHub Copilot, Gemini, etc.)
- Configurable concurrency limits
- Request validation and input sanitization
- Secure architecture with error handling

## Security Considerations

- CLI binary validation before execution
- Input sanitization for prompts
- Timeout enforcement
- No direct system command execution without validation
- Logging for audit trail

## Running Tests

```bash
cargo test
cargo test --release
```

## Configuration

Edit `~/.config/agentgate.toml` to configure providers:

```toml
[server]
host = "127.0.0.1"
port = 8787

[[provider]]
id = "opencode"
bin = "opencode"
models = ["gpt-4o", "gpt-4"]
```