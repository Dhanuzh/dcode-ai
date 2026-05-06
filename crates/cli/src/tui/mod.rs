//! Full-screen session TUI (transcript + streaming + composer).

pub mod app;
pub mod bridge;
pub mod busy_indicator;
pub mod clipboard;
pub mod composer;
pub mod connect_modal;
pub mod mouse_select;
pub mod onboarding;
pub mod replay;
pub mod scroll_buffer;
pub mod state;
pub mod theme;
pub mod tool_classify;
pub mod tool_summary;
pub mod tui_types;
pub mod tui_viewport;
pub mod widgets;

pub use app::{
    TuiCmd, git_create_branch, git_current_branch, git_list_branches, git_switch_branch,
    run_blocking,
};
pub use bridge::spawn_tui_bridge;
pub use replay::replay_event_log_into_state;
pub use state::{
    DisplayBlock, ModelPickerAction, ModelPickerEntry, SessionPickerEntry, TuiSessionState,
};
