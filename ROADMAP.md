# Phantom 開発ロードマップ

> **ドキュメントの目的**: 本書は Phantom を「現代の Web 開発を支える実用ツール」へ進化させるための、コンセプト再定義と段階的開発計画である。**LLM エージェントが単独で読んで迷わず実装着手できる粒度**で記述している。各タスクには ID・依存関係・変更対象ファイル・実装手順・受け入れ基準・テスト方法を明記した。
>
> 最終更新: 2026-07-05 / 対象リビジョン: `main` = `6e19d28` (Merge PR #5: fault injection)

---

## 目次

- [1. 現状分析](#1-現状分析)
  - [1.1 実装済み機能](#11-実装済み機能)
  - [1.2 Open PR の評価と処置方針](#12-open-pr-の評価と処置方針)
  - [1.3 現状の課題(ギャップ分析)](#13-現状の課題ギャップ分析)
- [2. コンセプト再定義](#2-コンセプト再定義)
- [3. アーキテクチャ進化方針](#3-アーキテクチャ進化方針)
- [4. ロードマップ全体像](#4-ロードマップ全体像)
- [5. Phase 0: リポジトリ健全化と基盤整備 (v0.1.x)](#5-phase-0-リポジトリ健全化と基盤整備-v01x)
- [6. Phase 1: コアユーザビリティ — 「信頼できる HTTP デバッガ」 (v0.2)](#6-phase-1-コアユーザビリティ--信頼できる-http-デバッガ-v02)
- [7. Phase 2: Web 開発ワークフロー統合 (v0.3)](#7-phase-2-web-開発ワークフロー統合-v03)
- [8. Phase 3: 仕様生成とエコシステム統合 (v0.4)](#8-phase-3-仕様生成とエコシステム統合-v04)
- [9. Phase 4: 長期研究トラック (v1.0 以降)](#9-phase-4-長期研究トラック-v10-以降)
- [10. 横断的関心事](#10-横断的関心事)
- [11. LLM エージェント向け作業プロトコル](#11-llm-エージェント向け作業プロトコル)
- [付録 A: JSONL スキーマ v2 (案)](#付録-a-jsonl-スキーマ-v2-案)
- [付録 B: CLI 全体シノプシス (目標形)](#付録-b-cli-全体シノプシス-目標形)
- [付録 C: タスク一覧サマリ](#付録-c-タスク一覧サマリ)
- [付録 D: 意思決定記録 (ADR)](#付録-d-意思決定記録-adr)

---

## 1. 現状分析

### 1.1 実装済み機能

Phantom は Rust 製のゼロ計装 HTTP/HTTPS 可観測ツール。Cargo ワークスペース構成(5 ライブラリクレート + 1 バイナリ)。

| 領域 | 実装状況 | 主要ファイル |
|---|---|---|
| MITM プロキシキャプチャ (HTTP/HTTPS) | ✅ hudsucker 0.22 ベース。HTTP/2 対応済み (PR #1) | `crates/phantom-capture/src/proxy.rs` |
| LD_PRELOAD キャプチャ (Linux, 平文 HTTP のみ) | ✅ libc `send`/`recv`/`close` フック | `crates/phantom-agent/src/lib.rs`, `crates/phantom-capture/src/ldpreload.rs` |
| Node.js 透過注入 | ✅ `phantom -- node app.js` で `proxy-preload.js` を `--require` 注入。http/https/axios/undici/fetch 対応 | `src/main.rs`, `tests/apps/node-app/proxy-preload.js` |
| ストレージ (Fjall LSM-tree) | ✅ `traces` / `by_time` / `by_trace_id` の 3 パーティション | `crates/phantom-storage/src/fjall_store.rs` |
| TUI (Ratatui) | ✅ 2 ペイン(リスト+詳細)、URL 部分一致フィルタのみ | `crates/phantom-tui/` |
| JSONL 出力 | ✅ 1 トレース 1 行、子プロセス終了で自動 exit | `src/main.rs` (`run_jsonl_output`) |
| フォールトインジェクション | ✅ `--fault delay:100ms` / `--fault error:503:0.5:/api` (PR #5) | `crates/phantom-capture/src/fault.rs` |
| 統合テスト | ✅ Node.js (http/https/axios/undici/fetch) + fault injection | `tests/proxy_node_integration.rs`, `tests/fault_injection.rs` |
| CI | ✅ fmt / clippy / build / test | `.github/workflows/ci.yml` |
| ドキュメント | △ `AGENTS.md`(充実), `docs/how-to-use.ja.md`, `README.md` はバッジのみでほぼ空 | — |

### 1.2 Open PR の評価と処置方針

#### PR #4: Java 統合テスト (Spring Boot + 4 種の Java HTTP クライアント)

- **内容**: `HTTP_PROXY` 経由の非侵襲キャプチャを Java (JDK HttpClient / AsyncHttpClient / Jetty / Apache HttpClient 5) で検証する統合テスト 2 本と Maven テストアプリを追加。java/mvn がなければ自動スキップ。
- **評価**: ✅ **有益。マージすべき**。「Node.js 以外でも動く」ことの回帰保証はコンセプト(ポリグロット対応)の裏付けになる。プロダクトコードに変更はなくリスクが低い。
- **懸念**: (a) base が古い main (`0ff64ee`、fault injection マージ前) のため rebase が必要。(b) テストアプリが trust-all SSLContext を使っており、Phase 0 の CA 永続化 (P0-3) 完了後は `-Djavax.net.ssl.trustStore` 方式へ更新するフォローアップが望ましい。
- **処置**: → タスク **P0-1** で rebase & マージ。

#### PR #2: MySQL トレース機能

- **内容**: LD_PRELOAD エージェントで MySQL プロトコルを解析し、`phantom-core` に `mysql.rs`、storage に `fjall_mysql.rs`、TUI にも MySQL 表示を追加する大型変更(11 ファイル)。
- **評価**: ⚠️ **現時点ではマージ非推奨**。理由:
  1. **コンセプトの希釈**: 本ロードマップは Phantom を「Web 開発向け HTTP レイヤーのツール」として再定義する(§2)。DB プロトコルは価値があるが、HTTP のコア体験が未完成な段階で第 2 のプロトコルを抱えると、TUI・ストレージ・JSONL スキーマすべてが二重化し保守コストが跳ね上がる。
  2. **技術的負債**: base が 4 ヶ月前の main (`d77ef69`) で、`src/main.rs` / `app.rs` / `ui.rs` など競合必至のファイルを触っている。また `HttpTrace` と並列の独立型として実装されており、後述の「プロトコル非依存イベントモデル」(P4-4) と設計が衝突する。
  3. LD_PRELOAD 限定 (Linux のみ・TLS 不可) で、主力のプロキシバックエンドと非対称。
- **処置**: → タスク **P0-2**。PR に経緯をコメントして **クローズし、GitHub Issue「Protocol plugins: MySQL/PostgreSQL tracing」として再登録**する。設計自体(パケット解析ロジック)は Phase 4 の P4-4 で再利用する。**削除ではなく延期**である。

### 1.3 現状の課題(ギャップ分析)

コードを精査して確認した、実用上のギャップ。番号は後続タスクから参照される。

| # | ギャップ | 詳細・根拠 | 対応タスク |
|---|---|---|---|
| G1 | **CA 証明書が起動ごとに使い捨て** | `proxy.rs:119 generate_ca()` が毎回新規生成。ディスク保存もエクスポート手段もないため、Node 以外のクライアントは TLS 検証を無効化(trust-all / `--insecure`)しないと HTTPS を復号できない。**HTTPS MITM ツールとして最大の欠陥** | P0-3 |
| G2 | **`HTTPS_PROXY` 環境変数を設定していない** | `src/main.rs spawn_proxy_child()` は `HTTP_PROXY`/`http_proxy` のみ設定。curl/Python/Go など大半のクライアントは HTTPS リクエストに `HTTPS_PROXY` を参照するため、Node 以外では HTTPS が素通りする | P0-3 |
| G3 | **圧縮ボディを展開しない** | `proxy.rs` に Content-Encoding 処理が存在しない。gzip/br 応答は TUI・JSONL で文字化けバイナリになる。現実の API はほぼ全て圧縮転送するため致命的 | P1-1 |
| G4 | ボディ 1MB で暗黙に切り捨て、痕跡が残らない | `proxy.rs:20 MAX_BODY_SIZE`。切り捨てられたか判別不能 | P1-2 |
| G5 | TUI が最小限 | フィルタは URL 部分一致のみ。ステータス/メソッド絞り込み・ボディ検索・JSON 整形・コピー・スクロール位置表示・ヘルプがない | P1-4 |
| G6 | 蓄積データの活用手段がない | Fjall に永続化はするが、読み出しは TUI 起動時の `list_recent(1000)` のみ。エクスポート・検索・セッション区別が不可能 | P1-3, P2-1, P2-6 |
| G7 | ストリーミング応答 (SSE)・WebSocket が扱えない | ボディを全読みしてから記録する設計のため、SSE は接続が終わるまで何も見えず、WebSocket は不可視 | P2-5 |
| G8 | 機微情報がそのまま保存される | `Authorization`/`Cookie` ヘッダや token を含むボディが平文でディスクに残る | P1-5 |
| G9 | README がほぼ空 | 初見ユーザー/エージェントが価値を把握できない | P0-4 |
| G10 | 配布手段が `cargo build` のみ | バイナリリリース・Homebrew・`cargo install` 手順がない | P3-5 |
| G11 | JSONL スキーマにバージョン概念がない | フィールド追加時に下流(jq スクリプト、AI エージェント)が壊れる | P1-3 |

---

## 2. コンセプト再定義

### 2.1 これまでのビジョンの問題

`plan.md` は「bpftime (ユーザー空間 eBPF) + Fjall + Candle LLM + Arazzo 自動推論 + Ratatui/Tauri」という野心的な構想を描いている。技術選定の調査価値は高いが、ロードマップとしては次の問題がある。

1. **段階的に価値を出せない**: Arazzo 自動推論や bpftime 統合は、それ単体で数ヶ月級のプロジェクトであり、完成するまでユーザーに届く価値がない。
2. **現実装との乖離**: 現在の Phantom は「MITM プロキシ + TUI + JSONL」であり、plan.md の構想とは別物。乖離したビジョンは開発の意思決定を迷わせる。
3. **市場の空白と一致しない**: mitmproxy(汎用 MITM)、Charles/Proxyman(GUI デバッガ)、Keploy(record & replay テスト)は存在するが、**「ターミナル完結・ゼロ設定・AI エージェント親和の API 開発ツールボックス」** は空白である。Phantom の既存資産(JSONL 自動 exit、Node 透過注入、fault injection)はまさにこの方向を向いている。

### 2.2 新コンセプト

> **Phantom は、モダン Web 開発者と AI コーディングエージェントのための「ローカルファースト API 開発ツールボックス」である。**
> アプリのコードを 1 行も変えずに、開発中のアプリが行う HTTP 通信を **見る(observe)・試す(perturb)・写す(record/replay/mock)・書き起こす(spec-gen)** ことができる。

4 つの動詞がプロダクトの柱である:

| 柱 | 意味 | 対応機能 |
|---|---|---|
| **Observe** | 通信を透過キャプチャして人間(TUI)と機械(JSONL)の両方に見せる | 既存プロキシ/TUI/JSONL + Phase 1 の品質向上 |
| **Perturb** | 遅延・エラー・帯域制限を注入して障害時の挙動を開発中に検証する | 既存 fault injection + Phase 2 拡張 |
| **Record & Replay** | 通信を記録し、リプレイ/モックサーバー化してオフライン開発・テストに使う | Phase 2 (HAR, replay, mock) |
| **Spec-gen** | 実トラフィックから OpenAPI 等の仕様を書き起こし、ドリフトを検知する | Phase 3 |

### 2.3 ターゲットユーザーと代表ユースケース

1. **フロントエンド/BFF 開発者**: 「この画面を開いたとき裏で何が飛んでいるか」を `phantom -- npm run dev` の一発で確認。遅い API・失敗リクエストを TUI で特定。
2. **バックエンド開発者**: 自サービスが呼ぶ外部 API(決済、認証、LLM API)の実リクエストを確認し、`--fault` でタイムアウト耐性を検証。
3. **AI コーディングエージェント**(Claude Code 等): `phantom --output jsonl -- <cmd>` でアプリの通信を構造化データとして取得し、デバッグ・テスト生成・API 理解に使う。**機械可読であることは Phantom の一級要件**。
4. **QA / SDET**: 記録したトラフィックをモックサーバー化し、外部依存なしの E2E テストを構築。
5. **API 設計者**: ドキュメントのない既存システムの実トラフィックから OpenAPI をリバース生成。

### 2.4 スコープ境界

- **やる**: HTTP/1.1, HTTP/2, WebSocket, SSE のキャプチャと操作。ローカル・単一開発者マシンでの利用。CLI/TUI/(将来)ローカル Web UI。
- **やらない(当面)**: 本番環境常駐エージェント、分散トレーシング基盤(Jaeger 代替)、カーネル eBPF、マルチユーザー SaaS。
- **長期研究トラックに隔離**(§9): bpftime、Arazzo 自動推論、ローカル LLM (Candle)、Tauri GUI、DB プロトコル(MySQL/PostgreSQL)。**「Phase 3 まで完了し、かつ利用実績がある」ことを着手条件とする。**

### 2.5 競合との差別化

| ツール | 強み | Phantom が取る差別化 |
|---|---|---|
| mitmproxy | 成熟した MITM、Python アドオン | ゼロ設定(`phantom -- cmd` 一発、CA 自動信頼注入)、Rust 単一バイナリ、AI エージェント向け JSONL、fault injection 内蔵 |
| Charles / Proxyman | GUI の見やすさ | ターミナル完結(SSH 先で動く)、無料 OSS、スクリプタブル |
| Keploy | record & replay テスト生成 | テスト特化ではなく汎用ツールボックス。TUI での探索体験 |
| HTTP Toolkit | ブラウザ/端末の自動インターセプト | CLI ファースト、記録データのローカル永続化と再利用(mock/spec-gen) |

---

## 3. アーキテクチャ進化方針

既存の設計原則(`AGENTS.md` 記載)は維持する。追加する方針は以下。

### 3.1 CLI をサブコマンド制へ移行(後方互換維持)

現在は単一コマンド + フラグ。Phase 0 で `clap` のサブコマンドを導入し、**サブコマンド省略時は従来どおり `run` 相当として動く**ようにする(既存ユーザー・既存テスト・既存ドキュメントを壊さない)。目標形は[付録 B](#付録-b-cli-全体シノプシス-目標形)。

### 3.2 「キャプチャ」と「活用」の分離

これまで: キャプチャ時にしかデータを見られない。
これから: Fjall ストアを唯一の真実とし、`export` / `replay` / `mock` / `spec` はすべて**ストアからの読み出しで動く独立サブコマンド**にする。これにより各機能が疎結合になり、LLM エージェントがタスク単位で並行実装できる。

### 3.3 スキーマバージョニング

`HttpTrace` の永続化表現と JSONL 出力に `schema_version` を導入(P1-3)。フィールド追加は同一メジャー内で常に「追加のみ・省略可能」とする。

### 3.4 クレート追加方針

新機能は既存クレートの肥大化を避けて追加する:

```
crates/
  phantom-core/      # (既存) 型・トレイト。今後も I/O なしを厳守
  phantom-storage/   # (既存) Fjall
  phantom-capture/   # (既存) proxy / ldpreload / fault
  phantom-tui/       # (既存) Ratatui
  phantom-agent/     # (既存) LD_PRELOAD dylib
  phantom-export/    # (新規, P2-1) HAR / curl / JSONL 変換
  phantom-replay/    # (新規, P2-3, P2-4) replay クライアント + mock サーバー
  phantom-spec/      # (新規, P3-1) OpenAPI 推論
```

依存方向は常に `main → phantom-* → phantom-core` を維持し、横方向依存(例: `phantom-replay → phantom-export`)は禁止。共有が必要な型は `phantom-core` へ昇格させる。

---

## 4. ロードマップ全体像

| Phase | バージョン | テーマ | 期間目安 | 完了条件(エグジット基準) |
|---|---|---|---|---|
| 0 | v0.1.x | リポジトリ健全化・基盤整備 | 1–2 週 | Open PR ゼロ、CA 永続化で `curl`/Python の HTTPS が `--insecure` なしで復号できる、README 完備 |
| 1 | v0.2 | 信頼できる HTTP デバッガ | 3–4 週 | gzip API を TUI で読める、`?` でヘルプが出る、`status:5xx` で絞れる、機微ヘッダをマスクできる |
| 2 | v0.3 | Web 開発ワークフロー統合 | 4–6 週 | HAR エクスポート → Chrome DevTools で開ける、`phantom mock` で記録から API スタブが立つ、SSE がリアルタイムに見える |
| 3 | v0.4 | 仕様生成とエコシステム統合 | 4–6 週 | 実トラフィックから valid な OpenAPI 3.1 が出る、バイナリ配布(GitHub Releases + Homebrew) |
| 4 | v1.0+ | 研究トラック(bpftime / Arazzo / GUI / DB プロトコル) | 未定 | 着手条件: Phase 3 完了 + 外部ユーザーからの需要シグナル |

**タスク ID 規約**: `P<phase>-<番号>`。付録 C に全タスクの依存グラフを掲載。

---

## 5. Phase 0: リポジトリ健全化と基盤整備 (v0.1.x)

### P0-1: PR #4 (Java 統合テスト) の rebase とマージ

- **目的**: ポリグロット対応(Node 以外でも動く)の回帰保証を得る。Open PR を滞留させない。
- **依存**: なし
- **規模**: S
- **変更対象**: ブランチ `claude/add-springboot-tests-ZiHDa`(PR #4)。プロダクトコードの変更なし。
- **実装手順**:
  1. `git fetch origin main claude/add-springboot-tests-ZiHDa`
  2. PR ブランチを最新 `main` (fault injection 込み) に rebase。競合は `tests/` 配下と `Cargo.toml` の dev-dependencies に限られる見込み。プロダクトコードに手を入れないこと。
  3. ローカルで `cargo test --test proxy_springboot_integration -- --nocapture` を実行(java/mvn 不在なら skip されることを確認)。
  4. `make check` が通ることを確認して push、CI green を待ってマージ。
  5. マージ後、`AGENTS.md` の「Testing」節に Java テスト 2 本の行を追記する。
- **受け入れ基準**:
  - [ ] PR #4 がマージ済みで、CI が green。
  - [ ] `AGENTS.md` に Java 統合テストの実行方法が記載されている。
- **備考**: CI ランナーに Java 17+/Maven がない場合はテストが skip される。それで良い(ローカル検証用)。CI に Java を入れるかは P0-5 で判断。

### P0-2: PR #2 (MySQL トレース) のクローズと Issue 化

- **目的**: コンセプト外の大型 PR を敬意をもって棚上げし、main との乖離拡大を止める(§1.2 参照)。
- **依存**: なし
- **規模**: S
- **実装手順**:
  1. PR #2 に次の趣旨のコメントを投稿する: 「HTTP コア体験を優先するロードマップ(ROADMAP.md §1.2, §9 P4-4)に基づき一旦クローズする。パケット解析の設計は Phase 4 のプロトコルプラグイン化で再利用する。」
  2. PR をクローズする(ブランチは削除しない)。
  3. GitHub Issue「Protocol plugin architecture: MySQL/PostgreSQL tracing (deferred, see ROADMAP P4-4)」を作成し、PR #2 へのリンクと、P4-4 の設計方針(イベントモデル一般化が前提)を記載する。
- **受け入れ基準**:
  - [ ] PR #2 がクローズされ、経緯コメントがある。
  - [ ] 対応する Issue が存在し、PR #2 とロードマップにリンクしている。

### P0-3: CA 証明書の永続化・信頼ストア自動注入・`HTTPS_PROXY` 設定 【最重要】

- **目的**: G1・G2 の解消。**Node 以外のあらゆるクライアント(curl, Python, Go, Java, ブラウザ)で、TLS 検証を無効化せずに HTTPS を復号できるようにする。** これが直らない限り「HTTPS 対応」は実質 Node 専用機能である。
- **依存**: なし(P0-6 のサブコマンド化と同時に進めると `phantom cert` を自然に置ける)
- **規模**: M
- **変更対象**:
  - `crates/phantom-capture/src/proxy.rs` — `generate_ca()` を「ロードまたは生成」に変更
  - `src/main.rs` — `spawn_proxy_child()` の環境変数注入、`cert` サブコマンド
  - `docs/how-to-use.ja.md`, `README.md` — 信頼手順の記載
- **実装手順**:
  1. **CA の永続化**: `proxy.rs` の `generate_ca()` を `load_or_generate_ca(data_dir: &Path)` に変更する。
     - 保存先: `<data_dir>/ca/phantom-ca.key.pem`(秘密鍵, パーミッション 0600)と `<data_dir>/ca/phantom-ca.cert.pem`。
     - 両ファイルが存在すれば `rcgen::KeyPair::from_pem()` / `CertificateParams::from_ca_cert_pem()` で復元、なければ生成して書き出す。
     - 有効期限は生成時から 10 年。CommonName は現行どおり `Phantom Proxy CA`。
     - `ProxyCaptureBackend::new()` のシグネチャに `data_dir` を渡す必要がある。`src/main.rs` から `cli.data_dir` を引き回すこと。
  2. **子プロセスへの信頼注入**: `spawn_proxy_child()` で以下の環境変数を追加設定する(いずれも CA cert の PEM パスを指す)。これにより主要スタックが**検証を無効化せず** Phantom CA を信頼する:
     - `HTTPS_PROXY` / `https_proxy` = `http://127.0.0.1:<port>` ← G2 修正
     - `NO_PROXY` / `no_proxy` は**設定しない**(ユーザーの既存値を尊重、上書きもしない)
     - `SSL_CERT_FILE`(OpenSSL 系/Ruby/一部 Go)= CA PEM パス
     - `CURL_CA_BUNDLE`(curl)= CA PEM パス
     - `REQUESTS_CA_BUNDLE`(Python requests)= CA PEM パス
     - `NODE_EXTRA_CA_CERTS`(Node.js)= CA PEM パス
     - `DENO_CERT`(Deno)= CA PEM パス
     - 注意: 既にユーザーが同名変数を設定している場合は**上書きせず、警告を `eprintln!` する**。
  3. **`phantom cert` サブコマンド**(P0-6 のサブコマンド基盤の上に実装):
     - `phantom cert path` — CA cert の絶対パスを stdout に出力(スクリプト用)。
     - `phantom cert export [--out FILE]` — PEM を指定先へコピー(デフォルト: カレントの `phantom-ca.cert.pem`)。
     - `phantom cert print` — PEM 本文を stdout へ。
     - OS 信頼ストアへの自動インストールは**行わない**(システム改変は明示操作であるべき)。代わりに `cert export` の出力末尾に、macOS (`security add-trusted-cert`)・Ubuntu (`update-ca-certificates`)・Windows (`certutil`) の手動コマンド例を eprintln で案内する。
  4. **proxy-preload.js の簡素化検討はしない**(既存の Node 経路は動いているため触らない。`NODE_EXTRA_CA_CERTS` は preload 非対応クライアントの保険となる)。
  5. テスト:
     - 単体: `load_or_generate_ca` が (a) 初回生成、(b) 再起動時に同一 cert を復元、(c) 壊れた PEM の場合に再生成しエラーにしない、をカバー。
     - 統合: `tests/proxy_curl_https_integration.rs` を新設。rustls のモックHTTPSバックエンド(既存テストの流儀を踏襲)に対し `phantom --output jsonl -- curl https://127.0.0.1:<port>/api/health` を実行し、**`--insecure` なしで** trace が取れることを確認。curl 不在なら skip。
- **受け入れ基準**:
  - [ ] Phantom を 2 回起動しても CA cert のフィンガープリントが同一。
  - [ ] `phantom -- curl https://httpbin.org/get`(または統合テストのモック)が `--insecure` なしで復号キャプチャできる。
  - [ ] `phantom -- python -c "import requests; requests.get('https://...')"` が検証エラーにならない(手動確認で可)。
  - [ ] `phantom cert path` が存在するファイルパスを返す。
  - [ ] 既存の Node 統合テストが全て green のまま。
- **セキュリティ注意**: 秘密鍵は 0600。`--data-dir` がプロジェクト内を指す場合に鍵がコミットされないよう、`ca/` ディレクトリに `.gitignore`(中身 `*`)を書き出すこと。

### P0-4: README の全面執筆

- **目的**: G9 の解消。初見の人間と LLM エージェントが 30 秒で価値を理解し 3 分で動かせる入口を作る。
- **依存**: なし(P0-3 完了後に HTTPS 手順を反映するのが望ましい)
- **規模**: S
- **変更対象**: `README.md`
- **実装手順**: 以下の構成で英語で書く(日本語版は `docs/how-to-use.ja.md` が既にあるためリンクする):
  1. タグライン: "Zero-instrumentation API observability toolbox for modern web development — observe, perturb, record, and spec your app's HTTP traffic without changing a line of code."
  2. 30 秒デモ(コードブロック 3 つ): `phantom -- node app.js` / `phantom --output jsonl -- npm test | jq ...` / `phantom --fault delay:500ms:/api -- node app.js`
  3. 機能一覧表(§1.1 を英訳・簡約)
  4. Installation(現状は `cargo build --release`。P3-5 後に更新)
  5. How it works(プロキシ/preload/LD_PRELOAD の 3 行説明 + 図)
  6. JSONL schema へのリンク(`AGENTS.md` の表を `docs/jsonl-schema.md` に切り出して参照)
  7. Roadmap(本ファイルへのリンク)・License
- **受け入れ基準**:
  - [ ] README に上記 7 セクションが存在する。
  - [ ] 記載コマンドがすべて現行 CLI で実際に動く(コピペ検証すること)。

### P0-5: CI 強化(macOS 追加・統合テスト分離)

- **目的**: プロキシは「クロスプラットフォーム」を謳うため、最低限 macOS でのビルド+単体テストを CI で保証する。統合テスト(Node/Java)は時間がかかるため別ジョブ化する。
- **依存**: P0-1
- **規模**: S
- **変更対象**: `.github/workflows/ci.yml`
- **実装手順**:
  1. 既存ジョブを `lint`(fmt+clippy, ubuntu)と `test`(matrix: `ubuntu-latest`, `macos-latest`)に分割。
  2. `integration` ジョブ(ubuntu のみ)を追加: `actions/setup-node@v4` (Node 22) を入れて `cargo test --test proxy_node_integration --test fault_injection -- --nocapture`。Java テストは `actions/setup-java@v4` (temurin 17) + Maven キャッシュを設定して実行(所要時間が 10 分を超えるようなら `schedule` + `workflow_dispatch` に降格して良い)。
  3. `Swatinem/rust-cache@v2` を全ジョブに導入。
- **受け入れ基準**:
  - [ ] macOS で build + 単体テストが green。
  - [ ] Node 統合テストが CI で実行されている(skip でなく)。

### P0-6: CLI サブコマンド基盤の導入(後方互換)

- **目的**: §3.1。Phase 1 以降の `export` / `replay` / `mock` / `spec` / `cert` を置ける構造を先に作る。
- **依存**: なし(P0-3 と同一 PR でも可)
- **規模**: M
- **変更対象**: `src/main.rs`(必要なら `src/cli.rs` に分離)
- **実装手順**:
  1. `clap` の `Subcommand` を導入し、`enum Command { Run(RunArgs), Cert(CertArgs) }` を定義。既存の全フラグ(`--backend`, `--output`, `--port`, `--insecure`, `--data-dir`, `--agent-lib`, `--fault`, `-- CMD`)は `RunArgs` へ移す。
  2. **後方互換**: サブコマンドが指定されない場合(`phantom --port 9090 -- node app.js` 等)は `Run` として解釈する。実装方法: トップレベル `Cli` 構造体に `#[command(subcommand)] command: Option<Command>` と `#[command(flatten)] run: RunArgs` を両方持たせ、`command == None` なら `run` を使う。`clap` の `args_conflicts_with_subcommands = true` を設定すること。
  3. ヘルプ文(`long_about` / `after_long_help`)は `run` サブコマンド側へ移し、トップレベルにはサブコマンド一覧と 3 行の概要を置く。
  4. 既存統合テストのコマンドライン(サブコマンドなし形式)を**変更せずに**全て通す。これが後方互換の回帰テストになる。
  5. `docs/how-to-use.ja.md` の CLI リファレンスを更新。
- **受け入れ基準**:
  - [ ] `phantom -- node app.js`(旧形式)と `phantom run -- node app.js`(新形式)が同一動作。
  - [ ] `phantom --help` にサブコマンド一覧、`phantom run --help` に詳細ヘルプが出る。
  - [ ] 既存統合テストが無変更で green。

---

## 6. Phase 1: コアユーザビリティ — 「信頼できる HTTP デバッガ」 (v0.2)

**テーマ**: 「表示されたものが正しく読める・探せる・機械が使える」。日常のデバッグで mitmproxy の代わりに選べる品質にする。

### P1-1: Content-Encoding の透過デコード

- **目的**: G3 の解消。gzip/brotli/zstd/deflate の応答ボディを人間可読で保存・表示する。
- **依存**: なし
- **規模**: M
- **変更対象**: `crates/phantom-capture/src/proxy.rs`, `crates/phantom-core/src/trace.rs`, `crates/phantom-capture/Cargo.toml`
- **実装手順**:
  1. 依存追加: `flate2`(gzip/deflate)、`brotli`、`zstd`。workspace deps に追加してから参照。
  2. `proxy.rs` のボディ収集後(`collect_body` の呼び出し側)で、レスポンスヘッダの `content-encoding` を見てデコードする関数 `fn decode_body(encoding: &str, bytes: &[u8]) -> Option<Vec<u8>>` を実装。対応: `gzip`, `deflate`, `br`, `zstd`, `identity`。複合(`gzip, br`)は右から順に解く。
  3. デコード成功時: `HttpTrace` にはデコード済みボディを格納し、新フィールド `content_encoding: Option<String>` に元のエンコーディング名を記録する(付録 A 参照)。デコード失敗時: 元バイト列をそのまま格納し `tracing::warn!` を出す(エラーで通信を壊さない)。
  4. **重要**: プロキシが下流へ返すレスポンス本体は**改変しない**(デコードは記録用コピーに対してのみ行う)。圧縮のままクライアントへ流す。
  5. リクエストボディの `content-encoding` も同様に処理(稀だが gRPC-web 等で存在する)。
  6. デコードは 1MB 制限(G4)の**後**ではなく**前**に適用するか?→ **圧縮バイトを最大 1MB まで収集 → デコード → デコード結果も `MAX_BODY_SIZE` で再度打ち切り**とする(zip bomb 対策としてデコード後サイズ上限は必須)。
  7. 単体テスト: gzip した JSON を復号できる/壊れた gzip で panic しない/zstd 済みボディがデコード後上限で切られる、の 3 ケースを `proxy.rs` の `#[cfg(test)]` に追加。
  8. 統合テスト: Node モックバックエンドに `Content-Encoding: gzip` 応答を追加し、JSONL の `response_body` が平文 JSON であることを assert。
- **受け入れ基準**:
  - [ ] gzip 応答の API を TUI で開くと JSON が読める。
  - [ ] JSONL に `content_encoding: "gzip"` が入り、`response_body` は展開済み。
  - [ ] クライアントが受け取るレスポンスはバイト単位で無改変(統合テストで body 一致を assert)。

### P1-2: ボディ切り捨てとバイナリの明示化

- **目的**: G4 の解消。「1MB で黙って切れる」「バイナリが文字化けする」を仕様として可視化する。
- **依存**: P1-1(同じ箇所を触るため直後に実施)
- **規模**: S
- **変更対象**: `crates/phantom-core/src/trace.rs`, `crates/phantom-capture/src/proxy.rs`, `src/main.rs`(JSONL)、`crates/phantom-tui/src/ui.rs`
- **実装手順**:
  1. `HttpTrace` に追加: `request_body_truncated: bool`, `response_body_truncated: bool`(デフォルト false、serde default で後方互換)。
  2. `run` に `--max-body <SIZE>` フラグを追加(デフォルト `1mb`。`512kb`/`5mb` のような表記を受け付けるパーサを実装、`0` で無制限)。`MAX_BODY_SIZE` 定数を設定値に置き換える。
  3. バイナリ判定: ボディ先頭 8KB に NUL バイトが含まれる、または UTF-8 として不正な割合が 10% を超える場合はバイナリとみなす。JSONL では `request_body_encoding` / `response_body_encoding` フィールド(`"utf-8"` | `"base64"`)を追加し、バイナリは base64 で出力する(付録 A)。
  4. TUI 詳細ペイン: 切り捨て時は `[body truncated at 1.0 MB — rerun with --max-body 0]`、バイナリ時は `[binary body, 24.3 KB, image/png]` のプレースホルダ表示。
- **受け入れ基準**:
  - [ ] 2MB の応答で `response_body_truncated: true` が JSONL に出る。
  - [ ] PNG 応答が JSONL で valid な base64 + `"base64"` マーカーになる(jq でパースが壊れない)。

### P1-3: JSONL スキーマ v2 と `phantom export jsonl` の下地

- **目的**: G6・G11 の解消。スキーマを versioned な公開契約に格上げする。
- **依存**: P1-1, P1-2(新フィールドを v2 に含めるため)
- **規模**: S
- **変更対象**: `src/main.rs`(`JsonlTrace`)、`docs/jsonl-schema.md`(新規)、`AGENTS.md`
- **実装手順**:
  1. `JsonlTrace` に `schema_version: u32`(値 `2`)を先頭フィールドとして追加。P1-1/P1-2 の新フィールドも追加。
  2. `docs/jsonl-schema.md` を新規作成し、付録 A の表を正とする完全なスキーマ文書を書く。`AGENTS.md`・`--help` のスキーマ記述をこの文書への参照+要約に差し替える(3 箇所に同じ表を持たない)。
  3. 互換性ポリシーを文書に明記: 「同一 `schema_version` 内ではフィールド追加のみ。削除・型変更・意味変更は version increment を伴う」。
- **受け入れ基準**:
  - [ ] 全 JSONL 行に `schema_version: 2` が含まれる。
  - [ ] `docs/jsonl-schema.md` が存在し、実装と一致する(統合テストで全フィールドの存在を assert)。

### P1-4: TUI 強化(第 1 弾)

- **目的**: G5 の解消。「見る」体験を実用レベルへ。
- **依存**: なし(P1-1 が入っているとボディ表示の価値が上がる)
- **規模**: L(内部で 4 つの独立 PR に分割可能。以下の 1.〜4. がそれぞれ 1 PR)
- **変更対象**: `crates/phantom-tui/src/app.rs`, `ui.rs`, `event.rs`, `lib.rs`
- **実装手順**:
  1. **構造化フィルタ** (`/` で入力): 現行の URL 部分一致に加え、`status:404` `status:4xx` `status:>=500` `method:post` `host:api.example.com` `path:/users` を空白区切り AND で解釈するミニクエリ言語を実装する。キーワードなしトークンは従来どおり URL 部分一致。パーサは `app.rs` に純関数 `fn parse_filter(input: &str) -> FilterExpr` として実装し、単体テストを最低 8 ケース書く(不正入力はプレーン部分一致にフォールバック)。
  2. **ヘルプオーバーレイ**: `?` でキーバインド一覧をモーダル表示、`Esc`/`?` で閉じる。フィルタ構文のチートシートも載せる。
  3. **詳細ペインの強化**: (a) `j/k` スクロールとスクロール位置インジケータ、(b) レスポンスボディが `content-type: application/json` のとき `serde_json` で pretty-print(失敗したら原文)、(c) タブ切替(`h/l` または `[`/`]`)で Request / Response / Headers / Timing のセクション移動。
  4. **コピー & エクスポートキー**: 選択トレースに対し `c` = curl コマンドとしてクリップボードへ(依存: `arboard`。クリップボード不可の環境では stderr に出力)、`w` = 単一トレースを JSON ファイルとして `./phantom-trace-<span_id>.json` に書き出し。curl 生成ロジックは後で P2-1 と共有するため `phantom-core` ではなく新設の変換モジュールに置く(P2-1 実装時に `phantom-export` へ移動)。
  5. すべての新キーはヘルプオーバーレイと `docs/how-to-use.ja.md` の表に反映する。
  6. TUI の状態遷移は既存規約どおり `App` のメソッドとして実装し、レンダリング関数は純関数を維持する。`app.rs` の単体テストでフィルタ・スクロール・タブ状態を検証。
- **受け入れ基準**:
  - [ ] `/status:5xx method:get` で該当トレースのみ表示。
  - [ ] `?` でヘルプが出る。
  - [ ] JSON 応答が整形表示される。
  - [ ] `c` で有効な curl コマンドが得られる(手動で再実行して同じレスポンスが返ること)。

### P1-5: 機微情報のリダクション

- **目的**: G8 の解消。トークンや Cookie が平文でディスク・JSONL・クリップボードに漏れることを防ぐ手段を提供する。
- **依存**: P1-3(スキーマに `redacted` 表現を追加するため)
- **規模**: M
- **変更対象**: `crates/phantom-core/src/trace.rs`(または新設 `redact.rs`)、`src/main.rs`, `crates/phantom-capture/src/proxy.rs`
- **実装手順**:
  1. `run` にフラグ追加: `--redact`(既定リストを有効化)、`--redact-header <NAME>`(繰り返し可、追加指定)、`--redact-body-field <JSON_KEY>`(繰り返し可)。
  2. 既定リスト: ヘッダ `authorization`, `proxy-authorization`, `cookie`, `set-cookie`, `x-api-key`; ボディの JSON キー `password`, `token`, `access_token`, `refresh_token`, `client_secret`, `api_key`。
  3. リダクション適用箇所: **トレースがチャネルに送られる前**(`proxy.rs` の `HttpTrace` 構築直後)に 1 回だけ適用する。値は `"[REDACTED]"` に置換(長さ情報も残さない)。ストレージ・TUI・JSONL・エクスポートすべてに一貫して効く。
  4. ボディの JSON キー置換は、パース可能な JSON のみ対象にトップレベルから再帰的にキー一致(大文字小文字無視)で置換する。パース不能ボディは触らない。
  5. デフォルトは **off**(ローカルデバッグでは生値が見たい)。ただし `docs/how-to-use.ja.md` に「トレースを共有・コミットする場合は必ず `--redact` を使う」と明記。
  6. 単体テスト: ヘッダ置換 / ネスト JSON の置換 / 非 JSON ボディ無変更 / 大文字ヘッダ名の一致。
- **受け入れ基準**:
  - [ ] `--redact` 付きで `authorization` ヘッダが JSONL・TUI ともに `[REDACTED]`。
  - [ ] `--redact-body-field secret` でボディ内 `"secret"` キーの値が置換される。

### P1-6: 非 Node ランタイムの透過対応マトリクス整備

- **目的**: P0-3 の環境変数注入が実際に各言語で機能することを検証・文書化し、動かないものを直す。「Node 専用ツール」の印象を払拭する。
- **依存**: P0-3
- **規模**: M
- **変更対象**: `tests/`(新規統合テスト)、`docs/how-to-use.ja.md`, `README.md`
- **実装手順**:
  1. `tests/apps/python-app/client.py` を新設: `urllib.request` と(あれば)`requests` で HTTP/HTTPS 各 1 リクエスト。`tests/proxy_python_integration.rs` から起動し、python3 不在なら skip。**期待**: P0-3 の `HTTPS_PROXY` + `REQUESTS_CA_BUNDLE`/`SSL_CERT_FILE` だけで復号キャプチャできること。
  2. 同様に `tests/apps/go-app/`(`net/http`、`go` 不在なら skip)。Go は `HTTPS_PROXY` と `SSL_CERT_FILE` を尊重する。
  3. curl 統合テスト(P0-3 で作成済みならスキップ)。
  4. 結果を `docs/compatibility.md`(新規)にマトリクスとして記録: 行 = ランタイム/ライブラリ、列 = HTTP キャプチャ / HTTPS キャプチャ / 必要条件。「未検証」も正直に書く。
- **受け入れ基準**:
  - [ ] Python / Go / curl の統合テストが存在し、ローカルで green(CI では処理系があれば実行)。
  - [ ] `docs/compatibility.md` が存在し README からリンクされる。

---

## 7. Phase 2: Web 開発ワークフロー統合 (v0.3)

**テーマ**: 記録したトラフィックを「資産」に変える。Record & Replay / Perturb の柱を完成させる。

### P2-1: `phantom export` サブコマンド(HAR / JSONL / curl)

- **目的**: G6 解消の中核。記録済みトレースを標準形式で取り出し、Chrome DevTools・他ツール・スクリプトへ渡せるようにする。
- **依存**: P0-6(サブコマンド基盤)、P1-3(スキーマ v2)
- **規模**: L
- **変更対象**: 新規クレート `crates/phantom-export/`、`src/main.rs`、`crates/phantom-core/src/storage.rs`(クエリ拡張)
- **実装手順**:
  1. `phantom-core` の `TraceStore` トレイトに範囲クエリを追加: `fn query(&self, opts: &QueryOptions) -> Result<Vec<HttpTrace>, StorageError>`。`QueryOptions { since: Option<SystemTime>, until: Option<SystemTime>, url_contains: Option<String>, method: Option<HttpMethod>, status_range: Option<(u16, u16)>, limit: usize }`。Fjall 実装は `by_time` パーティションの prefix scan で `since`/`until` を効かせ、残りはフィルタで良い(データ量はローカル規模)。
  2. 新クレート `phantom-export` を作成(§3.4 の依存規則に従う)。実装する変換:
     - `to_har(traces: &[HttpTrace]) -> serde_json::Value` — HAR 1.2 準拠。`log.creator = {name: "phantom", version: env!("CARGO_PKG_VERSION")}`。`startedDateTime` は ISO 8601、`time` は `duration_ms`。ボディは `postData.text` / `content.text`(base64 の場合 `encoding: "base64"`)。HAR 1.2 スキーマの必須フィールド(`cache: {}`, `timings` は send/wait/receive に分配できないため `wait = duration` で他 0)を漏らさないこと。
     - `to_curl(trace: &HttpTrace) -> String` — P1-4 で作った curl 生成をこのクレートへ移設して共有。
     - `to_jsonl(trace: &HttpTrace) -> JsonlTrace` — `src/main.rs` の変換ロジックをこのクレートへ移設(main は薄く保つ)。
  3. CLI: `phantom export --format har|jsonl|curl [--out FILE] [--since 1h] [--url-contains STR] [--method GET] [--status 400-599] [--limit N] [--data-dir DIR]`。`--since` は `30m`/`2h`/`7d` の相対表記と RFC3339 を受理。`--out` 省略時は stdout。
  4. テスト: HAR 出力を `serde_json` で再パースし必須フィールドを assert する単体テスト。Chrome DevTools への手動インポート確認を PR 説明に記載。
- **受け入れ基準**:
  - [ ] `phantom export --format har --out session.har` の成果物が Chrome DevTools の Network タブにインポートできる。
  - [ ] `--since 1h --status 500-599` で絞り込みが効く。
  - [ ] `src/main.rs` の JSONL 変換が `phantom-export` へ移動しても既存統合テストが green。

### P2-2: HAR インポートと TUI での閲覧

- **目的**: ブラウザで記録した HAR(DevTools からの書き出し)を Phantom の TUI・エクスポート・(将来の)mock で扱えるようにし、双方向の interop を完成させる。
- **依存**: P2-1
- **規模**: M
- **変更対象**: `crates/phantom-export/`(`from_har`)、`src/main.rs`(`phantom import`)
- **実装手順**:
  1. `phantom-export` に `from_har(json: &serde_json::Value) -> Result<Vec<HttpTrace>, ExportError>` を実装。trace_id/span_id は新規採番。復元できないフィールド(`source_addr` 等)は `None`。
  2. CLI: `phantom import <file.har> [--data-dir DIR]` — パースしてストアへ insert し、件数を表示。
  3. `phantom import file.har && phantom view` で閲覧できる…が `view` は未定義のため、本タスクで **`phantom view`**(キャプチャなしでストアを TUI 閲覧する読み取り専用モード)も追加する。実装は `run_tui` に空のダミーチャネルを渡すだけで成立する(`lib.rs` 参照)。
- **受け入れ基準**:
  - [ ] Chrome からエクスポートした実 HAR がエラーなくインポートできる(不正エントリは warn してスキップ)。
  - [ ] `phantom view` で過去トレースが閲覧できる(プロキシは起動しない)。

### P2-3: `phantom replay` — 記録リクエストの再送

- **目的**: 「さっきの失敗リクエストをもう一度」「API 修正後に同じ呼び出しで確認」を 1 コマンドにする。
- **依存**: P2-1(クエリ基盤)
- **規模**: M
- **変更対象**: 新規クレート `crates/phantom-replay/`、`src/main.rs`
- **実装手順**:
  1. `phantom-replay` に `reqwest`(rustls 構成)ベースの再送機能を実装: `async fn replay(trace: &HttpTrace, opts: &ReplayOptions) -> Result<HttpTrace, ReplayError>`。記録どおりの method/url/headers/body を送り、結果を新しい `HttpTrace`(新規 trace_id、`request_headers` に `x-phantom-replay-of: <元 span_id>` を付与)として返す。
  2. 除外ヘッダ: `host`, `content-length`, `connection`, `accept-encoding` は reqwest に任せて再設定する。
  3. `ReplayOptions { override_host: Option<String>, override_scheme: Option<String>, timeout: Duration }` — `--host localhost:3000` で本番記録をローカル実装に投げ直せるのが主要ユースケース。
  4. CLI: `phantom replay [--span-id ID | --last N | --url-contains STR ...(P2-1 と同じフィルタ群)] [--host HOST] [--save] [--output jsonl|summary]`。`--save` 指定時のみ結果をストアへ保存。デフォルト出力は `<method> <url> → <old_status> → <new_status> (<ms>)` の比較サマリ 1 行/件。
  5. 安全装置: フィルタが 20 件超に一致し、かつ `--yes` がない場合は件数を表示して中断する(誤って本番へ大量再送しない)。
- **受け入れ基準**:
  - [ ] `phantom replay --last 1` が直近トレースを再送し、新旧ステータスの比較を表示する。
  - [ ] `--host` で向き先を差し替えられる。
  - [ ] 21 件一致 + `--yes` なしで中断する。

### P2-4: `phantom mock` — 記録からのモックサーバー生成

- **目的**: 外部 API 依存を切り離したオフライン開発・決定論的テストを可能にする(Keploy 的価値の中核)。
- **依存**: P2-1
- **規模**: L
- **変更対象**: `crates/phantom-replay/`(mock モジュールを同居。§3.4 の表どおり)、`src/main.rs`
- **実装手順**:
  1. `axum` ベースの HTTP サーバーを実装。起動時にストアから対象トレース群(P2-1 のフィルタ)をロードし、ルーティングテーブルを構築する。
  2. **マッチング仕様**(この順で最初に一致したものを返す):
     1. method + path + query 完全一致
     2. method + path 完全一致(query 無視)
     3. method + パス構造一致(パスセグメント単位で、記録側セグメントが数値/UUID/16 進 24 文字以上なら任意値にマッチ — P3-1 のパラメータ推定と同じヒューリスティクスを共有)
     4. 不一致 → `501 Not Implemented` + JSON ボディ `{"phantom_mock": "no recorded response", "method": ..., "path": ...}`
  3. 同一キーに複数レコードがある場合は「記録順に順番に返し、尽きたら最後を繰り返す」(シーケンシャルモード。ステートフル API の連続呼び出しを再現するため)。
  4. レスポンスは記録どおりの status/headers/body。`content-encoding` は既に展開済み(P1-1)なので、当該ヘッダと `content-length` を除去して平文で返す。
  5. オプション: `--port 9000`(デフォルト)、`--latency recorded|none|<fixed_ms>`(デフォルト `none`。`recorded` は `duration_ms` を sleep)、`--cors`(全許可 CORS ヘッダ付与 + OPTIONS 自動応答)。
  6. 起動ログに「ロードしたルート一覧(method path → status, N variants)」を表示する。
  7. 統合テスト: 記録 → `phantom mock` 起動 → reqwest で叩いて記録どおりの応答が返る/未知パスが 501、をエンドツーエンドで検証。
- **受け入れ基準**:
  - [ ] `phantom mock --url-contains api.example.com --port 9000` で、記録済み `GET /users/42` に対し `GET /users/99` もパラメータ一致で応答する。
  - [ ] `--latency recorded` で記録レイテンシが再現される(±20ms 許容で統合テスト)。
  - [ ] `--cors` 指定でブラウザからの preflight が通る。

### P2-5: SSE / ストリーミング応答と WebSocket のキャプチャ

- **目的**: G7 の解消。モダン Web 開発(LLM ストリーミング、リアルタイム UI)で SSE/WS は不可避。「全読みしてから記録」の設計限界を破る。
- **依存**: P1-3(スキーマ拡張を伴うため)
- **規模**: L(SSE と WS で PR を分けること)
- **変更対象**: `crates/phantom-capture/src/proxy.rs`, `crates/phantom-core/src/trace.rs`, `crates/phantom-tui/`, `docs/jsonl-schema.md`
- **実装手順(SSE / ストリーミング)**:
  1. レスポンスヘッダが `content-type: text/event-stream`、または `transfer-encoding: chunked` かつボディ収集が `--stream-timeout`(新フラグ、デフォルト 10s)を超えた場合、**部分トレース**として扱う。
  2. 実装方式: ボディを tee するストリームラッパーを作り、(a) クライアントへは即時パススルー、(b) 記録側はチャンク到着ごとにバッファへ追記。**レスポンスヘッダ受信時点で `HttpTrace` を `status: "open"` で一度 emit** し、ストリーム終了(または `MAX_BODY_SIZE` 到達)時に完全版を同一 span_id で再 emit する。
  3. スキーマ追加(v2 内の追加なので互換): `stream: bool`, `stream_complete: bool`, `chunk_count: Option<u32>`。JSONL モードでは open/complete の 2 行が出る(同一 `span_id` で識別可能)ことを `docs/jsonl-schema.md` に明記。
  4. TUI: ストリーミング中のトレースに `⟳` インジケータ、完了で消す。ボディは SSE の場合 `data:` 行単位で改行表示。
- **実装手順(WebSocket)**:
  1. hudsucker の `WebSocketHandler` トレイトを実装し、フレーム(Text/Binary/Close)をキャプチャする。
  2. 新イベント型はあえて作らず、`HttpTrace` を「ハンドシェイク 1 件(101 応答)」として記録し、各メッセージは新フィールド `ws_messages: Option<Vec<WsMessage>>`(`WsMessage { direction: "send"|"recv", opcode, timestamp_ms, payload(1 メッセージ 64KB 上限, base64 可) }`)として同一トレースへ追記していく。上限 1000 メッセージ/接続、超過分はカウントのみ。
  3. ストレージ更新: `TraceStore` に `update(&self, trace: &HttpTrace)` を追加(同一キー上書きで実現可能)。
  4. TUI: 詳細ペインにメッセージのタイムライン表示(方向矢印 + ペイロード先頭 1 行)。
- **受け入れ基準**:
  - [ ] SSE エンドポイント(統合テストにモック追加)で、最初のイベント受信から 1 秒以内に TUI に行が現れる。
  - [ ] JSONL で open → complete の 2 行が同一 span_id で出る。
  - [ ] `wss://` エコーサーバー(テスト内モック)の送受信フレームが記録される。

### P2-6: セッション管理とタグ付け

- **目的**: 「昨日の記録と今日の記録が混ざる」問題を解消し、export/mock/replay の対象指定を人間的にする。
- **依存**: P2-1
- **規模**: M
- **変更対象**: `crates/phantom-core/src/trace.rs`, `crates/phantom-storage/src/fjall_store.rs`, `src/main.rs`, `crates/phantom-tui/`
- **実装手順**:
  1. `HttpTrace` に `session: String` を追加(serde default = `"default"`)。
  2. `run` に `--session <NAME>` を追加。省略時は `default`。加えて `--fresh` フラグ(そのセッションの既存データを起動時に削除)。
  3. ストレージ: 新パーティション `by_session`(キー `{session}\0{timestamp_be}{span_id}`)を追加し、`QueryOptions` に `session: Option<String>` を追加。既存パーティションは触らない(マイグレーション不要、旧データは `default` 扱い)。
  4. `phantom sessions list`(件数・期間つき)と `phantom sessions delete <NAME>` を追加。
  5. `export` / `replay` / `mock` / `view` の全フィルタに `--session` を追加。
  6. TUI: フッターに現行セッション名を表示。フィルタ構文に `session:NAME` を追加(P1-4 のパーサ拡張)。
- **受け入れ基準**:
  - [ ] `phantom run --session checkout-flow -- node app.js` → `phantom export --session checkout-flow --format har` が該当分のみ出力。
  - [ ] `phantom sessions list` が正しい件数を表示。
  - [ ] 旧データ(session フィールドなし)が壊れず `default` として見える。

### P2-7: フォールトインジェクション拡張

- **目的**: Perturb の柱を完成させる。カオステストの表現力を実務水準へ。
- **依存**: なし(既存 `fault.rs` の拡張)
- **規模**: M
- **変更対象**: `crates/phantom-capture/src/fault.rs`, `src/main.rs`, `tests/fault_injection.rs`
- **実装手順**:
  1. 新 spec 追加(既存の `delay:` / `error:` 文法と一貫させる):
     - `abort:0.1:/api` — 確率 10% で応答前に接続を切断(空応答/RST 相当。hudsucker では 502 を返さずストリームを drop)
     - `timeout:30s:/slow` — 応答を保留し続ける(クライアント側タイムアウトの検証用)
     - `bandwidth:56kbps` — レスポンスボディをチャンク分割 + sleep でスロットリング
     - `rewrite-status:200=503:0.5` — 上流の実応答を得た後にステータスだけ差し替え(error: と違い実処理は走る)
  2. `--fault-file <phantom-faults.yaml>` を追加。YAML 形式:
     ```yaml
     rules:
       - match: { url_contains: "/api/payment", method: POST }
         fault: { type: delay, min_ms: 200, max_ms: 800 }
       - match: { url_contains: "/api" }
         fault: { type: error, status: 503, probability: 0.1 }
     ```
     CLI の `--fault` 文字列は内部でこの構造にパースされる(単一の `FaultRule` 表現に統一)。
  3. 適用中のルールを TUI ヘッダに表示(例: `faults: 2 active`)。fault が発動したトレースには `fault_injected: Option<String>`(発動ルールの説明文字列)を記録し、TUI で行を黄色表示、JSONL にも出す(スキーマ追加)。
  4. 統合テスト: 各新 spec につき 1 ケース(`abort` はクライアントエラーになること、`bandwidth` は所要時間下限で検証)。
- **受け入れ基準**:
  - [ ] `--fault abort:1.0` で curl が `(52) Empty reply` 相当で失敗する。
  - [ ] `--fault-file` の YAML が CLI 指定と同じ挙動になる。
  - [ ] JSONL の `fault_injected` でどのルールが発動したか判別できる。

### P2-8: Deno / Bun の透過注入

- **目的**: モダン JS ランタイムのカバレッジ拡大。
- **依存**: P0-3
- **規模**: S
- **変更対象**: `src/main.rs`(`is_node_command` 周辺)、`tests/`
- **実装手順**:
  1. Bun: `bun` は Node 互換の `--require`(`--preload`)を持つ。`is_node_command` を `enum JsRuntime { Node, Bun, Deno }` を返す `detect_js_runtime` に改め、Bun には `--preload <script>` を注入。preload スクリプトの互換性問題(undici 内蔵でない等)は try/catch 済みの現行実装で概ね吸収されるはずだが、統合テストで検証し、必要なら分岐を preload 側に足す。
  2. Deno: preload 機構がないため env のみ(`HTTPS_PROXY` + `DENO_CERT`、P0-3 で設定済み)。Deno の fetch はこれで CONNECT + MITM が通る。特別扱い不要なことをテストで確認し、`docs/compatibility.md` に記載。
  3. 統合テスト: `tests/apps/node-app/client.js` を流用し、bun/deno 不在なら skip。
- **受け入れ基準**:
  - [ ] `phantom -- bun run client.js` / `phantom -- deno run --allow-net client.ts` で HTTPS トレースが取れる(処理系がある環境で)。
  - [ ] `docs/compatibility.md` に両ランタイムの行が追加されている。

---

## 8. Phase 3: 仕様生成とエコシステム統合 (v0.4)

**テーマ**: Spec-gen の柱と、ツールとしての流通(配布・他システム連携)。

### P3-1: `phantom spec openapi` — トラフィックからの OpenAPI 3.1 生成

- **目的**: 記録済みトレースから OpenAPI 3.1 ドキュメントをリバース生成する。plan.md の構想のうち、LLM なしで決定論的に実現できる部分を先に出す。
- **依存**: P2-1(クエリ)、P2-6(セッション)
- **規模**: L(2〜3 PR に分割: ①パス正規化+骨格生成 ②スキーマ推論 ③CLI 統合と丸め)
- **変更対象**: 新規クレート `crates/phantom-spec/`、`src/main.rs`
- **実装手順**:
  1. **対象抽出**: `QueryOptions` で絞ったトレース群を URL の origin ごとにグループ化。`--base-url` 指定時はその origin のみ。
  2. **パステンプレート化**(mock の 3. と同一ヒューリスティクスを `phantom-core` の共有関数へ昇格):
     - セグメントが 全数字 / UUID / 24 文字以上の hex / `[A-Za-z0-9_-]{20,}` → パラメータ化。
     - パラメータ名の推定: 直前セグメントの単数形 + `Id`(`/users/42` → `/users/{userId}`)。単数形化は末尾 `s`/`ies` の単純規則で良い。
     - 同一テンプレートに複数トレースが畳み込まれることを確認するテストを書く(`/users/1` と `/users/2` が 1 path item になる)。
  3. **スキーマ推論**: JSON ボディから JSON Schema を推論する `infer_schema(samples: &[serde_json::Value]) -> Schema` を実装。規則: 複数サンプルの union(あるサンプルに無いキーは `required` から外す)、混在型は `oneOf`、配列は要素サンプル最大 20 件から推論、ネスト深さ上限 16。既存クレート `schemars` は逆方向(Rust→schema)なので使わない。自前実装+単体テスト 10 ケース以上。
  4. **OpenAPI 組み立て**: `openapi: 3.1.0`。各 operation に: パラメータ(path/query — query は観測されたキーの union、すべて `required: false`)、`requestBody`(content-type 別)、`responses`(観測されたステータスごと、スキーマ+`example` は最初のサンプルの redact 済み値)。`operationId` は `{method}{PascalCase path}`(`getUsersUserId`)。`info.title` は origin、`servers` に origin。
  5. CLI: `phantom spec openapi [フィルタ群] [--base-url URL] [--out openapi.yaml] [--format yaml|json]`。デフォルト yaml(`serde_yaml`)。
  6. **検証**: 生成物が OpenAPI として valid であることをテストで保証する。CI に Node があるため `npx @redocly/cli lint` を統合テストから呼ぶ(npx 不在なら skip)。最低限、`serde_json`/`serde_yaml` ラウンドトリップと必須フィールド assert は常時実行。
- **受け入れ基準**:
  - [ ] Node 統合テストのトラフィックから生成した YAML が Redocly lint でエラー 0(警告は許容)。
  - [ ] `/users/1`・`/users/2` が `/users/{userId}` に統合され、レスポンススキーマが両サンプルの union になっている。
  - [ ] 100 トレース程度で 1 秒未満。

### P3-2: `phantom spec diff` — 仕様ドリフト検知

- **目的**: 「実装が OpenAPI から乖離していないか」を CI で検査できるようにする(contract testing の軽量版)。
- **依存**: P3-1
- **規模**: M
- **変更対象**: `crates/phantom-spec/`、`src/main.rs`
- **実装手順**:
  1. `phantom spec diff <existing-openapi.yaml> [フィルタ群]` — 記録トラフィックから生成した仕様と既存ファイルを比較。
  2. 検出項目(それぞれ結果行のカテゴリになる): `undocumented-endpoint`(トラフィックにあるが仕様にない)、`unused-endpoint`(仕様にあるがトラフィックにない — 情報表示のみ)、`undocumented-field`(応答に仕様外フィールド)、`missing-field`(仕様の required フィールドが応答にない)、`type-mismatch`。
  3. 出力: 人間向けテキスト(デフォルト)と `--format json`。終了コード: 乖離なし=0、`unused-endpoint` のみ=0、それ以外の乖離あり=1(CI 組込みを想定)。`--fail-on` で閾値カテゴリを調整可能。
- **受け入れ基準**:
  - [ ] 仕様にないエンドポイントを叩いた記録で exit code 1 と `undocumented-endpoint` が出る。
  - [ ] 完全一致で exit 0。

### P3-3: OpenTelemetry (OTLP) エクスポート

- **目的**: 既存の可観測性基盤(Jaeger, Grafana Tempo, Datadog)へトレースを流し込めるようにし、「ローカルツールで終わらない」出口を作る。
- **依存**: P1-3
- **規模**: M
- **変更対象**: `src/main.rs`(出力モード追加)、依存に `opentelemetry` + `opentelemetry-otlp`
- **実装手順**:
  1. `run` に `--otlp-endpoint <URL>` を追加(既存 `--output` とは独立に併用可能。TUI を見ながら OTLP へも送る、が成立する)。
  2. トレース変換: `HttpTrace` → OTel span。`trace_id`/`span_id` は既に W3C 互換なのでそのまま流用。span 属性は OTel semantic conventions (HTTP client span) に従う: `http.request.method`, `url.full`, `http.response.status_code`, `server.address`, `network.protocol.version`。ボディは属性にしない(サイズ超過するため。`--otlp-include-bodies` でオプトイン、各 8KB 打ち切り)。
  3. 送信は専用 tokio タスクでバッチ(`BatchSpanProcessor`)。終了時に flush。エンドポイント不達は warn 1 回 + 以降 rate-limit(通信のたびに spam しない)。
  4. 統合テスト: OTLP/HTTP の受け口をテスト内に立てる(単純な axum ハンドラで protobuf を受けてカウント)か、`opentelemetry-stdout` での変換単体テストに留めるか → **変換の単体テスト + 実サーバーは docker compose の手動確認手順を `docs/` に記載**で可とする。
- **受け入れ基準**:
  - [ ] `phantom run --otlp-endpoint http://localhost:4318 -- node app.js` で Jaeger UI にスパンが出る(手動確認手順が docs にある)。
  - [ ] trace_id が JSONL と OTLP で一致する。

### P3-4: `phantom serve` — ローカル Web UI(読み取り専用・第 1 版)

- **目的**: TUI では表現しきれないもの(ウォーターフォール、大きな JSON ツリー、共有可能な画面)を提供する。Tauri デスクトップアプリ(plan.md 構想)の代わりに、**依存ゼロで配れる埋め込み Web UI** を選ぶ(ADR-3)。
- **依存**: P2-1(クエリ API)、P2-6(セッション)
- **規模**: L(バックエンド API と UI で PR 分割)
- **変更対象**: `src/main.rs`、新規 `crates/phantom-serve/`(axum)、`web/`(フロントエンド、ビルド成果物を `rust-embed` で同梱)
- **実装手順**:
  1. **API**(axum, `127.0.0.1` バインド固定・トークン不要のローカル前提を明記):
     - `GET /api/traces?since=&url_contains=&status=&session=&limit=` → JSONL スキーマ v2 の JSON 配列
     - `GET /api/traces/:span_id` → 単一トレース(フルボディ)
     - `GET /api/stream` → SSE でライブトレース配信(キャプチャと併用時)
     - `GET /api/sessions`
  2. **UI**: ビルドステップを増やしたくないため、第 1 版は **フレームワークなしの素の TypeScript + Vite**(`web/` 以下)とし、成果物 `web/dist` を `rust-embed` でバイナリに焼き込む。画面: 左にトレーステーブル(仮想スクロール、列: time/method/status/url/duration/size)、右に詳細(headers / body の JSON ツリー / timing)。上部にフィルタバー(P1-4 と同じクエリ構文をフロントで再実装せず、そのまま API に渡してサーバー側で解釈)。
  3. CLI: `phantom serve [--port 5150] [--data-dir] [--session]`(閲覧専用)/ `phantom run --serve` (キャプチャと同時起動、SSE ライブ)。
  4. `Makefile` に `make web`(pnpm/npm build → dist 更新)を追加し、CI で dist の再現性を検証(dist をコミットする方式。ビルド環境がない貢献者を守る)。
- **受け入れ基準**:
  - [ ] `phantom serve` → ブラウザで過去トレースが閲覧・フィルタできる。
  - [ ] `phantom run --serve -- node app.js` でリクエストが発生と同時に画面に現れる(SSE)。
  - [ ] 外部インターフェースに bind していない(`127.0.0.1` 固定をテストで assert)。

### P3-5: 配布・リリースパイプライン

- **目的**: G10 の解消。`cargo build` できない人にも届ける。
- **依存**: なし(Phase 3 の任意時点)
- **規模**: M
- **変更対象**: `.github/workflows/release.yml`(新規)、`Makefile`, `README.md`
- **実装手順**:
  1. `cargo-dist` を導入(`dist init`)。ターゲット: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`(Windows は proxy backend のみなので `phantom-agent` を含めない)。
  2. タグ `v*` push で GitHub Releases にバイナリ+チェックサムを添付。インストールスクリプト(`curl | sh`)と Homebrew tap(`epli2/homebrew-phantom`)も cargo-dist の機能で生成。
  3. `crates.io` への `cargo install phantom-cli` 公開(バイナリ名は `phantom` を維持。crate 名 `phantom` は取得可否を確認し、不可なら `phantom-cli`)。
  4. バージョニング: SemVer。`CHANGELOG.md` を導入し、リリース PR で更新する運用を `AGENTS.md` に追記。
- **受け入れ基準**:
  - [ ] `v0.4.0` タグで 5 ターゲットのバイナリが Releases に並ぶ。
  - [ ] `brew install epli2/phantom/phantom` が通る。
  - [ ] README の Installation が更新されている。

---

## 9. Phase 4: 長期研究トラック (v1.0 以降)

**着手条件(ゲート)**: Phase 3 完了、かつ GitHub Stars/Issues 等で外部利用の需要シグナルがあること。各項目は着手前に **設計ドキュメント(`docs/rfcs/NNN-*.md`)を書き、レビューを経る**こと。

### P4-1: Arazzo ワークフロー推論

plan.md の中核構想。記録トレースの時系列から「API A の応答値が API B の入力に使われた」相関(Value Correlation)を検出し、Arazzo Specification を生成する。
- 前提: P3-1 の OpenAPI 生成が安定していること(Arazzo は OAS を `sourceDescriptions` として参照する)。
- 第 1 段は LLM なしのヒューリスティクス(完全一致値の遅延相関)で `phantom spec arazzo` を実装。UUID/token の追跡はこれで十分機能する。
- LLM による operationId 命名・セマンティック相関は第 2 段。**ローカル LLM (Candle) より先に、ユーザー持ち込みの API キー(Anthropic/OpenAI)対応を実装する方が現実的**(plan.md からの方針変更、ADR-4)。

### P4-2: bpftime / eBPF バックエンド

`CaptureBackend` トレイトの第 3 実装として検討。TLS ライブラリの uprobe フックにより CA 注入なしの HTTPS キャプチャが可能になる。plan.md の調査(コンテキストスイッチ排除、非特権実行)は有効だが、bpftime 自体の成熟度を毎四半期再評価してから着手する。Linux 限定であることに注意。

### P4-3: Tauri GUI

P3-4 の Web UI が利用実績を得たら、その資産(フロントエンド)をそのまま Tauri シェルへ載せる。Web UI と別物を作らないこと。

### P4-4: プロトコルプラグイン(MySQL / PostgreSQL / Redis / gRPC)

PR #2 の再来。前提となる設計作業: `HttpTrace` を `enum TraceEvent { Http(HttpTrace), Db(DbTrace), ... }` に一般化し、ストレージ・TUI・JSONL がイベント種別を透過的に扱えるようにする(これ自体が大型 RFC)。gRPC は HTTP/2 上なので既存プロキシの拡張(protobuf デコード、`--proto` でスキーマ指定)として最初に着手する価値がある。

### P4-5: Kubernetes サイドカー配布

ネイティブサイドカー(initContainers + restartPolicy: Always)としての Helm chart 提供。ローカルツールとしての完成後、チーム利用への拡張として検討。

---

## 10. 横断的関心事

### 10.1 品質ゲート(全 PR 共通)

- `make check`(fmt + clippy -D warnings + build + test)が green であること。
- 新機能には単体テスト必須。ユーザー可視の動作変更には統合テストまたは `docs/` 更新を伴うこと。
- `AGENTS.md` のコーディング規約(エラー処理二層戦略、newtype、インポート順)に従うこと。
- 破壊的変更(CLI フラグ削除、JSONL フィールド変更)は本ロードマップの改訂 + ADR 追加なしに行わないこと。

### 10.2 パフォーマンス予算

- プロキシ経由のレイテンシ追加: p50 < 3ms(fault なし、ローカルバックエンド)。
- 10,000 トレース保存時の TUI 起動: < 1s。
- リリースバイナリサイズ: < 30MB。
- Phase 2 完了時に `benches/`(criterion)でプロキシスループットの回帰ベンチを導入する。

### 10.3 セキュリティ・プライバシー原則

- CA 秘密鍵はユーザーローカル・0600・リポジトリ混入防止(P0-3)。
- ネットワーク待受はすべて `127.0.0.1` 固定(proxy / mock / serve)。外部公開が必要になったら明示フラグ + 警告で。
- テレメトリ(利用統計の送信)は**実装しない**。
- 記録データを外部に送る機能(OTLP、将来の LLM)はすべて明示オプトイン。

### 10.4 ドキュメント体系

| ファイル | 役割 | 更新タイミング |
|---|---|---|
| `README.md` | 英語・入口・価値提案 | 機能追加ごと |
| `docs/how-to-use.ja.md` | 日本語ユーザーガイド | 機能追加ごと |
| `docs/jsonl-schema.md` | JSONL 公開契約(P1-3 で新設) | スキーマ変更時 |
| `docs/compatibility.md` | ランタイム対応マトリクス(P1-6 で新設) | 検証追加時 |
| `docs/rfcs/` | Phase 4 設計文書 | 着手前 |
| `AGENTS.md` | エージェント向け開発規約 | 構造変化時 |
| `ROADMAP.md` | 本書 | Phase 完了時に振り返りを追記 |
| `plan.md` | 初期技術調査(歴史的文書) | 凍結。冒頭に「ROADMAP.md 参照」の注記を追加すること(P0-4 で実施) |

---

## 11. LLM エージェント向け作業プロトコル

本ロードマップのタスクを LLM エージェントが実行する際の手順。

1. **タスク選択**: 付録 C の依存グラフで、依存先がすべて ✅ のタスクだけに着手する。1 PR = 1 タスク(L タスクは記載どおり分割)。
2. **着手前**:
   - `git fetch origin main && git checkout -b feat/<task-id>-<slug> origin/main`
   - タスクの「変更対象」ファイルと、関連する `AGENTS.md`(ルート + 該当クレート)を読む。
   - 本書の記述と現行コードが食い違う場合(先行 PR で状況が変わった等)は、**コードを正**とし、PR 説明に食い違いを記録する。
3. **実装中**:
   - 「実装手順」の番号順に進める。手順にない大きな設計判断が必要になったら、独断で進めず PR を Draft にして判断材料を書く。
   - 新フラグ・新サブコマンドは必ず `--help` 文とドキュメント(§10.4 の該当ファイル)を同時に更新する。
4. **完了条件**: タスクの「受け入れ基準」チェックボックスをすべて満たし、`make check` が green で、PR 説明に (a) 対応タスク ID、(b) 受け入れ基準の充足状況、(c) 手動確認した内容、を記載していること。
5. **禁止事項**: 依存タスク未完了での見切り実装/JSONL スキーマの無断変更/`127.0.0.1` 以外への bind /複数タスクの混載 PR。

---

## 付録 A: JSONL スキーマ v2 (案)

`schema_version: 2`。v1(現行)からの変更は**追加のみ**。(*) が新規フィールド。

| フィールド | 型 | 導入 | 説明 |
|---|---|---|---|
| `schema_version` (*) | number | P1-3 | 常に `2` |
| `trace_id` | string | v1 | W3C 互換 128-bit(hex 32 桁) |
| `span_id` | string | v1 | 64-bit(hex 16 桁) |
| `timestamp_ms` | number | v1 | リクエスト開始(Unix epoch ms) |
| `duration_ms` | number | v1 | 往復レイテンシ |
| `method` | string | v1 | HTTP メソッド |
| `url` | string | v1 | 完全 URL |
| `status_code` | number | v1 | 応答ステータス |
| `protocol_version` | string | v1 | 例 `"HTTP/2.0"` |
| `request_headers` / `response_headers` | object | v1 | 小文字キー → 値 |
| `request_body` / `response_body` | string? | v1 | デコード済みボディ(P1-1 以降は Content-Encoding 展開済み) |
| `request_body_encoding` / `response_body_encoding` (*) | string? | P1-2 | `"utf-8"` \| `"base64"`。省略時 utf-8 |
| `request_body_truncated` / `response_body_truncated` (*) | boolean | P1-2 | `--max-body` による打ち切り |
| `content_encoding` (*) | string? | P1-1 | 元の圧縮方式(`"gzip"` 等)。展開失敗時は `body` が生バイトのままである印 |
| `session` (*) | string | P2-6 | セッション名。既定 `"default"` |
| `stream` / `stream_complete` / `chunk_count` (*) | boolean / boolean / number? | P2-5 | ストリーミング応答。open/complete の 2 行が同一 span_id で出る |
| `ws_messages` (*) | array? | P2-5 | WebSocket メッセージ(direction / opcode / timestamp_ms / payload) |
| `fault_injected` (*) | string? | P2-7 | 発動したフォールトルールの記述 |
| `source_addr` / `dest_addr` | string? | v1 | ソケットアドレス |

## 付録 B: CLI 全体シノプシス (目標形)

```
phantom [RUN-FLAGS] [-- CMD ...]          # 後方互換: サブコマンド省略 = run
phantom run   [--backend proxy|ldpreload] [--output tui|jsonl] [--port N]
              [--session NAME] [--fresh] [--max-body SIZE]
              [--redact] [--redact-header H]... [--redact-body-field K]...
              [--fault SPEC]... [--fault-file FILE]
              [--otlp-endpoint URL] [--serve] [--insecure]
              [--data-dir DIR] [--agent-lib PATH] [-- CMD ...]
phantom view    [--session NAME] [--data-dir DIR]           # 読み取り専用 TUI
phantom serve   [--port 5150] [--session NAME]              # Web UI
phantom cert    path | export [--out FILE] | print
phantom export  --format har|jsonl|curl [FILTERS] [--out FILE]
phantom import  <file.har>
phantom replay  [FILTERS | --span-id ID | --last N] [--host HOST] [--save] [--yes]
phantom mock    [FILTERS] [--port 9000] [--latency recorded|none|MS] [--cors]
phantom spec    openapi [FILTERS] [--base-url URL] [--out FILE]
              | diff <openapi.yaml> [FILTERS] [--fail-on CATEGORY]
              | arazzo ...                                   # Phase 4
phantom sessions list | delete <NAME>

FILTERS = [--session NAME] [--since DUR|RFC3339] [--until ...]
          [--url-contains STR] [--method M] [--status LO-HI] [--limit N]
          [--data-dir DIR]
```

## 付録 C: タスク一覧サマリ

| ID | タスク | 規模 | 依存 | Phase |
|---|---|---|---|---|
| P0-1 | PR #4 rebase & マージ | S | — | 0 |
| P0-2 | PR #2 クローズ & Issue 化 | S | — | 0 |
| P0-3 | CA 永続化 + 信頼注入 + HTTPS_PROXY | M | — | 0 |
| P0-4 | README 全面執筆 | S | (P0-3 推奨) | 0 |
| P0-5 | CI 強化 (macOS / 統合テスト) | S | P0-1 | 0 |
| P0-6 | CLI サブコマンド基盤 | M | — | 0 |
| P1-1 | Content-Encoding デコード | M | — | 1 |
| P1-2 | 切り捨て/バイナリ明示 + `--max-body` | S | P1-1 | 1 |
| P1-3 | JSONL スキーマ v2 | S | P1-1, P1-2 | 1 |
| P1-4 | TUI 強化第 1 弾(4 PR) | L | — | 1 |
| P1-5 | リダクション | M | P1-3 | 1 |
| P1-6 | 非 Node ランタイム検証マトリクス | M | P0-3 | 1 |
| P2-1 | `export` (HAR/JSONL/curl) | L | P0-6, P1-3 | 2 |
| P2-2 | HAR インポート + `view` | M | P2-1 | 2 |
| P2-3 | `replay` | M | P2-1 | 2 |
| P2-4 | `mock` | L | P2-1 | 2 |
| P2-5 | SSE / WebSocket(2 PR) | L | P1-3 | 2 |
| P2-6 | セッション管理 | M | P2-1 | 2 |
| P2-7 | フォールト拡張 + `--fault-file` | M | — | 2 |
| P2-8 | Deno / Bun | S | P0-3 | 2 |
| P3-1 | `spec openapi`(3 PR) | L | P2-1, P2-6 | 3 |
| P3-2 | `spec diff` | M | P3-1 | 3 |
| P3-3 | OTLP エクスポート | M | P1-3 | 3 |
| P3-4 | `serve` Web UI(2 PR) | L | P2-1, P2-6 | 3 |
| P3-5 | 配布パイプライン | M | — | 3 |
| P4-1〜P4-5 | 研究トラック | — | Phase 3 完了 | 4 |

**クリティカルパス**: P0-6 → P2-1 → {P2-2, P2-3, P2-4, P2-6} → {P3-1, P3-4}。
**並行可能な独立トラック**: P1-4(TUI)、P2-7(fault)、P3-5(配布)は他とほぼ干渉しない。

## 付録 D: 意思決定記録 (ADR)

- **ADR-1: HTTP レイヤーに集中し、DB プロトコルは Phase 4 へ延期する。** 理由: コア体験(HTTPS 復号、圧縮、TUI)が未完成の段階でのプロトコル追加は全レイヤーの複雑度を倍化させる。PR #2 はこの決定によりクローズ(§1.2)。
- **ADR-2: bpftime / カーネル eBPF より先にプロキシ + CA 自動信頼を磨く。** 理由: 開発マシンのユースケースではプロキシで 9 割のクライアントをカバーでき、クロスプラットフォームで動く。eBPF は Linux 限定かつ成熟待ち。
- **ADR-3: GUI は Tauri デスクトップアプリではなく、バイナリ埋め込みのローカル Web UI (`phantom serve`) から始める。** 理由: 配布物が単一バイナリのまま増えず、SSH ポートフォワードでリモート利用でき、将来 Tauri に載せ替える際もフロント資産を再利用できる。
- **ADR-4: LLM 統合はローカル推論 (Candle) より BYOK(ユーザーの API キー)を先行させる。** 理由: バイナリサイズ・モデル配布・品質のすべてで現実的。ローカル推論は需要が確認できてから。
- **ADR-5: JSONL スキーマは versioned な公開契約とし、追加のみ許す。** 理由: AI エージェントが第一級ユーザーであり、黙ったスキーマ変更は下流を静かに壊す。
