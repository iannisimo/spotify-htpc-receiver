//! The settings panel as a state machine: which row or view is showing, and what each remote
//! button asks the app to do. No Slint and no I/O here; `ui.rs` carries out the [`Effect`]s.

use std::path::PathBuf;

use crate::config::PairingMode;
use crate::input::Key;
use crate::viz::tree::PresetTree;

/// Beat sensitivity: 0.0–5.0 in steps of 0.1.
const SENSITIVITY_MAX: f32 = 5.0;
const SENSITIVITY_STEP: f32 = 0.1;
/// Preset duration: 5–300 s in steps of 5.
const DURATION_MIN: f64 = 5.0;
const DURATION_MAX: f64 = 300.0;
const DURATION_STEP: f64 = 5.0;
/// Visual delay: 0–2000 ms in steps of 10.
const DELAY_MAX: u32 = 2000;
const DELAY_STEP: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Pairing,
    BeatSensitivity,
    PresetDuration,
    Shuffle,
    VisualDelay,
    Calibrate,
    Presets,
}

impl Row {
    pub const ALL: [Row; 7] = [
        Row::Pairing,
        Row::BeatSensitivity,
        Row::PresetDuration,
        Row::Shuffle,
        Row::VisualDelay,
        Row::Calibrate,
        Row::Presets,
    ];

    fn step(self, delta: isize) -> Row {
        let at = Row::ALL.iter().position(|&r| r == self).unwrap_or(0);
        Row::ALL[at.saturating_add_signed(delta).min(Row::ALL.len() - 1)]
    }
}

/// The values the panel shows and edits.
#[derive(Debug, Clone, PartialEq)]
pub struct Values {
    pub pairing: PairingMode,
    pub beat_sensitivity: f32,
    pub preset_duration_s: f64,
    pub shuffle: bool,
    pub delay_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelView {
    Main(Row),
    Calibrate,
    Presets,
}

/// What the app should do after a button press.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Close the panel.
    Close,
    Pairing(PairingMode),
    BeatSensitivity(f32),
    PresetDuration(f64),
    Shuffle(bool),
    Delay(u32),
    StartCalibration,
    StopCalibration,
    /// The Presets view opened: scan the folder and call [`Panel::set_tree`].
    OpenPresets,
    /// A preset or folder was switched on or off: save and apply.
    PresetsChanged,
    /// Show this preset in the visualizer (switching automatic changes off).
    Demo(PathBuf),
    /// The Presets view closed: end any demo, and close a visualizer opened only for it.
    ClosePresets,
}

#[derive(Debug)]
pub struct Panel {
    values: Values,
    view: PanelView,
    tree: Option<PresetTree>,
}

impl Panel {
    pub fn new(values: Values) -> Panel {
        Panel {
            values,
            view: PanelView::Main(Row::Pairing),
            tree: None,
        }
    }

    pub fn view(&self) -> PanelView {
        self.view
    }

    pub fn values(&self) -> &Values {
        &self.values
    }

    pub fn set_tree(&mut self, tree: PresetTree) {
        self.tree = Some(tree);
    }

    pub fn tree(&self) -> Option<&PresetTree> {
        self.tree.as_ref()
    }

    pub fn tree_mut(&mut self) -> Option<&mut PresetTree> {
        self.tree.as_mut()
    }

    /// Applies one button press. The effects come in the order they must be carried out.
    pub fn key(&mut self, key: Key) -> Vec<Effect> {
        match self.view {
            PanelView::Main(row) => self.main_key(row, key),
            PanelView::Calibrate => self.calibrate_key(key),
            PanelView::Presets => self.presets_key(key),
        }
    }

    fn main_key(&mut self, row: Row, key: Key) -> Vec<Effect> {
        match key {
            Key::Up => self.view = PanelView::Main(row.step(-1)),
            Key::Down => self.view = PanelView::Main(row.step(1)),
            Key::Left => return self.adjust(row, -1),
            Key::Right => return self.adjust(row, 1),
            Key::Ok => return self.activate(row),
            Key::Back | Key::Settings => return vec![Effect::Close],
        }
        vec![]
    }

