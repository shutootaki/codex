//! # Skills データモデル
//!
//! スキル機能で使用するデータ構造を定義する。
//! Python のデータクラスや TypeScript の interface に相当する型定義。

use std::path::PathBuf; // ファイルパスを表す型。OS間の差異を吸収する。

use codex_protocol::protocol::SkillScope; // スキルのスコープ（Repo/User/System/Admin）

// =============================================================================
// SkillMetadata - スキルのメタデータ
// =============================================================================
/// スキルファイル（SKILL.md）から解析されたメタデータを保持する構造体。
///
/// ## derive マクロについて（Rust初心者向け）
/// `#[derive(...)]` は、構造体に自動的にトレイト（インターフェース）を実装する。
/// - `Debug`: `println!("{:?}", skill)` でデバッグ出力可能にする
/// - `Clone`: `.clone()` でディープコピーを作成可能にする（Python の copy.deepcopy 相当）
/// - `PartialEq, Eq`: `==` で比較可能にする
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMetadata {
    /// スキル名（例: "pdf-editor"）。最大64文字。
    pub name: String,

    /// スキルの説明。トリガー条件と用途を記述。最大1024文字。
    /// この説明文を元に、AIがスキルを使用するかどうかを判断する。
    pub description: String,

    /// 短い説明（オプション）。UIでの表示用。
    /// `Option<T>` は Python の `Optional[T]` や TypeScript の `T | null` に相当。
    /// `Some(値)` または `None` のいずれかを持つ。
    pub short_description: Option<String>,

    /// SKILL.mdファイルへの絶対パス。
    /// `PathBuf` は所有権を持つパス型（`String` と `&str` の関係に似ている）。
    pub path: PathBuf,

    /// スキルのスコープ（優先度順: Repo > User > System > Admin）。
    /// 同名スキルが複数存在する場合、優先度の高いものが使われる。
    pub scope: SkillScope,
}

// =============================================================================
// SkillError - スキル読み込みエラー
// =============================================================================
/// スキルファイルの解析に失敗した場合のエラー情報。
/// エラーが発生しても他のスキルの読み込みは継続する（fail-open方式）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillError {
    /// 問題が発生したファイルのパス。
    pub path: PathBuf,

    /// エラーメッセージ（例: "missing YAML frontmatter"）。
    pub message: String,
}

// =============================================================================
// SkillLoadOutcome - スキル読み込み結果
// =============================================================================
/// スキル読み込み処理の結果を保持する構造体。
/// 成功したスキルとエラー情報の両方を含む。
///
/// ## Default トレイトについて
/// `#[derive(Default)]` により、`SkillLoadOutcome::default()` で
/// 空のインスタンスを作成可能になる。
/// Python の `@dataclass` で `field(default_factory=list)` を指定するのに似ている。
#[derive(Debug, Clone, Default)]
pub struct SkillLoadOutcome {
    /// 正常に読み込まれたスキルのリスト。
    /// `Vec<T>` は Python の `list[T]` に相当する可変長配列。
    pub skills: Vec<SkillMetadata>,

    /// 読み込みに失敗したスキルのエラー情報。
    pub errors: Vec<SkillError>,
}
