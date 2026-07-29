# Test Plan: Right Sidebar

## Scope

Covers testing the right sidebar feature in `dcode-ai` across both surfaces:

1. **TUI (Terminal UI)** — sidebar state, toggle behavior, layout integration
2. **Web Chat** — sidebar rendering, session list, thinking/tool cards, status

---

## 1. TUI Sidebar Tests

### 1.1 State Management

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| T-01 | `sidebar_open` defaults to `true` on new session | State initialized with `sidebar_open: true` |
| T-02 | Toggle `sidebar_open` to `false` | `sidebar_open == false` |
| T-03 | Toggle `sidebar_open` back to `true` | `sidebar_open == true` |
| T-04 | `sidebar_toggle_bounds` is `None` before first render | Initial state is `None` |
| T-05 | After render, `sidebar_toggle_bounds` has valid `Rect` | Bounds set to a non-zero area |

### 1.2 Layout Integration

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| T-06 | `layout_with_sidebar(area, true)` returns full area + `None` sidebar | `(area, None)` — sidebar removed in fullscreen |
| T-07 | `layout_with_sidebar(area, false)` returns full area + `None` sidebar | `(area, None)` — same behavior regardless of toggle |
| T-08 | Main transcript area occupies full terminal width | No space reserved for sidebar |
| T-09 | Status bar spans full terminal width | No sidebar overflow |

### 1.3 Subagent Rows (Sidebar Content)

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| T-10 | `SubagentRow` created with valid fields | All fields (`id`, `task`, `phase`, `detail`, `running`) populated |
| T-11 | `subagents` list starts empty | `Vec::new()` on fresh session |
| T-12 | Add subagent row | Row appears in `subagents` vec |
| T-13 | Update subagent `running` to `false` | Row reflects stopped state |
| T-14 | Remove subagent row | Row removed from list |
| T-15 | Multiple subagents tracked independently | Each has unique `id`, separate state |

### 1.4 Keyboard Shortcut

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| T-16 | Press `Ctrl+B` | Toggles `sidebar_open` state |
| T-17 | Press `Ctrl+B` twice | Returns to original state |
| T-18 | `/sidebar` slash command in REPL | Prints "removed in fullscreen TUI" message |

---

## 2. Web Chat Sidebar Tests

### 2.1 Rendering

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| W-01 | Sidebar renders on page load | `<aside id="sidebar">` visible |
| W-02 | Sidebar shows session list | Sessions listed with labels |
| W-03 | Active session highlighted | Current session visually distinct |
| W-04 | Sidebar shows model/provider info | Provider name displayed |
| W-05 | Sidebar shows session status (idle/working) | Status indicator updates live |

### 2.2 Collapse/Expand

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| W-06 | Click sidebar toggle button | Sidebar collapses to 64px icon-only |
| W-07 | Click toggle again | Sidebar expands to full width (264px) |
| W-08 | Press `Ctrl+B` | Same as toggle button |
| W-09 | Collapse state persists in `localStorage` | Reload page, sidebar stays collapsed |
| W-10 | Expand state persists in `localStorage` | Reload page, sidebar stays expanded |

### 2.3 Mobile Behavior

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| W-11 | Resize to mobile (< 768px) | Sidebar hidden off-screen (`translateX(-100%)`) |
| W-12 | Click toggle on mobile | Sidebar slides in as overlay |
| W-13 | Click outside sidebar on mobile | Sidebar slides out |
| W-14 | Body class `sb-mobile-open` toggled | CSS transition animates smoothly |

### 2.4 Session Interaction

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| W-15 | Click a session in sidebar | Switches to that session |
| W-16 | New session created | Appears in sidebar list |
| W-17 | Session completes | Status updates in sidebar |
| W-18 | Session fails | Error state shown in sidebar |

### 2.5 Thinking/Tool Cards

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| W-19 | Thinking block appears in sidebar | Card rendered with thinking content |
| W-20 | Click thinking card header | Collapses/expands content |
| W-21 | Tool call appears | Card shows tool name + status |
| W-22 | Tool completes | Card shows success/failure badge |
| W-23 | Multiple tool cards | Each independently collapsible |

### 2.6 Status Display

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| W-24 | Model name shown | e.g. "gpt-4o", "claude-3-5-sonnet" |
| W-25 | Context tokens shown | Token count updates during streaming |
| W-26 | Idle/working indicator | Shows "idle" when no turn, "working" when streaming |
| W-27 | Token count updates live | Incrementing during LLM response |

---

## 3. IPC Sidebar Events

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| I-01 | `SubagentSpawned` event adds row to sidebar | New `SubagentRow` created |
| I-02 | `SubagentActivity` event updates existing row | `phase`/`detail` updated |
| I-03 | `SubagentCompleted` event marks row as done | `running = false` |
| I-04 | Events arrive out of order | State remains consistent |

---

## 4. Cross-Surface Consistency

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| C-01 | TUI and Web Chat show same session info | Model, tokens, status match |
| C-02 | Subagent list matches between surfaces | Same rows, same state |
| C-03 | Toggle in one surface doesn't affect other | Independent state per surface |

---

## 5. Edge Cases

| # | Test Case | Expected Result |
|---|-----------|-----------------|
| E-01 | Terminal width < 80 columns | Sidebar doesn't render (or renders minimal) |
| E-02 | Terminal height < 10 rows | Layout doesn't break |
| E-03 | 100+ subagents | Sidebar scrolls or paginates |
| E-04 | Sidebar toggle during active stream | No crash, state updates correctly |
| E-05 | Sidebar toggle during approval request | Approval UI stays functional |
| E-06 | Rapid toggle (10x fast) | State stays consistent, no race condition |

---

## Test Execution Notes

- **TUI tests**: Run `dcode-ai` in terminal, exercise keyboard shortcuts, verify layout
- **Web Chat tests**: Run `dcode-ai web`, open browser, test sidebar interactions
- **Unit tests**: Add `#[cfg(test)]` modules in `tui/state/` and verify state transitions
- **Snapshot tests**: Use `insta` for layout snapshot regression (existing pattern in codebase)
