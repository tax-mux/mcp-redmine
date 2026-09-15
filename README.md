# mcp-redmine

Redmine REST API を操作する MCP サーバ。**Docker コンテナで常駐**し、**SSE** で接続する。API キーはコンテナ内にのみ保持する（LLM / MCP クライアント設定には載せない）。

## 設計: シークレット隔離

| 置き場所 | API キー |
|----------|----------|
| ホストの `.env` / キーファイル（gitignore・Compose が読む） | ○ 格納（MCP クライアントには渡さない） |
| Docker コンテナ環境変数（Compose `env_file`） | ○ 実行時注入 |
| Cursor / OpenCode などの `mcp.json` | × URL のみ。`command` / `env` / キー禁止 |
| MCP ツール引数・レスポンス・エラー | × 拒否 / 除去 / マスク（`profile` 名のみ可） |

## 設計: エージェント身元（プロファイル）

- **通常、ツール引数 `profile` は不要・原則禁止**（ユーザーが明示許可した場合を除く）。身元はクライアント設定の `X-Redmine-Profile` のみ。
- 引数で `profile` を渡す／ヘッダ無し／ヘッダが `default` → エラー（default フォールバックなし）。
- MCP `initialize.instructions` にも同方針を載せる。

プロファイル名はキーストア（`.env` / `REDMINE_API_KEYS_FILE`）側で自由に定義する。権限の違いは Redmine 側のロールに依存する。接続後は `redmine_current_user` の `capabilities` で admin / プロジェクト一覧可否を確認する。

## セットアップ

```bash
cp .env.example .env
# .env: REDMINE_URL とキー（単一 or 複数プロファイル）
mkdir -p secrets
# secrets/redmine-keys.json を用意（.gitignore 済み）
# 任意: secrets/known-projects.json（examples/known-projects.json を参考）

docker compose up -d --build
curl -sS http://127.0.0.1:3100/health
```

Compose はホスト `3100` をコンテナ `8080` に公開する。LAN から使う場合はファイアウォールと到達性を確認する。

### 単一キー（互換）

```bash
REDMINE_URL=http://host.docker.internal:3000
REDMINE_API_KEY=your-key
```

`REDMINE_API_KEY` はプロファイル名 `default` として登録される。クライアントからは別プロファイル名を `X-Redmine-Profile` で指定する想定が一般的（ヘッダ `default` は拒否される）。

### 複数ユーザー / プロファイル

**A. JSON 環境変数 `REDMINE_API_KEYS`**

```bash
REDMINE_URL=http://host.docker.internal:3000
REDMINE_API_KEYS={"default":"...","alice":"...","bot":"..."}
```

**B. ファイル `REDMINE_API_KEYS_FILE`（推奨・権限を絞れる）**

```bash
REDMINE_API_KEYS_FILE=/secrets/redmine-keys.json
```

ファイル例（ホストで gitignore し、Compose でマウント）:

```json
{
  "profiles": {
    "default": "key-for-bootstrap-admin",
    "alice": "key-for-alice",
    "bot": "key-for-automation-bot"
  }
}
```

フラット形式 `{"default":"...","alice":"..."}` も可。

身元は **クライアントの `X-Redmine-Profile` ヘッダ**で固定する（例: `"alice"`）。ツール引数の `profile` は原則禁止（渡すとエラー）。ヘッダ無しや `default` もエラー。

プロファイル名の一覧は `redmine_list_profiles`（キーは返さない）。キーの追加・更新はオペレータが `.env` / ファイルを編集してコンテナを再起動する（LLM からキーを書かない）。

### known-project フォールバック（任意）

Reporter など `/projects.json` が空（または 403）になるプロファイル向けに、オペレータが既知プロジェクト一覧を渡せる。

```bash
REDMINE_KNOWN_PROJECTS_FILE=/secrets/known-projects.json
```

例（`examples/known-projects.json`）:

```json
{
  "profiles": ["bot"],
  "projects": [
    {"id": 1, "identifier": "example-project", "name": "Example Project"}
  ]
}
```

