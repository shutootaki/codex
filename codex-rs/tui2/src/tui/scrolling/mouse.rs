//! マウスホイール/トラックパッド入力のスクロール正規化。
//!
//! ターミナルのスクロールイベントは、ホイールのティック（ノッチ）あたりのイベント数が大きく異なり、
//! イベント間のタイミングもホイールとトラックパッドで大きく重複する。スクロール入力を正規化するため、
//! イベントをギャップで区切られた短いストリームとして扱い、ターミナルごとの「ティックあたりイベント数」
//! ファクターで行デルタに変換し、再描画を一定の周期にまとめる。
//!
//! マウスホイールの「ティック」（1ノッチ）は、ターミナルの生イベント密度に関係なく、
//! 固定行数（デフォルト: 3）でスクロールすることが期待される。トラックパッドスクロールは
//! より高精度を維持すべきである（小さな動きは行未満の蓄積となり、整数行に達したときのみスクロール）。
//!
//! ターミナルのマウススクロールイベントは大きさ（方向のみ）をエンコードしないため、
//! ホイールとトラックパッドの検出はヒューリスティックである。オーバーシュートを避けるため
//! トラックパッドとして扱うことをデフォルトとし、最初のティック分のイベントが素早く到着した場合に
//! ホイールに「昇格」させる。ヒューリスティックが合わない場合は、設定でホイール/トラックパッド
//! の動作を強制できる。
//!
//! データに基づく定数と分析については `codex-rs/tui2/docs/scroll_input_model.md` を参照。

use codex_core::config::types::ScrollInputMode;
use codex_core::terminal::TerminalInfo;
use codex_core::terminal::TerminalName;
use std::time::Duration;
use std::time::Instant;

const STREAM_GAP_MS: u64 = 80;
const STREAM_GAP: Duration = Duration::from_millis(STREAM_GAP_MS);
const REDRAW_CADENCE_MS: u64 = 16;
const REDRAW_CADENCE: Duration = Duration::from_millis(REDRAW_CADENCE_MS);
const DEFAULT_EVENTS_PER_TICK: u16 = 3;
const DEFAULT_WHEEL_LINES_PER_TICK: u16 = 3;
const DEFAULT_TRACKPAD_LINES_PER_TICK: u16 = 1;
const DEFAULT_SCROLL_MODE: ScrollInputMode = ScrollInputMode::Auto;
const DEFAULT_WHEEL_TICK_DETECT_MAX_MS: u64 = 12;
const DEFAULT_WHEEL_LIKE_MAX_DURATION_MS: u64 = 200;
const DEFAULT_TRACKPAD_ACCEL_EVENTS: u16 = 30;
const DEFAULT_TRACKPAD_ACCEL_MAX: u16 = 3;
const MAX_EVENTS_PER_STREAM: usize = 256;
const MAX_ACCUMULATED_LINES: i32 = 256;
const MIN_LINES_PER_WHEEL_STREAM: i32 = 1;

