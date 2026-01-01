use crate::app_backtrack::BacktrackState;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::ApprovalRequest;
use crate::chatwidget::ChatWidget;
use crate::clipboard_copy;
use crate::custom_terminal::Frame;
use crate::diff_render::DiffSummary;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::file_search::FileSearchManager;
use crate::history_cell::HistoryCell;
use crate::history_cell::UserHistoryCell;
use crate::model_migration::ModelMigrationOutcome;
use crate::model_migration::migration_copy_for_models;
use crate::model_migration::run_model_migration_prompt;
use crate::pager_overlay::Overlay;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::Renderable;
use crate::resume_picker::ResumeSelection;
use crate::transcript_copy_ui::TranscriptCopyUi;
use crate::transcript_multi_click::TranscriptMultiClick;
use crate::transcript_selection::TRANSCRIPT_GUTTER_COLS;
use crate::transcript_selection::TranscriptSelection;
use crate::transcript_selection::TranscriptSelectionPoint;
use crate::tui;
use crate::tui::TuiEvent;
use crate::tui::scrolling::MouseScrollState;
use crate::tui::scrolling::ScrollConfig;
use crate::tui::scrolling::ScrollConfigOverrides;
use crate::tui::scrolling::ScrollDirection;
use crate::tui::scrolling::ScrollUpdate;
use crate::tui::scrolling::TranscriptLineMeta;
use crate::tui::scrolling::TranscriptScroll;
use crate::update_action::UpdateAction;
use codex_ansi_escape::ansi_escape_line;
use codex_core::AuthManager;
use codex_core::ConversationManager;
use codex_core::config::Config;
use codex_core::config::edit::ConfigEditsBuilder;
#[cfg(target_os = "windows")]
use codex_core::features::Feature;
use codex_core::models_manager::manager::ModelsManager;
use codex_core::models_manager::model_presets::HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG;
use codex_core::models_manager::model_presets::HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG;
use codex_core::protocol::EventMsg;
use codex_core::protocol::FinalOutput;
use codex_core::protocol::ListSkillsResponseEvent;
use codex_core::protocol::Op;
use codex_core::protocol::SessionSource;
use codex_core::protocol::SkillErrorInfo;
use codex_core::protocol::TokenUsage;
use codex_core::terminal::terminal_info;
use codex_protocol::ConversationId;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelUpgrade;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::MouseButton;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use tokio::select;
use tokio::sync::mpsc::unbounded_channel;

#[cfg(not(debug_assertions))]
use crate::history_cell::UpdateAvailableHistoryCell;

#[derive(Debug, Clone)]
pub struct AppExitInfo {
    pub token_usage: TokenUsage,
    pub conversation_id: Option<ConversationId>,
    pub update_action: Option<UpdateAction>,
    /// TUI終了後に出力するANSIスタイル付きトランスクリプト行。
    ///
    /// これらの行は最終TUIビューポートと同じ幅でレンダリングされ、
    /// スタイリング（色、太字など）を含むため、スクロールバックで
    /// 画面上のトランスクリプトの視覚的構造が保持される。
    pub session_lines: Vec<String>,
}

impl From<AppExitInfo> for codex_tui::AppExitInfo {
    fn from(info: AppExitInfo) -> Self {
        codex_tui::AppExitInfo {
            token_usage: info.token_usage,
            conversation_id: info.conversation_id,
            update_action: info.update_action.map(Into::into),
        }
    }
}

fn session_summary(
    token_usage: TokenUsage,
    conversation_id: Option<ConversationId>,
) -> Option<SessionSummary> {
    if token_usage.is_zero() {
        return None;
    }

    let usage_line = FinalOutput::from(token_usage).to_string();
    let resume_command =
        conversation_id.map(|conversation_id| format!("codex resume {conversation_id}"));
    Some(SessionSummary {
        usage_line,
        resume_command,
    })
}

fn errors_for_cwd(cwd: &Path, response: &ListSkillsResponseEvent) -> Vec<SkillErrorInfo> {
    response
        .skills
        .iter()
        .find(|entry| entry.cwd.as_path() == cwd)
        .map(|entry| entry.errors.clone())
        .unwrap_or_default()
}

fn emit_skill_load_warnings(app_event_tx: &AppEventSender, errors: &[SkillErrorInfo]) {
    if errors.is_empty() {
        return;
    }

    let error_count = errors.len();
    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
        crate::history_cell::new_warning_event(format!(
            "Skipped loading {error_count} skill(s) due to invalid SKILL.md files."
        )),
    )));

    for error in errors {
        let path = error.path.display();
        let message = error.message.as_str();
        app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            crate::history_cell::new_warning_event(format!("{path}: {message}")),
        )));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSummary {
    usage_line: String,
    resume_command: Option<String>,
}

fn should_show_model_migration_prompt(
    current_model: &str,
    target_model: &str,
    seen_migrations: &BTreeMap<String, String>,
    available_models: &[ModelPreset],
) -> bool {
    if target_model == current_model {
        return false;
    }

    if let Some(seen_target) = seen_migrations.get(current_model)
        && seen_target == target_model
    {
        return false;
    }

    if available_models
        .iter()
        .any(|preset| preset.model == current_model && preset.upgrade.is_some())
    {
        return true;
    }

    if available_models
        .iter()
        .any(|preset| preset.upgrade.as_ref().map(|u| u.id.as_str()) == Some(target_model))
    {
        return true;
    }

    false
}

fn migration_prompt_hidden(config: &Config, migration_config_key: &str) -> bool {
    match migration_config_key {
        HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG => config
            .notices
            .hide_gpt_5_1_codex_max_migration_prompt
            .unwrap_or(false),
        HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG => {
            config.notices.hide_gpt5_1_migration_prompt.unwrap_or(false)
        }
        _ => false,
    }
}

async fn handle_model_migration_prompt_if_needed(
    tui: &mut tui::Tui,
    config: &mut Config,
    model: &str,
    app_event_tx: &AppEventSender,
    models_manager: Arc<ModelsManager>,
) -> Option<AppExitInfo> {
    let available_models = models_manager.list_models(config).await;
    let upgrade = available_models
        .iter()
        .find(|preset| preset.model == model)
        .and_then(|preset| preset.upgrade.as_ref());

    if let Some(ModelUpgrade {
        id: target_model,
        reasoning_effort_mapping,
        migration_config_key,
        ..
    }) = upgrade
    {
        if migration_prompt_hidden(config, migration_config_key.as_str()) {
            return None;
        }

        let target_model = target_model.to_string();
        if !should_show_model_migration_prompt(
            model,
            &target_model,
            &config.notices.model_migrations,
            &available_models,
        ) {
            return None;
        }

        let current_preset = available_models.iter().find(|preset| preset.model == model);
        let target_preset = available_models
            .iter()
            .find(|preset| preset.model == target_model);
        let target_display_name = target_preset
            .map(|preset| preset.display_name.clone())
            .unwrap_or_else(|| target_model.clone());
        let heading_label = if target_display_name == model {
            target_model.clone()
        } else {
            target_display_name.clone()
        };
        let target_description = target_preset.and_then(|preset| {
            if preset.description.is_empty() {
                None
            } else {
                Some(preset.description.clone())
            }
        });
        let can_opt_out = current_preset.is_some();
        let prompt_copy = migration_copy_for_models(
            model,
            &target_model,
            heading_label,
            target_description,
            can_opt_out,
        );
        match run_model_migration_prompt(tui, prompt_copy).await {
            ModelMigrationOutcome::Accepted => {
                app_event_tx.send(AppEvent::PersistModelMigrationPromptAcknowledged {
                    from_model: model.to_string(),
                    to_model: target_model.clone(),
                });
                config.model = Some(target_model.clone());

                let mapped_effort = if let Some(reasoning_effort_mapping) = reasoning_effort_mapping
                    && let Some(reasoning_effort) = config.model_reasoning_effort
                {
                    reasoning_effort_mapping
                        .get(&reasoning_effort)
                        .cloned()
                        .or(config.model_reasoning_effort)
                } else {
                    config.model_reasoning_effort
                };

                config.model_reasoning_effort = mapped_effort;

                app_event_tx.send(AppEvent::UpdateModel(target_model.clone()));
                app_event_tx.send(AppEvent::UpdateReasoningEffort(mapped_effort));
                app_event_tx.send(AppEvent::PersistModelSelection {
                    model: target_model.clone(),
                    effort: mapped_effort,
                });
            }
            ModelMigrationOutcome::Rejected => {
                app_event_tx.send(AppEvent::PersistModelMigrationPromptAcknowledged {
                    from_model: model.to_string(),
                    to_model: target_model.clone(),
                });
            }
            ModelMigrationOutcome::Exit => {
                return Some(AppExitInfo {
                    token_usage: TokenUsage::default(),
                    conversation_id: None,
                    update_action: None,
                    session_lines: Vec::new(),
                });
            }
        }
    }

    None
}

