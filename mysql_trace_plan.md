# MySQL クエリトレース機能 実装計画

## 概要

LD_PRELOAD バックエンド向けに MySQL COM_QUERY トレース機能を追加する。
`connect()` フックでポート3306の接続を検出し、MySQL wire protocol を解析して
SQLクエリとレスポンスをキャプチャする。TUIにMySQLタブを追加して表示する。

スコープ:
- **バックエンド**: LD_PRELOAD のみ（プロキシ不要）
- **コマンド**: COM_QUERY のみ（プリペアドステートメント除外）

---

## 1. データモデル設計

### 1.1 新規ファイル: `crates/phantom-core/src/mysql.rs`

```rust
use std::time::{Duration, SystemTime};
use serde::{Deserialize, Serialize};
use crate::{error::StorageError, trace::{SpanId, TraceId}};

/// MySQL クエリの実行結果（3種類のみ）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MysqlResponseKind {
    ResultSet { column_count: u64, row_count: u64 },
    Ok  { affected_rows: u64, last_insert_id: u64, warnings: u16 },
    Err { error_code: u16, sql_state: String, message: String },
}

/// MySQL 単一クエリのトレースレコード
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MysqlTrace {
    pub span_id: SpanId,
    pub trace_id: TraceId,
    pub parent_span_id: Option<SpanId>,
    pub query: String,
    pub response: MysqlResponseKind,
    pub timestamp: SystemTime,
    pub duration: Duration,
    pub dest_addr: Option<String>,
    pub db_name: Option<String>,
}

/// MySQL トレースストアトレイト（TraceStore と同じパターン）
pub trait MysqlStore: Send + Sync {
    fn insert(&self, trace: &MysqlTrace) -> Result<(), StorageError>;
    fn get_by_span_id(&self, span_id: &SpanId) -> Result<Option<MysqlTrace>, StorageError>;
    fn list_recent(&self, limit: usize, offset: usize) -> Result<Vec<MysqlTrace>, StorageError>;
    fn search_by_query(&self, pattern: &str, limit: usize) -> Result<Vec<MysqlTrace>, StorageError>;
    fn count(&self) -> Result<u64, StorageError>;
}
```

### 1.2 IPC メッセージ形式（エージェント側）

HTTP と MySQL を同一ソケットで多重化するために `msg_type` フィールドを使用:

```json
// HTTP（既存形式に msg_type を追加）
{"msg_type":"http","method":"GET","url":"...","status_code":200,...}

// MySQL（新規）
{
  "msg_type":"mysql",
  "query":"SELECT * FROM users",
  "duration_ms":4,
  "timestamp_ms":1234567890,
  "dest_addr":"127.0.0.1:3306",
  "db_name":"mydb",
  "affected_rows":null,
  "last_insert_id":null,
  "warnings":null,
  "column_count":3,
  "row_count":12,
  "error_code":null,
  "sql_state":null,
  "error_message":null
}
```

レスポンス種別判別ロジック:
- `error_code.is_some()` → `MysqlResponseKind::Err`
- `column_count.is_some()` → `MysqlResponseKind::ResultSet`
- それ以外 → `MysqlResponseKind::Ok`

---

## 2. 変更・新規作成ファイル一覧

| ファイル | 種別 | 変更内容 |
|---|---|---|
| `crates/phantom-core/src/mysql.rs` | **新規** | `MysqlTrace`, `MysqlResponseKind`, `MysqlStore` |
| `crates/phantom-core/src/lib.rs` | 変更 | `pub mod mysql;` を追加 |
| `crates/phantom-storage/src/fjall_mysql.rs` | **新規** | `FjallMysqlStore` 実装 |
| `crates/phantom-storage/src/lib.rs` | 変更 | `FjallMysqlStore` をエクスポート |
| `crates/phantom-agent/src/lib.rs` | 変更 | `connect()` フック、`MysqlConnState`、MySQL 解析 |
| `crates/phantom-capture/src/ldpreload.rs` | 変更 | `start_mysql_aware()` 追加、`msg_type` 分岐 |
| `crates/phantom-tui/src/app.rs` | 変更 | MySQL タブ状態追加 |
| `crates/phantom-tui/src/ui.rs` | 変更 | MySQL タブ描画追加 |
| `crates/phantom-tui/src/lib.rs` | 変更 | `run_tui()` シグネチャ拡張 |
| `src/main.rs` | 変更 | MySQL ストア・チャネル配線 |

