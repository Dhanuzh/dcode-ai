//! Keyboard handling for the session TUI.

use super::*;

/// `Ctrl+\`. Terminals in legacy mode report it as `Ctrl+4` (ASCII FS).
pub(super) fn is_view_toggle(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('\\') | KeyCode::Char('4'))
}

/// Two Esc presses within this window open the rewind picker.
const DOUBLE_ESC_WINDOW: std::time::Duration = std::time::Duration::from_millis(800);

/// Time for the second Ctrl-C / Ctrl-D that exits.
const EXIT_WINDOW: Duration = Duration::from_millis(2000);

/// What the main loop should do after a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyFlow {
    Continue,
    Exit,
}

/// Handle one key event: overlay/modal key machines, approvals, questions,
/// composer editing and global shortcuts. Runs with the state lock held.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_key_event(
    key: KeyEvent,
    g: &mut TuiSessionState,
    terminal_size: Option<ratatui::layout::Size>,
    slash_entries: &[SlashEntry],
    workspace_files: &[String],
    transcript_cache: &mut TranscriptRenderCache,
    scroll_buffer: &mut crate::tui::scroll_buffer::ScrollBuffer,
    scroll_speed: u16,
    composer_history: &mut Vec<String>,
    composer_history_index: &mut Option<usize>,
    composer_history_draft: &mut String,
    ctrl_c_armed_at: &mut Option<std::time::Instant>,
    cmd_tx: &UnboundedSender<TuiCmd>,
    question_answer_tx: Option<&UnboundedSender<(String, QuestionSelection)>>,
    approval_answer_tx: Option<&UnboundedSender<ApprovalAnswer>>,
    cancel_flag: Option<&Arc<std::sync::atomic::AtomicBool>>,
) -> KeyFlow {
    if matches!(key.kind, KeyEventKind::Release) {
        return KeyFlow::Continue;
    }
    // Customizable keybindings: translate the key through the
    // user's remap (rewrite custom→default, suppress reassigned
    // defaults). No-op when nothing is remapped.
    let key = if g.key_bindings.is_empty() {
        key
    } else {
        match translate_key(key, &g.key_bindings) {
            Some(k) => k,
            None => return KeyFlow::Continue,
        }
    };

    // Hybrid UI: Ctrl+\ toggles inline ⇄ full-screen dashboard.
    if is_view_toggle(&key) {
        g.toggle_view_mode();
        return KeyFlow::Continue;
    }
    // Dashboard paging scrolls the full transcript directly (the inline
    // scroll buffer models the scrollback viewport, not this pane).
    if g.view_mode == crate::tui::state::ViewMode::Dashboard
        && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
    {
        let page = terminal_size
            .map(|s| s.height.saturating_sub(8).max(4) as usize)
            .unwrap_or(10);
        if key.code == KeyCode::PageUp {
            g.transcript_follow_tail = false;
            g.scroll_lines = g.scroll_lines.saturating_sub(page);
        } else {
            g.scroll_lines = g.scroll_lines.saturating_add(page);
        }
        return KeyFlow::Continue;
    }

    // Emacs-style list navigation: while a list overlay is
    // active, Ctrl+N/Ctrl+P act as Down/Up. Exceptions keep
    // their stronger meanings: Ctrl+P still toggles the
    // command palette closed, and Ctrl+N still answers "no"
    // on an approval prompt.
    let list_nav_active = g.model_picker_open
        || g.session_picker_open
        || g.connect_modal_open
        || g.provider_picker_open
        || g.permission_picker_open
        || g.agent_picker_open
        || g.theme_picker_open
        || g.project_picker_open
        || g.branch_picker_open
        || g.question_modal_open
        || g.pins_modal_open
        || g.subagent_modal_open
        || g.backtrack_open
        || g.composer_history_search_open
        || slash_panel_visible(&g.input_buffer)
        || at_completion_active(&g.input_buffer, g.cursor_char_idx);
    let key = if (list_nav_active || g.command_palette_open)
        && key.modifiers == KeyModifiers::CONTROL
        && g.active_approval.is_none()
    {
        match key.code {
            KeyCode::Char('n') => KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            KeyCode::Char('p') if !g.command_palette_open => {
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)
            }
            _ => key,
        }
    } else {
        key
    };

    // Any keystroke clears active mouse text selection.
    if g.mouse_selection.is_some() {
        g.mouse_selection = None;
    }

    // The sudo password popup owns the keyboard while open.
    if let Some(prompt) = g.sudo_prompt.as_mut() {
        use crate::tui::sudo_prompt::SudoKey;
        match prompt.handle_key(key) {
            SudoKey::Handled => {}
            SudoKey::Cancel => {
                g.sudo_prompt = None;
                match g.askpass_reply.take() {
                    Some(tx) => {
                        let _ = tx.send(None);
                    }
                    None => {
                        let _ = cmd_tx.send(TuiCmd::SudoCancelled);
                    }
                }
            }
            SudoKey::Submit(password) => {
                g.sudo_prompt = None;
                match g.askpass_reply.take() {
                    // A running command's sudo is waiting for it.
                    Some(tx) => {
                        let _ = tx.send(Some(password));
                    }
                    None => {
                        let _ = cmd_tx.send(TuiCmd::SudoPassword(password));
                    }
                }
            }
        }
        return KeyFlow::Continue;
    }

    // `/setup` picker owns the keyboard while open (Ctrl+C still exits).
    if let Some(picker) = g.setup_picker.as_mut()
        && !(key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        use crate::tui::setup_picker::SetupKey;
        match picker.handle_key(key) {
            SetupKey::Handled => {}
            SetupKey::Close => g.setup_picker = None,
            SetupKey::Install(ids) => {
                g.setup_picker = None;
                let _ = cmd_tx.send(TuiCmd::Submit(format!("/setup {}", ids.join(" "))));
            }
        }
        return KeyFlow::Continue;
    }

    // `/mcp` panel owns the keyboard while open (Ctrl+C still exits).
    if let Some(panel) = g.mcp_panel.as_mut()
        && !(key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        use crate::tui::mcp_panel::McpKey;
        match panel.handle_key(key) {
            McpKey::Handled => {}
            McpKey::Close => g.mcp_panel = None,
            McpKey::Run(action) => {
                let _ = cmd_tx.send(TuiCmd::McpAction(action));
            }
        }
        return KeyFlow::Continue;
    }

    // `/plugins` browser owns the keyboard while open (Ctrl+C still exits).
    if let Some(browser) = g.plugin_browser.as_mut()
        && !(key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        use crate::tui::plugin_browser::BrowserKey;
        match browser.handle_key(key) {
            BrowserKey::Handled => {}
            BrowserKey::Close => g.plugin_browser = None,
            BrowserKey::Run(action) => {
                browser.busy = Some(action.progress());
                browser.status = None;
                let _ = cmd_tx.send(TuiCmd::PluginAction(action));
            }
        }
        return KeyFlow::Continue;
    }

    // Permission prompt: 1 yes · 2 always · 3 no, same as y / a / n.
    let key = if g.active_approval.is_some()
        && !g.approval_hunk_mode
        && key.modifiers == KeyModifiers::NONE
    {
        match key.code {
            KeyCode::Char('1') => KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
            KeyCode::Char('2') => KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            KeyCode::Char('3') => KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            _ => key,
        }
    } else {
        key
    };

    // Dashboard changes tab: Ctrl+D focuses it; while focused it owns
    // navigation keys. Overlays, approvals and questions keep priority.
    let dashboard_free = g.view_mode == crate::tui::state::ViewMode::Dashboard
        && !list_nav_active
        && !g.command_palette_open
        && g.active_approval.is_none()
        && g.active_question.is_none();
    if dashboard_free && key.code == KeyCode::Char('d') && key.modifiers == KeyModifiers::CONTROL {
        g.toggle_changes_focus();
        return KeyFlow::Continue;
    }
    // Alt+1/2/3 pick the right-rail tab (context, changes, agents).
    if dashboard_free && key.modifiers == KeyModifiers::ALT {
        use crate::tui::state::RailTab;
        let tab = match key.code {
            KeyCode::Char('1') => Some(RailTab::Context),
            KeyCode::Char('2') => Some(RailTab::Changes),
            KeyCode::Char('3') => Some(RailTab::Agents),
            _ => None,
        };
        if let Some(tab) = tab {
            g.select_rail_tab(tab);
            return KeyFlow::Continue;
        }
    }
    if dashboard_free && g.rail_focus && handle_changes_key(key, g) {
        return KeyFlow::Continue;
    }

    // Ctrl+C, like Claude Code:
    // 1) a turn is running → interrupt it;
    // 2) the /btw panel is open → close it;
    // 3) the prompt has text → clear it (↑ brings it back);
    // 4) otherwise "Press Ctrl-C again to exit" in the footer; a second
    //    press within 2s exits. Ctrl+D on an empty prompt works the same.
    let ctrl =
        key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT);
    let is_ctrl_c = ctrl && matches!(key.code, KeyCode::Char('c'));
    let is_ctrl_d = ctrl
        && matches!(key.code, KeyCode::Char('d'))
        && g.input_buffer.is_empty()
        && !escape_cancels_active_turn(g);
    if is_ctrl_c || is_ctrl_d {
        if is_ctrl_c && escape_cancels_active_turn(g) {
            request_turn_cancel(g, cancel_flag, cmd_tx);
            *ctrl_c_armed_at = None;
        } else if is_ctrl_c && g.btw.is_some() {
            g.btw = None;
            *ctrl_c_armed_at = None;
        } else if is_ctrl_c && !g.input_buffer.is_empty() {
            let text = g.take_input_text();
            if !text.trim().is_empty() && composer_history.last() != Some(&text) {
                composer_history.push(text);
            }
            *composer_history_index = None;
            composer_history_draft.clear();
            *ctrl_c_armed_at = None;
        } else {
            let now = std::time::Instant::now();
            let armed = ctrl_c_armed_at.is_some_and(|t| now.duration_since(t) <= EXIT_WINDOW);
            if armed {
                g.should_exit = true;
                let _ = cmd_tx.send(TuiCmd::Exit);
                return KeyFlow::Exit;
            }
            *ctrl_c_armed_at = Some(now);
            g.exit_hint = Some((
                now + EXIT_WINDOW,
                if is_ctrl_c {
                    "Press Ctrl-C again to exit"
                } else {
                    "Press Ctrl-D again to exit"
                },
            ));
        }
        g.touch_transcript();
        return KeyFlow::Continue;
    } else {
        *ctrl_c_armed_at = None;
        g.exit_hint = None;
    }

    // Force-close modals that aren't rendered in the inline
    // viewport, so their key handlers can't trap input. Modals
    // we DO render (command palette, model/theme/agent/project/
    // session pickers, info, question, approval) are left alone.
    g.anthropic_oauth_modal_open = false;
    g.provider_picker_open = false;
    g.permission_picker_open = false;
    g.branch_picker_open = false;
    g.pins_modal_open = false;
    g.subagent_modal_open = false;
    g.transcript_search_open = false;
    g.composer_history_search_open = false;

    if g.anthropic_oauth_modal_open {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                g.close_anthropic_oauth_modal();
                if g.onboarding_mode {
                    g.open_connect_modal();
                }
            }
            (KeyCode::Enter, _) => {
                let code = g.anthropic_oauth_code_input.trim().to_string();
                let verifier = g.anthropic_oauth_code_verifier.clone();
                if code.is_empty() {
                    g.push_error("[login] paste authorization code, then press Enter".into());
                } else {
                    g.close_anthropic_oauth_modal();
                    let _ = cmd_tx.send(TuiCmd::CompleteAnthropicOAuth {
                        code_verifier: verifier,
                        authorization_code: code,
                    });
                }
            }
            (KeyCode::Backspace, _) => {
                g.anthropic_oauth_code_input.pop();
            }
            (KeyCode::Insert, mods) if mods.contains(KeyModifiers::SHIFT) => {
                match paste_text_from_clipboard() {
                    Ok(pasted) => {
                        let normalized = pasted.replace('\r', "");
                        let value = normalized.trim_end_matches('\n');
                        g.anthropic_oauth_code_input.push_str(value);
                    }
                    Err(e) => g.push_error(format!("[login] clipboard paste failed: {e}")),
                }
            }
            (KeyCode::Char(c), mods)
                if c.eq_ignore_ascii_case(&'v')
                    && mods.contains(KeyModifiers::CONTROL)
                    && !mods.contains(KeyModifiers::ALT) =>
            {
                match paste_text_from_clipboard() {
                    Ok(pasted) => {
                        let normalized = pasted.replace('\r', "");
                        let value = normalized.trim_end_matches('\n');
                        g.anthropic_oauth_code_input.push_str(value);
                    }
                    Err(e) => g.push_error(format!("[login] clipboard paste failed: {e}")),
                }
            }
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                g.anthropic_oauth_code_input.push(c);
            }
            _ => {}
        }
        return KeyFlow::Continue;
    }
    if g.command_palette_open {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                Some(CommandPaletteKey::Cancel)
            }
            (KeyCode::Up, _) => Some(CommandPaletteKey::Up),
            (KeyCode::Down, _) => Some(CommandPaletteKey::Down),
            (KeyCode::Enter, _) => Some(CommandPaletteKey::Accept),
            (KeyCode::Backspace, _) => Some(CommandPaletteKey::Backspace),
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                Some(CommandPaletteKey::Char(c))
            }
            _ => None,
        };
        if let Some(k) = mapped {
            g.apply_command_palette_key(k);
        }
        return KeyFlow::Continue;
    }

    // Info modal (read-only scrollable popup).
    if g.info_modal_open {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                Some(InfoModalKey::Close)
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                Some(InfoModalKey::ScrollUp)
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                Some(InfoModalKey::ScrollDown)
            }
            (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
                Some(InfoModalKey::ScrollLeft)
            }
            (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => {
                Some(InfoModalKey::ScrollRight)
            }
            (KeyCode::Home, _) => Some(InfoModalKey::Home),
            (KeyCode::End, _) => Some(InfoModalKey::End),
            _ => None,
        };
        if let Some(k) = mapped {
            g.apply_info_modal_key(k);
        }
        return KeyFlow::Continue;
    }

    // Model picker popup.
    if g.model_picker_open {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(ModelPickerKey::Cancel),
            (KeyCode::Up, _) => Some(ModelPickerKey::Up),
            (KeyCode::Down, _) => Some(ModelPickerKey::Down),
            (KeyCode::Enter, _) => Some(ModelPickerKey::Accept),
            (KeyCode::Backspace, _) => Some(ModelPickerKey::Backspace),
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                Some(ModelPickerKey::Char(c))
            }
            _ => None,
        };
        if let Some(k) = mapped
            && let Some(action) = g.apply_model_picker_key(k)
        {
            match action {
                ModelPickerAction::SwitchProvider(p) => {
                    let _ = cmd_tx.send(TuiCmd::ApplyModelProvider(p));
                }
                ModelPickerAction::ApplyModel(m) => {
                    let _ = cmd_tx.send(TuiCmd::ApplyModel(m));
                }
            }
        }
        return KeyFlow::Continue;
    }

    // Connect provider (OpenCode-style `/connect`).
    if g.connect_modal_open {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(ConnectModalKey::Cancel),
            (KeyCode::Up, _) => Some(ConnectModalKey::Up),
            (KeyCode::Down, _) => Some(ConnectModalKey::Down),
            (KeyCode::Enter, _) => Some(ConnectModalKey::Accept),
            (KeyCode::Backspace, _) => Some(ConnectModalKey::Backspace),
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                Some(ConnectModalKey::Char(c))
            }
            _ => None,
        };
        if let Some(k) = mapped
            && let Some(action) = g.apply_connect_modal_key(k)
        {
            match action {
                ConnectAction::OAuthLogin(slug) => {
                    let store = AuthStore::load().unwrap_or_default();
                    if oauth_logged_in_for_slug(&store, slug) {
                        if let Some(cmd) = oauth_switch_command_for_slug(slug) {
                            let _ = cmd_tx.send(TuiCmd::Submit(cmd.to_string()));
                        } else {
                            let _ = cmd_tx.send(TuiCmd::Submit(format!("/login {slug}")));
                        }
                    } else {
                        let _ = cmd_tx.send(TuiCmd::Submit(format!("/login {slug}")));
                    }
                }
            }
        }
        return KeyFlow::Continue;
    }

    // Provider picker (settings).
    if g.provider_picker_open {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(ProviderPickerKey::Cancel),
            (KeyCode::Up, _) => Some(ProviderPickerKey::Up),
            (KeyCode::Down, _) => Some(ProviderPickerKey::Down),
            (KeyCode::Enter, _) => Some(ProviderPickerKey::Accept),
            _ => None,
        };
        if let Some(k) = mapped {
            match g.apply_provider_picker_key(k) {
                Some(ProviderPickerOutcome::Apply(p)) => {
                    let _ = cmd_tx.send(TuiCmd::ApplyDefaultProvider(p));
                }
                None => {}
            }
        }
        return KeyFlow::Continue;
    }

    // Project picker keyboard handling.
    if g.project_picker_open {
        use crate::tui::state::ProjectPickerKey;
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(ProjectPickerKey::Cancel),
            (KeyCode::Up, _) => Some(ProjectPickerKey::Up),
            (KeyCode::Down, _) => Some(ProjectPickerKey::Down),
            (KeyCode::Enter, _) => Some(ProjectPickerKey::Accept),
            (KeyCode::Delete, _) | (KeyCode::Backspace, KeyModifiers::CONTROL) => {
                Some(ProjectPickerKey::Remove)
            }
            _ => None,
        };
        if let Some(k) = mapped
            && let Some(idx) = g.apply_project_picker_key(k)
        {
            let _ = cmd_tx.send(TuiCmd::SwitchProject(idx));
        }
        return KeyFlow::Continue;
    }

    // Branch picker keyboard handling.
    if g.branch_picker_open {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(BranchPickerKey::Cancel),
            (KeyCode::Up, _) => Some(BranchPickerKey::Up),
            (KeyCode::Down, _) => Some(BranchPickerKey::Down),
            (KeyCode::Enter, _) => Some(BranchPickerKey::Accept),
            (KeyCode::Backspace, _) => Some(BranchPickerKey::Backspace),
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                Some(BranchPickerKey::Char(c))
            }
            _ => None,
        };
        if let Some(k) = mapped
            && let Some(cmd) = g.apply_branch_picker_key(k)
        {
            let _ = cmd_tx.send(cmd);
        }
        return KeyFlow::Continue;
    }

    // The `/btw` answer panel: Esc/Enter/Space close it, ↑↓ scroll; any
    // other key closes it and goes on to the composer.
    if let Some(view) = g.btw.as_mut() {
        let consumed = match key.code {
            KeyCode::Up => {
                view.scroll = view.scroll.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                view.scroll = view.scroll.saturating_add(1);
                true
            }
            KeyCode::PageUp => {
                view.scroll = view.scroll.saturating_sub(8);
                true
            }
            KeyCode::PageDown => {
                view.scroll = view.scroll.saturating_add(8);
                true
            }
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ') => {
                g.btw = None;
                true
            }
            _ => {
                g.btw = None;
                false
            }
        };
        g.touch_transcript();
        if consumed {
            return KeyFlow::Continue;
        }
    }

    // A decision prompt (sub-agent merge, memory tidy) owns the keys while
    // the composer is empty; typing a message dismisses nothing.
    if g.input_buffer.is_empty()
        && let Some(prompt) = g.active_choice_mut()
    {
        use crate::tui::choice::{ChoiceKey, ChoiceOutcome};
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Up, _) => Some(ChoiceKey::Up),
            (KeyCode::Down, _) => Some(ChoiceKey::Down),
            (KeyCode::Enter, _) => Some(ChoiceKey::Accept),
            (KeyCode::Esc, _) => Some(ChoiceKey::Cancel),
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                Some(ChoiceKey::Char(c))
            }
            _ => None,
        };
        // Any other letter starts a message instead (the prompt waits).
        let outcome = mapped.map(|k| prompt.handle_key(k));
        if let Some(outcome) = outcome
            && !matches!(outcome, ChoiceOutcome::Key(c) if c != 'd')
        {
            match outcome {
                ChoiceOutcome::Chosen(action, index) => {
                    g.choice_prompts.pop_front();
                    let _ = cmd_tx.send(TuiCmd::Choice(action, index));
                }
                ChoiceOutcome::Key(_) => {
                    if let Some(p) = g.choice_prompts.front() {
                        let _ = cmd_tx.send(TuiCmd::ChoiceDetail(p.action.clone()));
                    }
                }
                ChoiceOutcome::Stay => {}
            }
            g.touch_transcript();
            return KeyFlow::Continue;
        }
    }

    // Question modal keyboard handling.
    if g.question_modal_open {
        // The startup "approve all tools?" popup: Esc (or just starting to
        // type) keeps the default approval flow. Typed keys then go to the
        // composer, so a first message is never swallowed by the popup.
        let startup_popup = g
            .active_question
            .as_ref()
            .is_some_and(|q| q.question_id == STARTUP_APPROVE_ALL_QUESTION_ID);
        let typing = matches!(key.code, KeyCode::Char(_))
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
            && !matches!(key.code, KeyCode::Char('j' | 'k' | '1'..='9'));
        if startup_popup && (key.code == KeyCode::Esc || typing) {
            // Esc / typing = the safe choice (Manual).
            let decline = g
                .active_question
                .as_ref()
                .and_then(|q| {
                    q.options
                        .iter()
                        .position(|o| o.id == "manual" || o.id == "keep_default")
                })
                .unwrap_or(0);
            g.question_modal_index = decline;
            if let QuestionModalOutcome::Answer { selection, .. } =
                g.apply_question_modal_key(QuestionModalKey::Accept)
            {
                let _ = cmd_tx.send(TuiCmd::QuestionAnswer(selection));
            }
            if !typing {
                return KeyFlow::Continue;
            }
        } else {
            let mapped = match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => Some(QuestionModalKey::Cancel),
                (KeyCode::Up, _) | (KeyCode::BackTab, _) => Some(QuestionModalKey::Up),
                (KeyCode::Down, _) | (KeyCode::Tab, _) => Some(QuestionModalKey::Down),
                (KeyCode::Enter, _) => Some(QuestionModalKey::Accept),
                (KeyCode::Backspace, _) => Some(QuestionModalKey::Backspace),
                (KeyCode::Char(c), m)
                    if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
                {
                    Some(QuestionModalKey::Char(c))
                }
                _ => None,
            };
            if let Some(k) = mapped
                && let QuestionModalOutcome::Answer {
                    question_id,
                    selection,
                } = g.apply_question_modal_key(k)
            {
                // Route the answer. STARTUP_APPROVE_ALL keeps active_question
                // so its follow-up flow still sees it.
                if question_id != STARTUP_APPROVE_ALL_QUESTION_ID {
                    g.active_question = None;
                }
                if question_id == STARTUP_APPROVE_ALL_QUESTION_ID {
                    let _ = cmd_tx.send(TuiCmd::QuestionAnswer(selection));
                } else if let Some(tx) = question_answer_tx {
                    let _ = tx.send((question_id, selection));
                } else {
                    let _ = cmd_tx.send(TuiCmd::QuestionAnswer(selection));
                }
            }
            return KeyFlow::Continue;
        }
    }

    // Permission picker keyboard handling.
    if g.permission_picker_open {
        const PERM_COUNT: usize = 5;
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                g.close_permission_picker();
            }
            (KeyCode::Up, _) => {
                g.permission_picker_index = g.permission_picker_index.saturating_sub(1);
            }
            (KeyCode::Down, _) => {
                g.permission_picker_index = (g.permission_picker_index + 1).min(PERM_COUNT - 1);
            }
            (KeyCode::Enter, _) => {
                let idx = g.permission_picker_index;
                g.close_permission_picker();
                let _ = cmd_tx.send(TuiCmd::ApplyPermission(idx));
            }
            _ => {}
        }
        return KeyFlow::Continue;
    }

    // Theme picker keyboard handling.
    if g.theme_picker_open {
        let count = g.theme_picker_entries.len();
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                // Undo the live preview.
                if let Some(original) = g.theme_picker_original.take() {
                    theme::set_by_name(Some(&original));
                }
                g.close_theme_picker();
                g.touch_transcript();
            }
            (KeyCode::Char(c @ '1'..='9'), _) => {
                let idx = (c as usize) - ('1' as usize);
                if let Some(n) = g.theme_picker_entries.get(idx).cloned() {
                    g.theme_picker_original = None;
                    g.close_theme_picker();
                    theme::set_by_name(Some(&n));
                    g.touch_transcript();
                    let _ = cmd_tx.send(TuiCmd::ApplyTheme(n));
                }
            }
            (KeyCode::Up, _) => {
                if count > 0 {
                    g.theme_picker_index = g.theme_picker_index.saturating_sub(1);
                    // Live preview: apply selected theme immediately.
                    if let Some(name) = g.theme_picker_entries.get(g.theme_picker_index) {
                        theme::set_by_name(Some(name));
                    }
                    g.touch_transcript();
                }
            }
            (KeyCode::Down, _) => {
                if count > 0 {
                    g.theme_picker_index = (g.theme_picker_index + 1).min(count - 1);
                    if let Some(name) = g.theme_picker_entries.get(g.theme_picker_index) {
                        theme::set_by_name(Some(name));
                    }
                    g.touch_transcript();
                }
            }
            (KeyCode::Enter, _) => {
                let name = g.theme_picker_entries.get(g.theme_picker_index).cloned();
                g.theme_picker_original = None;
                g.close_theme_picker();
                if let Some(n) = name {
                    let _ = cmd_tx.send(TuiCmd::ApplyTheme(n));
                }
            }
            _ => {}
        }
        return KeyFlow::Continue;
    }

    // Agent profile picker keyboard handling.
    if g.agent_picker_open {
        const AGENT_COUNT: usize = 5;
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                g.close_agent_picker();
            }
            (KeyCode::Up, _) => {
                g.agent_picker_index = g.agent_picker_index.saturating_sub(1);
            }
            (KeyCode::Down, _) => {
                g.agent_picker_index = (g.agent_picker_index + 1).min(AGENT_COUNT - 1);
            }
            (KeyCode::Enter, _) => {
                let idx = g.agent_picker_index;
                g.close_agent_picker();
                let _ = cmd_tx.send(TuiCmd::SwitchAgent(idx));
            }
            _ => {}
        }
        return KeyFlow::Continue;
    }

    // Backtrack picker keyboard handling (Esc while idle).
    if g.backtrack_open {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(BacktrackKey::Cancel),
            (KeyCode::Up, _) => Some(BacktrackKey::Up),
            (KeyCode::Down, _) => Some(BacktrackKey::Down),
            (KeyCode::Enter, _) => Some(BacktrackKey::Accept),
            _ => None,
        };
        if let Some(k) = mapped {
            g.apply_backtrack_key(k);
        }
        return KeyFlow::Continue;
    }

    // Plan → build prompt: 1 auto-accept edits · 2 approve edits · 3 keep planning.
    if let Some(index) = g.plan_exit_index {
        let pick = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(2),
            (KeyCode::Up, _) => {
                g.plan_exit_index = Some(index.saturating_sub(1));
                None
            }
            (KeyCode::Down, _) => {
                g.plan_exit_index = Some((index + 1).min(2));
                None
            }
            (KeyCode::Enter, _) => Some(index),
            (KeyCode::Char(c @ '1'..='3'), _) => Some((c as usize) - ('1' as usize)),
            _ => None,
        };
        if let Some(pick) = pick {
            g.plan_exit_index = None;
            if pick < 2 {
                let _ = cmd_tx.send(TuiCmd::ExitPlan {
                    accept_edits: pick == 0,
                });
            }
        }
        return KeyFlow::Continue;
    }

    // Rewind scope step: 1 code+conversation · 2 conversation · 3 code.
    if g.rewind_pending.is_some() {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(BacktrackKey::Cancel),
            (KeyCode::Up, _) => Some(BacktrackKey::Up),
            (KeyCode::Down, _) => Some(BacktrackKey::Down),
            (KeyCode::Enter, _) => Some(BacktrackKey::Accept),
            (KeyCode::Char(c @ '1'..='3'), _) => {
                if let Some(p) = g.rewind_pending.as_mut() {
                    p.index = (c as usize) - ('1' as usize);
                }
                Some(BacktrackKey::Accept)
            }
            _ => None,
        };
        if let Some(k) = mapped
            && let Some(rewind) = g.apply_rewind_scope_key(k)
        {
            let _ = cmd_tx.send(TuiCmd::Backtrack {
                user_index_from_end: rewind.user_index_from_end,
                text: rewind.text,
                scope: rewind.scope,
            });
        }
        return KeyFlow::Continue;
    }

    // Session picker keyboard handling.
    if g.session_picker_open {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => Some(SessionPickerKey::Cancel),
            (KeyCode::Up, _) => Some(SessionPickerKey::Up),
            (KeyCode::Down, _) => Some(SessionPickerKey::Down),
            (KeyCode::Enter, _) => Some(SessionPickerKey::Accept),
            (KeyCode::Backspace, _) => Some(SessionPickerKey::Backspace),
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                Some(SessionPickerKey::Char(c))
            }
            _ => None,
        };
        if let Some(k) = mapped
            && let Some(id) = g.apply_session_picker_key(k)
        {
            let _ = cmd_tx.send(TuiCmd::ResumeSession(id));
        }
        return KeyFlow::Continue;
    }

    if g.pins_modal_open {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                Some(PinsModalKey::Close)
            }
            (KeyCode::Up, _) => Some(PinsModalKey::Up),
            (KeyCode::Down, _) => Some(PinsModalKey::Down),
            (KeyCode::Backspace, _) => Some(PinsModalKey::Delete),
            (KeyCode::Enter, _) => Some(PinsModalKey::Accept),
            (KeyCode::F(6), _) => Some(PinsModalKey::Copy),
            _ => None,
        };
        if let Some(k) = mapped
            && g.apply_pins_modal_key(k)
        {
            // Copy requested: clipboard write + status line stay in the loop.
            let msg = if let Some(note) = g.pinned_notes.get(g.pins_modal_index) {
                match copy_to_clipboard(&note.body) {
                    Ok(_) => "Copied pinned note".to_string(),
                    Err(e) => format!("Clipboard copy failed: {e}"),
                }
            } else {
                "No pinned note selected".to_string()
            };
            g.push_block(DisplayBlock::System(msg));
            g.touch_transcript();
            g.transcript_follow_tail = true;
            g.notification_count = 0;
        }
        return KeyFlow::Continue;
    }

    if g.subagent_modal_open {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('g'), KeyModifiers::CONTROL) => {
                g.close_subagent_modal();
            }
            (KeyCode::Up, _) => {
                g.subagent_modal_index = g.subagent_modal_index.saturating_sub(1);
            }
            (KeyCode::Down, _) => {
                if !g.subagents.is_empty() {
                    g.subagent_modal_index =
                        (g.subagent_modal_index + 1).min(g.subagents.len().saturating_sub(1));
                }
            }
            (KeyCode::Enter, _) => {
                if let Some(row) = g.subagents.get(g.subagent_modal_index) {
                    let id = row.id.clone();
                    g.close_subagent_modal();
                    let _ = cmd_tx.send(TuiCmd::ResumeSession(id));
                }
            }
            _ => {}
        }
        return KeyFlow::Continue;
    }

    if g.transcript_search_open {
        let sz = terminal_size;
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                g.close_transcript_search();
            }
            (KeyCode::Backspace, _) => {
                g.transcript_search_query.pop();
                g.transcript_search_index = 0;
            }
            (KeyCode::Up, _) => {
                let inner_w = sz.map(|s| s.width.saturating_sub(2)).unwrap_or(80);
                let (lines, _, _) = transcript_cache.get_or_rebuild(g, inner_w);
                let matches = transcript_search_matches(lines, &g.transcript_search_query);
                if !matches.is_empty() {
                    if g.transcript_search_index == 0 {
                        g.transcript_search_index = matches.len().saturating_sub(1);
                    } else {
                        g.transcript_search_index = g.transcript_search_index.saturating_sub(1);
                    }
                }
            }
            (KeyCode::Down, _) | (KeyCode::Enter, _) => {
                let inner_w = sz.map(|s| s.width.saturating_sub(2)).unwrap_or(80);
                let (lines, _, _) = transcript_cache.get_or_rebuild(g, inner_w);
                let matches = transcript_search_matches(lines, &g.transcript_search_query);
                if !matches.is_empty() {
                    g.transcript_search_index = (g.transcript_search_index + 1) % matches.len();
                }
            }
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                g.transcript_search_query.push(c);
                g.transcript_search_index = 0;
            }
            _ => {}
        }
        return KeyFlow::Continue;
    }

    if g.composer_history_search_open {
        let mapped = match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                Some(HistorySearchKey::Cancel)
            }
            (KeyCode::Backspace, _) => Some(HistorySearchKey::Backspace),
            (KeyCode::Up, _) => Some(HistorySearchKey::Up),
            (KeyCode::Down, _) => Some(HistorySearchKey::Down),
            (KeyCode::Enter, _) => Some(HistorySearchKey::Accept),
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                Some(HistorySearchKey::Char(c))
            }
            _ => None,
        };
        if let Some(k) = mapped {
            g.apply_history_search_key(k, composer_history);
        }
        return KeyFlow::Continue;
    }

    // Ctrl+X leader key dispatch.
    if g.leader_pending {
        g.leader_pending = false;
        match key.code {
            KeyCode::Char('m') | KeyCode::Char('M') => {
                let _ = cmd_tx.send(TuiCmd::OpenModelPicker);
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                let _ = cmd_tx.send(TuiCmd::OpenEditor);
            }
            KeyCode::Char('l') | KeyCode::Char('L') => {
                let _ = cmd_tx.send(TuiCmd::OpenSessions);
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                let _ = cmd_tx.send(TuiCmd::NewSession);
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                let _ = cmd_tx.send(TuiCmd::RunCompact);
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                let _ = cmd_tx.send(TuiCmd::OpenStatus);
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                let _ = cmd_tx.send(TuiCmd::OpenStatus);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                let _ = cmd_tx.send(TuiCmd::OpenAgentPicker);
            }
            KeyCode::Char('h') | KeyCode::Char('H') => {
                let _ = cmd_tx.send(TuiCmd::OpenHelp);
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                g.should_exit = true;
                let _ = cmd_tx.send(TuiCmd::Exit);
                return KeyFlow::Exit;
            }
            KeyCode::Char('f') => {
                let toggled = g.toggle_last_tool_block();
                let msg = if toggled {
                    "Toggled latest tool block (collapsed/expanded)".to_string()
                } else {
                    "No tool blocks to toggle yet".to_string()
                };
                g.push_block(DisplayBlock::System(msg));
                g.touch_transcript();
            }
            KeyCode::Char('F') => {
                g.toggle_all_tool_blocks();
                let state_msg = if g.all_tools_collapsed {
                    "Collapsed all tool blocks (Ctrl+X F to expand)"
                } else {
                    "Expanded all tool blocks"
                };
                g.push_block(DisplayBlock::System(state_msg.into()));
                g.touch_transcript();
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                let _ = cmd_tx.send(TuiCmd::OpenProjectPicker);
            }
            KeyCode::Char('v') | KeyCode::Char('V') => {
                g.transcript_overlay_open = true;
                g.transcript_overlay_scroll = usize::MAX; // start at bottom
            }
            _ => {}
        }
        return KeyFlow::Continue;
    }

    // `/vim`: NORMAL-mode keys edit the prompt; Esc still interrupts a
    // running turn first.
    if !(key.code == KeyCode::Esc && escape_cancels_active_turn(g))
        && super::vim::handle_vim_key(key, g)
    {
        return KeyFlow::Continue;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) if escape_cancels_active_turn(g) => {
            request_turn_cancel(g, cancel_flag, cmd_tx);
        }
        // Idle Esc twice with an empty composer: open the rewind picker
        // (restore code and/or conversation to a past message).
        (KeyCode::Esc, _) if !g.busy && g.input_buffer.is_empty() => {
            let now = std::time::Instant::now();
            let double = g
                .last_esc_at
                .is_some_and(|t| now.duration_since(t) <= DOUBLE_ESC_WINDOW);
            if double {
                g.last_esc_at = None;
                g.toast = None;
                g.open_backtrack();
            } else {
                g.last_esc_at = Some(now);
                g.show_toast("Esc again to rewind", crate::tui::state::ToastKind::Info);
            }
        }
        (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
            g.should_exit = true;
            let _ = cmd_tx.send(TuiCmd::Exit);
            return KeyFlow::Exit;
        }
        (KeyCode::F(6), _) => {
            let msg = if let Some(text) = latest_copyable_text(g) {
                match copy_to_clipboard(&text) {
                    Ok(_) => "Copied latest assistant response".to_string(),
                    Err(e) => format!("Clipboard copy failed: {e}"),
                }
            } else {
                "Nothing to copy yet".to_string()
            };
            g.push_block(DisplayBlock::System(msg));
            g.touch_transcript();
            g.transcript_follow_tail = true;
            g.notification_count = 0;
        }
        (KeyCode::F(7), _) => {
            let detail = g.blocks.iter().rev().find_map(|b| {
                if let DisplayBlock::ToolDone { detail, .. } = b {
                    if !detail.trim().is_empty() {
                        Some(detail.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });
            let msg = if let Some(text) = detail {
                match copy_to_clipboard(&text) {
                    Ok(_) => "Copied last tool output".to_string(),
                    Err(e) => format!("Clipboard copy failed: {e}"),
                }
            } else {
                "No tool output to copy yet".to_string()
            };
            g.push_block(DisplayBlock::System(msg));
            g.touch_transcript();
            g.transcript_follow_tail = true;
            g.notification_count = 0;
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            // Handled earlier in Event::Key with double-press semantics.
        }
        (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
            g.blocks.clear();
            g.block_timestamps.clear();
            g.flushed_block_count = 0;
            g.streaming_assistant = None;
            g.streaming_thinking = None;
            g.request_clear = true;
            g.touch_transcript();
            g.scroll_lines = 0;
            g.transcript_follow_tail = true;
            g.notification_count = 0;
            *composer_history_index = None;
            composer_history_draft.clear();
        }
        (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
            g.command_palette_open = true;
            g.command_palette_query.clear();
            g.palette_index = 0;
        }
        (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
            // Toggle reasoning/thinking, then re-render ALL history
            // (purge + re-flush) so already-scrolled content reflects
            // the new state — not just the live pane.
            g.thinking_expanded = !g.thinking_expanded;
            g.request_clear = true;
            g.touch_transcript();
        }
        (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
            // Fold / unfold ALL tool output across the whole
            // transcript (re-flushes flushed history too).
            g.toggle_all_tool_blocks();
            g.request_clear = true;
            g.touch_transcript();
        }
        (KeyCode::Char('g'), KeyModifiers::CONTROL) => {
            if g.subagents.is_empty() {
                g.push_block(DisplayBlock::System(
                    "No sub-agents to focus right now.".into(),
                ));
                g.touch_transcript();
                g.transcript_follow_tail = true;
                g.notification_count = 0;
            } else {
                g.open_subagent_modal();
            }
        }
        (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
            // When composer has text and no approval is pending,
            // Ctrl+K kills to end of line (Emacs-style).
            // Otherwise it pins the latest assistant message.
            let has_text = !g.input_buffer.trim().is_empty();
            if has_text && g.active_approval.is_none() {
                g.kill_input_to_end();
            } else {
                let msg = if let Some(note) = latest_pinnable_note(g) {
                    let exists = g.pinned_notes.iter().any(|n| n.body == note.body);
                    if !exists {
                        g.pinned_notes.insert(0, note);
                        if g.pinned_notes.len() > 20 {
                            g.pinned_notes.pop();
                        }
                        "Pinned latest message".to_string()
                    } else {
                        "Latest message is already pinned".to_string()
                    }
                } else {
                    "Nothing pinnable yet".to_string()
                };
                g.push_block(DisplayBlock::System(msg));
                g.touch_transcript();
                g.transcript_follow_tail = true;
                g.notification_count = 0;
            }
        }
        (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
            g.open_transcript_search();
        }
        (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
            g.composer_history_search_open = true;
            g.composer_history_search_query.clear();
            g.composer_history_search_index = 0;
        }
        (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
            // Claude Code: Ctrl+B moves a running shell command to the
            // background; otherwise it opens the status view.
            let shell_running = g.blocks.iter().rev().take(64).any(
                |b| matches!(b, DisplayBlock::ToolRunning { name, .. } if name == "execute_bash"),
            );
            if shell_running {
                dcode_ai_runtime::bash_tool::background_signal().notify_waiters();
                g.show_toast(
                    "Moving the command to the background…",
                    crate::tui::state::ToastKind::Info,
                );
            } else {
                let _ = cmd_tx.send(TuiCmd::OpenStatus);
            }
        }
        (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
            g.leader_pending = true;
        }
        // Alt+V is the reliable image-paste binding on Windows,
        // where the terminal itself owns Ctrl+V (and sends no
        // event at all when the clipboard holds only an image).
        (KeyCode::Char('v'), mods)
            if (mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT))
                || mods == KeyModifiers::ALT =>
        {
            if g.active_approval.is_some() || g.active_question.is_some() {
                return KeyFlow::Continue;
            }
            // Reading the clipboard can take a moment (WSL goes through
            // PowerShell): the main loop runs it in the background.
            g.paste_request = Some(true);
        }
        (KeyCode::Char('i'), KeyModifiers::CONTROL) => {
            *composer_history_index = None;
            composer_history_draft.clear();
            g.insert_input_char('\n');
        }
        // Shift+Tab cycles permission modes (Claude-style).
        (KeyCode::BackTab, _) => {
            let _ = cmd_tx.send(TuiCmd::CyclePermissionMode);
        }
        (KeyCode::Tab, mods)
            if mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) =>
        {
            *composer_history_index = None;
            composer_history_draft.clear();
            g.insert_input_char('\n');
        }
        // Tab while a turn runs: queue the message for after the turn.
        (KeyCode::Tab, KeyModifiers::NONE)
            if (g.busy || g.current_busy_state.is_active())
                && !g.input_buffer.trim().is_empty()
                && !slash_panel_visible(&g.input_buffer)
                && apply_selected_at_completion(
                    workspace_files,
                    &g.input_buffer,
                    g.cursor_char_idx,
                    g.at_menu_index,
                    false,
                )
                .is_none() =>
        {
            let line = g.take_input_text();
            let expanded = expand_paste_tokens(&line, &g.paste_store);
            g.paste_store.clear();
            g.paste_counter = 0;
            *composer_history_index = None;
            composer_history_draft.clear();
            g.followup_queue.push_back(expanded);
        }
        (KeyCode::Tab, _) => {
            if let Some((buf, cidx)) = apply_selected_at_completion(
                workspace_files,
                &g.input_buffer,
                g.cursor_char_idx,
                g.at_menu_index,
                false,
            ) {
                g.set_input_text_with_cursor(buf, cidx);
            } else {
                let slash_filtered = filter_slash_entries(slash_entries, &g.input_buffer);
                if !slash_filtered.is_empty() && slash_panel_visible(&g.input_buffer) {
                    let pick = g.slash_menu_index % slash_filtered.len();
                    g.set_input_text(slash_filtered[pick].command_str());
                } else {
                    let _ = cmd_tx.send(TuiCmd::CycleAgent);
                }
            }
        }
        (KeyCode::F(1), _) => {
            g.open_info_modal("Keybindings", keybindings_cheatsheet());
        }
        // `?` on an empty prompt shows the shortcuts (footer hint).
        (KeyCode::Char('?'), KeyModifiers::NONE | KeyModifiers::SHIFT)
            if g.input_buffer.is_empty() && !g.busy =>
        {
            g.open_info_modal("Shortcuts", keybindings_cheatsheet());
        }
        (KeyCode::F(2), KeyModifiers::NONE) => {
            let _ = cmd_tx.send(TuiCmd::CycleModel(true));
        }
        (KeyCode::F(2), KeyModifiers::SHIFT) => {
            let _ = cmd_tx.send(TuiCmd::CycleModel(false));
        }
        // Transcript zoom: Ctrl+Plus / Ctrl+Minus / Ctrl+0
        // to avoid stealing -, +, = from the composer.
        (KeyCode::Char('+'), KeyModifiers::CONTROL)
        | (KeyCode::Char('='), KeyModifiers::CONTROL) => {
            g.transcript_zoom_offset = (g.transcript_zoom_offset + 4).min(40);
            g.touch_transcript();
        }
        (KeyCode::Char('-'), KeyModifiers::CONTROL) => {
            g.transcript_zoom_offset = (g.transcript_zoom_offset - 4).max(-20);
            g.touch_transcript();
        }
        (KeyCode::Char('0'), KeyModifiers::CONTROL) => {
            g.transcript_zoom_offset = 0;
            g.touch_transcript();
        }
        (KeyCode::Up, KeyModifiers::NONE) if g.active_approval.is_some() => {
            g.approval_option_index = g.approval_option_index.saturating_sub(1);
            g.touch_transcript();
            return KeyFlow::Continue;
        }
        (KeyCode::Down, KeyModifiers::NONE) if g.active_approval.is_some() => {
            g.approval_option_index = (g.approval_option_index + 1).min(2);
            g.touch_transcript();
            return KeyFlow::Continue;
        }
        (KeyCode::Char('y'), KeyModifiers::NONE) | (KeyCode::Char('Y'), KeyModifiers::NONE)
            if g.active_approval.is_some() =>
        {
            if let Some(req) = g.active_approval.clone() {
                let call_id = req.call_id.clone();
                g.active_approval = None;
                g.approval_option_index = 0;
                g.clear_input();
                if let Some(tx) = approval_answer_tx {
                    let _ = tx.send(ApprovalAnswer::Verdict {
                        call_id,
                        approved: true,
                    });
                }
                return KeyFlow::Continue;
            }
        }
        (KeyCode::Char('n'), KeyModifiers::NONE) | (KeyCode::Char('N'), KeyModifiers::NONE)
            if g.active_approval.is_some() =>
        {
            if let Some(req) = g.active_approval.clone() {
                let call_id = req.call_id.clone();
                g.active_approval = None;
                g.approval_option_index = 0;
                g.clear_input();
                if let Some(tx) = approval_answer_tx {
                    let _ = tx.send(ApprovalAnswer::Verdict {
                        call_id,
                        approved: false,
                    });
                }
                return KeyFlow::Continue;
            }
        }
        (KeyCode::Char('a'), KeyModifiers::NONE) | (KeyCode::Char('A'), KeyModifiers::NONE)
            if g.active_approval.is_some() =>
        {
            if let Some(req) = g.active_approval.clone() {
                let pattern = req.allow_pattern();
                let call_id = req.call_id.clone();
                g.active_approval = None;
                g.approval_option_index = 0;
                g.clear_input();
                g.push_block(DisplayBlock::System(format!("Always allowing: {pattern}")));
                g.touch_transcript();
                if let Some(tx) = approval_answer_tx {
                    let _ = tx.send(ApprovalAnswer::AllowPattern { call_id, pattern });
                }
                return KeyFlow::Continue;
            }
        }
        // Toggle hunk-selection mode for file-writing approvals.
        (KeyCode::Char('h'), KeyModifiers::NONE)
            if g.active_approval.is_some() && !g.approval_hunk_mode =>
        {
            if let Some(req) = g.active_approval.as_ref() {
                let hunks = extract_approval_hunks(&req.tool, &req.input);
                if !hunks.is_empty() {
                    g.approval_hunk_selection = vec![true; hunks.len()];
                    g.approval_hunk_cursor = 0;
                    g.approval_hunk_mode = true;
                    g.touch_transcript();
                }
            }
            return KeyFlow::Continue;
        }
        // Hunk mode navigation.
        (KeyCode::Up, KeyModifiers::NONE) if g.approval_hunk_mode => {
            g.approval_hunk_cursor = g.approval_hunk_cursor.saturating_sub(1);
            g.touch_transcript();
            return KeyFlow::Continue;
        }
        (KeyCode::Down, KeyModifiers::NONE) if g.approval_hunk_mode => {
            let max = g.approval_hunk_selection.len().saturating_sub(1);
            g.approval_hunk_cursor = (g.approval_hunk_cursor + 1).min(max);
            g.touch_transcript();
            return KeyFlow::Continue;
        }
        // Toggle current hunk.
        (KeyCode::Char(' '), KeyModifiers::NONE) if g.approval_hunk_mode => {
            let idx = g.approval_hunk_cursor;
            if let Some(val) = g.approval_hunk_selection.get_mut(idx) {
                *val = !*val;
            }
            g.touch_transcript();
            return KeyFlow::Continue;
        }
        // Accept current hunk.
        (KeyCode::Char('y'), KeyModifiers::NONE) if g.approval_hunk_mode => {
            let idx = g.approval_hunk_cursor;
            if let Some(val) = g.approval_hunk_selection.get_mut(idx) {
                *val = true;
            }
            let max = g.approval_hunk_selection.len().saturating_sub(1);
            g.approval_hunk_cursor = (g.approval_hunk_cursor + 1).min(max);
            g.touch_transcript();
            return KeyFlow::Continue;
        }
        // Reject current hunk.
        (KeyCode::Char('n'), KeyModifiers::NONE) if g.approval_hunk_mode => {
            let idx = g.approval_hunk_cursor;
            if let Some(val) = g.approval_hunk_selection.get_mut(idx) {
                *val = false;
            }
            let max = g.approval_hunk_selection.len().saturating_sub(1);
            g.approval_hunk_cursor = (g.approval_hunk_cursor + 1).min(max);
            g.touch_transcript();
            return KeyFlow::Continue;
        }
        // Exit hunk mode without applying.
        (KeyCode::Esc, _) if g.approval_hunk_mode => {
            g.approval_hunk_mode = false;
            g.approval_hunk_selection.clear();
            g.touch_transcript();
            return KeyFlow::Continue;
        }
        (KeyCode::Char('y'), KeyModifiers::CONTROL) => {
            if let Some(req) = g.active_approval.clone() {
                let call_id = req.call_id.clone();
                g.active_approval = None;
                g.approval_option_index = 0;
                g.clear_input();
                if let Some(tx) = approval_answer_tx {
                    let _ = tx.send(ApprovalAnswer::Verdict {
                        call_id,
                        approved: true,
                    });
                }
                return KeyFlow::Continue;
            } else {
                // Yank from kill ring when not in an approval prompt.
                g.yank_input();
            }
        }
        (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
            if let Some(req) = g.active_approval.clone() {
                let call_id = req.call_id.clone();
                g.active_approval = None;
                g.approval_option_index = 0;
                g.clear_input();
                if let Some(tx) = approval_answer_tx {
                    let _ = tx.send(ApprovalAnswer::Verdict {
                        call_id,
                        approved: false,
                    });
                }
                return KeyFlow::Continue;
            }
        }
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            if let Some(req) = g.active_approval.clone() {
                let pattern = req.allow_pattern();
                let call_id = req.call_id.clone();
                g.active_approval = None;
                g.approval_option_index = 0;
                g.clear_input();
                g.push_block(DisplayBlock::System(format!("Always allowing: {pattern}")));
                g.touch_transcript();
                if let Some(tx) = approval_answer_tx {
                    let _ = tx.send(ApprovalAnswer::AllowPattern { call_id, pattern });
                }
                return KeyFlow::Continue;
            } else {
                // No approval pending: Emacs-style kill to start of line.
                g.kill_input_to_start();
            }
        }
        (KeyCode::Enter, mods) => {
            if mods.contains(KeyModifiers::SHIFT)
                && !mods.contains(KeyModifiers::CONTROL)
                && !mods.contains(KeyModifiers::ALT)
            {
                *composer_history_index = None;
                composer_history_draft.clear();
                g.insert_input_char('\n');
                return KeyFlow::Continue;
            }
            // `\` then Enter = newline, in any terminal (Claude Code's
            // fallback where Shift+Enter can't be told apart).
            if g.cursor_char_idx > 0
                && g.input_buffer.chars().nth(g.cursor_char_idx - 1) == Some('\\')
            {
                g.backspace_input();
                g.insert_input_char('\n');
                return KeyFlow::Continue;
            }
            // `/btw <question>` is answered on the side, even mid-turn.
            if let Some(rest) = g.input_buffer.trim_start().strip_prefix("/btw")
                && (rest.is_empty() || rest.starts_with(char::is_whitespace))
            {
                let question = rest.to_string();
                g.take_input_text();
                g.ask_btw(&question);
                return KeyFlow::Continue;
            }
            if let Some((buf, cidx)) = apply_selected_at_completion(
                workspace_files,
                &g.input_buffer,
                g.cursor_char_idx,
                g.at_menu_index,
                true,
            ) {
                g.set_input_text_with_cursor(buf, cidx);
                g.at_menu_index = 0;
                return KeyFlow::Continue;
            }
            // Slash completion (Codex-style): if the slash panel is
            // open and the typed text is a *partial* command, Enter
            // completes to the highlighted command before running —
            // so `/age` + Enter runs `/agent`, not an unknown command.
            if slash_panel_visible(&g.input_buffer) {
                let typed = g.input_buffer.trim().to_string();
                let is_exact = slash_entries.iter().any(|e| e.command_str() == typed);
                if !is_exact {
                    let filtered = filter_slash_entries(slash_entries, &g.input_buffer);
                    if !filtered.is_empty() {
                        let pick = g.slash_menu_index.min(filtered.len() - 1);
                        let cmd = filtered[pick].command_str();
                        g.set_input_text(cmd);
                    }
                }
            }
            let line = g.take_input_text();
            g.slash_menu_index = 0;
            let active_approval = g.active_approval.clone();
            let active_q = g.active_question.clone();
            if let Some(req) = active_approval {
                let t = line.trim();
                if t.is_empty() {
                    let call_id = req.call_id.clone();

                    // If in hunk mode, apply partial selection.
                    if g.approval_hunk_mode && !g.approval_hunk_selection.is_empty() {
                        let modified_input = build_hunk_modified_input(
                            &req.tool,
                            &req.input,
                            &g.approval_hunk_selection,
                        );
                        let all_selected = g.approval_hunk_selection.iter().all(|&s| s);
                        g.approval_hunk_mode = false;
                        g.approval_hunk_selection.clear();
                        g.active_approval = None;
                        g.approval_option_index = 0;
                        if let Some(tx) = approval_answer_tx {
                            if all_selected {
                                let _ = tx.send(ApprovalAnswer::Verdict {
                                    call_id,
                                    approved: true,
                                });
                            } else if let Some(modified) = modified_input {
                                let _ = tx.send(ApprovalAnswer::ModifiedApproval {
                                    call_id,
                                    modified_input: modified,
                                });
                            } else {
                                let _ = tx.send(ApprovalAnswer::Verdict {
                                    call_id,
                                    approved: true,
                                });
                            }
                        }
                        return KeyFlow::Continue;
                    }

                    let selection = g.approval_option_index.min(2);
                    g.active_approval = None;
                    g.approval_option_index = 0;
                    if let Some(tx) = approval_answer_tx {
                        match selection {
                            0 => {
                                let _ = tx.send(ApprovalAnswer::Verdict {
                                    call_id,
                                    approved: true,
                                });
                            }
                            1 => {
                                let pattern = req.allow_pattern();
                                let _ = tx.send(ApprovalAnswer::AllowPattern { call_id, pattern });
                            }
                            _ => {
                                let _ = tx.send(ApprovalAnswer::Verdict {
                                    call_id,
                                    approved: false,
                                });
                            }
                        }
                    } else {
                        let _ = cmd_tx.send(TuiCmd::CancelTurn);
                    }
                    return KeyFlow::Continue;
                }
                if t.starts_with('/') {
                    let lower = t.to_lowercase();
                    let slash_verdict = match lower.as_str() {
                        "/approve" | "/y" | "/yes" | "/ok" => Some(true),
                        "/deny" | "/n" | "/no" => Some(false),
                        _ => None,
                    };
                    if let Some(approved) = slash_verdict {
                        let call_id = req.call_id.clone();
                        g.active_approval = None;
                        g.approval_option_index = 0;
                        if let Some(tx) = approval_answer_tx {
                            let _ = tx.send(ApprovalAnswer::Verdict { call_id, approved });
                        } else {
                            let _ = cmd_tx.send(TuiCmd::CancelTurn);
                        }
                        return KeyFlow::Continue;
                    }
                    let _ = cmd_tx.send(TuiCmd::Submit(line));
                    return KeyFlow::Continue;
                }
                if let Some(approved) = parse_approval_verdict(t) {
                    let call_id = req.call_id.clone();
                    g.active_approval = None;
                    g.approval_option_index = 0;
                    if let Some(tx) = approval_answer_tx {
                        let _ = tx.send(ApprovalAnswer::Verdict { call_id, approved });
                    } else {
                        let _ = cmd_tx.send(TuiCmd::CancelTurn);
                    }
                    return KeyFlow::Continue;
                }
                g.push_block(DisplayBlock::System(
                    "Could not parse approval — try y, n, yes, no, ok, deny, or Ctrl+Y / Ctrl+N."
                        .into(),
                ));
                g.touch_transcript();
                return KeyFlow::Continue;
            }
            if let Some(ref q) = active_q {
                let t = line.trim();
                // `/auto-answer` must go through the side channel: `run_turn` is often
                // blocked on this question, so `cmd_rx` is not polled for Submit.
                if t == "/auto-answer" {
                    let qid = q.question_id.clone();
                    if qid == STARTUP_APPROVE_ALL_QUESTION_ID {
                        let _ = cmd_tx.send(TuiCmd::QuestionAnswer(QuestionSelection::Suggested));
                    } else if let Some(tx) = question_answer_tx {
                        let _ = tx.send((qid, QuestionSelection::Suggested));
                    } else {
                        let _ = cmd_tx.send(TuiCmd::QuestionAnswer(QuestionSelection::Suggested));
                    }
                    return KeyFlow::Continue;
                }
                if t.starts_with('/') {
                    let _ = cmd_tx.send(TuiCmd::Submit(line));
                    return KeyFlow::Continue;
                }
                if let Some(sel) = parse_tui_question_answer(&line, q) {
                    let qid = q.question_id.clone();
                    if qid == STARTUP_APPROVE_ALL_QUESTION_ID {
                        let _ = cmd_tx.send(TuiCmd::QuestionAnswer(sel));
                    } else if let Some(tx) = question_answer_tx {
                        let _ = tx.send((qid, sel));
                    } else {
                        let _ = cmd_tx.send(TuiCmd::QuestionAnswer(sel));
                    }
                    return KeyFlow::Continue;
                }
                g.push_block(DisplayBlock::System(
                    "Invalid answer: use Enter/0 for suggested, 1–n for an option, or custom text."
                        .into(),
                ));
                g.touch_transcript();
                return KeyFlow::Continue;
            }
            let expanded = expand_paste_tokens(&line, &g.paste_store);
            g.paste_store.clear();
            g.paste_counter = 0;
            if !expanded.trim().is_empty() && composer_history.last() != Some(&expanded) {
                composer_history.push(expanded.clone());
                if composer_history.len() > 200 {
                    composer_history.remove(0);
                }
            }
            *composer_history_index = None;
            composer_history_draft.clear();
            let busy_turn = g.busy || g.current_busy_state.is_active();
            if busy_turn {
                // Enter: the agent reads it at its next step. Alt+Enter, and
                // commands (`/…`, `!…`, `#…`), wait for the turn to end.
                let command = matches!(expanded.trim_start().chars().next(), Some('/' | '!' | '#'));
                if mods.contains(KeyModifiers::ALT) || command || !g.steer(expanded.clone()) {
                    g.followup_queue.push_back(expanded);
                }
                return KeyFlow::Continue;
            }
            if let Err(e) = stage_pasted_image_paths(g, &expanded, false) {
                g.push_error(format!("[image] {e}"));
            }
            let _ = cmd_tx.send(TuiCmd::Submit(expanded));
        }
        (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
            *composer_history_index = None;
            composer_history_draft.clear();
            g.insert_input_char('\n');
        }
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
            g.move_input_home_line();
        }
        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
            g.move_input_end_line();
        }
        // ── Word movement (Ctrl+←/→  and  Alt+B/F) ──────────
        (KeyCode::Left, mods) if mods.contains(KeyModifiers::CONTROL) => {
            g.move_input_word_backward();
        }
        (KeyCode::Right, mods) if mods.contains(KeyModifiers::CONTROL) => {
            g.move_input_word_forward();
        }
        (KeyCode::Char('b'), KeyModifiers::ALT) => {
            g.move_input_word_backward();
        }
        (KeyCode::Char('f'), KeyModifiers::ALT) => {
            g.move_input_word_forward();
        }
        // ── Word deletion (Ctrl+W / Alt+Backspace / Alt+D) ───
        (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
            g.delete_input_word_backward();
        }
        (KeyCode::Char('z'), KeyModifiers::CONTROL) => {
            g.undo_input();
        }
        (KeyCode::Backspace, KeyModifiers::ALT) => {
            g.delete_input_word_backward();
        }
        (KeyCode::Char('d'), KeyModifiers::ALT) => {
            g.delete_input_word_forward();
        }
        // ── Home / End on current logical line ───────────────
        (KeyCode::Home, _) => {
            g.move_input_home_line();
        }
        (KeyCode::End, _) => {
            g.move_input_end_line();
        }
        // ── Char movement ────────────────────────────────────
        (KeyCode::Left, _) => {
            g.move_input_left();
        }
        (KeyCode::Right, _) => {
            g.move_input_right();
        }
        (KeyCode::PageUp, _) => {
            if let Some(sz) = terminal_size {
                let area = Rect::new(0, 0, sz.width, sz.height);
                let (main_area, _) = layout_with_sidebar(area, g.sidebar_open);
                let sh = composer_chrome_height(
                    slash_entries,
                    workspace_files,
                    &g.input_buffer,
                    g.cursor_char_idx,
                );
                let input_h = if should_hide_composer_when_scrolling(g) {
                    0
                } else {
                    composer_input_height(g, main_area.width)
                };
                let (tr, _, _, _) =
                    layout_chunks(main_area, sh, input_h, g.unsent_count(), g.subagents.len());
                let (lines, _hits, _) =
                    transcript_cache.get_or_rebuild(g, tr.width.saturating_sub(2));
                let th = tr.height.saturating_sub(2) as usize;
                let w = tr.width.saturating_sub(2) as usize;
                scroll_buffer.replace_lines(lines.to_vec());
                scroll_buffer.set_from_top(g.scroll_lines, th, w);
                let step = th.max(scroll_speed as usize).max(1);
                scroll_buffer.scroll_up(step, w, th);
                let (from_top, _) = scroll_buffer.scroll_position_from_top(th, w);
                g.scroll_lines = from_top as usize;
                g.transcript_follow_tail = scroll_buffer.is_sticky();
            }
        }
        (KeyCode::PageDown, _) => {
            if let Some(sz) = terminal_size {
                let area = Rect::new(0, 0, sz.width, sz.height);
                let (main_area, _) = layout_with_sidebar(area, g.sidebar_open);
                let sh = composer_chrome_height(
                    slash_entries,
                    workspace_files,
                    &g.input_buffer,
                    g.cursor_char_idx,
                );
                let input_h = if should_hide_composer_when_scrolling(g) {
                    0
                } else {
                    composer_input_height(g, main_area.width)
                };
                let (tr, _, _, _) =
                    layout_chunks(main_area, sh, input_h, g.unsent_count(), g.subagents.len());
                let (lines, _hits, _) =
                    transcript_cache.get_or_rebuild(g, tr.width.saturating_sub(2));
                let th = tr.height.saturating_sub(2) as usize;
                let w = tr.width.saturating_sub(2) as usize;
                scroll_buffer.replace_lines(lines.to_vec());
                scroll_buffer.set_from_top(g.scroll_lines, th, w);
                let step = th.max(scroll_speed as usize).max(1);
                scroll_buffer.scroll_down(step);
                let (from_top, _) = scroll_buffer.scroll_position_from_top(th, w);
                g.scroll_lines = from_top as usize;
                g.transcript_follow_tail = scroll_buffer.is_sticky();
            }
        }
        // Up on an empty prompt with messages not yet sent: take them back
        // into the prompt to edit (Claude Code's "↑ to edit queued").
        (KeyCode::Up, KeyModifiers::NONE)
            if g.input_buffer.is_empty()
                && (!g.followup_queue.is_empty() || !g.steering_pending().is_empty()) =>
        {
            let text = g.take_unsent().join("\n");
            g.set_input_text(text);
        }
        (KeyCode::Up, _) => {
            let at_matches =
                at_completion_matches(workspace_files, &g.input_buffer, g.cursor_char_idx);
            if !at_matches.is_empty() && at_completion_active(&g.input_buffer, g.cursor_char_idx) {
                g.at_menu_index = g.at_menu_index.saturating_sub(1);
            } else {
                let slash_filtered = filter_slash_entries(slash_entries, &g.input_buffer);
                if !slash_filtered.is_empty() && slash_panel_visible(&g.input_buffer) {
                    g.slash_menu_index = g.slash_menu_index.saturating_sub(1);
                } else {
                    // Multiline: try moving up one visual line first.
                    let composer_w = terminal_size
                        .ok_or(())
                        .map(|sz| sz.width.saturating_sub(8) as usize)
                        .unwrap_or(80);
                    let moved_up = g.move_input_up(composer_w);
                    if moved_up {
                        // Moved within the composer — don't touch history.
                    } else {
                        let can_history = !composer_history.is_empty()
                            && (g.input_buffer.is_empty() || composer_history_index.is_some());
                        if can_history {
                            if composer_history_index.is_none() {
                                *composer_history_draft = g.input_buffer.clone();
                                *composer_history_index =
                                    Some(composer_history.len().saturating_sub(1));
                            } else if let Some(idx) = *composer_history_index {
                                *composer_history_index = Some(idx.saturating_sub(1));
                            }
                            if let Some(idx) = *composer_history_index
                                && let Some(entry) = composer_history.get(idx)
                            {
                                g.set_input_text(entry.clone());
                            }
                        } else {
                            if let Some(sz) = terminal_size {
                                let area = Rect::new(0, 0, sz.width, sz.height);
                                let (main_area, _) = layout_with_sidebar(area, g.sidebar_open);
                                let sh = composer_chrome_height(
                                    slash_entries,
                                    workspace_files,
                                    &g.input_buffer,
                                    g.cursor_char_idx,
                                );
                                let input_h = if should_hide_composer_when_scrolling(g) {
                                    0
                                } else {
                                    composer_input_height(g, main_area.width)
                                };
                                let (tr, _, _, _) = layout_chunks(
                                    main_area,
                                    sh,
                                    input_h,
                                    g.unsent_count(),
                                    g.subagents.len(),
                                );
                                let (lines, _hits, _) =
                                    transcript_cache.get_or_rebuild(g, tr.width.saturating_sub(2));
                                let th = tr.height.saturating_sub(2) as usize;
                                let w = tr.width.saturating_sub(2) as usize;
                                scroll_buffer.replace_lines(lines.to_vec());
                                scroll_buffer.set_from_top(g.scroll_lines, th, w);
                                scroll_buffer.scroll_up(scroll_speed as usize, w, th);
                                let (from_top, _) = scroll_buffer.scroll_position_from_top(th, w);
                                g.scroll_lines = from_top as usize;
                                g.transcript_follow_tail = scroll_buffer.is_sticky();
                            }
                        }
                    } // end else (moved_up)
                }
            }
        }
        (KeyCode::Down, _) => {
            let at_matches =
                at_completion_matches(workspace_files, &g.input_buffer, g.cursor_char_idx);
            if !at_matches.is_empty() && at_completion_active(&g.input_buffer, g.cursor_char_idx) {
                let n = at_matches.len();
                g.at_menu_index = (g.at_menu_index + 1) % n;
            } else {
                let slash_filtered = filter_slash_entries(slash_entries, &g.input_buffer);
                if !slash_filtered.is_empty() && slash_panel_visible(&g.input_buffer) {
                    let n = slash_filtered.len();
                    g.slash_menu_index = (g.slash_menu_index + 1) % n;
                } else {
                    // Multiline: try moving down one visual line first.
                    let composer_w = terminal_size
                        .ok_or(())
                        .map(|sz| sz.width.saturating_sub(8) as usize)
                        .unwrap_or(80);
                    let moved_down = g.move_input_down(composer_w);
                    if moved_down {
                        // Moved within composer — leave history alone.
                    } else {
                        if let Some(idx) = *composer_history_index {
                            if idx + 1 < composer_history.len() {
                                *composer_history_index = Some(idx + 1);
                                if let Some(entry) = composer_history.get(idx.saturating_add(1)) {
                                    g.set_input_text(entry.clone());
                                }
                            } else {
                                *composer_history_index = None;
                                g.set_input_text(composer_history_draft.clone());
                            }
                        } else {
                            let sz = terminal_size;
                            if let Some(sz) = sz {
                                let area = Rect::new(0, 0, sz.width, sz.height);
                                let (main_area, _) = layout_with_sidebar(area, g.sidebar_open);
                                let sh = composer_chrome_height(
                                    slash_entries,
                                    workspace_files,
                                    &g.input_buffer,
                                    g.cursor_char_idx,
                                );
                                let input_h = if should_hide_composer_when_scrolling(g) {
                                    0
                                } else {
                                    composer_input_height(g, main_area.width)
                                };
                                let (tr, _, _, _) = layout_chunks(
                                    main_area,
                                    sh,
                                    input_h,
                                    g.unsent_count(),
                                    g.subagents.len(),
                                );
                                let (lines, _hits, _) =
                                    transcript_cache.get_or_rebuild(g, tr.width.saturating_sub(2));
                                let th = tr.height.saturating_sub(2) as usize;
                                let w = tr.width.saturating_sub(2) as usize;
                                scroll_buffer.replace_lines(lines.to_vec());
                                scroll_buffer.set_from_top(g.scroll_lines, th, w);
                                scroll_buffer.scroll_down(scroll_speed as usize);
                                let (from_top, _) = scroll_buffer.scroll_position_from_top(th, w);
                                g.scroll_lines = from_top as usize;
                                g.transcript_follow_tail = scroll_buffer.is_sticky();
                            }
                        }
                    } // end else (moved_down)
                }
            }
        }
        (KeyCode::Backspace, _) => {
            *composer_history_index = None;
            composer_history_draft.clear();
            if g.cursor_char_idx > 0 {
                if let Some((buf, cidx)) =
                    delete_completed_at_mention(&g.input_buffer, g.cursor_char_idx)
                {
                    g.set_input_text_with_cursor(buf, cidx);
                } else {
                    g.backspace_input();
                }
                if slash_panel_visible(&g.input_buffer) {
                    let f = filter_slash_entries(slash_entries, &g.input_buffer);
                    if !f.is_empty() {
                        g.slash_menu_index = g.slash_menu_index.min(f.len().saturating_sub(1));
                    } else {
                        g.slash_menu_index = 0;
                    }
                }
            }
        }
        (KeyCode::Delete, _) => {
            *composer_history_index = None;
            composer_history_draft.clear();
            g.delete_input();
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            *composer_history_index = None;
            composer_history_draft.clear();
            g.insert_input_char(c);
            // The `@` list is ranked again: start from the best match.
            g.at_menu_index = 0;
            if slash_panel_visible(&g.input_buffer) {
                let f = filter_slash_entries(slash_entries, &g.input_buffer);
                if !f.is_empty() {
                    g.slash_menu_index = g.slash_menu_index.min(f.len().saturating_sub(1));
                }
            }
        }
        _ => {}
    }
    KeyFlow::Continue
}

