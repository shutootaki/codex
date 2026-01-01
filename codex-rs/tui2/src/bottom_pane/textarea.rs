use crate::key_hint::is_altgr;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::WidgetRef;
use std::cell::Ref;
use std::cell::RefCell;
use std::ops::Range;
use textwrap::Options;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

fn is_word_separator(ch: char) -> bool {
    WORD_SEPARATORS.contains(ch)
}

#[derive(Debug, Clone)]
struct TextElement {
    range: Range<usize>,
}

#[derive(Debug)]
pub(crate) struct TextArea {
    text: String,
    cursor_pos: usize,
    wrap_cache: RefCell<Option<WrapCache>>,
    preferred_col: Option<usize>,
    elements: Vec<TextElement>,
    kill_buffer: String,
}

#[derive(Debug, Clone)]
struct WrapCache {
    width: u16,
    lines: Vec<Range<usize>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct TextAreaState {
    /// 最初に表示される折り返し行へのインデックス。
    scroll: u16,
}

impl TextArea {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor_pos: 0,
            wrap_cache: RefCell::new(None),
            preferred_col: None,
            elements: Vec::new(),
            kill_buffer: String::new(),
        }
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor_pos = self.cursor_pos.clamp(0, self.text.len());
        self.wrap_cache.replace(None);
        self.preferred_col = None;
        self.elements.clear();
        self.kill_buffer.clear();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn insert_str(&mut self, text: &str) {
        self.insert_str_at(self.cursor_pos, text);
    }