---

## 3. 実装詳細

### Step 1: `phantom-core` — 型・トレイト定義

**`crates/phantom-core/src/mysql.rs`** を新規作成。
**`crates/phantom-core/src/lib.rs`** に `pub mod mysql;` を追加。

インラインテスト:
- `test_mysql_response_kind_serde` — 3バリアント全てのシリアライズ往復テスト
- `test_mysql_trace_serde_roundtrip` — `MysqlTrace` 全体のシリアライズ往復テスト

---

### Step 2: `phantom-storage` — Fjall ストア

**`crates/phantom-storage/src/fjall_mysql.rs`** を新規作成。

```rust
pub struct FjallMysqlStore {
    keyspace: Keyspace,
    mysql_traces: PartitionHandle,   // span_id (8B) → JSON
    mysql_by_time: PartitionHandle,  // timestamp_be (8B) ++ span_id (8B) → span_id (8B)
}
```

`FjallTraceStore` と同じパターン:
- `open(path)` で既存のキースペースに追加パーティションを開く
- `insert()` でバッチコミット（2パーティション同時書き込み）
- `list_recent()` は `mysql_by_time` を逆順スキャン → `mysql_traces` ルックアップ
- `search_by_query()` は `mysql_traces` 全件スキャンでSQL文字列マッチ

インラインテスト:
- `test_mysql_insert_and_get`
- `test_mysql_list_recent_ordering`
- `test_mysql_search_by_query`

---

### Step 3: `phantom-agent` — MySQL プロトコル解析

**`crates/phantom-agent/src/lib.rs`** に以下を追加:

#### 3.1 connect() フック

```rust
// ポート設定（環境変数、デフォルト3306）
static MYSQL_PORT: OnceLock<u16> = OnceLock::new();

fn mysql_port() -> u16 {
    *MYSQL_PORT.get_or_init(|| {
        std::env::var("PHANTOM_MYSQL_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3306)
    })
}

redhook::hook! {
    unsafe fn connect(
        sockfd: c_int,
        addr: *const libc::sockaddr,
        addrlen: libc::socklen_t,
    ) -> c_int => phantom_connect {
        // 再入ガード適用
        // 実際の connect() を呼び出し
        // AF_INET / AF_INET6 からポートを取得
        // ポート一致 → STATE_MAP に MysqlConnection を登録
    }
}
```

sockaddr パース（外部クレート不要、ポインタキャストで処理）:
```rust
fn extract_port(addr: *const libc::sockaddr, addrlen: libc::socklen_t) -> Option<u16> {
    unsafe {
        if addrlen as usize >= std::mem::size_of::<libc::sockaddr_in>() {
            let sa = &*(addr as *const libc::sockaddr_in);
            if sa.sin_family as i32 == libc::AF_INET {
                return Some(u16::from_be(sa.sin_port));
            }
        }
        if addrlen as usize >= std::mem::size_of::<libc::sockaddr_in6>() {
            let sa = &*(addr as *const libc::sockaddr_in6);
            if sa.sin6_family as i32 == libc::AF_INET6 {
                return Some(u16::from_be(sa.sin6_port));
            }
        }
        None
    }
}
```

#### 3.2 FdState 拡張

```rust
enum FdState {
    CollectingRequest { buf: Vec<u8> },
    CollectingResponse { ... },
    Http2(Box<H2ConnState>),
    MysqlConnection(Box<MysqlConnState>),  // ← 新規
}
```

#### 3.3 MysqlConnState ステートマシン

