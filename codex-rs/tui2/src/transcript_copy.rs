//! トランスクリプト選択をクリップボードテキストに変換。
//!
//! コピーはコンテンツ相対選択 (`TranscriptSelectionPoint`) で駆動されるが、
//! トランスクリプトはTUI用にスタイリングと折り返しでレンダリングされる。
//! このモジュールはレンダリングされたトランスクリプト行からクリップボードテキストを
//! 再構築し、ユーザーの期待を保持する:
//!
//! - ソフト折り返しされた文章はコピー時に単一の論理行として扱われる。
//! - コードブロックは意味のあるインデントを保持。
//! - Markdownの「ソースマーカー」はコピー時に出力される（インラインコードにはバッククォート、
//!   コードブロックにはトリプルバッククォートフェンス）、画面上のレンダリングが
//!   異なるスタイルであっても。
//!
//! ## 入力と不変条件
//!
//! クリップボードの再構築はトランスクリプトビューポートでレンダリングされる
//! 同じ*視覚的な行*に対して実行される:
//!
//! - `lines`: ガタースパンを含む折り返されたトランスクリプト `Line`。
//! - `joiner_before`: どの折り返し行が*ソフト折り返し*の継続かを記述する
//!   並列ベクター（折り返し境界で何を挿入するか）。
//! - `(line_index, column)` 選択ポイントは*コンテンツ空間*内（列はガターを除外）。
//!
//! 呼び出し元は `lines` と `joiner_before` を整列させておく必要がある。実際には、
//! `App` は両方を `transcript_render` から取得し、それ自体が各セルの
//! `HistoryCell::transcript_lines_with_joiners` 実装から構築する。
//!
//! ## スタイル派生のMarkdownキュー
//!
//! 忠実性のため、ビューポートがリテラル文字ではなくスタイルを使用してコンテンツを
//! レンダリングしている場合でも、Markdownソースマーカーをコピーする。現在、
//! コピーロジックはレンダリング時に適用するスタイリング（現在はシアンのスパン/行）
//! から「インラインコード」と「コードブロック」の境界を導出する。
//!
//! トランスクリプトのスタイリングが変更された場合（例えば、コードブロックがシアンを
//! 使用しなくなった場合）、クリップボード出力がユーザーの期待と一致し続けるよう
//! `is_code_block_line` と [`span_is_inline_code`] を更新する。
//!
//! 呼び出し元はコピーが可視ビューポート範囲のみをカバーするか
//! （`visible_start..visible_end` を渡して）、トランスクリプト全体をカバーするか
//! （`0..lines.len()` を渡して）を選択できる。
//!
//! UIアフォーダンス（キーバインド検出と画面上の「コピー」ピル）は
//! `transcript_copy_ui` にある。

use ratatui::text::Line;
use ratatui::text::Span;

use crate::history_cell::HistoryCell;
use crate::transcript_selection::TRANSCRIPT_GUTTER_COLS;
use crate::transcript_selection::TranscriptSelection;
use crate::transcript_selection::TranscriptSelectionPoint;
use std::sync::Arc;

/// 現在のトランスクリプト選択をクリップボードテキストにレンダリング。
///
/// これは `App` レベルのヘルパー: 画面上のビューポートと同じルールを使用して
/// 折り返されたトランスクリプト行を再構築し、トランスクリプト全体の範囲
/// （画面外の行を含む）に [`selection_to_copy_text`] を適用する。
pub(crate) fn selection_to_copy_text_for_cells(
    cells: &[Arc<dyn HistoryCell>],
    selection: TranscriptSelection,
    width: u16,
) -> Option<String> {
    let (anchor, head) = selection.anchor.zip(selection.head)?;

    let transcript = crate::transcript_render::build_wrapped_transcript_lines(cells, width);
    let total_lines = transcript.lines.len();
    if total_lines == 0 {
        return None;
    }

    selection_to_copy_text(
        &transcript.lines,
        &transcript.joiner_before,
        anchor,
        head,
        0,
        total_lines,
        width,
    )
}

