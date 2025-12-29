// This is derived from `ratatui::Terminal`, which is licensed under the following terms:
//
// The MIT License (MIT)
// Copyright (c) 2016-2022 Florian Dehau
// Copyright (c) 2023-2025 The Ratatui Developers
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.
use std::io;
use std::io::Write;

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::Colors;
use crossterm::style::Print;
use crossterm::style::SetAttribute;
use crossterm::style::SetBackgroundColor;
use crossterm::style::SetColors;
use crossterm::style::SetForegroundColor;
use crossterm::terminal::Clear;
use derive_more::IsVariant;
use ratatui::backend::Backend;
use ratatui::backend::ClearType;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::widgets::WidgetRef;

#[derive(Debug, Hash)]
pub struct Frame<'a> {
    /// このフレームの描画後にカーソルをどこに配置すべきか？
    ///
    /// `None`の場合、カーソルは非表示で位置はバックエンドによって制御される。
    /// `Some((x, y))`の場合、`Terminal::draw()`呼び出し後にカーソルは表示され`(x, y)`に配置される。
    pub(crate) cursor_position: Option<Position>,

    /// ビューポートの領域
    pub(crate) viewport_area: Rect,

    /// 現在のフレームを描画するために使用されるバッファ
    pub(crate) buffer: &'a mut Buffer,
}

impl Frame<'_> {
    /// 現在のフレームの領域
    ///
    /// レンダリング中に変更されないことが保証されているため、複数回呼び出し可能。
    ///
    /// アプリがバックエンドからのリサイズイベントをリッスンしている場合、
    /// 現在のフレームのレンダリングに使用される計算にはイベントからの値を無視し、
    /// 代わりにこの値を使用すべき（これが現在のフレームのレンダリングに使用されるバッファの領域）。
    pub const fn area(&self) -> Rect {
        self.viewport_area
    }

    /// [`WidgetRef::render_ref`]を使用して[`WidgetRef`]を現在のバッファにレンダリング。
    ///
    /// 通常、area引数は現在のフレームのサイズまたは現在のフレームのサブ領域
    /// （[`Layout`]を使用して全体領域を分割して取得可能）。
    #[allow(clippy::needless_pass_by_value)]
    pub fn render_widget_ref<W: WidgetRef>(&mut self, widget: W, area: Rect) {
        widget.render_ref(area, self.buffer);
    }

    /// このフレームの描画後、カーソルを表示し指定された(x, y)座標に配置。
    /// このメソッドが呼び出されない場合、カーソルは非表示になる。
    ///
    /// これは[`Terminal::hide_cursor`]、[`Terminal::show_cursor`]、
    /// [`Terminal::set_cursor_position`]の呼び出しと干渉することに注意。
    /// APIの1つを選択して一貫して使用すること。
    ///
    /// [`Terminal::hide_cursor`]: crate::Terminal::hide_cursor
    /// [`Terminal::show_cursor`]: crate::Terminal::show_cursor
    /// [`Terminal::set_cursor_position`]: crate::Terminal::set_cursor_position
    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) {
        self.cursor_position = Some(position.into());
    }

    /// この`Frame`が描画するバッファへの可変参照を取得。
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.buffer
    }
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct Terminal<B>
where
    B: Backend + Write,
{
    /// ターミナルとのインターフェースに使用されるバックエンド
    backend: B,
    /// 現在と前回の描画呼び出しの結果を保持。各描画パスの終了時に2つを比較し、
    /// ターミナルへの必要な更新を出力
    buffers: [Buffer; 2],
    /// 前の配列における現在のバッファのインデックス
    current: usize,
    /// カーソルが現在非表示かどうか
    pub hidden_cursor: bool,
    /// ビューポートの領域
    pub viewport_area: Rect,
    /// ターミナルの最後に知られたサイズ。内部バッファのリサイズが必要かどうかの検出に使用。
    pub last_known_screen_size: Size,
    /// カーソルの最後に知られた位置。ビューポートがインライン化されターミナルが
    /// リサイズされた時に新しい領域を見つけるために使用。
    pub last_known_cursor_pos: Position,
}