    /// Left/Right on a row: change its value one step.
    fn adjust(&mut self, row: Row, direction: i8) -> Vec<Effect> {
        let up = direction > 0;
        let v = &mut self.values;
        match row {
            Row::Pairing | Row::Shuffle => self.activate(row),
            Row::BeatSensitivity => {
                let steps = (v.beat_sensitivity / SENSITIVITY_STEP).round() as i32;
                let steps = if up { steps + 1 } else { steps - 1 };
                let value = (steps as f32 * SENSITIVITY_STEP).clamp(0.0, SENSITIVITY_MAX);
                // Rounded to one decimal so repeated steps never drift.
                let value = (value * 10.0).round() / 10.0;
                if value == v.beat_sensitivity {
                    return vec![];
                }
                v.beat_sensitivity = value;
                vec![Effect::BeatSensitivity(value)]
            }
            Row::PresetDuration => {
                let value = step_duration(v.preset_duration_s, up);
                if value == v.preset_duration_s {
                    return vec![];
                }
                v.preset_duration_s = value;
                vec![Effect::PresetDuration(value)]
            }
            Row::VisualDelay => self.step_delay(up),
            Row::Calibrate | Row::Presets => vec![],
        }
    }

    fn step_delay(&mut self, up: bool) -> Vec<Effect> {
        let current = self.values.delay_ms;
        let value = if up {
            (current / DELAY_STEP * DELAY_STEP + DELAY_STEP).min(DELAY_MAX)
        } else if current.is_multiple_of(DELAY_STEP) {
            current.saturating_sub(DELAY_STEP)
        } else {
            current / DELAY_STEP * DELAY_STEP
        };
        if value == current {
            return vec![];
        }
        self.values.delay_ms = value;
        vec![Effect::Delay(value)]
    }

    fn activate(&mut self, row: Row) -> Vec<Effect> {
        match row {
            Row::Pairing => {
                self.values.pairing = match self.values.pairing {
                    PairingMode::Off => PairingMode::On,
                    PairingMode::On => PairingMode::Off,
                };
                vec![Effect::Pairing(self.values.pairing)]
            }
            Row::Shuffle => {
                self.values.shuffle = !self.values.shuffle;
                vec![Effect::Shuffle(self.values.shuffle)]
            }
            Row::Calibrate => {
                self.view = PanelView::Calibrate;
                vec![Effect::StartCalibration]
            }
            Row::Presets => {
                self.view = PanelView::Presets;
                self.tree = None;
                vec![Effect::OpenPresets]
            }
            Row::BeatSensitivity | Row::PresetDuration | Row::VisualDelay => vec![],
        }
    }

    fn calibrate_key(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Left => self.step_delay(false),
            Key::Right => self.step_delay(true),
            Key::Back => {
                self.view = PanelView::Main(Row::Calibrate);
                vec![Effect::StopCalibration]
            }
            Key::Settings => vec![Effect::StopCalibration, Effect::Close],
            Key::Up | Key::Down | Key::Ok => vec![],
        }
    }

    fn presets_key(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Back => {
                self.view = PanelView::Main(Row::Presets);
                self.tree = None;
                return vec![Effect::ClosePresets];
            }
            Key::Settings => {
                self.tree = None;
                return vec![Effect::ClosePresets, Effect::Close];
            }
            _ => {}
        }
        let Some(tree) = self.tree.as_mut() else {
            return vec![]; // still scanning
        };
        match key {
            Key::Up => tree.move_cursor(-1),
            Key::Down => tree.move_cursor(1),
            Key::Left => tree.collapse(),
            Key::Right => match tree.cursor_preset() {
                Some(path) => {
                    tree.set_demo(Some(path.clone()));
                    return vec![Effect::Demo(path)];
                }
                None => tree.expand(),
            },
            Key::Ok => {
                tree.toggle();
                return vec![Effect::PresetsChanged];
            }
            Key::Back | Key::Settings => {}
        }
        vec![]
    }
}

