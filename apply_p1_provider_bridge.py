with open('src/provider/bridge.rs', 'r') as f:
    content = f.read()

# Add Any supertrait to BridgeSession
old = '''/// Bridge session trait for managing CLI bridge processes
#[async_trait]
pub trait BridgeSession: Send + Sync {'''
new = '''/// Bridge session trait for managing CLI bridge processes
#[async_trait]
pub trait BridgeSession: Send + Sync + std::any::Any {'''
content = content.replace(old, new)

with open('src/provider/bridge.rs', 'w') as f:
    f.write(content)
print('provider/bridge.rs done')