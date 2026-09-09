use serde_json::{json, Value};

pub const TOOL_CURRENT_USER: &str = "redmine_current_user";
pub const TOOL_ISSUES: &str = "redmine_issues";
pub const TOOL_API_REQUEST: &str = "redmine_api_request";
pub const TOOL_LIST_PROFILES: &str = "redmine_list_profiles";
pub const TOOL_PROVISION_USER: &str = "redmine_provision_user";
pub const TOOL_WIKI: &str = "redmine_wiki";
pub const TOOL_PROJECTS: &str = "redmine_projects";
pub const TOOL_METADATA: &str = "redmine_metadata";

pub(crate) const PROFILE_PROP: &str = "profile";

fn profile_property() -> Value {
    json!({
        "type": "string",
        "description": "DO NOT PASS. Tool-arg profile is principally forbidden without explicit user permission; omit it. Identity comes only from the X-Redmine-Profile connection header. The default fallback is disabled."
    })
}


pub fn all_tool_definitions() -> Value {
    json!([
        {
            "name": TOOL_LIST_PROFILES,
            "description": "List Redmine profile names available in the container. Never returns tokens.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_PROVISION_USER,
            "description": "Create a Redmine user with an auto-generated password (not returned), store their REST token under a profile name, and persist to the mounted keys file. Requires an admin token in the default profile. Never pass passwords or tokens as arguments.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "login": { "type": "string", "description": "Redmine login (also default profile name)" },
                    "profile": { "type": "string", "description": "Profile name to store the API key under (default: login)" },
                    "firstname": { "type": "string" },
                    "lastname": { "type": "string" },
                    "mail": { "type": "string", "description": "Email (default: {login}@users.mcp-redmine.local)" }
                },
                "required": ["login"],
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_CURRENT_USER,
            "description": "Get the authenticated Redmine user via /users/current.json plus profile name and capability hints (admin flag, project listing). Secret fields are stripped.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile": profile_property()
                },
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_ISSUES,
            "description": "Manage Redmine issues: list, get, create, or update. list returns compact metadata (id/subject/status/project/updated_on) and sets description_omitted=true (body via get). create/update accept flat args or nested issue object/JSON string; MCP wraps {issue:{...}}. create/update always return ok ACK (ok/action/issue_id/http_status/changed) even when Redmine body is empty (204). For journals use notes on update (not description). done_ratio: set while status is New/In Progress, then resolve (status_id=3); after Resolved, done_ratio often freezes. Never pass credentials as arguments.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "get", "create", "update"],
                        "description": "list, get, create, or update"
                    },
                    "issue_id": {
                        "type": ["string", "integer"],
                        "description": "Required for get and update. Also accepts top-level id or query.id. Do not wrap the id in quotes."
                    },
                    "include": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional include values for get (e.g. journals, children)"
                    },
                    "query": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Supported list filters: status_id, tracker_id, project_id, subproject_id, assigned_to_id, author_id, category_id, priority_id, fixed_version_id, subject. Use numeric ids and their is_closed flag from the redmine_metadata issue_statuses call instead of guessing (e.g. not 'resolved'); for many specific issue ids, call action=get with issue_id=N several times -- 'issue_ids[]' is NOT a supported filter. Do NOT add any other keys; unknown keys are rejected with an error.",
                    },
                    "limit": { "type": "integer", "description": "Page size for list (default 25)" },
                    "project_id": { "type": "string", "description": "Project ID or identifier. Required for create. Also valid as a list filter inside query: {\"query\": {\"project_id\": 3}} to list only that project's issues." },
                    "tracker_id": { "type": "integer", "description": "Tracker ID (required for create)" },
                    "status_id": { "type": "integer", "description": "Status ID (required for create, optional for update). Prefer numeric ids from redmine_metadata. Set done_ratio before moving to Resolved (3)." },
                    "subject": { "type": "string", "description": "Issue subject (required for create)" },
                    "description": { "type": "string", "description": "Issue description body (required for create; overwrites body on update)" },
                    "notes": { "type": "string", "description": "Journal note for update (does not replace description)" },
                    "attachment_paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Local file paths (mcp-redmine server FS) to attach. create: attached on creation; update: attached on update. Values are paths, never credentials."
                    },
                    "delete_attachment_ids": {
                        "type": "array",
                        "items": { "type": "integer" },
                        "description": "Attachment IDs to delete (action=update only). Executed as DELETE /attachments/{id} after the issue update succeeds."
                    },
                    "done_ratio": { "type": "integer", "description": "Progress 0-100. Set while status is New/In Progress; after Resolved (3) Redmine may freeze it. Parent aggregation depends on child closed status." },
                    "profile": profile_property()
                },
                "required": ["action"],
                "additionalProperties": true
            }
        },
        {
            "name": TOOL_PROJECTS,
            "description": "List or get Redmine projects. list returns only id/name/identifier plus total_count (and paging). get returns full project detail. openclaw profile gets a known-project fallback when Redmine returns an empty list.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "get"], "description": "list or get" },
                    "project_id": { "type": "string", "description": "Project id or identifier (required for get)" },
                    "limit": { "type": "integer", "description": "Page size for list (default 100)" },
                    "profile": profile_property()
                },
                "required": ["action"],
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_METADATA,
            "description": "Fetch Redmine enumerations used when creating/updating issues: trackers, issue_statuses, issue_priorities.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["trackers", "issue_statuses", "issue_priorities", "all"],
                        "description": "Which metadata to fetch (default: all)"
                    },
                    "profile": profile_property()
                },
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_API_REQUEST,
            "description": "Call any Redmine REST path. Paths must be Redmine REST (e.g. /issues.json), not local files. POST /issues.json auto-wraps flat fields in {issue:{...}}. GET list endpoints omit description bodies (description_omitted). Journals: GET include=journals via redmine_issues get — not /issues/:id/journals.json. Relations example: POST /issues/{id}/relations.json body {relation:{issue_to_id:N,relation_type:\"precedes\"}}. Delete issue: DELETE /issues/{id}.json.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "method": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"], "description": "HTTP method (default GET if omitted)" },
                    "path": { "type": "string", "description": "REST path, e.g. /projects.json" },
                    "query": { "type": "object", "additionalProperties": { "type": "string" } },
                    "body": { "type": "object" },
                    "profile": profile_property()
                },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": TOOL_WIKI,
            "description": "Manage Redmine wiki pages: list, get, create, update, delete. Authentication uses an in-container profile key; never pass credentials as arguments.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Project identifier (required)" },
                    "title": { "type": "string", "description": "Wiki page title (optional for list, required otherwise)" },
                    "action": { "type": "string", "enum": ["list", "get", "create", "update", "delete"] },
                    "include_attachments": { "type": "boolean" },
                    "text": { "type": "string", "description": "Wiki page content for create/update" },
                    "comments": { "type": "string" },
                    "version": { "type": "integer" },
                    "profile": profile_property()
                },
                "required": ["project_id", "action"],
                "additionalProperties": false
            }
        }
    ])
}