fn default_wheel_tick_detect_max_ms_for_terminal(name: TerminalName) -> u64 {
    // このしきい値は、自動モードでの「ホイールに昇格」のファストパスでのみ使用される。
    // ターミナルごとに設定しているのは、一部のターミナルが数十ミリ秒にわたってホイールティックを
    // 発行するため。グローバルなしきい値を厳しくすると、それらのホイールティックがトラックパッド
    // として誤分類され、遅く感じる。
    match name {
        TerminalName::WarpTerminal => 20,
        _ => DEFAULT_WHEEL_TICK_DETECT_MAX_MS,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollStreamKind {
    Unknown,
    Wheel,
    Trackpad,
}

/// 行デルタの符号を決定するための高レベルスクロール方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollDirection {
    Up,
    Down,
}

impl ScrollDirection {
    fn sign(self) -> i32 {
        match self {
            ScrollDirection::Up => -1,
            ScrollDirection::Down => 1,
        }
    }

    fn inverted(self) -> Self {
        match self {
            ScrollDirection::Up => ScrollDirection::Down,
            ScrollDirection::Down => ScrollDirection::Up,
        }
    }
}

/// ターミナルメタデータとユーザーオーバーライドから導出されるスクロール正規化設定。
///
/// これらは [`MouseScrollState`] が生の `ScrollUp`/`ScrollDown` イベントを
/// トランスクリプトビューポートの*表示行*のデルタに変換するためのノブである。
///
/// - `events_per_line` はターミナルごとの「イベント密度」を正規化する（スクロール移動の
///   1単位に対応する生イベント数）。
/// - `wheel_lines_per_tick` は短い離散ストリームをスケーリングし、単一のマウスホイールノッチが
///   従来の複数行スクロールの感触を維持するようにする。
///
/// プローブデータと根拠については `codex-rs/tui2/docs/scroll_input_model.md` を参照。
/// ユーザー向けオーバーライドは `config.toml` で以下として公開される:
/// - `tui.scroll_events_per_tick`
/// - `tui.scroll_wheel_lines`
/// - `tui.scroll_invert`
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScrollConfig {
    /// ターミナルごとの正規化ファクター（「ホイールティックあたりイベント数」）。
    ///
    /// ターミナルは同じ物理的ホイールノッチに対して約1〜9以上の生イベントを発行できる。
    /// このファクターを使用して生イベント数を「ティック」推定値に変換する。
    ///
    /// 各生スクロールイベントは `1 / events_per_tick` ティックを寄与する。そのティック値は
    /// アクティブなスクロールモード（ホイール vs トラックパッド）に応じて行にスケーリングされる。
    ///
    /// ユーザー向け名: `tui.scroll_events_per_tick`。
    events_per_tick: u16,

    /// マウスホイールティックあたりに適用される行数。
    ///
    /// 入力がホイールとして解釈される場合、1つの物理ホイールノッチがこの行数に
    /// マッピングされる。デフォルトは3で、典型的な「クラシックターミナル」スクロールに一致。
    wheel_lines_per_tick: u16,

    /// トラックパッドスクロールのティック相当あたりに適用される行数。
    ///
    /// トラックパッドには離散的な「ティック」がないが、ターミナルは依然として離散的な
    /// 上/下イベントを発行する。トラックパッドストリームを `trackpad_lines_per_tick / events_per_tick`
    /// 行/イベントとして解釈し、整数行に達するまで端数を蓄積する。
    trackpad_lines_per_tick: u16,

    /// トラックパッドアクセラレーション: +1倍の速度を得るのに必要なおおよそのイベント数。
    ///
    /// これは実用的なUXノブ: 一部のターミナルではトラックパッド入力の垂直イベント密度が
    /// 比較的低く、小さなスワイプが正しく感じても大きな/速いスワイプが遅く感じることがある。
    trackpad_accel_events: u16,

    /// トラックパッドアクセラレーション: トラックパッドストリームに適用される最大倍率。
    ///
    /// 1に設定するとアクセラレーションを実質無効化。
    trackpad_accel_max: u16,

    /// ホイール/トラックパッドの動作を強制するか、ストリームごとに推測する。
    mode: ScrollInputMode,

    /// 自動モードしきい値: 最初のホイールティックがホイールと見なされるまでの完了時間。
    ///
    /// ストリームの最初のイベントから `events_per_tick` イベントを確認するまでの時間を使用。
    /// 最初のティックがこれより速く完了すれば、ストリームをホイールに昇格させる。
    /// そうでなければ、トラックパッドとして扱い続ける。
    wheel_tick_detect_max: Duration,

    /// 自動モードフォールバック: 「ホイールライク」と見なされる最大期間。
    ///
    /// ストリームがこの期間内に終了し、自信を持って分類できなかった場合、ホイールとして扱う。
    /// これにより1イベント/ティックターミナル（WezTerm/iTerm/VS Code）でのホイールノッチも
    /// クラシックな複数行動作を得られる。
    wheel_like_max_duration: Duration,

    /// 垂直スクロール方向の符号を反転。
    ///
    /// ターミナルレベルの反転設定を推測しようとはしない。これは明示的な
    /// アプリケーションレベルのトグル。
    invert_direction: bool,
}

/// スクロール設定のオプションのユーザーオーバーライド。
///
/// ほとんどの呼び出し元はマージされた [`codex_core::config::Config`] フィールドから
/// これを構築すべきであり、TUI2はターミナルのデフォルトを継承し、ユーザーが設定した
/// もののみをオーバーライドする。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ScrollConfigOverrides {
    pub(crate) events_per_tick: Option<u16>,
    pub(crate) wheel_lines_per_tick: Option<u16>,
    pub(crate) trackpad_lines_per_tick: Option<u16>,
    pub(crate) trackpad_accel_events: Option<u16>,
    pub(crate) trackpad_accel_max: Option<u16>,
    pub(crate) mode: Option<ScrollInputMode>,
    pub(crate) wheel_tick_detect_max_ms: Option<u64>,
    pub(crate) wheel_like_max_duration_ms: Option<u64>,
    pub(crate) invert_direction: bool,
}

impl ScrollConfig {
    /// 検出されたターミナルメタデータからスクロール正規化のデフォルトを導出。
    ///
    /// [`TerminalInfo`]（特に [`TerminalName`]）を使用して経験的に導出された
    /// `events_per_line` デフォルトを選択する。ユーザーは `config.toml` 経由で
    /// `events_per_line` とホイールティックごとの乗数の両方をオーバーライドできる
    /// （[`ScrollConfig`] のドキュメントを参照）。
    pub(crate) fn from_terminal(terminal: &TerminalInfo, overrides: ScrollConfigOverrides) -> Self {
        let mut events_per_tick = match terminal.name {
            TerminalName::AppleTerminal => 3,
            TerminalName::WarpTerminal => 9,
            TerminalName::WezTerm => 1,
            TerminalName::Alacritty => 3,
            TerminalName::Ghostty => 3,
            TerminalName::Iterm2 => 1,
            TerminalName::VsCode => 1,
            TerminalName::Kitty => 3,
            _ => DEFAULT_EVENTS_PER_TICK,
        };

        if let Some(override_value) = overrides.events_per_tick {
            events_per_tick = override_value.max(1);
        }

        let mut wheel_lines_per_tick = DEFAULT_WHEEL_LINES_PER_TICK;
        if let Some(override_value) = overrides.wheel_lines_per_tick {
            wheel_lines_per_tick = override_value.max(1);
        }

        let mut trackpad_lines_per_tick = DEFAULT_TRACKPAD_LINES_PER_TICK;
        if let Some(override_value) = overrides.trackpad_lines_per_tick {
            trackpad_lines_per_tick = override_value.max(1);
        }

        let mut trackpad_accel_events = DEFAULT_TRACKPAD_ACCEL_EVENTS;
        if let Some(override_value) = overrides.trackpad_accel_events {
            trackpad_accel_events = override_value.max(1);
        }

        let mut trackpad_accel_max = DEFAULT_TRACKPAD_ACCEL_MAX;
        if let Some(override_value) = overrides.trackpad_accel_max {
            trackpad_accel_max = override_value.max(1);
        }

        let wheel_tick_detect_max_ms = overrides
            .wheel_tick_detect_max_ms
            .unwrap_or_else(|| default_wheel_tick_detect_max_ms_for_terminal(terminal.name));
        let wheel_tick_detect_max = Duration::from_millis(wheel_tick_detect_max_ms);
        let wheel_like_max_duration = Duration::from_millis(
            overrides
                .wheel_like_max_duration_ms
                .unwrap_or(DEFAULT_WHEEL_LIKE_MAX_DURATION_MS),
        );

        Self {
            events_per_tick,
            wheel_lines_per_tick,
            trackpad_lines_per_tick,
            trackpad_accel_events,
            trackpad_accel_max,
            mode: overrides.mode.unwrap_or(DEFAULT_SCROLL_MODE),
            wheel_tick_detect_max,
            wheel_like_max_duration,
            invert_direction: overrides.invert_direction,
        }
    }