```rust
struct MysqlConnState {
    dest_addr: Option<String>,
    db_name: Option<String>,
    send_buf: Vec<u8>,  // クライアント → サーバー
    recv_buf: Vec<u8>,  // サーバー → クライアント
    // ハンドシェイク追跡
    handshake_phase: HandshakePhase,
    // クエリ追跡
    query_state: MysqlQueryState,
}

enum HandshakePhase {
    WaitingGreeting,       // サーバーの初期挨拶待ち
    WaitingAuthOk,         // 認証OK待ち
    Done,                  // ハンドシェイク完了
}

enum MysqlQueryState {
    Idle,
    AwaitingResponse { query: String, started_at: Instant, timestamp_ms: u64 },
    ReadingResultSet {
        query: String, started_at: Instant, timestamp_ms: u64,
        column_count: u64,   // 最初のパケットから取得
        row_count: u64,      // データパケットをカウント
        phase: ResultSetPhase,
    },
}

enum ResultSetPhase {
    ReadingColumns { cols_seen: u64 },
    ReadingRows,
}
```

#### 3.4 MySQL パケット解析

```rust
/// MySQL パケット: [3B length LE][1B seq_id][payload]
fn parse_mysql_packet(buf: &[u8]) -> Option<(usize, u8, &[u8])> {
    if buf.len() < 4 { return None; }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], 0]) as usize;
    if buf.len() < 4 + len { return None; }
    Some((4 + len, buf[3], &buf[4..4 + len]))
}

/// MySQL 可変長整数デコード
fn decode_lenenc_int(buf: &[u8]) -> Option<(u64, usize)> {
    match buf.first()? {
        n @ 0x00..=0xfb => Some((*n as u64, 1)),
        0xfc => {
            if buf.len() < 3 { return None; }
            Some((u16::from_le_bytes([buf[1], buf[2]]) as u64, 3))
        }
        0xfd => {
            if buf.len() < 4 { return None; }
            Some((u32::from_le_bytes([buf[1], buf[2], buf[3], 0]) as u64, 4))
        }
        0xfe => {
            if buf.len() < 9 { return None; }
            Some((u64::from_le_bytes(buf[1..9].try_into().ok()?), 9))
        }
        _ => None,
    }
}
```

処理フロー（`process_mysql_outgoing`）:
1. `send_buf` に追記
2. `parse_mysql_packet` で先頭パケットを取り出す
3. `HandshakePhase::Done` かつ `seq_id == 0` かつ `payload[0] == 0x03` → COM_QUERY 検出
4. query text = `String::from_utf8_lossy(&payload[1..])` で取得
5. `MysqlQueryState::AwaitingResponse` に遷移

処理フロー（`process_mysql_incoming`）:
1. `recv_buf` に追記
2. パケット取り出し
3. `HandshakePhase::WaitingGreeting`: seq_id=0, payload[0]=0x0a → `WaitingAuthOk` へ
4. `HandshakePhase::WaitingAuthOk`: payload[0]=0x00 かつ seq_id>=2 → `Done` へ
5. `MysqlQueryState::AwaitingResponse`:
   - `0x00` → OK パケット（affected_rows/last_insert_id 解析） → emit → `Idle`
   - `0xff` → ERR パケット（error_code/sql_state/message 解析） → emit → `Idle`
   - それ以外 → ResultSet 開始（column_count を lenenc_int で解析）→ `ReadingResultSet`
6. `ReadingResultSet`:
   - カラム定義フェーズ: `cols_seen < column_count` ならカラム定義パケットをスキップ
   - EOF パケット（`0xfe`, < 9 bytes）でカラム定義終了 → `ReadingRows` へ
   - 行フェーズ: `0xfe`（EOF）または `0x00`（CLIENT_DEPRECATE_EOF の OK）で終了 → emit

#### 3.5 MysqlTraceMsg と emit

```rust
#[derive(serde::Serialize)]
struct MysqlTraceMsg {
    msg_type: &'static str,    // = "mysql"
    query: String,
    duration_ms: u64,
    timestamp_ms: u64,
    dest_addr: Option<String>,
    db_name: Option<String>,
    // OK フィールド
    affected_rows: Option<u64>,
    last_insert_id: Option<u64>,
    warnings: Option<u16>,
    // ResultSet フィールド
    column_count: Option<u64>,
    row_count: Option<u64>,
    // ERR フィールド
    error_code: Option<u16>,
    sql_state: Option<String>,
    error_message: Option<String>,
}
```

