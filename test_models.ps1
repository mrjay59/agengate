#!/usr/bin/env pwsh
$env:RUST_LOG="agentgate=debug,clap=error,axum=error,tower_http=error"
cargo run --release -- models 2>&1 | Select-String -Pattern "provider|model|ERROR|agentgate", "", "[DEBUG]"