pub(crate) struct App {
    pub(crate) server: Arc<ConversationManager>,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) chat_widget: ChatWidget,
    pub(crate) auth_manager: Arc<AuthManager>,
    /// 必要に応じてChatWidgetを再作成できるようConfigをここに保存。
    pub(crate) config: Config,
    pub(crate) current_model: String,
    pub(crate) active_profile: Option<String>,

    pub(crate) file_search: FileSearchManager,

    pub(crate) transcript_cells: Vec<Arc<dyn HistoryCell>>,

    #[allow(dead_code)]
    transcript_scroll: TranscriptScroll,
    transcript_selection: TranscriptSelection,
    transcript_multi_click: TranscriptMultiClick,
    transcript_view_top: usize,
    transcript_total_lines: usize,
    transcript_copy_ui: TranscriptCopyUi,

    // ページャーオーバーレイ状態（トランスクリプトまたはDiffなどの静的）
    pub(crate) overlay: Option<Overlay>,
    pub(crate) deferred_history_lines: Vec<Line<'static>>,
    has_emitted_history_lines: bool,

    pub(crate) enhanced_keys_supported: bool,

    /// CommitTickイベントを送信するアニメーションスレッドを制御。
    pub(crate) commit_anim_running: Arc<AtomicBool>,

    scroll_config: ScrollConfig,
    scroll_state: MouseScrollState,

    // Escバックトラック状態をグループ化
    pub(crate) backtrack: crate::app_backtrack::BacktrackState,
    pub(crate) feedback: codex_feedback::CodexFeedback,
    /// ユーザーが更新を確認した際に設定され、終了時に伝播される。
    pub(crate) pending_update_action: Option<UpdateAction>,

    /// 意図的に会話を停止する際（例：新しい会話を開始する前）に
    /// 次のShutdownCompleteイベントを無視する。
    suppress_shutdown_complete: bool,

    // ユーザー確認後の次のworld-writableスキャンを一度だけ抑制。
    skip_world_writable_scan_once: bool,
}
impl App {
    async fn shutdown_current_conversation(&mut self) {
        if let Some(conversation_id) = self.chat_widget.conversation_id() {
            self.suppress_shutdown_complete = true;
            self.chat_widget.submit_op(Op::Shutdown);
            self.server.remove_conversation(&conversation_id).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        auth_manager: Arc<AuthManager>,
        mut config: Config,
        active_profile: Option<String>,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        resume_selection: ResumeSelection,
        feedback: codex_feedback::CodexFeedback,
        is_first_run: bool,
    ) -> Result<AppExitInfo> {
        use tokio_stream::StreamExt;
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);

        let conversation_manager = Arc::new(ConversationManager::new(
            auth_manager.clone(),
            SessionSource::Cli,
        ));
        let mut model = conversation_manager
            .get_models_manager()
            .get_model(&config.model, &config)
            .await;
        let exit_info = handle_model_migration_prompt_if_needed(
            tui,
            &mut config,
            model.as_str(),
            &app_event_tx,
            conversation_manager.get_models_manager(),
        )
        .await;
        if let Some(exit_info) = exit_info {
            return Ok(exit_info);
        }
        if let Some(updated_model) = config.model.clone() {
            model = updated_model;
        }

        let enhanced_keys_supported = tui.enhanced_keys_supported();
        let model_family = conversation_manager
            .get_models_manager()
            .construct_model_family(model.as_str(), &config)
            .await;
        let mut chat_widget = match resume_selection {
            ResumeSelection::StartFresh | ResumeSelection::Exit => {
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_prompt: initial_prompt.clone(),
                    initial_images: initial_images.clone(),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: conversation_manager.get_models_manager(),
                    feedback: feedback.clone(),
                    is_first_run,
                    model_family: model_family.clone(),
                };
                ChatWidget::new(init, conversation_manager.clone())
            }
            ResumeSelection::Resume(path) => {
                let resumed = conversation_manager
                    .resume_conversation_from_rollout(
                        config.clone(),
                        path.clone(),
                        auth_manager.clone(),
                    )
                    .await
                    .wrap_err_with(|| {
                        format!("Failed to resume session from {}", path.display())
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_prompt: initial_prompt.clone(),
                    initial_images: initial_images.clone(),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: conversation_manager.get_models_manager(),
                    feedback: feedback.clone(),
                    is_first_run,
                    model_family: model_family.clone(),
                };
                ChatWidget::new_from_existing(
                    init,
                    resumed.conversation,
                    resumed.session_configured,
                )
            }
        };

        chat_widget.maybe_prompt_windows_sandbox_enable();

        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        #[cfg(not(debug_assertions))]
        let upgrade_version = crate::updates::get_upgrade_version(&config);
        let scroll_config = ScrollConfig::from_terminal(
            &terminal_info(),
            ScrollConfigOverrides {
                events_per_tick: config.tui_scroll_events_per_tick,
                wheel_lines_per_tick: config.tui_scroll_wheel_lines,
                trackpad_lines_per_tick: config.tui_scroll_trackpad_lines,
                trackpad_accel_events: config.tui_scroll_trackpad_accel_events,
                trackpad_accel_max: config.tui_scroll_trackpad_accel_max,
                mode: Some(config.tui_scroll_mode),
                wheel_tick_detect_max_ms: config.tui_scroll_wheel_tick_detect_max_ms,
                wheel_like_max_duration_ms: config.tui_scroll_wheel_like_max_duration_ms,
                invert_direction: config.tui_scroll_invert,
            },
        );

        let copy_selection_shortcut = crate::transcript_copy_ui::detect_copy_selection_shortcut();

        let mut app = Self {
            server: conversation_manager.clone(),
            app_event_tx,
            chat_widget,
            auth_manager: auth_manager.clone(),
            config,
            current_model: model.clone(),
            active_profile,
            file_search,
            enhanced_keys_supported,
            transcript_cells: Vec::new(),
            transcript_scroll: TranscriptScroll::default(),
            transcript_selection: TranscriptSelection::default(),
            transcript_multi_click: TranscriptMultiClick::default(),
            transcript_view_top: 0,
            transcript_total_lines: 0,
            transcript_copy_ui: TranscriptCopyUi::new_with_shortcut(copy_selection_shortcut),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            scroll_config,
            scroll_state: MouseScrollState::default(),
            backtrack: BacktrackState::default(),
            feedback: feedback.clone(),
            pending_update_action: None,
            suppress_shutdown_complete: false,
            skip_world_writable_scan_once: false,
        };

        // 起動時、Agentモード（workspace-write）またはReadOnlyがアクティブな場合、Windowsでworld-writableディレクトリについて警告。
        #[cfg(target_os = "windows")]
        {
            let should_check = codex_core::get_platform_sandbox().is_some()
                && matches!(
                    app.config.sandbox_policy.get(),
                    codex_core::protocol::SandboxPolicy::WorkspaceWrite { .. }
                        | codex_core::protocol::SandboxPolicy::ReadOnly
                )
                && !app
                    .config
                    .notices
                    .hide_world_writable_warning
                    .unwrap_or(false);
            if should_check {
                let cwd = app.config.cwd.clone();
                let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
                let tx = app.app_event_tx.clone();
                let logs_base_dir = app.config.codex_home.clone();
                let sandbox_policy = app.config.sandbox_policy.get().clone();
                Self::spawn_world_writable_scan(cwd, env_map, logs_base_dir, sandbox_policy, tx);
            }
        }

        #[cfg(not(debug_assertions))]
        if let Some(latest_version) = upgrade_version {
            app.handle_event(
                tui,
                AppEvent::InsertHistoryCell(Box::new(UpdateAvailableHistoryCell::new(
                    latest_version,
                    crate::update_action::get_update_action(),
                ))),
            )
            .await?;
        }

        let tui_events = tui.event_stream();
        tokio::pin!(tui_events);

        tui.frame_requester().schedule_frame();

        while select! {
            Some(event) = app_event_rx.recv() => {
                app.handle_event(tui, event).await?
            }
            Some(event) = tui_events.next() => {
                app.handle_tui_event(tui, event).await?
            }
        } {}
        let width = tui.terminal.last_known_screen_size.width;
        let session_lines = if width == 0 {
            Vec::new()
        } else {
            let transcript =
                crate::transcript_render::build_transcript_lines(&app.transcript_cells, width);
            let (lines, line_meta) = (transcript.lines, transcript.meta);
            let is_user_cell: Vec<bool> = app
                .transcript_cells
                .iter()
                .map(|cell| cell.as_any().is::<UserHistoryCell>())
                .collect();
            crate::transcript_render::render_lines_to_ansi(&lines, &line_meta, &is_user_cell, width)
        };

        tui.terminal.clear()?;
        Ok(AppExitInfo {
            token_usage: app.token_usage(),
            conversation_id: app.chat_widget.conversation_id(),
            update_action: app.pending_update_action,
            session_lines,
        })
    }

    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        event: TuiEvent,
    ) -> Result<bool> {
        if matches!(&event, TuiEvent::Draw) {
            self.handle_scroll_tick(tui);
        }

        if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, key_event).await;
                }
                TuiEvent::Mouse(mouse_event) => {
                    self.handle_mouse_event(tui, mouse_event);
                }
                TuiEvent::Paste(pasted) => {
                    // 多くのターミナルはペースト時に改行を\rに変換する（例: iTerm2）が、
                    // tui-textareaは\nを期待する。CRをLFに正規化。
                    // [tui-textarea]: https://github.com/rhysd/tui-textarea/blob/4d18622eeac13b309e0ff6a55a46ac6706da68cf/src/textarea.rs#L782-L783
                    // [iTerm2]: https://github.com/gnachman/iTerm2/blob/5d0c0d9f68523cbd0494dad5422998964a2ecd8d/sources/iTermPasteHelper.m#L206-L216
                    let pasted = pasted.replace("\r", "\n");
                    self.chat_widget.handle_paste(pasted);
                }
                TuiEvent::Draw => {
                    self.chat_widget.maybe_post_pending_notification(tui);
                    if self
                        .chat_widget
                        .handle_paste_burst_tick(tui.frame_requester())
                    {
                        return Ok(true);
                    }
                    let cells = self.transcript_cells.clone();
                    tui.draw(tui.terminal.size()?.height, |frame| {
                        let chat_height = self.chat_widget.desired_height(frame.area().width);
                        let chat_top = self.render_transcript_cells(frame, &cells, chat_height);
                        let chat_area = Rect {
                            x: frame.area().x,
                            y: chat_top,
                            width: frame.area().width,
                            height: chat_height.min(
                                frame
                                    .area()
                                    .height
                                    .saturating_sub(chat_top.saturating_sub(frame.area().y)),
                            ),
                        };
                        self.chat_widget.render(chat_area, frame.buffer);
                        let chat_bottom = chat_area.y.saturating_add(chat_area.height);
                        if chat_bottom < frame.area().bottom() {
                            Clear.render_ref(
                                Rect {
                                    x: frame.area().x,
                                    y: chat_bottom,
                                    width: frame.area().width,
                                    height: frame.area().bottom().saturating_sub(chat_bottom),
                                },
                                frame.buffer,
                            );
                        }
                        if let Some((x, y)) = self.chat_widget.cursor_pos(chat_area) {
                            frame.set_cursor_position((x, y));
                        }
                    })?;
                    let transcript_scrolled =
                        !matches!(self.transcript_scroll, TranscriptScroll::ToBottom);
                    let selection_active = matches!(
                        (self.transcript_selection.anchor, self.transcript_selection.head),
                        (Some(a), Some(b)) if a != b
                    );
                    let scroll_position = if self.transcript_total_lines == 0 {
                        None
                    } else {
                        Some((
                            self.transcript_view_top.saturating_add(1),
                            self.transcript_total_lines,
                        ))
                    };
                    self.chat_widget.set_transcript_ui_state(
                        transcript_scrolled,
                        selection_active,
                        scroll_position,
                        self.copy_selection_key(),
                    );
                }
            }
        }
        Ok(true)
    }

    pub(crate) fn render_transcript_cells(
        &mut self,
        frame: &mut Frame,
        cells: &[Arc<dyn HistoryCell>],
        chat_height: u16,
    ) -> u16 {
        let area = frame.area();
        if area.width == 0 || area.height == 0 {
            self.transcript_scroll = TranscriptScroll::default();
            self.transcript_view_top = 0;
            self.transcript_total_lines = 0;
            return area.bottom().saturating_sub(chat_height);
        }

        let chat_height = chat_height.min(area.height);
        let max_transcript_height = area.height.saturating_sub(chat_height);
        if max_transcript_height == 0 {
            self.transcript_scroll = TranscriptScroll::default();
            self.transcript_view_top = 0;
            self.transcript_total_lines = 0;
            return area.y;
        }

        let transcript_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: max_transcript_height,
        };

        let transcript =
            crate::transcript_render::build_wrapped_transcript_lines(cells, transcript_area.width);
        let (lines, line_meta) = (transcript.lines, transcript.meta);
        if lines.is_empty() {
            Clear.render_ref(transcript_area, frame.buffer);
            self.transcript_scroll = TranscriptScroll::default();
            self.transcript_view_top = 0;
            self.transcript_total_lines = 0;
            return area.y;
        }

        let is_user_cell: Vec<bool> = cells
            .iter()
            .map(|c| c.as_any().is::<UserHistoryCell>())
            .collect();

        let total_lines = lines.len();
        self.transcript_total_lines = total_lines;
        let max_visible = std::cmp::min(max_transcript_height as usize, total_lines);
        let max_start = total_lines.saturating_sub(max_visible);

        let (scroll_state, top_offset) = self.transcript_scroll.resolve_top(&line_meta, max_start);
        self.transcript_scroll = scroll_state;
        self.transcript_view_top = top_offset;

        let transcript_visible_height = max_visible as u16;
        let chat_top = if total_lines <= max_transcript_height as usize {
            let gap = if transcript_visible_height == 0 { 0 } else { 1 };
            area.y
                .saturating_add(transcript_visible_height)
                .saturating_add(gap)
        } else {
            area.bottom().saturating_sub(chat_height)
        };

        let clear_height = chat_top.saturating_sub(area.y);
        if clear_height > 0 {
            Clear.render_ref(
                Rect {
                    x: area.x,
                    y: area.y,
                    width: area.width,
                    height: clear_height,
                },
                frame.buffer,
            );
        }

        let transcript_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: transcript_visible_height,
        };

        for (row_index, line_index) in (top_offset..total_lines).enumerate() {
            if row_index >= max_visible {
                break;
            }

            let y = transcript_area.y + row_index as u16;
            let row_area = Rect {
                x: transcript_area.x,
                y,
                width: transcript_area.width,
                height: 1,
            };

            let is_user_row = line_meta
                .get(line_index)
                .and_then(TranscriptLineMeta::cell_index)
                .map(|cell_index| is_user_cell.get(cell_index).copied().unwrap_or(false))
                .unwrap_or(false);
            if is_user_row {
                let base_style = crate::style::user_message_style();
                for x in row_area.x..row_area.right() {
                    let cell = &mut frame.buffer[(x, y)];
                    let style = cell.style().patch(base_style);
                    cell.set_style(style);
                }
            }

            lines[line_index].render_ref(row_area, frame.buffer);
        }

        self.apply_transcript_selection(transcript_area, frame.buffer);
        if let (Some(anchor), Some(head)) = (
            self.transcript_selection.anchor,
            self.transcript_selection.head,
        ) && anchor != head
        {
            self.transcript_copy_ui.render_copy_pill(
                transcript_area,
                frame.buffer,
                (anchor.line_index, anchor.column),
                (head.line_index, head.column),
                self.transcript_view_top,
                self.transcript_total_lines,
            );
        } else {
            self.transcript_copy_ui.clear_affordance();
        }
        chat_top
    }

    /// メイントランスクリプトビューでのマウスインタラクションを処理。
    ///
    /// - マウスホイール移動はストリームベースの正規化（イベント毎行係数、
    ///   離散vs連続ストリーム）を使用して会話履歴をスクロールし、
    ///   ターミナル自体のスクロールバックとは独立。
    /// - マウスドラッグはフラット化されたトランスクリプト行と列で定義された
    ///   テキスト選択を調整し、選択は絶対画面行ではなく基礎コンテンツに固定。
    /// - ユーザーがビューが最下部を追従中かつタスクがアクティブに実行中
    ///   （例：レスポンスをストリーミング中）に選択を拡張するためドラッグすると、
    ///   スクロールモードはまずアンカー位置に変換され、進行中の更新が
    ///   選択下のビューポートを移動しなくなる。ドラッグなしの単純クリックは
    ///   スクロール動作を変更しない。
    /// - トランスクリプト領域外のマウスイベント（例：コンポーザー/フッター上）は
    ///   トランスクリプト選択状態を開始または変更してはならない。
    ///   トランスクリプト外での左クリックは既存の選択をクリアし、
    ///   ユーザーがハイライトを解除できるようにする。
    fn handle_mouse_event(
        &mut self,
        tui: &mut tui::Tui,
        mouse_event: crossterm::event::MouseEvent,
    ) {
        use crossterm::event::MouseEventKind;

        if self.overlay.is_some() {
            return;
        }

        let size = tui.terminal.last_known_screen_size;
        let width = size.width;
        let height = size.height;
        if width == 0 || height == 0 {
            return;
        }

        let chat_height = self.chat_widget.desired_height(width);
        if chat_height >= height {
            return;
        }

        // コンポーザー上部のトランスクリプト領域上のイベントのみ処理。
        let transcript_height = height.saturating_sub(chat_height);
        if transcript_height == 0 {
            return;
        }

        let transcript_area = Rect {
            x: 0,
            y: 0,
            width,
            height: transcript_height,
        };
        let base_x = transcript_area.x.saturating_add(TRANSCRIPT_GUTTER_COLS);
        let max_x = transcript_area.right().saturating_sub(1);

        // トランスクリプト選択において、トランスクリプトを唯一のインタラクティブ領域として扱う。
        //
        // これによりコンポーザー/フッターでのクリックがトランスクリプト選択を開始または
        // 拡張することを防ぎつつ、トランスクリプト外での左クリックで既存のハイライトを
        // クリアすることは引き続き許可する。
        if mouse_event.row < transcript_area.y || mouse_event.row >= transcript_area.bottom() {
            if matches!(
                mouse_event.kind,
                MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            ) && (self.transcript_selection.anchor.is_some()
                || self.transcript_selection.head.is_some())
            {
                self.transcript_selection = TranscriptSelection::default();
                // マウスイベントは本質的に再描画をトリガーしない；クリアされた
                // ハイライトが即座に反映されるよう再描画をスケジュール。
                tui.frame_requester().schedule_frame();
            }
            return;
        }

        let mut clamped_x = mouse_event.column;
        let clamped_y = mouse_event.row;
        if clamped_x < base_x {
            clamped_x = base_x;
        }
        if clamped_x > max_x {
            clamped_x = max_x;
        }

        let streaming = self.chat_widget.is_task_running();

        if matches!(mouse_event.kind, MouseEventKind::Down(MouseButton::Left))
            && self
                .transcript_copy_ui
                .hit_test(mouse_event.column, mouse_event.row)
        {
            self.copy_transcript_selection(tui);
            return;
        }

        match mouse_event.kind {
            MouseEventKind::ScrollUp => {
                let scroll_update = self.mouse_scroll_update(ScrollDirection::Up);
                self.apply_scroll_update(
                    tui,
                    scroll_update,
                    transcript_area.height as usize,
                    transcript_area.width,
                    true,
                );
            }
            MouseEventKind::ScrollDown => {
                let scroll_update = self.mouse_scroll_update(ScrollDirection::Down);
                self.apply_scroll_update(
                    tui,
                    scroll_update,
                    transcript_area.height as usize,
                    transcript_area.width,
                    true,
                );
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {}
            MouseEventKind::Down(MouseButton::Left) => {
                self.transcript_copy_ui.set_dragging(true);
                let point = self.transcript_point_from_coordinates(
                    transcript_area,
                    base_x,
                    clamped_x,
                    clamped_y,
                );
                if self.transcript_multi_click.on_mouse_down(
                    &mut self.transcript_selection,
                    &self.transcript_cells,
                    transcript_area.width,
                    point,
                ) {
                    tui.frame_requester().schedule_frame();
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let point = self.transcript_point_from_coordinates(
                    transcript_area,
                    base_x,
                    clamped_x,
                    clamped_y,
                );
                let outcome = crate::transcript_selection::on_mouse_drag(
                    &mut self.transcript_selection,
                    &self.transcript_scroll,
                    point,
                    streaming,
                );
                self.transcript_multi_click
                    .on_mouse_drag(&self.transcript_selection, point);
                if outcome.lock_scroll {
                    self.lock_transcript_scroll_to_current_view(
                        transcript_area.height as usize,
                        transcript_area.width,
                    );
                }
                if outcome.changed {
                    tui.frame_requester().schedule_frame();
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.transcript_copy_ui.set_dragging(false);
                let selection_changed =
                    crate::transcript_selection::on_mouse_up(&mut self.transcript_selection);
                let has_active_selection = self.transcript_selection.anchor.is_some()
                    && self.transcript_selection.head.is_some();
                if selection_changed || has_active_selection {
                    tui.frame_requester().schedule_frame();
                }
            }
            _ => {}
        }
    }

    /// 単一のマウススクロールイベント（方向のみ）を正規化されたスクロール更新に変換。
    ///
    /// 現在の[`ScrollConfig`]を使用して[`MouseScrollState::on_scroll_event`]に委譲。
    /// 返される[`ScrollUpdate`]は意図的に以下に分割される:
    ///
    /// - `lines`: トランスクリプトビューポートに即座に適用する視覚的行の*デルタ*。
    ///   - 符号規約は[`ScrollDirection`]に一致（`Up`は負、`Down`は正）。
    ///   - トラックパッド風モードでサブ行の端数がまだ蓄積中の場合は0になりうる。
    /// - `next_tick_in`: フォローアップティックをトリガーすべきオプションの遅延。
    ///   ストリームクローズは明示的な「ジェスチャー終了」イベントではなく*時間ギャップ*で
    ///   定義されるため必要。[`App::apply_scroll_update`]と
    ///   [`App::handle_scroll_tick`]を参照。
    ///
    /// TUI2では、そのフォローアップティックは`TuiEvent::Draw`経由で駆動される: フレームを
    /// スケジュールし、次の描画で[`MouseScrollState::on_tick`]を呼び出してアイドルストリームを
    /// クローズし、新たに到達した整数行をフラッシュする。これにより蓄積されたスクロールが
    /// 次のユーザー入力到着時にのみ適用される「停止ラグ」の知覚を防ぐ。
    fn mouse_scroll_update(&mut self, direction: ScrollDirection) -> ScrollUpdate {
        self.scroll_state
            .on_scroll_event(direction, self.scroll_config)
    }

    /// [`ScrollUpdate`]をトランスクリプトビューポートに適用し、必要なフォローアップティックをスケジュール。
    ///
    /// `update.lines`は[`App::scroll_transcript`]経由で即座に適用される。
    ///
    /// `update.next_tick_in`が`Some`の場合、`TuiEvent::Draw`が[`App::handle_scroll_tick`]を
    /// 呼び出してアイドル後にストリームをクローズおよび/または保留中の整数行をケイデンス
    /// フラッシュできるよう、将来のフレームをスケジュール。
    ///
    /// `schedule_frame`は[`App::scroll_transcript`]に転送され、スクロールが追加の描画を
    /// 要求すべきかを制御。`TuiEvent::Draw`ティック中にスクロールを適用する際は
    /// 冗長なフレームを避けるため`false`を渡す。
    fn apply_scroll_update(
        &mut self,
        tui: &mut tui::Tui,
        update: ScrollUpdate,
        visible_lines: usize,
        width: u16,
        schedule_frame: bool,
    ) {
        if update.lines != 0 {
            self.scroll_transcript(tui, update.lines, visible_lines, width, schedule_frame);
        }
        if let Some(delay) = update.next_tick_in {
            tui.frame_requester().schedule_frame_in(delay);
        }
    }

    /// マウススクロールのストリームクローズとケイデンスベースのフラッシュを駆動。
    ///
    /// レンダリング前の毎`TuiEvent::Draw`で呼び出される。スクロールストリームがアクティブな場合:
    ///
    /// - ストリームギャップ閾値より長くアイドルになったらストリームをクローズ。
    /// - トラックパッド風ストリームでは、新しいイベントが到着しなくても
    ///   再描画ケイデンスで整数行デルタをフラッシュ。
    ///
    /// 既に描画ティック中のため、結果の更新は`schedule_frame = false`で適用される。
    fn handle_scroll_tick(&mut self, tui: &mut tui::Tui) {
        let Some((visible_lines, width)) = self.transcript_scroll_dimensions(tui) else {
            return;
        };
        let update = self.scroll_state.on_tick();
        self.apply_scroll_update(tui, update, visible_lines, width, false);
    }

    /// スクロールに使用するトランスクリプトビューポートの寸法を計算。
    ///
    /// マウススクロールは「表示可能なトランスクリプト行」（ターミナル高さから
    /// チャットコンポーザーの高さを引いたもの）の観点で適用される。
    /// 非描画イベント中にターミナルをクエリすることを避けるため、
    /// 最後に知られたターミナルサイズから計算。
    ///
    /// ターミナルがまだサイズ設定されていないか、チャット領域が全高さを
    /// 消費している場合は`None`、それ以外は`(visible_lines, width)`を返す。
    fn transcript_scroll_dimensions(&self, tui: &tui::Tui) -> Option<(usize, u16)> {
        let size = tui.terminal.last_known_screen_size;
        let width = size.width;
        let height = size.height;
        if width == 0 || height == 0 {
            return None;
        }

        let chat_height = self.chat_widget.desired_height(width);
        if chat_height >= height {
            return None;
        }

        let transcript_height = height.saturating_sub(chat_height);
        if transcript_height == 0 {
            return None;
        }

        Some((transcript_height as usize, width))
    }

    /// トランスクリプトを指定された視覚的行数だけスクロール。
    ///
    /// これはメインビューでのマウスホイール移動とPgUp/PgDnキーの背後にある
    /// 共有実装。スクロール状態はトランスクリプトセルとその内部行インデックスの
    /// 観点で表現されるため、スクロールは論理的な会話コンテンツを参照し、
    /// 折り返しやストリーミングが視覚的リフローを引き起こしても安定を保つ。
    ///
    /// `schedule_frame`は追加の描画を要求するかを制御；`TuiEvent::Draw`ティック中に
    /// スクロールを適用する際は冗長なフレームを避けるため`false`を渡す。
    fn scroll_transcript(
        &mut self,
        tui: &mut tui::Tui,
        delta_lines: i32,
        visible_lines: usize,
        width: u16,
        schedule_frame: bool,
    ) {
        if visible_lines == 0 {
            return;
        }

        let transcript =
            crate::transcript_render::build_wrapped_transcript_lines(&self.transcript_cells, width);
        let line_meta = transcript.meta;
        self.transcript_scroll =
            self.transcript_scroll
                .scrolled_by(delta_lines, &line_meta, visible_lines);

        if schedule_frame {
            // 再描画を要求；フレームスケジューラはバーストを統合し60fpsに制限。
            tui.frame_requester().schedule_frame();
        }
    }

    /// `ToBottom`（自動追従）スクロール状態を現在のビューでの固定アンカーに変換。
    ///
    /// 新しい出力がストリーミング中にユーザーがマウス選択を開始した場合、
    /// ビューは最新行の自動追従を停止し、選択が意図したコンテンツに留まるべき。
    /// このヘルパーは指定された幅でフラット化されたトランスクリプトを検査し、
    /// 現在の上端行に対応する具体的な位置を導出し、ユーザーが再度スクロールするまで
    /// その位置を安定に保つスクロールモードに切り替える。
    fn lock_transcript_scroll_to_current_view(&mut self, visible_lines: usize, width: u16) {
        if self.transcript_cells.is_empty() || visible_lines == 0 || width == 0 {
            return;
        }

        let transcript =
            crate::transcript_render::build_wrapped_transcript_lines(&self.transcript_cells, width);
        let (lines, line_meta) = (transcript.lines, transcript.meta);
        if lines.is_empty() || line_meta.is_empty() {
            return;
        }

        let total_lines = lines.len();
        let max_visible = std::cmp::min(visible_lines, total_lines);
        if max_visible == 0 {
            return;
        }

        let max_start = total_lines.saturating_sub(max_visible);
        let top_offset = match self.transcript_scroll {
            TranscriptScroll::ToBottom => max_start,
            TranscriptScroll::Scrolled { .. } => {
                // 既にアンカー済み；ロック不要。
                return;
            }
        };

        if let Some(scroll_state) = TranscriptScroll::anchor_for(&line_meta, top_offset) {
            self.transcript_scroll = scroll_state;
        }
    }

    /// 現在のトランスクリプト選択を指定されたバッファに適用。
    ///
    /// 選択はフラット化され折り返されたトランスクリプト行インデックスと列の観点で
    /// 定義される。このメソッドはこれらのコンテンツ相対エンドポイントを
    /// `transcript_view_top`と`transcript_total_lines`に基づいて現在表示中の
    /// ビューポートにマッピングし、ユーザーがスクロールしてもハイライトが
    /// コンテンツと共に移動するようにする。
    fn apply_transcript_selection(&self, area: Rect, buf: &mut Buffer) {
        let (anchor, head) = match (
            self.transcript_selection.anchor,
            self.transcript_selection.head,
        ) {
            (Some(a), Some(h)) => (a, h),
            _ => return,
        };

        if self.transcript_total_lines == 0 {
            return;
        }

        let base_x = area.x.saturating_add(TRANSCRIPT_GUTTER_COLS);
        let max_x = area.right().saturating_sub(1);

        let (start, end) = crate::transcript_selection::ordered_endpoints(anchor, head);

        let visible_start = self.transcript_view_top;
        let visible_end = self
            .transcript_view_top
            .saturating_add(area.height as usize)
            .min(self.transcript_total_lines);

        for (row_index, line_index) in (visible_start..visible_end).enumerate() {
            if line_index < start.line_index || line_index > end.line_index {
                continue;
            }

            let y = area.y + row_index as u16;

            let mut first_text_x = None;
            let mut last_text_x = None;
            for x in base_x..=max_x {
                let cell = &buf[(x, y)];
                if cell.symbol() != " " {
                    if first_text_x.is_none() {
                        first_text_x = Some(x);
                    }
                    last_text_x = Some(x);
                }
            }

            let (text_start, text_end) = match (first_text_x, last_text_x) {
                // インデントスペースを選択可能領域の一部として扱うため、
                // トランスクリプトガターの右側の最初のコンテンツ列から開始するが、
                // 末尾パディングが含まれないよう最後の非スペースグリフに制限。
                (Some(_), Some(e)) => (base_x, e),
                _ => continue,
            };

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

            let row_sel_start = base_x.saturating_add(line_start_col);
            let row_sel_end = base_x.saturating_add(line_end_col).min(max_x);

            if row_sel_start > row_sel_end {
                continue;
            }

            let from_x = row_sel_start.max(text_start);
            let to_x = row_sel_end.min(text_end);

            if from_x > to_x {
                continue;
            }

            for x in from_x..=to_x {
                let cell = &mut buf[(x, y)];
                let style = cell.style();
                cell.set_style(style.add_modifier(ratatui::style::Modifier::REVERSED));
            }
        }
    }

    /// 現在選択されているトランスクリプト領域をシステムクリップボードにコピー。
    ///
    /// 選択はフラット化され折り返されたトランスクリプト行インデックスと列の観点で
    /// 定義され、このメソッドは画面上のレンダリングに使用されるのと同じ折り返し
    /// トランスクリプトを再構築し、コピーされたテキストがハイライトされた領域に
    /// 近く一致するようにする。
    ///
    /// 重要: コピーは現在のビューポートだけでなく、選択のコンテンツ相対範囲全体に
    /// 対して動作する。選択は表示領域の外に拡張できる（例えば、選択後にスクロール
    /// したり、自動スクロール中に選択したり）ため、クリップボードのペイロードは
    /// 選択されたトランスクリプト全体を反映すべき。
    fn copy_transcript_selection(&mut self, tui: &tui::Tui) {
        let size = tui.terminal.last_known_screen_size;
        let width = size.width;
        let height = size.height;
        if width == 0 || height == 0 {
            return;
        }

        let chat_height = self.chat_widget.desired_height(width);
        if chat_height >= height {
            return;
        }

        let transcript_height = height.saturating_sub(chat_height);
        if transcript_height == 0 {
            return;
        }

        let Some(text) = crate::transcript_copy::selection_to_copy_text_for_cells(
            &self.transcript_cells,
            self.transcript_selection,
            width,
        ) else {
            return;
        };
        if let Err(err) = clipboard_copy::copy_text(text) {
            tracing::error!(error = %err, "failed to copy selection to clipboard");
        }
    }

    fn copy_selection_key(&self) -> crate::key_hint::KeyBinding {
        self.transcript_copy_ui.key_binding()
    }

    /// トランスクリプト領域内のマウス位置をコンテンツ相対選択ポイントに
    /// マッピング（選択可能なトランスクリプトコンテンツがある場合）。
    fn transcript_point_from_coordinates(
        &self,
        transcript_area: Rect,
        base_x: u16,
        x: u16,
        y: u16,
    ) -> Option<TranscriptSelectionPoint> {
        if self.transcript_total_lines == 0 {
            return None;
        }

        let mut row_index = y.saturating_sub(transcript_area.y);
        if row_index >= transcript_area.height {
            if transcript_area.height == 0 {
                return None;
            }
            row_index = transcript_area.height.saturating_sub(1);
        }

        let max_line = self.transcript_total_lines.saturating_sub(1);
        let line_index = self
            .transcript_view_top
            .saturating_add(usize::from(row_index))
            .min(max_line);
        let column = x.saturating_sub(base_x);

        Some(TranscriptSelectionPoint { line_index, column })
    }

    async fn handle_event(&mut self, tui: &mut tui::Tui, event: AppEvent) -> Result<bool> {
        let model_family = self
            .server
            .get_models_manager()
            .construct_model_family(self.current_model.as_str(), &self.config)
            .await;
        match event {
            AppEvent::NewSession => {
                let summary = session_summary(
                    self.chat_widget.token_usage(),
                    self.chat_widget.conversation_id(),
                );
                self.shutdown_current_conversation().await;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: self.config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: self.app_event_tx.clone(),
                    initial_prompt: None,
                    initial_images: Vec::new(),
                    enhanced_keys_supported: self.enhanced_keys_supported,
                    auth_manager: self.auth_manager.clone(),
                    models_manager: self.server.get_models_manager(),
                    feedback: self.feedback.clone(),
                    is_first_run: false,
                    model_family: model_family.clone(),
                };
                self.chat_widget = ChatWidget::new(init, self.server.clone());
                self.current_model = model_family.get_model_slug().to_string();
                if let Some(summary) = summary {
                    let mut lines: Vec<Line<'static>> = vec![summary.usage_line.clone().into()];
                    if let Some(command) = summary.resume_command {
                        let spans = vec!["To continue this session, run ".into(), command.cyan()];
                        lines.push(spans.into());
                    }
                    self.chat_widget.add_plain_history_lines(lines);
                }
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenResumePicker => {
                match crate::resume_picker::run_resume_picker(
                    tui,
                    &self.config.codex_home,
                    &self.config.model_provider_id,
                    false,
                )
                .await?
                {
                    ResumeSelection::Resume(path) => {
                        let summary = session_summary(
                            self.chat_widget.token_usage(),
                            self.chat_widget.conversation_id(),
                        );
                        match self
                            .server
                            .resume_conversation_from_rollout(
                                self.config.clone(),
                                path.clone(),
                                self.auth_manager.clone(),
                            )
                            .await
                        {
                            Ok(resumed) => {
                                self.shutdown_current_conversation().await;
                                let init = crate::chatwidget::ChatWidgetInit {
                                    config: self.config.clone(),
                                    frame_requester: tui.frame_requester(),
                                    app_event_tx: self.app_event_tx.clone(),
                                    initial_prompt: None,
                                    initial_images: Vec::new(),
                                    enhanced_keys_supported: self.enhanced_keys_supported,
                                    auth_manager: self.auth_manager.clone(),
                                    models_manager: self.server.get_models_manager(),
                                    feedback: self.feedback.clone(),
                                    is_first_run: false,
                                    model_family: model_family.clone(),
                                };
                                self.chat_widget = ChatWidget::new_from_existing(
                                    init,
                                    resumed.conversation,
                                    resumed.session_configured,
                                );
                                self.current_model = model_family.get_model_slug().to_string();
                                if let Some(summary) = summary {
                                    let mut lines: Vec<Line<'static>> =
                                        vec![summary.usage_line.clone().into()];
                                    if let Some(command) = summary.resume_command {
                                        let spans = vec![
                                            "To continue this session, run ".into(),
                                            command.cyan(),
                                        ];
                                        lines.push(spans.into());
                                    }
                                    self.chat_widget.add_plain_history_lines(lines);
                                }
                            }
                            Err(err) => {
                                self.chat_widget.add_error_message(format!(
                                    "Failed to resume session from {}: {err}",
                                    path.display()
                                ));
                            }
                        }
                    }
                    ResumeSelection::Exit | ResumeSelection::StartFresh => {}
                }

                // オルトスクリーンを離れるとインラインビューポートが空白になる可能性がある；いずれにせよ再描画を強制。
                tui.frame_requester().schedule_frame();
            }
            AppEvent::InsertHistoryCell(cell) => {
                let cell: Arc<dyn HistoryCell> = cell.into();
                if let Some(Overlay::Transcript(transcript)) = &mut self.overlay {
                    transcript.insert_cell(cell.clone());
                    tui.frame_requester().schedule_frame();
                }
                self.transcript_cells.push(cell.clone());
                let mut display = cell.display_lines(tui.terminal.last_known_screen_size.width);
                if !display.is_empty() {
                    // 進行中のストリームの一部ではない新しいセルに対してのみ
                    // 区切りの空行を挿入。ストリーミング継続はチャンク間に
                    // 余分な空行を蓄積すべきではない。
                    if !cell.is_stream_continuation() {
                        if self.has_emitted_history_lines {
                            display.insert(0, Line::from(""));
                        } else {
                            self.has_emitted_history_lines = true;
                        }
                    }
                    if self.overlay.is_some() {
                        self.deferred_history_lines.extend(display);
                    }
                }
            }
            AppEvent::StartCommitAnimation => {
                if self
                    .commit_anim_running
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let tx = self.app_event_tx.clone();
                    let running = self.commit_anim_running.clone();
                    thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(50));
                            tx.send(AppEvent::CommitTick);
                        }
                    });
                }
            }
            AppEvent::StopCommitAnimation => {
                self.commit_anim_running.store(false, Ordering::Release);
            }
            AppEvent::CommitTick => {
                self.chat_widget.on_commit_tick();
            }
            AppEvent::CodexEvent(event) => {
                if self.suppress_shutdown_complete
                    && matches!(event.msg, EventMsg::ShutdownComplete)
                {
                    self.suppress_shutdown_complete = false;
                    return Ok(true);
                }
                if let EventMsg::ListSkillsResponse(response) = &event.msg {
                    let cwd = self.chat_widget.config_ref().cwd.clone();
                    let errors = errors_for_cwd(&cwd, response);
                    emit_skill_load_warnings(&self.app_event_tx, &errors);
                }
                self.chat_widget.handle_codex_event(event);
            }
            AppEvent::ConversationHistory(ev) => {
                self.on_conversation_history_for_backtrack(tui, ev).await?;
            }
            AppEvent::ExitRequest => {
                return Ok(false);
            }
            AppEvent::CodexOp(op) => self.chat_widget.submit_op(op),
            AppEvent::DiffResult(text) => {
                // ボトムペインの進行中状態をクリア
                self.chat_widget.on_diff_complete();
                // TUIヘルパーを使用してオルトスクリーンに入り、ページャー行を構築
                let _ = tui.enter_alt_screen();
                let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    text.lines().map(ansi_escape_line).collect()
                };
                self.overlay = Some(Overlay::new_static_with_lines(
                    pager_lines,
                    "D I F F".to_string(),
                ));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::StartFileSearch(query) => {
                if !query.is_empty() {
                    self.file_search.on_user_query(query);
                }
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.chat_widget.apply_file_search_result(query, matches);
            }
            AppEvent::RateLimitSnapshotFetched(snapshot) => {
                self.chat_widget.on_rate_limit_snapshot(Some(snapshot));
            }
            AppEvent::UpdateReasoningEffort(effort) => {
                self.on_update_reasoning_effort(effort);
            }
            AppEvent::UpdateModel(model) => {
                let model_family = self
                    .server
                    .get_models_manager()
                    .construct_model_family(&model, &self.config)
                    .await;
                self.chat_widget.set_model(&model, model_family);
                self.current_model = model;
            }
            AppEvent::OpenReasoningPopup { model } => {
                self.chat_widget.open_reasoning_popup(model);
            }
            AppEvent::OpenAllModelsPopup { models } => {
                self.chat_widget.open_all_models_popup(models);
            }
            AppEvent::OpenFullAccessConfirmation { preset } => {
                self.chat_widget.open_full_access_confirmation(preset);
            }
            AppEvent::OpenWorldWritableWarningConfirmation {
                preset,
                sample_paths,
                extra_count,
                failed_scan,
            } => {
                self.chat_widget.open_world_writable_warning_confirmation(
                    preset,
                    sample_paths,
                    extra_count,
                    failed_scan,
                );
            }
            AppEvent::OpenFeedbackNote {
                category,
                include_logs,
            } => {
                self.chat_widget.open_feedback_note(category, include_logs);
            }
            AppEvent::OpenFeedbackConsent { category } => {
                self.chat_widget.open_feedback_consent(category);
            }
            AppEvent::OpenWindowsSandboxEnablePrompt { preset } => {
                self.chat_widget.open_windows_sandbox_enable_prompt(preset);
            }
            AppEvent::EnableWindowsSandboxForAgentMode { preset } => {
                #[cfg(target_os = "windows")]
                {
                    let profile = self.active_profile.as_deref();
                    let feature_key = Feature::WindowsSandbox.key();
                    match ConfigEditsBuilder::new(&self.config.codex_home)
                        .with_profile(profile)
                        .set_feature_enabled(feature_key, true)
                        .apply()
                        .await
                    {
                        Ok(()) => {
                            self.config.set_windows_sandbox_globally(true);
                            self.chat_widget.clear_forced_auto_mode_downgrade();
                            if let Some((sample_paths, extra_count, failed_scan)) =
                                self.chat_widget.world_writable_warning_details()
                            {
                                self.app_event_tx.send(
                                    AppEvent::OpenWorldWritableWarningConfirmation {
                                        preset: Some(preset.clone()),
                                        sample_paths,
                                        extra_count,
                                        failed_scan,
                                    },
                                );
                            } else {
                                self.app_event_tx.send(AppEvent::CodexOp(
                                    Op::OverrideTurnContext {
                                        cwd: None,
                                        approval_policy: Some(preset.approval),
                                        sandbox_policy: Some(preset.sandbox.clone()),
                                        model: None,
                                        effort: None,
                                        summary: None,
                                    },
                                ));
                                self.app_event_tx
                                    .send(AppEvent::UpdateAskForApprovalPolicy(preset.approval));
                                self.app_event_tx
                                    .send(AppEvent::UpdateSandboxPolicy(preset.sandbox.clone()));
                                self.chat_widget.add_info_message(
                                    "Enabled experimental Windows sandbox.".to_string(),
                                    None,
                                );
                            }
                        }
                        Err(err) => {
                            tracing::error!(
                                error = %err,
                                "failed to enable Windows sandbox feature"
                            );
                            self.chat_widget.add_error_message(format!(
                                "Failed to enable the Windows sandbox feature: {err}"
                            ));
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = preset;
                }
            }
            AppEvent::PersistModelSelection { model, effort } => {
                let profile = self.active_profile.as_deref();
                match ConfigEditsBuilder::new(&self.config.codex_home)
                    .with_profile(profile)
                    .set_model(Some(model.as_str()), effort)
                    .apply()
                    .await
                {
                    Ok(()) => {
                        let mut message = format!("Model changed to {model}");
                        if let Some(label) = Self::reasoning_label_for(&model, effort) {
                            message.push(' ');
                            message.push_str(label);
                        }
                        if let Some(profile) = profile {
                            message.push_str(" for ");
                            message.push_str(profile);
                            message.push_str(" profile");
                        }
                        self.chat_widget.add_info_message(message, None);
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist model selection"
                        );
                        if let Some(profile) = profile {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save model for profile `{profile}`: {err}"
                            ));
                        } else {
                            self.chat_widget
                                .add_error_message(format!("Failed to save default model: {err}"));
                        }
                    }
                }
            }
            AppEvent::UpdateAskForApprovalPolicy(policy) => {
                self.chat_widget.set_approval_policy(policy);
            }
            AppEvent::UpdateSandboxPolicy(policy) => {
                #[cfg(target_os = "windows")]
                let policy_is_workspace_write_or_ro = matches!(
                    &policy,
                    codex_core::protocol::SandboxPolicy::WorkspaceWrite { .. }
                        | codex_core::protocol::SandboxPolicy::ReadOnly
                );

                if let Err(err) = self.config.sandbox_policy.set(policy.clone()) {
                    tracing::warn!(%err, "failed to set sandbox policy on app config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set sandbox policy: {err}"));
                    return Ok(true);
                }
                #[cfg(target_os = "windows")]
                if !matches!(&policy, codex_core::protocol::SandboxPolicy::ReadOnly)
                    || codex_core::get_platform_sandbox().is_some()
                {
                    self.config.forced_auto_mode_downgraded_on_windows = false;
                }
                if let Err(err) = self.chat_widget.set_sandbox_policy(policy) {
                    tracing::warn!(%err, "failed to set sandbox policy on chat config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set sandbox policy: {err}"));
                    return Ok(true);
                }

                // サンドボックスポリシーがworkspace-writeまたはread-onlyになった場合、Windowsのworld-writableスキャンを実行。
                #[cfg(target_os = "windows")]
                {
                    // ユーザーが続行を確認した直後の一度だけの抑制。
                    if self.skip_world_writable_scan_once {
                        self.skip_world_writable_scan_once = false;
                        return Ok(true);
                    }

                    let should_check = codex_core::get_platform_sandbox().is_some()
                        && policy_is_workspace_write_or_ro
                        && !self.chat_widget.world_writable_warning_hidden();
                    if should_check {
                        let cwd = self.config.cwd.clone();
                        let env_map: std::collections::HashMap<String, String> =
                            std::env::vars().collect();
                        let tx = self.app_event_tx.clone();
                        let logs_base_dir = self.config.codex_home.clone();
                        let sandbox_policy = self.config.sandbox_policy.get().clone();
                        Self::spawn_world_writable_scan(
                            cwd,
                            env_map,
                            logs_base_dir,
                            sandbox_policy,
                            tx,
                        );
                    }
                }
            }
            AppEvent::SkipNextWorldWritableScan => {
                self.skip_world_writable_scan_once = true;
            }
            AppEvent::UpdateFullAccessWarningAcknowledged(ack) => {
                self.chat_widget.set_full_access_warning_acknowledged(ack);
            }
            AppEvent::UpdateWorldWritableWarningAcknowledged(ack) => {
                self.chat_widget
                    .set_world_writable_warning_acknowledged(ack);
            }
            AppEvent::UpdateRateLimitSwitchPromptHidden(hidden) => {
                self.chat_widget.set_rate_limit_switch_prompt_hidden(hidden);
            }
            AppEvent::PersistFullAccessWarningAcknowledged => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.codex_home)
                    .set_hide_full_access_warning(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist full access warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save full access confirmation preference: {err}"
                    ));
                }
            }
            AppEvent::PersistWorldWritableWarningAcknowledged => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.codex_home)
                    .set_hide_world_writable_warning(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist world-writable warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save Agent mode warning preference: {err}"
                    ));
                }
            }
            AppEvent::PersistRateLimitSwitchPromptHidden => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.codex_home)
                    .set_hide_rate_limit_model_nudge(true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist rate limit switch prompt preference"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save rate limit reminder preference: {err}"
                    ));
                }
            }
            AppEvent::PersistModelMigrationPromptAcknowledged {
                from_model,
                to_model,
            } => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.codex_home)
                    .record_model_migration_seen(from_model.as_str(), to_model.as_str())
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist model migration prompt acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save model migration prompt preference: {err}"
                    ));
                }
            }
            AppEvent::OpenApprovalsPopup => {
                self.chat_widget.open_approvals_popup();
            }
            AppEvent::OpenReviewBranchPicker(cwd) => {
                self.chat_widget.show_review_branch_picker(&cwd).await;
            }
            AppEvent::OpenReviewCommitPicker(cwd) => {
                self.chat_widget.show_review_commit_picker(&cwd).await;
            }
            AppEvent::OpenReviewCustomPrompt => {
                self.chat_widget.show_review_custom_prompt();
            }
            AppEvent::FullScreenApprovalRequest(request) => match request {
                ApprovalRequest::ApplyPatch { cwd, changes, .. } => {
                    let _ = tui.enter_alt_screen();
                    let diff_summary = DiffSummary::new(changes, cwd);
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![diff_summary.into()],
                        "P A T C H".to_string(),
                    ));
                }
                ApprovalRequest::Exec { command, .. } => {
                    let _ = tui.enter_alt_screen();
                    let full_cmd = strip_bash_lc_and_escape(&command);
                    let full_cmd_lines = highlight_bash_to_lines(&full_cmd);
                    self.overlay = Some(Overlay::new_static_with_lines(
                        full_cmd_lines,
                        "E X E C".to_string(),
                    ));
                }
                ApprovalRequest::McpElicitation {
                    server_name,
                    message,
                    ..
                } => {
                    let _ = tui.enter_alt_screen();
                    let paragraph = Paragraph::new(vec![
                        Line::from(vec!["Server: ".into(), server_name.bold()]),
                        Line::from(""),
                        Line::from(message),
                    ])
                    .wrap(Wrap { trim: false });
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(paragraph)],
                        "E L I C I T A T I O N".to_string(),
                    ));
                }
            },
        }
        Ok(true)
    }

    fn reasoning_label(reasoning_effort: Option<ReasoningEffortConfig>) -> &'static str {
        match reasoning_effort {
            Some(ReasoningEffortConfig::Minimal) => "minimal",
            Some(ReasoningEffortConfig::Low) => "low",
            Some(ReasoningEffortConfig::Medium) => "medium",
            Some(ReasoningEffortConfig::High) => "high",
            Some(ReasoningEffortConfig::XHigh) => "xhigh",
            None | Some(ReasoningEffortConfig::None) => "default",
        }
    }

    fn reasoning_label_for(
        model: &str,
        reasoning_effort: Option<ReasoningEffortConfig>,
    ) -> Option<&'static str> {
        (!model.starts_with("codex-auto-")).then(|| Self::reasoning_label(reasoning_effort))
    }

    pub(crate) fn token_usage(&self) -> codex_core::protocol::TokenUsage {
        self.chat_widget.token_usage()
    }

    fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        self.chat_widget.set_reasoning_effort(effort);
        self.config.model_reasoning_effort = effort;
    }

    async fn handle_key_event(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                // オルトスクリーンに入り、ビューポートをフルサイズに設定。
                let _ = tui.enter_alt_screen();
                self.overlay = Some(Overlay::new_transcript(self.transcript_cells.clone()));
                tui.frame_requester().schedule_frame();
            }
            // Escはコンポーザーがフォーカスされ空の状態で、通常（作業中でない）モードの
            // 時のみバックトラッキングを準備/進行。その他の状態ではEscを転送し、
            // アクティブなUI（例: ステータスインジケーター、モーダル、ポップアップ）が
            // 処理するようにする。
            KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                if self.chat_widget.is_normal_backtrack_mode()
                    && self.chat_widget.composer_is_empty()
                {
                    self.handle_backtrack_esc_key(tui);
                } else {
                    self.chat_widget.handle_key_event(key_event);
                }
            }
            KeyEvent {
                code: KeyCode::Char(ch),
                modifiers,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } if self.transcript_copy_ui.is_copy_key(ch, modifiers) => {
                self.copy_transcript_selection(tui);
            }
            KeyEvent {
                code: KeyCode::PageUp,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                let size = tui.terminal.last_known_screen_size;
                let width = size.width;
                let height = size.height;
                if width > 0 && height > 0 {
                    let chat_height = self.chat_widget.desired_height(width);
                    if chat_height < height {
                        let transcript_height = height.saturating_sub(chat_height);
                        if transcript_height > 0 {
                            let delta = -i32::from(transcript_height);
                            self.scroll_transcript(
                                tui,
                                delta,
                                usize::from(transcript_height),
                                width,
                                true,
                            );
                        }
                    }
                }
            }
            KeyEvent {
                code: KeyCode::PageDown,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                let size = tui.terminal.last_known_screen_size;
                let width = size.width;
                let height = size.height;
                if width > 0 && height > 0 {
                    let chat_height = self.chat_widget.desired_height(width);
                    if chat_height < height {
                        let transcript_height = height.saturating_sub(chat_height);
                        if transcript_height > 0 {
                            let delta = i32::from(transcript_height);
                            self.scroll_transcript(
                                tui,
                                delta,
                                usize::from(transcript_height),
                                width,
                                true,
                            );
                        }
                    }
                }
            }
            KeyEvent {
                code: KeyCode::Home,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                if !self.transcript_cells.is_empty() {
                    self.transcript_scroll = TranscriptScroll::Scrolled {
                        cell_index: 0,
                        line_in_cell: 0,
                    };
                    tui.frame_requester().schedule_frame();
                }
            }
            KeyEvent {
                code: KeyCode::End,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                self.transcript_scroll = TranscriptScroll::ToBottom;
                tui.frame_requester().schedule_frame();
            }
            // Enterは準備済み + カウント > 0 の時にバックトラックを確認。それ以外はウィジェットに渡す。
            KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            } if self.backtrack.primed
                && self.backtrack.nth_user_message != usize::MAX
                && self.chat_widget.composer_is_empty() =>
            {
                // 明確さのためヘルパーに委譲；動作を保持。
                self.confirm_backtrack_from_main();
            }
            KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                // Esc以外のキー押下は準備済みバックトラックをキャンセルすべき。
                // これによりユーザーが入力を開始した後の古い「Esc準備済み」状態を回避
                // （後でバックスペースで空にしても）。
                if key_event.code != KeyCode::Esc && self.backtrack.primed {
                    self.reset_backtrack_state();
                }
                self.chat_widget.handle_key_event(key_event);
            }
            _ => {
                // Releaseキーイベントを無視。
            }
        };
    }

    #[cfg(target_os = "windows")]
    fn spawn_world_writable_scan(
        cwd: PathBuf,
        env_map: std::collections::HashMap<String, String>,
        logs_base_dir: PathBuf,
        sandbox_policy: codex_core::protocol::SandboxPolicy,
        tx: AppEventSender,
    ) {
        tokio::task::spawn_blocking(move || {
            let result = codex_windows_sandbox::apply_world_writable_scan_and_denies(
                &logs_base_dir,
                &cwd,
                &env_map,
                &sandbox_policy,
                Some(logs_base_dir.as_path()),
            );
            if result.is_err() {
                // スキャン失敗: 例なしで警告。
                tx.send(AppEvent::OpenWorldWritableWarningConfirmation {
                    preset: None,
                    sample_paths: Vec::new(),
                    extra_count: 0usize,
                    failed_scan: true,
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_backtrack::BacktrackState;
    use crate::app_backtrack::user_count;
    use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
    use crate::file_search::FileSearchManager;
    use crate::history_cell::AgentMessageCell;
    use crate::history_cell::HistoryCell;
    use crate::history_cell::UserHistoryCell;
    use crate::history_cell::new_session_info;
    use crate::transcript_copy_ui::CopySelectionShortcut;
    use codex_core::AuthManager;
    use codex_core::CodexAuth;
    use codex_core::ConversationManager;
    use codex_core::protocol::AskForApproval;
    use codex_core::protocol::Event;
    use codex_core::protocol::EventMsg;
    use codex_core::protocol::SandboxPolicy;
    use codex_core::protocol::SessionConfiguredEvent;
    use codex_protocol::ConversationId;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::Line;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    async fn make_test_app() -> App {
        let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
        let config = chat_widget.config_ref().clone();
        let current_model = chat_widget.get_model_family().get_model_slug().to_string();
        let server = Arc::new(ConversationManager::with_models_provider(
            CodexAuth::from_api_key("Test API Key"),
            config.model_provider.clone(),
        ));
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());

        App {
            server,
            app_event_tx,
            chat_widget,
            auth_manager,
            config,
            current_model,
            active_profile: None,
            file_search,
            transcript_cells: Vec::new(),
            transcript_scroll: TranscriptScroll::default(),
            transcript_selection: TranscriptSelection::default(),
            transcript_multi_click: TranscriptMultiClick::default(),
            transcript_view_top: 0,
            transcript_total_lines: 0,
            transcript_copy_ui: TranscriptCopyUi::new_with_shortcut(
                CopySelectionShortcut::CtrlShiftC,
            ),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            enhanced_keys_supported: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            scroll_config: ScrollConfig::default(),
            scroll_state: MouseScrollState::default(),
            backtrack: BacktrackState::default(),
            feedback: codex_feedback::CodexFeedback::new(),
            pending_update_action: None,
            suppress_shutdown_complete: false,
            skip_world_writable_scan_once: false,
        }
    }

    async fn make_test_app_with_channels() -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        tokio::sync::mpsc::UnboundedReceiver<Op>,
    ) {
        let (chat_widget, app_event_tx, rx, op_rx) = make_chatwidget_manual_with_sender().await;
        let config = chat_widget.config_ref().clone();
        let current_model = chat_widget.get_model_family().get_model_slug().to_string();
        let server = Arc::new(ConversationManager::with_models_provider(
            CodexAuth::from_api_key("Test API Key"),
            config.model_provider.clone(),
        ));
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());

        (
            App {
                server,
                app_event_tx,
                chat_widget,
                auth_manager,
                config,
                current_model,
                active_profile: None,
                file_search,
                transcript_cells: Vec::new(),
                transcript_scroll: TranscriptScroll::default(),
                transcript_selection: TranscriptSelection::default(),
                transcript_multi_click: TranscriptMultiClick::default(),
                transcript_view_top: 0,
                transcript_total_lines: 0,
                transcript_copy_ui: TranscriptCopyUi::new_with_shortcut(
                    CopySelectionShortcut::CtrlShiftC,
                ),
                overlay: None,
                deferred_history_lines: Vec::new(),
                has_emitted_history_lines: false,
                enhanced_keys_supported: false,
                commit_anim_running: Arc::new(AtomicBool::new(false)),
                scroll_config: ScrollConfig::default(),
                scroll_state: MouseScrollState::default(),
                backtrack: BacktrackState::default(),
                feedback: codex_feedback::CodexFeedback::new(),
                pending_update_action: None,
                suppress_shutdown_complete: false,
                skip_world_writable_scan_once: false,
            },
            rx,
            op_rx,
        )
    }

    fn all_model_presets() -> Vec<ModelPreset> {
        codex_core::models_manager::model_presets::all_model_presets().clone()
    }

    #[tokio::test]
    async fn model_migration_prompt_only_shows_for_deprecated_models() {
        let seen = BTreeMap::new();
        assert!(should_show_model_migration_prompt(
            "gpt-5",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
        assert!(should_show_model_migration_prompt(
            "gpt-5-codex",
            "gpt-5.1-codex",
            &seen,
            &all_model_presets()
        ));
        assert!(should_show_model_migration_prompt(
            "gpt-5-codex-mini",
            "gpt-5.1-codex-mini",
            &seen,
            &all_model_presets()
        ));
        assert!(should_show_model_migration_prompt(
            "gpt-5.1-codex",
            "gpt-5.1-codex-max",
            &seen,
            &all_model_presets()
        ));
        assert!(!should_show_model_migration_prompt(
            "gpt-5.1-codex",
            "gpt-5.1-codex",
            &seen,
            &all_model_presets()
        ));
    }

    #[tokio::test]
    async fn transcript_selection_copy_includes_offscreen_lines() {
        let mut app = make_test_app().await;
        app.transcript_cells = vec![Arc::new(AgentMessageCell::new(
            vec![
                Line::from("one"),
                Line::from("two"),
                Line::from("three"),
                Line::from("four"),
            ],
            true,
        ))];

        app.transcript_view_top = 2;
        app.transcript_selection.anchor = Some(TranscriptSelectionPoint {
            line_index: 0,
            column: 0,
        });
        app.transcript_selection.head = Some(TranscriptSelectionPoint {
            line_index: 3,
            column: u16::MAX,
        });

        let text = crate::transcript_copy::selection_to_copy_text_for_cells(
            &app.transcript_cells,
            app.transcript_selection,
            40,
        )
        .expect("expected text");
        assert_eq!(text, "one\ntwo\nthree\nfour");
    }

    #[tokio::test]
    async fn model_migration_prompt_respects_hide_flag_and_self_target() {
        let mut seen = BTreeMap::new();
        seen.insert("gpt-5".to_string(), "gpt-5.1".to_string());
        assert!(!should_show_model_migration_prompt(
            "gpt-5",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
        assert!(!should_show_model_migration_prompt(
            "gpt-5.1",
            "gpt-5.1",
            &seen,
            &all_model_presets()
        ));
    }

    #[tokio::test]
    async fn update_reasoning_effort_updates_config() {
        let mut app = make_test_app().await;
        app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Medium);
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

        app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.chat_widget.config_ref().model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn backtrack_selection_with_duplicate_history_targets_unique_turn() {
        let mut app = make_test_app().await;

        let user_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(UserHistoryCell {
                message: text.to_string(),
            }) as Arc<dyn HistoryCell>
        };
        let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(AgentMessageCell::new(
                vec![Line::from(text.to_string())],
                true,
            )) as Arc<dyn HistoryCell>
        };

        let make_header = |is_first| {
            let event = SessionConfiguredEvent {
                session_id: ConversationId::new(),
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                approval_policy: AskForApproval::Never,
                sandbox_policy: SandboxPolicy::ReadOnly,
                cwd: PathBuf::from("/home/user/project"),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                rollout_path: PathBuf::new(),
            };
            Arc::new(new_session_info(
                app.chat_widget.config_ref(),
                app.current_model.as_str(),
                event,
                is_first,
            )) as Arc<dyn HistoryCell>
        };

        // フォーク用のトリミング後、履歴の再生、編集されたターンの追加後の
        // トランスクリプトをシミュレート。セッションヘッダーは保持された履歴と
        // フォークされた会話の再生されたターンを分離。
        app.transcript_cells = vec![
            make_header(true),
            user_cell("first question"),
            agent_cell("answer first"),
            user_cell("follow-up"),
            agent_cell("answer follow-up"),
            make_header(false),
            user_cell("first question"),
            agent_cell("answer first"),
            user_cell("follow-up (edited)"),
            agent_cell("answer edited"),
        ];

        assert_eq!(user_count(&app.transcript_cells), 2);

        app.backtrack.base_id = Some(ConversationId::new());
        app.backtrack.primed = true;
        app.backtrack.nth_user_message = user_count(&app.transcript_cells).saturating_sub(1);

        app.confirm_backtrack_from_main();

        let (_, nth, prefill) = app.backtrack.pending.clone().expect("pending backtrack");
        assert_eq!(nth, 1);
        assert_eq!(prefill, "follow-up (edited)");
    }

    #[tokio::test]
    async fn transcript_selection_moves_with_scroll() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let mut app = make_test_app().await;
        app.transcript_total_lines = 3;

        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 2,
        };

        // 論理行1、列2..4に選択をアンカー。
        app.transcript_selection = TranscriptSelection {
            anchor: Some(TranscriptSelectionPoint {
                line_index: 1,
                column: 2,
            }),
            head: Some(TranscriptSelectionPoint {
                line_index: 1,
                column: 4,
            }),
        };

        // 最初のレンダリング: ビューの先頭が行0なので、行1は2行目にマップ。
        app.transcript_view_top = 0;
        let mut buf = Buffer::empty(area);
        for x in 2..area.width {
            buf[(x, 0)].set_symbol("A");
            buf[(x, 1)].set_symbol("B");
        }

        app.apply_transcript_selection(area, &mut buf);

        // ビューが先頭にアンカーされている時、最初の行には選択が適用されるべきではない。
        for x in 0..area.width {
            let cell = &buf[(x, 0)];
            assert!(cell.style().add_modifier.is_empty());
        }

        // 1行下にスクロールした後、同じ論理行が最初の行にレンダリングされ、
        // ハイライトもそれと共に移動すべき。
        app.transcript_view_top = 1;
        let mut buf_scrolled = Buffer::empty(area);
        for x in 2..area.width {
            buf_scrolled[(x, 0)].set_symbol("B");
            buf_scrolled[(x, 1)].set_symbol("C");
        }

        app.apply_transcript_selection(area, &mut buf_scrolled);

        // スクロール後、選択は2行目ではなく最初の行に適用されるべき。
        for x in 0..area.width {
            let cell = &buf_scrolled[(x, 1)];
            assert!(cell.style().add_modifier.is_empty());
        }
    }

    #[tokio::test]
    async fn transcript_selection_renders_copy_affordance() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let mut app = make_test_app().await;
        app.transcript_total_lines = 3;
        app.transcript_view_top = 0;

        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 3,
        };

        app.transcript_selection = TranscriptSelection {
            anchor: Some(TranscriptSelectionPoint {
                line_index: 1,
                column: 2,
            }),
            head: Some(TranscriptSelectionPoint {
                line_index: 1,
                column: 6,
            }),
        };

        let mut buf = Buffer::empty(area);
        for y in 0..area.height {
            for x in 2..area.width.saturating_sub(1) {
                buf[(x, y)].set_symbol("X");
            }
        }

        app.apply_transcript_selection(area, &mut buf);
        let anchor = app.transcript_selection.anchor.expect("anchor");
        let head = app.transcript_selection.head.expect("head");
        app.transcript_copy_ui.render_copy_pill(
            area,
            &mut buf,
            (anchor.line_index, anchor.column),
            (head.line_index, head.column),
            app.transcript_view_top,
            app.transcript_total_lines,
        );

        let mut s = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                s.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            s.push('\n');
        }

        assert!(s.contains("copy"));
        assert!(s.contains("ctrl + shift + c"));
        assert!(app.transcript_copy_ui.hit_test(10, 2));
    }

    #[tokio::test]
    async fn transcript_selection_renders_ctrl_y_copy_affordance_in_vscode_mode() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let mut app = make_test_app().await;
        app.transcript_copy_ui = TranscriptCopyUi::new_with_shortcut(CopySelectionShortcut::CtrlY);
        app.transcript_total_lines = 3;
        app.transcript_view_top = 0;

        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 3,
        };

        app.transcript_selection = TranscriptSelection {
            anchor: Some(TranscriptSelectionPoint {
                line_index: 1,
                column: 2,
            }),
            head: Some(TranscriptSelectionPoint {
                line_index: 1,
                column: 6,
            }),
        };

        let mut buf = Buffer::empty(area);
        for y in 0..area.height {
            for x in 2..area.width.saturating_sub(1) {
                buf[(x, y)].set_symbol("X");
            }
        }

        app.apply_transcript_selection(area, &mut buf);
        let anchor = app.transcript_selection.anchor.expect("anchor");
        let head = app.transcript_selection.head.expect("head");
        app.transcript_copy_ui.render_copy_pill(
            area,
            &mut buf,
            (anchor.line_index, anchor.column),
            (head.line_index, head.column),
            app.transcript_view_top,
            app.transcript_total_lines,
        );

        let mut s = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                s.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            s.push('\n');
        }

        assert!(s.contains("copy"));
        assert!(s.contains("ctrl + y"));
        assert!(!s.contains("ctrl + shift + c"));
        assert!(app.transcript_copy_ui.hit_test(10, 2));
    }

    #[tokio::test]
    async fn transcript_selection_hides_copy_affordance_while_dragging() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;

        let mut app = make_test_app().await;
        app.transcript_total_lines = 3;
        app.transcript_view_top = 0;
        app.transcript_copy_ui.set_dragging(true);

        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 3,
        };

        app.transcript_selection = TranscriptSelection {
            anchor: Some(TranscriptSelectionPoint {
                line_index: 1,
                column: 2,
            }),
            head: Some(TranscriptSelectionPoint {
                line_index: 1,
                column: 6,
            }),
        };

        let mut buf = Buffer::empty(area);
        for y in 0..area.height {
            for x in 2..area.width.saturating_sub(1) {
                buf[(x, y)].set_symbol("X");
            }
        }

        let anchor = app.transcript_selection.anchor.expect("anchor");
        let head = app.transcript_selection.head.expect("head");
        app.transcript_copy_ui.render_copy_pill(
            area,
            &mut buf,
            (anchor.line_index, anchor.column),
            (head.line_index, head.column),
            app.transcript_view_top,
            app.transcript_total_lines,
        );

        let mut s = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                s.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            s.push('\n');
        }

        assert!(!s.contains("copy"));
        assert!(!app.transcript_copy_ui.hit_test(10, 2));
    }

    #[tokio::test]
    async fn new_session_requests_shutdown_for_previous_conversation() {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let conversation_id = ConversationId::new();
        let event = SessionConfiguredEvent {
            session_id: conversation_id,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::ReadOnly,
            cwd: PathBuf::from("/home/user/project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            rollout_path: PathBuf::new(),
        };

        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(event),
        });

        while app_event_rx.try_recv().is_ok() {}
        while op_rx.try_recv().is_ok() {}

        app.shutdown_current_conversation().await;

        match op_rx.try_recv() {
            Ok(Op::Shutdown) => {}
            Ok(other) => panic!("expected Op::Shutdown, got {other:?}"),
            Err(_) => panic!("expected shutdown op to be sent"),
        }
    }

    #[tokio::test]
    async fn session_summary_skip_zero_usage() {
        assert!(session_summary(TokenUsage::default(), None).is_none());
    }

    #[tokio::test]
    async fn render_lines_to_ansi_pads_user_rows_to_full_width() {
        let line: Line<'static> = Line::from("hi");
        let lines = vec![line];
        let line_meta = vec![TranscriptLineMeta::CellLine {
            cell_index: 0,
            line_in_cell: 0,
        }];
        let is_user_cell = vec![true];
        let width: u16 = 10;

        let rendered = crate::transcript_render::render_lines_to_ansi(
            &lines,
            &line_meta,
            &is_user_cell,
            width,
        );
        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].contains("hi"));
    }

    #[tokio::test]
    async fn session_summary_includes_resume_hint() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        };
        let conversation =
            ConversationId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();

        let summary = session_summary(usage, Some(conversation)).expect("summary");
        assert_eq!(
            summary.usage_line,
            "Token usage: total=12 input=10 output=2"
        );
        assert_eq!(
            summary.resume_command,
            Some("codex resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }
}
