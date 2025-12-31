//! # Skills ローダー
//!
//! スキルファイル（SKILL.md）を検出・解析するモジュール。
//! 複数のスキルルート（リポジトリ、ユーザー、システム、管理者）から
//! スキルを読み込み、優先度に基づいて重複を排除する。
//!
//! ## 処理の流れ
//! 1. スキルルートの決定（`skill_roots_for_cwd`）
//! 2. 各ルートからスキルを検出（`discover_skills_under_root`）
//! 3. YAMLフロントマターを解析（`parse_skill_file`）
//! 4. 重複排除と並べ替え（`load_skills_from_roots`）

// =============================================================================
// インポート
// =============================================================================
// Rustでは `use` でモジュールや型をインポートする。Python の `import` に相当。
// `crate::` は現在のクレート（パッケージ）のルートを指す。

use crate::config::Config; // アプリケーション設定
use crate::git_info::resolve_root_git_project_for_trust; // Gitリポジトリのルート取得
use crate::skills::model::SkillError;
use crate::skills::model::SkillLoadOutcome;
use crate::skills::model::SkillMetadata;
use crate::skills::system::system_cache_root_dir;
use codex_protocol::protocol::SkillScope;

// 外部クレート（サードパーティライブラリ）
use dunce::canonicalize as normalize_path; // パスの正規化（Windowsの\\?\プレフィックス除去）
use serde::Deserialize; // JSONやYAMLからのデシリアライズ用トレイト
use std::collections::HashSet; // Python の set に相当
use std::collections::VecDeque; // 両端キュー（BFS探索用）
use std::error::Error; // エラートレイト
use std::fmt; // フォーマット用トレイト
use std::fs; // ファイルシステム操作
use std::path::Path; // パスへの参照（&str のパス版）
use std::path::PathBuf; // 所有権を持つパス（String のパス版）
use tracing::error; // ログ出力用マクロ

// =============================================================================
// 内部データ構造（YAMLフロントマター用）
// =============================================================================
/// SKILL.mdのYAMLフロントマターをデシリアライズするための構造体。
///
/// `#[derive(Deserialize)]` により、serde_yaml がYAMLから自動変換してくれる。
/// Python の pydantic や dataclasses + dacite に似た仕組み。
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    /// `#[serde(default)]` は、YAMLにこのフィールドがない場合に
    /// `Default::default()` を使用することを示す。
    #[serde(default)]
    metadata: SkillFrontmatterMetadata,
}

/// フロントマターの `metadata` セクション用の構造体。
#[derive(Debug, Default, Deserialize)]
struct SkillFrontmatterMetadata {
    /// `#[serde(rename = "short-description")]` はYAMLのキー名を指定。
    /// Rustでは変数名にハイフンが使えないため、アンダースコアに変換。
    #[serde(default, rename = "short-description")]
    short_description: Option<String>,
}

// =============================================================================
// 定数
// =============================================================================
// `const` はコンパイル時定数。Python の大文字変数や TypeScript の `as const` に相当。
// `&str` は文字列スライス（参照）。所有権を持たない読み取り専用の文字列。

const SKILLS_FILENAME: &str = "SKILL.md"; // スキルファイルの名前
const SKILLS_DIR_NAME: &str = "skills"; // スキルディレクトリ名
const REPO_ROOT_CONFIG_DIR_NAME: &str = ".codex"; // リポジトリ設定ディレクトリ
const ADMIN_SKILLS_ROOT: &str = "/etc/codex/skills"; // 管理者スキルのパス（UNIXのみ）
const MAX_NAME_LEN: usize = 64; // スキル名の最大文字数
const MAX_DESCRIPTION_LEN: usize = 1024; // 説明の最大文字数
const MAX_SHORT_DESCRIPTION_LEN: usize = MAX_DESCRIPTION_LEN;

