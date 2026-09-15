# mcp-redmine

Redmine REST API を操作する MCP サーバ。**Docker コンテナで常駐**し、**SSE** で接続する。API キーはコンテナ内にのみ保持する（LLM / MCP クライアント設定には載せない）。

## 設計: シークレット隔離

| 置き場所 | API キー |
|----------|----------|
| ホストの `.env` / キーファイル（gitignore・Compose が読む） | ○ 格納（MCP クライアントには渡さない） |
| Docker コンテナ環境変数（Compose `env_file`） | ○ 実行時注入 |
| Cursor / OpenCode の `mcp.json` | × URL のみ。`command` / `env` / キー禁止 |
| MCP ツール引数・レスポンス・エラー | × 拒否 / 除去 / マスク（`profile` 名のみ可） |

## 設計: エージェント身元（プロファイル）

- **通常、ツール引数 `profile` は不要・原則禁止**（ユーザーが明示許可した場合を除く）。身元はクライアント設定の `X-Redmine-Profile` のみ。
- 引数で `profile` を渡す／ヘッダ無し／ヘッダが `default` → エラー（default フォールバックなし）。
- MCP `initialize.instructions` にも同方針を載せる。

## セットアップ

```bash
cp .env.example .env
# .env: REDMINE_URL とキー（単一 or 複数プロファイル）

docker compose up -d --build
curl -sS http://127.0.0.1:3100/health
# LAN からも到達可（ホストが 0.0.0.0:3100 で公開）: curl -sS http://<LAN_IP>:3100/health
```

### 単一キー（互換）

```bash
REDMINE_URL=http://host.docker.internal:3000
REDMINE_API_KEY=your-key
```

`REDMINE_API_KEY` はプロファイル名 `default` として登録される。

### 複数ユーザー / プロファイル

**A. JSON 環境変数 `REDMINE_API_KEYS`**

```bash
REDMINE_URL=http://host.docker.internal:3000
REDMINE_API_KEYS={"default":"...","openclaw":"...","cursor":"..."}
REDMINE_PROFILE=default
```

**B. ファイル `REDMINE_API_KEYS_FILE`（推奨・権限を絞れる）**

```bash
REDMINE_API_KEYS_FILE=/secrets/redmine-keys.json
```

ファイル例（ホストで gitignore し、Compose でマウント）:

```json
{
  "profiles": {
    "default": "key-for-default-user",
    "openclaw": "key-for-openclaw-bot",
    "alice": "key-for-alice"
  }
}
```

フラット形式 `{"default":"...","alice":"..."}` も可。

身元は **クライアントの `X-Redmine-Profile` ヘッダ**で固定する（例: `"opencode"`）。ツール引数の `profile` は原則禁止（渡すとエラー）。ヘッダ無しや `default` もエラー。

プロファイル名の一覧は `redmine_list_profiles`（キーは返さない）。キーの追加・更新はオペレータが `.env` / ファイルを編集してコンテナを再起動する（LLM からキーを書かない）。

## Cursor 設定（URL のみ）

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

クライアントは SSE エンドポイントにだけ接続する。docker コマンドも API キーも渡さない。プロファイルを固定したい場合は `headers.X-Redmine-Profile` を付ける（上記「Cursor 設定」参照）。

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
| `redmine_issues` | `list` / `get` / `create` / `update`。フラット引数 |
| `redmine_projects` | `list`（id/name/identifier + total_count）/ `get`。openclaw 向け既知プロジェクト fallback |
| `redmine_metadata` | trackers / issue_statuses / issue_priorities |
| `redmine_api_request` | 任意 REST パス（パス検証・issue POST 自動ラップ）。relations: `POST /issues/{id}/relations.json` |

### プロファイル用途

| プロファイル名 | 想定用途 | 注意 |
|----------------|----------|------|
| `default` | ホスト既定（多くの場合 admin 相当） | コンテナ `REDMINE_PROFILE` の既定 |
| `admin` / `takahiro` | 管理者操作・ステータス変更 | 権限が必要な更新向き |
| `opencode` | OpenCode | admin フラグ付きのことが多い |
| `cursor` | Cursor エージェント | プロジェクト一覧は可。admin ではない |
| `pi` | pi-agent | Developer。`X-Redmine-Profile: pi` |
| `openclaw` | Reporter 系 bot | `/projects.json` が空になりやすい → `redmine_projects` の fallback を使う |

`redmine_current_user` の `capabilities` で admin / can_list_projects を確認する。

### done_ratio（進捗率）の順序

1. **先に** `done_ratio` を更新する（status は新規=1 / 進行中=2 のまま）
2. **その後** `status_id=3`（解決）にする
3. 解決後に rate だけ変えると凍結されて効かないことが多い
4. 親の自動集計は子のクローズ状態に依存する（環境設定次第）

### list と description

`redmine_issues` / `api_request` の **list** は本文を落とす。応答に `description_omitted: true` と `_hint` が付く。本文・journals は `action=get`。

### 添付（attachments）

`redmine_issues` に `attachment_paths` / `delete_attachment_ids` を渡すと、MCP サーバがファイルを読み、
`POST /uploads.json` → token → issue 連携（`issue.uploads`）で添付する。**ファイルは MCP サーバ上のローカルパス**を指定する。

```json
// create: スクリーンショットを添付して作成
{
  "action": "create",
  "project_id": "mcp-redmine",
  "tracker_id": 2,
  "subject": "example",
  "description": "body",
  "attachment_paths": ["/path/to/screenshot.png"]
}

// update: 添付を追加しつつ、既存添付 #12 を削除
{
  "action": "update",
  "issue_id": "42",
  "notes": "log",
  "attachment_paths": ["/path/to/logs.txt"],
  "delete_attachment_ids": [12]
}
```

- 削除は `action=update` でのみ有効。issue 更新後に `DELETE /attachments/{id}`（1 ID ずつ）として実行される
- 添付一覧・ダウンロードは既存の `include=attachments`（`action=get` / `api_request`）を使う
- エラーは path 付き（ファイル未存在 / 読込失敗 / 4xx・5xx）で返る

### ユーザー自動登録

```text
redmine_provision_user { "login": "cursor" }
```

- パスワードはコンテナ内で生成し、**応答に含めない**
- API キーは `REDMINE_API_KEYS_FILE` にプロファイルとして保存
- 応答は `profile` / `login` / `user_id` / `mail` のみ
- デフォルトプロファイルの API キーが **Redmine admin** であること

### Cursor 設定（URL + プロファイルヘッダ）

API キーは mcp.json に書かない。プロファイル名だけヘッダで指定する:

```json
{
  "mcpServers": {
    "mcp-redmine": {
      "url": "http://127.0.0.1:3100/sse",
      "headers": {
        "X-Redmine-Profile": "cursor"
      }
    }
  }
}
```

## テスト

```bash
cargo test
docker compose up -d --build
```

## チケット

Redmine プロジェクト `mcp-redmine`（#196 ほか）。
