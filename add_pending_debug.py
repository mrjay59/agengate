with open('tests/bridge_resolve_auth_test.rs', 'r') as f:
    content = f.read()

# Add pending_count usage
content = content.replace(
    'tokio::time::sleep(std::time::Duration::from_millis(100)).await;',
    'tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let pending = bridge.pending_count();\n    eprintln!("DEBUG: pending calls count after sleep: {}", pending);'
)

with open('tests/bridge_resolve_auth_test.rs', 'w') as f:
    f.write(content)
print('Done')