# Phantom — 使い方ガイド

Phantom は **ゼロ計装の HTTP 観測ツール**です。アプリケーションのコードを変更せずに HTTP トラフィックをキャプチャし、インタラクティブな TUI またはスクリプト向けの JSON Lines ストリームで表示します。

---

## 目次

1. [概要](#概要)
2. [ビルド](#ビルド)
3. [クイックスタート](#クイックスタート)
4. [キャプチャモード](#キャプチャモード)
   - [プロキシモード](#プロキシモード)
   - [LD_PRELOAD モード（Linux 限定）](#ld_preload-モードlinux-限定)
5. [出力モード](#出力モード)
   - [TUI モード（デフォルト）](#tui-モードデフォルト)
   - [JSONL モード](#jsonl-モード)
6. [TUI 操作キー](#tui-操作キー)
7. [CLI リファレンス](#cli-リファレンス)
8. [Docker を使ったテスト](#docker-を使ったテスト)
9. [データ保存先](#データ保存先)

---

## 概要

| 機能 | 詳細 |
|------|------|
| キャプチャ方式 | MITM プロキシ または LD_PRELOAD |
| 対応プロトコル | HTTP / HTTPS（プロキシ）、HTTP のみ（LD_PRELOAD） |
| 対応 OS | プロキシ: macOS / Linux / Windows、LD_PRELOAD: Linux のみ |
| 表示形式 | インタラクティブ TUI または JSON Lines (stdout) |
| データ永続化 | Fjall KV ストア（直近 1000 件を TUI に表示） |

---

## ビルド

**前提条件**: Rust 1.75 以降（stable）

```bash
# リポジトリを取得
git clone <repo-url>
cd phantom

# リリースビルド
cargo build --release

# バイナリは target/release/phantom に生成されます
```

開発中は `cargo run --` でそのまま実行できます（以下の例では `cargo run --` を使用します）。

---

## クイックスタート

**macOS / Linux で 30 秒体験:**

```bash
# ターミナル 1: Phantom をプロキシモードで起動
cargo run

# ターミナル 2: プロキシ経由でリクエストを送信
curl -x http://127.0.0.1:8080 http://httpbin.org/get
```

TUI にリクエストが表示されます。`q` または `Ctrl+C` で終了。

---

## キャプチャモード

### プロキシモード

アプリケーションが **プロキシ設定をサポートしている**場合に使います。HTTPS もキャプチャ可能です。

```bash
# デフォルトポート 8080 で起動
cargo run -- --backend proxy

# ポートを変更する場合
cargo run -- --backend proxy --port 9090
```

プロキシ経由でリクエストを送る例:

```bash
# curl の場合
curl -x http://127.0.0.1:8080 https://api.example.com/users

# 環境変数でプロキシを設定する場合（多くのツールが対応）
export http_proxy=http://127.0.0.1:8080
export https_proxy=http://127.0.0.1:8080
curl https://api.example.com/users
```

### LD_PRELOAD モード（Linux 限定）

**Linux のみ**対応。アプリケーションの `send` / `recv` / `close` システムコールをフックしてキャプチャします。プロキシ設定が不要ですが、**平文 HTTP のみ**対応（HTTPS はソケット層で暗号化されているためキャプチャ不可）。

```bash
# Step 1: エージェント dylib をビルド
cargo build -p phantom-agent

# Step 2: LD_PRELOAD モードで対象コマンドを実行
cargo run -- \
  --backend ldpreload \
  --agent-lib ./target/debug/libphantom_agent.so \
  -- curl http://httpbin.org/get

# リリースビルドのエージェントを使う場合
cargo run --release -- \
  --backend ldpreload \
  --agent-lib ./target/release/libphantom_agent.so \
  -- your-app --arg1 --arg2
```

---

## 出力モード

### TUI モード（デフォルト）

インタラクティブな 2 ペインビューアーが起動します。

```bash
cargo run -- --output tui   # 明示指定（省略可）
cargo run                   # デフォルトでも TUI が起動
```

**レイアウト:**

```
┌─────────────────────────────────────────────────────────────────┐
│ 時刻        メソッド  URL                     ステータス  所要時間 │
│ 12:34:56    GET      http://api.example.com/…  200        42ms   │
│ 12:34:57    POST     http://api.example.com/…  201        93ms   │
├──────────────────────────┬──────────────────────────────────────┤
│  トレースリスト (45%)     │  詳細ビュー (55%)                    │
│                           │                                      │
│  選択中のトレース詳細      │  リクエストヘッダー、ボディ           │
│                           │  レスポンスヘッダー、ボディ           │
└──────────────────────────┴──────────────────────────────────────┘
```

### JSONL モード

1 トレース = 1 行の JSON を stdout に出力します。スクリプトや AI ワークフローとの連携に最適です。

```bash
cargo run -- --output jsonl

# jq で絞り込む例
cargo run -- --output jsonl | jq '{method, status_code, url, duration_ms}'

# LD_PRELOAD と組み合わせる場合
cargo run -- --backend ldpreload \
  --agent-lib ./target/debug/libphantom_agent.so \
  --output jsonl \
  -- curl http://httpbin.org/post -d '{"key":"value"}'
```

**出力フィールド:**

| フィールド | 型 | 説明 |
|-----------|-----|------|
| `timestamp_ms` | number | リクエスト開始時刻（Unix ミリ秒） |
| `duration_ms` | number | 往復所要時間（ミリ秒） |
| `method` | string | HTTP メソッド（GET, POST, …） |
| `url` | string | 完全な URL |
| `status_code` | number | HTTP ステータスコード |
| `protocol_version` | string | HTTP/1.1 など |
| `request_headers` | object | リクエストヘッダー（小文字キー） |
| `response_headers` | object | レスポンスヘッダー（小文字キー） |
| `request_body` | string | リクエストボディ（UTF-8） |
| `response_body` | string | レスポンスボディ（UTF-8） |
| `source_addr` | string? | 送信元アドレス |
| `dest_addr` | string? | 宛先アドレス |
| `trace_id` | string | W3C Trace ID |
| `span_id` | string | W3C Span ID |

---

## TUI 操作キー

### ナビゲーション

| キー | 動作 |
|------|------|
| `j` / ↓ | 下に移動 |
| `k` / ↑ | 上に移動 |
| `g` / Home | 先頭に移動 |
| `G` / End | 末尾に移動 |
| Tab | トレースリスト ↔ 詳細ペインを切り替え |

### フィルタ

| キー | 動作 |
|------|------|
| `/` | フィルタ入力モードを開始（URL を部分一致で絞り込み、大文字小文字を無視） |
| Backspace | フィルタ文字を 1 文字削除 |
| Enter | フィルタを確定してリストに戻る |
| Esc | フィルタをクリアして解除 |

### その他

| キー | 動作 |
|------|------|
| `q` | 終了 |
| Ctrl+C | 終了 |

---

## CLI リファレンス

```
USAGE:
    phantom [OPTIONS] [-- CMD...]

OPTIONS:
    -b, --backend <BACKEND>     キャプチャバックエンド
                                  proxy     MITM プロキシ（デフォルト）
                                  ldpreload LD_PRELOAD フック（Linux のみ）

    -o, --output <MODE>         出力モード
                                  tui   インタラクティブ TUI（デフォルト）
                                  jsonl JSON Lines を stdout に出力

    -p, --port <PORT>           プロキシのリッスンポート（デフォルト: 8080）

    -d, --data-dir <PATH>       トレースの保存ディレクトリ
                                （デフォルト: ~/.local/share/phantom/data）

        --agent-lib <PATH>      libphantom_agent.so のパス
                                （ldpreload モード時に必須）

    [-- CMD...]                 トレース対象コマンドと引数
                                （ldpreload モード時に指定）

    -h, --help                  ヘルプを表示
    -V, --version               バージョンを表示
```

**使用例:**

```bash
# プロキシをポート 9090 で起動
phantom --port 9090

# LD_PRELOAD で Python スクリプトをトレース
phantom --backend ldpreload \
  --agent-lib /path/to/libphantom_agent.so \
  -- python3 my_script.py

# JSONL でキャプチャしてファイルに保存
phantom --output jsonl > traces.jsonl
```

---

## Docker を使ったテスト

LD_PRELOAD は Linux 限定のため、macOS や Windows でも Makefile のターゲットで Docker 上テストできます。

```bash
# Docker イメージをビルド（初回のみ、3〜5 分かかる場合があります）
make docker-build

# LD_PRELOAD + TUI でテスト（curl のトレースが表示される）
make docker-test-ldpreload

# プロキシ + TUI でテスト（ポート 8080 を使用）
make docker-test-proxy

# LD_PRELOAD + JSONL でテスト（stdout に JSON が流れる）
make docker-test-jsonl

# HTTP/HTTPS の統合テストスイートを実行
make docker-test-integration

# Docker コンテナに入って自由に操作
make docker-shell

# Docker イメージとコンテナを削除
make docker-clean
```

その他の Makefile ターゲット:

```bash
make build          # ワークスペース全体をビルド (debug)
make release        # 最適化ビルド (release)
make release-linux  # Linux x86_64 向けクロスコンパイル (cargo-zigbuild 必須)
make release-linux-aarch64 # Linux aarch64 向けクロスコンパイル
make clean          # ビルド成果物を消去
make test           # Rust テストを実行
make fmt            # フォーマットチェック
make fmt-fix        # フォーマット自動修正
make clippy         # Clippy リント (警告はエラー)
make clippy-linux   # Linux 向け Clippy リント
make check          # fmt, clippy, clippy-linux, build, test をまとめて実行
```

---

## データ保存先

トレースは Fjall KV ストアに永続化されます。次回起動時も過去のトレースを参照できます（TUI 起動時は直近 1000 件を読み込み）。

**デフォルトパス:**

| OS | パス |
|----|------|
| Linux | `~/.local/share/phantom/data` |
| macOS | `~/Library/Application Support/phantom/data` |
| Windows | `%APPDATA%\phantom\data` |

**カスタマイズ:**

```bash
# 任意のディレクトリを指定
phantom --data-dir /tmp/phantom-traces

# プロジェクトごとにトレースを分けたい場合
phantom --data-dir ./traces/my-project
```