// =============================================================================
// エラー型
// =============================================================================
/// スキルファイル解析時のエラーを表すenum（列挙型）。
///
/// ## Rustのenum について（Python/JS経験者向け）
/// Rustの enum は「タグ付きユニオン」や「代数的データ型」とも呼ばれる。
/// Python の Union[Type1, Type2] や TypeScript の `type Error = A | B` に似ているが、
/// 各バリアントがデータを持てる点が異なる。
///
/// ```rust
/// enum Result { Ok(値), Err(エラー) }  // 成功か失敗のどちらか
/// enum Option { Some(値), None }       // 値があるかないか
/// ```
#[derive(Debug)]
enum SkillParseError {
    /// ファイル読み込みエラー（IOエラーをラップ）
    Read(std::io::Error),
    /// YAMLフロントマター（---で囲まれた部分）が見つからない
    MissingFrontmatter,
    /// YAMLの解析に失敗
    InvalidYaml(serde_yaml::Error),
    /// 必須フィールドが欠落（`&'static str` はプログラム全体で有効な文字列参照）
    MissingField(&'static str),
    /// フィールドの値が不正（長すぎるなど）
    InvalidField { field: &'static str, reason: String },
}

/// `Display` トレイトの実装。`println!("{}", error)` で表示可能にする。
/// Python の `__str__` メソッドに相当。
impl fmt::Display for SkillParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `match` は Rust のパターンマッチング。Python の match-case や
        // JavaScript の switch に似ているが、網羅性チェックがある。
        match self {
            SkillParseError::Read(e) => write!(f, "failed to read file: {e}"),
            SkillParseError::MissingFrontmatter => {
                write!(f, "missing YAML frontmatter delimited by ---")
            }
            SkillParseError::InvalidYaml(e) => write!(f, "invalid YAML: {e}"),
            SkillParseError::MissingField(field) => write!(f, "missing field `{field}`"),
            SkillParseError::InvalidField { field, reason } => {
                write!(f, "invalid {field}: {reason}")
            }
        }
    }
}

/// `Error` トレイトを実装（空のまま）。これにより標準エラー処理で使用可能になる。
impl Error for SkillParseError {}

// =============================================================================
// パブリックAPI
// =============================================================================

/// 設定に基づいてスキルを読み込むメイン関数。
/// 外部から呼ばれるエントリーポイント。
pub fn load_skills(config: &Config) -> SkillLoadOutcome {
    load_skills_from_roots(skill_roots(config))
}

/// スキルルート（検索対象ディレクトリ）を表す構造体。
pub(crate) struct SkillRoot {
    pub(crate) path: PathBuf,     // ディレクトリパス
    pub(crate) scope: SkillScope, // スコープ（優先度を決める）
}

/// 複数のスキルルートからスキルを読み込む。
///
/// ## ジェネリクスについて（Rust初心者向け）
/// `<I>` はジェネリック型パラメータ。Python の TypeVar や TypeScript の <T> に相当。
///
/// `where I: IntoIterator<Item = SkillRoot>` は型制約（トレイト境界）で、
/// 「I は SkillRoot を要素とするイテレータに変換できる型でなければならない」という意味。
/// Python の `Iterable[SkillRoot]` 型ヒントに相当。
///
/// これにより `Vec<SkillRoot>` も `[SkillRoot; 3]` も引数として渡せる。
pub(crate) fn load_skills_from_roots<I>(roots: I) -> SkillLoadOutcome
where
    I: IntoIterator<Item = SkillRoot>,
{
    // `default()` で空の結果を作成（skills: [], errors: []）
    let mut outcome = SkillLoadOutcome::default();

    // 各ルートディレクトリを探索してスキルを収集
    for root in roots {
        discover_skills_under_root(&root.path, root.scope, &mut outcome);
    }

    // --- 重複排除 ---
    // 同名スキルが複数ある場合、最初に見つかったもの（優先度の高いスコープ）を保持。
    // roots は [Repo, User, System, Admin] の順で渡されるため、
    // Repo スキルが最優先される。
    let mut seen: HashSet<String> = HashSet::new();
    // `retain` は条件を満たす要素のみ保持。Python の filter() に似ている。
    // `seen.insert()` は新規挿入なら true、既存なら false を返す。
    outcome
        .skills
        .retain(|skill| seen.insert(skill.name.clone()));

    // --- ソート ---
    // 名前順、同名ならパス順でソート。
    // `cmp` は Ordering (Less, Equal, Greater) を返す比較メソッド。
    // `then_with` は最初の比較が Equal の場合にのみ次の比較を実行。
    outcome
        .skills
        .sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));

    outcome
}

// =============================================================================
// スキルルート生成関数
// =============================================================================
// 各スコープのスキルディレクトリパスを生成する関数群。

/// ユーザースキルのルート: ~/.codex/skills/
pub(crate) fn user_skills_root(codex_home: &Path) -> SkillRoot {
    SkillRoot {
        // `join` はパスを連結。Python の os.path.join() に相当。
        path: codex_home.join(SKILLS_DIR_NAME),
        scope: SkillScope::User,
    }
}

/// システムスキルのルート: ~/.codex/skills/.system/
/// コンパイル時に埋め込まれた組み込みスキルが展開される場所。
pub(crate) fn system_skills_root(codex_home: &Path) -> SkillRoot {
    SkillRoot {
        path: system_cache_root_dir(codex_home),
        scope: SkillScope::System,
    }
}