    fn events_per_tick_f32(self) -> f32 {
        self.events_per_tick.max(1) as f32
    }

    fn wheel_lines_per_tick_f32(self) -> f32 {
        self.wheel_lines_per_tick.max(1) as f32
    }

    fn trackpad_lines_per_tick_f32(self) -> f32 {
        self.trackpad_lines_per_tick.max(1) as f32
    }

    fn trackpad_events_per_tick_f32(self) -> f32 {
        // `events_per_tick` はホイールの動作から導出され、同じ物理的移動に対する
        // 実際のトラックパッドイベント密度よりもはるかに大きくなりうる。これをトラックパッドに
        // 直接使用すると、Ghostty/Warpなどのターミナルが人工的に遅く感じる。
        //
        // グローバルな「典型的」ホイールティックサイズ（3）で上限を設定することで、
        // ホイールの正規化を維持しつつ、ターミナル間でより一貫したトラックパッドの感触を実現。
        self.events_per_tick.clamp(1, DEFAULT_EVENTS_PER_TICK) as f32
    }

    fn trackpad_accel_events_f32(self) -> f32 {
        self.trackpad_accel_events.max(1) as f32
    }

    fn trackpad_accel_max_f32(self) -> f32 {
        self.trackpad_accel_max.max(1) as f32
    }

    fn apply_direction(self, direction: ScrollDirection) -> ScrollDirection {
        if self.invert_direction {
            direction.inverted()
        } else {
            direction
        }
    }
}

impl Default for ScrollConfig {
    fn default() -> Self {
        Self {
            events_per_tick: DEFAULT_EVENTS_PER_TICK,
            wheel_lines_per_tick: DEFAULT_WHEEL_LINES_PER_TICK,
            trackpad_lines_per_tick: DEFAULT_TRACKPAD_LINES_PER_TICK,
            trackpad_accel_events: DEFAULT_TRACKPAD_ACCEL_EVENTS,
            trackpad_accel_max: DEFAULT_TRACKPAD_ACCEL_MAX,
            mode: DEFAULT_SCROLL_MODE,
            wheel_tick_detect_max: Duration::from_millis(DEFAULT_WHEEL_TICK_DETECT_MAX_MS),
            wheel_like_max_duration: Duration::from_millis(DEFAULT_WHEEL_LIKE_MAX_DURATION_MS),
            invert_direction: false,
        }
    }
}

/// スクロール処理からの出力: 適用する行数とストリーム終了確認のタイミング。
///
/// 呼び出し元は `lines` を即座に適用すべき。`next_tick_in` が `Some` の場合、
/// フォローアップティックをスケジュール（通常はフレームを要求）して、
/// [`MouseScrollState::on_tick`] が無音期間後にストリームを閉じられるようにする。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScrollUpdate {
    pub(crate) lines: i32,
    pub(crate) next_tick_in: Option<Duration>,
}

/// マウススクロール入力ストリームを追跡し、再描画をまとめる。
///
/// これは離散的なターミナルスクロールイベント（`ScrollUp`/`ScrollDown`）を
/// ビューポート行デルタに変換するステートマシン。
/// `codex-rs/tui2/docs/scroll_input_model.md` に記述されているストリームベースモデルを実装:
///
/// - **ストリーム**: イベントのシーケンスは、[`STREAM_GAP`] より大きいギャップまたは
///   方向の反転がストリームを閉じるまで、1つのユーザージェスチャーとして扱われる。
/// - **正規化**: ストリームは [`ScrollConfig`] を使用して行デルタに変換される
///   （ターミナルごとの `events_per_tick`、モードごとの lines-per-tick、オプションの反転）。
/// - **まとめ**: トラックパッドストリームは最大 [`REDRAW_CADENCE`] ごとにフラッシュされ、
///   非常に高密度なターミナルでの氾濫を回避。ホイールストリームは即座にフラッシュして
///   レスポンシブに感じさせる。
/// - **フォローアップティック**: ストリームのクローズは*時間ギャップ*で定義されるため、
///   ストリームがアクティブな間、呼び出し元は定期的なティックをスケジュールする必要がある。
///   返される [`ScrollUpdate::next_tick_in`] が次の推奨ウェイクアップを提供。
///
/// 典型的な使用法:
/// - 各垂直スクロールイベントに対して [`MouseScrollState::on_scroll_event`] を呼び出す。
/// - 返された [`ScrollUpdate::lines`] をトランスクリプトスクロール状態に適用。
/// - [`ScrollUpdate::next_tick_in`] が存在する場合、遅延ティックをスケジュールし、
///   アイドル後にストリームを閉じるために [`MouseScrollState::on_tick`] を呼び出す。
#[derive(Clone, Debug)]
pub(crate) struct MouseScrollState {
    stream: Option<ScrollStream>,
    last_redraw_at: Instant,
    carry_lines: f32,
    carry_direction: Option<ScrollDirection>,
}

impl MouseScrollState {
    /// 決定論的な時間起点を持つ新しいスクロール状態を作成。
    ///
    /// これは主にユニットテストで使用され、`now` 値を選択することでまとめやストリームギャップの
    /// 動作を制御できる。本番コードは一般的に [`Default`] と
    /// `Instant::now()` ベースのエントリポイントを使用。
    fn new_at(now: Instant) -> Self {
        Self {
            stream: None,
            last_redraw_at: now,
            carry_lines: 0.0,
            carry_direction: None,
        }
    }

