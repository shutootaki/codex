//! トランスクリプト相対のマルチクリック選択ヘルパー。
//!
//! このモジュールは**レンダリングされたトランスクリプトモデル**（折り返されたトランスクリプト行
//! + コンテンツ列）に基づいてマルチクリック選択を実装する。ターミナルバッファ座標ではない。
//!
//! ターミナル `(row, col)` 座標は一時的: スクロール、リサイズ、リフロー（特にストリーミング中）により
//! トランスクリプトコンテンツの特定部分が画面上のどこに表示されるかが変わる。
//! トランスクリプト相対の選択座標はフラット化され折り返されたトランスクリプト行モデルに
//! アンカーされているため安定している。
//!
//! 統合ノート:
//! - マウスイベント → `TranscriptSelectionPoint` マッピングは `app.rs` で処理。
//! - このモジュール:
//!   - 近接するクリックをマルチクリックシーケンスにグループ化
//!   - 現在のクリック数に基づいて選択を拡張
//!   - `HistoryCell::display_lines(width)` から折り返されたトランスクリプト行を再構築し、
//!     選択拡張が画面上の折り返しと一致するようにする。
//! - TUI2ではドラッグでトランスクリプト選択を開始。シングルクリックはアンカーを保存するが
//!   「アクティブ」な選択（headなし）ではない。マルチクリック選択（ダブル/トリプル/クアッド+）は
//!   即座にアクティブな選択を*作成する*。
//!
//! 計算量 / コストモデル:
//! - シングルクリックは `O(1)`（クリック追跡 + キャレット配置のみ）
//! - マルチクリック拡張は現在の折り返されたトランスクリプトビューを再構築
//!   (`O(レンダリングされたトランスクリプトテキスト総量)`) し、選択が画面に表示されている
//!   もの*今*と一致するようにする（ストリーミング/リフローを含む）。
//!
//! 座標:
//! - `TranscriptSelectionPoint::line_index` はフラット化され折り返されたトランスクリプト行
//!   （「表示行」）へのインデックス。
//! - `TranscriptSelectionPoint::column` は0ベースの*コンテンツ*列オフセットで、
//!   トランスクリプトガター (`TRANSCRIPT_GUTTER_COLS`) の直後から測定。
//! - 選択のエンドポイントは包含的（選択されたセルの閉区間を表す）。
//!
//! 選択拡張はUI指向:
//! - 「単語」選択は表示幅 (`unicode_width`) と軽量な文字クラスヒューリスティックを使用。
//! - 「段落」選択は連続する非空の折り返し行に基づく。
//! - 「セル」選択は単一の履歴セルに属するすべての折り返し行を選択
//!   (`HistoryCell::display_lines` が返す単位)。

use crate::history_cell::HistoryCell;
use crate::transcript_selection::TRANSCRIPT_GUTTER_COLS;
use crate::transcript_selection::TranscriptSelection;
use crate::transcript_selection::TranscriptSelectionPoint;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_line;
use ratatui::text::Line;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use unicode_width::UnicodeWidthChar;

/// トランスクリプトビューポート用のステートフルなマルチクリック選択ハンドラー。
///
/// マウスイベント間でマルチクリックシーケンスを推論するために必要なクリック履歴を保持。
/// 実際の選択拡張は現在のトランスクリプトコンテンツから計算され、画面上の折り返しと
/// 整合性を保つ。
#[derive(Debug, Default)]
pub(crate) struct TranscriptMultiClick {
    /// 最近のクリックを追跡し、マルチクリックシーケンスを推論できるようにする。
    ///
    /// これは意図的に選択自体から分離されている: 選択のエンドポイントは
    /// `TranscriptSelection` が所有し、マルチクリックの動作は一時的な入力ジェスチャー状態。
    tracker: ClickTracker,
}

impl TranscriptMultiClick {
    /// トランスクリプトビューポート内での左ボタンマウスダウンを処理。
    ///
    /// `App` のマウスハンドラーから呼び出されることを想定。
    ///
    /// 動作:
    /// - 常に基礎となる選択アンカーを更新（[`crate::transcript_selection::on_mouse_down`] に委譲）
    ///   し、ドラッグがこのポイントから拡張できるようにする。
    /// - クリックを潜在的なマルチクリックシーケンスの一部として追跡。
    /// - マルチクリック（ダブル/トリプル/クアッド+）時は、選択を拡張されたアクティブな
    ///   選択（単語/行/段落）で置き換える。
    ///
    /// `width` はレンダリングに使用されるトランスクリプトビューポート幅と一致する必要があり、
    /// 折り返し（したがって単語/段落の境界）がユーザーに見えるものと揃うようにする。
    ///
    /// 選択が変更されたかどうかを返す（再描画を要求するかどうかの判断に有用）。
    pub(crate) fn on_mouse_down(
        &mut self,
        selection: &mut TranscriptSelection,
        cells: &[Arc<dyn HistoryCell>],
        width: u16,
        point: Option<TranscriptSelectionPoint>,
    ) -> bool {
        self.on_mouse_down_at(selection, cells, width, point, Instant::now())
    }