impl<B> Drop for Terminal<B>
where
    B: Backend,
    B: Write,
{
    #[allow(clippy::print_stderr)]
    fn drop(&mut self) {
        // カーソル状態の復元を試行
        if self.hidden_cursor
            && let Err(err) = self.show_cursor()
        {
            eprintln!("Failed to show the cursor: {err}");
        }
    }
}

impl<B> Terminal<B>
where
    B: Backend,
    B: Write,
{
    /// 指定された[`Backend`]と[`TerminalOptions`]で新しい[`Terminal`]を作成。
    pub fn with_options(mut backend: B) -> io::Result<Self> {
        let screen_size = backend.size()?;
        let cursor_pos = backend.get_cursor_position()?;
        Ok(Self {
            backend,
            buffers: [Buffer::empty(Rect::ZERO), Buffer::empty(Rect::ZERO)],
            current: 0,
            hidden_cursor: false,
            viewport_area: Rect::new(0, cursor_pos.y, 0, 0),
            last_known_screen_size: screen_size,
            last_known_cursor_pos: cursor_pos,
        })
    }

    /// レンダリング用にターミナル状態への一貫したビューを提供するFrameオブジェクトを取得。
    pub fn get_frame(&mut self) -> Frame<'_> {
        Frame {
            cursor_position: None,
            viewport_area: self.viewport_area,
            buffer: self.current_buffer_mut(),
        }
    }

    /// 現在のバッファを参照として取得。
    fn current_buffer(&self) -> &Buffer {
        &self.buffers[self.current]
    }

    /// 現在のバッファを可変参照として取得。
    fn current_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current]
    }

    /// 前のバッファを参照として取得。
    fn previous_buffer(&self) -> &Buffer {
        &self.buffers[1 - self.current]
    }

    /// 前のバッファを可変参照として取得。
    fn previous_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[1 - self.current]
    }

    /// バックエンドを取得
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// バックエンドを可変参照として取得
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// 前のバッファと現在のバッファの差分を取得し、描画のために現在のバックエンドに渡す。
    pub fn flush(&mut self) -> io::Result<()> {
        let updates = diff_buffers(self.previous_buffer(), self.current_buffer());
        let last_put_command = updates.iter().rfind(|command| command.is_put());
        if let Some(&DrawCommand::Put { x, y, .. }) = last_put_command {
            self.last_known_cursor_pos = Position { x, y };
        }
        draw(&mut self.backend, updates.into_iter())
    }

    /// 内部バッファが要求された領域に一致するようにTerminalを更新。
    ///
    /// 要求された領域はレンダリング時の一貫性を保つために保存される。
    /// これにより画面全体がクリアされる。
    pub fn resize(&mut self, screen_size: Size) -> io::Result<()> {
        self.last_known_screen_size = screen_size;
        Ok(())
    }

    /// ビューポート領域を設定。
    pub fn set_viewport_area(&mut self, area: Rect) {
        self.current_buffer_mut().resize(area);
        self.previous_buffer_mut().resize(area);
        self.viewport_area = area;
    }

    /// バックエンドにサイズを問い合わせ、前回のサイズと一致しない場合はリサイズ。
    pub fn autoresize(&mut self) -> io::Result<()> {
        let screen_size = self.size()?;
        if screen_size != self.last_known_screen_size {
            self.resize(screen_size)?;
        }
        Ok(())
    }

    /// 単一のフレームをターミナルに描画。
    ///
    /// 成功した場合は[`CompletedFrame`]を返し、それ以外は[`std::io::Error`]を返す。
    ///
    /// このメソッドに渡されるレンダーコールバックが失敗する可能性がある場合は、代わりに[`try_draw`]を使用。
    ///
    /// アプリケーションはターミナルを継続的にレンダリングするためにループで`draw`または
    /// [`try_draw`]を呼び出すべき。これらのメソッドはターミナルへの描画の主要なエントリポイント。
    ///
    /// [`try_draw`]: Terminal::try_draw
    ///
    /// このメソッドは:
    ///
    /// - 必要に応じてターミナルを自動リサイズ
    /// - レンダーコールバックを呼び出し、レンダリング用の[`Frame`]参照を渡す
    /// - 現在のバッファをバックエンドにコピーして現在の内部状態をフラッシュ
    /// - レンダリングクロージャ中に設定されていた場合、カーソルを最後に知られた位置に移動
    ///
    /// レンダーコールバックは呼び出された時にフレーム全体を完全にレンダリングすべきで、
    /// 前のフレームから変更されていない領域も含む。これは各フレームが前のフレームと比較されて
    /// 変更点を判定し、変更点のみがターミナルに書き込まれるため。レンダーコールバックが
    /// フレームを完全にレンダリングしない場合、ターミナルは一貫性のない状態になる。
    pub fn draw<F>(&mut self, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.try_draw(|frame| {
            render_callback(frame);
            io::Result::Ok(())
        })
    }

    /// 単一のフレームをターミナルに描画しようとする。
    ///
    /// 成功した場合は[`CompletedFrame`]を含む[`Result::Ok`]を返し、
    /// それ以外は失敗の原因となった[`std::io::Error`]を含む[`Result::Err`]を返す。
    ///
    /// これは[`Terminal::draw`]と同等だが、レンダーコールバックは何も返さない代わりに
    /// `Result`を返す関数またはクロージャ。
    ///
    /// アプリケーションはターミナルを継続的にレンダリングするためにループで`try_draw`または
    /// [`draw`]を呼び出すべき。これらのメソッドはターミナルへの描画の主要なエントリポイント。
    ///
    /// [`draw`]: Terminal::draw
    ///
    /// このメソッドは:
    ///
    /// - 必要に応じてターミナルを自動リサイズ
    /// - レンダーコールバックを呼び出し、レンダリング用の[`Frame`]参照を渡す
    /// - 現在のバッファをバックエンドにコピーして現在の内部状態をフラッシュ
    /// - レンダリングクロージャ中に設定されていた場合、カーソルを最後に知られた位置に移動
    /// - 現在のバッファとターミナルの領域を含む[`CompletedFrame`]を返す
    ///
    /// `try_draw`に渡されるレンダーコールバックは、[`Into`]トレイトを使用して
    /// [`std::io::Error`]に変換可能なエラー型を持つ任意の[`Result`]を返すことができる。
    /// これによりレンダリング中に発生するエラーを`?`演算子で伝播することが可能。
    /// レンダーコールバックがエラーを返した場合、エラーは`try_draw`から[`std::io::Error`]として
    /// 返され、ターミナルは更新されない。
    ///
    /// このメソッドが返す[`CompletedFrame`]はデバッグやテスト目的に有用だが、
    /// 通常のアプリケーションでは使用されないことが多い。
    ///
    /// レンダーコールバックは呼び出された時にフレーム全体を完全にレンダリングすべきで、
    /// 前のフレームから変更されていない領域も含む。これは各フレームが前のフレームと比較されて
    /// 変更点を判定し、変更点のみがターミナルに書き込まれるため。レンダー関数が
    /// フレームを完全にレンダリングしない場合、ターミナルは一貫性のない状態になる。
    pub fn try_draw<F, E>(&mut self, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame) -> Result<(), E>,
        E: Into<io::Error>,
    {
        // 自動リサイズ - そうしないと縮小時にグリッチが発生するか、拡大時に
        // ウィジェットとターミナル間の非同期が発生しOOBになる可能性がある。
        self.autoresize()?;

        let mut frame = self.get_frame();

        render_callback(&mut frame).map_err(Into::into)?;

        // フレームを先にstdoutにフラッシュする必要があるため、カーソル位置をすぐに変更できない。
        // しかしフレームはBufferへの&mutを保持しているため、フレームを保持し続けることもできない。
        // そのためFrameから重要なデータを取り出してドロップする。
        let cursor_position = frame.cursor_position;

        // stdoutに描画
        self.flush()?;

        match cursor_position {
            None => self.hide_cursor()?,
            Some(position) => {
                self.show_cursor()?;
                self.set_cursor_position(position)?;
            }
        }

        self.swap_buffers();

        Backend::flush(&mut self.backend)?;

        Ok(())
    }

    /// カーソルを非表示にする。
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        self.hidden_cursor = true;
        Ok(())
    }

    /// カーソルを表示する。
    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.backend.show_cursor()?;
        self.hidden_cursor = false;
        Ok(())
    }

    /// 現在のカーソル位置を取得。
    ///
    /// これは最後の描画呼び出し後のカーソル位置。
    #[allow(dead_code)]
    pub fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.backend.get_cursor_position()
    }

    /// カーソル位置を設定。
    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        self.backend.set_cursor_position(position)?;
        self.last_known_cursor_pos = position;
        Ok(())
    }

    /// ターミナルをクリアし、次の描画呼び出しで完全な再描画を強制。
    pub fn clear(&mut self) -> io::Result<()> {
        if self.viewport_area.is_empty() {
            return Ok(());
        }
        self.backend
            .set_cursor_position(self.viewport_area.as_position())?;
        self.backend.clear_region(ClearType::AfterCursor)?;
        // 次の更新で全てを再描画するようにバックバッファをリセット。
        self.previous_buffer_mut().reset();
        Ok(())
    }

    /// 非アクティブなバッファをクリアし、現在のバッファと交換
    pub fn swap_buffers(&mut self) {
        self.previous_buffer_mut().reset();
        self.current = 1 - self.current;
    }

    /// バックエンドの実際のサイズを問い合わせ。
    pub fn size(&self) -> io::Result<Size> {
        self.backend.size()
    }
}