    /// 現在時刻を使用してスクロールイベントを処理。
    ///
    /// これはTUIイベントループで使用される通常の本番エントリポイント。
    /// `Instant::now()` を使用して [`MouseScrollState::on_scroll_event_at`] に転送。
    ///
    /// 返された [`ScrollUpdate::next_tick_in`] が `Some` の場合、呼び出し元は将来の
    /// ティックをスケジュール（通常はフレームを要求）し、ストリームがアイドル後に閉じられるよう
    /// [`MouseScrollState::on_tick`]（またはテストでは [`MouseScrollState::on_tick_at`]）
    /// を呼び出すべき。それらのティックがなければ、ストリームは*新しい*スクロールイベントが
    /// 到着したときのみ閉じられ、トラックパッドスクロールの端数がフラッシュされずに残り、
    /// 停止動作がラグって感じることがある。
    pub(crate) fn on_scroll_event(
        &mut self,
        direction: ScrollDirection,
        config: ScrollConfig,
    ) -> ScrollUpdate {
        self.on_scroll_event_at(Instant::now(), direction, config)
    }

    /// 特定の時刻でスクロールイベントを処理。
    ///
    /// これはスクロールストリームステートマシンの決定論的エントリポイント。
    /// 壁時計時間に依存せずに、ストリーム分割、まとめられた再描画、ストリーム終了時のフラッシュを
    /// テストするユニットテストを書くために存在。
    ///
    /// 動作は [`MouseScrollState::on_scroll_event`] と同一だが、呼び出し元がタイムスタンプ
    /// (`now`) を提供する。実際のアプリでは、タイムスタンプは `Instant::now()` から。
    ///
    /// 主要な詳細（完全なモデルは `codex-rs/tui2/docs/scroll_input_model.md` を参照）:
    ///
    /// - **ストリーム境界**: [`STREAM_GAP`] より大きいギャップまたは方向の反転は、
    ///   前のストリームを閉じて新しいストリームを開始。
    /// - **ホイール vs トラックパッド**: 自動モードでティック分のイベントが素早く到着すると、
    ///   ストリームの種類がホイールに昇格されうる。そうでなければトラックパッドのまま。
    /// - **再描画のまとめ**: ホイールストリームは即座にフラッシュ。トラックパッドストリームは
    ///   最大 [`REDRAW_CADENCE`] ごとにフラッシュ。
    /// - **フォローアップティック**: 返された [`ScrollUpdate::next_tick_in`] は、アイドル
    ///   ストリームを閉じて残りの整数行をフラッシュするために [`MouseScrollState::on_tick_at`]
    ///   を呼び出すべきタイミングを呼び出し元に通知。TUI2ではアプリのフレームスケジューラを通じて配線。
    pub(crate) fn on_scroll_event_at(
        &mut self,
        now: Instant,
        direction: ScrollDirection,
        config: ScrollConfig,
    ) -> ScrollUpdate {
        let direction = config.apply_direction(direction);
        let mut lines = 0;

        if let Some(mut stream) = self.stream.take() {
            let gap = now.duration_since(stream.last);
            if gap > STREAM_GAP || stream.direction != direction {
                lines += self.finalize_stream_at(now, &mut stream);
            } else {
                self.stream = Some(stream);
            }
        }

        if self.stream.is_none() {
            if self.carry_direction != Some(direction) {
                self.carry_lines = 0.0;
                self.carry_direction = Some(direction);
            }
            self.stream = Some(ScrollStream::new(now, direction, config));
        }
        let carry_lines = self.carry_lines;
        let Some(stream) = self.stream.as_mut() else {
            unreachable!("stream inserted above");
        };
        stream.push_event(now, direction);
        stream.maybe_promote_kind(now);

        // ホイールスクロールは即座に感じるべき。トラックパッドストリームは非常に高密度な
        // ターミナルでの氾濫を避けるため、固定の再描画周期にまとめられる。
        if stream.is_wheel_like()
            || now.duration_since(self.last_redraw_at) >= REDRAW_CADENCE
            || stream.just_promoted
        {
            lines += Self::flush_lines_at(&mut self.last_redraw_at, carry_lines, now, stream);
            stream.just_promoted = false;
        }

        ScrollUpdate {
            lines,
            next_tick_in: self.next_tick_in(now),
        }
    }

    /// 現在時刻に基づいてアクティブなストリームが終了したかを確認。
    pub(crate) fn on_tick(&mut self) -> ScrollUpdate {
        self.on_tick_at(Instant::now())
    }

    /// 特定の時刻でアクティブなストリームが終了したかを確認（テスト用）。
    ///
    /// ストリームがまだアクティブと見なされている間、新しいスクロールイベントが到着しなくても
    /// これを呼び出すべき。2つの役割がある:
    ///
    /// - **ストリームクローズ**: ストリームが [`STREAM_GAP`] より長くアイドルだった場合、
    ///   それを閉じて残りの整数行スクロールをフラッシュ。
    /// - **まとめられたフラッシュ**: トラックパッドストリームでは、新しいイベントがなくても
    ///   [`REDRAW_CADENCE`] でフラッシュ。これにより、ストリームが最終的に閉じるときの
    ///   「遅れたジャンプ」（ユーザーはオーバーシュートと解釈）を回避。
    pub(crate) fn on_tick_at(&mut self, now: Instant) -> ScrollUpdate {
        let mut lines = 0;
        if let Some(mut stream) = self.stream.take() {
            let gap = now.duration_since(stream.last);
            if gap > STREAM_GAP {
                lines = self.finalize_stream_at(now, &mut stream);
            } else {
                // 新しいイベントはないが、追加の整数行を適用するのに十分な端数スクロールが
                // 蓄積されている可能性がある。固定周期でのフラッシュは、ストリームが最終的に
                // 閉じるときの「遅れたジャンプ」（ユーザーはオーバーシュートと認識）を防ぐ。
                if now.duration_since(self.last_redraw_at) >= REDRAW_CADENCE {
                    lines = Self::flush_lines_at(
                        &mut self.last_redraw_at,
                        self.carry_lines,
                        now,
                        &mut stream,
                    );
                }
                self.stream = Some(stream);
            }
        }

        ScrollUpdate {
            lines,
            next_tick_in: self.next_tick_in(now),
        }
    }