/// 選択された領域をクリップボードテキストにレンダリング。
///
/// `lines` はTUIでレンダリングされた折り返しトランスクリプト行で、
/// 先頭のガタースパンを含む必要がある。`start`/`end` 列はコンテンツ空間で
/// 表現され（ガターを除外）、エンドポイントが逆の場合は内部で順序付けされる。
///
/// `joiner_before[i]` はソフト折り返しされた文章行の継続である場合に
/// `lines[i]` の*前に*挿入する正確な文字列。これによりコピーが
/// ソフト折り返しされた文章を単一の論理行として扱える。
///
/// 注意:
///
/// - コード/事前整形のランについて、ユーザーが「右端まで」を選択した場合、
///   ビューポート幅を超えて拡張することが許可され、狭いターミナルで
///   切り詰められた論理行を生成しないようにする。
/// - Markdownマーカーはレンダリング時のスタイルから導出される（モジュールドキュメント参照）。
/// - 列計算は表示幅を認識（ワイドグリフは複数列としてカウント）。
///
/// 入力が空の選択を意味する場合、または `width` がガター + 少なくとも1つの
/// コンテンツ列を含むには小さすぎる場合、`None` を返す。
pub(crate) fn selection_to_copy_text(
    lines: &[Line<'static>],
    joiner_before: &[Option<String>],
    start: TranscriptSelectionPoint,
    end: TranscriptSelectionPoint,
    visible_start: usize,
    visible_end: usize,
    width: u16,
) -> Option<String> {
    use ratatui::style::Color;

    if width <= TRANSCRIPT_GUTTER_COLS {
        return None;
    }

    // 選択ポイントはコンテンツ相対座標で表現され、どちらの方向でも提供される可能性がある
    // （「逆方向に」ドラッグ）。残りのロジックが `start <= end` を仮定できるよう、
    // 順方向の `(start, end)` ペアに正規化。
    let (start, end) = order_points(start, end);
    if start == end {
        return None;
    }

    // トランスクリプト `Line` は左ガター（バレット/プレフィックススペース）を含む。
    // 選択列はガターを除外するため、`base_x` を加算して選択列を絶対列に変換。
    let base_x = TRANSCRIPT_GUTTER_COLS;
    let max_x = width.saturating_sub(1);

    let mut out = String::new();
    let mut prev_selected_line: Option<usize> = None;

    // コード/事前整形された視覚的な行のランの周囲にMarkdownフェンスを出力:
    // - ビューポートがスタイル化されていてもクリップボードがソーススタイルマーカー (` ``` `) をキャプチャ
    // - インデントが保持され、エディタでのペーストが安定
    let mut in_code_run = false;

    // `wrote_any` により、すべての決定ポイントで「最初の出力行」を特別扱いせずに
    // セパレーター（改行またはソフト折り返しジョイナー）を処理できる。
    let mut wrote_any = false;

    for line_index in visible_start..visible_end {
        // 選択の行範囲と交差する行のみを考慮。（選択エンドポイントは他の場所でクランプされる。
        // インデックスが存在しない場合、`lines.get(...)` は `None` を返す。）
        if line_index < start.line_index || line_index > end.line_index {
            continue;
        }

        let line = lines.get(line_index)?;

        // コードブロック（およびその他の事前整形コンテンツ）はスタイリングで検出され、
        // 「逐語的な行」としてコピーされる（インラインMarkdownの再エンコードなし）。
        // これにより狭いターミナルでの特別な処理も可能: 「右端まで」を選択すると
        // ビューポートで切り詰められたスライスではなく、完全な論理行をコピーする。
        let is_code_block_line = line.style.fg == Some(Color::Cyan);

        // 行をフラット化して最右の非スペース列を計算。これを使用して:
        // - 末尾の右マージンパディングのコピーを回避
        // - 文章選択をビューポート幅にクランプ
        let flat = line_to_flat(line);
        let text_end = if is_code_block_line {
            last_non_space_col(flat.as_str())
        } else {
            last_non_space_col(flat.as_str()).map(|c| c.min(max_x))
        };

        // 選択エンドポイントをこの特定の視覚的な行の選択範囲に変換:
        // - 最初の行は開始列をクランプ
        // - 最後の行は終了列をクランプ
        // - 中間の行は行全体を選択。
        let line_start_col = if line_index == start.line_index {
            start.column
        } else {
            0
        };
        let line_end_col = if line_index == end.line_index {
            end.column
        } else {
            max_x.saturating_sub(base_x)
        };

        let row_sel_start = base_x.saturating_add(line_start_col).min(max_x);

        // コード/事前整形行の場合、「選択がビューポートの端で終わる」を特別な
        // 「論理行の終わりまでコピー」ケースとして扱う。これにより狭いターミナルで
        // ユーザーが右端にドラッグしたとき、切り詰められたクリップボードコンテンツを生成しない。
        let row_sel_end = if is_code_block_line && line_end_col >= max_x.saturating_sub(base_x) {
            u16::MAX
        } else {
            base_x.saturating_add(line_end_col).min(max_x)
        };
        if row_sel_start > row_sel_end {
            continue;
        }

        let selected_line = if let Some(text_end) = text_end {
            let from_col = row_sel_start.max(base_x);
            let to_col = row_sel_end.min(text_end);
            if from_col > to_col {
                Line::default().style(line.style)
            } else {
                slice_line_by_cols(line, from_col, to_col)
            }
        } else {
            Line::default().style(line.style)
        };

        // 選択された `Line` をMarkdownソースに変換:
        // - 文章の場合: インラインコードスパンをバッククォートで囲む。
        // - コードブロックの場合: インデント/スペースを保持するため生のフラットテキストを返す。
        let line_text = line_to_markdown(&selected_line, is_code_block_line);

        // コード/事前整形ランへの/からの遷移を追跡し、トリプルバッククォートフェンスを出力。
        // コードランは常に改行で先行する文章から分離。
        if is_code_block_line && !in_code_run {
            if wrote_any {
                out.push('\n');
            }
            out.push_str("```");
            out.push('\n');
            in_code_run = true;
            prev_selected_line = None;
            wrote_any = true;
        } else if !is_code_block_line && in_code_run {
            out.push('\n');
            out.push_str("```");
            out.push('\n');
            in_code_run = false;
            prev_selected_line = None;
            wrote_any = true;
        }

        // コードラン内でコピーするとき、選択された各視覚的な行はフェンス内のリテラル行になる
        // （ソフト折り返しの結合なし）。空文字列を行として書き込むことで明示的な空行を保持。
        if in_code_run {
            if wrote_any && (!out.ends_with('\n') || prev_selected_line.is_some()) {
                out.push('\n');
            }
            out.push_str(line_text.as_str());
            prev_selected_line = Some(line_index);
            wrote_any = true;
            continue;
        }

        // 文章パス:
        // - この行が前の選択行のソフト折り返し継続の場合、改行の代わりに
        //   記録されたジョイナー（多くの場合スペース）を挿入。
        // - それ以外の場合、ハード改行を保持するために改行を挿入。
        if wrote_any {
            let joiner = joiner_before.get(line_index).cloned().unwrap_or(None);
            if prev_selected_line == Some(line_index.saturating_sub(1))
                && let Some(joiner) = joiner
            {
                out.push_str(joiner.as_str());
            } else {
                out.push('\n');
            }
        }

        out.push_str(line_text.as_str());
        prev_selected_line = Some(line_index);
        wrote_any = true;
    }

    if in_code_run {
        out.push('\n');
        out.push_str("```");
    }

    (!out.is_empty()).then_some(out)
}

/// 2つの選択エンドポイントをトランスクリプト順序で `(start, end)` に順序付け。
///
/// ドラッグは逆順のエンドポイントを生成する可能性がある。呼び出し元は通常、
/// 視覚的な行を反復処理する前に正規化された範囲を望む。
fn order_points(
    a: TranscriptSelectionPoint,
    b: TranscriptSelectionPoint,
) -> (TranscriptSelectionPoint, TranscriptSelectionPoint) {
    if (b.line_index < a.line_index) || (b.line_index == a.line_index && b.column < a.column) {
        (b, a)
    } else {
        (a, b)
    }
}

/// スタイル付き `Line` をプレーンテキストコンテンツにフラット化。
///
/// カーソル/列の計算およびプレーンテキストのコード行の出力に使用。
fn line_to_flat(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>()
}

/// `flat` 内の最後の非スペース*表示列*を返す（包含的）。
///
/// 表示幅を認識するため、ワイドグリフ（例: CJK）は複数列進む。
///
/// 理由: トランスクリプトレンダリングはビューポート幅までパディングすることが多い。
/// コピーはその右マージンの空白を含めないべき。
fn last_non_space_col(flat: &str) -> Option<u16> {
    use unicode_width::UnicodeWidthChar;

    let mut col: u16 = 0;
    let mut last: Option<u16> = None;
    for ch in flat.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if ch != ' ' {
            let end = col.saturating_add(w.saturating_sub(1));
            last = Some(end);
        }
        col = col.saturating_add(w);
    }
    last
}