use ratatui::buffer::Cell;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, IsVariant)]
enum DrawCommand {
    Put { x: u16, y: u16, cell: Cell },
    ClearToEnd { x: u16, y: u16, bg: Color },
}

fn diff_buffers(a: &Buffer, b: &Buffer) -> Vec<DrawCommand> {
    let previous_buffer = &a.content;
    let next_buffer = &b.content;

    let mut updates = vec![];
    let mut last_nonblank_columns = vec![0; a.area.height as usize];
    for y in 0..a.area.height {
        let row_start = y as usize * a.area.width as usize;
        let row_end = row_start + a.area.width as usize;
        let row = &next_buffer[row_start..row_end];
        let bg = row.last().map(|cell| cell.bg).unwrap_or(Color::Reset);

        // 行をスキャンしてまだ重要な右端の列を見つける: 非スペースグリフ、
        // bgが行の末尾bgと異なるセル、またはモディファイア付きのセル。
        // マルチ幅グリフはその表示幅全体にわたって領域を拡張。
        // その後の行の残りは単一のClearToEndでクリアでき、複数のスペースPutコマンドを
        // 発行するよりパフォーマンスが向上。
        let mut last_nonblank_column = 0usize;
        let mut column = 0usize;
        while column < row.len() {
            let cell = &row[column];
            let width = cell.symbol().width();
            if cell.symbol() != " " || cell.bg != bg || cell.modifier != Modifier::empty() {
                last_nonblank_column = column + (width.saturating_sub(1));
            }
            column += width.max(1); // treat zero-width symbols as width 1
        }

        if last_nonblank_column + 1 < row.len() {
            let (x, y) = a.pos_of(row_start + last_nonblank_column + 1);
            updates.push(DrawCommand::ClearToEnd { x, y, bg });
        }

        last_nonblank_columns[y as usize] = last_nonblank_column as u16;
    }

    // 先行するマルチ幅文字の描画/置換により無効化されたセル:
    let mut invalidated: usize = 0;
    // 先行するマルチ幅文字がその場所を占めるためスキップすべき現在のバッファからのセル
    // （スキップされるセルはとにかく空白のはず）、またはセル毎スキップのため:
    let mut to_skip: usize = 0;
    for (i, (current, previous)) in next_buffer.iter().zip(previous_buffer.iter()).enumerate() {
        if !current.skip && (current != previous || invalidated > 0) && to_skip == 0 {
            let (x, y) = a.pos_of(i);
            let row = i / a.area.width as usize;
            if x <= last_nonblank_columns[row] {
                updates.push(DrawCommand::Put {
                    x,
                    y,
                    cell: next_buffer[i].clone(),
                });
            }
        }

        to_skip = current.symbol().width().saturating_sub(1);

        let affected_width = std::cmp::max(current.symbol().width(), previous.symbol().width());
        invalidated = std::cmp::max(affected_width, invalidated).saturating_sub(1);
    }
    updates
}

