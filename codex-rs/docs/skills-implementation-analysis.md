# Codex-RS コードリーディングガイド: Skills機能を中心に

## はじめに

### このドキュメントの目的

このドキュメントは **codex-rs プロジェクトのコードリーディングを効率化し、理解を促進する** ことを目的としている。特に Skills 機能を中心に、以下を提供する：

1. **全体像の把握** - Codex のデータフローを理解し、各コンポーネントの役割を把握
2. **Skills 機能の深掘り** - Skills がどこでどのように使われるかを詳細に解説
3. **コードへの直接リンク** - ファイルパスと行番号を明示し、すぐにコードを参照可能に

### Codex とは何か

Codex は OpenAI が開発した AI コーディングアシスタントのコマンドラインインターフェース（CLI）である。ユーザーが自然言語で指示を出すと、AI がコードの生成、編集、ファイル操作、シェルコマンドの実行などを行う。codex-rs はこの CLI ツールの Rust 実装であり、高いパフォーマンスと型安全性を特徴とする。

主な機能：

- **対話型インターフェース**: TUI（Text User Interface）を通じてユーザーと対話
- **ツール呼び出し**: ファイル読み書き、シェルコマンド実行、MCP サーバー連携
- **Skills 機能**: 専門的な知識やワークフローを AI にプラグインとして提供
- **セッション管理**: 会話履歴の保存・再開、トークン使用量の管理

### 対象読者

- Python/JavaScript の経験があり、Rust を学習中の開発者
- Codex のアーキテクチャを理解したい開発者
- Skills 機能を拡張・カスタマイズしたい開発者

### 読み方のガイド