/// 管理者スキルのルート: /etc/codex/skills/（UNIXのみ）
/// システム管理者が全ユーザー向けにスキルを配置できる場所。
pub(crate) fn admin_skills_root() -> SkillRoot {
    SkillRoot {
        // `PathBuf::from()` は文字列からパスを作成。
        path: PathBuf::from(ADMIN_SKILLS_ROOT),
        scope: SkillScope::Admin,
    }
}

/// リポジトリスキルのルート: .codex/skills/（リポジトリ内）
///
/// Gitリポジトリ内の .codex/skills/ ディレクトリを探す。
/// カレントディレクトリから親ディレクトリを遡り、Gitルートまで探索する。
///
/// ## Option<T> について
/// `Option<T>` は値が存在するかもしれないことを表す型。
/// - `Some(値)`: 値が存在する
/// - `None`: 値が存在しない
/// Python の `Optional[T]` や JavaScript の `T | null` に相当。
/// `?` 演算子で None を早期リターンできる（Python の early return パターン）。
pub(crate) fn repo_skills_root(cwd: &Path) -> Option<SkillRoot> {
    // cwd がファイルならその親ディレクトリを使う
    // `?` は None の場合に関数から None を返す（early return）
    let base = if cwd.is_dir() { cwd } else { cwd.parent()? };
    // パスを正規化。失敗時は元のパスを使う。
    // `unwrap_or_else` はエラー時のフォールバック。Python の except に似ている。
    let base = normalize_path(base).unwrap_or_else(|_| base.to_path_buf());

    // Gitリポジトリのルートを取得（信頼チェック付き）
    // `.map()` は Option/Result の中身を変換。Python の map() に似ている。
    let repo_root =
        resolve_root_git_project_for_trust(&base).map(|root| normalize_path(&root).unwrap_or(root));

    let scope = SkillScope::Repo;

    // Gitリポジトリ内の場合
    // `if let Some(x) = ...` はパターンマッチングによる条件分岐。
    // Python の `if (x := ...) is not None:` に似ている。
    if let Some(repo_root) = repo_root.as_deref() {
        // base から親ディレクトリを遡って .codex/skills/ を探す
        // `ancestors()` は自身から "/" までの全ての親パスを返すイテレータ
        for dir in base.ancestors() {
            let skills_root = dir.join(REPO_ROOT_CONFIG_DIR_NAME).join(SKILLS_DIR_NAME);
            if skills_root.is_dir() {
                return Some(SkillRoot {
                    path: skills_root,
                    scope,
                });
            }

            // Gitルートを超えて探索しない（セキュリティ対策）
            if dir == repo_root {
                break;
            }
        }
        return None;
    }

    // Gitリポジトリ外の場合はカレントディレクトリのみチェック
    let skills_root = base.join(REPO_ROOT_CONFIG_DIR_NAME).join(SKILLS_DIR_NAME);
    // `bool.then_some(値)` は true なら Some(値)、false なら None を返す
    skills_root.is_dir().then_some(SkillRoot {
        path: skills_root,
        scope,
    })
}

/// 指定されたカレントディレクトリに対するスキルルートのリストを生成。
///
/// 優先度順（高い順）:
/// 1. Repo - リポジトリ内の .codex/skills/
/// 2. User - ~/.codex/skills/
/// 3. System - ~/.codex/skills/.system/
/// 4. Admin - /etc/codex/skills/（UNIXのみ）
pub(crate) fn skill_roots_for_cwd(codex_home: &Path, cwd: &Path) -> Vec<SkillRoot> {
    let mut roots = Vec::new();

    // リポジトリスキルがあれば最初に追加（最優先）
    if let Some(repo_root) = repo_skills_root(cwd) {
        roots.push(repo_root);
    }

    // 読み込み順序が重要: 名前で重複排除時、最初に見つかったものが採用される。
    // 優先度: repo > user > system > admin
    roots.push(user_skills_root(codex_home));
    roots.push(system_skills_root(codex_home));

    // `cfg!(unix)` はコンパイル時条件。UNIXプラットフォームでのみ true。
    // Python の `if sys.platform != 'win32':` に相当するが、コンパイル時に評価される。
    if cfg!(unix) {
        roots.push(admin_skills_root());
    }

    roots
}

/// Config からスキルルートを生成（内部ヘルパー関数）。
fn skill_roots(config: &Config) -> Vec<SkillRoot> {
    skill_roots_for_cwd(&config.codex_home, &config.cwd)
}