/// Keys for the focused dashboard rail (Ctrl+D). Returns false for keys it
/// does not own; those drop focus and fall through to the composer.
fn handle_changes_key(key: KeyEvent, g: &mut TuiSessionState) -> bool {
    let plain = key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT;
    match key.code {
        KeyCode::Down | KeyCode::Char('j') if plain => g.diff_review.move_file(1),
        KeyCode::Up | KeyCode::Char('k') if plain => g.diff_review.move_file(-1),
        KeyCode::Char('r') if plain => {
            let ws = g.workspace_root.clone();
            if let Some(msg) = g.diff_review.press_revert(&ws) {
                let kind = if msg.starts_with("Reverted") {
                    crate::tui::state::ToastKind::Success
                } else if msg.starts_with("Revert failed") {
                    crate::tui::state::ToastKind::Error
                } else {
                    crate::tui::state::ToastKind::Info
                };
                g.show_toast(msg, kind);
            }
        }
        KeyCode::Char('g') if plain => {
            let ws = g.workspace_root.clone();
            g.diff_review.refresh(&ws);
        }
        // The full-screen diff, opened on the selected file.
        KeyCode::Enter => g.diff_overlay_open = true,
        // Move between the rail tabs; focus stays on the rail.
        KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h')
            if plain || key.code == KeyCode::BackTab =>
        {
            g.cycle_rail_tab(-1)
        }
        KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') if plain => g.cycle_rail_tab(1),
        KeyCode::Esc => g.rail_focus = false,
        _ => {
            g.rail_focus = false;
            g.diff_review.confirm_revert = false;
            return false;
        }
    }
    true
}
