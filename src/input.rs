//! Remote/keyboard input. The state machine lands in a later step; for now only the key vocabulary.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_parse_case_insensitively() {
        assert_eq!(key_from_name("ok"), Some(Key::Ok));
        assert_eq!(key_from_name("BACK"), Some(Key::Back));
        assert_eq!(key_from_name("Left"), Some(Key::Left));
        assert_eq!(key_from_name("enter"), None);
        assert_eq!(key_from_name(""), None);
    }
}
