# Admin API Documentation

## Authentication (admin scope)

All `/admin/*` endpoints (including the deprecated aliases) require an
admin-scoped key. **Default is fail-closed in production**: with no admin key
configured every admin request is rejected (HTTP 403). Configure
`AGENTGATE_ADMIN_API_KEYS` (comma separated) or `AGENTGATE_ADMIN_API_KEY`, then
send `Authorization: Bearer <key>` (or `X-AgentGate-Admin-Key`). Use a separate
key from the data-plane `server.api_keys` so an ordinary chat client cannot
reach the management plane. `AGENTGATE_ADMIN_ALLOW_NO_AUTH=1` disables the
check and must **never** be used outside a local, loopback-only development
session; it logs `admin.auth.dev_open` when active. All responses redact
`api_key` values and proxy credentials.

Gateway state is persisted at `AGENTGATE_GATEWAY_STATE_PATH` (default
`<config dir>/agentgate/gateway.json`).

## Gateway management (new)

| Method | Path | Purpose |
|---|---|---|
| GET/POST | `/admin/providers` | List / add or update a provider |
| GET/PATCH/DELETE | `/admin/providers/:id` | Read / patch / disable (use `?hard=true` to delete) |
| POST | `/admin/providers/:id/test` | Smoke test (`/models` for HTTP, availability for CLI) |
| GET/POST | `/admin/models` | List / add model mapping |
| GET/PATCH/DELETE | `/admin/models/:id` | Manage a model mapping |
| POST | `/admin/models/sync?provider_id=...` | Pull upstream `/models` catalog |
| GET/POST | `/admin/combos` | List / add virtual combo model |
| GET/PATCH/DELETE | `/admin/combos/:id` | Read / patch / delete a combo model |

The persisted catalog is authoritative; combo writes are mirrored to the
runtime combo registry and the router also reads the catalog directly, so a
combo created here is immediately executable. Validation includes cycle
prevention, `max_attempts <= candidates` and known fallback conditions
(`quota`, `rate_limit`, `timeout`, `5xx`, `unavailable`, `empty_response`).
| GET/POST | `/admin/proxies` | List / add single or pool proxy config |
| GET/PATCH/DELETE | `/admin/proxies/:id` | Manage a proxy config |
| POST | `/admin/proxies/:id/test` | Test endpoints and report health |
| GET | `/admin/pools` | List load-balancer pools |
| PUT | `/admin/pools/:name` | Configure pool strategy/sticky/members/fallback |
| GET | `/admin/status` | LB/proxy health snapshot |

Proxy strategies: `round_robin`, `random`, `least_errors`, `sticky`.
Pool strategies: `round_robin`, `weighted_round_robin`, `least_latency`,
`least_errors`, `random`; sticky key: `none|session_id|api_key`. Candidates that
are quota-disabled or cooling down are filtered out; when the pool is empty the
request falls back to `opencode/9router/free-forever`.

Legacy `POST /admin/provider`, `POST /admin/key` and `POST /admin/proxy` remain
as aliases and return `Deprecation: true`.

## Add Provider (legacy shape)
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