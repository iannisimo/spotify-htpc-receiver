//! Remote/keyboard input: what each button does on each screen.

use crate::config::Config;
use crate::model::Command;

/// A logical remote button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Ok,
    Back,
}

/// Parses a logical key name (`left`, `right`, `up`, `down`, `ok`, `back`), ignoring ASCII case.
pub fn key_from_name(name: &str) -> Option<Key> {
    const NAMES: [(&str, Key); 6] = [
        ("left", Key::Left),
        ("right", Key::Right),
        ("up", Key::Up),
        ("down", Key::Down),
        ("ok", Key::Ok),
        ("back", Key::Back),
    ];
    NAMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|&(_, key)| key)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    #[default]
    NowPlaying,
    Visualizer,
}

/// The now-playing button row, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    Previous,
    #[default]
    PlayPause,
    Next,
    Visualizer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiState {
    pub screen: Screen,
    pub focus: Focus,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Send(Command),
    OpenVisualizer,
    CloseVisualizer,
    PreviousPreset,
    NextPreset,
    TogglePresetLock,
    BeatSensitivityDelta(f32),
    None,
}

/// Beat sensitivity change per Up/Down press in the visualizer.
const BEAT_SENSITIVITY_STEP: f32 = 0.1;

impl Focus {
    const ROW: [Focus; 4] = [
        Focus::Previous,
        Focus::PlayPause,
        Focus::Next,
        Focus::Visualizer,
    ];

    /// The neighbour in the button row, clamped at the ends (no wrap).
    fn step(self, delta: isize) -> Focus {
        let index = Self::ROW.iter().position(|&f| f == self).unwrap_or(0);
        let last = Self::ROW.len() - 1;
        Self::ROW[index.saturating_add_signed(delta).min(last)]
    }
}

