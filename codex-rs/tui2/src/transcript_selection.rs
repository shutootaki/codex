//! トランスクリプト選択プリミティブ。
//!
//! トランスクリプト（履歴）ビューポートは折り返し後のフラット化された視覚的な行の
//! リストとしてレンダリングされる。トランスクリプト内の選択はスクロールや
//! ターミナルリサイズを跨いで安定している必要があるため、エンドポイントは
//! *コンテンツ相対*座標で表現される:
//!
//! - `line_index`: フラット化され折り返されたトランスクリプト行（視覚的な行）への
//!   インデックス。
//! - `column`: その視覚的な行内の0ベースオフセットで、ガターの右側の最初の
//!   コンテンツ列から測定。
//!
//! これらの座標は意図的に現在のビューポートと独立している: ユーザーは選択後に
//! スクロールでき、選択は同じ会話コンテンツを参照し続けるべき。
//!
//! クリップボードの再構築は `transcript_copy` で実装されており（画面外の行を含む）、
//! キーバインド検出と画面上のコピーアフォーダンスは `transcript_copy_ui` にある。
//!
//! ## マウス選択セマンティクス
//!
//! トランスクリプトはクリック&ドラッグ選択をサポート。シンプルクリックで
//! 邪魔な1セルハイライトを残さないよう、ドラッグでheadポイントが更新されるまで
//! 選択はアクティブにならない。

use crate::tui::scrolling::TranscriptScroll;

/// トランスクリプトガター（バレット/プレフィックススペース）用に予約された列数。
///
/// トランスクリプトレンダリングは各行の先頭に短いガター（例: `• ` または
/// 継続パディング）を付ける。選択座標は意図的にこのガターを除外し、
/// 選択/コピーがターミナル絶対列ではなくコンテンツ列で動作するようにする。
pub(crate) const TRANSCRIPT_GUTTER_COLS: u16 = 2;

/// インライントランスクリプトビューポート内のコンテンツ相対選択。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TranscriptSelection {
    /// 初期選択ポイント（選択ドラッグが開始した場所）。
    ///
    /// ドラッグ中は固定のまま。ハイライトされた領域は `anchor` と `head` の間のスパン。
    pub(crate) anchor: Option<TranscriptSelectionPoint>,
    /// 現在の選択ポイント（選択ドラッグが現在終了している場所）。
    ///
    /// ユーザーがドラッグするまで `None`。これによりシンプルクリックが
    /// 永続的な選択ハイライトを作成することを防ぐ。
    pub(crate) head: Option<TranscriptSelectionPoint>,
}

/// トランスクリプト選択の単一エンドポイント。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TranscriptSelectionPoint {
    /// フラット化され折り返されたトランスクリプト行へのインデックス。
    pub(crate) line_index: usize,
    /// 0ベースのコンテンツ列（ガターを除外）。
    ///
    /// これはターミナル絶対列ではない: 呼び出し元はレンダリングされた
    /// バッファ行にマッピングする際にガターオフセットを加算。
    pub(crate) column: u16,
}

impl TranscriptSelectionPoint {
    /// 指定された折り返し行インデックスと列で選択エンドポイントを作成。
    pub(crate) const fn new(line_index: usize, column: u16) -> Self {
        Self { line_index, column }
    }
}

impl From<(usize, u16)> for TranscriptSelectionPoint {
    fn from((line_index, column): (usize, u16)) -> Self {
        Self::new(line_index, column)
    }
}

/// トランスクリプト順序で `start <= end` となる `(start, end)` を返す。
pub(crate) fn ordered_endpoints(
    anchor: TranscriptSelectionPoint,
    head: TranscriptSelectionPoint,
) -> (TranscriptSelectionPoint, TranscriptSelectionPoint) {
    if anchor <= head {
        (anchor, head)
    } else {
        (head, anchor)
    }
}

/// 潜在的なトランスクリプト選択を開始（左ボタンダウン）。
///
/// アンカーポイントを記録し、既存のheadをクリア。ドラッグでheadが設定されるまで
/// 選択は「アクティブ」とは見なされず、シンプルクリックで1セル領域を
/// ハイライトすることを回避。
///
/// 選択が変更されたかどうかを返す（再描画を要求するかどうかの判断に有用）。
pub(crate) fn on_mouse_down(
    selection: &mut TranscriptSelection,
    point: Option<TranscriptSelectionPoint>,
) -> bool {
    let before = *selection;
    let Some(point) = point else {
        return false;
    };
    begin(selection, point);
    *selection != before
}