    pub fn insert_str_at(&mut self, pos: usize, text: &str) {
        let pos = self.clamp_pos_for_insertion(pos);
        self.text.insert_str(pos, text);
        self.wrap_cache.replace(None);
        if pos <= self.cursor_pos {
            self.cursor_pos += text.len();
        }
        self.shift_elements(pos, 0, text.len());
        self.preferred_col = None;
    }

    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        let range = self.expand_range_to_element_boundaries(range);
        self.replace_range_raw(range, text);
    }

    fn replace_range_raw(&mut self, range: std::ops::Range<usize>, text: &str) {
        assert!(range.start <= range.end);
        let start = range.start.clamp(0, self.text.len());
        let end = range.end.clamp(0, self.text.len());
        let removed_len = end - start;
        let inserted_len = text.len();
        if removed_len == 0 && inserted_len == 0 {
            return;
        }
        let diff = inserted_len as isize - removed_len as isize;

        self.text.replace_range(range, text);
        self.wrap_cache.replace(None);
        self.preferred_col = None;
        self.update_elements_after_replace(start, end, inserted_len);

        // 編集を反映してカーソル位置を更新。
        self.cursor_pos = if self.cursor_pos < start {
            // カーソルは編集範囲より前 – シフトなし。
            self.cursor_pos
        } else if self.cursor_pos <= end {
            // カーソルは置換範囲内 – 新しいテキストの末尾に移動。
            start + inserted_len
        } else {
            // カーソルは置換範囲より後 – 長さの差分だけシフト。
            ((self.cursor_pos as isize) + diff) as usize
        }
        .min(self.text.len());

        // カーソルがエレメント内部にないことを確認
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
    }

    pub fn cursor(&self) -> usize {
        self.cursor_pos
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor_pos = pos.clamp(0, self.text.len());
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        self.wrapped_lines(width).len() as u16
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_pos_with_state(area, TextAreaState::default())
    }

    /// スクロールを考慮した画面上のカーソル位置を計算。
    pub fn cursor_pos_with_state(&self, area: Rect, state: TextAreaState) -> Option<(u16, u16)> {
        let lines = self.wrapped_lines(area.width);
        let effective_scroll = self.effective_scroll(area.height, &lines, state.scroll);
        let i = Self::wrapped_line_index_by_start(&lines, self.cursor_pos)?;
        let ls = &lines[i];
        let col = self.text[ls.start..self.cursor_pos].width() as u16;
        let screen_row = i
            .saturating_sub(effective_scroll as usize)
            .try_into()
            .unwrap_or(0);
        Some((area.x + col, area.y + screen_row))
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn current_display_col(&self) -> usize {
        let bol = self.beginning_of_current_line();
        self.text[bol..self.cursor_pos].width()
    }

    fn wrapped_line_index_by_start(lines: &[Range<usize>], pos: usize) -> Option<usize> {
        // partition_pointは述語がfalseになる最初の要素のインデックスを返す。
        // つまり、start <= pos である要素の数。
        let idx = lines.partition_point(|r| r.start <= pos);
        if idx == 0 { None } else { Some(idx - 1) }
    }

    fn move_to_display_col_on_line(
        &mut self,
        line_start: usize,
        line_end: usize,
        target_col: usize,
    ) {
        let mut width_so_far = 0usize;
        for (i, g) in self.text[line_start..line_end].grapheme_indices(true) {
            width_so_far += g.width();
            if width_so_far > target_col {
                self.cursor_pos = line_start + i;
                // エレメント内部に着地しないよう最も近い境界に丸める
                self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
                return;
            }
        }
        self.cursor_pos = line_end;
        self.cursor_pos = self.clamp_pos_to_nearest_boundary(self.cursor_pos);
    }

    fn beginning_of_line(&self, pos: usize) -> usize {
        self.text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }
    fn beginning_of_current_line(&self) -> usize {
        self.beginning_of_line(self.cursor_pos)
    }

    fn end_of_line(&self, pos: usize) -> usize {
        self.text[pos..]
            .find('\n')
            .map(|i| i + pos)
            .unwrap_or(self.text.len())
    }
    fn end_of_current_line(&self) -> usize {
        self.end_of_line(self.cursor_pos)
    }

    pub fn input(&mut self, event: KeyEvent) {
        match event {
            // 一部のターミナル（または設定）はControlキーのコードを
            // CONTROLモディファイアを報告せずにC0制御文字として送信する。
            // Ctrl-B/F/P/Nの一般的なフォールバックをここで処理し、
            // リテラル制御バイトとして挿入されないようにする。
            KeyEvent { code: KeyCode::Char('\u{0002}'), modifiers: KeyModifiers::NONE, .. } /* ^B */ => {
                self.move_cursor_left();
            }
            KeyEvent { code: KeyCode::Char('\u{0006}'), modifiers: KeyModifiers::NONE, .. } /* ^F */ => {
                self.move_cursor_right();
            }
            KeyEvent { code: KeyCode::Char('\u{0010}'), modifiers: KeyModifiers::NONE, .. } /* ^P */ => {
                self.move_cursor_up();
            }
            KeyEvent { code: KeyCode::Char('\u{000e}'), modifiers: KeyModifiers::NONE, .. } /* ^N */ => {
                self.move_cursor_down();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                // 通常の文字（およびShift修飾）を挿入。ALTが押されているときは挿入しない。
                // 多くのターミナルがOption/MetaコンボをALT+<char>（例: ESC f/ESC b）に
                // マッピングしてワードナビゲーションに使うため。それらは以下で明示的に処理。
                modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
                ..
            } => self.insert_str(&c.to_string()),
            KeyEvent {
                code: KeyCode::Char('j' | 'm'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                ..
            } => self.insert_str("\n"),
            KeyEvent {
                code: KeyCode::Char('h'),
                modifiers,
                ..
            } if modifiers == (KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.delete_backward_word()
            },
            // Windows AltGrはALT|CONTROLを生成する。上記で特定のControl+Altバインディングに
            // マッチしない限り、通常の文字入力として扱う。
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if is_altgr(modifiers) => self.insert_str(&c.to_string()),
            KeyEvent {
                code: KeyCode::Backspace,
                modifiers: KeyModifiers::ALT,
                ..
            } => self.delete_backward_word(),
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.delete_backward(1),
            KeyEvent {
                code: KeyCode::Delete,
                modifiers: KeyModifiers::ALT,
                ..
            }  => self.delete_forward_word(),
            KeyEvent {
                code: KeyCode::Delete,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.delete_forward(1),

            KeyEvent {
                code: KeyCode::Char('w'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.delete_backward_word();
            }
            // Meta-b -> 前のワードの先頭に移動
            // Meta-f -> 次のワードの末尾に移動
            // 多くのターミナルはOption（macOS）をAltにマッピングする。Alt|Shiftを送るものもあるため、contains(ALT)でマッチ。
            KeyEvent {
                code: KeyCode::Char('b'),
                modifiers: KeyModifiers::ALT,
                ..
            } => {
                self.set_cursor(self.beginning_of_previous_word());
            }
            KeyEvent {
                code: KeyCode::Char('f'),
                modifiers: KeyModifiers::ALT,
                ..
            } => {
                self.set_cursor(self.end_of_next_word());
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.kill_to_beginning_of_line();
            }
            KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.kill_to_end_of_line();
            }
            KeyEvent {
                code: KeyCode::Char('y'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.yank();
            }

            // カーソル移動
            KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_cursor_left();
            }
            KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_cursor_right();
            }
            KeyEvent {
                code: KeyCode::Char('b'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.move_cursor_left();
            }
            KeyEvent {
                code: KeyCode::Char('f'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.move_cursor_right();
            }
            KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.move_cursor_up();
            }
            KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.move_cursor_down();
            }
            // 一部のターミナルはワード単位の移動にAlt+矢印を送信：
            // Option/Left -> Alt+Left（前のワードの先頭）
            // Option/Right -> Alt+Right（次のワードの末尾）
            KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::ALT,
                ..
            }
            | KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.set_cursor(self.beginning_of_previous_word());
            }
            KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::ALT,
                ..
            }
            | KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.set_cursor(self.end_of_next_word());
            }
            KeyEvent {
                code: KeyCode::Up, ..
            } => {
                self.move_cursor_up();
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => {
                self.move_cursor_down();
            }
            KeyEvent {
                code: KeyCode::Home,
                ..
            } => {
                self.move_cursor_to_beginning_of_line(false);
            }
            KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.move_cursor_to_beginning_of_line(true);
            }

            KeyEvent {
                code: KeyCode::End, ..
            } => {
                self.move_cursor_to_end_of_line(false);
            }
            KeyEvent {
                code: KeyCode::Char('e'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.move_cursor_to_end_of_line(true);
            }
            _o => {
                #[cfg(feature = "debug-logs")]
                tracing::debug!("Unhandled key event in TextArea: {:?}", _o);
            }
        }
    }

    // ####### 入力関数 #######
    pub fn delete_backward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos == 0 {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.prev_atomic_boundary(target);
            if target == 0 {
                break;
            }
        }
        self.replace_range(target..self.cursor_pos, "");
    }

    pub fn delete_forward(&mut self, n: usize) {
        if n == 0 || self.cursor_pos >= self.text.len() {
            return;
        }
        let mut target = self.cursor_pos;
        for _ in 0..n {
            target = self.next_atomic_boundary(target);
            if target >= self.text.len() {
                break;
            }
        }
        self.replace_range(self.cursor_pos..target, "");
    }

    pub fn delete_backward_word(&mut self) {
        let start = self.beginning_of_previous_word();
        self.kill_range(start..self.cursor_pos);
    }

    /// 「ワード」セマンティクスを使用してカーソルの右側のテキストを削除。
    ///
    /// `end_of_next_word()`で決定される次のワードの末尾まで、現在のカーソル位置から
    /// 削除する。カーソルとそのワードの間の空白（改行を含む）も削除に含まれる。
    pub fn delete_forward_word(&mut self) {
        let end = self.end_of_next_word();
        if end > self.cursor_pos {
            self.kill_range(self.cursor_pos..end);
        }
    }

    pub fn kill_to_end_of_line(&mut self) {
        let eol = self.end_of_current_line();
        let range = if self.cursor_pos == eol {
            if eol < self.text.len() {
                Some(self.cursor_pos..eol + 1)
            } else {
                None
            }
        } else {
            Some(self.cursor_pos..eol)
        };

        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    pub fn kill_to_beginning_of_line(&mut self) {
        let bol = self.beginning_of_current_line();
        let range = if self.cursor_pos == bol {
            if bol > 0 { Some(bol - 1..bol) } else { None }
        } else {
            Some(bol..self.cursor_pos)
        };

        if let Some(range) = range {
            self.kill_range(range);
        }
    }

    pub fn yank(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        let text = self.kill_buffer.clone();
        self.insert_str(&text);
    }

    fn kill_range(&mut self, range: Range<usize>) {
        let range = self.expand_range_to_element_boundaries(range);
        if range.start >= range.end {
            return;
        }

        let removed = self.text[range.clone()].to_string();
        if removed.is_empty() {
            return;
        }

        self.kill_buffer = removed;
        self.replace_range_raw(range, "");
    }

    /// カーソルを1つの書記素クラスタ分左に移動。
    pub fn move_cursor_left(&mut self) {
        self.cursor_pos = self.prev_atomic_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    /// カーソルを1つの書記素クラスタ分右に移動。
    pub fn move_cursor_right(&mut self) {
        self.cursor_pos = self.next_atomic_boundary(self.cursor_pos);
        self.preferred_col = None;
    }

    pub fn move_cursor_up(&mut self) {
        // 折り返しキャッシュがある場合、折り返された（視覚的な）行を跨ぐナビゲーションを優先。
        if let Some((target_col, maybe_line)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some(idx) = Self::wrapped_line_index_by_start(lines, self.cursor_pos) {
                    let cur_range = &lines[idx];
                    let target_col = self
                        .preferred_col
                        .unwrap_or_else(|| self.text[cur_range.start..self.cursor_pos].width());
                    if idx > 0 {
                        let prev = &lines[idx - 1];
                        let line_start = prev.start;
                        let line_end = prev.end.saturating_sub(1);
                        Some((target_col, Some((line_start, line_end))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            // 折り返し情報があった。それに応じて移動を適用。
            match maybe_line {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col_on_line(line_start, line_end, target_col);
                    return;
                }
                None => {
                    // 既に最初の視覚的な行にいる -> 先頭に移動
                    self.cursor_pos = 0;
                    self.preferred_col = None;
                    return;
                }
            }
        }

        // 折り返し情報がまだない場合は論理行ナビゲーションにフォールバック。
        if let Some(prev_nl) = self.text[..self.cursor_pos].rfind('\n') {
            let target_col = match self.preferred_col {
                Some(c) => c,
                None => {
                    let c = self.current_display_col();
                    self.preferred_col = Some(c);
                    c
                }
            };
            let prev_line_start = self.text[..prev_nl].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let prev_line_end = prev_nl;
            self.move_to_display_col_on_line(prev_line_start, prev_line_end, target_col);
        } else {
            self.cursor_pos = 0;
            self.preferred_col = None;
        }
    }

    pub fn move_cursor_down(&mut self) {
        // 折り返しキャッシュがある場合、折り返された（視覚的な）行を跨ぐナビゲーションを優先。
        if let Some((target_col, move_to_last)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some(idx) = Self::wrapped_line_index_by_start(lines, self.cursor_pos) {
                    let cur_range = &lines[idx];
                    let target_col = self
                        .preferred_col
                        .unwrap_or_else(|| self.text[cur_range.start..self.cursor_pos].width());
                    if idx + 1 < lines.len() {
                        let next = &lines[idx + 1];
                        let line_start = next.start;
                        let line_end = next.end.saturating_sub(1);
                        Some((target_col, Some((line_start, line_end))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            match move_to_last {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col_on_line(line_start, line_end, target_col);
                    return;
                }
                None => {
                    // 既に最後の視覚的な行にいる -> 末尾に移動
                    self.cursor_pos = self.text.len();
                    self.preferred_col = None;
                    return;
                }
            }
        }

        // 折り返し情報がまだない場合は論理行ナビゲーションにフォールバック。
        let target_col = match self.preferred_col {
            Some(c) => c,
            None => {
                let c = self.current_display_col();
                self.preferred_col = Some(c);
                c
            }
        };
        if let Some(next_nl) = self.text[self.cursor_pos..]
            .find('\n')
            .map(|i| i + self.cursor_pos)
        {
            let next_line_start = next_nl + 1;
            let next_line_end = self.text[next_line_start..]
                .find('\n')
                .map(|i| i + next_line_start)
                .unwrap_or(self.text.len());
            self.move_to_display_col_on_line(next_line_start, next_line_end, target_col);
        } else {
            self.cursor_pos = self.text.len();
            self.preferred_col = None;
        }
    }

    pub fn move_cursor_to_beginning_of_line(&mut self, move_up_at_bol: bool) {
        let bol = self.beginning_of_current_line();
        if move_up_at_bol && self.cursor_pos == bol {
            self.set_cursor(self.beginning_of_line(self.cursor_pos.saturating_sub(1)));
        } else {
            self.set_cursor(bol);
        }
        self.preferred_col = None;
    }

    pub fn move_cursor_to_end_of_line(&mut self, move_down_at_eol: bool) {
        let eol = self.end_of_current_line();
        if move_down_at_eol && self.cursor_pos == eol {
            let next_pos = (self.cursor_pos.saturating_add(1)).min(self.text.len());
            self.set_cursor(self.end_of_line(next_pos));
        } else {
            self.set_cursor(eol);
        }
    }

    // ===== テキストエレメントサポート =====

    pub fn insert_element(&mut self, text: &str) {
        let start = self.clamp_pos_for_insertion(self.cursor_pos);
        self.insert_str_at(start, text);
        let end = start + text.len();
        self.add_element(start..end);
        // カーソルを挿入されたエレメントの末尾に配置
        self.set_cursor(end);
    }

    fn add_element(&mut self, range: Range<usize>) {
        let elem = TextElement { range };
        self.elements.push(elem);
        self.elements.sort_by_key(|e| e.range.start);
    }

    fn find_element_containing(&self, pos: usize) -> Option<usize> {
        self.elements
            .iter()
            .position(|e| pos > e.range.start && pos < e.range.end)
    }

    fn clamp_pos_to_nearest_boundary(&self, mut pos: usize) -> usize {
        if pos > self.text.len() {
            pos = self.text.len();
        }
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            let dist_start = pos.saturating_sub(e.range.start);
            let dist_end = e.range.end.saturating_sub(pos);
            if dist_start <= dist_end {
                e.range.start
            } else {
                e.range.end
            }
        } else {
            pos
        }
    }

    fn clamp_pos_for_insertion(&self, pos: usize) -> usize {
        // エレメントの中間への挿入を許可しない
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            // 挿入のために最も近いエッジを選択
            let dist_start = pos.saturating_sub(e.range.start);
            let dist_end = e.range.end.saturating_sub(pos);
            if dist_start <= dist_end {
                e.range.start
            } else {
                e.range.end
            }
        } else {
            pos
        }
    }

    fn expand_range_to_element_boundaries(&self, mut range: Range<usize>) -> Range<usize> {
        // 交差するエレメントを完全に含むよう拡張
        loop {
            let mut changed = false;
            for e in &self.elements {
                if e.range.start < range.end && e.range.end > range.start {
                    let new_start = range.start.min(e.range.start);
                    let new_end = range.end.max(e.range.end);
                    if new_start != range.start || new_end != range.end {
                        range.start = new_start;
                        range.end = new_end;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        range
    }

    fn shift_elements(&mut self, at: usize, removed: usize, inserted: usize) {
        // 汎用シフト：純粋な挿入ではremoved = 0、削除ではinserted = 0。
        let end = at + removed;
        let diff = inserted as isize - removed as isize;
        // 操作によって完全に削除されたエレメントを除去し、残りをシフト
        self.elements
            .retain(|e| !(e.range.start >= at && e.range.end <= end));
        for e in &mut self.elements {
            if e.range.end <= at {
                // 編集より前
            } else if e.range.start >= end {
                // 編集より後
                e.range.start = ((e.range.start as isize) + diff) as usize;
                e.range.end = ((e.range.end as isize) + diff) as usize;
            } else {
                // エレメントとオーバーラップしているが完全に含まれていない
                // （エレメント対応の置換を使用していれば発生しないはずだが、
                // 新しい境界にスナップして優雅にデグレード）
                let new_start = at.min(e.range.start);
                let new_end = at + inserted.max(e.range.end.saturating_sub(end));
                e.range.start = new_start;
                e.range.end = new_end;
            }
        }
    }

    fn update_elements_after_replace(&mut self, start: usize, end: usize, inserted_len: usize) {
        self.shift_elements(start, end.saturating_sub(start), inserted_len);
    }

    fn prev_atomic_boundary(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        // 現在エレメントの末尾または内部にいる場合、そのエレメントの先頭にジャンプ。
        if let Some(idx) = self
            .elements
            .iter()
            .position(|e| pos > e.range.start && pos <= e.range.end)
        {
            return self.elements[idx].range.start;
        }
        let mut gc = unicode_segmentation::GraphemeCursor::new(pos, self.text.len(), false);
        match gc.prev_boundary(&self.text, 0) {
            Ok(Some(b)) => {
                if let Some(idx) = self.find_element_containing(b) {
                    self.elements[idx].range.start
                } else {
                    b
                }
            }
            Ok(None) => 0,
            Err(_) => pos.saturating_sub(1),
        }
    }

    fn next_atomic_boundary(&self, pos: usize) -> usize {
        if pos >= self.text.len() {
            return self.text.len();
        }
        // 現在エレメントの先頭または内部にいる場合、そのエレメントの末尾にジャンプ。
        if let Some(idx) = self
            .elements
            .iter()
            .position(|e| pos >= e.range.start && pos < e.range.end)
        {
            return self.elements[idx].range.end;
        }
        let mut gc = unicode_segmentation::GraphemeCursor::new(pos, self.text.len(), false);
        match gc.next_boundary(&self.text, 0) {
            Ok(Some(b)) => {
                if let Some(idx) = self.find_element_containing(b) {
                    self.elements[idx].range.end
                } else {
                    b
                }
            }
            Ok(None) => self.text.len(),
            Err(_) => pos.saturating_add(1),
        }
    }

    pub(crate) fn beginning_of_previous_word(&self) -> usize {
        let prefix = &self.text[..self.cursor_pos];
        let Some((first_non_ws_idx, ch)) = prefix
            .char_indices()
            .rev()
            .find(|&(_, ch)| !ch.is_whitespace())
        else {
            return 0;
        };
        let is_separator = is_word_separator(ch);
        let mut start = first_non_ws_idx;
        for (idx, ch) in prefix[..first_non_ws_idx].char_indices().rev() {
            if ch.is_whitespace() || is_word_separator(ch) != is_separator {
                start = idx + ch.len_utf8();
                break;
            }
            start = idx;
        }
        self.adjust_pos_out_of_elements(start, true)
    }

    pub(crate) fn end_of_next_word(&self) -> usize {
        let Some(first_non_ws) = self.text[self.cursor_pos..].find(|c: char| !c.is_whitespace())
        else {
            return self.text.len();
        };
        let word_start = self.cursor_pos + first_non_ws;
        let mut iter = self.text[word_start..].char_indices();
        let Some((_, first_ch)) = iter.next() else {
            return word_start;
        };
        let is_separator = is_word_separator(first_ch);
        let mut end = self.text.len();
        for (idx, ch) in iter {
            if ch.is_whitespace() || is_word_separator(ch) != is_separator {
                end = word_start + idx;
                break;
            }
        }
        self.adjust_pos_out_of_elements(end, false)
    }

    fn adjust_pos_out_of_elements(&self, pos: usize, prefer_start: bool) -> usize {
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            if prefer_start {
                e.range.start
            } else {
                e.range.end
            }
        } else {
            pos
        }
    }

    #[expect(clippy::unwrap_used)]
    fn wrapped_lines(&self, width: u16) -> Ref<'_, Vec<Range<usize>>> {
        // キャッシュの準備を確認（可変借用の可能性があり、その後ドロップ）
        {
            let mut cache = self.wrap_cache.borrow_mut();
            let needs_recalc = match cache.as_ref() {
                Some(c) => c.width != width,
                None => true,
            };
            if needs_recalc {
                let lines = crate::wrapping::wrap_ranges(
                    &self.text,
                    Options::new(width as usize).wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
                );
                *cache = Some(WrapCache { width, lines });
            }
        }

        let cache = self.wrap_cache.borrow();
        Ref::map(cache, |c| &c.as_ref().unwrap().lines)
    }

    /// 現在のエリアサイズと折り返し行に対して、不変条件を満たすために
    /// 使用すべきスクロールオフセットを計算。
    ///
    /// - カーソルは常に画面上に表示。
    /// - コンテンツがエリアに収まる場合はスクロールなし。
    fn effective_scroll(
        &self,
        area_height: u16,
        lines: &[Range<usize>],
        current_scroll: u16,
    ) -> u16 {
        let total_lines = lines.len() as u16;
        if area_height >= total_lines {
            return 0;
        }

        // カーソルは折り返し行のどこにあるか？境界位置（posが折り返し行の先頭と等しい場合）は
        // その後の行に割り当てることを優先。
        let cursor_line_idx =
            Self::wrapped_line_index_by_start(lines, self.cursor_pos).unwrap_or(0) as u16;

        let max_scroll = total_lines.saturating_sub(area_height);
        let mut scroll = current_scroll.min(max_scroll);

        // カーソルが[scroll, scroll + area_height)内に見えることを保証
        if cursor_line_idx < scroll {
            scroll = cursor_line_idx;
        } else if cursor_line_idx >= scroll + area_height {
            scroll = cursor_line_idx + 1 - area_height;
        }
        scroll
    }
}

impl WidgetRef for &TextArea {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.wrapped_lines(area.width);
        self.render_lines(area, buf, &lines, 0..lines.len());
    }
}

impl StatefulWidgetRef for &TextArea {
    type State = TextAreaState;

    fn render_ref(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let lines = self.wrapped_lines(area.width);
        let scroll = self.effective_scroll(area.height, &lines, state.scroll);
        state.scroll = scroll;

        let start = scroll as usize;
        let end = (scroll + area.height).min(lines.len() as u16) as usize;
        self.render_lines(area, buf, &lines, start..end);
    }
}

impl TextArea {
    fn render_lines(
        &self,
        area: Rect,
        buf: &mut Buffer,
        lines: &[Range<usize>],
        range: std::ops::Range<usize>,
    ) {
        for (row, idx) in range.enumerate() {
            let r = &lines[idx];
            let y = area.y + row as u16;
            let line_range = r.start..r.end - 1;
            // デフォルトスタイルでベースラインを描画。
            buf.set_string(area.x, y, &self.text[line_range.clone()], Style::default());

            // この行と交差するエレメントのスタイル付きセグメントをオーバーレイ。
            for elem in &self.elements {
                // 表示されるスライスとのオーバーラップを計算。
                let overlap_start = elem.range.start.max(line_range.start);
                let overlap_end = elem.range.end.min(line_range.end);
                if overlap_start >= overlap_end {
                    continue;
                }
                let styled = &self.text[overlap_start..overlap_end];
                let x_off = self.text[line_range.start..overlap_start].width() as u16;
                let style = Style::default().fg(Color::Cyan);
                buf.set_string(area.x + x_off, y, styled, style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // crossterm型は未使用警告を避けるため意図的にここではインポートしない
    use rand::prelude::*;

    fn rand_grapheme(rng: &mut rand::rngs::StdRng) -> String {
        let r: u8 = rng.random_range(0..100);
        match r {
            0..=4 => "\n".to_string(),
            5..=12 => " ".to_string(),
            13..=35 => (rng.random_range(b'a'..=b'z') as char).to_string(),
            36..=45 => (rng.random_range(b'A'..=b'Z') as char).to_string(),
            46..=52 => (rng.random_range(b'0'..=b'9') as char).to_string(),
            53..=65 => {
                // 絵文字（ワイドグラフェム）
                let choices = ["👍", "😊", "🐍", "🚀", "🧪", "🌟"];
                choices[rng.random_range(0..choices.len())].to_string()
            }
            66..=75 => {
                // CJKワイド文字
                let choices = ["漢", "字", "測", "試", "你", "好", "界", "编", "码"];
                choices[rng.random_range(0..choices.len())].to_string()
            }
            76..=85 => {
                // 結合マークシーケンス
                let base = ["e", "a", "o", "n", "u"][rng.random_range(0..5)];
                let marks = ["\u{0301}", "\u{0308}", "\u{0302}", "\u{0303}"];
                format!("{base}{}", marks[rng.random_range(0..marks.len())])
            }
            86..=92 => {
                // 非ラテン単一コードポイント（ギリシャ語、キリル文字、ヘブライ語）
                let choices = ["Ω", "β", "Ж", "ю", "ש", "م", "ह"];
                choices[rng.random_range(0..choices.len())].to_string()
            }
            _ => {
                // ZWJシーケンス（単一グラフェムだが複数コードポイント）
                let choices = [
                    "👩\u{200D}💻", // 女性技術者
                    "👨\u{200D}💻", // 男性技術者
                    "🏳️\u{200D}🌈", // レインボーフラッグ
                ];
                choices[rng.random_range(0..choices.len())].to_string()
            }
        }
    }

    fn ta_with(text: &str) -> TextArea {
        let mut t = TextArea::new();
        t.insert_str(text);
        t
    }

    #[test]
    fn insert_and_replace_update_cursor_and_text() {
        // 挿入ヘルパー
        let mut t = ta_with("hello");
        t.set_cursor(5);
        t.insert_str("!");
        assert_eq!(t.text(), "hello!");
        assert_eq!(t.cursor(), 6);

        t.insert_str_at(0, "X");
        assert_eq!(t.text(), "Xhello!");
        assert_eq!(t.cursor(), 7);

        // カーソルの後への挿入はカーソルを移動させない
        t.set_cursor(1);
        let end = t.text().len();
        t.insert_str_at(end, "Y");
        assert_eq!(t.text(), "Xhello!Y");
        assert_eq!(t.cursor(), 1);

        // replace_rangeのケース
        // 1) カーソルが範囲より前
        let mut t = ta_with("abcd");
        t.set_cursor(1);
        t.replace_range(2..3, "Z");
        assert_eq!(t.text(), "abZd");
        assert_eq!(t.cursor(), 1);

        // 2) カーソルが範囲内
        let mut t = ta_with("abcd");
        t.set_cursor(2);
        t.replace_range(1..3, "Q");
        assert_eq!(t.text(), "aQd");
        assert_eq!(t.cursor(), 2);

        // 3) カーソルが範囲より後で差分だけシフト
        let mut t = ta_with("abcd");
        t.set_cursor(4);
        t.replace_range(0..1, "AA");
        assert_eq!(t.text(), "AAbcd");
        assert_eq!(t.cursor(), 5);
    }

    #[test]
    fn delete_backward_and_forward_edges() {
        let mut t = ta_with("abc");
        t.set_cursor(1);
        t.delete_backward(1);
        assert_eq!(t.text(), "bc");
        assert_eq!(t.cursor(), 0);

        // 先頭での後方削除は何もしない
        t.set_cursor(0);
        t.delete_backward(1);
        assert_eq!(t.text(), "bc");
        assert_eq!(t.cursor(), 0);

        // 前方削除は次のグラフェムを削除
        t.set_cursor(1);
        t.delete_forward(1);
        assert_eq!(t.text(), "b");
        assert_eq!(t.cursor(), 1);

        // 末尾での前方削除は何もしない
        t.set_cursor(t.text().len());
        t.delete_forward(1);
        assert_eq!(t.text(), "b");
    }

    #[test]
    fn delete_backward_word_and_kill_line_variants() {
        // 末尾でのワード後方削除は前のワード全体を削除
        let mut t = ta_with("hello   world  ");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "hello   ");
        assert_eq!(t.cursor(), 8);

        // ワードの中からは、ワードの先頭からカーソルまで削除
        let mut t = ta_with("foo bar");
        t.set_cursor(6); // inside "bar" (after 'a')
        t.delete_backward_word();
        assert_eq!(t.text(), "foo r");
        assert_eq!(t.cursor(), 4);

        // 末尾からは、最後のワードのみを削除
        let mut t = ta_with("foo bar");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "foo ");
        assert_eq!(t.cursor(), 4);

        // EOLにいないときのkill_to_end_of_line
        let mut t = ta_with("abc\ndef");
        t.set_cursor(1); // on first line, middle
        t.kill_to_end_of_line();
        assert_eq!(t.text(), "a\ndef");
        assert_eq!(t.cursor(), 1);

        // EOLにいるときのkill_to_end_of_lineは改行を削除
        let mut t = ta_with("abc\ndef");
        t.set_cursor(3); // EOL of first line
        t.kill_to_end_of_line();
        assert_eq!(t.text(), "abcdef");
        assert_eq!(t.cursor(), 3);

        // 行の途中からのkill_to_beginning_of_line
        let mut t = ta_with("abc\ndef");
        t.set_cursor(5); // on second line, after 'e'
        t.kill_to_beginning_of_line();
        assert_eq!(t.text(), "abc\nef");

        // 最初でない行の先頭でのkill_to_beginning_of_lineは前の改行を削除
        let mut t = ta_with("abc\ndef");
        t.set_cursor(4); // beginning of second line
        t.kill_to_beginning_of_line();
        assert_eq!(t.text(), "abcdef");
        assert_eq!(t.cursor(), 3);
    }

    #[test]
    fn delete_forward_word_variants() {
        let mut t = ta_with("hello   world ");
        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), "   world ");
        assert_eq!(t.cursor(), 0);

        let mut t = ta_with("hello   world ");
        t.set_cursor(1);
        t.delete_forward_word();
        assert_eq!(t.text(), "h   world ");
        assert_eq!(t.cursor(), 1);

        let mut t = ta_with("hello   world");
        t.set_cursor(t.text().len());
        t.delete_forward_word();
        assert_eq!(t.text(), "hello   world");
        assert_eq!(t.cursor(), t.text().len());

        let mut t = ta_with("foo   \nbar");
        t.set_cursor(3);
        t.delete_forward_word();
        assert_eq!(t.text(), "foo");
        assert_eq!(t.cursor(), 3);

        let mut t = ta_with("foo\nbar");
        t.set_cursor(3);
        t.delete_forward_word();
        assert_eq!(t.text(), "foo");
        assert_eq!(t.cursor(), 3);

        let mut t = ta_with("hello   world ");
        t.set_cursor(t.text().len() + 10);
        t.delete_forward_word();
        assert_eq!(t.text(), "hello   world ");
        assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn delete_forward_word_handles_atomic_elements() {
        let mut t = TextArea::new();
        t.insert_element("<element>");
        t.insert_str(" tail");

        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), " tail");
        assert_eq!(t.cursor(), 0);

        let mut t = TextArea::new();
        t.insert_str("   ");
        t.insert_element("<element>");
        t.insert_str(" tail");

        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), " tail");
        assert_eq!(t.cursor(), 0);

        let mut t = TextArea::new();
        t.insert_str("prefix ");
        t.insert_element("<element>");
        t.insert_str(" tail");

        // エレメントの中央にカーソルがあるとき、delete_forward_wordはエレメントを削除
        let elem_range = t.elements[0].range.clone();
        t.cursor_pos = elem_range.start + (elem_range.len() / 2);
        t.delete_forward_word();
        assert_eq!(t.text(), "prefix  tail");
        assert_eq!(t.cursor(), elem_range.start);
    }

    #[test]
    fn delete_backward_word_respects_word_separators() {
        let mut t = ta_with("path/to/file");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "path/to/");
        assert_eq!(t.cursor(), t.text().len());

        t.delete_backward_word();
        assert_eq!(t.text(), "path/to");
        assert_eq!(t.cursor(), t.text().len());

        let mut t = ta_with("foo/ ");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "foo");
        assert_eq!(t.cursor(), 3);

        let mut t = ta_with("foo /");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "foo ");
        assert_eq!(t.cursor(), 4);
    }

    #[test]
    fn delete_forward_word_respects_word_separators() {
        let mut t = ta_with("path/to/file");
        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), "/to/file");
        assert_eq!(t.cursor(), 0);

        t.delete_forward_word();
        assert_eq!(t.text(), "to/file");
        assert_eq!(t.cursor(), 0);

        let mut t = ta_with("/ foo");
        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), " foo");
        assert_eq!(t.cursor(), 0);

        let mut t = ta_with(" /foo");
        t.set_cursor(0);
        t.delete_forward_word();
        assert_eq!(t.text(), "foo");
        assert_eq!(t.cursor(), 0);
    }

    #[test]
    fn yank_restores_last_kill() {
        let mut t = ta_with("hello");
        t.set_cursor(0);
        t.kill_to_end_of_line();
        assert_eq!(t.text(), "");
        assert_eq!(t.cursor(), 0);

        t.yank();
        assert_eq!(t.text(), "hello");
        assert_eq!(t.cursor(), 5);

        let mut t = ta_with("hello world");
        t.set_cursor(t.text().len());
        t.delete_backward_word();
        assert_eq!(t.text(), "hello ");
        assert_eq!(t.cursor(), 6);

        t.yank();
        assert_eq!(t.text(), "hello world");
        assert_eq!(t.cursor(), 11);

        let mut t = ta_with("hello");
        t.set_cursor(5);
        t.kill_to_beginning_of_line();
        assert_eq!(t.text(), "");
        assert_eq!(t.cursor(), 0);

        t.yank();
        assert_eq!(t.text(), "hello");
        assert_eq!(t.cursor(), 5);
    }

    #[test]
    fn cursor_left_and_right_handle_graphemes() {
        let mut t = ta_with("a👍b");
        t.set_cursor(t.text().len());

        t.move_cursor_left(); // before 'b'
        let after_first_left = t.cursor();
        t.move_cursor_left(); // before '👍'
        let after_second_left = t.cursor();
        t.move_cursor_left(); // before 'a'
        let after_third_left = t.cursor();

        assert!(after_first_left < t.text().len());
        assert!(after_second_left < after_first_left);
        assert!(after_third_left < after_second_left);

        // 安全に末尾まで右に戻る
        t.move_cursor_right();
        t.move_cursor_right();
        t.move_cursor_right();
        assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn control_b_and_f_move_cursor() {
        let mut t = ta_with("abcd");
        t.set_cursor(1);

        t.input(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 2);

        t.input(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn control_b_f_fallback_control_chars_move_cursor() {
        let mut t = ta_with("abcd");
        t.set_cursor(2);

        // CONTROLモディファイアなしでC0制御文字を送信するターミナルをシミュレート。
        // ^B (U+0002) は左に移動すべき
        t.input(KeyEvent::new(KeyCode::Char('\u{0002}'), KeyModifiers::NONE));
        assert_eq!(t.cursor(), 1);

        // ^F (U+0006) は右に移動すべき
        t.input(KeyEvent::new(KeyCode::Char('\u{0006}'), KeyModifiers::NONE));
        assert_eq!(t.cursor(), 2);
    }

    #[test]
    fn delete_backward_word_alt_keys() {
        // カスタムAlt+Ctrl+hバインディングのテスト
        let mut t = ta_with("hello world");
        t.set_cursor(t.text().len()); // cursor at the end
        t.input(KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(t.text(), "hello ");
        assert_eq!(t.cursor(), 6);

        // 標準Alt+Backspaceバインディングのテスト
        let mut t = ta_with("hello world");
        t.set_cursor(t.text().len()); // cursor at the end
        t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(t.text(), "hello ");
        assert_eq!(t.cursor(), 6);
    }

    #[test]
    fn delete_backward_word_handles_narrow_no_break_space() {
        let mut t = ta_with("32\u{202F}AM");
        t.set_cursor(t.text().len());
        t.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        pretty_assertions::assert_eq!(t.text(), "32\u{202F}");
        pretty_assertions::assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn delete_forward_word_with_without_alt_modifier() {
        let mut t = ta_with("hello world");
        t.set_cursor(0);
        t.input(KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT));
        assert_eq!(t.text(), " world");
        assert_eq!(t.cursor(), 0);

        let mut t = ta_with("hello");
        t.set_cursor(0);
        t.input(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(t.text(), "ello");
        assert_eq!(t.cursor(), 0);
    }

    #[test]
    fn control_h_backspace() {
        // Ctrl+Hをバックスペースとしてテスト
        let mut t = ta_with("12345");
        t.set_cursor(3); // cursor after '3'
        t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(t.text(), "1245");
        assert_eq!(t.cursor(), 2);

        // 先頭でのCtrl+Hのテスト（何もしないはず）
        t.set_cursor(0);
        t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(t.text(), "1245");
        assert_eq!(t.cursor(), 0);

        // 末尾でのCtrl+Hのテスト
        t.set_cursor(t.text().len());
        t.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(t.text(), "124");
        assert_eq!(t.cursor(), 3);
    }

    #[cfg_attr(not(windows), ignore = "AltGr modifier only applies on Windows")]
    #[test]
    fn altgr_ctrl_alt_char_inserts_literal() {
        let mut t = ta_with("");
        t.input(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert_eq!(t.text(), "c");
        assert_eq!(t.cursor(), 1);
    }

    #[test]
    fn cursor_vertical_movement_across_lines_and_bounds() {
        let mut t = ta_with("short\nloooooooooong\nmid");
        // カーソルを2行目の列5に配置
        let second_line_start = 6; // 最初の'\n'の後
        t.set_cursor(second_line_start + 5);

        // 上移動：ターゲット列は保持され、行の長さでクランプ
        t.move_cursor_up();
        assert_eq!(t.cursor(), 5); // first line has len 5

        // もう一度上移動でテキストの先頭へ
        t.move_cursor_up();
        assert_eq!(t.cursor(), 0);

        // 下移動：開始位置からターゲット列を追跡
        t.move_cursor_down();
        // 最初の下移動で、2行目の列0に着地すべき（ターゲット列は0として記憶）
        let pos_after_down = t.cursor();
        assert!(pos_after_down >= second_line_start);

        // もう一度下移動で3行目へ。その長さにクランプ
        t.move_cursor_down();
        let third_line_start = t.text().find("mid").unwrap();
        let third_line_end = third_line_start + 3;
        assert!(t.cursor() >= third_line_start && t.cursor() <= third_line_end);

        // 最後の行での下移動は末尾にジャンプ
        t.move_cursor_down();
        assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn home_end_and_emacs_style_home_end() {
        let mut t = ta_with("one\ntwo\nthree");
        // 2行目の中央に配置
        let second_line_start = t.text().find("two").unwrap();
        t.set_cursor(second_line_start + 1);

        t.move_cursor_to_beginning_of_line(false);
        assert_eq!(t.cursor(), second_line_start);

        // Ctrl-Aの動作：BOLにいる場合、前の行の先頭に移動
        t.move_cursor_to_beginning_of_line(true);
        assert_eq!(t.cursor(), 0); // beginning of first line

        // 1行目のEOLに移動
        t.move_cursor_to_end_of_line(false);
        assert_eq!(t.cursor(), 3);

        // Ctrl-E：EOLにいる場合、次の行の末尾に移動
        t.move_cursor_to_end_of_line(true);
        // 2行目（"two"）の末尾はその'\n'の直前
        let end_second_nl = t.text().find("\nthree").unwrap();
        assert_eq!(t.cursor(), end_second_nl);
    }

    #[test]
    fn end_of_line_or_down_at_end_of_text() {
        let mut t = ta_with("one\ntwo");
        // カーソルをテキストの絶対末尾に配置
        t.set_cursor(t.text().len());
        // パニックせずに末尾に留まるべき
        t.move_cursor_to_end_of_line(true);
        assert_eq!(t.cursor(), t.text().len());

        // 最終行でない行のEOLにいるときの動作も検証：
        let eol_first_line = 3; // "one\ntwo"の'\n'のインデックス
        t.set_cursor(eol_first_line);
        t.move_cursor_to_end_of_line(true);
        assert_eq!(t.cursor(), t.text().len()); // moves to end of next (last) line
    }

    #[test]
    fn word_navigation_helpers() {
        let t = ta_with("  alpha  beta   gamma");
        let mut t = t; // set_cursor用に可変に
        // カーソルを"alpha"の後に配置
        let after_alpha = t.text().find("alpha").unwrap() + "alpha".len();
        t.set_cursor(after_alpha);
        assert_eq!(t.beginning_of_previous_word(), 2); // skip initial spaces

        // カーソルをbetaの先頭に配置
        let beta_start = t.text().find("beta").unwrap();
        t.set_cursor(beta_start);
        assert_eq!(t.end_of_next_word(), beta_start + "beta".len());

        // 末尾にいる場合、end_of_next_wordはlenを返す
        t.set_cursor(t.text().len());
        assert_eq!(t.end_of_next_word(), t.text().len());
    }

    #[test]
    fn wrapping_and_cursor_positions() {
        let mut t = ta_with("hello world here");
        let area = Rect::new(0, 0, 6, 10); // 幅6 -> ワードを折り返す
        // desired heightは折り返された行をカウント
        assert!(t.desired_height(area.width) >= 3);

        // カーソルを"world"内に配置
        let world_start = t.text().find("world").unwrap();
        t.set_cursor(world_start + 3);
        let (_x, y) = t.cursor_pos(area).unwrap();
        assert_eq!(y, 1); // world should be on second wrapped line

        // ステートと小さい高さで、カーソルは表示可能な行にマッピングされる
        let mut state = TextAreaState::default();
        let small_area = Rect::new(0, 0, 6, 1);
        // 最初の呼び出し：カーソルが見えない -> effective scrollがそれを保証
        let (_x, y) = t.cursor_pos_with_state(small_area, state).unwrap();
        assert_eq!(y, 0);

        // ステートでレンダリングして実際のスクロール値を更新
        let mut buf = Buffer::empty(small_area);
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), small_area, &mut buf, &mut state);
        // レンダリング後、state.scrollはカーソル行が収まるよう調整されるべき
        let effective_lines = t.desired_height(small_area.width);
        assert!(state.scroll < effective_lines);
    }

    #[test]
    fn cursor_pos_with_state_basic_and_scroll_behaviors() {
        // ケース1：折り返し不要、高さが収まる — スクロールは無視され、yは直接マッピング。
        let mut t = ta_with("hello world");
        t.set_cursor(3);
        let area = Rect::new(2, 5, 20, 3);
        // 不合理なスクロールが提供されても、コンテンツがエリアに収まるとき
        // effective scrollは0で、カーソル位置はcursor_posと一致。
        let bad_state = TextAreaState { scroll: 999 };
        let (x1, y1) = t.cursor_pos(area).unwrap();
        let (x2, y2) = t.cursor_pos_with_state(area, bad_state).unwrap();
        assert_eq!((x2, y2), (x1, y1));

        // ケース2：カーソルが現在のウィンドウより下 — effective scrollを調整後
        // 下端行（area.height - 1）にクランプされるべき。
        let mut t = ta_with("one two three four five six");
        // 多くの視覚的な行に強制的に折り返し。
        let wrap_width = 4;
        let _ = t.desired_height(wrap_width);
        // カーソルを末尾付近に配置し、最初のウィンドウより確実に下に。
        t.set_cursor(t.text().len().saturating_sub(2));
        let small_area = Rect::new(0, 0, wrap_width, 2);
        let state = TextAreaState { scroll: 0 };
        let (_x, y) = t.cursor_pos_with_state(small_area, state).unwrap();
        assert_eq!(y, small_area.y + small_area.height - 1);

        // ケース3：カーソルが現在のウィンドウより上 — 提供されたスクロールが
        // 大きすぎる場合、yは上端行（0）になるべき。
        let mut t = ta_with("alpha beta gamma delta epsilon zeta");
        let wrap_width = 5;
        let lines = t.desired_height(wrap_width);
        // カーソルを先頭付近に配置し、過剰なスクロールで上端行に移動させる。
        t.set_cursor(1);
        let area = Rect::new(0, 0, wrap_width, 3);
        let state = TextAreaState {
            scroll: lines.saturating_mul(2),
        };
        let (_x, y) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!(y, area.y);
    }

    #[test]
    fn wrapped_navigation_across_visual_lines() {
        let mut t = ta_with("abcdefghij");
        // 幅4で強制折り返し：行 -> ["abcd", "efgh", "ij"]
        let _ = t.desired_height(4);

        // 最初から、下移動は次の折り返し行の先頭（インデックス4）に行くべき
        t.set_cursor(0);
        t.move_cursor_down();
        assert_eq!(t.cursor(), 4);

        // 境界インデックス4のカーソルは2番目の折り返し行の先頭に表示されるべき
        t.set_cursor(4);
        let area = Rect::new(0, 0, 4, 10);
        let (x, y) = t.cursor_pos(area).unwrap();
        assert_eq!((x, y), (0, 1));

        // ステートと小さい高さで、カーソルは行0、列0に見えるべき
        let small_area = Rect::new(0, 0, 4, 1);
        let state = TextAreaState::default();
        let (x, y) = t.cursor_pos_with_state(small_area, state).unwrap();
        assert_eq!((x, y), (0, 0));

        // カーソルを2番目の折り返し行（"efgh"）の中央、'g'に配置
        t.set_cursor(6);
        // 上移動は前の折り返し行の同じ列に行くべき -> インデックス2（'c'）
        t.move_cursor_up();
        assert_eq!(t.cursor(), 2);

        // 下移動は次の折り返し行の同じ位置に戻るべき -> インデックス6（'g'）に戻る
        t.move_cursor_down();
        assert_eq!(t.cursor(), 6);

        // もう一度下移動で3番目の折り返し行へ。ターゲット列は2だが、行の長さは2 -> 末尾にクランプ
        t.move_cursor_down();
        assert_eq!(t.cursor(), t.text().len());
    }

    #[test]
    fn cursor_pos_with_state_after_movements() {
        let mut t = ta_with("abcdefghij");
        // Wrap width 4 -> visual lines: abcd | efgh | ij
        let _ = t.desired_height(4);
        let area = Rect::new(0, 0, 4, 2);
        let mut state = TextAreaState::default();
        let mut buf = Buffer::empty(area);

        // 先頭から開始
        t.set_cursor(0);
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x, y), (0, 0));

        // 2番目の視覚的な行に下移動。2行ビューポート内の下端行（行1）にいるべき
        t.move_cursor_down();
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x, y), (0, 1));

        // 3番目の視覚的な行に下移動。ビューポートがスクロールし、カーソルを下端行に保持
        t.move_cursor_down();
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x, y), (0, 1));

        // 2番目の視覚的な行に上移動。現在のスクロールで上端行に表示
        t.move_cursor_up();
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x, y) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x, y), (0, 0));

        // 移動を跨いだ列の保持：1行目の列2に設定し、下移動
        t.set_cursor(2);
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x0, y0) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x0, y0), (2, 0));
        t.move_cursor_down();
        ratatui::widgets::StatefulWidgetRef::render_ref(&(&t), area, &mut buf, &mut state);
        let (x1, y1) = t.cursor_pos_with_state(area, state).unwrap();
        assert_eq!((x1, y1), (2, 1));
    }

    #[test]
    fn wrapped_navigation_with_newlines_and_spaces() {
        // スペースと明示的な改行を含めて境界を検証
        let mut t = ta_with("word1  word2\nword3");
        // 幅6は"word1  "を折り返し、その後改行前に"word2"
        let _ = t.desired_height(6);

        // カーソルを改行前の2番目の折り返し行、"word2"の列1に配置
        let start_word2 = t.text().find("word2").unwrap();
        t.set_cursor(start_word2 + 1);

        // 上移動で1番目の折り返し行、列1 -> インデックス1
        t.move_cursor_up();
        assert_eq!(t.cursor(), 1);

        // 下移動で"word2"の同じ視覚的な列に戻る
        t.move_cursor_down();
        assert_eq!(t.cursor(), start_word2 + 1);

        // もう一度下移動で論理改行を越えて次の視覚的な行（"word3"）へ、必要に応じて長さにクランプ
        t.move_cursor_down();
        let start_word3 = t.text().find("word3").unwrap();
        assert!(t.cursor() >= start_word3 && t.cursor() <= start_word3 + "word3".len());
    }

    #[test]
    fn wrapped_navigation_with_wide_graphemes() {
        // 4つのサムズアップ、各々表示幅2、幅3でグラフェム境界内での折り返しを強制
        let mut t = ta_with("👍👍👍👍");
        let _ = t.desired_height(3);

        // カーソルを2番目の絵文字の後に配置（1番目の折り返し行にあるべき）
        t.set_cursor("👍👍".len());

        // 下移動で次の折り返し行の先頭に行くべき（同じ列が保持されるがクランプ）
        t.move_cursor_down();
        // 3番目の絵文字内またはその先頭に着地することを期待
        let pos_after_down = t.cursor();
        assert!(pos_after_down >= "👍👍".len());

        // 上移動で元の位置に戻るべき
        t.move_cursor_up();
        assert_eq!(t.cursor(), "👍👍".len());
    }

    #[test]
    fn fuzz_textarea_randomized() {
        // 再現性のための決定論的シード
        // 太平洋時間（PST/PDT）の現在の日付に基づいてRNGをシード。
        // これにより1日以内はファズテストが決定論的になりつつ、
        // 日ごとに変化してカバレッジを向上。
        let pst_today_seed: u64 = (chrono::Utc::now() - chrono::Duration::hours(8))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp() as u64;
        let mut rng = rand::rngs::StdRng::seed_from_u64(pst_today_seed);

        for _case in 0..500 {
            let mut ta = TextArea::new();
            let mut state = TextAreaState::default();
            // 挿入するエレメントペイロードを追跡。ペイロードは'['と']'を使用し、
            // rand_grapheme()では生成されないため、偶発的な衝突を回避。
            let mut elem_texts: Vec<String> = Vec::new();
            let mut next_elem_id: usize = 0;
            // ランダムなベース文字列で開始
            let base_len = rng.random_range(0..30);
            let mut base = String::new();
            for _ in 0..base_len {
                base.push_str(&rand_grapheme(&mut rng));
            }
            ta.set_text(&base);
            // 初期カーソル用の有効な文字境界を選択
            let mut boundaries: Vec<usize> = vec![0];
            boundaries.extend(ta.text().char_indices().map(|(i, _)| i).skip(1));
            boundaries.push(ta.text().len());
            let init = boundaries[rng.random_range(0..boundaries.len())];
            ta.set_cursor(init);

            let mut width: u16 = rng.random_range(1..=12);
            let mut height: u16 = rng.random_range(1..=4);

            for _step in 0..60 {
                // ほぼ安定した幅/高さ、時々変更
                if rng.random_bool(0.1) {
                    width = rng.random_range(1..=12);
                }
                if rng.random_bool(0.1) {
                    height = rng.random_range(1..=4);
                }

                // 操作を選択
                match rng.random_range(0..18) {
                    0 => {
                        // カーソル位置に小さなランダム文字列を挿入
                        let len = rng.random_range(0..6);
                        let mut s = String::new();
                        for _ in 0..len {
                            s.push_str(&rand_grapheme(&mut rng));
                        }
                        ta.insert_str(&s);
                    }
                    1 => {
                        // 小さなランダムスライスでreplace_range
                        let mut b: Vec<usize> = vec![0];
                        b.extend(ta.text().char_indices().map(|(i, _)| i).skip(1));
                        b.push(ta.text().len());
                        let i1 = rng.random_range(0..b.len());
                        let i2 = rng.random_range(0..b.len());
                        let (start, end) = if b[i1] <= b[i2] {
                            (b[i1], b[i2])
                        } else {
                            (b[i2], b[i1])
                        };
                        let insert_len = rng.random_range(0..=4);
                        let mut s = String::new();
                        for _ in 0..insert_len {
                            s.push_str(&rand_grapheme(&mut rng));
                        }
                        let before = ta.text().len();
                        // 選択した範囲がエレメントと交差する場合、replace_rangeはエレメント境界まで
                        // 拡張するため、単純なサイズ差分アサーションは成り立たない。
                        let intersects_element = elem_texts.iter().any(|payload| {
                            if let Some(pstart) = ta.text().find(payload) {
                                let pend = pstart + payload.len();
                                pstart < end && pend > start
                            } else {
                                false
                            }
                        });
                        ta.replace_range(start..end, &s);
                        if !intersects_element {
                            let after = ta.text().len();
                            assert_eq!(
                                after as isize,
                                before as isize + (s.len() as isize) - ((end - start) as isize)
                            );
                        }
                    }
                    2 => ta.delete_backward(rng.random_range(0..=3)),
                    3 => ta.delete_forward(rng.random_range(0..=3)),
                    4 => ta.delete_backward_word(),
                    5 => ta.kill_to_beginning_of_line(),
                    6 => ta.kill_to_end_of_line(),
                    7 => ta.move_cursor_left(),
                    8 => ta.move_cursor_right(),
                    9 => ta.move_cursor_up(),
                    10 => ta.move_cursor_down(),
                    11 => ta.move_cursor_to_beginning_of_line(true),
                    12 => ta.move_cursor_to_end_of_line(true),
                    13 => {
                        // ユニークなセンチネルペイロードでエレメントを挿入
                        let payload =
                            format!("[[EL#{}:{}]]", next_elem_id, rng.random_range(1000..9999));
                        next_elem_id += 1;
                        ta.insert_element(&payload);
                        elem_texts.push(payload);
                    }
                    14 => {
                        // 既存エレメント内への挿入を試行（境界にクランプされるべき）
                        if let Some(payload) = elem_texts.choose(&mut rng).cloned()
                            && let Some(start) = ta.text().find(&payload)
                        {
                            let end = start + payload.len();
                            if end - start > 2 {
                                let pos = rng.random_range(start + 1..end - 1);
                                let ins = rand_grapheme(&mut rng);
                                ta.insert_str_at(pos, &ins);
                            }
                        }
                    }
                    15 => {
                        // エレメントと交差する範囲を置換 -> エレメント全体が置換されるべき
                        if let Some(payload) = elem_texts.choose(&mut rng).cloned()
                            && let Some(start) = ta.text().find(&payload)
                        {
                            let end = start + payload.len();
                            // 交差する範囲 [start-δ, end-δ2) を作成
                            let mut s = start.saturating_sub(rng.random_range(0..=2));
                            let mut e = (end + rng.random_range(0..=2)).min(ta.text().len());
                            // String::replace_rangeの契約を満たすため文字境界に揃える
                            let txt = ta.text();
                            while s > 0 && !txt.is_char_boundary(s) {
                                s -= 1;
                            }
                            while e < txt.len() && !txt.is_char_boundary(e) {
                                e += 1;
                            }
                            if s < e {
                                // 小さな置換テキスト
                                let mut srep = String::new();
                                for _ in 0..rng.random_range(0..=2) {
                                    srep.push_str(&rand_grapheme(&mut rng));
                                }
                                ta.replace_range(s..e, &srep);
                            }
                        }
                    }
                    16 => {
                        // カーソルをエレメント内部の位置に設定してみる。クランプされるべき
                        if let Some(payload) = elem_texts.choose(&mut rng).cloned()
                            && let Some(start) = ta.text().find(&payload)
                        {
                            let end = start + payload.len();
                            if end - start > 2 {
                                let pos = rng.random_range(start + 1..end - 1);
                                ta.set_cursor(pos);
                            }
                        }
                    }
                    _ => {
                        // ワード境界にジャンプ
                        if rng.random_bool(0.5) {
                            let p = ta.beginning_of_previous_word();
                            ta.set_cursor(p);
                        } else {
                            let p = ta.end_of_next_word();
                            ta.set_cursor(p);
                        }
                    }
                }

                // 健全性不変条件
                assert!(ta.cursor() <= ta.text().len());

                // エレメント不変条件
                for payload in &elem_texts {
                    if let Some(start) = ta.text().find(payload) {
                        let end = start + payload.len();
                        // 1) エレメント内のテキストは最初に設定されたペイロードと一致
                        assert_eq!(&ta.text()[start..end], payload);
                        // 2) カーソルは厳密にエレメント内部にない
                        let c = ta.cursor();
                        assert!(
                            c <= start || c >= end,
                            "cursor inside element: {start}..{end} at {c}"
                        );
                    }
                }

                // レンダリングしてカーソル位置を計算。境界内にあることとパニックしないことを確認
                let area = Rect::new(0, 0, width, height);
                // すべての折り返し行に十分な高さのエリアへステートレスレンダリング
                let total_lines = ta.desired_height(width);
                let full_area = Rect::new(0, 0, width, total_lines.max(1));
                let mut buf = Buffer::empty(full_area);
                ratatui::widgets::WidgetRef::render_ref(&(&ta), full_area, &mut buf);

                // cursor_pos：存在する場合xは幅内でなければならない
                let _ = ta.cursor_pos(area);

                // cursor_pos_with_state：常にビューポート行内
                let (_x, _y) = ta
                    .cursor_pos_with_state(area, state)
                    .unwrap_or((area.x, area.y));

                // ステートフルレンダリングはパニックせず、スクロールを更新すべき
                let mut sbuf = Buffer::empty(area);
                ratatui::widgets::StatefulWidgetRef::render_ref(
                    &(&ta),
                    area,
                    &mut sbuf,
                    &mut state,
                );

                // 折り返し後、desired heightはスクロールなしでレンダリングする行数と等しい
                let total_lines = total_lines as usize;
                // コンテンツがエリアの高さに収まる場合、state.scrollはtotal_linesを超えてはならない
                if (height as usize) >= total_lines {
                    assert_eq!(state.scroll, 0);
                }
            }
        }
    }
}
