//! # Skills モジュール
//!
//! Codexの能力を拡張する「スキル」機能を提供するモジュール。
//! スキルはSKILL.mdファイルで定義され、AIエージェントに専門的な知識や
//! ワークフローを提供する。
//!
//! ## モジュール構成
//! - `model`: データ構造（SkillMetadata等）
//! - `loader`: スキルファイルの検出・解析
//! - `manager`: キャッシング付きスキル管理
//! - `injection`: プロンプトへのスキル注入
//! - `render`: スキル一覧のマークダウン生成
//! - `system`: 組み込みシステムスキルのインストール

// =============================================================================
// サブモジュールの宣言
// =============================================================================
// Rustでは `pub mod xxx;` で同じディレクトリ内の xxx.rs または xxx/mod.rs を
// サブモジュールとして公開する。Python の `from . import xxx` に相当。
pub mod injection;
pub mod loader;
pub mod manager;
pub mod model;
pub mod render;
pub mod system;

// =============================================================================
// 再エクスポート (Re-exports)
// =============================================================================
// `pub use` は他モジュールの型/関数をこのモジュールから直接アクセス可能にする。
// Python の `from .injection import SkillInjections` に相当。
//
// `pub(crate)` はクレート内部でのみ公開（外部クレートからはアクセス不可）。
// Python には直接対応する概念がないが、`_` プレフィックスの慣習に近い。

pub(crate) use injection::SkillInjections;
pub(crate) use injection::build_skill_injections;

// `pub use` は完全公開。外部クレートからもアクセス可能。
pub use loader::load_skills;
pub use manager::SkillsManager;
pub use model::SkillError;
pub use model::SkillLoadOutcome;
pub use model::SkillMetadata;
pub use render::render_skills_section;