    /// ユーザーがドラッグ選択中であることをハンドラーに通知。
    ///
    /// ドラッグ選択はマルチクリックシーケンスの継続として解釈されるべきではないため、
    /// カーソルがアンカーポイントから離れたらクリック履歴をリセット。
    ///
    /// `point` はトランスクリプトコンテンツ座標にクランプされていることを想定。
    /// `point` が `None` の場合、これはno-op。
    pub(crate) fn on_mouse_drag(
        &mut self,
        selection: &TranscriptSelection,
        point: Option<TranscriptSelectionPoint>,
    ) {
        let (Some(anchor), Some(point)) = (selection.anchor, point) else {
            return;
        };

        // 一部のターミナルはボタンが押されている間、非常に小さなカーソル動作に対して
        // `Drag` イベントを発行する（例: クリック中のトラックパッドの「ジッター」）。
        // *任意の*ドラッグでクリックシーケンスをリセットするとダブル/クアッドクリックが
        // トリガーしにくくなるため、カーソルがアンカーから意味のある距離離れた場合のみ
        // ドラッグジェスチャーとして扱う。
        let moved_to_other_wrapped_line = point.line_index != anchor.line_index;
        let moved_far_enough_horizontally =
            point.column.abs_diff(anchor.column) > ClickTracker::MAX_COLUMN_DISTANCE;
        if moved_to_other_wrapped_line || moved_far_enough_horizontally {
            self.tracker.reset();
        }
    }

    /// [`Self::on_mouse_down`] のテスト可能な実装。
    ///
    /// `now` を入力として受け取ることで、テストでクリックグループ化を決定論的にする。
    ///
    /// 高レベルフロー（呼び出し元が選択ステートマシンを頭でシミュレートしなくて済むよう
    /// ここに記載）:
    /// 1. [`crate::transcript_selection::on_mouse_down`] を使用して基礎となる選択状態を更新。
    ///    TUI2ではアンカーを記録し、headをクリアして、シングルクリックが可視選択を
    ///    残さないようにする。
    /// 2. クリックがトランスクリプトコンテンツ外（`point == None`）の場合、
    ///    クリックトラッカーをリセットして戻る。
    /// 3. クリックをトラッカーに登録してクリック数を推論。
    /// 4. マルチクリック（`>= 2`）の場合、*現在の*折り返されたトランスクリプトビューから
    ///    拡張された選択を計算し、アクティブな選択（`anchor` + `head` 設定）で上書き。
    fn on_mouse_down_at(
        &mut self,
        selection: &mut TranscriptSelection,
        cells: &[Arc<dyn HistoryCell>],
        width: u16,
        point: Option<TranscriptSelectionPoint>,
        now: Instant,
    ) -> bool {
        let before = *selection;

        let selection_changed = crate::transcript_selection::on_mouse_down(selection, point);
        let Some(point) = point else {
            self.tracker.reset();
            return selection_changed;
        };

        let click_count = self.tracker.register_click(point, now);
        if click_count == 1 {
            return *selection != before;
        }

        *selection = selection_for_click(cells, width, point, click_count);
        *selection != before
    }
}

/// 最近のクリックを追跡し、マルチクリック数を推論できるようにする。
#[derive(Debug, Default)]
struct ClickTracker {
    /// 最後に観測されたクリック（近接するクリックをシーケンスにグループ化するために使用）。
    last_click: Option<Click>,
}

/// マルチクリックグループ化に使用される単一のクリックイベント。
#[derive(Debug, Clone, Copy)]
struct Click {
    /// トランスクリプト座標でのクリック位置。
    point: TranscriptSelectionPoint,
    /// 現在のシーケンスのクリック数。
    click_count: u8,
    /// クリックが発生した時刻（マルチクリックグループ化の境界に使用）。
    at: Instant,
}

impl ClickTracker {
    /// シーケンスの一部と見なされるクリック間の最大時間間隔。
    const MAX_DELAY: Duration = Duration::from_millis(650);
    /// マルチクリックグループ化で「同じクリックターゲット」と見なされる
    /// 最大水平移動距離（トランスクリプト*コンテンツ*列単位）。
    const MAX_COLUMN_DISTANCE: u16 = 4;

    /// クリック履歴をリセットし、次のクリックが新しいシーケンスを開始するようにする。
    fn reset(&mut self) {
        self.last_click = None;
    }

    /// クリックを記録し、このシーケンスの推論されたクリック数を返す。
    ///
    /// クリックがグループ化される条件:
    /// - 時間的に近い（`MAX_DELAY`）、かつ
    /// - 同じトランスクリプト折り返し行をターゲット、かつ
    /// - ほぼ同じコンテンツ列で発生（`MAX_COLUMN_DISTANCE`）、
    ///   シーケンス後半のクリックほど許容度が増加
    ///
    /// 返されるカウントは `u8::MAX` で飽和（`>= 4` のバケットのみを気にする）。
    fn register_click(&mut self, point: TranscriptSelectionPoint, now: Instant) -> u8 {
        let mut click_count = 1u8;
        if let Some(prev) = self.last_click
            && now.duration_since(prev.at) <= Self::MAX_DELAY
            && prev.point.line_index == point.line_index
            && prev.point.column.abs_diff(point.column) <= max_column_distance(prev.click_count)
        {
            click_count = prev.click_count.saturating_add(1);
        }

        self.last_click = Some(Click {
            point,
            click_count,
            at: now,
        });

        click_count
    }
}