// =============================================================================
// ディレクトリ探索
// =============================================================================

/// 指定されたルートディレクトリ以下のスキルファイルを探索する。
///
/// BFS（幅優先探索）でディレクトリを走査し、SKILL.md ファイルを見つけたら
/// 解析して outcome に追加する。
///
/// ## `&mut` について（所有権とボローイング）
/// Rust では変数の所有権を厳密に管理する。
/// - `&T`: 不変参照（読み取りのみ）。Python の通常の引数渡しに近い。
/// - `&mut T`: 可変参照（読み書き可能）。この関数では outcome を変更するため必要。
///
/// 同時に複数の可変参照は持てない（データ競合を防ぐ）。
fn discover_skills_under_root(root: &Path, scope: SkillScope, outcome: &mut SkillLoadOutcome) {
    // `let Ok(x) = expr else { ... }` はパターンマッチングによるエラーハンドリング。
    // Err の場合は else ブロックを実行して早期リターン。
    let Ok(root) = normalize_path(root) else {
        return;
    };

    // ディレクトリでなければスキップ
    if !root.is_dir() {
        return;
    }

    // BFS用のキュー。`VecDeque` は両端キュー（front から pop、back に push）。
    // `VecDeque::from([...])` は配列から VecDeque を作成。
    let mut queue: VecDeque<PathBuf> = VecDeque::from([root]);

    // キューが空になるまでループ
    // `while let Some(x) = expr` は、expr が Some を返す限りループ続行。
    while let Some(dir) = queue.pop_front() {
        // ディレクトリの中身を読み取り
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                // エラー時はログを出力して次のディレクトリへ
                // `error!` は tracing クレートのログマクロ
                error!("failed to read skills dir {}: {e:#}", dir.display());
                continue;
            }
        };

        // `flatten()` は Iterator<Item=Result<T,E>> を Iterator<Item=T> に変換
        // エラーの要素は単にスキップされる（Python の filter + map に似ている）
        for entry in entries.flatten() {
            let path = entry.path();

            // ファイル名を取得。Unicode変換失敗時はスキップ。
            // `and_then` は Option をチェーンで処理。Python の `x and x.method()` に似ている。
            let file_name = match path.file_name().and_then(|f| f.to_str()) {
                Some(name) => name,
                None => continue,
            };

            // 隠しファイル（.で始まる）はスキップ
            if file_name.starts_with('.') {
                continue;
            }

            // ファイルタイプを取得できなければスキップ
            let Ok(file_type) = entry.file_type() else {
                continue;
            };

            // シンボリックリンクはスキップ（セキュリティ対策）
            if file_type.is_symlink() {
                continue;
            }

            // ディレクトリなら探索キューに追加
            if file_type.is_dir() {
                queue.push_back(path);
                continue;
            }

            // SKILL.md ファイルを見つけたら解析
            if file_type.is_file() && file_name == SKILLS_FILENAME {
                match parse_skill_file(&path, scope) {
                    Ok(skill) => {
                        outcome.skills.push(skill);
                    }
                    Err(err) => {
                        // システムスキルのエラーは無視（ログのみ）
                        // ユーザースキルのエラーは記録してUIに表示
                        if scope != SkillScope::System {
                            outcome.errors.push(SkillError {
                                path,
                                message: err.to_string(),
                            });
                        }
                    }
                }
            }
        }
    }
}

// =============================================================================
// スキルファイル解析
// =============================================================================

