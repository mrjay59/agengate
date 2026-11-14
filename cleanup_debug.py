with open('tests/bridge_resolve_auth_test.rs', 'r') as f:
    content = f.read()

# Remove debug code
content = content.replace(
    'eprintln!("DEBUG: hold_tool_call starting");\n',
    ''
)
content = content.replace(
    'eprintln!("DEBUG: hold_tool_call finished: {:?}", res.is_ok());\n',
    ''
)
content = content.replace(
    'eprintln!("DEBUG: after sleep, checking pending calls count: {}", registry.bridge.pending_calls.lock().await.len());\n',
    ''
)

with open('tests/bridge_resolve_auth_test.rs', 'w') as f:
    f.write(content)
print('Done')