# Admin API Documentation

## Add Provider
- **Method:** POST
- **Path:** /admin/providers/add
- **Request Body:**
  {
    "id": "custom-provider",
    "bin": "/path/to/cli",
    "models_cmd": ["models"],
    "chat_cmd": ["run", "--model", "{model}", "--prompt-stdin"],
    "prompt_via": "stdin",
    "output": "text",
    "jsonl_text_path": "/delta/text"
  }
- **Response:** {"status":"success","message":"Provider added successfully"}

## Add Key
- **Method:** POST
- **Path:** /admin/keys/add
- **Request Body:**
  {
    "key": "your-api-key",
    "description": "Optional description"
  }
- **Response:** {"status":"success","message":"API key added successfully"}

## Proxy
- **Method:** POST
- **Path:** /proxy
- **Request Body:**
  {
    "url": "https://example.com/api",
    "method": "GET",
    "headers": {"Authorization": "Bearer token"},
    "body": {"key": "value"}
  }
- **Response:** {"status":"success","data":{}}

## Add Skill
- **Method:** POST
- **Path:** /admin/skills/add
- **Request Body:**
  {
    "name": "skill-name",
    "description": "Optional description",
    "command": "skill-command"
  }
- **Response:** {"status":"success","message":"Skill added successfully"}

## Add MCP
- **Method:** POST
- **Path:** /admin/mcp/add
- **Request Body:**
  {
    "name": "mcp-name",
    "description": "Optional description",
    "config": {"key": "value"}
  }
- **Response:** {"status":"success","message":"MCP added successfully"}