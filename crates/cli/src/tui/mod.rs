//! Full-screen session TUI (transcript + streaming + composer).

pub mod activity;
pub mod answer_parse;
pub mod app;
pub mod branch_picker;
pub mod bridge;
pub mod btw;
pub mod busy_indicator;
pub mod choice;
pub mod clipboard;
pub mod composer;
pub mod composer_input;
pub mod connect_modal;
#[allow(dead_code)]
pub mod diff_hunk;
pub(crate) mod diff_overlay;
pub(crate) mod diff_review;
pub mod git;
pub mod keymap;
pub mod layout;
pub mod markdown;
pub mod mcp_panel;
pub mod mouse;
pub mod mouse_select;
pub mod notify;
pub mod oauth_status;
pub mod office;
pub mod office_overlay;
pub mod onboarding;
pub mod palette;
pub mod paste;
pub mod path_parse;
pub mod plugin_browser;
pub mod plugin_summary;
pub mod render;
pub mod render_helpers;
pub mod replay;
pub mod scroll_buffer;
pub mod setup_picker;
pub mod slash_entries;
pub mod state;
pub mod statusline;
pub mod sudo_prompt;
pub mod terminal;
pub mod theme;
pub mod timeline_overlay;
pub mod tool_summary;
pub mod transcript;
pub mod transcript_overlay;
pub mod tui_types;
pub mod tui_viewport;
pub mod widgets;

#[cfg(test)]
mod sidebar_tests;

pub use app::{TuiCmd, run_blocking};
pub use bridge::spawn_tui_bridge;
pub use busy_indicator::turn_summary;
pub use git::{git_create_branch, git_current_branch, git_list_branches, git_switch_branch};
pub use replay::replay_event_log_into_state;
pub use state::{DisplayBlock, SessionPickerEntry, TuiSessionState};
