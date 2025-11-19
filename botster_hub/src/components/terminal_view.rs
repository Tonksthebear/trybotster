use crate::app::{AppEvent, Id, Msg};
use crate::Agent;
use tuirealm::command::{Cmd, CmdResult};
use tuirealm::props::{Alignment, BorderType, Borders, Color, TextSpan};
use tuirealm::tui::layout::Rect;
use tuirealm::tui::widgets::{Block, Borders as TuiBorders, Paragraph, Wrap};
use tuirealm::Frame;
use tuirealm::{Component, Event, MockComponent, State, StateValue};

#[derive(Default)]
pub struct TerminalView {
    scroll_offset: usize,
    focus_mode: bool,
}

impl TerminalView {
    pub fn new() -> Self {
        Self {
            scroll_offset: 0,
            focus_mode: false,
        }
    }

    pub fn with_agent(agent: &Agent, scroll: usize, focus: bool) -> Vec<String> {
        agent.get_vt100_screen()
    }
}

impl MockComponent for TerminalView {
    fn view(&mut self, frame: &mut Frame, area: Rect) {
        // This will be called by the application to render
        // For now, just render a placeholder
        let title = if self.focus_mode {
            " Terminal [FOCUS MODE - ESC to exit] "
        } else {
            " Terminal [F=focus] "
        };

        let block = Block::default().borders(TuiBorders::ALL).title(title);

        let paragraph = Paragraph::new("Terminal view placeholder")
            .block(block)
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, area);
    }

    fn query(&self, attr: tuirealm::Attribute) -> Option<tuirealm::AttrValue> {
        None
    }

    fn attr(&mut self, _attr: tuirealm::Attribute, _value: tuirealm::AttrValue) {
        // No-op
    }

    fn state(&self) -> State {
        State::None
    }

    fn perform(&mut self, cmd: Cmd) -> CmdResult {
        match cmd {
            Cmd::Scroll(tuirealm::command::Direction::Down) => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                CmdResult::Changed(State::None)
            }
            Cmd::Scroll(tuirealm::command::Direction::Up) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                CmdResult::Changed(State::None)
            }
            Cmd::GoTo(tuirealm::command::Position::Begin) => {
                self.scroll_offset = 0;
                CmdResult::Changed(State::None)
            }
            Cmd::GoTo(tuirealm::command::Position::End) => {
                self.scroll_offset = usize::MAX;
                CmdResult::Changed(State::None)
            }
            _ => CmdResult::None,
        }
    }
}

impl Component<Msg, NoUserEvent> for TerminalView {
    fn on(&mut self, ev: AppEvent) -> Option<Msg> {
        use tuirealm::event::{Key, KeyEvent, KeyModifiers};

        match ev {
            Event::Keyboard(KeyEvent {
                code: Key::Char('f'),
                modifiers: KeyModifiers::NONE,
            }) => {
                self.focus_mode = true;
                Some(Msg::EnterFocusMode)
            }
            Event::Keyboard(KeyEvent {
                code: Key::Esc,
                modifiers: KeyModifiers::NONE,
            }) if self.focus_mode => {
                self.focus_mode = false;
                Some(Msg::ExitFocusMode)
            }
            Event::Keyboard(KeyEvent {
                code: Key::Down, ..
            })
            | Event::Keyboard(KeyEvent {
                code: Key::Char('j'),
                modifiers: KeyModifiers::NONE,
            }) => {
                self.perform(Cmd::Scroll(tuirealm::command::Direction::Down));
                Some(Msg::ScrollDown)
            }
            Event::Keyboard(KeyEvent { code: Key::Up, .. })
            | Event::Keyboard(KeyEvent {
                code: Key::Char('k'),
                modifiers: KeyModifiers::NONE,
            }) => {
                self.perform(Cmd::Scroll(tuirealm::command::Direction::Up));
                Some(Msg::ScrollUp)
            }
            Event::Keyboard(KeyEvent {
                code: Key::Home, ..
            }) => {
                self.perform(Cmd::GoTo(tuirealm::command::Position::Begin));
                Some(Msg::ScrollToTop)
            }
            Event::Keyboard(KeyEvent { code: Key::End, .. }) => {
                self.perform(Cmd::GoTo(tuirealm::command::Position::End));
                Some(Msg::ScrollToBottom)
            }
            Event::Keyboard(KeyEvent {
                code: Key::Char('q'),
                modifiers: KeyModifiers::NONE,
            }) if !self.focus_mode => Some(Msg::AppClose),
            _ => {
                if self.focus_mode {
                    // In focus mode, forward all keys to PTY
                    // TODO: Convert event to PTY input
                    Some(Msg::None)
                } else {
                    None
                }
            }
        }
    }
}

use tuirealm::NoUserEvent;