fn draw<I>(writer: &mut impl Write, commands: I) -> io::Result<()>
where
    I: Iterator<Item = DrawCommand>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut modifier = Modifier::empty();
    let mut last_pos: Option<Position> = None;
    for command in commands {
        let (x, y) = match command {
            DrawCommand::Put { x, y, .. } => (x, y),
            DrawCommand::ClearToEnd { x, y, .. } => (x, y),
        };
        // 前の位置が(x - 1, y)でなかった場合、カーソルを移動
        if !matches!(last_pos, Some(p) if x == p.x + 1 && y == p.y) {
            queue!(writer, MoveTo(x, y))?;
        }
        last_pos = Some(Position { x, y });
        match command {
            DrawCommand::Put { cell, .. } => {
                if cell.modifier != modifier {
                    let diff = ModifierDiff {
                        from: modifier,
                        to: cell.modifier,
                    };
                    diff.queue(writer)?;
                    modifier = cell.modifier;
                }
                if cell.fg != fg || cell.bg != bg {
                    queue!(
                        writer,
                        SetColors(Colors::new(cell.fg.into(), cell.bg.into()))
                    )?;
                    fg = cell.fg;
                    bg = cell.bg;
                }

                queue!(writer, Print(cell.symbol()))?;
            }
            DrawCommand::ClearToEnd { bg: clear_bg, .. } => {
                queue!(writer, SetAttribute(crossterm::style::Attribute::Reset))?;
                modifier = Modifier::empty();
                queue!(writer, SetBackgroundColor(clear_bg.into()))?;
                bg = clear_bg;
                queue!(writer, Clear(crossterm::terminal::ClearType::UntilNewLine))?;
            }
        }
    }

    queue!(
        writer,
        SetForegroundColor(crossterm::style::Color::Reset),
        SetBackgroundColor(crossterm::style::Color::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )?;

    Ok(())
}

