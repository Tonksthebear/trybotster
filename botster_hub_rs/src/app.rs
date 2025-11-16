use tuirealm::NoUserEvent;

// Component IDs
#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub enum Id {
    AgentList,
    TerminalView,
}

// Messages for inter-component communication
#[derive(Debug, PartialEq)]
pub enum Msg {
    AppClose,
    AgentSelected(usize),
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    EnterFocusMode,
    ExitFocusMode,
    None,
}

pub type AppEvent = tuirealm::Event<NoUserEvent>;