/// マウスドラッグ更新の結果。
///
/// [`on_mouse_drag`] によって返される。選択状態の更新を `App` レベルのアクションから
/// 分離し、呼び出し元が再描画のスケジュールやトランスクリプトスクロール位置の
/// ロックをいつ行うか決定できるようにする。
///
/// `lock_scroll` は呼び出し元がトランスクリプトビューポートをロックすべき
/// （現在下部に追従している場合）ことを示し、進行中のストリーミング出力が
/// カーソル下の選択を移動しないようにする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MouseDragOutcome {
    /// 選択が変更されたかどうか（再描画を要求するかどうかの判断に有用）。
    pub(crate) changed: bool,
    /// 呼び出し元がトランスクリプトスクロール位置をロックすべきかどうか。
    pub(crate) lock_scroll: bool,
}

/// 左ボタンドラッグの選択状態を更新。
///
/// 選択のhead（アクティブな選択を作成）を設定し、以下を返す:
///
/// - `changed`: 選択状態が変更されたかどうか（再描画を要求するかどうかの判断に有用）。
/// - `lock_scroll`: ストリーミング出力到着中に選択下のビューポートを固定するため、
///   呼び出し元がトランスクリプトスクロールをロックすべきかどうか。
///
/// `point` は既にトランスクリプトのコンテンツ領域にクランプされていることを想定
/// （例: ガター内ではない）。`point` が `None` の場合、これはno-op。
pub(crate) fn on_mouse_drag(
    selection: &mut TranscriptSelection,
    scroll: &TranscriptScroll,
    point: Option<TranscriptSelectionPoint>,
    streaming: bool,
) -> MouseDragOutcome {
    let before = *selection;
    let Some(point) = point else {
        return MouseDragOutcome {
            changed: false,
            lock_scroll: false,
        };
    };
    let lock_scroll = drag(selection, scroll, point, streaming);
    MouseDragOutcome {
        changed: *selection != before,
        lock_scroll,
    }
}

/// 左ボタンがリリースされたときに選択状態を確定。
///
/// 選択がアクティブにならなかった（headなし）場合、またはheadがanchorと等しく
/// なった場合、選択はクリアされ、クリックが永続的なハイライトを残さないようにする。
///
/// 選択が変更されたかどうかを返す（再描画を要求するかどうかの判断に有用）。
pub(crate) fn on_mouse_up(selection: &mut TranscriptSelection) -> bool {
    let before = *selection;
    end(selection);
    *selection != before
}

/// アンカーを記録しheadをクリアして潜在的な選択を開始。
///
/// これによりプレーンクリックがアクティブな選択/ハイライトを作成しないことを保証。
/// `head` を設定する最初のドラッグで選択がアクティブになる。
fn begin(selection: &mut TranscriptSelection, point: TranscriptSelectionPoint) {
    *selection = TranscriptSelection {
        anchor: Some(point),
        head: None,
    };
}

/// アンカーされている場合に `head` を設定してドラッグ中の選択状態を更新。
///
/// ストリーミング中で下部に追従している場合に、新しい出力がカーソル下の選択を
/// 移動しないよう、呼び出し元がトランスクリプトスクロール位置をロックすべきかを返す。
fn drag(
    selection: &mut TranscriptSelection,
    scroll: &TranscriptScroll,
    point: TranscriptSelectionPoint,
    streaming: bool,
) -> bool {
    let Some(anchor) = selection.anchor else {
        return false;
    };

    let should_lock_scroll =
        streaming && matches!(*scroll, TranscriptScroll::ToBottom) && point != anchor;

    selection.head = Some(point);

    should_lock_scroll
}

