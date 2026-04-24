# Question Modal Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the number-key-based Ask Question option picker with a modal popup using arrow key navigation.

**Architecture:** Add modal state fields and open/close helpers to `TuiSessionState`, render the modal as a centered popup (same pattern as permission/agent pickers), intercept key events when modal is open, and trigger the modal automatically when a `QuestionRequested` event arrives.

**Tech Stack:** Rust, ratatui, crossterm

**Spec:** `docs/superpowers/specs/2026-03-24-question-modal-picker-design.md`

---

### Task 1: Add modal state fields and open/close helpers

**Files:**
- Modify: `crates/cli/src/tui/state.rs:61-171` (add fields to `TuiSessionState`)
- Modify: `crates/cli/src/tui/state.rs:187-266` (add defaults in `new()`)
- Modify: `crates/cli/src/tui/state.rs:268-383` (add helper methods)

- [ ] **Step 1: Add state fields to `TuiSessionState` struct**

Add after `agent_picker_index` (line 157):

```rust
/// Question modal popup (arrow-key option picker).
pub question_modal_open: bool,
pub question_modal_index: usize,
pub question_modal_scroll: usize,
```

- [ ] **Step 2: Add defaults in `new()` constructor**

Add after `agent_picker_index: 0,` (line 256):

```rust
question_modal_open: false,
question_modal_index: 0,
question_modal_scroll: 0,
```

- [ ] **Step 3: Add `open_question_modal` / `close_question_modal` helpers**

Add after `close_agent_picker` (line 353):

