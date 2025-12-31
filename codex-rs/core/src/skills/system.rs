//! # システムスキル
//!
//! コンパイル時にバイナリに埋め込まれた組み込みスキルを管理する。
//! 初回起動時に ~/.codex/skills/.system/ に展開される。
//!
//! ## 仕組み
//! 1. `include_dir!` マクロでコンパイル時にスキルファイルをバイナリに埋め込み
//! 2. 起動時にフィンガープリント（ハッシュ）をチェック
//! 3. 変更があれば古いファイルを削除して再展開
//! 4. マーカーファイルにフィンガープリントを保存

use codex_utils_absolute_path::AbsolutePathBuf;
use include_dir::Dir; // ディレクトリをコンパイル時に埋め込むためのクレート
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;

use thiserror::Error; // エラー型を簡単に定義するためのクレート

// =============================================================================
// コンパイル時埋め込み
// =============================================================================
/// システムスキルディレクトリをコンパイル時にバイナリに埋め込む。
///
/// ## `include_dir!` マクロについて
/// このマクロは **コンパイル時** に指定されたディレクトリの全ファイルを
/// バイナリに埋め込む。実行時にファイルシステムからの読み取りは不要。
///
/// `$CARGO_MANIFEST_DIR` は Cargo.toml があるディレクトリを指す環境変数。
/// Python でいえば `__file__` のディレクトリに相当。
///
/// これにより、バイナリ単体でスキルを配布できる。
const SYSTEM_SKILLS_DIR: Dir =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/skills/assets/samples");

// =============================================================================
// 定数
// =============================================================================
const SYSTEM_SKILLS_DIR_NAME: &str = ".system"; // システムスキルを展開するディレクトリ名
const SKILLS_DIR_NAME: &str = "skills";
const SYSTEM_SKILLS_MARKER_FILENAME: &str = ".codex-system-skills.marker"; // フィンガープリント保存用
const SYSTEM_SKILLS_MARKER_SALT: &str = "v1"; // フィンガープリント計算のソルト（バージョン管理用）

// =============================================================================
// パス解決
// =============================================================================

/// システムスキルのキャッシュディレクトリを返す。
///
/// 通常は `CODEX_HOME/skills/.system`（例: ~/.codex/skills/.system）
pub(crate) fn system_cache_root_dir(codex_home: &Path) -> PathBuf {
    // `AbsolutePathBuf::try_from` は相対パスを絶対パスに変換
    // `.and_then()` は Result/Option のチェーン処理
    // 成功時は次の処理へ、失敗時はエラーをそのまま伝播
    AbsolutePathBuf::try_from(codex_home)
        .and_then(|codex_home| system_cache_root_dir_abs(&codex_home))
        .map(AbsolutePathBuf::into_path_buf)
        // エラー時はフォールバックとして単純なパス結合を使用
        .unwrap_or_else(|_| {
            codex_home
                .join(SKILLS_DIR_NAME)
                .join(SYSTEM_SKILLS_DIR_NAME)
        })
}

/// 絶対パス版のキャッシュディレクトリ計算（内部ヘルパー）。
fn system_cache_root_dir_abs(codex_home: &AbsolutePathBuf) -> std::io::Result<AbsolutePathBuf> {
    codex_home
        .join(SKILLS_DIR_NAME)? // ? でエラーを伝播
        .join(SYSTEM_SKILLS_DIR_NAME)
}

// =============================================================================
// システムスキルのインストール
// =============================================================================

/// 埋め込まれたシステムスキルを `CODEX_HOME/skills/.system` にインストールする。
///
/// ## 処理の流れ
/// 1. 埋め込みスキルのフィンガープリント（ハッシュ）を計算
/// 2. マーカーファイルと比較（一致すればスキップ）
/// 3. 不一致なら既存ディレクトリを削除
/// 4. 埋め込みファイルを展開
/// 5. 新しいフィンガープリントをマーカーファイルに保存
///
/// ## なぜフィンガープリントを使うか
/// 起動のたびに全ファイルを書き直すのは非効率。
/// フィンガープリントが一致すれば、スキルは変更されていないと判断できる。
pub(crate) fn install_system_skills(codex_home: &Path) -> Result<(), SystemSkillsError> {
    // パスを絶対パスに正規化
    let codex_home = AbsolutePathBuf::try_from(codex_home)
        .map_err(|source| SystemSkillsError::io("normalize codex home dir", source))?;

    // skills ディレクトリを作成（なければ）
    let skills_root_dir = codex_home
        .join(SKILLS_DIR_NAME)
        .map_err(|source| SystemSkillsError::io("resolve skills root dir", source))?;
    // `create_dir_all` は mkdir -p 相当（親ディレクトリも作成）
    fs::create_dir_all(skills_root_dir.as_path())
        .map_err(|source| SystemSkillsError::io("create skills root dir", source))?;

    // システムスキルの展開先
    let dest_system = system_cache_root_dir_abs(&codex_home)
        .map_err(|source| SystemSkillsError::io("resolve system skills cache root dir", source))?;

    // マーカーファイルのパス
    let marker_path = dest_system
        .join(SYSTEM_SKILLS_MARKER_FILENAME)
        .map_err(|source| SystemSkillsError::io("resolve system skills marker path", source))?;

    // 埋め込みスキルのフィンガープリントを計算
    let expected_fingerprint = embedded_system_skills_fingerprint();

    // フィンガープリントが一致すればスキップ（変更なし）
    // `.is_ok_and()` は Result が Ok かつ条件を満たすかをチェック
    if dest_system.as_path().is_dir()
        && read_marker(&marker_path).is_ok_and(|marker| marker == expected_fingerprint)
    {
        return Ok(()); // 何もせずに成功を返す
    }

    // --- 再インストールが必要 ---

    // 既存のシステムスキルディレクトリを削除
    if dest_system.as_path().exists() {
        fs::remove_dir_all(dest_system.as_path())
            .map_err(|source| SystemSkillsError::io("remove existing system skills dir", source))?;
    }

    // 埋め込みファイルを展開
    write_embedded_dir(&SYSTEM_SKILLS_DIR, &dest_system)?;

    // 新しいフィンガープリントをマーカーファイルに書き込み
    fs::write(marker_path.as_path(), format!("{expected_fingerprint}\n"))
        .map_err(|source| SystemSkillsError::io("write system skills marker", source))?;

    Ok(())
}