/// マウスアップで選択を確定。
///
/// 選択がアクティブにならなかった（headなし）場合、またはheadがanchorと等しく
/// なった場合に選択をクリアし、クリックが1セルハイライトを残さないようにする。
fn end(selection: &mut TranscriptSelection) {
    if selection.head.is_none() || selection.anchor == selection.head {
        *selection = TranscriptSelection::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn selection_only_highlights_on_drag() {
        let anchor = TranscriptSelectionPoint::new(0, 1);
        let head = TranscriptSelectionPoint::new(0, 3);

        let mut selection = TranscriptSelection::default();
        assert!(on_mouse_down(&mut selection, Some(anchor)));
        assert_eq!(
            selection,
            TranscriptSelection {
                anchor: Some(anchor),
                head: None,
            }
        );

        assert!(on_mouse_up(&mut selection));
        assert_eq!(selection, TranscriptSelection::default());

        assert!(on_mouse_down(&mut selection, Some(anchor)));
        let outcome = on_mouse_drag(
            &mut selection,
            &TranscriptScroll::ToBottom,
            Some(head),
            false,
        );
        assert!(outcome.changed);
        assert!(!outcome.lock_scroll);
        assert_eq!(
            selection,
            TranscriptSelection {
                anchor: Some(anchor),
                head: Some(head),
            }
        );
    }

    #[test]
    fn selection_clears_when_drag_ends_at_anchor() {
        let point = TranscriptSelectionPoint::new(0, 1);

        let mut selection = TranscriptSelection::default();
        assert!(on_mouse_down(&mut selection, Some(point)));
        let outcome = on_mouse_drag(
            &mut selection,
            &TranscriptScroll::ToBottom,
            Some(point),
            false,
        );
        assert!(outcome.changed);
        assert!(!outcome.lock_scroll);
        assert!(on_mouse_up(&mut selection));

        assert_eq!(selection, TranscriptSelection::default());
    }

    #[test]
    fn drag_requests_scroll_lock_when_streaming_at_bottom_and_point_moves() {
        let anchor = TranscriptSelectionPoint::new(0, 1);
        let head = TranscriptSelectionPoint::new(0, 2);

        let mut selection = TranscriptSelection::default();
        assert!(on_mouse_down(&mut selection, Some(anchor)));
        let outcome = on_mouse_drag(
            &mut selection,
            &TranscriptScroll::ToBottom,
            Some(head),
            true,
        );
        assert!(outcome.changed);
        assert!(outcome.lock_scroll);
    }

    #[test]
    fn selection_helpers_noop_without_points_or_anchor() {
        let mut selection = TranscriptSelection::default();
        assert!(!on_mouse_down(&mut selection, None));
        assert_eq!(selection, TranscriptSelection::default());

        let outcome = on_mouse_drag(&mut selection, &TranscriptScroll::ToBottom, None, false);
        assert_eq!(
            outcome,
            MouseDragOutcome {
                changed: false,
                lock_scroll: false,
            }
        );
        assert_eq!(selection, TranscriptSelection::default());

        let outcome = on_mouse_drag(
            &mut selection,
            &TranscriptScroll::ToBottom,
            Some(TranscriptSelectionPoint::new(0, 1)),
            false,
        );
        assert_eq!(
            outcome,
            MouseDragOutcome {
                changed: false,
                lock_scroll: false,
            }
        );
        assert_eq!(selection, TranscriptSelection::default());

        assert!(!on_mouse_up(&mut selection));
        assert_eq!(selection, TranscriptSelection::default());
    }

    #[test]
    fn mouse_down_resets_head() {
        let anchor = TranscriptSelectionPoint::new(0, 1);
        let head = TranscriptSelectionPoint::new(0, 2);
        let next_anchor = TranscriptSelectionPoint::new(1, 0);

        let mut selection = TranscriptSelection {
            anchor: Some(anchor),
            head: Some(head),
        };

        assert!(on_mouse_down(&mut selection, Some(next_anchor)));
        assert_eq!(
            selection,
            TranscriptSelection {
                anchor: Some(next_anchor),
                head: None,
            }
        );
    }

    #[test]
    fn dragging_does_not_request_scroll_lock_when_not_at_bottom() {
        let anchor = TranscriptSelectionPoint::new(0, 1);
        let head = TranscriptSelectionPoint::new(0, 2);

        let mut selection = TranscriptSelection::default();
        assert!(on_mouse_down(&mut selection, Some(anchor)));
        let outcome = on_mouse_drag(
            &mut selection,
            &TranscriptScroll::Scrolled {
                cell_index: 0,
                line_in_cell: 0,
            },
            Some(head),
            true,
        );
        assert!(outcome.changed);
        assert!(!outcome.lock_scroll);
    }
}
