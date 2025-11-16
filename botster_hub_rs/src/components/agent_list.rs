use crate::app::{AppEvent, Msg};
use crate::Agent;
use tuirealm::command::{Cmd, CmdResult, Direction, Position};
use tuirealm::tui::layout::Rect;
use tuirealm::tui::style::{Color, Modifier, Style};
use tuirealm::tui::widgets::{Block, Borders, List, ListItem, ListState};
use tuirealm::Frame;
use tuirealm::NoUserEvent;
use tuirealm::{Component, Event, MockComponent, State};

pub struct AgentList {
    selected: usize,
}

impl AgentList {
    pub fn new() -> Self {
        Self { selected: 0 }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }
}

impl MockComponent for AgentList {
    fn view(&mut self, frame: &mut Frame, area: Rect) {
        // Placeholder - will be filled with actual agent data from app state
        let items = vec![ListItem::new("Agent 1"), ListItem::new("Agent 2")];

        let mut state = ListState::default();
        state.select(Some(self.selected));

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Agents "))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
            .highlight_symbol("> ");

        frame.render_stateful_widget(list, area, &mut state);
    }

    fn query(&self, _attr: tuirealm::Attribute) -> Option<tuirealm::AttrValue> {
        None
    }

    fn attr(&mut self, _attr: tuirealm::Attribute, _value: tuirealm::AttrValue) {
        // No-op
    }

    fn state(&self) -> State {
        State::One(tuirealm::StateValue::Usize(self.selected))
    }

    fn perform(&mut self, cmd: Cmd) -> CmdResult {
        match cmd {
            Cmd::Move(Direction::Down) => {
                // TODO: Get actual agent count
                let max: usize = 10; // placeholder
                if self.selected < max.saturating_sub(1) {
                    self.selected += 1;
                    CmdResult::Changed(self.state())
                } else {
                    CmdResult::None
                }
            }
            Cmd::Move(Direction::Up) => {
                if self.selected > 0 {
                    self.selected -= 1;
                    CmdResult::Changed(self.state())
                } else {
                    CmdResult::None
                }
            }
            _ => CmdResult::None,
        }
    }
}

impl Component<Msg, NoUserEvent> for AgentList {
    fn on(&mut self, ev: AppEvent) -> Option<Msg> {
        use tuirealm::event::{Key, KeyEvent, KeyModifiers};

        match ev {
            Event::Keyboard(KeyEvent {
                code: Key::Down, ..
            })
            | Event::Keyboard(KeyEvent {
                code: Key::Char('j'),
                modifiers: KeyModifiers::NONE,
            }) => {
                self.perform(Cmd::Move(Direction::Down));
                Some(Msg::AgentSelected(self.selected))
            }
            Event::Keyboard(KeyEvent { code: Key::Up, .. })
            | Event::Keyboard(KeyEvent {
                code: Key::Char('k'),
                modifiers: KeyModifiers::NONE,
            }) => {
                self.perform(Cmd::Move(Direction::Up));
                Some(Msg::AgentSelected(self.selected))
            }
            Event::Keyboard(KeyEvent {
                code: Key::Char('q'),
                modifiers: KeyModifiers::NONE,
            }) => Some(Msg::AppClose),
            _ => None,
        }
    }
}