/// 既存のクリックシーケンスを継続するための列距離許容値。
///
/// 選択が拡張された後は意図的にグループ化を緩和: ユーザーが「行全体」または「段落」
/// ステップにいるとき、ほぼ同一の列を要求するとクアッドクリックがトリガーしにくくなる。
/// ユーザーは既にハイライトされた行の別の場所を自然にクリックできるため。
fn max_column_distance(prev_click_count: u8) -> u16 {
    match prev_click_count {
        0 | 1 => ClickTracker::MAX_COLUMN_DISTANCE,
        2 => ClickTracker::MAX_COLUMN_DISTANCE.saturating_mul(2),
        _ => u16::MAX,
    }
}

/// クリック（および推論された `click_count`）をトランスクリプト選択に拡張。
///
/// これはマルチクリック動作の核心。拡張された選択では、選択境界がレンダリングされた
/// トランスクリプトモデル（生のソース文字列でもターミナルバッファ座標でもない）と
/// 揃うよう、履歴セルから現在の折り返されたトランスクリプトビューを再構築。
///
/// `TranscriptSelectionPoint::column` はコンテンツ座標で解釈:
/// 列0はトランスクリプトガター (`TRANSCRIPT_GUTTER_COLS`) の直後の最初の列。
/// 返される選択列は指定された `width` のコンテンツ幅にクランプ。
///
/// ジェスチャーマッピング:
/// - ダブルクリックはクリックされた折り返し行の「単語っぽい」連続を選択
/// - トリプルクリックは折り返し行全体を選択
/// - クアッド+クリックは含まれる段落を選択（連続する非空の折り返し行、
///   空/スペーサー行は段落区切りとして扱う）
/// - クイント+クリックは履歴セル全体を選択
///
/// 返される選択は常に「アクティブ」（`anchor` と `head` の両方が設定）。これは
/// TUI2の通常のシングルクリック動作（ドラッグが選択をアクティブにするまでアンカーのみ保存）
/// と意図的に異なる。
///
/// 防御性:
/// - トランスクリプトが空、または折り返しが行を生成しない場合、マルチクリックが
///   「選択なし」を生成しないよう、`point` でのキャレットのような選択にフォールバック
/// - `point` が折り返し行リストの終端を超えて参照する場合、スクロール/リサイズ/リフロー中も
///   動作が安定するよう最後の折り返し行にクランプ
fn selection_for_click(
    cells: &[Arc<dyn HistoryCell>],
    width: u16,
    point: TranscriptSelectionPoint,
    click_count: u8,
) -> TranscriptSelection {
    if click_count == 1 {
        return TranscriptSelection {
            anchor: Some(point),
            head: Some(point),
        };
    }

    // `width` はガターを含む総ビューポート幅。選択列はコンテンツ相対なので、
    // 選択可能な最大*コンテンツ*列を計算。
    let max_content_col = width
        .saturating_sub(1)
        .saturating_sub(TRANSCRIPT_GUTTER_COLS);

    // トランスクリプトがレンダリングする同じ論理行ストリームを再構築。これにより
    // 拡張境界が現在のストリーミング出力と現在の折り返し幅と揃う。
    let (lines, line_cell_index) = build_transcript_lines_with_cell_index(cells, width);
    if lines.is_empty() {
        return TranscriptSelection {
            anchor: Some(point),
            head: Some(point),
        };
    }

    // 折り返された*視覚的な*行に基づいて拡張し、トリプル/クアッド/クイントクリック
    // 選択が現在の折り返し幅を尊重するようにする。
    let (wrapped, wrapped_cell_index) = word_wrap_lines_with_cell_index(
        &lines,
        &line_cell_index,
        RtOptions::new(width.max(1) as usize),
    );
    if wrapped.is_empty() {
        return TranscriptSelection {
            anchor: Some(point),
            head: Some(point),
        };
    }

    // ターゲット行と列の両方を現在の折り返しビューにクランプ。これは
    // ライブストリーミング中に重要: UIがクリックをクランプしてから拡張を計算するまでの間に
    // トランスクリプトが成長する可能性があるため。
    let line_index = point.line_index.min(wrapped.len().saturating_sub(1));
    let point = TranscriptSelectionPoint::new(line_index, point.column.min(max_content_col));

    if click_count == 2 {
        let Some((start, end)) =
            word_bounds_in_wrapped_line(&wrapped[line_index], TRANSCRIPT_GUTTER_COLS, point.column)
        else {
            return TranscriptSelection {
                anchor: Some(point),
                head: Some(point),
            };
        };
        return TranscriptSelection {
            anchor: Some(TranscriptSelectionPoint::new(
                line_index,
                start.min(max_content_col),
            )),
            head: Some(TranscriptSelectionPoint::new(
                line_index,
                end.min(max_content_col),
            )),
        };
    }

    if click_count == 3 {
        return TranscriptSelection {
            anchor: Some(TranscriptSelectionPoint::new(line_index, 0)),
            head: Some(TranscriptSelectionPoint::new(line_index, max_content_col)),
        };
    }

    if click_count == 4 {
        let (start_line, end_line) =
            paragraph_bounds_in_wrapped_lines(&wrapped, TRANSCRIPT_GUTTER_COLS, line_index)
                .unwrap_or((line_index, line_index));
        return TranscriptSelection {
            anchor: Some(TranscriptSelectionPoint::new(start_line, 0)),
            head: Some(TranscriptSelectionPoint::new(end_line, max_content_col)),
        };
    }

    let Some((start_line, end_line)) =
        cell_bounds_in_wrapped_lines(&wrapped_cell_index, line_index)
    else {
        return TranscriptSelection {
            anchor: Some(point),
            head: Some(point),
        };
    };
    TranscriptSelection {
        anchor: Some(TranscriptSelectionPoint::new(start_line, 0)),
        head: Some(TranscriptSelectionPoint::new(end_line, max_content_col)),
    }
}