/// `ModifierDiff`構造体は2つの`Modifier`値間の差分を計算するために使用される。
/// これはターミナル表示の更新時に便利で、必要な変更のみを送信することで
/// より効率的な更新を可能にする。
struct ModifierDiff {
    pub from: Modifier,
    pub to: Modifier,
}

impl ModifierDiff {
    fn queue<W: io::Write>(self, w: &mut W) -> io::Result<()> {
        use crossterm::style::Attribute as CAttribute;
        let removed = self.from - self.to;
        if removed.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::NoReverse))?;
        }
        if removed.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
            if self.to.contains(Modifier::DIM) {
                queue!(w, SetAttribute(CAttribute::Dim))?;
            }
        }
        if removed.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::NoItalic))?;
        }
        if removed.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::NoUnderline))?;
        }
        if removed.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
        }
        if removed.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::NotCrossedOut))?;
        }
        if removed.contains(Modifier::SLOW_BLINK) || removed.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::NoBlink))?;
        }

        let added = self.to - self.from;
        if added.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::Reverse))?;
        }
        if added.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::Bold))?;
        }
        if added.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::Italic))?;
        }
        if added.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::Underlined))?;
        }
        if added.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::Dim))?;
        }
        if added.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::CrossedOut))?;
        }
        if added.contains(Modifier::SLOW_BLINK) {
            queue!(w, SetAttribute(CAttribute::SlowBlink))?;
        }
        if added.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::RapidBlink))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn diff_buffers_does_not_emit_clear_to_end_for_full_width_row() {
        let area = Rect::new(0, 0, 3, 2);
        let previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);

        next.cell_mut((2, 0))
            .expect("cell should exist")
            .set_symbol("X");

        let commands = diff_buffers(&previous, &next);

        let clear_count = commands
            .iter()
            .filter(|command| matches!(command, DrawCommand::ClearToEnd { y, .. } if *y == 0))
            .count();
        assert_eq!(
            0, clear_count,
            "expected diff_buffers not to emit ClearToEnd; commands: {commands:?}",
        );
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Put { x: 2, y: 0, .. })),
            "expected diff_buffers to update the final cell; commands: {commands:?}",
        );
    }

    #[test]
    fn diff_buffers_clear_to_end_starts_after_wide_char() {
        let area = Rect::new(0, 0, 10, 1);
        let mut previous = Buffer::empty(area);
        let mut next = Buffer::empty(area);

        previous.set_string(0, 0, "中文", Style::default());
        next.set_string(0, 0, "中", Style::default());

        let commands = diff_buffers(&previous, &next);
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, DrawCommand::ClearToEnd { x: 2, y: 0, .. })),
            "expected clear-to-end to start after the remaining wide char; commands: {commands:?}"
        );
    }
}