/// Applies a button press to the UI state and returns what the app should do.
pub fn handle_key(ui: &mut UiState, key: Key, cfg: &Config) -> Action {
    match ui.screen {
        Screen::NowPlaying => match key {
            Key::Left => {
                ui.focus = ui.focus.step(-1);
                Action::None
            }
            Key::Right => {
                ui.focus = ui.focus.step(1);
                Action::None
            }
            Key::Ok => match ui.focus {
                Focus::Previous => Action::Send(Command::Previous),
                Focus::PlayPause => Action::Send(Command::PlayPause),
                Focus::Next => Action::Send(Command::Next),
                Focus::Visualizer => {
                    ui.screen = Screen::Visualizer;
                    Action::OpenVisualizer
                }
            },
            Key::Up | Key::Down if cfg.audio_controls => {
                let step = i8::try_from(cfg.volume_step).unwrap_or(i8::MAX);
                let delta = if key == Key::Up { step } else { -step };
                Action::Send(Command::VolumeDelta(delta))
            }
            Key::Up | Key::Down | Key::Back => Action::None,
        },
        Screen::Visualizer => match key {
            Key::Left => Action::PreviousPreset,
            Key::Right => Action::NextPreset,
            Key::Up => Action::BeatSensitivityDelta(BEAT_SENSITIVITY_STEP),
            Key::Down => Action::BeatSensitivityDelta(-BEAT_SENSITIVITY_STEP),
            Key::Ok => Action::TogglePresetLock,
            Key::Back => {
                ui.screen = Screen::NowPlaying;
                Action::CloseVisualizer
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(audio_controls: bool, volume_step: u8) -> Config {
        Config {
            audio_controls,
            volume_step,
            ..Config::default()
        }
    }

    fn now_playing(focus: Focus) -> UiState {
        UiState {
            screen: Screen::NowPlaying,
            focus,
        }
    }

    fn visualizer() -> UiState {
        UiState {
            screen: Screen::Visualizer,
            focus: Focus::Visualizer,
        }
    }

    #[test]
    fn starts_on_now_playing_with_play_pause_focused() {
        assert_eq!(UiState::default(), now_playing(Focus::PlayPause));
    }

    #[test]
    fn focus_moves_right_across_row_and_clamps() {
        let mut ui = now_playing(Focus::Previous);
        let c = Config::default();
        for expected in [
            Focus::PlayPause,
            Focus::Next,
            Focus::Visualizer,
            Focus::Visualizer,
        ] {
            assert_eq!(handle_key(&mut ui, Key::Right, &c), Action::None);
            assert_eq!(ui.focus, expected);
        }
    }

    #[test]
    fn focus_moves_left_across_row_and_clamps() {
        let mut ui = now_playing(Focus::Visualizer);
        let c = Config::default();
        for expected in [
            Focus::Next,
            Focus::PlayPause,
            Focus::Previous,
            Focus::Previous,
        ] {
            assert_eq!(handle_key(&mut ui, Key::Left, &c), Action::None);
            assert_eq!(ui.focus, expected);
        }
    }

    #[test]
    fn ok_on_play_pause_sends_command() {
        let mut ui = now_playing(Focus::PlayPause);
        let action = handle_key(&mut ui, Key::Ok, &Config::default());
        assert_eq!(action, Action::Send(Command::PlayPause));
        assert_eq!(ui, now_playing(Focus::PlayPause));
    }

    #[test]
    fn ok_on_previous_and_next_sends_skip() {
        let c = Config::default();
        let mut ui = now_playing(Focus::Previous);
        assert_eq!(
            handle_key(&mut ui, Key::Ok, &c),
            Action::Send(Command::Previous)
        );
        let mut ui = now_playing(Focus::Next);
        assert_eq!(
            handle_key(&mut ui, Key::Ok, &c),
            Action::Send(Command::Next)
        );
    }

    #[test]
    fn ok_on_visualizer_opens_it() {
        let mut ui = now_playing(Focus::Visualizer);
        let action = handle_key(&mut ui, Key::Ok, &Config::default());
        assert_eq!(action, Action::OpenVisualizer);
        assert_eq!(ui.screen, Screen::Visualizer);
    }

    #[test]
    fn up_down_ignored_when_audio_controls_off() {
        let mut ui = now_playing(Focus::PlayPause);
        let c = cfg(false, 5);
        assert_eq!(handle_key(&mut ui, Key::Up, &c), Action::None);
        assert_eq!(handle_key(&mut ui, Key::Down, &c), Action::None);
        assert_eq!(ui, now_playing(Focus::PlayPause));
    }

    #[test]
    fn up_down_volume_when_enabled() {
        let mut ui = now_playing(Focus::Next);
        let c = cfg(true, 5);
        assert_eq!(
            handle_key(&mut ui, Key::Up, &c),
            Action::Send(Command::VolumeDelta(5))
        );
        assert_eq!(
            handle_key(&mut ui, Key::Down, &c),
            Action::Send(Command::VolumeDelta(-5))
        );
        assert_eq!(ui, now_playing(Focus::Next), "focus does not move");
    }

    #[test]
    fn volume_step_from_config() {
        let mut ui = UiState::default();
        let c = cfg(true, 25);
        assert_eq!(
            handle_key(&mut ui, Key::Up, &c),
            Action::Send(Command::VolumeDelta(25))
        );
        assert_eq!(
            handle_key(&mut ui, Key::Down, &c),
            Action::Send(Command::VolumeDelta(-25))
        );
    }

    #[test]
    fn back_on_now_playing_does_nothing() {
        let mut ui = now_playing(Focus::Next);
        assert_eq!(
            handle_key(&mut ui, Key::Back, &Config::default()),
            Action::None
        );
        assert_eq!(ui, now_playing(Focus::Next));
    }

    #[test]
    fn visualizer_left_right_change_preset() {
        let mut ui = visualizer();
        let c = Config::default();
        assert_eq!(handle_key(&mut ui, Key::Left, &c), Action::PreviousPreset);
        assert_eq!(handle_key(&mut ui, Key::Right, &c), Action::NextPreset);
        assert_eq!(ui, visualizer());
    }

    #[test]
    fn visualizer_up_down_adjust_beat_sensitivity() {
        let mut ui = visualizer();
        let c = Config::default();
        assert_eq!(
            handle_key(&mut ui, Key::Up, &c),
            Action::BeatSensitivityDelta(0.1)
        );
        assert_eq!(
            handle_key(&mut ui, Key::Down, &c),
            Action::BeatSensitivityDelta(-0.1)
        );
    }

    #[test]
    fn visualizer_ok_toggles_lock() {
        let mut ui = visualizer();
        assert_eq!(
            handle_key(&mut ui, Key::Ok, &Config::default()),
            Action::TogglePresetLock
        );
        assert_eq!(ui, visualizer());
    }

    #[test]
    fn back_returns_from_visualizer() {
        let mut ui = now_playing(Focus::Visualizer);
        let c = Config::default();
        handle_key(&mut ui, Key::Ok, &c);
        assert_eq!(handle_key(&mut ui, Key::Back, &c), Action::CloseVisualizer);
        assert_eq!(
            ui,
            now_playing(Focus::Visualizer),
            "focus stays on the button that opened it"
        );
    }

    #[test]
    fn volume_keys_do_not_change_volume_inside_visualizer() {
        let mut ui = visualizer();
        let c = cfg(true, 5);
        for key in [Key::Up, Key::Down] {
            let action = handle_key(&mut ui, key, &c);
            assert!(
                !matches!(action, Action::Send(_)),
                "{key:?} gave {action:?}"
            );
        }
    }

    #[test]
    fn key_names_parse_case_insensitively() {
        assert_eq!(key_from_name("ok"), Some(Key::Ok));
        assert_eq!(key_from_name("BACK"), Some(Key::Back));
        assert_eq!(key_from_name("Left"), Some(Key::Left));
        assert_eq!(key_from_name("enter"), None);
        assert_eq!(key_from_name(""), None);
    }
}