/// 表示列範囲を `flat` 内のUTF-8バイト範囲にマッピング。
///
/// 返される範囲は `flat` のスライスおよび元の `Span` 文字列のスライスに適している
/// （スパンローカルオフセットに変換後）。
///
/// Unicodeスカラー値をウォークし、表示幅で進むため、呼び出し元は
/// 選択モデルが使用するのと同じ列セマンティクスに基づいてスライスできる。
fn byte_range_for_cols(flat: &str, start_col: u16, end_col: u16) -> Option<std::ops::Range<usize>> {
    use unicode_width::UnicodeWidthChar;

    // 選択列（バイトではなく表示列）をUTF-8バイト範囲に変換。これは意図的に
    // Unicode幅を認識: ワイドグリフは複数列をカバーするが、1つの `char` と
    // 数バイトを占める。
    //
    // 戦略:
    // - `char_indices()` で `flat` をウォークしながら現在の表示列を追跡。
    // - 開始バイトはレンダリングされた列が `start_col` と交差する最初の文字。
    // - 終了バイトはレンダリングされた列が `end_col` と交差する最後の文字の終わり。
    let mut col: u16 = 0;
    let mut start_byte: Option<usize> = None;
    let mut end_byte: Option<usize> = None;

    for (idx, ch) in flat.char_indices() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        let end = col.saturating_add(w.saturating_sub(1));

        // 開始は包含的: 右端が開始列に到達する最初のグリフを選択。
        if start_byte.is_none() && end >= start_col {
            start_byte = Some(idx);
        }

        // 終了は列空間で包含的。`end_col` 以前の間は終了バイトを拡張し続ける。
        // これは `end_col` の前に始まるが後に終わるワイドグリフも含む。
        if col <= end_col {
            end_byte = Some(idx + ch.len_utf8());
        }

        col = col.saturating_add(w);
        if col > end_col && start_byte.is_some() {
            break;
        }
    }

    match (start_byte, end_byte) {
        (Some(s), Some(e)) if e >= s => Some(s..e),
        _ => None,
    }
}