/// SKILL.md ファイルを解析して SkillMetadata を生成する。
///
/// ## Result<T, E> について
/// `Result` は成功か失敗を表す型。Python の例外に相当するが、明示的に扱う必要がある。
/// - `Ok(値)`: 成功
/// - `Err(エラー)`: 失敗
///
/// `?` 演算子は Err の場合に関数から即座にリターンする糖衣構文。
/// ```rust
/// let x = some_fn()?;  // Err なら即リターン、Ok なら値を取り出す
/// // Python でいうと以下と同等:
/// // x = some_fn()
/// // if isinstance(x, Err): return x
/// ```
fn parse_skill_file(path: &Path, scope: SkillScope) -> Result<SkillMetadata, SkillParseError> {
    // ファイル全体を文字列として読み込み
    // `.map_err()` はエラー型を変換。ここでは io::Error を SkillParseError::Read に変換。
    let contents = fs::read_to_string(path).map_err(SkillParseError::Read)?;

    // YAMLフロントマター（---で囲まれた部分）を抽出
    // `.ok_or()` は Option を Result に変換。None なら指定したエラーに。
    let frontmatter = extract_frontmatter(&contents).ok_or(SkillParseError::MissingFrontmatter)?;

    // YAMLをパース。serde_yaml が自動的に SkillFrontmatter 構造体にマッピング。
    let parsed: SkillFrontmatter =
        serde_yaml::from_str(&frontmatter).map_err(SkillParseError::InvalidYaml)?;

    // フィールドの正規化（改行やタブを空白に置換）
    let name = sanitize_single_line(&parsed.name);
    let description = sanitize_single_line(&parsed.description);

    // short_description はオプショナルなのでチェーン処理
    // `.as_deref()` は Option<String> を Option<&str> に変換
    // `.filter()` は条件を満たさない場合に None に変換
    let short_description = parsed
        .metadata
        .short_description
        .as_deref()
        .map(sanitize_single_line)
        .filter(|value| !value.is_empty());

    // バリデーション（長さチェック）
    validate_field(&name, MAX_NAME_LEN, "name")?;
    validate_field(&description, MAX_DESCRIPTION_LEN, "description")?;
    if let Some(short_description) = short_description.as_deref() {
        validate_field(
            short_description,
            MAX_SHORT_DESCRIPTION_LEN,
            "metadata.short-description",
        )?;
    }

    // パスを正規化（シンボリックリンク解決など）
    let resolved_path = normalize_path(path).unwrap_or_else(|_| path.to_path_buf());

    // SkillMetadata を構築して返す
    // 構造体の最後の式は暗黙的に return される（セミコロンなし）
    Ok(SkillMetadata {
        name,
        description,
        short_description,
        path: resolved_path,
        scope,
    })
}

// =============================================================================
// ヘルパー関数
// =============================================================================