/// トランスクリプト履歴セルをUIが使用するのと同じ行ストリームにフラット化。
///
/// `App::build_transcript_lines` のセマンティクスをミラー: 非継続セル間に空白の
/// スペーサー行を挿入し、単語/段落境界がユーザーに見えるものと一致するようにする。
#[cfg(test)]
fn build_transcript_lines(cells: &[Arc<dyn HistoryCell>], width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut has_emitted_lines = false;

    for cell in cells {
        let cell_lines = cell.display_lines(width);
        if cell_lines.is_empty() {
            continue;
        }

        if !cell.is_stream_continuation() {
            if has_emitted_lines {
                // `App` は異なる（非継続）履歴セル間にスペーサーを挿入する。
                // ここでもそれを保持し、段落検出がユーザーに見えるものと一致するようにする。
                lines.push(Line::from(""));
            } else {
                has_emitted_lines = true;
            }
        }

        lines.extend(cell_lines);
    }

    lines
}

/// [`build_transcript_lines`] と同様だが、各行から元の履歴セルインデックスへの
/// マッピングも返す。
///
/// このマッピングにより、折り返された視覚的な行インデックスを使用して
/// 「履歴セル全体を選択」を実装できる。
fn build_transcript_lines_with_cell_index(
    cells: &[Arc<dyn HistoryCell>],
    width: u16,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut line_cell_index: Vec<Option<usize>> = Vec::new();
    let mut has_emitted_lines = false;

    for (cell_index, cell) in cells.iter().enumerate() {
        let cell_lines = cell.display_lines(width);
        if cell_lines.is_empty() {
            continue;
        }

        if !cell.is_stream_continuation() {
            if has_emitted_lines {
                lines.push(Line::from(""));
                line_cell_index.push(None);
            } else {
                has_emitted_lines = true;
            }
        }

        line_cell_index.extend(std::iter::repeat_n(Some(cell_index), cell_lines.len()));
        lines.extend(cell_lines);
    }

    debug_assert_eq!(lines.len(), line_cell_index.len());
    (lines, line_cell_index)
}

