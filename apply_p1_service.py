with open('src/service.rs', 'r') as f:
    content = f.read()

# Fix resolve_by_openai_tool_call_id call
old = 'let _ = self.bridge.resolve_by_openai_tool_call_id(id, res).await;'
new = 'let _ = self.bridge.resolve_by_openai_tool_call_id(id).await;'
content = content.replace(old, new)

with open('src/service.rs', 'w') as f:
    f.write(content)
print('service.rs done')