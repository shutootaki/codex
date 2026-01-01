//! # Skills レンダリング
//!
//! 利用可能なスキル一覧をマークダウン形式で生成する。
//! 生成されたマークダウンはシステムプロンプトの一部としてモデルに渡される。
//!
//! ## 役割
//! - スキルのメタデータ（名前、説明、パス）を一覧表示
//! - スキルの使用ルールやガイダンスを提供
//! - モデルがスキルを適切に使用するための指示を含む

use crate::skills::model::SkillMetadata;

/// スキル一覧セクションをマークダウン形式で生成する。
///
/// ## 戻り値
/// - `Some(String)`: スキルが存在する場合、マークダウン文字列を返す
/// - `None`: スキルが空の場合（セクションを生成しない）
///
/// ## Option を返す理由
/// スキルがない場合にセクション自体を省略するため。
/// 呼び出し側で `if let Some(section) = render_skills_section(&skills)` のように
/// 条件付きで処理できる。
pub fn render_skills_section(skills: &[SkillMetadata]) -> Option<String> {
    // スキルが空なら None を返す（セクションを生成しない）
    if skills.is_empty() {
        return None;
    }

    // マークダウン行を格納するベクタ
    let mut lines: Vec<String> = Vec::new();

    // セクションヘッダー
    lines.push("## Skills".to_string());
    lines.push("These skills are discovered at startup from multiple local sources. Each entry includes a name, description, and file path so you can open the source for full instructions.".to_string());

    // 各スキルをリスト形式で追加
    for skill in skills {
        // パス区切り文字を正規化（Windowsの \ を / に変換）
        let path_str = skill.path.to_string_lossy().replace('\\', "/");
        // `.as_str()` は String から &str への変換
        let name = skill.name.as_str();
        let description = skill.description.as_str();
        // `format!` マクロで文字列をフォーマット（Python の f-string に相当）
        lines.push(format!("- {name}: {description} (file: {path_str})"));
    }

    // スキル使用ルールのガイダンスを追加
    // `r###"..."###` は raw string リテラル（エスケープ不要、複数行OK）
    // Python の r""" や """ に相当
    lines.push(
        r###"- Discovery: Available skills are listed in project docs and may also appear in a runtime "## Skills" section (name + description + file path). These are the sources of truth; skill bodies live on disk at the listed paths.
- Trigger rules: If the user names a skill (with `$SkillName` or plain text) OR the task clearly matches a skill's description, you must use that skill for that turn. Multiple mentions mean use them all. Do not carry skills across turns unless re-mentioned.
- Missing/blocked: If a named skill isn't in the list or the path can't be read, say so briefly and continue with the best fallback.
- How to use a skill (progressive disclosure):
  1) After deciding to use a skill, open its `SKILL.md`. Read only enough to follow the workflow.
  2) If `SKILL.md` points to extra folders such as `references/`, load only the specific files needed for the request; don't bulk-load everything.
  3) If `scripts/` exist, prefer running or patching them instead of retyping large code blocks.
  4) If `assets/` or templates exist, reuse them instead of recreating from scratch.
- Description as trigger: The YAML `description` in `SKILL.md` is the primary trigger signal; rely on it to decide applicability. If unsure, ask a brief clarification before proceeding.
- Coordination and sequencing:
  - If multiple skills apply, choose the minimal set that covers the request and state the order you'll use them.
  - Announce which skill(s) you're using and why (one short line). If you skip an obvious skill, say why.
- Context hygiene:
  - Keep context small: summarize long sections instead of pasting them; only load extra files when needed.
  - Avoid deeply nested references; prefer one-hop files explicitly linked from `SKILL.md`.
  - When variants exist (frameworks, providers, domains), pick only the relevant reference file(s) and note that choice.
- Safety and fallback: If a skill can't be applied cleanly (missing files, unclear instructions), state the issue, pick the next-best approach, and continue."###
            .to_string(),
    );

    // 行を改行で結合して返す
    // `.join("\n")` は Python の "\n".join(lines) に相当
    Some(lines.join("\n"))
}