```rust
pub fn open_question_modal(&mut self) {
    self.question_modal_open = true;
    self.question_modal_index = 0;
    self.question_modal_scroll = 0;
}

pub fn close_question_modal(&mut self) {
    self.question_modal_open = false;
    self.question_modal_index = 0;
    self.question_modal_scroll = 0;
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p dcode-ai-cli 2>&1 | tail -5`
Expected: no errors (new fields are unused warnings are fine)

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/tui/state.rs
git commit -m "feat(tui): add question modal state fields and helpers (#32)"
```

---

### Task 2: Auto-open modal on QuestionRequested event

**Files:**
- Modify: `crates/cli/src/tui/state.rs:586-591` (`apply_event` for `QuestionRequested`)

- [ ] **Step 1: Modify `QuestionRequested` handler to open the modal**

Change the existing handler at line 586-591 from:

```rust
AgentEvent::QuestionRequested { question } => {
    self.active_question = Some(question.clone());
    self.blocks.push(DisplayBlock::Question(question.clone()));
    self.transcript_follow_tail = true;
}
```

To:

```rust
AgentEvent::QuestionRequested { question } => {
    self.active_question = Some(question.clone());
    self.blocks.push(DisplayBlock::Question(question.clone()));
    self.transcript_follow_tail = true;
    self.open_question_modal();
}
```

- [ ] **Step 2b: Add defensive `close_question_modal()` to `QuestionResolved` handler**

In the `QuestionResolved` handler (line 592-600), add `self.close_question_modal();` after `self.active_question = None;`:

```rust
AgentEvent::QuestionResolved {
    question_id,
    selection,
} => {
    self.active_question = None;
    self.close_question_modal();
    self.blocks.push(DisplayBlock::System(format!(
        "Answered question {question_id}: {selection:?}"
    )));
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p dcode-ai-cli 2>&1 | tail -5`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add crates/cli/src/tui/state.rs
git commit -m "feat(tui): auto-open question modal on QuestionRequested (#32)"
```

---

### Task 3: Add key event handling for the question modal

**Files:**
- Modify: `crates/cli/src/tui/app.rs:3062-3111` (add question modal handler before permission picker handler)

- [ ] **Step 1: Add question modal keyboard handler**

Insert before the permission picker handler (before line 3063 `// Permission picker keyboard handling.`):

```rust
// Question modal keyboard handling.
if g.question_modal_open {
    if let Some(ref q) = g.active_question.clone() {
        // Total items: 1 (suggested) + options.len() + (1 if allow_custom for "Chat about this")
        let total = 1 + q.options.len() + if q.allow_custom { 1 } else { 0 };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                if q.allow_custom {
                    // Fall back to inline text input
                    g.close_question_modal();
                }
                // If !allow_custom, Esc is a no-op
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                g.question_modal_index = g.question_modal_index.saturating_sub(1);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                g.question_modal_index = (g.question_modal_index + 1).min(total - 1);
            }
            (KeyCode::Enter, _) => {
                let idx = g.question_modal_index;
                let sel = if idx == 0 {
                    // Suggested answer
                    Some(QuestionSelection::Suggested)
                } else if idx <= q.options.len() {
                    // Regular option (1-based → 0-based)
                    Some(QuestionSelection::Option {
                        option_id: q.options[idx - 1].id.clone(),
                    })
                } else {
                    // "Chat about this" — fall back to inline text input
                    None
                };

                if let Some(sel) = sel {
                    let qid = q.question_id.clone();
                    g.close_question_modal();
                    g.active_question = None;
                    drop(g);
                    if let Some(ref tx) = question_answer_tx {
                        let _ = tx.send((qid, sel));
                    } else {
                        let _ = cmd_tx.send(TuiCmd::QuestionAnswer(sel));
                    }
                } else {
                    // "Chat about this" — close modal, keep active_question
                    g.close_question_modal();
                }
            }
            _ => {}
        }
    }
    continue;
}
```

- [ ] **Step 2: Add mouse event swallowing for question modal**

In the mouse event swallowing section (around line 2590-2599), add after `Event::Mouse(_) if g.session_picker_open => continue,`:

```rust
Event::Mouse(_) if g.question_modal_open => continue,
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p dcode-ai-cli 2>&1 | tail -5`
Expected: no errors

- [ ] **Step 4: Commit**

```bash
git add crates/cli/src/tui/app.rs
git commit -m "feat(tui): add question modal key event handling (#32)"
```

---

### Task 4: Render the question modal popup

**Files:**
- Modify: `crates/cli/src/tui/app.rs:2085-2086` (add rendering after permission picker rendering block)

- [ ] **Step 1: Add question modal rendering**

Insert after the agent picker rendering block (after the closing `}` of the `if g.agent_picker_open` block, around line 2140):

```rust
// Question modal popup (arrow-key option picker).
if g.question_modal_open {
    if let Some(ref q) = g.active_question {
        let has_chat_option = q.allow_custom;
        let total_items = 1 + q.options.len() + if has_chat_option { 1 } else { 0 };
        // +4 for: title line, blank, blank before footer, footer
        let rows = (total_items as u16).saturating_add(6).max(8);
        let popup_w = 60u16.min(area.width.saturating_sub(4));
        let popup_area = centered_rect(area, popup_w, rows);

        let mut lines: Vec<Line> = vec![
            Line::from(Span::styled(
                format!(" {} ", q.prompt),
                Style::default()
                    .fg(theme::ASSISTANT)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::default(),
        ];

        // Suggested answer (index 0)
        let suggested_label = format!(" Suggested: {} ", q.suggested_answer);
        if g.question_modal_index == 0 {
            lines.push(Line::from(Span::styled(
                format!(" ► {}", suggested_label.trim()),
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::USER)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                format!("   {}", suggested_label.trim()),
                Style::default().fg(theme::TEXT),
            )));
        }

        // Options (index 1..n)
        for (i, o) in q.options.iter().enumerate() {
            let item_idx = i + 1;
            let label = format!("{} ", o.label);
            if g.question_modal_index == item_idx {
                lines.push(Line::from(Span::styled(
                    format!(" ► {}", label.trim()),
                    Style::default()
                        .fg(Color::Black)
                        .bg(theme::USER)
                        .add_modifier(Modifier::BOLD),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("   {}", label.trim()),
                    Style::default().fg(theme::TEXT),
                )));
            }
        }

        // "Chat about this" (last item, only if allow_custom)
        if has_chat_option {
            let chat_idx = 1 + q.options.len();
            if g.question_modal_index == chat_idx {
                lines.push(Line::from(Span::styled(
                    " ► Chat about this",
                    Style::default()
                        .fg(Color::Black)
                        .bg(theme::USER)
                        .add_modifier(Modifier::BOLD),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    "   Chat about this",
                    Style::default()
                        .fg(theme::MUTED)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }

        // Footer
        lines.push(Line::default());
        let footer_text = if has_chat_option {
            " ↑↓ select · Enter confirm · Esc chat "
        } else {
            " ↑↓ select · Enter confirm "
        };
        lines.push(Line::from(Span::styled(
            footer_text,
            Style::default().fg(theme::MUTED),
        )));

        frame.render_widget(ClearWidget, popup_area);
        let popup = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::BORDER))
                    .title(Span::styled(
                        " question ",
                        Style::default().fg(theme::WARN),
                    )),
            )
            .style(Style::default().bg(theme::SURFACE))
            .wrap(Wrap { trim: false });
        frame.render_widget(popup, popup_area);
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p dcode-ai-cli 2>&1 | tail -5`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add crates/cli/src/tui/app.rs
git commit -m "feat(tui): render question modal popup (#32)"
```

---

### Task 5: Suppress composer hint when modal is open

**Files:**
- Modify: `crates/cli/src/tui/app.rs:1801-1804` (composer hint line)

- [ ] **Step 1: Guard the question hint with `!question_modal_open`**

Change the hint at line 1801-1804 from:

```rust
} else if g.active_question.is_some() {
    Line::from(Span::styled(
        "Enter / 0 = suggested · 1–n = option · click underlined line · /auto-answer · End = transcript bottom (empty input)",
        Style::default().fg(theme::WARN),
    ))
```

To:

```rust
} else if g.active_question.is_some() && !g.question_modal_open {
    Line::from(Span::styled(
        "Enter / 0 = suggested · 1–n = option · click underlined line · /auto-answer · End = transcript bottom (empty input)",
        Style::default().fg(theme::WARN),
    ))
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p dcode-ai-cli 2>&1 | tail -5`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add crates/cli/src/tui/app.rs
git commit -m "feat(tui): suppress composer hint when question modal is open (#32)"
```

---

### Task 6: Update existing tests and add new tests

**Files:**
- Modify: `crates/cli/src/tui/state.rs:749-878` (test module)

- [ ] **Step 1: Add test for `open_question_modal` / `close_question_modal` helpers**

Add to the test module:

```rust
#[test]
fn open_close_question_modal() {
    let mut st = TuiSessionState::new(
        "s".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
    );
    assert!(!st.question_modal_open);
    assert_eq!(st.question_modal_index, 0);

    st.open_question_modal();
    assert!(st.question_modal_open);
    assert_eq!(st.question_modal_index, 0);
    assert_eq!(st.question_modal_scroll, 0);

    st.question_modal_index = 3;
    st.close_question_modal();
    assert!(!st.question_modal_open);
    assert_eq!(st.question_modal_index, 0);
    assert_eq!(st.question_modal_scroll, 0);
}
```

- [ ] **Step 2: Add test verifying `QuestionRequested` event opens the modal**

```rust
#[test]
fn question_requested_opens_modal() {
    let mut st = TuiSessionState::new(
        "s".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
    );
    let q = InteractiveQuestionPayload {
        question_id: "q-1".into(),
        call_id: "c1".into(),
        prompt: "Pick".into(),
        options: vec![QuestionOption {
            id: "a".into(),
            label: "A".into(),
        }],
        allow_custom: true,
        suggested_answer: "A".into(),
    };
    st.apply_event(&AgentEvent::QuestionRequested {
        question: q.clone(),
    });
    assert!(st.question_modal_open);
    assert_eq!(st.question_modal_index, 0);
    assert!(st.active_question.is_some());
}
```

- [ ] **Step 3: Add test for `QuestionResolved` clears modal state defensively**

```rust
#[test]
fn question_resolved_closes_modal() {
    let mut st = TuiSessionState::new(
        "s".into(),
        "m".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp"),
    );
    st.question_modal_open = true;
    st.question_modal_index = 2;
    st.apply_event(&AgentEvent::QuestionResolved {
        question_id: "q-1".into(),
        selection: QuestionSelection::Suggested,
    });
    assert!(st.active_question.is_none());
    assert!(!st.question_modal_open);
    assert_eq!(st.question_modal_index, 0);
}
```

- [ ] **Step 4: Run all tests**

Run: `cargo test -p dcode-ai-cli 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/tui/state.rs
git commit -m "test(tui): add question modal state tests (#32)"
```

---

### Task 7: Manual verification and final cleanup

- [ ] **Step 1: Build the full project**

Run: `cargo build 2>&1 | tail -10`
Expected: builds successfully

- [ ] **Step 2: Run all tests across workspace**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 3: Manual verification checklist**

Run the application and trigger a question (use the ask_question tool). Verify:
- Modal appears centered with the question prompt as title
- Suggested answer is highlighted at index 0
- Arrow up/down moves the highlight correctly
- Enter on suggested → sends `QuestionSelection::Suggested`
- Enter on an option → sends `QuestionSelection::Option`
- Enter on "Chat about this" → closes modal, inline text input appears
- Esc with `allow_custom=true` → closes modal, inline text input appears
- Esc with `allow_custom=false` → no-op, modal stays open
- Mouse clicks are swallowed while modal is open
- Composer hint is hidden while modal is open
- `/auto-answer` still works (bypasses modal via existing path)

- [ ] **Step 4: Commit any cleanup**

```bash
git add -A
git commit -m "feat(tui): question modal picker - final cleanup (#32)"
```
