//! Presentation-only state from the compositor's XKB keymap and modifiers.
use xkbcommon::xkb;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeyboardStatus {
    pub caps_lock: bool,
    pub layout: Option<String>,
}

/// Focused seat metadata, selected by the keyboard producing input. Without
/// an input source (login's snapshot), report only unanimous, known state.
pub struct KeyboardStates<K> {
    focused: std::collections::HashMap<K, Option<KeyboardStatus>>,
    active: Option<K>,
}

impl<K> Default for KeyboardStates<K> {
    fn default() -> Self {
        Self {
            focused: Default::default(),
            active: None,
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone> KeyboardStates<K> {
    pub fn enter(&mut self, keyboard: K) {
        self.focused.insert(keyboard, None);
    }
    pub fn update(&mut self, keyboard: &K, status: Option<KeyboardStatus>) {
        if let Some(entry) = self.focused.get_mut(keyboard) {
            *entry = status;
        }
    }
    pub fn select(&mut self, keyboard: &K) {
        if self.focused.contains_key(keyboard) {
            self.active = Some(keyboard.clone());
        }
    }
    pub fn leave(&mut self, keyboard: &K) {
        self.focused.remove(keyboard);
        if self.active.as_ref() == Some(keyboard) {
            self.active = None;
        }
    }
    pub fn status(&self) -> Option<&KeyboardStatus> {
        if let Some(active) = &self.active {
            return self.focused.get(active)?.as_ref();
        }
        let mut states = self.focused.values();
        let first = states.next()?.as_ref()?;
        states
            .all(|state| state.as_ref() == Some(first))
            .then_some(first)
    }
}

pub fn layout_names(keymap: String) -> Vec<String> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_string(
        &context,
        keymap,
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .map(|map| {
        (0..map.num_layouts())
            .map(|i| map.layout_get_name(i).to_owned())
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seat_metadata_follows_input_and_nonselected_removal_does_not_clear_it() {
        let a = KeyboardStatus {
            caps_lock: true,
            layout: Some("French".into()),
        };
        let b = KeyboardStatus {
            caps_lock: false,
            layout: Some("English (US)".into()),
        };
        let mut seats = KeyboardStates::default();
        seats.enter(1);
        seats.update(&1, Some(a.clone()));
        assert_eq!(seats.status(), Some(&a));
        seats.enter(2);
        assert_eq!(seats.status(), None, "unknown focused seat is not guessed");
        seats.update(&2, Some(b.clone()));
        assert_eq!(seats.status(), None, "conflicting snapshots are omitted");
        seats.select(&1);
        assert_eq!(seats.status(), Some(&a));
        seats.select(&2);
        assert_eq!(
            seats.status(),
            Some(&b),
            "ordinary input switches the source"
        );
        seats.leave(&1);
        assert_eq!(seats.status(), Some(&b));
        seats.update(&2, None);
        assert_eq!(
            seats.status(),
            None,
            "keymap replacement invalidates old metadata"
        );
        seats.leave(&2);
        assert_eq!(seats.status(), None);
    }
}
