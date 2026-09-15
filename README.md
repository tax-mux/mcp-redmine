# mcp-redmine

[日本語版 / Japanese](README-jp.md)

MCP server for the Redmine REST API. It runs as a **long-lived Docker container** and speaks **SSE**. API keys stay inside the container (never in LLM / MCP client config).

## Design: secret isolation

| Location | API key |
|----------|---------|
| Host `.env` / key file (gitignored; Compose reads it) | Yes (never passed to MCP clients) |
| Docker container env (`Compose env_file`) | Yes (runtime injection) |
| Cursor / OpenCode `mcp.json` | No — URL only. No `command` / `env` / keys |
| MCP tool args / responses / errors | No — reject / strip / redact (`profile` name only) |

## Design: agent identity (profiles)

- **Normally omit tool-arg `profile` — principally forbidden** unless the user explicitly allows it. Identity comes only from the client `X-Redmine-Profile` header.
- Passing `profile` as a tool arg, missing header, or header `default` → error (no default fallback).
- The same policy is included in MCP `initialize.instructions`.

Profile names are defined in the key store (`.env` / `REDMINE_API_KEYS_FILE`). Permissions follow Redmine roles. After connect, check `redmine_current_user` `capabilities` for admin / project listing.

## Setup

```bash
cp .env.example .env
# .env: REDMINE_URL and keys (single or multi-profile)
mkdir -p secrets
# create secrets/redmine-keys.json (gitignored)
# optional: secrets/known-projects.json (see examples/known-projects.json)

docker compose up -d --build
curl -sS http://127.0.0.1:3100/health
```

Compose publishes host `3100` to container `8080`. For LAN access, check firewall and reachability.

### Single key (compat)

```bash
REDMINE_URL=http://host.docker.internal:3000
REDMINE_API_KEY=your-key
```

`REDMINE_API_KEY` is registered as profile `default`. Clients usually send a different name via `X-Redmine-Profile` (header `default` is rejected).

### Multiple users / profiles

**A. JSON env `REDMINE_API_KEYS`**

```bash
REDMINE_URL=http://host.docker.internal:3000
REDMINE_API_KEYS={"default":"...","alice":"...","bot":"..."}
```

**B. File `REDMINE_API_KEYS_FILE` (recommended)**

```bash
REDMINE_API_KEYS_FILE=/secrets/redmine-keys.json
```

Example file (gitignored on the host; mounted by Compose):

```json
{
  "profiles": {
    "default": "key-for-bootstrap-admin",
    "alice": "key-for-alice",
    "bot": "key-for-automation-bot"
  }
}
```

Flat form `{"default":"...","alice":"..."}` is also accepted.

Fix identity with the client **`X-Redmine-Profile` header** (e.g. `"alice"`). Tool-arg `profile` is principally forbidden. Missing header or `default` also errors.

List profile names with `redmine_list_profiles` (no keys returned). Operators add/update keys in `.env` / the file and restart the container (never write keys from the LLM).

### Known-project fallback (optional)

For profiles where `/projects.json` is empty (or 403), operators can supply a known project list:

```bash
REDMINE_KNOWN_PROJECTS_FILE=/secrets/known-projects.json
```

Example (`examples/known-projects.json`):

```json
{
  "profiles": ["bot"],
  "projects": [
    {"id": 1, "identifier": "example-project", "name": "Example Project"}
  ]
}
```

- Unset / missing file → no fallback
- Only names listed in `profiles` apply (case-insensitive)
- Responses include `_fallback: true`

## MCP client config

### URL only

`examples/mcp.json`:

```json
{
  "mcpServers": {
    "mcp-redmine": {
      "url": "http://127.0.0.1:3100/sse"
    }
  }
}
```

Connect to the SSE endpoint only. Do not pass docker commands or API keys.

### URL + profile header (recommended)

Do not put API keys in `mcp.json`. Send only the profile name in a header:

```json
{
  "mcpServers": {
    "mcp-redmine": {
      "url": "http://127.0.0.1:3100/sse",
      "headers": {
        "X-Redmine-Profile": "alice"
      }
    }
  }
}
```

## Endpoints

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/sse` | MCP SSE (`endpoint` event announces the message URL) |
| POST | `/message?sessionId=...` | JSON-RPC (responses on SSE `message` events) |
| GET | `/health` | Health check |

## Tools

| Name | Description |
|------|-------------|
| `redmine_list_profiles` | Profile names in the container (no keys) |
| `redmine_provision_user` | Create a user from `login` only. Password is auto-generated (not returned). Persist API key as a profile |
| `redmine_current_user` | Authenticated user + profile/capabilities (`api_key` stripped) |
| `redmine_issues` | `list` / `get` / `create` / `update` with flat args; attachments supported |
| `redmine_projects` | `list` (id/name/identifier + total_count) / `get`. Configured profiles may get known-project fallback on empty list |
| `redmine_metadata` | trackers / issue_statuses / issue_priorities |
| `redmine_wiki` | wiki page list / get / create / update / delete |
| `redmine_api_request` | Any REST path (path validation; issue POST auto-wrap). Relations: `POST /issues/{id}/relations.json` |

### `done_ratio` order

1. Update `done_ratio` **first** (status stays New / In Progress)
2. Then move to Resolved
3. Changing only the ratio after resolve often freezes and has no effect
4. Parent auto-aggregation depends on child closed state (instance settings)

Numeric `status_id` values can differ per instance. Confirm with `redmine_metadata` before create/update.

### list vs description

`list` for `redmine_issues` / `api_request` omits bodies. Responses set `description_omitted: true` and `_hint`. Use `action=get` for body / journals.

### Attachments

Pass `attachment_paths` / `delete_attachment_ids` to `redmine_issues`. The MCP server reads files, then `POST /uploads.json` → token → issue `uploads`. **Paths are local to the MCP server filesystem.**

```json
{
  "action": "create",
  "project_id": "my-project",
  "tracker_id": 2,
  "subject": "example",
  "description": "body",
  "attachment_paths": ["/path/to/screenshot.png"]
}
```

```json
{
  "action": "update",
  "issue_id": "42",
  "notes": "log",
  "attachment_paths": ["/path/to/logs.txt"],
  "delete_attachment_ids": [12]
}
```

- Deletes apply only on `action=update` (after the issue update: `DELETE /attachments/{id}` per id)
- List/download via `include=attachments` on `get` / `api_request`
- Errors include the path (missing file / read failure / 4xx·5xx)

### User provisioning

```text
redmine_provision_user { "login": "alice" }
```

- Password is generated in-container and **never returned**
- API key is stored in `REDMINE_API_KEYS_FILE` as a profile
- Response fields: `profile` / `login` / `user_id` / `mail` only
- The default profile key must be a **Redmine admin**

## Testing

```bash
cargo test
docker compose up -d --build
```

## Versioning

- **Source of truth**: `version` in `Cargo.toml` (SemVer)
- `/health` and MCP `initialize.serverInfo.version` both use `CARGO_PKG_VERSION`
- Changelog: [CHANGELOG.md](CHANGELOG.md)

Release steps:

1. Add a section to `CHANGELOG.md`
2. Bump `version` in `Cargo.toml`
3. Commit
4. `git tag -a vX.Y.Z -m "vX.Y.Z"` and push (`git push --tags`)

## License

MIT — see [LICENSE](LICENSE).
