with open('tests/bridge_resolve_auth_test.rs', 'r') as f:
    content = f.read()

content = content.replace(
    'let pending = bridge.pending_count();',
    'let pending = bridge.pending_count().await;'
)

with open('tests/bridge_resolve_auth_test.rs', 'w') as f:
    f.write(content)
print('Done')