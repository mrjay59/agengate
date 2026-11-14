with open('tests/bridge_resolve_auth_test.rs', 'r') as f:
    content = f.read()

# Add debug before post_resolve
content = content.replace(
    'let pending = bridge_for_debug.pending_count().await;\n    eprintln!("DEBUG: pending calls count after sleep: {}", pending);',
    'let pending = bridge_for_debug.pending_count().await;\n    eprintln!("DEBUG: pending calls count after sleep: {}", pending);\n    let session_id_opt = bridge_for_debug.pending_session_id("call_auth").await;\n    eprintln!("DEBUG: pending_session_id for call_auth: {:?}", session_id_opt);'
)

with open('tests/bridge_resolve_auth_test.rs', 'w') as f:
    f.write(content)
print('Done')