// =============================================================================
// ヘルパー関数
// =============================================================================

/// マーカーファイルからフィンガープリントを読み取る。
fn read_marker(path: &AbsolutePathBuf) -> Result<String, SystemSkillsError> {
    Ok(fs::read_to_string(path.as_path())
        .map_err(|source| SystemSkillsError::io("read system skills marker", source))?
        .trim() // 末尾の改行を除去
        .to_string())
}

/// 埋め込みシステムスキルのフィンガープリント（ハッシュ値）を計算する。
///
/// 全ファイルのパスと内容をハッシュ化して、一意の識別子を生成。
/// これにより、スキルファイルが変更されたかどうかを検出できる。
fn embedded_system_skills_fingerprint() -> String {
    // 各エントリを (パス, ファイル内容のハッシュ) のタプルに変換
    let mut items: Vec<(String, Option<u64>)> = SYSTEM_SKILLS_DIR
        .entries()
        .iter()
        .map(|entry| match entry {
            // ディレクトリの場合はハッシュなし
            include_dir::DirEntry::Dir(dir) => (dir.path().to_string_lossy().to_string(), None),
            // ファイルの場合は内容をハッシュ化
            include_dir::DirEntry::File(file) => {
                let mut file_hasher = DefaultHasher::new();
                // `.hash()` でハッシャーにデータを追加
                file.contents().hash(&mut file_hasher);
                (
                    file.path().to_string_lossy().to_string(),
                    // `.finish()` でハッシュ値を取得
                    Some(file_hasher.finish()),
                )
            }
        })
        .collect();

    // パス順でソート（順序を安定させるため）
    // `sort_unstable_by` は安定ソートではないが高速
    items.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));

    // 全体をハッシュ化
    let mut hasher = DefaultHasher::new();
    // ソルトを追加（フィンガープリント形式のバージョン管理用）
    SYSTEM_SKILLS_MARKER_SALT.hash(&mut hasher);
    for (path, contents_hash) in items {
        path.hash(&mut hasher);
        contents_hash.hash(&mut hasher);
    }

    // 16進数文字列として返す
    // `{:x}` は小文字16進数フォーマット
    format!("{:x}", hasher.finish())
}

/// 埋め込みディレクトリをディスクに書き出す。
///
/// 再帰的にディレクトリ構造を保持しながらファイルを展開する。
fn write_embedded_dir(dir: &Dir<'_>, dest: &AbsolutePathBuf) -> Result<(), SystemSkillsError> {
    // 出力先ディレクトリを作成
    fs::create_dir_all(dest.as_path())
        .map_err(|source| SystemSkillsError::io("create system skills dir", source))?;

    // 各エントリを処理
    for entry in dir.entries() {
        match entry {
            // サブディレクトリの場合は再帰的に処理
            include_dir::DirEntry::Dir(subdir) => {
                let subdir_dest = dest.join(subdir.path()).map_err(|source| {
                    SystemSkillsError::io("resolve system skills subdir", source)
                })?;
                fs::create_dir_all(subdir_dest.as_path()).map_err(|source| {
                    SystemSkillsError::io("create system skills subdir", source)
                })?;
                // 再帰呼び出し
                write_embedded_dir(subdir, dest)?;
            }
            // ファイルの場合は内容を書き出し
            include_dir::DirEntry::File(file) => {
                let path = dest.join(file.path()).map_err(|source| {
                    SystemSkillsError::io("resolve system skills file", source)
                })?;
                // 親ディレクトリがなければ作成
                if let Some(parent) = path.as_path().parent() {
                    fs::create_dir_all(parent).map_err(|source| {
                        SystemSkillsError::io("create system skills file parent", source)
                    })?;
                }
                // ファイル内容を書き出し
                // `file.contents()` は &[u8]（埋め込まれたバイト列）を返す
                fs::write(path.as_path(), file.contents())
                    .map_err(|source| SystemSkillsError::io("write system skill file", source))?;
            }
        }
    }

    Ok(())
}

// =============================================================================
// エラー型
// =============================================================================

/// システムスキル関連のエラー。
///
/// ## `#[derive(Error)]` について（thiserror クレート）
/// `thiserror` クレートにより、`std::error::Error` トレイトの実装を
/// 自動生成できる。`#[error("...")]` でエラーメッセージを定義。
///
/// Python の Exception クラスを継承するのに似ている。
#[derive(Debug, Error)]
pub(crate) enum SystemSkillsError {
    /// IOエラー。`action` に何をしようとしていたかを記録。
    /// `#[source]` は原因となったエラーを指定（エラーチェーン用）。
    #[error("io error while {action}: {source}")]
    Io {
        action: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl SystemSkillsError {
    /// IOエラーを作成するヘルパー関数。
    fn io(action: &'static str, source: std::io::Error) -> Self {
        Self::Io { action, source }
    }
}