/// 行を折り返し、各行から履歴セルインデックスへのマッピングを引き継ぐ。
///
/// [`word_wrap_lines_borrowed`] の動作をミラーし、選択拡張がレンダリングと
/// 同じ折り返し行モデルを使用するようにする。
fn word_wrap_lines_with_cell_index<'a, O>(
    lines: &'a [Line<'a>],
    line_cell_index: &[Option<usize>],
    width_or_options: O,
) -> (Vec<Line<'a>>, Vec<Option<usize>>)
where
    O: Into<RtOptions<'a>>,
{
    debug_assert_eq!(lines.len(), line_cell_index.len());

    let base_opts: RtOptions<'a> = width_or_options.into();
    let mut out: Vec<Line<'a>> = Vec::new();
    let mut out_cell_index: Vec<Option<usize>> = Vec::new();

    let mut first = true;
    for (line, cell_index) in lines.iter().zip(line_cell_index.iter().copied()) {
        let opts = if first {
            base_opts.clone()
        } else {
            base_opts
                .clone()
                .initial_indent(base_opts.subsequent_indent.clone())
        };

        let wrapped = word_wrap_line(line, opts);
        out_cell_index.extend(std::iter::repeat_n(cell_index, wrapped.len()));
        out.extend(wrapped);
        first = false;
    }

    debug_assert_eq!(out.len(), out_cell_index.len());
    (out, out_cell_index)
}

/// 単一の履歴セルに属する連続した折り返し行の範囲に拡張。
///
/// `line_index` は折り返し行座標。`line_index` の行がスペーサー（セルインデックスなし）
/// の場合、最も近い前のセルを選択し、フォールバックとして次のセルを選択。
fn cell_bounds_in_wrapped_lines(
    wrapped_cell_index: &[Option<usize>],
    line_index: usize,
) -> Option<(usize, usize)> {
    let total = wrapped_cell_index.len();
    if total == 0 {
        return None;
    }

    let mut target = line_index.min(total.saturating_sub(1));
    let mut cell_index = wrapped_cell_index[target];
    if cell_index.is_none() {
        if let Some(found) = (0..target)
            .rev()
            .find(|idx| wrapped_cell_index[*idx].is_some())
        {
            target = found;
            cell_index = wrapped_cell_index[found];
        } else if let Some(found) =
            (target + 1..total).find(|idx| wrapped_cell_index[*idx].is_some())
        {
            target = found;
            cell_index = wrapped_cell_index[found];
        }
    }
    let cell_index = cell_index?;

    let mut start = target;
    while start > 0 && wrapped_cell_index[start - 1] == Some(cell_index) {
        start = start.saturating_sub(1);
    }

    let mut end = target;
    while end + 1 < total && wrapped_cell_index[end + 1] == Some(cell_index) {
        end = end.saturating_add(1);
    }

    Some((start, end))
}

/// 「単語っぽい」選択に使用される大まかな文字クラス。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordCharClass {
    /// 任意の空白（連続した実行として選択）。
    Whitespace,
    /// 英数字 + トークン句読点（パス/識別子/URL）。
    Token,
    /// その他すべて。
    Other,
}

/// UI指向の「単語っぽい」選択のために文字を分類。
///
/// 意図的に完全なUnicode単語境界セマンティクスを試みない。
/// ターミナルトランスクリプトのインタラクション向けにチューニングされており、
/// 「単語」は多くの場合、識別子、パス、URL、句読点隣接トークンを意味する。
fn word_char_class(ch: char) -> WordCharClass {
    if ch.is_whitespace() {
        return WordCharClass::Whitespace;
    }

    let is_token = ch.is_alphanumeric()
        || matches!(
            ch,
            '_' | '-'
                | '.'
                | '/'
                | '\\'
                | ':'
                | '@'
                | '#'
                | '$'
                | '%'
                | '+'
                | '='
                | '?'
                | '&'
                | '~'
                | '*'
        );
    if is_token {
        WordCharClass::Token
    } else {
        WordCharClass::Other
    }
}

/// スタイル付き `Line` をプレーンテキスト表現に連結。
///
/// マルチクリック選択はレンダリングされたテキストコンテンツ（ユーザーに見えるもの）で
/// 動作し、スタイリングとは独立。
fn flatten_line_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// `prefix_cols` 表示列に対応するUTF-8バイトインデックスを検索。
///
/// クリックや段落区切りを解釈する際にトランスクリプトガター/プレフィックスを
/// 除外するために使用。列計算はターミナルレイアウトと一致させるため、
/// バイトオフセットではなく表示幅を使用。
fn byte_index_after_prefix_cols(text: &str, prefix_cols: u16) -> usize {
    let mut col = 0u16;
    for (idx, ch) in text.char_indices() {
        if col >= prefix_cols {
            return idx;
        }
        col = col.saturating_add(UnicodeWidthChar::width(ch).unwrap_or(0) as u16);
    }
    text.len()
}

/// クリック下の「単語」の（包含的な）コンテンツ列境界を計算。
///
/// *レンダリングされた*行の観点で定義:
/// - `line` は視覚的な折り返しトランスクリプト行（ガター/プレフィックスを含む）。
/// - `prefix_cols` は左側で無視する表示列数（トランスクリプトガター）。
/// - `click_col` は0ベースのコンテンツ列で、ガターの直後の最初の列から測定。
///
/// 返される `(start, end)` はコンテンツ列での包含的な選択範囲 (`0..=max_content_col`)
/// で、[`TranscriptSelectionPoint::column`] の値として適している。
fn word_bounds_in_wrapped_line(
    line: &Line<'_>,
    prefix_cols: u16,
    click_col: u16,
) -> Option<(u16, u16)> {
    // プレーンテキストにフラット化し、表示される各グリフを（表示幅による）列範囲に
    // マッピングすることで単語境界を計算。これは基礎となるスパンに複数のスタイルが
    // あっても、ユーザーに見えるものをミラーする。
    //
    // 注意/制限:
    // - 書記素クラスタではなく `char` レベルで動作。ほとんどのトランスクリプト
    //   コンテンツ（ASCIIっぽいトークン/パス/URL）にはこれで十分。
    // - ゼロ幅文字はスキップ。ターミナルセルを占有しない。
    let full = flatten_line_text(line);
    let prefix_byte = byte_index_after_prefix_cols(&full, prefix_cols);
    let content = &full[prefix_byte..];

    let mut cells: Vec<(char, u16, u16)> = Vec::new();
    let mut col = 0u16;
    for ch in content.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if w == 0 {
            continue;
        }
        let start = col;
        let end = col.saturating_add(w);
        cells.push((ch, start, end));
        col = end;
    }

    let total_width = col;
    if cells.is_empty() || total_width == 0 {
        return None;
    }

    let click_col = click_col.min(total_width.saturating_sub(1));
    let mut idx = cells
        .iter()
        .position(|(_, start, end)| click_col >= *start && click_col < *end)
        .unwrap_or(0);
    if idx >= cells.len() {
        idx = cells.len().saturating_sub(1);
    }

    let class = word_char_class(cells[idx].0);

    let mut start_idx = idx;
    while start_idx > 0 && word_char_class(cells[start_idx - 1].0) == class {
        start_idx = start_idx.saturating_sub(1);
    }

    let mut end_idx = idx;
    while end_idx + 1 < cells.len() && word_char_class(cells[end_idx + 1].0) == class {
        end_idx = end_idx.saturating_add(1);
    }

    let start_col = cells[start_idx].1;
    let end_col = cells[end_idx].2.saturating_sub(1);
    Some((start_col, end_col))
}