/// 文字列を単一行に正規化する。
/// 連続する空白文字（改行、タブ含む）を単一スペースに置換。
fn sanitize_single_line(raw: &str) -> String {
    // `split_whitespace()` は空白で分割し、空の要素をスキップ
    // `collect::<Vec<_>>()` でベクタに収集（`_` は型推論に任せる）
    // `join(" ")` で空白区切りで結合
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// フィールドのバリデーション。
/// 空でないこと、最大長以下であることを確認。
fn validate_field(
    value: &str,
    max_len: usize,
    field_name: &'static str,
) -> Result<(), SkillParseError> {
    if value.is_empty() {
        return Err(SkillParseError::MissingField(field_name));
    }
    // `.chars().count()` はUnicode文字数をカウント（バイト数ではない）
    if value.chars().count() > max_len {
        return Err(SkillParseError::InvalidField {
            field: field_name,
            reason: format!("exceeds maximum length of {max_len} characters"),
        });
    }
    // ユニット型 `()` を返す（Python の None、JavaScript の undefined に近い）
    Ok(())
}

/// YAML フロントマターを抽出する。
///
/// ```markdown
/// ---
/// name: skill-name
/// description: ...
/// ---
/// # 本文はここから
/// ```
fn extract_frontmatter(contents: &str) -> Option<String> {
    // `.lines()` は行ごとのイテレータを返す
    let mut lines = contents.lines();

    // 最初の行が "---" でなければフロントマターなし
    // `matches!` マクロはパターンマッチングの条件式版
    if !matches!(lines.next(), Some(line) if line.trim() == "---") {
        return None;
    }

    let mut frontmatter_lines: Vec<&str> = Vec::new();
    let mut found_closing = false;

    // 閉じる "---" が見つかるまで行を収集
    // `by_ref()` はイテレータを借用（所有権を移動させない）
    for line in lines.by_ref() {
        if line.trim() == "---" {
            found_closing = true;
            break;
        }
        frontmatter_lines.push(line);
    }

    // フロントマターが空か、閉じる "---" がなければ None
    if frontmatter_lines.is_empty() || !found_closing {
        return None;
    }

    // 行を改行で結合して返す
    Some(frontmatter_lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigBuilder;
    use codex_protocol::protocol::SkillScope;
    use pretty_assertions::assert_eq;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    async fn make_config(codex_home: &TempDir) -> Config {
        let mut config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .build()
            .await
            .expect("defaults for test should always succeed");

        config.cwd = codex_home.path().to_path_buf();
        config
    }

    fn write_skill(codex_home: &TempDir, dir: &str, name: &str, description: &str) -> PathBuf {
        write_skill_at(&codex_home.path().join("skills"), dir, name, description)
    }

    fn write_system_skill(
        codex_home: &TempDir,
        dir: &str,
        name: &str,
        description: &str,
    ) -> PathBuf {
        write_skill_at(
            &codex_home.path().join("skills/.system"),
            dir,
            name,
            description,
        )
    }

    fn write_skill_at(root: &Path, dir: &str, name: &str, description: &str) -> PathBuf {
        let skill_dir = root.join(dir);
        fs::create_dir_all(&skill_dir).unwrap();
        let indented_description = description.replace('\n', "\n  ");
        let content = format!(
            "---\nname: {name}\ndescription: |-\n  {indented_description}\n---\n\n# Body\n"
        );
        let path = skill_dir.join(SKILLS_FILENAME);
        fs::write(&path, content).unwrap();
        path
    }

    #[tokio::test]
    async fn loads_valid_skill() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_skill(&codex_home, "demo", "demo-skill", "does things\ncarefully");
        let cfg = make_config(&codex_home).await;

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        let skill = &outcome.skills[0];
        assert_eq!(skill.name, "demo-skill");
        assert_eq!(skill.description, "does things carefully");
        assert_eq!(skill.short_description, None);
        let path_str = skill.path.to_string_lossy().replace('\\', "/");
        assert!(
            path_str.ends_with("skills/demo/SKILL.md"),
            "unexpected path {path_str}"
        );
    }

    #[tokio::test]
    async fn loads_short_description_from_metadata() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let skill_dir = codex_home.path().join("skills/demo");
        fs::create_dir_all(&skill_dir).unwrap();
        let contents = "---\nname: demo-skill\ndescription: long description\nmetadata:\n  short-description: short summary\n---\n\n# Body\n";
        fs::write(skill_dir.join(SKILLS_FILENAME), contents).unwrap();

        let cfg = make_config(&codex_home).await;
        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(
            outcome.skills[0].short_description,
            Some("short summary".to_string())
        );
    }

    #[tokio::test]
    async fn enforces_short_description_length_limits() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let skill_dir = codex_home.path().join("skills/demo");
        fs::create_dir_all(&skill_dir).unwrap();
        let too_long = "x".repeat(MAX_SHORT_DESCRIPTION_LEN + 1);
        let contents = format!(
            "---\nname: demo-skill\ndescription: long description\nmetadata:\n  short-description: {too_long}\n---\n\n# Body\n"
        );
        fs::write(skill_dir.join(SKILLS_FILENAME), contents).unwrap();

        let cfg = make_config(&codex_home).await;
        let outcome = load_skills(&cfg);
        assert_eq!(outcome.skills.len(), 0);
        assert_eq!(outcome.errors.len(), 1);
        assert!(
            outcome.errors[0]
                .message
                .contains("invalid metadata.short-description"),
            "expected length error, got: {:?}",
            outcome.errors
        );
    }

    #[tokio::test]
    async fn skips_hidden_and_invalid() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let hidden_dir = codex_home.path().join("skills/.hidden");
        fs::create_dir_all(&hidden_dir).unwrap();
        fs::write(
            hidden_dir.join(SKILLS_FILENAME),
            "---\nname: hidden\ndescription: hidden\n---\n",
        )
        .unwrap();

        // Invalid because missing closing frontmatter.
        let invalid_dir = codex_home.path().join("skills/invalid");
        fs::create_dir_all(&invalid_dir).unwrap();
        fs::write(invalid_dir.join(SKILLS_FILENAME), "---\nname: bad").unwrap();

        let cfg = make_config(&codex_home).await;
        let outcome = load_skills(&cfg);
        assert_eq!(outcome.skills.len(), 0);
        assert_eq!(outcome.errors.len(), 1);
        assert!(
            outcome.errors[0]
                .message
                .contains("missing YAML frontmatter"),
            "expected frontmatter error"
        );
    }

    #[tokio::test]
    async fn enforces_length_limits() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let max_desc = "\u{1F4A1}".repeat(MAX_DESCRIPTION_LEN);
        write_skill(&codex_home, "max-len", "max-len", &max_desc);
        let cfg = make_config(&codex_home).await;

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);

        let too_long_desc = "\u{1F4A1}".repeat(MAX_DESCRIPTION_LEN + 1);
        write_skill(&codex_home, "too-long", "too-long", &too_long_desc);
        let outcome = load_skills(&cfg);
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(outcome.errors.len(), 1);
        assert!(
            outcome.errors[0].message.contains("invalid description"),
            "expected length error"
        );
    }

    #[tokio::test]
    async fn loads_skills_from_repo_root() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let repo_dir = tempfile::tempdir().expect("tempdir");

        let status = Command::new("git")
            .arg("init")
            .current_dir(repo_dir.path())
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed");

        let skills_root = repo_dir
            .path()
            .join(REPO_ROOT_CONFIG_DIR_NAME)
            .join(SKILLS_DIR_NAME);
        write_skill_at(&skills_root, "repo", "repo-skill", "from repo");
        let mut cfg = make_config(&codex_home).await;
        cfg.cwd = repo_dir.path().to_path_buf();
        let repo_root = normalize_path(&skills_root).unwrap_or_else(|_| skills_root.clone());

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        let skill = &outcome.skills[0];
        assert_eq!(skill.name, "repo-skill");
        assert!(skill.path.starts_with(&repo_root));
    }

    #[tokio::test]
    async fn loads_skills_from_nearest_codex_dir_under_repo_root() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let repo_dir = tempfile::tempdir().expect("tempdir");

        let status = Command::new("git")
            .arg("init")
            .current_dir(repo_dir.path())
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed");

        let nested_dir = repo_dir.path().join("nested/inner");
        fs::create_dir_all(&nested_dir).unwrap();

        write_skill_at(
            &repo_dir
                .path()
                .join(REPO_ROOT_CONFIG_DIR_NAME)
                .join(SKILLS_DIR_NAME),
            "root",
            "root-skill",
            "from root",
        );
        write_skill_at(
            &repo_dir
                .path()
                .join("nested")
                .join(REPO_ROOT_CONFIG_DIR_NAME)
                .join(SKILLS_DIR_NAME),
            "nested",
            "nested-skill",
            "from nested",
        );

        let mut cfg = make_config(&codex_home).await;
        cfg.cwd = nested_dir;

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(outcome.skills[0].name, "nested-skill");
    }

    #[tokio::test]
    async fn loads_skills_from_codex_dir_when_not_git_repo() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let work_dir = tempfile::tempdir().expect("tempdir");

        write_skill_at(
            &work_dir
                .path()
                .join(REPO_ROOT_CONFIG_DIR_NAME)
                .join(SKILLS_DIR_NAME),
            "local",
            "local-skill",
            "from cwd",
        );

        let mut cfg = make_config(&codex_home).await;
        cfg.cwd = work_dir.path().to_path_buf();

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(outcome.skills[0].name, "local-skill");
        assert_eq!(outcome.skills[0].scope, SkillScope::Repo);
    }

    #[tokio::test]
    async fn deduplicates_by_name_preferring_repo_over_user() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let repo_dir = tempfile::tempdir().expect("tempdir");

        let status = Command::new("git")
            .arg("init")
            .current_dir(repo_dir.path())
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed");

        write_skill(&codex_home, "user", "dupe-skill", "from user");
        write_skill_at(
            &repo_dir
                .path()
                .join(REPO_ROOT_CONFIG_DIR_NAME)
                .join(SKILLS_DIR_NAME),
            "repo",
            "dupe-skill",
            "from repo",
        );

        let mut cfg = make_config(&codex_home).await;
        cfg.cwd = repo_dir.path().to_path_buf();

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(outcome.skills[0].name, "dupe-skill");
        assert_eq!(outcome.skills[0].scope, SkillScope::Repo);
    }

    #[tokio::test]
    async fn loads_system_skills_when_present() {
        let codex_home = tempfile::tempdir().expect("tempdir");

        write_system_skill(&codex_home, "system", "dupe-skill", "from system");
        write_skill(&codex_home, "user", "dupe-skill", "from user");

        let cfg = make_config(&codex_home).await;
        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(outcome.skills[0].description, "from user");
        assert_eq!(outcome.skills[0].scope, SkillScope::User);
    }

    #[tokio::test]
    async fn repo_skills_search_does_not_escape_repo_root() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let outer_dir = tempfile::tempdir().expect("tempdir");
        let repo_dir = outer_dir.path().join("repo");
        fs::create_dir_all(&repo_dir).unwrap();

        write_skill_at(
            &outer_dir
                .path()
                .join(REPO_ROOT_CONFIG_DIR_NAME)
                .join(SKILLS_DIR_NAME),
            "outer",
            "outer-skill",
            "from outer",
        );

        let status = Command::new("git")
            .arg("init")
            .current_dir(&repo_dir)
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed");

        let mut cfg = make_config(&codex_home).await;
        cfg.cwd = repo_dir;

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 0);
    }

    #[tokio::test]
    async fn loads_skills_when_cwd_is_file_in_repo() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let repo_dir = tempfile::tempdir().expect("tempdir");

        let status = Command::new("git")
            .arg("init")
            .current_dir(repo_dir.path())
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed");

        write_skill_at(
            &repo_dir
                .path()
                .join(REPO_ROOT_CONFIG_DIR_NAME)
                .join(SKILLS_DIR_NAME),
            "repo",
            "repo-skill",
            "from repo",
        );
        let file_path = repo_dir.path().join("some-file.txt");
        fs::write(&file_path, "contents").unwrap();

        let mut cfg = make_config(&codex_home).await;
        cfg.cwd = file_path;

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(outcome.skills[0].name, "repo-skill");
        assert_eq!(outcome.skills[0].scope, SkillScope::Repo);
    }

    #[tokio::test]
    async fn non_git_repo_skills_search_does_not_walk_parents() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let outer_dir = tempfile::tempdir().expect("tempdir");
        let nested_dir = outer_dir.path().join("nested/inner");
        fs::create_dir_all(&nested_dir).unwrap();

        write_skill_at(
            &outer_dir
                .path()
                .join(REPO_ROOT_CONFIG_DIR_NAME)
                .join(SKILLS_DIR_NAME),
            "outer",
            "outer-skill",
            "from outer",
        );

        let mut cfg = make_config(&codex_home).await;
        cfg.cwd = nested_dir;

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 0);
    }

    #[tokio::test]
    async fn loads_skills_from_system_cache_when_present() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let work_dir = tempfile::tempdir().expect("tempdir");

        write_system_skill(&codex_home, "system", "system-skill", "from system");

        let mut cfg = make_config(&codex_home).await;
        cfg.cwd = work_dir.path().to_path_buf();

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(outcome.skills[0].name, "system-skill");
        assert_eq!(outcome.skills[0].scope, SkillScope::System);
    }

    #[tokio::test]
    async fn skill_roots_include_admin_with_lowest_priority_on_unix() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let cfg = make_config(&codex_home).await;

        let scopes: Vec<SkillScope> = skill_roots(&cfg)
            .into_iter()
            .map(|root| root.scope)
            .collect();
        let mut expected = vec![SkillScope::User, SkillScope::System];
        if cfg!(unix) {
            expected.push(SkillScope::Admin);
        }
        assert_eq!(scopes, expected);
    }

    #[tokio::test]
    async fn deduplicates_by_name_preferring_system_over_admin() {
        let system_dir = tempfile::tempdir().expect("tempdir");
        let admin_dir = tempfile::tempdir().expect("tempdir");

        write_skill_at(system_dir.path(), "system", "dupe-skill", "from system");
        write_skill_at(admin_dir.path(), "admin", "dupe-skill", "from admin");

        let outcome = load_skills_from_roots([
            SkillRoot {
                path: system_dir.path().to_path_buf(),
                scope: SkillScope::System,
            },
            SkillRoot {
                path: admin_dir.path().to_path_buf(),
                scope: SkillScope::Admin,
            },
        ]);

        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(outcome.skills[0].name, "dupe-skill");
        assert_eq!(outcome.skills[0].scope, SkillScope::System);
    }

    #[tokio::test]
    async fn deduplicates_by_name_preferring_user_over_system() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let work_dir = tempfile::tempdir().expect("tempdir");

        write_skill(&codex_home, "user", "dupe-skill", "from user");
        write_system_skill(&codex_home, "system", "dupe-skill", "from system");

        let mut cfg = make_config(&codex_home).await;
        cfg.cwd = work_dir.path().to_path_buf();

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(outcome.skills[0].name, "dupe-skill");
        assert_eq!(outcome.skills[0].scope, SkillScope::User);
    }

    #[tokio::test]
    async fn deduplicates_by_name_preferring_repo_over_system() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let repo_dir = tempfile::tempdir().expect("tempdir");

        let status = Command::new("git")
            .arg("init")
            .current_dir(repo_dir.path())
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed");

        write_skill_at(
            &repo_dir
                .path()
                .join(REPO_ROOT_CONFIG_DIR_NAME)
                .join(SKILLS_DIR_NAME),
            "repo",
            "dupe-skill",
            "from repo",
        );
        write_system_skill(&codex_home, "system", "dupe-skill", "from system");

        let mut cfg = make_config(&codex_home).await;
        cfg.cwd = repo_dir.path().to_path_buf();

        let outcome = load_skills(&cfg);
        assert!(
            outcome.errors.is_empty(),
            "unexpected errors: {:?}",
            outcome.errors
        );
        assert_eq!(outcome.skills.len(), 1);
        assert_eq!(outcome.skills[0].name, "dupe-skill");
        assert_eq!(outcome.skills[0].scope, SkillScope::Repo);
    }
}
