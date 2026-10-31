use agentgate::config::ProviderConfig;
use agentgate::provider::{CliProvider, CliProviderImpl, strip_ansi_codes};

#[test]
fn test_strip_ansi_codes() {
    // Test comprehensive ANSI code stripping
    let input = "\x1b[32mmodel-name\x1b[0m";
    assert_eq!(strip_ansi_codes(input), "model-name");

    // Test bold + color combination
    let input = "\x1b[1;32mmodel-a\x1b[0m\n\x1b[1;33mmodel-b\x1b[0m";
    assert_eq!(strip_ansi_codes(input), "model-a\nmodel-b");

    // Test various color codes
    let input = "\x1b[31mred\x1b[32mgreen\x1b[33myellow\x1b[34mblue\x1b[0m";
    assert_eq!(strip_ansi_codes(input), "redgreenyellowblue");

    // No ANSI codes
    let input = "plain-text";
    assert_eq!(strip_ansi_codes(input), "plain-text");

    // Empty string
    assert_eq!(strip_ansi_codes(""), "");

    // Multiple codes on single line
    let input = "\x1b[1;32m\x1b[4munderlined-bold\x1b[0m";
    assert_eq!(strip_ansi_codes(input), "underlined-bold");
}

#[test]
fn test_parse_models_output_plain() {
    let config = ProviderConfig {
        id: "test".to_string(),
        enabled: true,
        bin: Some("test".to_string()),
        models: vec![],
        models_cmd: None,
        chat_cmd: None,
        strip_ansi: true,
    };
    let provider = CliProviderImpl::new(config);

    let output = "model-a\nmodel-b\nmodel-c";
    let result = provider.parse_models_output(&output);
    assert_eq!(result, vec!["model-a", "model-b", "model-c"]);
}

#[test]
fn test_parse_models_output_json_array() {
    let config = ProviderConfig {
        id: "test".to_string(),
        enabled: true,
        bin: Some("test".to_string()),
        models: vec![],
        models_cmd: None,
        chat_cmd: None,
        strip_ansi: false,
    };
    let provider = CliProviderImpl::new(config);

    let output = r#"["gpt-4","gpt-3.5-turbo","claude-3-opus"]"#;
    let result = provider.parse_models_output(&output);
    assert_eq!(result, vec!["gpt-4", "gpt-3.5-turbo", "claude-3-opus"]);
}

#[test]
fn test_parse_models_output_json_object() {
    let config = ProviderConfig {
        id: "test".to_string(),
        enabled: true,
        bin: Some("test".to_string()),
        models: vec![],
        models_cmd: None,
        chat_cmd: None,
        strip_ansi: false,
    };
    let provider = CliProviderImpl::new(config);

    let output = r#"{"models":["model1","model2"]}"#;
    let result = provider.parse_models_output(&output);
    assert_eq!(result, vec!["model1", "model2"]);
}

#[test]
fn test_parse_models_output_with_ansi() {
    let config = ProviderConfig {
        id: "test".to_string(),
        enabled: true,
        bin: Some("test".to_string()),
        models: vec![],
        models_cmd: None,
        chat_cmd: None,
        strip_ansi: true,
    };
    let provider = CliProviderImpl::new(config);

    let output = "\x1b[1;32mmodel-a\x1b[0m\n\x1b[33mmodel-b\x1b[0m\nmodel-c";
    let result = provider.parse_models_output(&output);
    assert_eq!(result, vec!["model-a", "model-b", "model-c"]);
}

#[test]
fn test_check_available_opencode() {
    let config = ProviderConfig {
        id: "opencode".to_string(),
        enabled: true,
        bin: Some("opencode".to_string()),
        models: vec![],
        models_cmd: None,
        chat_cmd: None,
        strip_ansi: true,
    };

    let provider = CliProviderImpl::new(config);
    let result = futures::executor::block_on(provider.check_available());
    println!("check_available result: {}", result);

    #[cfg(target_os = "windows")]
    assert!(result, "opencode.cmd should be detected on Windows");
}

#[test]
fn test_config_loads_all_providers() {
    // Create a temp config file with multiple providers
    let temp_dir = std::env::temp_dir();
    let config_path = temp_dir.join("agentgate_test_config.toml");

    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 8787

[defaults]
workdir = "."
max_concurrency_per_provider = 2
model_cache_ttl_secs = 300

[[provider]]
id = "openai"
bin = "openai"
models = ["gpt-4", "gpt-3.5-turbo"]

[[provider]]
id = "anthropic"
bin = "claude"
models = ["claude-3-opus", "claude-3-sonnet"]

[[provider]]
id = "opencode"
bin = "opencode"
models_cmd = ["--list-models"]
strip_ansi = true
"#;

    std::fs::write(&config_path, toml_content).unwrap();

    let content = std::fs::read_to_string(&config_path).unwrap();
    let config: agentgate::config::Config = toml::from_str(&content).unwrap();

    assert_eq!(config.provider.len(), 3, "All 3 providers should be loaded");
    assert_eq!(config.provider[0].id, "openai");
    assert_eq!(config.provider[1].id, "anthropic");
    assert_eq!(config.provider[2].id, "opencode");
    assert!(config.provider[2].models_cmd.is_some());
    assert_eq!(
        config.provider[2].models_cmd.as_ref().unwrap(),
        &vec!["--list-models".to_string()]
    );

    // Clean up
    std::fs::remove_file(&config_path).ok();
}
