//! Full-screen session TUI (transcript + streaming + composer).

pub mod answer_parse;
pub mod app;
pub mod branch_picker;
pub mod bridge;
pub mod busy_indicator;
pub mod clipboard;
pub mod composer;
pub mod composer_input;
pub mod connect_modal;
#[allow(dead_code)]
pub mod diff_hunk;
pub mod git;
pub mod layout;
pub mod markdown;
pub mod mouse;
pub mod mouse_select;
pub mod oauth_status;
pub mod onboarding;
pub mod palette;
pub mod paste;
pub mod path_parse;
pub mod render;
pub mod render_helpers;
pub mod replay;
pub mod scroll_buffer;
pub mod shimmer;
pub mod slash_entries;
pub mod state;
pub mod terminal;
pub mod theme;
pub mod tool_summary;
pub mod transcript;
pub mod tui_types;
pub mod tui_viewport;
pub mod widgets;

pub use app::{TuiCmd, run_blocking};
pub use bridge::spawn_tui_bridge;
pub use git::{git_create_branch, git_current_branch, git_list_branches, git_switch_branch};
pub use replay::replay_event_log_into_state;
pub use state::{
    DisplayBlock, ModelPickerAction, ModelPickerEntry, SessionPickerEntry, TuiSessionState,
};