同一の `emit_msg()` / Unix ソケットを使用（既存インフラを再利用）。

#### 3.6 既存 HTTP フック統合

`process_outgoing(key, data, tls)` を修正:
- `state_map().lock()` で状態を取得
- `FdState::MysqlConnection` の場合 → `process_mysql_outgoing()` に分岐

`process_incoming(key, data, tls)` / `process_teardown(key)` も同様。

インラインテスト（`#[cfg(test)]` モジュール）:
- `test_parse_mysql_packet_complete` / `test_parse_mysql_packet_incomplete`
- `test_decode_lenenc_int_all_forms`
- `test_mysql_handshake_detection`
- `test_mysql_state_ok_response`
- `test_mysql_state_err_response`
- `test_mysql_state_resultset`

---

### Step 4: `phantom-capture/ldpreload.rs` — IPC 多重化

#### msg_type による分岐

```rust
// 現行: serde_json::from_slice::<AgentTrace>(&buf[..n])
// 変更後:
let val: serde_json::Value = serde_json::from_slice(&buf[..n])?;
match val.get("msg_type").and_then(|v| v.as_str()) {
    Some("mysql") => { /* MySQL パース */ }
    _ => { /* HTTP パース（既存ロジック） */ }
}
```

既存の HTTP メッセージは `msg_type` フィールドがなくても `_` ブランチで処理される。

#### 新メソッド `start_mysql_aware()`

```rust
pub fn start_mysql_aware(
    &mut self,
) -> Result<(mpsc::Receiver<HttpTrace>, mpsc::Receiver<MysqlTrace>), CaptureError> {
    // ソケットバインド、チャネル生成
    // タスクスポーン: msg_type で HTTP/MySQL に分配
    Ok((http_rx, mysql_rx))
}
```

既存 `CaptureBackend::start()` は `start_mysql_aware()` を呼び出し、MySQL 受信側を drop:
```rust
fn start(&mut self) -> Result<mpsc::Receiver<HttpTrace>, CaptureError> {
    let (http_rx, _mysql_rx) = self.start_mysql_aware()?;
    Ok(http_rx)
}
```

これにより既存のプロキシバックエンドとのインターフェース互換性を維持する。

---

### Step 5: `phantom-tui` — MySQL タブ

#### `app.rs` 変更

```rust
#[derive(Debug, Default, PartialEq)]
pub enum ActiveTab { #[default] Http, Mysql }

pub struct App {
    // 既存フィールド...
    pub mysql_traces: Vec<MysqlTrace>,
    pub mysql_selected_index: usize,
    pub mysql_trace_count: u64,
    pub active_tab: ActiveTab,
}

impl App {
    pub fn add_mysql_trace(&mut self, trace: MysqlTrace) { ... }
    pub fn selected_mysql_trace(&self) -> Option<&MysqlTrace> { ... }
    pub fn filtered_mysql_traces(&self) -> Vec<&MysqlTrace> { ... }
    pub fn switch_tab(&mut self, tab: ActiveTab) { ... }
}
```

ナビゲーションキー（`j/k/↑/↓`）はアクティブタブの対応リストに作用。

#### `ui.rs` 変更

タブバー（メインエリア上部に追加）:
```
 [1] HTTP (42)    [2] MySQL (7)
```

MySQL リスト列:
```
Time     | Query (truncated 60 chars)              | Result          | Duration
12:34:56 | SELECT * FROM users WHERE id = 1        | 3 cols, 12 rows | 4ms
12:34:57 | INSERT INTO events (type) VALUES ('auth')| OK, 1 affected  | 2ms
12:34:58 | SELECT * FROM nonexistent_table          | ERR 1146        | 1ms
```

カラーコーディング:
- ResultSet → Green
- Ok → Cyan
- Err → Red

MySQL 詳細パネル（右ペイン）:
- クエリ全文（ラップ表示）
- レスポンス詳細（affected_rows / column_count+row_count / error message）
- タイムスタンプ、実行時間、接続先アドレス

#### `lib.rs` 変更

