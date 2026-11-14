with open('src/bridge.rs', 'r') as f:
    content = f.read()

# Fix ToolSchema initialization - use simple string replacement
old = 'ToolSchema { name: "Read".to_string(), description: None, parameters: serde_json::json!({}) }'
new = 'ToolSchema { name: "Read".to_string(), description: "".to_string(), parameters: serde_json::json!({}), provider: "test".to_string(), workspace: "test".to_string() }'

content = content.replace(old, new)

with open('src/bridge.rs', 'w') as f:
    f.write(content)
print('Done')