/// `line_index` を囲む段落の（包含的な）折り返し行インデックス境界を計算。
///
/// 段落は*折り返された視覚的な行*（基礎となる履歴セルではない）で定義:
/// 段落は連続する非空の折り返し行であり、空の行（トランスクリプトガター/プレフィックスを
/// トリミング後）が段落を区切る。
///
/// `line_index` が区切り行を指している場合、最も近い前の非区切り行を選択し、
/// 履歴セル間のスペーサー行でのクアッドクリックが上の段落を選択するようにする
/// （一般的なターミナルUXの期待と一致）。
fn paragraph_bounds_in_wrapped_lines(
    lines: &[Line<'_>],
    prefix_cols: u16,
    line_index: usize,
) -> Option<(usize, usize)> {
    if lines.is_empty() {
        return None;
    }

    // 段落区切りはトランスクリプトガターをスキップした後に判定されるため、
    // ガタープレフィックスのみを含む行も「空」としてカウント。
    let is_break = |idx: usize| -> bool {
        let full = flatten_line_text(&lines[idx]);
        let prefix_byte = byte_index_after_prefix_cols(&full, prefix_cols);
        full[prefix_byte..].trim().is_empty()
    };

    let mut target = line_index.min(lines.len().saturating_sub(1));
    if is_break(target) {
        // 履歴セル間に挿入されたスペーサー行については上の段落を優先。
        // 上に段落がない場合、下の次の段落にフォールバック。
        target = (0..target)
            .rev()
            .find(|idx| !is_break(*idx))
            .or_else(|| (target + 1..lines.len()).find(|idx| !is_break(*idx)))?;
    }

    let mut start = target;
    while start > 0 && !is_break(start - 1) {
        start = start.saturating_sub(1);
    }

    let mut end = target;
    while end + 1 < lines.len() && !is_break(end + 1) {
        end = end.saturating_add(1);
    }

    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::text::Line;

    #[derive(Debug)]
    struct StaticCell {
        lines: Vec<Line<'static>>,
        is_stream_continuation: bool,
    }

    impl StaticCell {
        fn new(lines: Vec<Line<'static>>) -> Self {
            Self {
                lines,
                is_stream_continuation: false,
            }
        }

        fn continuation(lines: Vec<Line<'static>>) -> Self {
            Self {
                lines,
                is_stream_continuation: true,
            }
        }
    }

    impl HistoryCell for StaticCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            self.lines.clone()
        }

        fn is_stream_continuation(&self) -> bool {
            self.is_stream_continuation
        }
    }

    #[test]
    fn word_bounds_respects_prefix_and_word_classes() {
        let line = Line::from("› hello   world");
        let prefix_cols = 2;

        assert_eq!(
            word_bounds_in_wrapped_line(&line, prefix_cols, 1),
            Some((0, 4))
        );
        assert_eq!(
            word_bounds_in_wrapped_line(&line, prefix_cols, 6),
            Some((5, 7))
        );
        assert_eq!(
            word_bounds_in_wrapped_line(&line, prefix_cols, 9),
            Some((8, 12))
        );
    }

    #[test]
    fn paragraph_bounds_selects_contiguous_non_empty_lines() {
        let lines = vec![
            Line::from("› first"),
            Line::from("  second"),
            Line::from(""),
            Line::from("› third"),
        ];
        let prefix_cols = 2;

        assert_eq!(
            paragraph_bounds_in_wrapped_lines(&lines, prefix_cols, 1),
            Some((0, 1))
        );
        assert_eq!(
            paragraph_bounds_in_wrapped_lines(&lines, prefix_cols, 2),
            Some((0, 1))
        );
        assert_eq!(
            paragraph_bounds_in_wrapped_lines(&lines, prefix_cols, 3),
            Some((3, 3))
        );
    }

    #[test]
    fn click_sequence_expands_selection_word_then_line_then_paragraph() {
        let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(StaticCell::new(vec![
            Line::from("› first"),
            Line::from("  second"),
        ]))];
        let width = 20;

        let mut multi = TranscriptMultiClick::default();
        let t0 = Instant::now();
        let point = TranscriptSelectionPoint::new(1, 1);
        let mut selection = TranscriptSelection::default();

        multi.on_mouse_down_at(&mut selection, &cells, width, Some(point), t0);
        assert_eq!(selection.anchor, Some(point));
        assert_eq!(selection.head, None);

        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(10),
        );
        assert_eq!(
            selection
                .anchor
                .zip(selection.head)
                .map(|(a, h)| (a.line_index, a.column, h.column)),
            Some((1, 0, 5))
        );

        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(20),
        );
        let max_content_col = width
            .saturating_sub(1)
            .saturating_sub(TRANSCRIPT_GUTTER_COLS);
        assert_eq!(
            selection
                .anchor
                .zip(selection.head)
                .map(|(a, h)| (a.line_index, a.column, h.column)),
            Some((1, 0, max_content_col))
        );

        // 最後のクリックはハイライトされた行の他の場所に着地できる。
        // それでもマルチクリックシーケンスの継続として扱いたい。
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(TranscriptSelectionPoint::new(point.line_index, 10)),
            t0 + Duration::from_millis(30),
        );
        assert_eq!(
            selection
                .anchor
                .zip(selection.head)
                .map(|(a, h)| (a.line_index, h.line_index)),
            Some((0, 1))
        );
    }

    #[test]
    fn double_click_on_whitespace_selects_whitespace_run() {
        let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(StaticCell::new(vec![Line::from(
            "› hello   world",
        )]))];
        let width = 40;

        let mut multi = TranscriptMultiClick::default();
        let t0 = Instant::now();
        let point = TranscriptSelectionPoint::new(0, 6);
        let mut selection = TranscriptSelection::default();

        multi.on_mouse_down_at(&mut selection, &cells, width, Some(point), t0);
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(5),
        );

        assert_eq!(
            selection
                .anchor
                .zip(selection.head)
                .map(|(a, h)| (a.column, h.column)),
            Some((5, 7))
        );
    }

    #[test]
    fn click_sequence_resets_when_click_moves_too_far_horizontally() {
        let cells: Vec<Arc<dyn HistoryCell>> =
            vec![Arc::new(StaticCell::new(vec![Line::from("› hello world")]))];
        let width = 40;

        let mut multi = TranscriptMultiClick::default();
        let t0 = Instant::now();
        let mut selection = TranscriptSelection::default();

        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(TranscriptSelectionPoint::new(0, 0)),
            t0,
        );
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(TranscriptSelectionPoint::new(0, 10)),
            t0 + Duration::from_millis(10),
        );

        assert_eq!(selection.anchor, Some(TranscriptSelectionPoint::new(0, 10)));
        assert_eq!(selection.head, None);
    }

    #[test]
    fn click_sequence_resets_when_click_is_too_slow() {
        let cells: Vec<Arc<dyn HistoryCell>> =
            vec![Arc::new(StaticCell::new(vec![Line::from("› hello world")]))];
        let width = 40;

        let mut multi = TranscriptMultiClick::default();
        let t0 = Instant::now();
        let point = TranscriptSelectionPoint::new(0, 1);
        let mut selection = TranscriptSelection::default();

        multi.on_mouse_down_at(&mut selection, &cells, width, Some(point), t0);
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + ClickTracker::MAX_DELAY + Duration::from_millis(1),
        );

        assert_eq!(selection.anchor, Some(point));
        assert_eq!(selection.head, None);
    }

    #[test]
    fn click_sequence_resets_when_click_changes_line() {
        let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(StaticCell::new(vec![
            Line::from("› first"),
            Line::from("  second"),
        ]))];
        let width = 40;

        let mut multi = TranscriptMultiClick::default();
        let t0 = Instant::now();
        let mut selection = TranscriptSelection::default();

        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(TranscriptSelectionPoint::new(0, 1)),
            t0,
        );
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(TranscriptSelectionPoint::new(1, 1)),
            t0 + Duration::from_millis(10),
        );

        assert_eq!(selection.anchor, Some(TranscriptSelectionPoint::new(1, 1)));
        assert_eq!(selection.head, None);
    }

    #[test]
    fn drag_resets_multi_click_sequence() {
        let cells: Vec<Arc<dyn HistoryCell>> =
            vec![Arc::new(StaticCell::new(vec![Line::from("› hello world")]))];
        let width = 40;

        let mut multi = TranscriptMultiClick::default();
        let t0 = Instant::now();
        let point = TranscriptSelectionPoint::new(0, 1);
        let mut selection = TranscriptSelection::default();

        multi.on_mouse_down_at(&mut selection, &cells, width, Some(point), t0);
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(10),
        );
        assert_eq!(
            selection
                .anchor
                .zip(selection.head)
                .map(|(a, h)| (a.column, h.column)),
            Some((0, 4))
        );

        multi.on_mouse_drag(&selection, Some(TranscriptSelectionPoint::new(0, 10)));
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(20),
        );
        assert_eq!(selection.anchor, Some(point));
        assert_eq!(selection.head, None);
    }

    #[test]
    fn small_drag_jitter_does_not_reset_multi_click_sequence() {
        let cells: Vec<Arc<dyn HistoryCell>> =
            vec![Arc::new(StaticCell::new(vec![Line::from("› hello world")]))];
        let width = 40;

        let mut multi = TranscriptMultiClick::default();
        let t0 = Instant::now();
        let point = TranscriptSelectionPoint::new(0, 1);
        let mut selection = TranscriptSelection::default();

        multi.on_mouse_down_at(&mut selection, &cells, width, Some(point), t0);
        multi.on_mouse_drag(&selection, Some(TranscriptSelectionPoint::new(0, 2)));

        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(10),
        );
        assert_eq!(
            selection
                .anchor
                .zip(selection.head)
                .map(|(a, h)| (a.column, h.column)),
            Some((0, 4))
        );
    }

    #[test]
    fn paragraph_selects_nearest_non_empty_when_clicking_break_line() {
        let cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(StaticCell::new(vec![Line::from("› first")])),
            Arc::new(StaticCell::new(vec![Line::from("› second")])),
        ];
        let width = 40;

        let mut multi = TranscriptMultiClick::default();
        let t0 = Instant::now();
        let mut selection = TranscriptSelection::default();

        // インデックス1は2つの非継続セル間に挿入されたスペーサー行。
        let point = TranscriptSelectionPoint::new(1, 0);
        multi.on_mouse_down_at(&mut selection, &cells, width, Some(point), t0);
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(10),
        );
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(20),
        );
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(30),
        );

        assert_eq!(
            selection
                .anchor
                .zip(selection.head)
                .map(|(a, h)| (a.line_index, h.line_index)),
            Some((0, 0))
        );
    }

    #[test]
    fn build_transcript_lines_inserts_spacer_between_non_continuation_cells() {
        let cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(StaticCell::new(vec![Line::from("› first")])),
            Arc::new(StaticCell::continuation(vec![Line::from("  cont")])),
            Arc::new(StaticCell::new(vec![Line::from("› second")])),
        ];
        let width = 40;

        let lines = build_transcript_lines(&cells, width);
        let text: Vec<String> = lines.iter().map(flatten_line_text).collect();
        assert_eq!(text, vec!["› first", "  cont", "", "› second"]);
    }

    #[test]
    fn quint_click_selects_entire_history_cell() {
        let cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(StaticCell::new(vec![
                Line::from("› first"),
                Line::from(""),
                Line::from("  second"),
            ])),
            Arc::new(StaticCell::new(vec![Line::from("› other")])),
        ];
        let width = 40;

        let mut multi = TranscriptMultiClick::default();
        let t0 = Instant::now();
        let point = TranscriptSelectionPoint::new(2, 1);
        let mut selection = TranscriptSelection::default();

        multi.on_mouse_down_at(&mut selection, &cells, width, Some(point), t0);
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(10),
        );
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(20),
        );
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(point),
            t0 + Duration::from_millis(30),
        );
        multi.on_mouse_down_at(
            &mut selection,
            &cells,
            width,
            Some(TranscriptSelectionPoint::new(2, 10)),
            t0 + Duration::from_millis(40),
        );

        let max_content_col = width
            .saturating_sub(1)
            .saturating_sub(TRANSCRIPT_GUTTER_COLS);
        assert_eq!(
            selection.anchor.zip(selection.head).map(|(a, h)| (
                a.line_index,
                a.column,
                h.line_index,
                h.column
            )),
            Some((0, 0, 2, max_content_col))
        );
    }

    #[test]
    fn quint_click_on_spacer_selects_cell_above() {
        let cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(StaticCell::new(vec![Line::from("› first")])),
            Arc::new(StaticCell::new(vec![Line::from("› second")])),
        ];
        let width = 40;

        let mut multi = TranscriptMultiClick::default();
        let t0 = Instant::now();
        let mut selection = TranscriptSelection::default();

        // インデックス1は2つの非継続セル間に挿入されたスペーサー行。
        let point = TranscriptSelectionPoint::new(1, 0);
        for (idx, dt) in [0u64, 10, 20, 30, 40].into_iter().enumerate() {
            multi.on_mouse_down_at(
                &mut selection,
                &cells,
                width,
                Some(TranscriptSelectionPoint::new(
                    point.line_index,
                    if idx < 3 { 0 } else { (idx as u16) * 5 },
                )),
                t0 + Duration::from_millis(dt),
            );
        }

        let max_content_col = width
            .saturating_sub(1)
            .saturating_sub(TRANSCRIPT_GUTTER_COLS);
        assert_eq!(
            selection.anchor.zip(selection.head).map(|(a, h)| (
                a.line_index,
                a.column,
                h.line_index,
                h.column
            )),
            Some((0, 0, 0, max_content_col))
        );
    }
}
