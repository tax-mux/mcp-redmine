# mcp-redmine

Redmine REST API を操作する MCP サーバ。**Docker コンテナで常駐**し、**SSE** で接続する。API キーはコンテナ内にのみ保持する（LLM / MCP クライアント設定には載せない）。

## 設計: シークレット隔離

| 置き場所 | API キー |
|----------|----------|
| ホストの `.env` / キーファイル（gitignore・Compose が読む） | ○ 格納（MCP クライアントには渡さない） |
| Docker コンテナ環境変数（Compose `env_file`） | ○ 実行時注入 |
| Cursor / OpenCode の `mcp.json` | × URL のみ。`command` / `env` / キー禁止 |
| MCP ツール引数・レスポンス・エラー | × 拒否 / 除去 / マスク（`profile` 名のみ可） |

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

ツール呼び出し時に任意引数 `profile`（例: `"openclaw"`）を渡す。省略時は `REDMINE_PROFILE` または `default`。

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
| `redmine_api_request` | 任意 REST パス（パス検証・issue POST 自動ラップ） |

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