    /// ストリームを終了し、トラックパッドキャリー状態を更新。
    ///
    /// ストリームが終了したことがわかっている場合（ギャップ/方向反転）に呼び出し元がこれを呼び出す。
    /// 自動モードの最終的なホイール/トラックパッド分類を強制し、整数行デルタをフラッシュし、
    /// 次のストリームがスムーズに継続できるよう、トラックパッドストリームの残りの端数スクロールを保持。
    fn finalize_stream_at(&mut self, now: Instant, stream: &mut ScrollStream) -> i32 {
        stream.finalize_kind();
        let lines = Self::flush_lines_at(&mut self.last_redraw_at, self.carry_lines, now, stream);

        // トラックパッドストリームのストリーム境界をまたいで、行未満の端数スクロールを保持。
        if stream.kind != ScrollStreamKind::Wheel && stream.config.mode != ScrollInputMode::Wheel {
            self.carry_lines =
                stream.desired_lines_f32(self.carry_lines) - stream.applied_lines as f32;
        } else {
            self.carry_lines = 0.0;
        }

        lines
    }

    /// アクティブストリームの新たに到達した整数行デルタを計算して適用。
    ///
    /// ストリームの蓄積されたイベントを*希望する合計行位置*に変換し、
    /// 整数行に切り捨て、このストリームで既に適用されたものとの差分を返す。
    ///
    /// ホイールストリームでは、丸めや誤検出によりホイールノッチが「デッド」にならないよう、
    /// ゼロ以外の入力に対して最小±1行も適用。
    fn flush_lines_at(
        last_redraw_at: &mut Instant,
        carry_lines: f32,
        now: Instant,
        stream: &mut ScrollStream,
    ) -> i32 {
        let desired_total = stream.desired_lines_f32(carry_lines);
        let mut desired_lines = desired_total.trunc() as i32;

        // ホイールモード（またはホイールストリーム）では、ゼロ以外の入力に対して少なくとも1行を保証。
        // これは `events_per_tick` が誤検出またはオーバーライドされたときの「デッド」ホイールティックを回避。
        if stream.is_wheel_like() && desired_lines == 0 && stream.accumulated_events != 0 {
            desired_lines = stream.accumulated_events.signum() * MIN_LINES_PER_WHEEL_STREAM;
        }

        let mut delta = desired_lines - stream.applied_lines;
        if delta == 0 {
            return 0;
        }

        delta = delta.clamp(-MAX_ACCUMULATED_LINES, MAX_ACCUMULATED_LINES);
        stream.applied_lines = stream.applied_lines.saturating_add(delta);
        *last_redraw_at = now;
        delta
    }

    /// 呼び出し元が次に [`MouseScrollState::on_tick_at`] を呼び出すべきタイミングを決定。
    ///
    /// ストリームがアクティブな間、2つの理由でフォローアップティックが必要:
    ///
    /// - **ストリームクローズ**: [`STREAM_GAP`] の間アイドルになったら、ストリームを終了。
    /// - **トラックパッドのまとめ**: 整数行が保留中だがまだ [`REDRAW_CADENCE`] に達していない場合、
    ///   ビューポートが迅速に更新されるよう、より早いティックをスケジュール。
    ///
    /// `None` を返すことは、ストリームがアクティブでない（またはすでにギャップしきい値を過ぎている）ことを意味する。
    fn next_tick_in(&self, now: Instant) -> Option<Duration> {
        let stream = self.stream.as_ref()?;
        let gap = now.duration_since(stream.last);
        if gap > STREAM_GAP {
            return None;
        }

        let mut next = STREAM_GAP.saturating_sub(gap);

        // 少なくとも1整数行を蓄積したがまだフラッシュしていない場合（最後のイベントが
        // 再描画周期が経過する前に到着したため）、迅速にフラッシュできるよう早いティックをスケジュール。
        let desired_lines = stream.desired_lines_f32(self.carry_lines).trunc() as i32;
        if desired_lines != stream.applied_lines {
            let since_redraw = now.duration_since(self.last_redraw_at);
            let until_redraw = if since_redraw >= REDRAW_CADENCE {
                Duration::from_millis(0)
            } else {
                REDRAW_CADENCE.saturating_sub(since_redraw)
            };
            next = next.min(until_redraw);
        }

        Some(next)
    }
}

impl Default for MouseScrollState {
    fn default() -> Self {
        Self::new_at(Instant::now())
    }
}

#[derive(Clone, Debug)]
/// ユーザーが1つのスクロールジェスチャーを実行中に蓄積されるストリームごとの状態。
///
/// 「ストリーム」は [`STREAM_GAP`]（無音）と方向変更で定義される1つの連続したジェスチャーに対応。
/// ストリームは生イベント数を蓄積し、[`ScrollConfig`] を通じて希望する合計行位置に変換。
/// 外側の [`MouseScrollState`] は `desired_total` と `applied_lines` の間のデルタのみを適用し、
/// 呼び出し元がスクロール更新を増分行デルタとして扱えるようにする。
///
/// この型は意図的にこのモジュールの外に公開されていない。パブリックAPIは2つのエントリポイント:
///
/// - 新しいイベント用の [`MouseScrollState::on_scroll_event_at`]。
/// - アイドルギャップクローズとまとめられたフラッシュ用の [`MouseScrollState::on_tick_at`]。
///
/// 完全な根拠とプローブ由来の定数については `codex-rs/tui2/docs/scroll_input_model.md` を参照。
struct ScrollStream {
    start: Instant,
    last: Instant,
    direction: ScrollDirection,
    event_count: usize,
    accumulated_events: i32,
    applied_lines: i32,
    config: ScrollConfig,
    kind: ScrollStreamKind,
    first_tick_completed_at: Option<Instant>,
    just_promoted: bool,
}

