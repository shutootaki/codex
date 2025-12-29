//! # Skills インジェクション
//!
//! ユーザーが選択したスキルをプロンプトに注入する処理を担当。
//! スキルファイル（SKILL.md）の内容を読み込み、モデルに渡す形式に変換する。
//!
//! ## 処理の流れ
//! 1. ユーザー入力からスキル指定を抽出
//! 2. 各スキルファイルの内容を非同期で読み込み
//! 3. ResponseItem 形式に変換してモデルに渡す

use std::collections::HashSet;

use crate::skills::SkillLoadOutcome;
use crate::skills::SkillMetadata;
use crate::user_instructions::SkillInstructions;
use codex_protocol::models::ResponseItem;
use codex_protocol::user_input::UserInput;
use tokio::fs;  // 非同期ファイルシステム操作（tokio ランタイム用）

// =============================================================================
// SkillInjections - 注入結果
// =============================================================================
/// スキル注入処理の結果を保持する構造体。
///
/// 成功したスキル（items）と、読み込みに失敗した警告（warnings）を含む。
#[derive(Debug, Default)]
pub(crate) struct SkillInjections {
    /// モデルに注入するレスポンスアイテム。
    /// 各アイテムはスキルの内容（SKILL.md 全体）を含む。
    pub(crate) items: Vec<ResponseItem>,

    /// スキル読み込みに失敗した場合の警告メッセージ。
    pub(crate) warnings: Vec<String>,
}

// =============================================================================
// メイン関数
// =============================================================================
/// ユーザー入力からスキル注入を構築する。
///
/// ## async/await について（非同期処理）
/// `async fn` は非同期関数を定義する。呼び出すと `Future` を返し、
/// `.await` で結果を待つ。
///
/// Python の `async def` / `await` と同じ概念。
/// JavaScript の `async function` / `await` とも同様。
///
/// ```rust
/// // 呼び出し側
/// let result = build_skill_injections(inputs, skills).await;
/// ```
///
/// ## 引数の型について
/// - `&[UserInput]`: スライス（配列への参照）。Python の `list[UserInput]` に近い。
/// - `Option<&SkillLoadOutcome>`: 値があるかもしれない参照。
pub(crate) async fn build_skill_injections(
    inputs: &[UserInput],
    skills: Option<&SkillLoadOutcome>,
) -> SkillInjections {
    // 入力が空ならスキップ
    if inputs.is_empty() {
        return SkillInjections::default();
    }

    // スキルがなければスキップ
    // `let Some(x) = expr else { ... }` はパターンマッチングによる早期リターン。
    let Some(outcome) = skills else {
        return SkillInjections::default();
    };

    // ユーザーが明示的に指定したスキルを収集
    let mentioned_skills = collect_explicit_skill_mentions(inputs, &outcome.skills);
    if mentioned_skills.is_empty() {
        return SkillInjections::default();
    }

    // 結果を格納する構造体を初期化
    // `Vec::with_capacity()` は事前に容量を確保して再割り当てを減らす最適化
    let mut result = SkillInjections {
        items: Vec::with_capacity(mentioned_skills.len()),
        warnings: Vec::new(),
    };

    // 各スキルファイルを非同期で読み込み
    for skill in mentioned_skills {
        // `tokio::fs::read_to_string` は非同期版の fs::read_to_string
        // `.await` で読み込み完了を待つ（その間、他のタスクを実行可能）
        match fs::read_to_string(&skill.path).await {
            Ok(contents) => {
                // 読み込み成功: ResponseItem に変換して追加
                // `ResponseItem::from()` は `From` トレイトによる型変換
                result.items.push(ResponseItem::from(SkillInstructions {
                    name: skill.name,
                    // `to_string_lossy()` は非UTF-8文字を � に置換して String 化
                    // `into_owned()` は Cow<str> を String に変換
                    path: skill.path.to_string_lossy().into_owned(),
                    contents,
                }));
            }
            Err(err) => {
                // 読み込み失敗: 警告メッセージを記録
                // `{err:#}` は詳細なエラー表示（原因チェーンを含む）
                let message = format!(
                    "Failed to load skill {} at {}: {err:#}",
                    skill.name,
                    skill.path.display()
                );
                result.warnings.push(message);
            }
        }
    }

    result
}

// =============================================================================
// ヘルパー関数
// =============================================================================
/// ユーザー入力から明示的なスキル指定を収集する。
///
/// ユーザーが `$skill-name` のように指定したスキルを抽出し、
/// 利用可能なスキルリストと照合して返す。
fn collect_explicit_skill_mentions(
    inputs: &[UserInput],
    skills: &[SkillMetadata],
) -> Vec<SkillMetadata> {
    let mut selected: Vec<SkillMetadata> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for input in inputs {
        // パターンマッチングで UserInput::Skill バリアントを抽出
        // `if let ... && ... && ...` は複数条件を組み合わせたパターンマッチング
        //
        // 条件:
        // 1. input が UserInput::Skill 型である
        // 2. その名前がまだ seen に含まれていない（重複排除）
        // 3. 利用可能なスキルリストに存在する
        if let UserInput::Skill { name, path } = input
            && seen.insert(name.clone())  // insert は新規なら true を返す
            && let Some(skill) = skills.iter().find(|s| s.name == *name && s.path == *path)
        {
            selected.push(skill.clone());
        }
    }

    selected
}