/// スタイル付き `Line` を表示列でスライスし、スパンごとのスタイルを保持。
///
/// これはMarkdown再エンコード前に使用されるコア「選択 → スタイル付きサブストリング」ヘルパー。
/// 各貢献スパンを独立してスライスすることでスパン間のスタイル混合を回避し、
/// 元の行レベルスタイルで新しい `Line` に再組み立て。
fn slice_line_by_cols(line: &Line<'static>, start_col: u16, end_col: u16) -> Line<'static> {
    // `Line` スパンは独自のスタイルを持つ独立した文字列スライスを格納。スタイリングを
    // 保持しながら列でスライスするため:
    // 1) 行をフラット化し、フラット化された文字列で目的のUTF-8バイト範囲を計算。
    // 2) フラット化された文字列内の各スパンのバイト範囲を計算。
    // 3) 選択範囲と各スパン範囲を交差させ、スパンごとにスライスしてスタイルを保持。
    let flat = line_to_flat(line);
    let mut span_bounds: Vec<(std::ops::Range<usize>, ratatui::style::Style)> = Vec::new();
    let mut acc = 0usize;
    for s in &line.spans {
        let start = acc;
        let text = s.content.as_ref();
        acc += text.len();
        span_bounds.push((start..acc, s.style));
    }

    let Some(range) = byte_range_for_cols(flat.as_str(), start_col, end_col) else {
        return Line::default().style(line.style);
    };

    // フラット化されたバイト範囲を（スパンローカル）スライスに逆変換。
    let start_byte = range.start;
    let end_byte = range.end;
    let mut spans: Vec<ratatui::text::Span<'static>> = Vec::new();
    for (i, (r, style)) in span_bounds.iter().enumerate() {
        let s = r.start;
        let e = r.end;
        if e <= start_byte {
            continue;
        }
        if s >= end_byte {
            break;
        }
        let seg_start = start_byte.max(s);
        let seg_end = end_byte.min(e);
        if seg_end > seg_start {
            let local_start = seg_start - s;
            let local_end = seg_end - s;
            let content = line.spans[i].content.as_ref();
            spans.push(ratatui::text::Span {
                style: *style,
                content: content[local_start..local_end].to_string().into(),
            });
        }
        if e >= end_byte {
            break;
        }
    }
    Line::from(spans).style(line.style)
}

/// Markdownを再構築する際に、スパンを「インラインコード」として扱うかどうか。
///
/// TUI2はシアン前景を使用してインラインコードをレンダリング。リンクもシアンを使用するが
/// 下線付きなので、リンクをバッククォートで囲まないよう下線付きシアンスパンを除外。
fn span_is_inline_code(span: &Span<'_>) -> bool {
    use ratatui::style::Color;

    span.style.fg == Some(Color::Cyan)
        && !span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
}

/// 選択されたスタイル付き `Line` をMarkdown風のソーステキストに逆変換。
///
/// - 文章の場合: ソースマーカーを保持するためインラインコードスパンのランをバッククォートで囲む。
/// - コードブロックの場合: 呼び出し元がラン全体をトリプルバッククォートフェンスで囲むため、
///   生のフラットテキストを出力（追加エスケープなし）。
fn line_to_markdown(line: &Line<'static>, is_code_block: bool) -> String {
    if is_code_block {
        return line_to_flat(line);
    }

    let mut out = String::new();
    let mut in_code = false;
    for span in &line.spans {
        let is_code = span_is_inline_code(span);
        if is_code && !in_code {
            out.push('`');
            in_code = true;
        } else if !is_code && in_code {
            out.push('`');
            in_code = false;
        }
        out.push_str(span.content.as_ref());
    }
    if in_code {
        out.push('`');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::style::Color;
    use ratatui::style::Style;
    use ratatui::style::Stylize;

    #[test]
    fn selection_to_copy_text_returns_none_for_zero_content_width() {
        let lines = vec![Line::from("• Hello")];
        let joiner_before = vec![None];
        let start = TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        };
        let end = TranscriptSelectionPoint {
            line_index: 0,
            column: 1,
        };

        assert_eq!(
            selection_to_copy_text(
                &lines,
                &joiner_before,
                start,
                end,
                0,
                lines.len(),
                TRANSCRIPT_GUTTER_COLS,
            ),
            None
        );
    }

    #[test]
    fn selection_to_copy_text_returns_none_for_empty_selection_point() {
        let lines = vec![Line::from("• Hello")];
        let joiner_before = vec![None];
        let pt = TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        };

        assert_eq!(
            selection_to_copy_text(&lines, &joiner_before, pt, pt, 0, lines.len(), 20),
            None
        );
    }

    #[test]
    fn selection_to_copy_text_orders_reversed_endpoints() {
        let lines = vec![Line::from("• Hello world")];
        let joiner_before = vec![None];

        let start = TranscriptSelectionPoint {
            line_index: 0,
            column: 10,
        };
        let end = TranscriptSelectionPoint {
            line_index: 0,
            column: 6,
        };

        let out = selection_to_copy_text(&lines, &joiner_before, start, end, 0, 1, 80)
            .expect("expected text");

        assert_eq!(out, "world");
    }

    #[test]
    fn copy_selection_soft_wrap_joins_without_newline() {
        let lines = vec![Line::from("• Hello"), Line::from("  world")];
        let joiner_before = vec![None, Some(" ".to_string())];
        let start = TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        };
        let end = TranscriptSelectionPoint {
            line_index: 1,
            column: 100,
        };

        let out = selection_to_copy_text(&lines, &joiner_before, start, end, 0, lines.len(), 20)
            .expect("expected text");

        assert_eq!(out, "Hello world");
    }

    #[test]
    fn copy_selection_wraps_inline_code_in_backticks() {
        let lines = vec![Line::from(vec![
            "• ".into(),
            "Use ".into(),
            ratatui::text::Span::from("foo()").style(Style::new().fg(Color::Cyan)),
            " now".into(),
        ])];
        let joiner_before = vec![None];
        let start = TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        };
        let end = TranscriptSelectionPoint {
            line_index: 0,
            column: 100,
        };

        let out = selection_to_copy_text(&lines, &joiner_before, start, end, 0, 1, 80)
            .expect("expected text");

        assert_eq!(out, "Use `foo()` now");
    }

    #[test]
    fn selection_to_copy_text_for_cells_reconstructs_full_code_line_beyond_viewport() {
        #[derive(Debug)]
        struct FakeCell {
            lines: Vec<Line<'static>>,
            joiner_before: Vec<Option<String>>,
        }

        impl HistoryCell for FakeCell {
            fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
                self.lines.clone()
            }

            fn transcript_lines_with_joiners(
                &self,
                _width: u16,
            ) -> crate::history_cell::TranscriptLinesWithJoiners {
                crate::history_cell::TranscriptLinesWithJoiners {
                    lines: self.lines.clone(),
                    joiner_before: self.joiner_before.clone(),
                }
            }
        }

        let style = Style::new().fg(Color::Cyan);
        let cell = FakeCell {
            lines: vec![Line::from("•     0123456789ABCDEFGHIJ").style(style)],
            joiner_before: vec![None],
        };
        let cells: Vec<std::sync::Arc<dyn HistoryCell>> = vec![std::sync::Arc::new(cell)];

        let width: u16 = 12;
        let max_x = width.saturating_sub(1);
        let viewport_edge_col = max_x.saturating_sub(TRANSCRIPT_GUTTER_COLS);

        let selection = TranscriptSelection {
            anchor: Some(TranscriptSelectionPoint::new(0, 0)),
            head: Some(TranscriptSelectionPoint::new(0, viewport_edge_col)),
        };

        let out =
            selection_to_copy_text_for_cells(&cells, selection, width).expect("expected text");
        assert_eq!(out, "```\n    0123456789ABCDEFGHIJ\n```");
    }

    #[test]
    fn order_points_orders_by_line_then_column() {
        let a = TranscriptSelectionPoint::new(2, 5);
        let b = TranscriptSelectionPoint::new(1, 10);
        assert_eq!(order_points(a, b), (b, a));

        let a = TranscriptSelectionPoint::new(1, 5);
        let b = TranscriptSelectionPoint::new(1, 10);
        assert_eq!(order_points(a, b), (a, b));
    }

    #[test]
    fn line_to_flat_concatenates_spans() {
        let line = Line::from(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(line_to_flat(&line), "abc");
    }

    #[test]
    fn last_non_space_col_counts_display_width() {
        // 「コ」は幅2なので、「コX」は列0..=2を占める。
        assert_eq!(last_non_space_col("コX"), Some(2));
        assert_eq!(last_non_space_col("a  "), Some(0));
        assert_eq!(last_non_space_col("   "), None);
    }

    #[test]
    fn byte_range_for_cols_maps_columns_to_utf8_bytes() {
        let flat = "abcd";
        let range = byte_range_for_cols(flat, 1, 2).expect("range");
        assert_eq!(&flat[range], "bc");

        let flat = "コX";
        let range = byte_range_for_cols(flat, 0, 2).expect("range");
        assert_eq!(&flat[range], "コX");
    }

    #[test]
    fn slice_line_by_cols_preserves_span_styles() {
        let line = Line::from(vec![
            "• ".into(),
            "Hello".red(),
            " ".into(),
            "world".green(),
        ]);

        // 「llo wo」をスライス（スパン境界を跨ぐ）。
        let sliced = slice_line_by_cols(&line, 4, 9);
        assert_eq!(line_to_flat(&sliced), "llo wo");
        assert_eq!(sliced.spans.len(), 3);
        assert_eq!(sliced.spans[0].content.as_ref(), "llo");
        assert_eq!(sliced.spans[0].style.fg, Some(Color::Red));
        assert_eq!(sliced.spans[1].content.as_ref(), " ");
        assert_eq!(sliced.spans[2].content.as_ref(), "wo");
        assert_eq!(sliced.spans[2].style.fg, Some(Color::Green));
    }

    #[test]
    fn span_is_inline_code_excludes_underlined_cyan() {
        let inline_code = Span::from("x").style(Style::new().fg(Color::Cyan));
        assert!(span_is_inline_code(&inline_code));

        let link_like = Span::from("x").style(Style::new().fg(Color::Cyan).underlined());
        assert!(!span_is_inline_code(&link_like));

        let other = Span::from("x").style(Style::new().fg(Color::Green));
        assert!(!span_is_inline_code(&other));
    }

    #[test]
    fn line_to_markdown_wraps_contiguous_inline_code_spans() {
        let line = Line::from(vec![
            "Use ".into(),
            Span::from("foo").style(Style::new().fg(Color::Cyan)),
            Span::from("()").style(Style::new().fg(Color::Cyan)),
            " now".into(),
        ]);
        assert_eq!(line_to_markdown(&line, false), "Use `foo()` now");
    }

    #[test]
    fn copy_selection_preserves_wide_glyphs() {
        let lines = vec![Line::from("• コX")];
        let joiner_before = vec![None];

        let start = TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        };
        let end = TranscriptSelectionPoint {
            line_index: 0,
            column: 2,
        };

        let out = selection_to_copy_text(&lines, &joiner_before, start, end, 0, 1, 80)
            .expect("expected text");

        assert_eq!(out, "コX");
    }

    #[test]
    fn copy_selection_wraps_code_block_in_fences_and_preserves_indent() {
        let style = Style::new().fg(Color::Cyan);
        let lines = vec![
            Line::from("•     fn main() {}").style(style),
            Line::from("      println!(\"hi\");").style(style),
        ];
        let joiner_before = vec![None, None];
        let start = TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        };
        let end = TranscriptSelectionPoint {
            line_index: 1,
            column: 100,
        };

        let out = selection_to_copy_text(&lines, &joiner_before, start, end, 0, lines.len(), 80)
            .expect("expected text");

        assert_eq!(out, "```\n    fn main() {}\n    println!(\"hi\");\n```");
    }

    #[test]
    fn copy_selection_code_block_end_col_at_viewport_edge_copies_full_line() {
        let style = Style::new().fg(Color::Cyan);
        let lines = vec![Line::from("•     0123456789ABCDEFGHIJ").style(style)];
        let joiner_before = vec![None];

        let width: u16 = 12;
        let max_x = width.saturating_sub(1);
        let viewport_edge_col = max_x.saturating_sub(TRANSCRIPT_GUTTER_COLS);

        let start = TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        };
        let end = TranscriptSelectionPoint {
            line_index: 0,
            column: viewport_edge_col,
        };

        let out = selection_to_copy_text(&lines, &joiner_before, start, end, 0, 1, width)
            .expect("expected text");

        assert_eq!(out, "```\n    0123456789ABCDEFGHIJ\n```");
    }

    #[test]
    fn copy_selection_code_block_end_col_before_viewport_edge_copies_partial_line() {
        let style = Style::new().fg(Color::Cyan);
        let lines = vec![Line::from("•     0123456789ABCDEFGHIJ").style(style)];
        let joiner_before = vec![None];

        let width: u16 = 12;

        let start = TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        };
        let end = TranscriptSelectionPoint {
            line_index: 0,
            column: 7,
        };

        let out = selection_to_copy_text(&lines, &joiner_before, start, end, 0, 1, width)
            .expect("expected text");

        assert_eq!(out, "```\n    0123\n```");
    }
}