- 未設定・ファイル無し → フォールバックしない
- `profiles` に列挙した名前（大文字小文字無視）だけが対象
- 応答には `_fallback: true` が付く

## MCP クライアント設定

### URL のみ

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

クライアントは SSE エンドポイントにだけ接続する。docker コマンドも API キーも渡さない。

### URL + プロファイルヘッダ（推奨）

API キーは `mcp.json` に書かない。プロファイル名だけヘッダで指定する:

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

## エンドポイント

| メソッド | パス | 用途 |
|----------|------|------|
| GET | `/sse` | MCP SSE（`endpoint` イベントで message URL を通知） |
| POST | `/message?sessionId=...` | JSON-RPC 送信（応答は SSE の `message` イベント） |
| GET | `/health` | ヘルスチェック |

## ツール

| 名前 | 説明 |
|------|------|
| `redmine_list_profiles` | コンテナ内プロファイル名一覧（キーなし） |
| `redmine_provision_user` | login だけでユーザー作成。パスワード自動生成（返さない）。API キーをプロファイルに永続化 |
| `redmine_current_user` | 認証ユーザー + profile/capabilities（`api_key` 除去済み） |
| `redmine_issues` | `list` / `get` / `create` / `update`。フラット引数。添付対応 |
| `redmine_projects` | `list`（id/name/identifier + total_count）/ `get`。設定ファイル対象プロファイルは空一覧時に known-project フォールバック可 |
| `redmine_metadata` | trackers / issue_statuses / issue_priorities |
| `redmine_wiki` | wiki ページの list / get / create / update / delete |
| `redmine_api_request` | 任意 REST パス（パス検証・issue POST 自動ラップ）。relations: `POST /issues/{id}/relations.json` |

### done_ratio（進捗率）の順序

1. **先に** `done_ratio` を更新する（status は新規 / 進行中のまま）
2. **その後** 解決ステータスにする
3. 解決後に rate だけ変えると凍結されて効かないことが多い
4. 親の自動集計は子のクローズ状態に依存する（環境設定次第）

数値の `status_id` は環境ごとに異なる場合がある。作成・更新前に `redmine_metadata` で確認する。

### list と description

`redmine_issues` / `api_request` の **list** は本文を落とす。応答に `description_omitted: true` と `_hint` が付く。本文・journals は `action=get`。

### 添付（attachments）

`redmine_issues` に `attachment_paths` / `delete_attachment_ids` を渡すと、MCP サーバがファイルを読み、
`POST /uploads.json` → token → issue 連携（`issue.uploads`）で添付する。**ファイルは MCP サーバ上のローカルパス**を指定する。

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

- 削除は `action=update` でのみ有効。issue 更新後に `DELETE /attachments/{id}`（1 ID ずつ）として実行される
- 添付一覧・ダウンロードは `include=attachments`（`action=get` / `api_request`）を使う
- エラーは path 付き（ファイル未存在 / 読込失敗 / 4xx・5xx）で返る

### ユーザー自動登録

```text
redmine_provision_user { "login": "alice" }
```

- パスワードはコンテナ内で生成し、**応答に含めない**
- API キーは `REDMINE_API_KEYS_FILE` にプロファイルとして保存
- 応答は `profile` / `login` / `user_id` / `mail` のみ
- デフォルトプロファイルの API キーが **Redmine admin** であること

## テスト

```bash
cargo test
docker compose up -d --build
```

## バージョニング

- **正本**: `Cargo.toml` の `version`（SemVer）
- `/health` と MCP `initialize.serverInfo.version` はどちらも `CARGO_PKG_VERSION` を返す
- 変更履歴: [CHANGELOG.md](CHANGELOG.md)

リリース手順:

1. `CHANGELOG.md` に節を追加する
2. `Cargo.toml` の `version` を上げる
3. コミットする
4. `git tag -a vX.Y.Z -m "vX.Y.Z"` して push（`git push --tags`）

## License

MIT（`Cargo.toml` の `license` フィールド参照。リポジトリ直下の `LICENSE` ファイル追加は別途）。