impl ScrollStream {
    /// `now` で新しいストリームを開始。
    ///
    /// 初期の `kind` は [`ScrollStreamKind::Unknown`]。自動モードでは、ストリームは
    /// [`ScrollStream::maybe_promote_kind`] がストリームをホイールに昇格させるまで、
    /// （オーバーシュートを避けるため）トラックパッドのように動作し始める。
    fn new(now: Instant, direction: ScrollDirection, config: ScrollConfig) -> Self {
        Self {
            start: now,
            last: now,
            direction,
            event_count: 0,
            accumulated_events: 0,
            applied_lines: 0,
            config,
            kind: ScrollStreamKind::Unknown,
            first_tick_completed_at: None,
            just_promoted: false,
        }
    }

    /// ストリームに1つの生イベントを記録。
    ///
    /// ストリームの最終確認タイムスタンプ、方向、カウンターを更新。カウンターは
    /// ターミナルが極めて高密度なストリームを発行する場合の氾濫と数値オーバーフローを避けるためクランプ。
    fn push_event(&mut self, now: Instant, direction: ScrollDirection) {
        self.last = now;
        self.direction = direction;
        self.event_count = self
            .event_count
            .saturating_add(1)
            .min(MAX_EVENTS_PER_STREAM);
        self.accumulated_events = (self.accumulated_events + direction.sign()).clamp(
            -(MAX_EVENTS_PER_STREAM as i32),
            MAX_EVENTS_PER_STREAM as i32,
        );
    }

    /// 最初のティックが素早く完了した場合、自動モードストリームをホイールに昇格。
    ///
    /// ターミナルはしばしばホイールノッチを `events_per_tick` 個の生イベントの短いバーストにまとめる。
    /// 少なくともその数のイベントを観測し、それらが [`ScrollConfig::wheel_tick_detect_max`] 内に
    /// 到着した場合、ストリームをホイールとして扱い、ノッチが固定の複数行量をスクロールする
    /// （クラシックな感触）。
    ///
    /// これは `events_per_tick >= 2` の場合にのみ試みる。1イベント/ティックターミナルでは
    /// 「ティック完了時間」シグナルがない。自動モードはそれらを [`ScrollStream::finalize_kind`] の
    /// ストリーム終了時フォールバックで処理。
    fn maybe_promote_kind(&mut self, now: Instant) {
        if self.config.mode != ScrollInputMode::Auto {
            return;
        }
        if self.kind != ScrollStreamKind::Unknown {
            return;
        }

        let events_per_tick = self.config.events_per_tick.max(1) as usize;
        if events_per_tick >= 2 && self.event_count >= events_per_tick {
            self.first_tick_completed_at.get_or_insert(now);
            let elapsed = now.duration_since(self.start);
            if elapsed <= self.config.wheel_tick_detect_max {
                self.kind = ScrollStreamKind::Wheel;
                self.just_promoted = true;
            }
        }
    }

    /// ストリームのホイール/トラックパッド分類を確定。
    ///
    /// 強制モード（`wheel`/`trackpad`）では、単にストリームの種類を設定。
    ///
    /// 自動モードでは、ホイールに昇格されなかったストリームはトラックパッドのまま。ただし
    /// 1イベント/ティックターミナル用の小さなストリーム終了時フォールバックを除く。
    /// そのフォールバックは非常に小さく短命なストリームをホイールとして扱い、
    /// WezTerm/iTerm/VS Codeでのホイールが期待される複数行ノッチ動作を得られるようにする。
    fn finalize_kind(&mut self) {
        match self.config.mode {
            ScrollInputMode::Wheel => self.kind = ScrollStreamKind::Wheel,
            ScrollInputMode::Trackpad => self.kind = ScrollStreamKind::Trackpad,
            ScrollInputMode::Auto => {
                if self.kind != ScrollStreamKind::Unknown {
                    return;
                }
                // 高速に完了する最初のティックを確認できなかった場合、ストリームをトラックパッドとして
                // 扱い続ける。唯一の例外は1イベント/ホイールティックを発行するターミナル:
                // そこでは「ティック完了時間」を観測できないため、*非常に小さな*バースト用の
                // 保守的なストリーム終了時フォールバックを使用。
                let duration = self.last.duration_since(self.start);
                if self.config.events_per_tick <= 1
                    && self.event_count <= 2
                    && duration <= self.config.wheel_like_max_duration
                {
                    self.kind = ScrollStreamKind::Wheel;
                } else {
                    self.kind = ScrollStreamKind::Trackpad;
                }
            }
        }
    }

    /// このストリームが現在ホイールのように動作すべきかどうか。
    ///
    /// 自動モードでは、ストリームは昇格された後（または終了時に1イベントフォールバックが
    /// トリガーされた後）にのみホイールライクになる。`kind` がまだ不明な間は、
    /// オーバーシュートを避けるためストリームをトラックパッドとして扱う。
    fn is_wheel_like(&self) -> bool {
        match self.config.mode {
            ScrollInputMode::Wheel => true,
            ScrollInputMode::Trackpad => false,
            ScrollInputMode::Auto => matches!(self.kind, ScrollStreamKind::Wheel),
        }
    }

    /// モードごとのティックあたり行数のスケーリングファクター。
    ///
    /// 自動モードでは、不明なストリームは昇格されるまでトラックパッドファクターを使用。
    fn effective_lines_per_tick_f32(&self) -> f32 {
        match self.config.mode {
            ScrollInputMode::Wheel => self.config.wheel_lines_per_tick_f32(),
            ScrollInputMode::Trackpad => self.config.trackpad_lines_per_tick_f32(),
            ScrollInputMode::Auto => match self.kind {
                ScrollStreamKind::Wheel => self.config.wheel_lines_per_tick_f32(),
                ScrollStreamKind::Trackpad | ScrollStreamKind::Unknown => {
                    self.config.trackpad_lines_per_tick_f32()
                }
            },
        }
    }

