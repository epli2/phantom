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
| 対応プロトコル | HTTP / HTTPS（プロキシ、LD_PRELOAD 両方） |
| 対応 OS | プロキシ: macOS / Linux / Windows、LD_PRELOAD: Linux のみ |
| 表示形式 | インタラクティブ TUI または JSON Lines (stdout) |
| データ永続化 | Fjall KV ストア（LSMツリーによる高速保存） |

---

## ビルド

**前提条件**: 
- Rust 1.75 以降（stable）
- **(Java 連携用、任意)**: JDK 11 以降

```bash
# リポジトリを取得
git clone <repo-url>
cd phantom

# 本体 (Rust) のビルド
cargo build --release
```

`cargo build` 実行時に `build.rs` が `crates/phantom-java-agent/` の Java Agent (`Agent.java`) を `javac`/`jar` で自動ビルドし、`phantom-java-agent.jar` として埋め込みます。JDK が見つからない環境（JDKなしのCI/Dockerイメージなど）では、ビルド自体は空のプレースホルダjarで継続されますが、その場合 Java アプリのトレース機能（`-javaagent` 注入）だけが無効になります。手動でのビルド操作は不要です。

---

## クイックスタート

**30 秒で体験:**

### 1. Node.js アプリをトレース
```bash
phantom run -- node app.js
```

### 2. Java アプリをトレース (HTTP/HTTPS 両対応)
Phantom は `JAVA_TOOL_OPTIONS` 経由でプロキシ設定 (`-Dhttp(s).proxyHost/Port`) と Java Agent (`-javaagent:phantom-java-agent.jar`) を自動的に注入し、JVM 全体の SSL 検証を無効化（MITM 対応）します。
```bash
phantom run -- java -jar my-app.jar
```

### 3. 一般的なコマンドをトレース (HTTP のみ)
```bash
phantom run -- curl http://httpbin.org/get
```

### 4. JSONL モードでストリーム処理
```bash
phantom run --output jsonl -- node app.js | jq 'select(.status_code >= 400)'
```

---

## キャプチャモード

### プロキシモード（推奨）

MITM（中間者）プロキシとして動作します。クロスプラットフォーム対応で、HTTPS もキャプチャ可能です。

#### Node.js の自動連携
`phantom run -- node app.js` のように実行すると、Phantom は `--require` 引数を用いて透過的にプロキシ設定を注入します。これにより、**axios, undici, fetch() などを用いた HTTPS 通信もコード変更なしでキャプチャ可能**です。

#### Java の自動連携
`phantom run -- java ...` のように実行すると、Phantom は環境変数 `JAVA_TOOL_OPTIONS` を介して **Phantom Java Agent** を注入します。

- **SSL 検証の自動回避**: Phantom が生成する自己署名証明書を自動的に信頼させるため、`SSLHandshakeException` を回避できます。
- **プロキシの強制適用**: アプリ側でプロキシ設定が書かれていなくても、通信を強制的に Phantom へ誘導します。
- **対応ライブラリ**: JDK 標準の `HttpClient`、`Apache HttpClient`、`OkHttp` など。
  - ※ `Netty` や `Jetty` など独自のネットワークスタックを持つライブラリは、ライブラリ側の設定で「システムプロキシを使用する」オプションを有効にしてください。

#### PHP の自動連携（curl 拡張）
`phantom run -- php app.php` のように実行すると、Phantom が生成した MITM CA 証明書を一時 PEM ファイルへ書き出し、`-d curl.cainfo=<path>` として自動注入します。libcurl は `HTTP_PROXY`/`HTTPS_PROXY` を標準で読むため、コード注入なしで curl 拡張の HTTP/HTTPS 通信（Guzzle のデフォルトハンドラを含む）をキャプチャできます。

- 対象は **curl 拡張のみ**（`file_get_contents` などの PHP streams は対象外）。
- `curl.cainfo` の利用には **PHP 5.3.7 以降**が必要です。
- アプリ側が `CURLOPT_CAINFO`/`CURLOPT_SSL_VERIFYPEER` を明示的に設定している場合、Phantom の CA 注入が上書きされ HTTPS キャプチャが失敗することがあります。

#### その他のアプリケーション
環境変数 `HTTP_PROXY` を自動設定します。
```bash
# 明示的にポートを指定して起動
phantom run --port 9090 -- curl http://example.com
```

### LD_PRELOAD モード（Linux 限定）

アプリケーションの libc 呼び出し（`send`/`recv`）と OpenSSL 呼び出し（`SSL_write`/`SSL_read`）を直接フックします。プロキシ設定を無視するツールや、コンテナ内での利用に適しており、**HTTP・HTTPS の両方**に対応します（プロキシ証明書は関与しません）。動的リンクされたプロセスであれば言語を問わず動作します。

```bash
# エージェントをビルド
cargo build -p phantom-agent

# トレース実行
phantom run --backend ldpreload \
        --agent-lib ./target/debug/libphantom_agent.so \
        -- curl http://example.com
```

---

## 出力モード

### TUI モード（デフォルト）

インタラクティブな 2 ペインビューアーです。`Tab` キーでリストと詳細表示を切り替えます。

### JSONL モード

1 トレースを 1 行の JSON として出力します。

**スキーマ要約:**
- `trace_id` / `span_id`: W3C 互換 ID
- `timestamp_ms`: 開始時刻 (Unix Epoch)
- `duration_ms`: レイテンシ
- `method` / `url` / `status_code`: 基本情報
- `request_headers` / `response_headers`: ヘッダーマップ
- `request_body` / `response_body`: UTF-8 デコード済みボディ (optional)

---

## TUI 操作キー

| キー | 動作 |
|------|------|
| `j` / `k` | 上下移動 |
| `Tab` | トレースリスト ↔ 詳細ペイン切り替え |
| `/` | フィルタモード（URL 部分一致） |
| `Esc` | フィルタ解除 / 戻る |
| `q` / `Ctrl+C` | 終了 |

---

## CLI リファレンス

```text
OPTIONS:
    -b, --backend <BACKEND>  [proxy, ldpreload] (デフォルト: proxy)
    -o, --output <MODE>      [tui, jsonl] (デフォルト: tui)
    -p, --port <PORT>        プロキシポート (デフォルト: 8080)
    --bind <ADDR>            プロキシのバインドアドレス (デフォルト: 127.0.0.1)
                              0.0.0.0 で他ホスト/コンテナから到達可能に（認証なし、信頼できるネットワークのみ）
    --insecure               バックエンド接続時の TLS 検証を無効化
    -d, --data-dir <DIR>     データ保存先
    --agent-lib <PATH>       libphantom_agent.so のパス (ldpreload 用)
    --fault <SPEC>           フォルトインジェクション（繰り返し指定可、proxy バックエンドのみ）
                              例: delay:100ms, delay:100ms-500ms, error:503, error:500:0.1:/api
    --max-body <N>           JSONL 出力時のボディを N バイトに切り詰め (0 = 無制限)
    --headers-only           JSONL 出力からボディを省略
    -- <COMMAND>             実行・追跡するコマンド
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

---

## ロードマップ

- **bpftime (Userspace eBPF) 統合**: uprobe よりも 10 倍高速なゼロ計装キャプチャ。
- **ワークフロー自動推論**: キャプチャしたデータから **Arazzo Specification** を AI (`Candle`) で自動生成。
- **GUI アプリケーション**: `Tauri` を用いたクロスプラットフォームデスクトップアプリ。
