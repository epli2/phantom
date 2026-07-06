# Phantom — 使い方ガイド

Phantom は **ゼロ計装の HTTP/HTTPS 観測ツール**です。アプリケーションのコードを変更せずにトラフィックをキャプチャし、インタラクティブな TUI またはスクリプト向けの JSON Lines ストリームで表示・保存します。

---

## 目次

1. [概要](#概要)
2. [ビルド](#ビルド)
3. [クイックスタート](#クイックスタート)
4. [キャプチャモード](#キャプチャモード)
   - [プロキシモード（推奨）](#プロキシモード推奨)
   - [LD_PRELOAD モード（Linux 限定）](#ld_preload-モードlinux-限定)
5. [出力モード](#出力モード)
   - [TUI モード（デフォルト）](#tui-モードデフォルト)
   - [JSONL モード](#jsonl-モード)
6. [TUI 操作キー](#tui-操作キー)
7. [CLI リファレンス](#cli-リファレンス)
8. [Docker を使ったテスト](#docker-を使ったテスト)
9. [データ保存先](#データ保存先)
10. [ロードマップ](#ロードマップ)

---

## 概要

| 機能 | 詳細 |
|------|------|
| キャプチャ方式 | MITM プロキシ（透過的インジェクション対応） または LD_PRELOAD |
| 対応プロトコル | HTTP / HTTPS（プロキシ）、HTTP のみ（LD_PRELOAD） |
| 対応 OS | プロキシ: macOS / Linux / Windows、LD_PRELOAD: Linux のみ |
| 表示形式 | インタラクティブ TUI または JSON Lines (stdout) |
| データ永続化 | Fjall KV ストア（LSMツリーによる高速保存） |

---

## ビルド

**前提条件**: 
- Rust 1.75 以降（stable）
- **(Java 連携用)**: JDK 11 以降

```bash
# リポジトリを取得
git clone <repo-url>
cd phantom

# 本体 (Rust) のビルド
cargo build --release

# Java Agent のビルド (Java アプリを追跡する場合に必要)
# ※ 詳細は crates/phantom-java-agent 参照
cd crates/phantom-java-agent
javac -d out src/com/example/phantom/Agent.java
echo "Premain-Class: com.example.phantom.Agent" > manifest.txt
jar cvfm phantom-java-agent.jar manifest.txt -C out .
cd ../..
```

---

## クイックスタート

**30 秒で体験:**

### 1. Node.js アプリをトレース
```bash
phantom -- node app.js
```

### 2. Java アプリをトレース (HTTP/HTTPS 両対応)
Phantom は自動的に Java Agent を注入し、プロキシ設定と SSL 検証の無効化（MITM 対応）を強制します。
```bash
phantom -- java -jar my-app.jar
```

### 3. 一般的なコマンドをトレース (HTTP のみ)
```bash
phantom -- curl http://httpbin.org/get
```

### 3. JSONL モードでストリーム処理
```bash
phantom --output jsonl -- node app.js | jq 'select(.status_code >= 400)'
```

---

## キャプチャモード

### プロキシモード（推奨）

MITM（中間者）プロキシとして動作します。クロスプラットフォーム対応で、HTTPS もキャプチャ可能です。

#### Node.js の自動連携
`phantom -- node app.js` のように実行すると、Phantom は `--require` 引数を用いて透過的にプロキシ設定を注入します。これにより、**axios, undici, fetch() などを用いた HTTPS 通信もコード変更なしでキャプチャ可能**です。

#### Java の自動連携
`phantom -- java ...` のように実行すると、Phantom は環境変数 `JAVA_TOOL_OPTIONS` を介して **Phantom Java Agent** を注入します。

- **SSL 検証の自動回避**: Phantom が生成する自己署名証明書を自動的に信頼させるため、`SSLHandshakeException` を回避できます。
- **プロキシの強制適用**: アプリ側でプロキシ設定が書かれていなくても、通信を強制的に Phantom へ誘導します。
- **対応ライブラリ**: JDK 標準の `HttpClient`、`Apache HttpClient`、`OkHttp` など。
  - ※ `Netty` や `Jetty` など独自のネットワークスタックを持つライブラリは、ライブラリ側の設定で「システムプロキシを使用する」オプションを有効にしてください。

#### その他のアプリケーション
`HTTP_PROXY` / `HTTPS_PROXY` に加えて、CA 信頼用の環境変数（`CURL_CA_BUNDLE`, `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS`, `DENO_CERT`）を自動設定します。これにより **curl や Python (requests) などの HTTPS 通信も、TLS 検証を無効化せずに**キャプチャできます。

```bash
# HTTPS も検証エラーなしでキャプチャされる
phantom --port 9090 -- curl https://example.com
```

継承された `NO_PROXY` / `ALL_PROXY` / `npm_config_*` 系のプロキシ変数は、キャプチャ漏れの原因になるため対象プロセスからクリアされます（クリア時は起動ログに表示）。

#### CA 証明書と HTTPS

HTTPS の復号に使う CA はデータディレクトリ配下（`<data-dir>/ca/`）に**永続化**され、再起動しても同じ証明書が使われます。Phantom が起動した子プロセスは自動的にこの CA を信頼するため、通常は何もする必要はありません。

ブラウザなど Phantom の外で起動するアプリから信頼する場合:

```bash
phantom cert export   # phantom-ca.cert.pem を書き出し、OS 別の信頼登録手順を表示
phantom cert path     # PEM ファイルのパスを表示（スクリプト用）
phantom cert print    # PEM 本文を stdout へ
```

CA 秘密鍵はパーミッション 0600 で保存され、`ca/` ディレクトリには `.gitignore` が自動生成されるためリポジトリに誤コミットされません。

### LD_PRELOAD モード（Linux 限定）

アプリケーションのシステムコールを直接フックします。プロキシ設定を無視するツールや、コンテナ内での利用に適していますが、**平文 HTTP のみ**対応です。

```bash
# エージェントをビルド
cargo build -p phantom-agent

# トレース実行
phantom --backend ldpreload \
        --agent-lib ./target/debug/libphantom_agent.so \
        -- curl http://example.com
```

---

## 出力モード

### TUI モード（デフォルト）

インタラクティブな 2 ペインビューアーです。`Tab` キーでリストと詳細表示を切り替えます。

### JSONL モード

1 トレースを 1 行の JSON として出力します。詳細なフィールド一覧・互換性ポリシーは
[`docs/jsonl-schema.md`](jsonl-schema.md) を参照してください（`schema_version` は
加算のみで進化し、既存フィールドの削除・改名・型変更は行いません）。

**スキーマ要約 (schema_version 2):**
- `trace_id` / `span_id`: W3C 互換 ID
- `timestamp_ms`: 開始時刻 (Unix Epoch)
- `duration_ms`: レイテンシ
- `method` / `url` / `status_code`: 基本情報
- `request_headers` / `response_headers`: ヘッダーマップ
- `request_body` / `response_body`: ボディ (`*_body_encoding` が `utf-8`/`base64` を示す。`*_body_truncated`、`*_content_encoding` も参照)

---

## TUI 操作キー

| キー | 動作 |
|------|------|
| `j` / `k` / `↓` / `↑` | 上下移動（詳細ペインではスクロール） |
| `g` / `Home`, `G` / `End` | 先頭 / 末尾へジャンプ |
| `Tab` | トレースリスト ↔ 詳細ペイン切り替え |
| `[` / `]` | 詳細ペインのタブ切り替え（Request / Response / Headers / Timing） |
| `/` | フィルタモード開始 |
| `Esc` | フィルタ解除 / 戻る |
| `c` | 選択中のトレースを `curl` コマンドとしてクリップボードにコピー |
| `w` | 選択中のトレースを `phantom-trace-<span_id>.json` に書き出し |
| `?` | ヘルプオーバーレイの表示/非表示 |
| `q` / `Ctrl+C` | 終了 |

### フィルタ構文 (`/`)

スペース区切りの単語は AND 条件になります。プレフィックスなしの単語は URL 部分一致です。

| 構文 | 意味 | 例 |
|------|------|-----|
| `status:<code>` | ステータスコード完全一致 | `status:404` |
| `status:<N>xx` | ステータスクラス一致 | `status:5xx` |
| `status:>=N` / `<=N` / `>N` / `<N` | ステータスコード比較 | `status:>=500` |
| `method:<name>` | HTTP メソッド一致（大小文字区別なし） | `method:post` |
| `host:<substr>` | URL ホスト部分一致 | `host:api.example.com` |
| `path:<substr>` | URL パス部分一致 | `path:/users` |

例: `status:5xx method:post` は「POST かつ 5xx」のトレースのみ表示します。

---

## CLI リファレンス

```text
USAGE:
    phantom [OPTIONS] [-- <COMMAND>]      # キャプチャ (phantom run と同義)
    phantom run [OPTIONS] [-- <COMMAND>]  # キャプチャ (明示形)
    phantom cert <path|print|export>      # HTTPS 傍受用 CA の管理

OPTIONS (run):
    -b, --backend <BACKEND>       [proxy, ldpreload] (デフォルト: proxy)
    -o, --output <MODE>           [tui, jsonl] (デフォルト: tui)
    -p, --port <PORT>             プロキシポート (デフォルト: 8080)
    --insecure                    バックエンド接続時の TLS 検証を無効化
    -d, --data-dir <DIR>          データ保存先
    --max-body <SIZE>             ボディ保存の上限 ("512kb"/"1mb"/"2gb"、"0"で無制限。デフォルト "1mb")
    --redact                      既定の機微ヘッダ・JSON フィールドを [REDACTED] に置換
    --redact-header <NAME>        追加でリダクションするヘッダ名 (繰り返し指定可)
    --redact-body-field <KEY>     追加でリダクションする JSON ボディのキー名 (繰り返し指定可)
    --fault <SPEC>                フォールト注入 (繰り返し指定可)
    --agent-lib <PATH>            libphantom_agent.so のパス (ldpreload 用)
    -- <COMMAND>                  実行・追跡するコマンド
```

サブコマンドを省略した従来形式 (`phantom -- node app.js`) は `phantom run` と完全に同じ動作です。

### 機微情報のリダクション

`--redact` を付けると、既定の機微ヘッダ(`authorization`, `proxy-authorization`, `cookie`, `set-cookie`, `x-api-key`)と JSON ボディの既定フィールド(`password`, `token`, `access_token`, `refresh_token`, `client_secret`, `api_key`)が `[REDACTED]` に置換されます。デフォルトは **off**(ローカルデバッグでは生値を確認したいため)。**トレースを共有・コミットする場合は必ず `--redact` を使ってください。**

```bash
phantom --redact -- node app.js
phantom --redact-header x-internal-token -- node app.js   # 個別追加のみも可能
```

---

## Docker を使ったテスト

Makefile を使用して Linux 環境 (LD_PRELOAD 等) をテストできます。

```bash
make docker-build             # イメージ作成
make docker-test-integration  # 統合テスト実行
```

---

## データ保存先

デフォルトでは以下のパスに Fjall (LSM-tree) 形式で保存されます。

- **Linux**: `~/.local/share/phantom/data`
- **macOS**: `~/Library/Application Support/phantom/data`
- **Windows**: `%APPDATA%\phantom\data`

HTTPS 傍受用の CA 証明書・秘密鍵は `<data-dir>/ca/` に保存されます。

---

## ロードマップ

Phantom は「ローカルファーストの API 開発ツールボックス」(観る / 乱す / 写す / 書き起こす) へ段階的に進化します。HAR エクスポート、リクエストリプレイ、記録からのモックサーバー生成、WebSocket/SSE キャプチャ、OpenAPI 自動生成などの詳細な計画は [ROADMAP.md](../ROADMAP.md) を参照してください。