    /// このストリームの希望する合計行位置を計算（トラックパッドキャリーを含む）。
    ///
    /// 生イベント数を適切な除数とスケーリングを使用して行単位に変換:
    ///
    /// - ホイールライク: `lines = events * (wheel_lines_per_tick / events_per_tick)`
    /// - トラックパッドライク: `lines = events * (trackpad_lines_per_tick / min(events_per_tick, 3))`
    ///
    /// トラックパッドストリームでは `carry_lines`（前のストリームからの端数余り）も追加し、
    /// 制限付きアクセラレーションを適用。返される値はガードレールとしてクランプ。
    fn desired_lines_f32(&self, carry_lines: f32) -> f32 {
        let events_per_tick = if self.is_wheel_like() {
            self.config.events_per_tick_f32()
        } else {
            self.config.trackpad_events_per_tick_f32()
        };
        let lines_per_tick = self.effective_lines_per_tick_f32();

        // 注: ここでのクランプはガードレール。主な保護は event_count の制限。
        let mut total = (self.accumulated_events as f32 * (lines_per_tick / events_per_tick))
            .clamp(
                -(MAX_ACCUMULATED_LINES as f32),
                MAX_ACCUMULATED_LINES as f32,
            );
        if !self.is_wheel_like() {
            total = (total + carry_lines).clamp(
                -(MAX_ACCUMULATED_LINES as f32),
                MAX_ACCUMULATED_LINES as f32,
            );

            // トラックパッドアクセラレーション: 小さなスワイプは精密に保ちつつ、大きな/速いスワイプを
            // 高速化してより多くのコンテンツをカバーできるようにする。これは意図的にシンプルで制限付き。
            let event_count = self.accumulated_events.abs() as f32;
            let accel = (1.0 + (event_count / self.config.trackpad_accel_events_f32()))
                .clamp(1.0, self.config.trackpad_accel_max_f32());
            total = (total * accel).clamp(
                -(MAX_ACCUMULATED_LINES as f32),
                MAX_ACCUMULATED_LINES as f32,
            );
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn terminal_info_named(name: TerminalName) -> TerminalInfo {
        TerminalInfo {
            name,
            term_program: None,
            version: None,
            term: None,
            multiplexer: None,
        }
    }

    #[test]
    fn terminal_overrides_match_current_defaults() {
        let wezterm = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::WezTerm),
            ScrollConfigOverrides::default(),
        );
        let warp = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::WarpTerminal),
            ScrollConfigOverrides::default(),
        );
        let ghostty = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::Ghostty),
            ScrollConfigOverrides::default(),
        );
        let unknown = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::Unknown),
            ScrollConfigOverrides::default(),
        );

        assert_eq!(wezterm.events_per_tick, 1);
        assert_eq!(wezterm.wheel_lines_per_tick, DEFAULT_WHEEL_LINES_PER_TICK);
        assert_eq!(warp.events_per_tick, 9);
        assert_eq!(ghostty.events_per_tick, 3);
        assert_eq!(unknown.events_per_tick, DEFAULT_EVENTS_PER_TICK);
    }

    #[test]
    fn wheel_tick_scrolls_three_lines_even_when_terminal_emits_three_events() {
        let config = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::AppleTerminal),
            ScrollConfigOverrides {
                events_per_tick: Some(3),
                mode: Some(ScrollInputMode::Auto),
                ..ScrollConfigOverrides::default()
            },
        );
        let base = Instant::now();
        let mut state = MouseScrollState::new_at(base);

        // ティックあたり3つの生イベントを発行するターミナルでの単一ホイールノッチをシミュレート。
        let _ = state.on_scroll_event_at(
            base + Duration::from_millis(1),
            ScrollDirection::Down,
            config,
        );
        let _ = state.on_scroll_event_at(
            base + Duration::from_millis(2),
            ScrollDirection::Down,
            config,
        );
        let update = state.on_scroll_event_at(
            base + Duration::from_millis(3),
            ScrollDirection::Down,
            config,
        );

        assert_eq!(
            update,
            ScrollUpdate {
                lines: 3,
                next_tick_in: Some(Duration::from_millis(STREAM_GAP_MS)),
            }
        );
    }

    #[test]
    fn wheel_tick_scrolls_three_lines_when_terminal_emits_nine_events() {
        let config = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::WarpTerminal),
            ScrollConfigOverrides {
                events_per_tick: Some(9),
                mode: Some(ScrollInputMode::Auto),
                ..ScrollConfigOverrides::default()
            },
        );
        let base = Instant::now();
        let mut state = MouseScrollState::new_at(base);

        let mut update = ScrollUpdate::default();
        for idx in 0..9u64 {
            update = state.on_scroll_event_at(
                base + Duration::from_millis(idx + 1),
                ScrollDirection::Down,
                config,
            );
        }
        assert_eq!(update.lines, 3);
    }

    #[test]
    fn wheel_lines_override_scales_wheel_ticks() {
        let config = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::AppleTerminal),
            ScrollConfigOverrides {
                events_per_tick: Some(3),
                wheel_lines_per_tick: Some(2),
                mode: Some(ScrollInputMode::Wheel),
                ..ScrollConfigOverrides::default()
            },
        );
        let base = Instant::now();
        let mut state = MouseScrollState::new_at(base);

        let first = state.on_scroll_event_at(
            base + Duration::from_millis(1),
            ScrollDirection::Down,
            config,
        );
        let second = state.on_scroll_event_at(
            base + Duration::from_millis(2),
            ScrollDirection::Down,
            config,
        );
        let third = state.on_scroll_event_at(
            base + Duration::from_millis(3),
            ScrollDirection::Down,
            config,
        );

        assert_eq!(first.lines + second.lines + third.lines, 2);
    }

    #[test]
    fn ghostty_trackpad_is_not_penalized_by_wheel_event_density() {
        let config = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::Ghostty),
            ScrollConfigOverrides {
                events_per_tick: Some(9),
                mode: Some(ScrollInputMode::Trackpad),
                ..ScrollConfigOverrides::default()
            },
        );
        let base = Instant::now();
        let mut state = MouseScrollState::new_at(base);

        let _ = state.on_scroll_event_at(
            base + Duration::from_millis(1),
            ScrollDirection::Down,
            config,
        );
        let _ = state.on_scroll_event_at(
            base + Duration::from_millis(2),
            ScrollDirection::Down,
            config,
        );
        let update = state.on_scroll_event_at(
            base + Duration::from_millis(REDRAW_CADENCE_MS + 1),
            ScrollDirection::Down,
            config,
        );

        // トラックパッドモードは正規化に上限付きのイベント/ティックを使用するため、
        // ホイールティックサイズが9でも3イベントで少なくとも1行が生成されるべき。
        assert_eq!(update.lines, 1);
    }

    #[test]
    fn trackpad_acceleration_speeds_up_large_swipes_without_affecting_small_swipes_too_much() {
        let config = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::Ghostty),
            ScrollConfigOverrides {
                events_per_tick: Some(9),
                trackpad_accel_events: Some(30),
                trackpad_accel_max: Some(3),
                mode: Some(ScrollInputMode::Trackpad),
                ..ScrollConfigOverrides::default()
            },
        );
        let base = Instant::now();
        let mut state = MouseScrollState::new_at(base);

        let mut total_lines = 0;
        for idx in 0..60u64 {
            let update = state.on_scroll_event_at(
                base + Duration::from_millis((idx + 1) * (REDRAW_CADENCE_MS + 1)),
                ScrollDirection::Down,
                config,
            );
            total_lines += update.lines;
        }
        total_lines += state
            .on_tick_at(base + Duration::from_millis(60 * (REDRAW_CADENCE_MS + 1)) + STREAM_GAP)
            .lines;

        // アクセラレーションなしで、60イベント（各1/3行）は約20行になる。アクセラレーションありでは
        // 明らかに速くなるべき。
        assert!(total_lines >= 30, "total_lines={total_lines}");
    }

    #[test]
    fn direction_flip_closes_previous_stream() {
        let config = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::AppleTerminal),
            ScrollConfigOverrides {
                events_per_tick: Some(3),
                mode: Some(ScrollInputMode::Auto),
                ..ScrollConfigOverrides::default()
            },
        );
        let base = Instant::now();
        let mut state = MouseScrollState::new_at(base);

        let _ =
            state.on_scroll_event_at(base + Duration::from_millis(1), ScrollDirection::Up, config);
        let _ =
            state.on_scroll_event_at(base + Duration::from_millis(2), ScrollDirection::Up, config);
        let up =
            state.on_scroll_event_at(base + Duration::from_millis(3), ScrollDirection::Up, config);
        let down = state.on_scroll_event_at(
            base + Duration::from_millis(4),
            ScrollDirection::Down,
            config,
        );

        assert_eq!(
            up,
            ScrollUpdate {
                lines: -3,
                next_tick_in: Some(Duration::from_millis(STREAM_GAP_MS)),
            }
        );
        assert_eq!(
            down,
            ScrollUpdate {
                lines: 0,
                next_tick_in: Some(Duration::from_millis(STREAM_GAP_MS)),
            }
        );
    }

    #[test]
    fn continuous_stream_coalesces_redraws() {
        let config = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::AppleTerminal),
            ScrollConfigOverrides {
                events_per_tick: Some(1),
                mode: Some(ScrollInputMode::Trackpad),
                ..ScrollConfigOverrides::default()
            },
        );
        let base = Instant::now();
        let mut state = MouseScrollState::new_at(base);

        let first = state.on_scroll_event_at(
            base + Duration::from_millis(1),
            ScrollDirection::Down,
            config,
        );
        let second = state.on_scroll_event_at(
            base + Duration::from_millis(10),
            ScrollDirection::Down,
            config,
        );
        let third = state.on_scroll_event_at(
            base + Duration::from_millis(20),
            ScrollDirection::Down,
            config,
        );

        assert_eq!(
            first,
            ScrollUpdate {
                lines: 0,
                next_tick_in: Some(Duration::from_millis(REDRAW_CADENCE_MS - 1)),
            }
        );
        assert_eq!(
            second,
            ScrollUpdate {
                lines: 0,
                next_tick_in: Some(Duration::from_millis(REDRAW_CADENCE_MS - 10)),
            }
        );
        assert_eq!(
            third,
            ScrollUpdate {
                lines: 3,
                next_tick_in: Some(Duration::from_millis(STREAM_GAP_MS)),
            }
        );
    }

    #[test]
    fn invert_direction_flips_sign() {
        let config = ScrollConfig::from_terminal(
            &terminal_info_named(TerminalName::AppleTerminal),
            ScrollConfigOverrides {
                events_per_tick: Some(1),
                invert_direction: true,
                ..ScrollConfigOverrides::default()
            },
        );
        let base = Instant::now();
        let mut state = MouseScrollState::new_at(base);

        let update = state.on_scroll_event_at(
            base + Duration::from_millis(REDRAW_CADENCE_MS + 1),
            ScrollDirection::Up,
            config,
        );

        assert_eq!(
            update,
            ScrollUpdate {
                lines: 1,
                next_tick_in: Some(Duration::from_millis(STREAM_GAP_MS)),
            }
        );
    }
}