```rust
pub async fn run_tui(
    store: Arc<dyn TraceStore>,
    mysql_store: Arc<dyn MysqlStore>,
    trace_rx: mpsc::Receiver<HttpTrace>,
    mysql_rx: mpsc::Receiver<MysqlTrace>,
    backend_name: &str,
) -> std::io::Result<()>
```

起動時の既存トレース読み込み:
```rust
// HTTP（既存）
for trace in store.list_recent(1000, 0).unwrap_or_default() {
    app.add_trace(trace);
}
// MySQL（新規）
for trace in mysql_store.list_recent(1000, 0).unwrap_or_default() {
    app.add_mysql_trace(trace);
}
```

メインループでの `mysql_rx.try_recv()` 追記。

キーバインド追加:
- `1` → HTTP タブに切り替え
- `2` → MySQL タブに切り替え

---

### Step 6: `src/main.rs` — 配線

```rust
// ストア（既存 FjallTraceStore の隣に開く）
let mysql_store = Arc::new(FjallMysqlStore::open(&data_dir)?);

// バックエンド別チャネル
let (trace_rx, mysql_rx) = match cli.backend {
    Backend::Proxy => {
        let mut backend = ProxyCaptureBackend::new(cli.port);
        let trace_rx = backend.start()?;
        let (_tx, mysql_rx) = mpsc::channel::<MysqlTrace>(1);  // ダミー（送信なし）
        (trace_rx, mysql_rx)
    }
    Backend::Ldpreload => {
        let mut backend = /* 既存 */;
        backend.start_mysql_aware()?
    }
};

// TUI or JSONL
match cli.output {
    OutputMode::Tui => {
        phantom_tui::run_tui(store, mysql_store, trace_rx, mysql_rx, &backend.name()).await?
    }
    OutputMode::Jsonl => {
        // mysql_rx からも受信して stdout に JSONL 出力
        run_jsonl_output(store, mysql_store, trace_rx, mysql_rx, None).await?
    }
}
```

ステータスバーの更新:
```
phantom v0.1.0 | HTTP: 42 | MySQL: 7 | Capturing via ldpreload
```

---

## 4. 実装順序（依存関係順）

```
Step 1: phantom-core (mysql.rs)          ← すべての基盤
    ↓
Step 2: phantom-storage (fjall_mysql.rs) ← core に依存
    ↓
Step 3: phantom-agent (lib.rs)          ← 独立（依存なし）、2と並行可
    ↓
Step 4: phantom-capture (ldpreload.rs)  ← core + agent IPC 形式に依存
    ↓
Step 5: phantom-tui                     ← core に依存
    ↓
Step 6: src/main.rs                     ← 全ての依存先
```

---

## 5. テスト戦略

### ユニットテスト（インライン `#[cfg(test)]`）

| ファイル | テスト名 |
|---|---|
| `phantom-core/src/mysql.rs` | `test_mysql_response_kind_serde`, `test_mysql_trace_serde_roundtrip` |
| `phantom-storage/src/fjall_mysql.rs` | `test_mysql_insert_and_get`, `test_mysql_list_recent_ordering`, `test_mysql_search_by_query` |
| `phantom-agent/src/lib.rs` | `test_parse_mysql_packet_*`, `test_decode_lenenc_int_*`, `test_mysql_state_machine_*` |

### 統合テスト（Docker Compose）

`compose.yaml` に MySQL サービスを追加し、LD_PRELOAD 経由でクエリを送信して
トレースが正しくキャプチャされるかを検証。

---

## 6. 既存コードへの影響

| 変更点 | 影響 |
|---|---|
| `TraceMsg` に `msg_type` フィールドなし | `ldpreload.rs` で `_` マッチに fallback するため後方互換 |
| `run_tui()` シグネチャ変更 | `main.rs` のみ変更（public API は crate 内のみ） |
| `FdState` に `MysqlConnection` 追加 | `phantom-agent` はワークスペース外なので影響なし |
| 新パーティション（`mysql_*`） | 既存のキースペースに追加、既存パーティションと競合なし |

---

## 7. 環境変数

| 変数名 | 説明 | デフォルト |
|---|---|---|
| `PHANTOM_MYSQL_PORT` | MySQL 接続検出ポート | `3306` |