| 目的                       | 読むべきセクション                                                                                  |
| -------------------------- | --------------------------------------------------------------------------------------------------- |
| 全体像を素早く把握したい   | [クイックリファレンス](#クイックリファレンスtldr)、[全体アーキテクチャ](#part-1-全体アーキテクチャ) |
| Skills 機能を理解したい    | [Part 2: Skills機能](#part-2-skills機能の詳細)                                                      |
| 特定のファイルを読みたい   | [ファイル・関数リファレンス](#ファイル関数リファレンス)                                             |
| コードに直接ジャンプしたい | 各セクションの `**ファイル**: path:line` 表記を参照                                                 |

### 用語解説

| 用語        | 説明                                                       |
| ----------- | ---------------------------------------------------------- |
| **Op**      | Operation の略。ユーザー入力やシステムイベントを表す列挙型 |
| **Session** | 一つの会話セッション。履歴や設定を管理する                 |
| **Turn**    | AI との一往復（ユーザー入力 → AI 応答）                    |
| **Skill**   | AI に専門知識を提供するプラグイン。SKILL.md ファイルで定義 |
| **MCP**     | Model Context Protocol。外部ツールとの連携プロトコル       |
| **Rollout** | 会話履歴の永続化ファイル。セッション再開に使用             |

---

## クイックリファレンス（TL;DR）

### Codex の基本フロー（3行要約）

Codex はユーザーの入力をイベントとして受け取り、非同期チャネル経由でセッションに渡し、LLM API を呼び出して応答を生成する。Skills はこの流れの中で2箇所に注入される：セッション開始時（利用可能なスキル一覧）とタスク実行時（選択されたスキルの詳細内容）。

```
ユーザー入力 → [TUI] → Op → [Submission Channel] → [Session] → [run_task] → [run_turn] → [ModelClient] → LLM API
                                                         ↓
                                                   Skills 注入
```

### Skills 機能の要点

Skills は「AI に対するオンボーディングガイド」として機能する。プロジェクト固有のルール、ツールの使い方、ドメイン知識などを SKILL.md ファイルにまとめておくことで、AI が適切に振る舞えるようになる。

| 項目                 | 内容                                                        |
| -------------------- | ----------------------------------------------------------- |
| **機能**             | SKILL.md ファイルからスキルを読み込み、LLM プロンプトに注入 |
| **主要ファイル**     | `core/src/skills/` ディレクトリ（6ファイル）                |
| **エントリポイント** | `SkillsManager::new()` → `skills_for_cwd()`                 |
| **注入タイミング**   | セッション開始時（一覧）+ ユーザー入力時（詳細内容）        |
| **優先順位**         | Repo > User > System > Admin                                |

### 主要ファイル一覧（Skills）

以下は Skills モジュールを構成するファイル一覧である。読む順序としては、まず `model.rs` でデータ構造を理解し、次に `manager.rs` でエントリポイントを確認、その後 `loader.rs` で詳細な読み込みロジックを追う流れを推奨する。

| ファイル              | 行数 | 役割                          | 最初に読むべき関数         |
| --------------------- | ---- | ----------------------------- | -------------------------- |
| `skills/mod.rs`       | 46   | モジュール定義と re-export    | -                          |
| `skills/model.rs`     | 76   | データ構造（SkillMetadata等） | `SkillMetadata` 構造体     |
| `skills/loader.rs`    | 1129 | スキル検出・YAML解析          | `load_skills_from_roots()` |
| `skills/manager.rs`   | 118  | キャッシング管理              | `skills_for_cwd()`         |
| `skills/injection.rs` | 148  | プロンプト注入                | `build_skill_injections()` |
| `skills/render.rs`    | 75   | マークダウン生成              | `render_skills_section()`  |
| `skills/system.rs`    | 272  | システムスキル展開            | `install_system_skills()`  |

### 主要ファイル一覧（コア）

Skills モジュールは単独では動作せず、コアモジュールから呼び出される。以下のファイルが Skills との接点を持つ。

| ファイル                  | 役割                             | Skills との関連                                           |
| ------------------------- | -------------------------------- | --------------------------------------------------------- |
| `codex.rs`                | セッション管理・タスク実行       | `run_task()` で Skills 注入（2229-2253行目付近）          |
| `project_doc.rs`          | プロジェクトドキュメント読み込み | `get_user_instructions()` で Skills 一覧挿入（35-69行目） |
| `conversation_manager.rs` | 会話マネージャー                 | `SkillsManager` 初期化（54-65行目）                       |
| `client.rs`               | LLM クライアント                 | プロンプト送信（Skills 内容を含む）                       |

---

# Part 1: 全体アーキテクチャ

Skills を理解する前に、Codex 全体のデータフローを把握することが重要である。このセクションでは、ユーザーがコマンドを入力してから AI が応答するまでの一連の流れを追う。

## 1.0 真のエントリポイント：main() から会話開始まで

Codex TUI の起動フローは `tui2/src/main.rs` から始まる。起動処理は大きく分けて以下の段階を経る：

1. **ランタイム初期化**: Tokio 非同期ランタイムの作成
2. **設定読み込み**: CLI 引数と設定ファイルのマージ
3. **認証処理**: API キーの取得と検証
4. **セッション作成**: ConversationManager と Session の初期化
5. **イベントループ開始**: ユーザー入力の待ち受け

### 1.0.1 main() の構造

`main()` 関数は Rust プログラムのエントリポイントである。Codex では `arg0_dispatch_or_else` というラッパー関数を使用して、同一バイナリで複数の役割を果たせるようにしている。これは「arg0 トリック」と呼ばれるテクニックで、プログラムがどのような名前で呼び出されたかによって動作を変える仕組みである。

**ファイル**: `tui2/src/main.rs:16-31`

```rust
fn main() -> anyhow::Result<()> {
    // arg0_dispatch_or_else は「arg0トリック」を処理するラッパー
    arg0_dispatch_or_else(|codex_linux_sandbox_exe| async move {
        let top_cli = TopCli::parse();  // clap でコマンドライン引数をパース
        let mut inner = top_cli.inner;
        // ...設定のマージ...
        let exit_info = run_main(inner, codex_linux_sandbox_exe).await?;
        // ...終了処理...
        Ok(())
    })
}
```

**ポイント**:

- `arg0_dispatch_or_else` は単一バイナリで複数のCLI機能を提供するための仕組み
- `codex-linux-sandbox` として呼ばれたらサンドボックス処理を実行（Linux のみ）
- 通常呼び出しなら Tokio ランタイムを作成して `run_main()` を実行
- `anyhow::Result<()>` は任意のエラー型を扱える Result 型（エラーハンドリングの簡略化）

### 1.0.2 arg0_dispatch_or_else の役割

この関数は Tokio 非同期ランタイムを初期化し、メインの非同期処理を実行する。Rust の非同期処理はランタイムなしでは動作しないため、この初期化は必須である。Tokio は Rust で最も広く使われている非同期ランタイムで、ネットワーク I/O やファイル操作を効率的に処理できる。

**ファイル**: `arg0/src/lib.rs:86-108`

```rust
pub fn arg0_dispatch_or_else<F, Fut>(main_fn: F) -> anyhow::Result<()>
where
    F: FnOnce(Option<PathBuf>) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    // 1. arg0 ディスパッチ（特殊エイリアス経由の呼び出しをチェック）
    let _path_entry = arg0_dispatch();

    // 2. Tokio ランタイムを作成
    let runtime = tokio::runtime::Runtime::new()?;

    // 3. 非同期エントリポイントを実行
    runtime.block_on(async move {
        let codex_linux_sandbox_exe: Option<PathBuf> = if cfg!(target_os = "linux") {
            std::env::current_exe().ok()
        } else {
            None
        };
        main_fn(codex_linux_sandbox_exe).await
    })
}
```

**処理の流れ**:

1. `arg0_dispatch()` で `.env` ファイル読み込み + PATH 設定
2. Tokio マルチスレッドランタイムを構築（複数のワーカースレッドで並列処理）
3. `block_on()` で非同期処理を同期的に待機し、`run_main` を実行

**Rust 初心者向け解説**:

- `where` 句はジェネリクスの型制約を指定する。ここでは「`F` は引数を1つ取り `Future` を返す関数」という制約
- `cfg!(target_os = "linux")` はコンパイル時条件で、Linux でのみ true になる
- `runtime.block_on()` は非同期処理を同期的に実行する唯一の方法（非同期コンテキスト外で使用）

### 1.0.3 run_main() の処理

`run_main()` は設定の読み込みとアプリケーションの初期化を担当する。設定は複数のソースからマージされる：コマンドライン引数、環境変数、設定ファイル（`~/.codex/config.toml`）の順で優先度が高い。

**ファイル**: `tui2/src/lib.rs:109-353`

```
run_main(cli, codex_linux_sandbox_exe)
  │
  ├─ 1. CLI引数の処理
  │     • sandbox_mode, approval_policy の設定
  │     • --oss フラグの処理（オープンソースモデルの使用）
  │
  ├─ 2. 設定の読み込み
  │     • find_codex_home() → ~/.codex ディレクトリを特定
  │     • load_config_as_toml_with_cli_overrides() → 設定ファイルと CLI をマージ
  │     • load_config_or_exit() → 設定が不正なら終了
  │
  ├─ 3. ログ設定
  │     • tracing_subscriber の初期化（構造化ログ）
  │     • ファイルログ: codex-tui.log に出力
  │
  ├─ 4. OpenTelemetry 初期化（有効な場合）
  │     • 分散トレーシングとメトリクス収集
  │
  └─ 5. run_ratatui_app() 呼び出し → TUI 開始
```

設定ファイルのパスは通常 `~/.codex/config.toml` であり、環境変数 `CODEX_HOME` で変更できる。

### 1.0.4 run_ratatui_app() の処理

`run_ratatui_app()` は TUI（Text User Interface）の初期化と起動を担当する。Ratatui は Rust の TUI ライブラリで、ターミナル上にリッチなインターフェースを描画できる。この関数は「代替画面モード」に切り替えることで、通常のターミナル出力を保護しつつ専用の UI を表示する。

**ファイル**: `tui2/src/lib.rs:355-553`

```
run_ratatui_app(cli, config, overrides, ...)
  │
  ├─ 1. Terminal 初期化
  │     • tui::init() → 代替画面モード（Alternate Screen）に切り替え
  │     • Tui::new(terminal) → Ratatui のターミナルラッパーを作成
  │
  ├─ 2. AuthManager 作成
  │     • AuthManager::shared(codex_home, ...) → API 認証情報を管理
  │     • ChatGPT 認証、OAuth、API キーのいずれかを使用
  │
  ├─ 3. オンボーディング（初回実行時）
  │     • ログイン画面の表示
  │     • ディレクトリ信頼確認（セキュリティ機能）
  │
  ├─ 4. セッション再開の判定
  │     • --resume-session-id: 指定 ID で既存セッションを再開
  │     • --resume-last: 最後のセッションを再開
  │     • --resume-picker: セッション選択 UI を表示
  │     • なし: 新規セッションを開始
  │
  └─ 5. App::run() 呼び出し ← ★ ここで会話ループが開始
```

代替画面モードを使用することで、Codex 終了後にターミナルの元の状態が復元される。これは `vim` や `less` と同じ動作である。

### 1.0.5 App::run() の処理

`App::run()` はメインイベントループを管理する中核関数である。ここで `ConversationManager` が作成され、その中で `SkillsManager` が初期化される。これが Skills 機能のエントリポイントとなる重要な箇所である。

**ファイル**: `tui2/src/app.rs:374-`

```rust
pub async fn run(...) -> Result<AppExitInfo> {
    // 1. イベントチャネル作成
    //    unbounded_channel は容量制限なしの非同期チャネル
    let (app_event_tx, mut app_event_rx) = unbounded_channel();

    // 2. ★ ConversationManager 作成 ★
    //    ここで SkillsManager も初期化される
    let conversation_manager = Arc::new(ConversationManager::new(
        auth_manager.clone(),
        SessionSource::Cli,  // CLI から起動されたことを示す
    ));

    // 3. モデルの取得とマイグレーションチェック
    //    使用するモデル（GPT-4 等）を確定
    let model = conversation_manager
        .get_models_manager()
        .get_model(&config.model, &config)
        .await;

    // 4. ChatWidget 作成（新規 or 再開）
    let chat_widget = match resume_selection {
        ResumeSelection::StartFresh => {
            ChatWidget::new(init, conversation_manager.clone())
        }
        ResumeSelection::Resume(path) => {
            // rollout ファイルから会話を復元
            ChatWidget::new_from_existing(init, resumed.conversation, ...)
        }
    };

    // 5. メインイベントループ
    //    キーボード入力とシステムイベントを処理
    // ...
}
```

**重要ポイント**:

- `Arc<T>` はスレッドセーフな参照カウント型。複数のタスク間で安全にデータを共有できる
- `unbounded_channel` はバックプレッシャーなしのチャネル。イベントが大量に発生しても詰まらない
- `SessionSource::Cli` はセッションの起動元を示し、ログやテレメトリに使用される

### 1.0.6 ConversationManager::new() の処理

`ConversationManager` は複数の会話セッションを管理するコンポーネントである。ここで `SkillsManager` が作成され、システムスキルのインストールも行われる。`SkillsManager::new()` の内部で `install_system_skills()` が呼ばれ、バイナリに埋め込まれた組み込みスキルが `~/.codex/skills/.system/` に展開される。

**ファイル**: `core/src/conversation_manager.rs:53-65`

```rust
impl ConversationManager {
    pub fn new(auth_manager: Arc<AuthManager>, session_source: SessionSource) -> Self {
        // ★ SkillsManager を作成 ★
        // この中で install_system_skills() が呼ばれ、組み込みスキルが展開される
        let skills_manager = Arc::new(SkillsManager::new(
            auth_manager.codex_home().to_path_buf()  // ~/.codex
        ));

        Self {
            // 複数セッションを管理する HashMap（スレッドセーフ）
            conversations: Arc::new(RwLock::new(HashMap::new())),
            auth_manager: auth_manager.clone(),
            session_source,
            // モデル情報を管理（利用可能なモデル一覧、キャッシュ等）
            models_manager: Arc::new(ModelsManager::new(auth_manager)),
            skills_manager,  // ← スキルマネージャーを保持
        }
    }
}
```

**SkillsManager 初期化時の処理**:

1. `install_system_skills()` を呼び出し
2. 埋め込みスキルのフィンガープリント（ハッシュ）を計算
3. 既存のマーカーファイルと比較し、変更があれば再展開
4. 空のキャッシュを初期化

### 1.0.7 ChatWidget と spawn_agent

`spawn_agent()` は TUI と Codex コアの間をつなぐブリッジ関数である。バックグラウンドタスクを起動し、ユーザー操作（Op）を Codex に転送し、Codex からのイベントを UI に戻す双方向の通信を確立する。

この関数が返す `UnboundedSender<Op>` を通じて、UI はユーザー入力を非同期的に送信できる。

**ファイル**: `tui2/src/chatwidget/agent.rs:18-71`

```rust
pub(crate) fn spawn_agent(
    config: Config,
    app_event_tx: AppEventSender,
    server: Arc<ConversationManager>,
) -> UnboundedSender<Op> {
    // UI からの Op を受け取るチャネルを作成
    let (codex_op_tx, mut codex_op_rx) = unbounded_channel::<Op>();

    // バックグラウンドタスクを起動
    tokio::spawn(async move {
        // ★ new_conversation() を呼び出し → Codex::spawn() ★
        // ここで Skills の読み込みも行われる
        let NewConversation {
            conversation_id: _,
            conversation,
            session_configured,
        } = server.new_conversation(config).await?;

        // SessionConfigured イベントを UI に送信
        // これにより UI はセッション準備完了を認識する
        app_event_tx.send(AppEvent::CodexEvent(ev));

        // Op 転送ループ（UI → Codex）
        // 別タスクとして起動し、UI からの操作を Codex に転送
        tokio::spawn(async move {
            while let Some(op) = codex_op_rx.recv().await {
                conversation.submit(op).await;
            }
        });

        // イベントループ（Codex → UI）
        // Codex からのイベント（応答テキスト、ツール呼び出し等）を UI に転送
        while let Ok(event) = conversation.next_event().await {
            app_event_tx.send(AppEvent::CodexEvent(event));
        }
    });

    codex_op_tx  // UI側はこのチャネルで Op を送信
}
```

**データフローの要点**:

- **UI → Codex**: `codex_op_tx.send(Op::UserInput {...})` でユーザー入力を送信
- **Codex → UI**: `app_event_tx.send(AppEvent::CodexEvent(...))` でイベントを受信
- 両者は完全に非同期で動作し、UI のブロッキングを防ぐ

### 1.0.8 完全な起動シーケンス図

```
┌─────────────────────────────────────────────────────────────────────┐
│                        STARTUP SEQUENCE                              │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│   $ codex "hello"                                                   │
│       │                                                              │
│       ▼                                                              │
│   ┌────────────────────────────────────────────────────────────┐    │
│   │ tui2/src/main.rs                                             │    │
│   │   fn main()                                                  │    │
│   │     └─ arg0_dispatch_or_else()                              │    │
│   └──────────────────────────┬─────────────────────────────────┘    │
│                              │                                       │
│                              ▼                                       │
│   ┌────────────────────────────────────────────────────────────┐    │
│   │ arg0/src/lib.rs                                             │    │
│   │   arg0_dispatch_or_else()                                    │    │
│   │     ├─ arg0_dispatch() → .env 読み込み、PATH 設定           │    │
│   │     ├─ tokio::runtime::Runtime::new()                       │    │
│   │     └─ runtime.block_on(run_main())                         │    │
│   └──────────────────────────┬─────────────────────────────────┘    │
│                              │                                       │
│                              ▼                                       │
│   ┌────────────────────────────────────────────────────────────┐    │
│   │ tui2/src/lib.rs                                              │    │
│   │   run_main()                                                 │    │
│   │     ├─ CLI 引数処理                                         │    │
│   │     ├─ 設定読み込み (config.toml, ~/.codex/...)            │    │
│   │     ├─ ログ初期化                                           │    │
│   │     └─ run_ratatui_app()                                    │    │
│   └──────────────────────────┬─────────────────────────────────┘    │
│                              │                                       │
│                              ▼                                       │
│   ┌────────────────────────────────────────────────────────────┐    │
│   │ tui2/src/lib.rs                                              │    │
│   │   run_ratatui_app()                                          │    │
│   │     ├─ Terminal 初期化（代替画面モード）                    │    │
│   │     ├─ AuthManager::shared()                                │    │
│   │     ├─ オンボーディング（初回）                             │    │
│   │     └─ App::run()                                           │    │
│   └──────────────────────────┬─────────────────────────────────┘    │
│                              │                                       │
│                              ▼                                       │
│   ┌────────────────────────────────────────────────────────────┐    │
│   │ tui2/src/app.rs                                              │    │
│   │   App::run()                                                 │    │
│   │     ├─ ★ ConversationManager::new() ★                      │    │
│   │     │     └─ SkillsManager::new() ← システムスキルインストール│   │
│   │     ├─ ChatWidget::new()                                    │    │
│   │     └─ メインイベントループ                                 │    │
│   └──────────────────────────┬─────────────────────────────────┘    │
│                              │                                       │
│                              ▼                                       │
│   ┌────────────────────────────────────────────────────────────┐    │
│   │ tui2/src/chatwidget/agent.rs                                 │    │
│   │   spawn_agent()                                              │    │
│   │     ├─ ★ server.new_conversation() ★                       │    │
│   │     │     └─ Codex::spawn() ← Session, submission_loop 起動 │    │
│   │     ├─ Op 転送ループ（UI → Codex）                          │    │
│   │     └─ イベントループ（Codex → UI）                         │    │
│   └──────────────────────────┬─────────────────────────────────┘    │
│                              │                                       │
│                              ▼                                       │
│   ┌────────────────────────────────────────────────────────────┐    │
│   │ core/src/codex.rs                                           │    │
│   │   Codex::spawn()                                             │    │
│   │     ├─ チャネル作成 (tx_sub, tx_event)                      │    │
│   │     ├─ Skills 読み込み（skills_for_cwd）                    │    │
│   │     ├─ get_user_instructions() ← Skills 一覧を含む          │    │
│   │     ├─ Session::new()                                       │    │
│   │     └─ tokio::spawn(submission_loop)                        │    │
│   └─────────────────────────────────────────────────────────────┘    │
│                                                                      │
│   ════════════════════════════════════════════════════════════════  │
│   ここまでが「起動フェーズ」                                         │
│   以降は「ユーザー入力フェーズ」（次のセクション 1.1 で解説）       │
│   ════════════════════════════════════════════════════════════════  │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 1. ユーザー入力からLLM出力までの完全なフロー

### 1.1 フロー概要図

```
┌─────────────────────────────────────────────────────────────────┐
│ 1. TUI USER INPUT                                               │
│    tui2/src/*.rs                                                 │
└──────────────────────┬──────────────────────────────────────────┘
                       │ Op::UserInput { items }
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│ 2. SUBMISSION CHANNEL                                           │
│    Codex::submit() → tx_sub (async_channel)                     │
└──────────────────────┬──────────────────────────────────────────┘
                       │ Submission { id, op }
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│ 3. SUBMISSION LOOP                                              │
│    submission_loop() → handlers::user_input_or_turn()           │
└──────────────────────┬──────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│ 4. TASK SPAWNING                                                │
│    Session::spawn_task() → tokio::spawn(run_task)               │
└──────────────────────┬──────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│ 5. RUN TASK ← ★ Skills 注入はここ ★                             │
│    run_task() → build_skill_injections() → ターンループ          │
└──────────────────────┬──────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│ 6. PROMPT CONSTRUCTION                                          │
│    run_turn() → Prompt { input, tools, instructions }           │
└──────────────────────┬──────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│ 7. API CLIENT REQUEST                                           │
│    ModelClient::stream() → SSE ストリーミング                    │
└──────────────────────┬──────────────────────────────────────────┘
                       │ SSE Stream
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│ 8. RESPONSE STREAM PROCESSING                                   │
│    try_run_turn() → イベント処理ループ                           │
└──────────────────────┬──────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│ 9. EVENT EMISSION                                               │
│    send_event() → tx_event → TUI/クライアント                    │
└─────────────────────────────────────────────────────────────────┘
```

### 1.2 各ステップの詳細

#### ステップ1: ユーザー入力の受信

**ファイル**: `tui2/src/main.rs`, `protocol/src/protocol.rs`

TUI（Text User Interface）がキーボード入力を受け取り、`Op`（Operation）に変換する。

```rust
// 主要な Op 型（protocol.rs）
pub enum Op {
    UserInput { items: Vec<UserInput> },  // ユーザーからのテキスト入力
    UserTurn { items: Vec<UserInput>, settings: SessionSettingsUpdate },
    Interrupt,                             // 実行中の処理を中断
    ExecApproval { ... },                  // コマンド実行の承認
    PatchApproval { ... },                 // ファイル変更の承認
}
```

#### ステップ2: Submission Channel

**ファイル**: `core/src/codex.rs:306-323`

`Op` は `Submission` にラップされ、非同期チャネル経由でセッションに送信される。

```rust
pub async fn submit(&self, op: Op) -> CodexResult<String> {
    let id = self.next_id.fetch_add(1, Ordering::SeqCst).to_string();
    let sub = Submission { id: id.clone(), op };
    self.tx_sub.send(sub).await.map_err(|_| CodexErr::InternalAgentDied)?;
    Ok(id)
}
```

**チャネル構成**:

| チャネル                | 型                         | 容量   | 用途             |
| ----------------------- | -------------------------- | ------ | ---------------- |
| `tx_sub` / `rx_sub`     | `async_channel::bounded`   | 64     | ユーザー Op 送信 |
| `tx_event` / `rx_event` | `async_channel::unbounded` | 無制限 | イベント配信     |

#### ステップ3: Submission Loop

**ファイル**: `core/src/codex.rs:1578-1672`

バックグラウンドタスクとして動作し、チャネルから `Submission` を受信して処理する。

```rust
async fn submission_loop(sess: Arc<Session>, config: Arc<Config>, rx_sub: Receiver<Submission>) {
    while let Ok(sub) = rx_sub.recv().await {
        match sub.op.clone() {
            Op::UserInput { .. } | Op::UserTurn { .. } => {
                handlers::user_input_or_turn(&sess, sub.id, sub.op, &mut previous_context).await;
            }
            Op::Interrupt => {
                handlers::interrupt(&sess, sub.id, &mut previous_context).await;
            }
            // ... 他の Op 処理 ...
        }
    }
}
```

#### ステップ4: セッションとターンコンテキスト

**ファイル**: `core/src/codex.rs:336-373`, `core/src/state/session.rs`

```rust
// Session: 会話全体を管理
pub(crate) struct Session {
    conversation_id: ConversationId,
    tx_event: Sender<Event>,
    state: Mutex<SessionState>,           // 履歴、設定
    active_turn: Mutex<Option<ActiveTurn>>, // 実行中タスク
    services: SessionServices,            // MCP、Skills 等の共有サービス
}

// TurnContext: 1回のターン（LLMへのリクエスト〜レスポンス）を管理
pub(crate) struct TurnContext {
    client: ModelClient,                  // LLM クライアント
    user_instructions: Option<String>,    // ★ Skills 一覧を含む
    tools_config: ToolsConfig,            // ツール設定
    // ...
}
```

#### ステップ5: タスク生成と実行（★Skills注入ポイント★）

**ファイル**: `core/src/codex.rs:2205-2347`

```rust
pub(crate) async fn run_task(...) -> Option<String> {
    // 1. 自動コンパクションチェック
    if sess.get_total_token_usage().await >= auto_compact_limit {
        run_auto_compact(&sess, &turn_context).await;
    }

    // 2. ★ Skills 注入 ★
    let SkillInjections { items: skill_items, warnings } =
        build_skill_injections(&input, skills_outcome.as_ref()).await;

    // 3. ユーザー入力を履歴に記録
    sess.record_response_item_and_emit_turn_item(&turn_context, response_item).await;

    // 4. スキルコンテンツを履歴に追加
    if !skill_items.is_empty() {
        sess.record_conversation_items(&turn_context, &skill_items).await;
    }

    // 5. ターンループ
    loop {
        match run_turn(sess.clone(), turn_context.clone(), turn_input, ...).await {
            Ok(result) if !result.needs_follow_up => break,
            Ok(_) => continue,  // ツール呼び出し後、次のターンへ
            Err(_) => break,
        }
    }
}
```

#### ステップ6-7: プロンプト構築と API リクエスト

**ファイル**: `core/src/codex.rs:2365-2469`, `core/src/client.rs`

```rust
async fn run_turn(...) -> CodexResult<TurnRunResult> {
    // 1. ツールルーターを構築
    let router = Arc::new(ToolRouter::from_config(&turn_context.tools_config, mcp_tools));

    // 2. プロンプトを構築
    let prompt = Prompt {
        input,                              // 会話履歴（Skills 内容を含む）
        tools: router.specs(),              // 利用可能なツール
        base_instructions_override: ...,
    };

    // 3. LLM にストリーミングリクエスト
    let stream = turn_context.client.clone().stream(&prompt).await??;

    // 4. ストリーム処理
    try_run_turn(router, sess, turn_context, &prompt, stream, ...).await
}
```

#### ステップ8-9: レスポンス処理とイベント送信

**ファイル**: `core/src/codex.rs:2504-2710`

```rust
loop {
    let event = stream.next().or_cancel(&cancellation_token).await?;

    match event {
        ResponseEvent::OutputTextDelta(delta) => {
            // リアルタイム表示用のデルタを送信
            sess.send_event(&turn_context, EventMsg::AgentMessageContentDelta(...)).await;
        }
        ResponseEvent::OutputItemDone(item) => {
            // 出力アイテム完成（ツール呼び出しの場合は実行キューへ）
            if let Some(tool_future) = output.tool_future {
                in_flight.push_back(tool_future);
            }
        }
        ResponseEvent::Completed { token_usage, .. } => {
            sess.update_token_usage_info(&turn_context, token_usage.as_ref()).await;
            break Ok(TurnRunResult { needs_follow_up, last_agent_message });
        }
    }
}
```

### 1.3 シーケンス図

```
User                TUI              Codex           Session          ModelClient        LLM API
  │                  │                 │                │                  │                │
  │ キー入力          │                 │                │                  │                │
  │─────────────────>│                 │                │                  │                │
  │                  │ Op::UserInput   │                │                  │                │
  │                  │────────────────>│                │                  │                │
  │                  │                 │ submit()       │                  │                │
  │                  │                 │───────────────>│                  │                │
  │                  │                 │                │ spawn_task()     │                │
  │                  │                 │                │──────────┐       │                │
  │                  │                 │                │          │       │                │
  │                  │                 │                │ ★Skills注入★    │                │
  │                  │                 │                │<─────────┘       │                │
  │                  │                 │                │                  │                │
  │                  │                 │                │ run_turn()       │                │
  │                  │                 │                │──────────────────>│                │
  │                  │                 │                │                  │ stream()       │
  │                  │                 │                │                  │───────────────>│
  │                  │                 │                │                  │ SSE Stream     │
  │                  │                 │                │                  │<───────────────│
  │                  │                 │                │ ResponseEvent    │                │
  │                  │                 │                │<─────────────────│                │
  │                  │                 │ Event          │                  │                │
  │                  │<────────────────│<───────────────│                  │                │
  │ 表示更新          │                 │                │                  │                │
  │<─────────────────│                 │                │                  │                │
```

---

## 1.4 初期化フロー詳細

Codex が起動してからユーザー入力を受け付けるまでの初期化フローを詳細に解説する。

### 1.4.1 マネージャーの初期化順序

**ファイル**: `conversation_manager.rs:53-133`

```
ConversationManager::new_conversation()
  │
  ├─ AuthManager（認証）
  │    • CodexAuth: 認証モード（ChatGPT, OAuth, ApiKey）
  │    • トークンリフレッシュ: 8時間間隔
  │    • auth_manager.rs:42-100
  │
  ├─ ModelsManager（モデル管理）
  │    • ローカルモデルプリセット読み込み
  │    • キャッシュされたリモートモデル読み込み
  │    • キャッシュ TTL: 300秒（デフォルト）
  │    • models_manager/manager.rs:49-77
  │
  ├─ SkillsManager（スキル管理）
  │    • システムスキルのインストール
  │    • スキル検索ルートの設定
  │    • skills/manager.rs
  │
  └─ Codex::spawn()
       │
       ├─ チャネル作成
       │    • tx_sub/rx_sub: bounded(64) - Submission 用
       │    • tx_event/rx_event: unbounded() - Event 用
       │
       ├─ 設定読み込み
       │    • Skills 読み込み（有効な場合）
       │    • ExecPolicyManager::load()
       │    • get_user_instructions()
       │
       ├─ Session::new()
       │    • SessionState 作成（ContextManager 含む）
       │    • SessionServices 初期化
       │    • MCP 接続マネージャー起動
       │    • SessionConfiguredEvent 送信
       │
       └─ tokio::spawn(submission_loop)
            • メインイベント処理ループ開始
```

### 1.4.2 Session 作成の詳細

**ファイル**: `codex.rs:573-729`

```rust
impl Session {
    pub async fn new(
        session_configuration: SessionConfiguration,
        config: Arc<Config>,
        auth_manager: Arc<AuthManager>,
        models_manager: Arc<ModelsManager>,
        exec_policy: ExecPolicyManager,
        tx_event: Sender<Event>,
        conversation_history: InitialHistory,
        session_source: SessionSource,
        skills_manager: Arc<SkillsManager>,
    ) -> Result<Arc<Self>, SessionError> {
        // 1. 会話履歴の初期化
        let history = match conversation_history {
            InitialHistory::Fresh => ContextManager::new(),
            InitialHistory::Loaded(items) => ContextManager::from_items(items),
        };

        // 2. SessionState 作成
        let state = SessionState {
            session_configuration,
            history,
            latest_rate_limits: None,
        };

        // 3. SessionServices 作成（共有サービス群）
        let services = SessionServices {
            mcp_connection_manager: Arc::new(RwLock::new(McpConnectionManager::new())),
            unified_exec_manager: UnifiedExecSessionManager::new(),
            auth_manager,
            models_manager,
            skills_manager,
            // ... 他のサービス
        };

        // 4. MCP サーバー接続開始（バックグラウンド）
        let mcp_startup = tokio::spawn(async move {
            mcp_connection_manager.initialize(&config).await
        });

        // 5. SessionConfiguredEvent 送信
        tx_event.send(Event {
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent { ... }),
            ...
        }).await?;

        Ok(Arc::new(Session { ... }))
    }
}
```

---

## 1.5 セッションアーキテクチャ詳細

Session、SessionState、TurnContext の関係と役割を詳細に解説する。

### 1.5.1 階層構造

```
┌─────────────────────────────────────────────────────────────────┐
│                          Session                                 │
│  ライフタイム: 会話全体（セッション開始〜終了）                      │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │                     SessionState                            ││
│  │  ライフタイム: 会話全体（可変状態）                           ││
│  │                                                             ││
│  │  • session_configuration: SessionConfiguration              ││
│  │  • history: ContextManager（会話履歴）                       ││
│  │  • latest_rate_limits: Option<RateLimitSnapshot>            ││
│  └─────────────────────────────────────────────────────────────┘│
│                                                                  │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │                   ActiveTurn（実行中ターン）                  ││
│  │  ライフタイム: タスク実行中のみ                               ││
│  │                                                             ││
│  │  • tasks: IndexMap<String, RunningTask>                     ││
│  │  • turn_state: Arc<Mutex<TurnState>>                        ││
│  │      ├─ pending_approvals: HashMap<String, oneshot::Sender> ││
│  │      └─ pending_input: Vec<ResponseInputItem>               ││
│  └─────────────────────────────────────────────────────────────┘│
│                                                                  │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │                   SessionServices（共有サービス）             ││
│  │  ライフタイム: 会話全体（不変参照）                           ││
│  │                                                             ││
│  │  • mcp_connection_manager: Arc<RwLock<McpConnectionManager>>││
│  │  • unified_exec_manager: UnifiedExecSessionManager          ││
│  │  • auth_manager: Arc<AuthManager>                           ││
│  │  • models_manager: Arc<ModelsManager>                       ││
│  │  • skills_manager: Arc<SkillsManager>                       ││
│  │  • tool_approvals: Mutex<ApprovalStore>                     ││
│  │  • rollout: Mutex<Option<RolloutRecorder>>                  ││
│  └─────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                       TurnContext                                │
│  ライフタイム: 1回のターン（不変、ターンごとに新規作成）            │
│                                                                  │
│  • sub_id: String                    （サブミッション ID）        │
│  • client: ModelClient               （LLM クライアント）         │
│  • cwd: PathBuf                      （作業ディレクトリ）         │
│  • developer_instructions: Option<String>                        │
│  • base_instructions: Option<String>                             │
│  • user_instructions: Option<String> （★ Skills 一覧を含む）      │
│  • approval_policy: AskForApproval   （承認ポリシー）             │
│  • sandbox_policy: SandboxPolicy     （サンドボックス設定）       │
│  • tools_config: ToolsConfig         （利用可能ツール）           │
│  • truncation_policy: TruncationPolicy（切り捨てポリシー）        │
└─────────────────────────────────────────────────────────────────┘
```

### 1.5.2 Session の主要フィールド

**ファイル**: `codex.rs:339-349`

```rust
pub(crate) struct Session {
    // 不変フィールド
    conversation_id: ConversationId,      // 会話の一意識別子
    tx_event: Sender<Event>,              // イベント送信チャネル
    features: Features,                   // 有効な機能フラグ

    // 可変状態（Mutex で保護）
    state: Mutex<SessionState>,           // 履歴・設定
    active_turn: Mutex<Option<ActiveTurn>>, // 実行中のターン

    // 共有サービス
    pub(crate) services: SessionServices,

    // 内部 ID 生成
    next_internal_sub_id: AtomicU64,
}
```

### 1.5.3 SessionServices の詳細

**ファイル**: `state/service.rs:17-31`

```rust
pub(crate) struct SessionServices {
    // MCP（Model Context Protocol）
    pub mcp_connection_manager: Arc<RwLock<McpConnectionManager>>,
    pub mcp_startup_cancellation_token: CancellationToken,

    // コマンド実行
    pub unified_exec_manager: UnifiedExecSessionManager,
    pub user_shell: Arc<crate::shell::Shell>,

    // 認証・モデル
    pub auth_manager: Arc<AuthManager>,
    pub models_manager: Arc<ModelsManager>,

    // スキル
    pub skills_manager: Arc<SkillsManager>,

    // ツール承認
    pub tool_approvals: Mutex<ApprovalStore>,

    // 永続化
    pub rollout: Mutex<Option<RolloutRecorder>>,

    // ポリシー
    pub exec_policy: ExecPolicyManager,

    // 監視
    pub otel_manager: OtelManager,

    // 表示設定
    pub show_raw_agent_reasoning: bool,

    // 通知
    pub notifier: UserNotifier,
}
```

---

## 1.6 並行処理モデル詳細

Codex の非同期処理アーキテクチャを詳細に解説する。

### 1.6.1 チャネルアーキテクチャ

```
┌─────────────────────────────────────────────────────────────────┐
│                     チャネル一覧                                  │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  1. Submission Channel（ユーザー入力）                           │
│     ┌────────────┐  async_channel::bounded(64)  ┌────────────┐  │
│     │   Client   │ ──────────────────────────> │ submission │  │
│     │ (submit()) │      Submission { id, op }   │   _loop()  │  │
│     └────────────┘                              └────────────┘  │
│                                                                  │
│  2. Event Channel（イベント配信）                                │
│     ┌────────────┐  async_channel::unbounded()  ┌────────────┐  │
│     │  Session   │ ──────────────────────────> │   Client   │  │
│     │send_event()│      Event { id, msg }       │next_event()│  │
│     └────────────┘                              └────────────┘  │
│                                                                  │
│  3. Approval Channel（ツール承認）                               │
│     ┌────────────┐   oneshot::channel()    ┌────────────────┐  │
│     │ ToolCall   │ ─────────────────────> │ TurnState      │  │
│     │ Execution  │    ReviewDecision       │pending_approvals│ │
│     └────────────┘ <───────────────────── └────────────────┘  │
│                     notify_approval()                            │
│                                                                  │
│  4. Response Stream（API レスポンス）                            │
│     ┌────────────┐   mpsc::channel(1600)   ┌────────────────┐  │
│     │   API      │ ─────────────────────> │ try_run_turn() │  │
│     │  Stream    │     ResponseEvent        │ イベントループ  │  │
│     └────────────┘                          └────────────────┘  │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### 1.6.2 タスクライフサイクル

**ファイル**: `tasks/mod.rs:106-160`

```
Session::spawn_task(turn_context, input, task)
  │
  ├─ 1. 既存タスクを中断
  │     abort_all_tasks(TurnAbortReason::Replaced)
  │       └─ 全 RunningTask の cancellation_token.cancel()
  │
  ├─ 2. キャンセルトークン作成
  │     let cancellation_token = CancellationToken::new()
  │     let child_token = cancellation_token.child_token()
  │
  ├─ 3. 完了通知用 Notify 作成
  │     let done = Arc::new(Notify::new())
  │
  ├─ 4. tokio::spawn() でタスク生成
  │     tokio::spawn(async move {
  │       ├─ task.run(session_ctx, ctx, input, child_token).await
  │       ├─ session_ctx.flush_rollout().await  // 永続化
  │       ├─ if !cancelled {
  │       │    session.on_task_finished(ctx, last_message).await
  │       │  }
  │       └─ done.notify_waiters()  // 完了通知
  │     })
  │
  └─ 5. RunningTask として登録
       register_new_active_task(RunningTask {
         handle: JoinHandle,
         cancellation_token,
         done: Arc<Notify>,
         kind: task.kind(),
       })
```

### 1.6.3 キャンセルパターン

**ファイル**: `tools/parallel.rs:73-90`

```rust
// ツール実行時のキャンセル対応パターン
tokio::select! {
    // キャンセルされた場合
    _ = cancellation_token.cancelled() => {
        // 早期リターン（クリーンアップ済み）
        return Err(ToolCallError::Cancelled);
    }

    // 正常実行
    res = async {
        // 並列実行可能なら読み取りロック、そうでなければ書き込みロック
        let _guard = if supports_parallel {
            Either::Left(lock.read().await)   // 複数同時実行可
        } else {
            Either::Right(lock.write().await) // 排他実行
        };

        // ツール実行
        tool_handler.handle(invocation).await
    } => res
}
```

### 1.6.4 ロック使用パターン

| コンポーネント                            | ロック型     | ファイル      | 用途         | 競合パターン     |
| ----------------------------------------- | ------------ | ------------- | ------------ | ---------------- |
| `Session::state`                          | `Mutex`      | codex.rs:342  | 履歴・設定   | 低（書き込み多） |
| `Session::active_turn`                    | `Mutex`      | codex.rs:346  | 実行中タスク | 低               |
| `SessionServices::mcp_connection_manager` | `RwLock`     | service.rs:18 | MCP サーバー | 高（読み取り多） |
| `SessionServices::tool_approvals`         | `Mutex`      | service.rs:29 | 承認ストア   | 低               |
| `SessionServices::rollout`                | `Mutex`      | service.rs:24 | 永続化       | 低               |
| `ActiveTurn::turn_state`                  | `Arc<Mutex>` | turn.rs:21    | 承認待ち     | 中               |
| `TurnDiffTracker`                         | `Arc<Mutex>` | codex.rs:2260 | ファイル変更 | 低               |

---

## 1.7 履歴管理詳細

会話履歴の管理と Context Window の制御を詳細に解説する。

### 1.7.1 ContextManager

**ファイル**: `context_manager/history.rs:18-250`

```rust
pub(crate) struct ContextManager {
    items: Vec<ResponseItem>,        // 履歴（古い順）
    token_info: Option<TokenUsageInfo>,  // トークン使用量
}

impl ContextManager {
    // 履歴への記録（切り捨てポリシー適用）
    pub fn record_items<'a>(
        &mut self,
        items: impl Iterator<Item = &'a ResponseItem>,
        policy: TruncationPolicy,
    ) {
        for item in items {
            let truncated = policy.truncate(item);
            self.items.push(truncated);
        }
    }

    // プロンプト用履歴取得（GhostSnapshot を除外）
    pub fn get_history_for_prompt(&self) -> Vec<ResponseItem> {
        self.items
            .iter()
            .filter(|item| !matches!(item, ResponseItem::GhostSnapshot(_)))
            .cloned()
            .collect()
    }

    // トークン数推定
    pub fn estimate_token_count(&self, turn_context: &TurnContext) -> i64 {
        let base_tokens = estimate_base_tokens(turn_context);
        let item_tokens: i64 = self.items.iter().map(|item| {
            match item {
                ResponseItem::GhostSnapshot(_) => 0,  // カウントしない
                ResponseItem::Reasoning { content } => {
                    estimate_reasoning_length(content.len())
                }
                _ => {
                    let serialized = serde_json::to_string(item).unwrap_or_default();
                    approx_token_count(serialized.len())
                }
            }
        }).sum();
        base_tokens + item_tokens
    }
}
```

### 1.7.2 切り捨てポリシー

**ファイル**: `truncate.rs:14-96`

```rust
pub enum TruncationPolicy {
    Bytes(usize),   // 最大バイト数
    Tokens(usize),  // 最大トークン数
}

impl TruncationPolicy {
    // トークン数からバイト数に変換（4 bytes ≈ 1 token）
    pub fn to_bytes(&self) -> usize {
        match self {
            Self::Bytes(b) => *b,
            Self::Tokens(t) => t * 4,
        }
    }

    // ResponseItem を切り捨て
    pub fn truncate(&self, item: &ResponseItem) -> ResponseItem {
        match item {
            ResponseItem::FunctionCallOutput { output, .. } => {
                let max_bytes = self.to_bytes();
                if output.len() > max_bytes {
                    let truncated = &output[..max_bytes];
                    ResponseItem::FunctionCallOutput {
                        output: format!("{truncated}\n[truncated]"),
                        ..item.clone()
                    }
                } else {
                    item.clone()
                }
            }
            _ => item.clone(),
        }
    }
}
```

### 1.7.3 自動コンパクション

**ファイル**: `codex.rs:2215-2223`

```rust
// トークン使用量がモデルの制限に達したら自動コンパクション
let auto_compact_limit = turn_context
    .client
    .get_model_family()
    .auto_compact_token_limit()
    .unwrap_or(i64::MAX);

let total_usage_tokens = sess.get_total_token_usage().await;
if total_usage_tokens >= auto_compact_limit {
    run_auto_compact(&sess, &turn_context).await;
}
```

---

## 1.8 ツール実行詳細

ツール呼び出しの処理フローを詳細に解説する。

### 1.8.1 ToolRouter アーキテクチャ

**ファイル**: `tools/router.rs:21-151`

```
┌─────────────────────────────────────────────────────────────────┐
│                      ToolRouter                                  │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │                   ToolRegistry                              ││
│  │                                                             ││
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ ││
│  │  │  Function   │  │    MCP      │  │    LocalShell       │ ││
│  │  │  Handlers   │  │  Handlers   │  │    Handler          │ ││
│  │  │             │  │             │  │                     │ ││
│  │  │ • read_file │  │ • server1__ │  │ • shell             │ ││
│  │  │ • write_file│  │   tool1     │  │ • python            │ ││
│  │  │ • bash      │  │ • server2__ │  │                     │ ││
│  │  │ • ...       │  │   tool2     │  │                     │ ││
│  │  └─────────────┘  └─────────────┘  └─────────────────────┘ ││
│  └─────────────────────────────────────────────────────────────┘│
│                                                                  │
│  dispatch_tool_call(tool_name, call_id, payload)                │
│    │                                                             │
│    ├─ 1. ToolCall を作成                                         │
│    ├─ 2. ToolInvocation を作成（TurnDiffTracker 付き）           │
│    ├─ 3. ToolRegistry からハンドラを検索                         │
│    └─ 4. handler.handle(invocation) を実行                       │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### 1.8.2 ツール呼び出しフロー

**ファイル**: `codex.rs:2504-2600`

```rust
// try_run_turn 内のツール実行ループ
loop {
    match stream.next().await {
        ResponseEvent::OutputItemDone(item) => {
            match item {
                OutputItem::FunctionCall { name, call_id, arguments } => {
                    // 1. ツール呼び出しをビルド
                    let tool_call = router.build_tool_call(
                        &name,
                        &call_id,
                        arguments,
                    )?;

                    // 2. ToolInvocation を作成
                    let invocation = ToolInvocation {
                        tool_call,
                        turn_context: Arc::clone(&turn_context),
                        turn_diff_tracker: Arc::clone(&turn_diff_tracker),
                        session: Arc::clone(&sess),
                    };

                    // 3. 非同期でツールを実行（キューに追加）
                    let future = router.dispatch_tool_call(invocation);
                    in_flight.push_back(future);

                    needs_follow_up = true;  // 次のターンが必要
                }
                OutputItem::Message { content } => {
                    last_agent_message = Some(content);
                }
            }
        }
        // ...
    }
}

// ツール実行結果を収集
while let Some(result) = in_flight.next().await {
    match result {
        Ok(output) => {
            sess.record_conversation_items(&turn_context, &[output]).await;
        }
        Err(e) => {
            sess.send_event(&turn_context, EventMsg::Error(e)).await;
        }
    }
}
```

### 1.8.3 MCP ツール統合

**ファイル**: `mcp_connection_manager.rs:1-250`

```rust
// MCP ツール名のフォーマット
// 形式: mcp__<server>__<tool>
// 最大長: 64文字
// ハッシュ衝突時: SHA1 サフィックス追加

impl McpConnectionManager {
    // 全サーバーからツールを収集
    pub async fn list_all_tools(&self) -> Result<Vec<ToolSpec>> {
        let mut tools = Vec::new();
        for (server_name, connection) in &self.connections {
            for tool in connection.list_tools().await? {
                let qualified_name = format!("mcp__{}__{}", server_name, tool.name);
                tools.push(ToolSpec {
                    name: qualified_name,
                    description: tool.description,
                    parameters: tool.input_schema,
                });
            }
        }
        Ok(tools)
    }

    // ツール実行
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value> {
        let connection = self.connections.get(server)?;
        connection.call_tool(tool, arguments).await
    }
}
```

---

## 1.9 イベントシステム詳細

イベントの種類と流れを詳細に解説する。

### 1.9.1 EventMsg の種類

| カテゴリ           | イベント型                   | 説明                   |
| ------------------ | ---------------------------- | ---------------------- |
| **セッション**     | `SessionConfigured`          | セッション準備完了     |
| **タスク**         | `TaskStarted`                | タスク開始             |
|                    | `TaskComplete`               | タスク完了             |
| **アイテム**       | `ItemStarted`                | メッセージ/ツール開始  |
|                    | `ItemCompleted`              | メッセージ/ツール完了  |
| **ストリーミング** | `AgentMessageContentDelta`   | トークン単位のテキスト |
|                    | `AgentReasoningContentDelta` | 推論内容のデルタ       |
| **コマンド実行**   | `ExecCommandBegin`           | シェルコマンド開始     |
|                    | `ExecCommandEnd`             | シェルコマンド終了     |
| **ファイル操作**   | `PatchApplyBegin`            | パッチ適用開始         |
|                    | `PatchApplyEnd`              | パッチ適用終了         |
| **承認要求**       | `ExecApprovalRequest`        | コマンド実行承認要求   |
|                    | `ApplyPatchApprovalRequest`  | パッチ適用承認要求     |
| **メトリクス**     | `TokenCount`                 | トークン使用量         |
|                    | `RateLimitSnapshot`          | レート制限状態         |
| **エラー**         | `Error`                      | 致命的エラー           |
|                    | `StreamError`                | ストリーム切断         |
|                    | `Warning`                    | 警告                   |
| **その他**         | `TurnDiff`                   | ファイル変更追跡       |
|                    | `BackgroundEvent`            | バックグラウンド処理   |

### 1.9.2 イベントフロー

**ファイル**: `codex.rs:961-986`

```rust
// Session からイベントを送信
impl Session {
    pub(crate) async fn send_event(&self, turn_context: &TurnContext, msg: EventMsg) {
        let event = Event {
            id: turn_context.sub_id.clone(),
            msg: msg.clone(),
            conversation_id: self.conversation_id,
        };
        self.send_event_raw(event).await;
    }

    pub(crate) async fn send_event_raw(&self, event: Event) {
        // 1. Rollout に永続化
        let rollout_items = vec![RolloutItem::EventMsg(event.msg.clone())];
        self.persist_rollout_items(&rollout_items).await;

        // 2. チャネル経由でクライアントに送信
        if let Err(e) = self.tx_event.send(event).await {
            error!("failed to send event: {e}");
        }
    }
}

// クライアント側での受信
impl Codex {
    pub async fn next_event(&self) -> CodexResult<Event> {
        self.rx_event.recv().await.map_err(|_| CodexErr::InternalAgentDied)
    }
}
```

### 1.9.3 Rollout（永続化）

**永続化タイミング**:

| タイミング     | 内容                | ファイル:行   |
| -------------- | ------------------- | ------------- |
| セッション開始 | `SessionConfigured` | codex.rs:686  |
| 履歴記録       | 全 `ResponseItem`   | codex.rs:1173 |
| イベント送信   | 全 `EventMsg`       | codex.rs:981  |
| コンパクション | 圧縮記録            | codex.rs:2349 |

```rust
// Rollout フラッシュパターン
impl Session {
    pub(crate) async fn flush_rollout(&self) {
        let recorder = {
            let guard = self.services.rollout.lock().await;
            guard.clone()
        };
        if let Some(rec) = recorder {
            if let Err(e) = rec.flush().await {
                error!("failed to flush rollout: {e:#}");
            }
        }
    }
}
```

---

## 1.10 全体アーキテクチャ図

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Codex Architecture                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌──────────────┐                                                           │
│  │     TUI      │  キーボード入力を Op に変換                                │
│  └──────┬───────┘                                                           │
│         │ Op::UserInput { items }                                           │
│         ▼                                                                    │
│  ┌──────────────┐  submit()   ┌──────────────┐                              │
│  │    Codex     │ ──────────> │  tx_sub      │  async_channel::bounded(64)  │
│  │              │             │  (Sender)    │                              │
│  └──────────────┘             └──────┬───────┘                              │
│                                      │                                       │
│                                      ▼                                       │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                        submission_loop (tokio::spawn)                  │  │
│  │                                                                        │  │
│  │   while let Ok(sub) = rx_sub.recv().await {                           │  │
│  │       match sub.op {                                                   │  │
│  │           UserInput/UserTurn => spawn_task(RegularTask)               │  │
│  │           Interrupt => abort_all_tasks()                              │  │
│  │           ExecApproval/PatchApproval => notify_approval()             │  │
│  │           Compact => spawn_task(CompactTask)                          │  │
│  │           ...                                                          │  │
│  │       }                                                                │  │
│  │   }                                                                    │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                                      │                                       │
│                                      ▼                                       │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                              Session                                   │  │
│  │                                                                        │  │
│  │   ┌─────────────────────────┐  ┌─────────────────────────────────────┐│  │
│  │   │     SessionState        │  │        SessionServices              ││  │
│  │   │                         │  │                                     ││  │
│  │   │ • session_configuration │  │ • mcp_connection_manager (RwLock)   ││  │
│  │   │ • history (ContextMgr)  │  │ • unified_exec_manager              ││  │
│  │   │ • rate_limits           │  │ • auth_manager                      ││  │
│  │   │                         │  │ • models_manager                    ││  │
│  │   │   [Mutex protected]     │  │ • skills_manager                    ││  │
│  │   └─────────────────────────┘  │ • tool_approvals (Mutex)            ││  │
│  │                                 │ • rollout (Mutex)                   ││  │
│  │   ┌─────────────────────────┐  └─────────────────────────────────────┘│  │
│  │   │      ActiveTurn         │                                         │  │
│  │   │                         │                                         │  │
│  │   │ • tasks: IndexMap       │                                         │  │
│  │   │ • turn_state:           │                                         │  │
│  │   │   - pending_approvals   │                                         │  │
│  │   │   - pending_input       │                                         │  │
│  │   │                         │                                         │  │
│  │   │   [Mutex protected]     │                                         │  │
│  │   └─────────────────────────┘                                         │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                                      │                                       │
│                                      ▼                                       │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                     SessionTask (tokio::spawn)                         │  │
│  │                                                                        │  │
│  │   run_task(session, turn_context, input, cancellation_token)          │  │
│  │       │                                                                │  │
│  │       ├─ build_skill_injections()  ← ★ Skills 注入                    │  │
│  │       ├─ record_conversation_items()                                   │  │
│  │       │                                                                │  │
│  │       └─ loop { run_turn() }                                           │  │
│  │              │                                                         │  │
│  │              ├─ ToolRouter::from_config()                              │  │
│  │              ├─ Prompt { input, tools, instructions }                  │  │
│  │              ├─ ModelClient::stream()  ──────────────────────────┐    │  │
│  │              │                                                   │    │  │
│  │              └─ try_run_turn()                                   │    │  │
│  │                    │                                             │    │  │
│  │                    ├─ ResponseEvent::OutputTextDelta             │    │  │
│  │                    │    └─ send_event(AgentMessageContentDelta)  │    │  │
│  │                    │                                             │    │  │
│  │                    ├─ ResponseEvent::OutputItemDone              │    │  │
│  │                    │    └─ dispatch_tool_call() ──┐              │    │  │
│  │                    │                              │              │    │  │
│  │                    └─ ResponseEvent::Completed    │              │    │  │
│  │                         └─ update_token_usage()   │              │    │  │
│  │                                                   │              │    │  │
│  └───────────────────────────────────────────────────│──────────────│────┘  │
│                                                      │              │        │
│                              ┌───────────────────────┘              │        │
│                              ▼                                      ▼        │
│  ┌───────────────────────────────────────┐  ┌────────────────────────────┐  │
│  │           ToolRouter                   │  │       ModelClient          │  │
│  │                                        │  │                            │  │
│  │   ┌────────────────────────────────┐  │  │   stream_responses_api()   │  │
│  │   │        ToolRegistry            │  │  │       │                    │  │
│  │   │                                │  │  │       ▼                    │  │
│  │   │  • Function handlers           │  │  │   ┌────────────────────┐  │  │
│  │   │  • MCP handlers                │  │  │   │  LLM API           │  │  │
│  │   │  • LocalShell handler          │  │  │   │  (SSE Streaming)   │  │  │
│  │   └────────────────────────────────┘  │  │   └────────────────────┘  │  │
│  │                                        │  │                            │  │
│  │   dispatch_tool_call()                 │  │   ResponseStream           │  │
│  │       │                                │  │   mpsc::channel(1600)      │  │
│  │       ├─ 承認チェック                  │  │                            │  │
│  │       ├─ サンドボックス実行            │  │                            │  │
│  │       └─ 結果を ResponseItem に        │  │                            │  │
│  └───────────────────────────────────────┘  └────────────────────────────┘  │
│                                                                              │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                          Event Flow                                    │  │
│  │                                                                        │  │
│  │   Session::send_event()                                                │  │
│  │       │                                                                │  │
│  │       ├─ persist_rollout_items()  → Rollout (disk)                    │  │
│  │       │                                                                │  │
│  │       └─ tx_event.send()  → async_channel::unbounded()                │  │
│  │                                    │                                   │  │
│  │                                    ▼                                   │  │
│  │                             ┌──────────────┐                           │  │
│  │                             │   Codex      │                           │  │
│  │                             │ next_event() │                           │  │
│  │                             └──────┬───────┘                           │  │
│  │                                    │                                   │  │
│  │                                    ▼                                   │  │
│  │                             ┌──────────────┐                           │  │
│  │                             │     TUI      │  画面更新                 │  │
│  │                             └──────────────┘                           │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

# Part 2: Skills機能の詳細

## 2. Skills機能とは

**スキルは Codex の能力を拡張するモジュール化されたパッケージ**である。AI エージェントに対する「オンボーディングガイド」として機能し、以下を提供する：

1. **専門的なワークフロー** - 特定ドメイン向けの多段階手順
2. **ツール統合** - 特定のファイル形式や API の使用方法
3. **ドメイン専門知識** - 企業固有の知識、スキーマ、ビジネスロジック
4. **バンドルリソース** - スクリプト、参照資料、複雑なタスク用アセット

## 3. Skills が使われるタイミング

Skills は **2つのフェーズ** で処理される。

### フェーズ1: 起動時（スキル一覧の注入）

```
ConversationManager::new()
    → SkillsManager::new()          ← システムスキルのインストール
    → Codex::spawn()
        → skills_manager.skills_for_cwd()  ← スキル読み込み
        → get_user_instructions()
            → render_skills_section()  ← "## Skills\n- name: desc..." 生成
    → SessionConfiguration.user_instructions に格納
```

**結果**: モデルは「どんなスキルが利用可能か」を認識する。

### フェーズ2: ユーザー入力時（スキル内容の注入）

```
run_task()
    → skills_manager.skills_for_cwd()  ← キャッシュから取得
    → build_skill_injections()
        → collect_explicit_skill_mentions()  ← "$skill-name" を抽出
        → tokio::fs::read_to_string(skill.path)  ← SKILL.md 読み込み
    → sess.record_conversation_items(&skill_items)  ← 履歴に追加
```

**結果**: モデルはスキルの詳細な指示を参照して応答を生成する。

### コード位置まとめ

| 処理                     | ファイル                  | 行番号    |
| ------------------------ | ------------------------- | --------- |
| SkillsManager 初期化     | `conversation_manager.rs` | 54-55     |
| スキル読み込み           | `codex.rs`                | 220-233   |
| システムプロンプト構築   | `codex.rs`                | 235-241   |
| スキル一覧レンダリング   | `project_doc.rs`          | 35-39     |
| タスク実行時のスキル注入 | `codex.rs`                | 2229-2253 |
| スキルコンテンツ読み込み | `skills/injection.rs`     | 56-114    |

---

## 4. Skills モジュールの構造

```
codex-rs/core/src/skills/
├── mod.rs          # モジュール定義とパブリック API（エントリポイント）
├── model.rs        # データ構造定義（SkillMetadata, SkillError 等）
├── loader.rs       # スキル検出・解析（最大のファイル、1129行）
├── manager.rs      # キャッシング管理（SkillsManager）
├── injection.rs    # プロンプトへの注入処理
├── render.rs       # マークダウン生成
└── system.rs       # システムスキルのインストール
    └── assets/samples/  # 組み込みスキル
        ├── skill-creator/SKILL.md
        └── skill-installer/SKILL.md
```

### 読む順序の推奨

1. **`model.rs`** - データ構造を理解（`SkillMetadata`, `SkillScope`）
2. **`manager.rs`** - エントリポイント（`skills_for_cwd()`）
3. **`loader.rs`** - スキル検出の詳細（`load_skills_from_roots()`, `parse_skill_file()`）
4. **`injection.rs`** - プロンプト注入（`build_skill_injections()`）
5. **`render.rs`** - マークダウン生成（`render_skills_section()`）
6. **`system.rs`** - システムスキル（`install_system_skills()`）

---

## 5. データモデル

Skills 機能は、データの流れと状態を明確に管理するために、いくつかの重要なデータ構造を定義している。これらの構造体は `skills/model.rs` にまとめられており、スキルのメタデータ、スコープ（優先度）、読み込み結果を表現する。

Rust では構造体（struct）と列挙型（enum）を組み合わせてデータモデルを構築する。Python のデータクラスや TypeScript のインターフェースに相当するが、パターンマッチングと組み合わせることでより堅牢なコードが書ける。

### 5.1 SkillMetadata

**概要**: スキルの基本情報を保持する構造体。YAML フロントマターから抽出された情報がここに格納される。

- `name`: スキルの識別名（例: "pdf-editor"）。ユーザーが `$pdf-editor` のように入力する際に使用される
- `description`: スキルの説明。LLM がスキルを選択する際のトリガー条件として機能する
- `short_description`: TUI のポップアップに表示される短い説明（オプション）
- `path`: SKILL.md ファイルへの絶対パス。スキル内容を読み込む際に使用
- `scope`: スキルの優先度レベル（Repo > User > System > Admin）

**ファイル**: `model.rs:6-12`

```rust
/// スキルのメタデータ（YAML フロントマターから抽出）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: String,                      // スキル名（最大64文字）
    pub description: String,               // 説明（最大1024文字）
    pub short_description: Option<String>, // TUI 表示用の短い説明
    pub path: PathBuf,                     // SKILL.md への絶対パス
    pub scope: SkillScope,                 // スコープ（優先度）
}
```

### 5.2 SkillScope（優先度順）

**概要**: スキルの検索場所と優先度を表す列挙型。同名のスキルが複数存在する場合、より高い優先度（上にあるもの）が採用される。

この設計により：

- **プロジェクト固有のスキル**（Repo）が汎用スキルを上書きできる
- **ユーザー定義のスキル**（User）が組み込みスキルをカスタマイズできる
- **システム管理者**（Admin）が組織全体のポリシーを設定できる

例えば、`pdf-editor` という名前のスキルがリポジトリとユーザーディレクトリの両方にある場合、リポジトリ版が使用される。

**ファイル**: `protocol/src/protocol.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SkillScope {
    Repo,    // リポジトリ固有（.codex/skills/）- 最優先
    User,    // ユーザー定義（~/.codex/skills/）
    System,  // 組み込み（~/.codex/skills/.system/）
    Admin,   // 管理者（/etc/codex/skills/）- 最低優先
}
```

### 5.3 SkillLoadOutcome

**概要**: スキル読み込み処理の結果を表す構造体。成功と失敗の両方を保持することで、一部のスキルで問題があっても他のスキルは使用できる（フォールトトレラントな設計）。

`Default` トレイトを実装しているため、`SkillLoadOutcome::default()` で空の結果を作成できる。これは「スキルなし」の状態を表現するのに便利。

**ファイル**: `model.rs`

```rust
/// スキル読み込みの結果
#[derive(Debug, Clone, Default)]
pub struct SkillLoadOutcome {
    pub skills: Vec<SkillMetadata>,  // 読み込み成功したスキル
    pub errors: Vec<SkillError>,     // 読み込み失敗したスキル
}
```

---

## 6. コードウォークスルー: スキル読み込み

**ファイル**: `loader.rs`（1129行、最大のファイル）

このセクションでは、スキルがディスクからどのように読み込まれるかを詳細に解説する。処理は4つの段階に分かれている：

1. **検索ルートの決定**: どのディレクトリからスキルを探すか
2. **ディレクトリ探索**: BFS（幅優先探索）で SKILL.md ファイルを発見
3. **ファイルのパース**: YAML フロントマターを解析して `SkillMetadata` を生成
4. **重複排除**: 同名スキルは高優先度のものだけを採用

この設計のポイントは、**fail-open** の原則に従っていることである。つまり、一部のスキルで読み込みエラーが発生しても、他のスキルは正常に使用できる。エラーは `SkillLoadOutcome.errors` に記録され、ユーザーに通知される。

### 6.1 検索ルートの決定

**概要**: 作業ディレクトリ（cwd）に基づいて、スキルを検索するディレクトリの一覧を生成する。検索順序は優先度順（Repo → User → System → Admin）になっている。

リポジトリスキルは、Git リポジトリのルートにある `.codex/skills/` ディレクトリから読み込まれる。これにより、プロジェクト固有のスキルをバージョン管理できる。

**関数**: `skill_roots_for_cwd()` (loader.rs:100-130)

```rust
pub(crate) fn skill_roots_for_cwd(codex_home: &Path, cwd: &Path) -> Vec<SkillRoot> {
    let mut roots = Vec::new();

    // 1. リポジトリスキル（最優先）
    if let Some(repo_root) = repo_skills_root(cwd) {
        roots.push(repo_root);  // {repo}/.codex/skills/
    }

    // 2. ユーザースキル
    roots.push(user_skills_root(codex_home));  // ~/.codex/skills/

    // 3. システムスキル
    roots.push(system_skills_root(codex_home));  // ~/.codex/skills/.system/

    // 4. 管理者スキル（UNIX のみ）
    if cfg!(unix) {
        roots.push(admin_skills_root());  // /etc/codex/skills/
    }

    roots
}
```

### 6.2 ディレクトリ探索（BFS）

**概要**: 指定されたルートディレクトリ以下を BFS（幅優先探索）で走査し、`SKILL.md` ファイルを発見する。

DFS（深さ優先探索）ではなく BFS を使用する理由は、スキルディレクトリの構造が浅いことが多いため。また、VecDeque を使った BFS は実装がシンプルで、メモリ効率も良い。

セキュリティ上の考慮として、以下のエントリはスキップされる：

- **隠しファイル/ディレクトリ**（`.` で始まるもの）: 設定ファイルや内部状態を誤って読み込まないため
- **シンボリックリンク**: シンボリックリンク攻撃を防ぐため

**関数**: `discover_skills_under_root()` (loader.rs:200-280)

```rust
fn discover_skills_under_root(root: &SkillRoot) -> Vec<Result<SkillMetadata, SkillError>> {
    let mut results = Vec::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();  // BFS キュー
    queue.push_back(root.path.clone());

    while let Some(dir) = queue.pop_front() {
        for entry in fs::read_dir(&dir)? {
            let path = entry.path();

            // 隠しファイル・シンボリックリンクはスキップ
            if is_hidden(&path) || path.is_symlink() {
                continue;
            }

            if path.is_dir() {
                queue.push_back(path);  // サブディレクトリをキューに追加
            } else if path.file_name() == Some(OsStr::new("SKILL.md")) {
                // SKILL.md を発見 → パース
                results.push(parse_skill_file(&path, root.scope));
            }
        }
    }

    results
}
```

### 6.3 スキルファイルのパース

**概要**: 発見された `SKILL.md` ファイルを読み込み、YAML フロントマターを解析して `SkillMetadata` を生成する。

YAML フロントマターは、Markdown ファイルの先頭に `---` で囲まれた YAML ブロックを配置する形式。Jekyll や Hugo などの静的サイトジェネレーターで広く使われている慣習である。

パース処理の流れ：

1. ファイル内容を読み込み
2. `---` で囲まれた部分を抽出
3. `serde_yaml` でデシリアライズ
4. フィールドのバリデーション（長さ制限チェック）
5. `SkillMetadata` を構築

バリデーションにより、悪意のある長大なスキル名や説明文がシステムに悪影響を与えることを防いでいる。

**関数**: `parse_skill_file()` (loader.rs:300-380)

```rust
fn parse_skill_file(path: &Path, scope: SkillScope) -> Result<SkillMetadata, SkillParseError> {
    let contents = fs::read_to_string(path)?;

    // YAML フロントマターを抽出（--- で囲まれた部分）
    let frontmatter = extract_frontmatter(&contents)?;

    // serde_yaml でデシリアライズ
    let parsed: SkillFrontmatter = serde_yaml::from_str(&frontmatter)?;

    // バリデーション
    validate_field(&parsed.name, MAX_NAME_LEN, "name")?;           // 64文字以下
    validate_field(&parsed.description, MAX_DESCRIPTION_LEN, "description")?;  // 1024文字以下

    Ok(SkillMetadata {
        name: parsed.name,
        description: parsed.description,
        short_description: parsed.metadata.and_then(|m| m.short_description),
        path: path.to_path_buf(),
        scope,
    })
}
```

### 6.4 重複排除

**概要**: 複数のディレクトリから読み込まれたスキルを統合し、同名のスキルは最初に読み込まれたもの（高優先度）を採用する。

この処理がスコープ優先度を実現する核心部分である。`roots` は優先度順（Repo → User → System → Admin）で渡されるため、`HashSet` を使った重複チェックで自然と高優先度のスキルが残る。

`retain` メソッドは、クロージャが `true` を返す要素だけを残すフィルタ処理。`seen.insert()` は新規追加時に `true` を返すため、初めて見たスキル名だけが残る仕組みになっている。

**関数**: `load_skills_from_roots()` (loader.rs:150-190)

```rust
pub fn load_skills_from_roots(roots: Vec<SkillRoot>) -> SkillLoadOutcome {
    let mut outcome = SkillLoadOutcome::default();

    for root in roots {
        for result in discover_skills_under_root(&root) {
            match result {
                Ok(skill) => outcome.skills.push(skill),
                Err(e) => outcome.errors.push(e.into()),
            }
        }
    }

    // 同名スキルは最初に読み込まれたもの（高優先度）を採用
    let mut seen: HashSet<String> = HashSet::new();
    outcome.skills.retain(|skill| seen.insert(skill.name.clone()));

    outcome
}
```

---

## 7. コードウォークスルー: キャッシング

**ファイル**: `manager.rs`（118行）

スキルの読み込みはファイルシステム操作を伴うため、比較的コストが高い処理である。`SkillsManager` はこれをキャッシュすることで、同じ cwd（作業ディレクトリ）に対する複数回のリクエストを効率化する。

キャッシュ設計のポイント：

- **cwd をキーとしたキャッシュ**: 同じディレクトリで作業している間は再読み込み不要
- **RwLock による並行アクセス対応**: 複数のスレッドが同時にスキルを参照できる
- **強制リロードオプション**: ユーザーが明示的に再読み込みを要求できる

### 7.1 SkillsManager 構造体

**概要**: スキルのライフサイクル全体を管理するマネージャー。`ConversationManager::new()` で作成され、セッション全体で共有される。

`RwLock<HashMap<...>>` は「読み取り/書き込みロック付きのハッシュマップ」。複数のスレッドが同時に読み取りできるが、書き込みは排他的（一度に1スレッドのみ）。

```rust
pub struct SkillsManager {
    codex_home: PathBuf,
    // cwd をキーとしたスキル読み込み結果のキャッシュ
    cache_by_cwd: RwLock<HashMap<PathBuf, SkillLoadOutcome>>,
}
```

### 7.2 初期化

**概要**: `SkillsManager` の作成時に、システムスキル（バイナリに埋め込まれたスキル）を `~/.codex/skills/.system/` に展開する。

システムスキルのインストールでエラーが発生しても、`SkillsManager` 自体の作成は続行される。これは「スキル機能が使えなくてもアプリケーション全体は動作する」というフォールトトレラントな設計。

**関数**: `new()` (manager.rs:43-65)

```rust
pub fn new(codex_home: PathBuf) -> Self {
    // システムスキル（バイナリ埋め込み）をインストール
    if let Err(err) = install_system_skills(&codex_home) {
        tracing::error!("failed to install system skills: {err}");
    }

    Self {
        codex_home,
        cache_by_cwd: RwLock::new(HashMap::new()),
    }
}
```

### 7.3 スキル取得（キャッシュ付き）

**概要**: 指定された cwd に対するスキルを取得する。キャッシュにあればそれを返し、なければ読み込んでキャッシュに保存する。

キャッシュの読み取りと書き込みで別々のロック（`.read()` と `.write()`）を使用している点に注目。読み取りは並行して実行できるが、書き込みは排他的。これにより、複数のスレッドが同時にスキルを参照しても効率的に動作する。

`PoisonError`（ロックを保持したスレッドがパニックした場合のエラー）の処理として、`err.into_inner()` でロックの中身を強制的に取得している。これは「ロックが汚染されていても処理を続ける」という決断で、スキル読み込みの失敗がアプリケーション全体をクラッシュさせないための配慮。

**関数**: `skills_for_cwd_with_options()` (manager.rs:75-116)

```rust
pub fn skills_for_cwd_with_options(&self, cwd: &Path, force_reload: bool) -> SkillLoadOutcome {
    // 1. キャッシュをチェック
    let cached = match self.cache_by_cwd.read() {
        Ok(cache) => cache.get(cwd).cloned(),
        Err(err) => err.into_inner().get(cwd).cloned(),
    };

    // 2. キャッシュヒット && 強制リロードでなければキャッシュを返す
    if !force_reload && let Some(outcome) = cached {
        return outcome;
    }

    // 3. キャッシュミス → 読み込み
    let roots = skill_roots_for_cwd(&self.codex_home, cwd);
    let outcome = load_skills_from_roots(roots);

    // 4. キャッシュに保存
    match self.cache_by_cwd.write() {
        Ok(mut cache) => { cache.insert(cwd.to_path_buf(), outcome.clone()); }
        Err(err) => { err.into_inner().insert(cwd.to_path_buf(), outcome.clone()); }
    }

    outcome
}
```

---

## 8. コードウォークスルー: プロンプト注入

**ファイル**: `injection.rs`（148行）

このセクションでは、ユーザーが選択したスキルの内容（SKILL.md）を読み込み、LLM に渡す形式に変換する処理を解説する。

**なぜ注入が必要か？**
起動時には「どんなスキルがあるか」（メタデータ）だけをモデルに伝える。ユーザーがスキルを選択して初めて、そのスキルの詳細な指示（SKILL.md の全文）を読み込む。これにより：

- トークン使用量を抑える（使わないスキルの指示は読み込まない）
- 最新のスキル内容を取得できる（ファイルが更新されていれば反映される）

### 8.1 SkillInjections 構造体

**概要**: スキル注入処理の結果を保持する構造体。成功したスキル（ResponseItem として）と、失敗した警告メッセージを分けて管理する。

`Default` トレイトを実装しているため、`SkillInjections::default()` で「空の注入」を表現できる。スキルが指定されなかった場合はこれを返す。

```rust
#[derive(Debug, Default)]
pub(crate) struct SkillInjections {
    pub(crate) items: Vec<ResponseItem>,  // 注入するスキル内容
    pub(crate) warnings: Vec<String>,     // 読み込み失敗時の警告
}
```

### 8.2 スキル注入のビルド

**概要**: ユーザー入力からスキル指定を抽出し、該当するスキルファイルを読み込んで `SkillInjections` を構築する。

この関数は `async` である点に注目。`tokio::fs::read_to_string()` は非同期のファイル読み込みで、ファイル I/O を待っている間に他のタスクを実行できる。これは複数のスキルファイルを効率的に読み込むために重要。

処理の流れ：

1. 入力から `UserInput::Skill` バリアントを抽出
2. 利用可能なスキル一覧と照合
3. 各スキルファイルを非同期で読み込み
4. `ResponseItem` に変換して結果に追加

**関数**: `build_skill_injections()` (injection.rs:56-114)

```rust
pub(crate) async fn build_skill_injections(
    inputs: &[UserInput],
    skills: Option<&SkillLoadOutcome>,
) -> SkillInjections {
    // 1. 入力からスキル指定を抽出
    let mentioned_skills = collect_explicit_skill_mentions(inputs, &outcome.skills);
    if mentioned_skills.is_empty() {
        return SkillInjections::default();
    }

    let mut result = SkillInjections::default();

    // 2. 各スキルファイルを非同期で読み込み
    for skill in mentioned_skills {
        match tokio::fs::read_to_string(&skill.path).await {
            Ok(contents) => {
                // ResponseItem に変換して追加
                result.items.push(ResponseItem::from(SkillInstructions {
                    name: skill.name,
                    path: skill.path.to_string_lossy().into_owned(),
                    contents,
                }));
            }
            Err(err) => {
                result.warnings.push(format!(
                    "Failed to load skill {} at {}: {err:#}",
                    skill.name, skill.path.display()
                ));
            }
        }
    }

    result
}
```

### 8.3 スキル指定の抽出

**概要**: ユーザー入力（`UserInput` の配列）から、`UserInput::Skill` バリアントを抽出し、利用可能なスキル一覧と照合する。

Rust の `if let ... && ...` 構文（let chains）は、パターンマッチングと条件式を組み合わせる強力な機能。この例では3つの条件を同時にチェックしている：

1. `input` が `UserInput::Skill` 型である
2. そのスキル名がまだ `seen` に含まれていない（重複排除）
3. 利用可能なスキル一覧に存在する

`seen.insert(name.clone())` は、新規追加時に `true` を返し、既存要素の場合は `false` を返す。これを条件式として使うことで、重複排除とチェックを1行で実現している。

**関数**: `collect_explicit_skill_mentions()` (injection.rs:123-147)

```rust
fn collect_explicit_skill_mentions(
    inputs: &[UserInput],
    skills: &[SkillMetadata],
) -> Vec<SkillMetadata> {
    let mut selected = Vec::new();
    let mut seen = HashSet::new();

    for input in inputs {
        // UserInput::Skill バリアントを抽出
        if let UserInput::Skill { name, path } = input
            && seen.insert(name.clone())  // 重複排除
            && let Some(skill) = skills.iter().find(|s| s.name == *name && s.path == *path)
        {
            selected.push(skill.clone());
        }
    }

    selected
}
```

---

## 9. コードウォークスルー: マークダウン生成

**ファイル**: `render.rs`（75行）

このセクションでは、スキル一覧をマークダウン形式に変換する処理を解説する。生成されたマークダウンはシステムプロンプトの一部として LLM に渡される。

**なぜマークダウン形式か？**

- LLM は構造化されたテキストを理解しやすい
- 人間がデバッグ時に読みやすい
- 他のドキュメントと一貫した形式

### 9.1 スキル一覧のレンダリング

**概要**: 利用可能なスキルの一覧をマークダウン形式で生成する。スキルがない場合は `None` を返し、システムプロンプトにスキルセクション自体を含めない。

この関数は以下を生成する：

1. **ヘッダー**: `## Skills` と説明文
2. **スキルリスト**: 各スキルの名前、説明、ファイルパス
3. **使用ガイダンス**: LLM がスキルを適切に使用するための詳細なルール

ガイダンスには「プログレッシブディスクロージャー」（段階的開示）の指示が含まれている。これにより、LLM はスキルの内容を一度に全て読み込むのではなく、必要に応じて段階的に参照するよう指示される。

**関数**: `render_skills_section()` (render.rs:23-74)

```rust
pub fn render_skills_section(skills: &[SkillMetadata]) -> Option<String> {
    if skills.is_empty() {
        return None;  // スキルがなければセクションを生成しない
    }

    let mut lines: Vec<String> = Vec::new();

    // ヘッダー
    lines.push("## Skills".to_string());
    lines.push("These skills are discovered at startup from multiple local sources...".to_string());

    // 各スキルをリスト形式で追加
    for skill in skills {
        let path_str = skill.path.to_string_lossy().replace('\\', "/");
        lines.push(format!("- {}: {} (file: {})", skill.name, skill.description, path_str));
    }

    // 使用ルールのガイダンスを追加
    lines.push(r###"- Discovery: Available skills are listed in project docs...
- Trigger rules: If the user names a skill...
- How to use a skill (progressive disclosure):
  1) After deciding to use a skill, open its `SKILL.md`...
..."###.to_string());

    Some(lines.join("\n"))
}
```

---

## 10. コードウォークスルー: システムスキル

**ファイル**: `system.rs`（272行）

システムスキルは、Codex バイナリに埋め込まれた「組み込みスキル」である。これにより：

- **配布が簡単**: バイナリ1つでスキルも含まれる
- **初回起動時に展開**: `~/.codex/skills/.system/` に書き出される
- **アップデート検出**: フィンガープリント（ハッシュ）で変更を検出

Python でいえば、パッケージ内にデータファイルを含めて `pkg_resources` で読み込むようなもの。Node.js では `__dirname` からの相対パスでアセットを読み込む手法に相当する。

### 10.1 コンパイル時埋め込み

**概要**: `include_dir!` マクロは、コンパイル時に指定されたディレクトリの全ファイルをバイナリに埋め込む。実行時にファイルシステムからの読み取りは不要。

`$CARGO_MANIFEST_DIR` は Cargo.toml があるディレクトリを指す環境変数。Python の `__file__` のディレクトリ、Node.js の `__dirname` に相当する。

```rust
/// システムスキルをコンパイル時にバイナリに埋め込む
const SYSTEM_SKILLS_DIR: Dir =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/skills/assets/samples");
```

### 10.2 インストール処理

**概要**: バイナリに埋め込まれたシステムスキルを `~/.codex/skills/.system/` に展開する。フィンガープリント（ハッシュ）を使って変更を検出し、必要な場合のみ再展開する。

**なぜフィンガープリントを使うか？**
起動のたびに全ファイルを書き直すのは非効率。フィンガープリントが一致すれば、スキルは変更されていないと判断できる。これは Web 開発でのファイルハッシュを使ったキャッシュ無効化と同じ考え方。

処理の流れ：

1. 埋め込みスキルのフィンガープリントを計算
2. マーカーファイル（`.codex-system-skills.marker`）と比較
3. 一致すればスキップ（何もしない）
4. 不一致なら既存ディレクトリを削除して再展開
5. 新しいフィンガープリントをマーカーファイルに保存

**関数**: `install_system_skills()` (system.rs:92-141)

```rust
pub(crate) fn install_system_skills(codex_home: &Path) -> Result<(), SystemSkillsError> {
    let dest_system = system_cache_root_dir_abs(&codex_home)?;  // ~/.codex/skills/.system/
    let marker_path = dest_system.join(SYSTEM_SKILLS_MARKER_FILENAME)?;

    // フィンガープリント（ハッシュ）を計算
    let expected_fingerprint = embedded_system_skills_fingerprint();

    // マーカーファイルと比較（一致すればスキップ）
    if dest_system.as_path().is_dir()
        && read_marker(&marker_path).is_ok_and(|marker| marker == expected_fingerprint)
    {
        return Ok(());  // 変更なし
    }

    // 不一致 → 再インストール
    if dest_system.as_path().exists() {
        fs::remove_dir_all(dest_system.as_path())?;
    }

    // 埋め込みファイルを展開
    write_embedded_dir(&SYSTEM_SKILLS_DIR, &dest_system)?;

    // マーカーファイルに新しいフィンガープリントを保存
    fs::write(marker_path.as_path(), format!("{expected_fingerprint}\n"))?;

    Ok(())
}
```

### 10.3 現在の組み込みスキル

| スキル            | 説明                                        |
| ----------------- | ------------------------------------------- |
| `skill-creator`   | 新しいスキルを作成するためのガイド（374行） |
| `skill-installer` | 既存スキルをインストールするためのガイド    |

---

# Part 3: 設定と使い方

## 11. 機能フラグ

**ファイル**: `features.rs:386-391`

```rust
FeatureSpec {
    id: Feature::Skills,
    key: "skills",
    stage: Stage::Experimental,
    default_enabled: true,  // 2025-12-18 からデフォルト有効
}
```

**設定方法**:

```toml
# ~/.codex/config.toml
[features]
skills = true  # または false で無効化
```

---

## 12. スキルファイルの構造

### 12.1 ディレクトリ構成

```
skill-name/
├── SKILL.md (必須)
│   ├── YAML フロントマター
│   │   ├── name: スキル名
│   │   ├── description: トリガー条件と用途
│   │   └── metadata.short-description: 短い説明（オプション）
│   └── Markdown インストラクション
└── バンドルリソース（オプション）
    ├── scripts/        # 実行可能コード
    ├── references/     # 参照ドキュメント
    └── assets/         # テンプレート、アイコン等
```

### 12.2 SKILL.md のフロントマター例

```yaml
---
name: pdf-editor
description: Edit PDF files including rotation, merging, splitting, and text extraction. Use when the user wants to manipulate PDF documents.
metadata:
  short-description: Edit and manipulate PDF files
---
# PDF Editor Skill

## Usage
1. ...
```

### 12.3 バリデーションルール

| 項目              | ルール                     | エラー時                              |
| ----------------- | -------------------------- | ------------------------------------- |
| name              | 最大64文字                 | `SkillParseError::InvalidField`       |
| description       | 最大1024文字               | `SkillParseError::InvalidField`       |
| short-description | 最大1024文字               | `SkillParseError::InvalidField`       |
| フロントマター    | `---` で囲まれた YAML 必須 | `SkillParseError::MissingFrontmatter` |

---

## 13. ユーザーインタラクションフロー

```
1. ユーザーが $ を入力してスキルポップアップを開く
   ↓
2. ファジー検索でスキルを選択（short-description 表示）
   ↓
3. Enter で選択
   ↓
4. UserInput::Skill { name, path } が生成される
   ↓
5. build_skill_injections() で SKILL.md の内容を読み込み
   ↓
6. ResponseItem として会話履歴に追加
   ↓
7. LLM に送信
```

---

# Part 4: 設計と品質

## 14. 設計原則

### 14.1 プログレッシブディスクロージャー

スキルは3段階のローディングシステムを使用：

1. **メタデータ（名前+説明）** - 常時読み込み（約100ワード）
2. **SKILL.md 本体** - トリガー時のみ（5kワード以下推奨）
3. **バンドルリソース** - 必要に応じて読み込み

### 14.2 コンテキスト効率

- スキルはトークン予算を共有
- 不要な情報を含めない
- スクリプトは読み込みではなく実行優先

### 14.3 セキュリティ境界

- **リポジトリスキル**: Git 信頼チェック必須（`resolve_root_git_project_for_trust()`）
- **システムスキル**: バイナリ埋め込みのみ
- **優先度による上書き保護**: 高優先度スコープが勝つ

---

## 15. エラーハンドリング

### 15.1 エラー処理の方針

| スコープ       | エラー時の動作                                                   |
| -------------- | ---------------------------------------------------------------- |
| システムスキル | ログ出力のみ、スキップ                                           |
| ユーザースキル | `SkillError` として記録、`ListSkillsResponseEvent.errors` で返す |
| ロード途中     | 他のスキルに影響しない（fail-open）                              |

### 15.2 SystemSkillsError

**ファイル**: `system.rs:254-271`

```rust
#[derive(Debug, Error)]
pub(crate) enum SystemSkillsError {
    #[error("io error while {action}: {source}")]
    Io {
        action: &'static str,
        #[source]
        source: std::io::Error,
    },
}
```

---

# Part 5: リファレンス

## ファイル・関数リファレンス

### Skills モジュール

| ファイル              | 行数 | 主要な関数/構造体                                                                                                          | 役割                      |
| --------------------- | ---- | -------------------------------------------------------------------------------------------------------------------------- | ------------------------- |
| `skills/mod.rs`       | 46   | -                                                                                                                          | モジュール定義、re-export |
| `skills/model.rs`     | 76   | `SkillMetadata`, `SkillError`, `SkillLoadOutcome`                                                                          | データ構造                |
| `skills/loader.rs`    | 1129 | `load_skills()`, `load_skills_from_roots()`, `skill_roots_for_cwd()`, `discover_skills_under_root()`, `parse_skill_file()` | スキル検出・解析          |
| `skills/manager.rs`   | 118  | `SkillsManager`, `skills_for_cwd()`, `skills_for_cwd_with_options()`                                                       | キャッシング管理          |
| `skills/injection.rs` | 148  | `SkillInjections`, `build_skill_injections()`, `collect_explicit_skill_mentions()`                                         | プロンプト注入            |
| `skills/render.rs`    | 75   | `render_skills_section()`                                                                                                  | マークダウン生成          |
| `skills/system.rs`    | 272  | `install_system_skills()`, `embedded_system_skills_fingerprint()`, `write_embedded_dir()`                                  | システムスキル            |

### コアモジュール

| ファイル                  | 主要な関数/構造体                                                                           | Skills との関連                       |
| ------------------------- | ------------------------------------------------------------------------------------------- | ------------------------------------- |
| `codex.rs`                | `Session`, `TurnContext`, `run_task()`, `run_turn()`, `try_run_turn()`, `submission_loop()` | Skills 注入、プロンプト構築           |
| `project_doc.rs`          | `get_user_instructions()`, `read_project_docs()`                                            | Skills 一覧をシステムプロンプトに挿入 |
| `conversation_manager.rs` | `ConversationManager::new()`                                                                | `SkillsManager` 初期化                |
| `client.rs`               | `ModelClient`, `stream()`, `stream_responses_api()`                                         | LLM へのリクエスト送信                |
| `features.rs`             | `Feature::Skills`                                                                           | 機能フラグ定義                        |

### 行番号クイックリファレンス

| 処理                             | ファイル                  | 行番号    |
| -------------------------------- | ------------------------- | --------- |
| SkillsManager 初期化             | `conversation_manager.rs` | 54-55     |
| セッション開始時のスキル読み込み | `codex.rs`                | 220-233   |
| システムプロンプト構築           | `codex.rs`                | 235-241   |
| スキル一覧レンダリング呼び出し   | `project_doc.rs`          | 35-39     |
| run_task でのスキル注入          | `codex.rs`                | 2229-2253 |
| Submission チャネル送信          | `codex.rs`                | 306-323   |
| Submission ループ                | `codex.rs`                | 1578-1672 |
| run_turn プロンプト構築          | `codex.rs`                | 2365-2469 |
| try_run_turn イベントループ      | `codex.rs`                | 2504-2710 |

---

## 実装時期（Git 履歴）

| 日付           | コミット    | 内容                                                            |
| -------------- | ----------- | --------------------------------------------------------------- |
| **2025-12-01** | `a8d5ad37b` | 最初の実装 - "feat: experimental support for skills.md (#7412)" |
| 2025-12-02     | `9a50a0440` | TUI でのスキル選択 UI (`$` or `/skills`)                        |
| 2025-12-05     | `93f61dbc5` | リポジトリルートからのスキル読み込み対応                        |
| 2025-12-10     | `5d77d4db6` | SkillsManager によるキャッシング再実装                          |
| 2025-12-16     | `da3869eeb` | システムスキルのサポート追加                                    |
| **2025-12-18** | `d35337227` | **デフォルト有効化** (`default_enabled: true`)                  |
| 2025-12-19     | `8120c8765` | 管理者スキル（Admin scope）のサポート                           |

最初のコミットから約2週間後にデフォルト有効になった。

---

## まとめ

このドキュメントでは、codex-rs プロジェクトのコードリーディングを効率化するため、以下を解説した：

1. **全体アーキテクチャ**: ユーザー入力から LLM 出力までの9ステップのデータフロー
2. **Skills 機能の詳細**: 2フェーズ（起動時・入力時）での注入タイミングとコードパス
3. **各モジュールのウォークスルー**: 読む順序と主要関数の解説
4. **リファレンス**: ファイル・関数・行番号の一覧

Codex の Skills 機能は、**モジュール化された専門知識パッケージ**として、AI エージェントに動的に機能を拡張できる。実装は堅牢で、複数スコープ、優先度ベースの重複排除、セキュリティ検査が組み込まれている。

コードリーディングを始める際は、まず [クイックリファレンス](#クイックリファレンスtldr) で全体像を把握し、その後 [Part 1](#part-1-全体アーキテクチャ) でデータフローを理解してから、[Part 2](#part-2-skills機能の詳細) で Skills の詳細に進むことを推奨する。
