#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuContent {
    None,
    Slash,
    FileMention,
    TranscriptSearch,
    ComposerHistorySearch,
    CommandPalette,
    ModelPicker,
    SessionPicker,
    Connect,
    Approval,
    Question,
    Pins,
    SubAgents,
    Info,
}
