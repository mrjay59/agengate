with open('tests/bridge_resolve_auth_test.rs', 'r') as f:
    content = f.read()

# Add debug after pending_session_id
content = content.replace(
    'let session_id_opt = bridge_for_debug.pending_session_id("call_auth").await;\n    eprintln!("DEBUG: pending_session_id for call_auth: {:?}", session_id_opt);',
    'let session_id_opt = bridge_for_debug.pending_session_id("call_auth").await;\n    eprintln!("DEBUG: pending_session_id for call_auth: {:?}", session_id_opt);\n    if let Some(ref sid) = session_id_opt {\n        let session = bridge_for_debug.get_session(sid).await;\n        eprintln!("DEBUG: get_session for {:?}: {:?}", sid, session.is_some());\n    }'
)

with open('tests/bridge_resolve_auth_test.rs', 'w') as f:
    f.write(content)
print('Done')