/// The next duration in the direction asked: multiples of 5 within 5–300. A larger value from
/// the file is left alone going up, and goes down to 300.
fn step_duration(current: f64, up: bool) -> f64 {
    if up {
        if current >= DURATION_MAX {
            return current;
        }
        ((current / DURATION_STEP).floor() * DURATION_STEP + DURATION_STEP).min(DURATION_MAX)
    } else if current > DURATION_MAX {
        DURATION_MAX
    } else {
        ((current / DURATION_STEP).ceil() * DURATION_STEP - DURATION_STEP).max(DURATION_MIN)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::viz::disabled::DisabledPresets;

    fn values() -> Values {
        Values {
            pairing: PairingMode::Off,
            beat_sensitivity: 1.0,
            preset_duration_s: 30.0,
            shuffle: true,
            delay_ms: 45,
        }
    }

    fn panel_on(row: Row) -> Panel {
        let mut p = Panel::new(values());
        for _ in 0..Row::ALL.iter().position(|&r| r == row).unwrap() {
            p.key(Key::Down);
        }
        assert_eq!(p.view(), PanelView::Main(row));
        p
    }

    fn sample_tree() -> PresetTree {
        let root = PathBuf::from("/p");
        let list: Vec<PathBuf> = ["A/a1.milk", "A/sub/a2.milk", "B/b1.milk", "top.milk"]
            .iter()
            .map(|r| root.join(r))
            .collect();
        PresetTree::build(&root, &list, &DisabledPresets::default())
    }

    /// A panel in the Presets view with the sample tree loaded.
    fn presets_panel() -> Panel {
        let mut p = panel_on(Row::Presets);
        assert_eq!(p.key(Key::Ok), [Effect::OpenPresets]);
        p.set_tree(sample_tree());
        p
    }

    #[test]
    fn opens_on_the_first_row_and_up_stays() {
        let mut p = Panel::new(values());
        assert_eq!(p.view(), PanelView::Main(Row::Pairing));
        assert_eq!(p.key(Key::Up), []);
        assert_eq!(p.view(), PanelView::Main(Row::Pairing));
        for _ in 0..20 {
            p.key(Key::Down);
        }
        assert_eq!(
            p.view(),
            PanelView::Main(Row::Presets),
            "clamped at the last row"
        );
    }

    #[test]
    fn pairing_toggles_on_ok_and_left_right() {
        let mut p = Panel::new(values());
        assert_eq!(p.key(Key::Ok), [Effect::Pairing(PairingMode::On)]);
        assert_eq!(p.key(Key::Right), [Effect::Pairing(PairingMode::Off)]);
        assert_eq!(p.key(Key::Left), [Effect::Pairing(PairingMode::On)]);
        assert_eq!(p.values().pairing, PairingMode::On);
    }

    #[test]
    fn beat_sensitivity_steps_and_clamps() {
        let mut p = panel_on(Row::BeatSensitivity);
        assert_eq!(p.key(Key::Right), [Effect::BeatSensitivity(1.1)]);
        assert_eq!(p.key(Key::Left), [Effect::BeatSensitivity(1.0)]);
        for _ in 0..100 {
            p.key(Key::Right);
        }
        assert_eq!(p.values().beat_sensitivity, 5.0);
        assert_eq!(p.key(Key::Right), [], "no change, no effect");
        for _ in 0..100 {
            p.key(Key::Left);
        }
        assert_eq!(p.values().beat_sensitivity, 0.0);
        assert_eq!(p.key(Key::Left), []);
    }

    #[test]
    fn preset_duration_steps_and_large_value() {
        let mut p = panel_on(Row::PresetDuration);
        assert_eq!(p.key(Key::Right), [Effect::PresetDuration(35.0)]);
        assert_eq!(p.key(Key::Left), [Effect::PresetDuration(30.0)]);
        // A file value in between lands on the next multiple.
        assert_eq!(step_duration(32.5, true), 35.0);
        assert_eq!(step_duration(32.5, false), 30.0);
        // Limits.
        assert_eq!(step_duration(5.0, false), 5.0);
        assert_eq!(step_duration(1.0, false), 5.0);
        assert_eq!(step_duration(300.0, true), 300.0);
        // Above the range: shown as is, stepping down lands on 300.
        assert_eq!(step_duration(400.0, true), 400.0);
        assert_eq!(step_duration(400.0, false), 300.0);

        let mut p = Panel::new(Values {
            preset_duration_s: 400.0,
            ..values()
        });
        p.key(Key::Down);
        p.key(Key::Down);
        assert_eq!(p.key(Key::Right), []);
        assert_eq!(p.key(Key::Left), [Effect::PresetDuration(300.0)]);
    }

    #[test]
    fn delay_steps_by_10_and_clamps() {
        let mut p = panel_on(Row::VisualDelay);
        assert_eq!(
            p.key(Key::Right),
            [Effect::Delay(50)],
            "45 goes to the next 10"
        );
        assert_eq!(p.key(Key::Left), [Effect::Delay(40)]);
        p.values.delay_ms = 0;
        assert_eq!(p.key(Key::Left), []);
        p.values.delay_ms = 2000;
        assert_eq!(p.key(Key::Right), []);
        p.values.delay_ms = 1995;
        assert_eq!(p.key(Key::Right), [Effect::Delay(2000)]);
    }

    #[test]
    fn ok_toggles_shuffle() {
        let mut p = panel_on(Row::Shuffle);
        assert_eq!(p.key(Key::Ok), [Effect::Shuffle(false)]);
        assert_eq!(p.key(Key::Ok), [Effect::Shuffle(true)]);
    }

    #[test]
    fn ok_on_value_rows_does_nothing() {
        for row in [Row::BeatSensitivity, Row::PresetDuration, Row::VisualDelay] {
            assert_eq!(panel_on(row).key(Key::Ok), [], "{row:?}");
        }
    }

    #[test]
    fn calibrate_enter_and_back() {
        let mut p = panel_on(Row::Calibrate);
        assert_eq!(p.key(Key::Ok), [Effect::StartCalibration]);
        assert_eq!(p.view(), PanelView::Calibrate);
        assert_eq!(p.key(Key::Right), [Effect::Delay(50)]);
        assert_eq!(p.key(Key::Left), [Effect::Delay(40)]);
        assert_eq!(p.key(Key::Back), [Effect::StopCalibration]);
        assert_eq!(p.view(), PanelView::Main(Row::Calibrate));
        assert_eq!(p.values().delay_ms, 40, "the delay found stays");
    }

    #[test]
    fn calibrate_stops_when_the_panel_closes() {
        let mut p = panel_on(Row::Calibrate);
        p.key(Key::Ok);
        assert_eq!(
            p.key(Key::Settings),
            [Effect::StopCalibration, Effect::Close]
        );
    }

    #[test]
    fn presets_navigation_and_toggle() {
        let mut p = panel_on(Row::Presets);
        assert_eq!(p.key(Key::Ok), [Effect::OpenPresets]);
        assert_eq!(p.view(), PanelView::Presets);
        assert!(p.tree().is_none());
        assert_eq!(p.key(Key::Down), [], "nothing to move in before the scan");
        p.set_tree(sample_tree());
        assert_eq!(
            p.key(Key::Ok),
            [Effect::PresetsChanged],
            "folder A switched off"
        );
        assert_eq!(p.tree().unwrap().counts(), (4, 2));
        assert_eq!(p.key(Key::Ok), [Effect::PresetsChanged], "and on again");
        assert_eq!(p.tree().unwrap().counts(), (4, 0));
        assert_eq!(p.key(Key::Back), [Effect::ClosePresets]);
        assert_eq!(p.view(), PanelView::Main(Row::Presets));
        assert!(p.tree().is_none());
    }

    #[test]
    fn right_demos_presets_and_expands_folders() {
        let mut p = presets_panel();
        assert_eq!(p.key(Key::Right), [], "on a folder it expands");
        assert_eq!(
            p.tree().unwrap().visible_rows(10).len(),
            6,
            "/, A, sub, a1, B, top"
        );
        p.key(Key::Down); // sub
        p.key(Key::Down); // a1
        assert_eq!(
            p.key(Key::Right),
            [Effect::Demo(Path::new("/p/A/a1.milk").to_owned())]
        );
        let rows = p.tree().unwrap().visible_rows(10);
        assert_eq!(rows.iter().filter(|r| r.demo).count(), 1);
        assert!(rows.iter().any(|r| r.demo && r.label == "a1"));
        // Switched-off presets can be demoed too.
        assert_eq!(p.key(Key::Ok), [Effect::PresetsChanged]);
        assert!(matches!(p.key(Key::Right).as_slice(), [Effect::Demo(_)]));
        // Left collapses, then walks up.
        assert_eq!(p.key(Key::Left), []);
        assert!(
            p.tree().unwrap().visible_rows(10)[1].focused,
            "on A, below the root"
        );
    }

    #[test]
    fn every_exit_from_presets_ends_demo() {
        let mut p = presets_panel();
        p.key(Key::Down); // B
        p.key(Key::Down); // top
        assert!(matches!(p.key(Key::Right).as_slice(), [Effect::Demo(_)]));
        assert_eq!(p.key(Key::Back), [Effect::ClosePresets]);

        let mut p = presets_panel();
        assert_eq!(
            p.key(Key::Settings),
            [Effect::ClosePresets, Effect::Close],
            "ClosePresets comes before Close"
        );
        assert!(p.tree().is_none());
    }

    #[test]
    fn back_and_settings_key_close_from_main() {
        assert_eq!(panel_on(Row::Shuffle).key(Key::Back), [Effect::Close]);
        assert_eq!(panel_on(Row::Shuffle).key(Key::Settings), [Effect::Close]);
    }
}
