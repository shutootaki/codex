//! # Skills マネージャー
//!
//! スキルの読み込み結果をキャッシュして効率的にアクセスするためのマネージャー。
//! ワーキングディレクトリごとにスキル一覧をキャッシュする。

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::RwLock;  // 読み書きロック（マルチスレッド対応）

use crate::skills::SkillLoadOutcome;
use crate::skills::loader::load_skills_from_roots;
use crate::skills::loader::skill_roots_for_cwd;
use crate::skills::system::install_system_skills;

// =============================================================================
// SkillsManager
// =============================================================================
/// スキルの読み込み結果をキャッシュするマネージャー。
///
/// ## RwLock について（並行処理）
/// `RwLock<T>` は読み書きロックで、以下の特性を持つ：
/// - 複数のスレッドが同時に読み取り可能
/// - 書き込みは排他的（1スレッドのみ）
///
/// Python の `threading.RLock()` に似ているが、読み取りと書き込みを区別する。
/// JavaScript はシングルスレッドなので直接対応する概念がない。
///
/// ## なぜキャッシュが必要か
/// スキルの読み込みはファイルシステム操作を伴うため比較的コストが高い。
/// 同じ cwd に対する複数回のリクエストをキャッシュで効率化する。
pub struct SkillsManager {
    /// Codex のホームディレクトリ（通常 ~/.codex）
    codex_home: PathBuf,

    /// cwd（カレントディレクトリ）をキーとしたスキル読み込み結果のキャッシュ
    /// `RwLock` でスレッドセーフにアクセス
    cache_by_cwd: RwLock<HashMap<PathBuf, SkillLoadOutcome>>,
}

/// `impl` ブロックで構造体にメソッドを定義する。
/// Python のクラスメソッド定義に相当。
impl SkillsManager {
    /// 新しい SkillsManager を作成する。
    ///
    /// ## 関連関数（Associated Function）について
    /// `Self` は現在の型（ここでは SkillsManager）を指す。
    /// `self` を引数に取らない関数は「関連関数」と呼ばれ、
    /// Python の `@classmethod` や `@staticmethod` に似ている。
    /// `SkillsManager::new(path)` のように呼び出す。
    pub fn new(codex_home: PathBuf) -> Self {
        // システムスキル（組み込みスキル）をインストール
        // エラー時はログ出力のみで続行
        if let Err(err) = install_system_skills(&codex_home) {
            tracing::error!("failed to install system skills: {err}");
        }

        // 構造体を初期化して返す
        // `Self { ... }` は `SkillsManager { ... }` と同等
        Self {
            codex_home,
            // `RwLock::new()` でロックを初期化
            cache_by_cwd: RwLock::new(HashMap::new()),
        }
    }

    /// 指定された cwd に対するスキルを取得（キャッシュ使用）。
    ///
    /// ## `&self` について
    /// `&self` は自身への不変参照。Python の `self` に相当するが、
    /// 明示的に参照であることを示す。
    /// - `self`: 所有権を取得（呼び出し後に使用不可）
    /// - `&self`: 不変参照（読み取りのみ）
    /// - `&mut self`: 可変参照（変更可能）
    pub fn skills_for_cwd(&self, cwd: &Path) -> SkillLoadOutcome {
        self.skills_for_cwd_with_options(cwd, false)
    }

    /// 指定された cwd に対するスキルを取得（オプション付き）。
    ///
    /// `force_reload: true` でキャッシュを無視して再読み込み。
    pub fn skills_for_cwd_with_options(&self, cwd: &Path, force_reload: bool) -> SkillLoadOutcome {
        // --- キャッシュの読み取り ---
        // `.read()` で読み取りロックを取得。複数スレッドが同時に読める。
        // `Result<RwLockReadGuard, PoisonError>` を返す。
        // PoisonError は別スレッドがパニックした場合に発生。
        let cached = match self.cache_by_cwd.read() {
            Ok(cache) => cache.get(cwd).cloned(),
            // `into_inner()` はロックの中身を取得（パニックしたスレッドのデータを復旧）
            Err(err) => err.into_inner().get(cwd).cloned(),
        };

        // キャッシュがあり、強制リロードでなければキャッシュを返す
        // `if !cond && let Some(x) = ...` はパターンマッチングと条件の組み合わせ
        if !force_reload && let Some(outcome) = cached {
            return outcome;
        }

        // --- スキルの読み込み ---
        let roots = skill_roots_for_cwd(&self.codex_home, cwd);
        let outcome = load_skills_from_roots(roots);

        // --- キャッシュへの書き込み ---
        // `.write()` で書き込みロックを取得。他のスレッドはブロックされる。
        match self.cache_by_cwd.write() {
            Ok(mut cache) => {
                // `mut cache` は可変参照を受け取る。HashMap を変更するため必要。
                cache.insert(cwd.to_path_buf(), outcome.clone());
            }
            Err(err) => {
                err.into_inner().insert(cwd.to_path_buf(), outcome.clone());
            }
        }

        outcome
    }
}
