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

**前提条件**: Rust 1.75 以降（stable）

```bash
# リポジトリを取得
git clone <repo-url>
cd phantom

# ビルド
cargo build --release

# バイナリは target/release/phantom に生成されます。
# パスを通すか、以下の例では `phantom` コマンドとして説明します。
```

---

## クイックスタート

**30 秒で体験:**

### 1. Node.js アプリをトレース (HTTP/HTTPS 両対応)
Node.js の場合、Phantom は自動的に `proxy-preload.js` を注入するため、アプリ側でプロキシ設定を意識する必要はありません。

```bash
phantom -- node app.js
```

### 2. 一般的なコマンドをトレース (HTTP のみ)
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

#### その他のアプリケーション
環境変数 `HTTP_PROXY` を自動設定します。
```bash
# 明示的にポートを指定して起動
phantom --port 9090 -- curl http://example.com
```

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
    --insecure               バックエンド接続時の TLS 検証を無効化
    -d, --data-dir <DIR>     データ保存先
    --agent-lib <PATH>       libphantom_agent.so のパス (ldpreload 用)
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
