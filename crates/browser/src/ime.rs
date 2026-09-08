//! Translation between winit's platform IME events and Servo's
//! composition-event lifecycle.

use servo::{CompositionEvent, CompositionState};
use winit::event::Ime;

/// Translate one platform IME event.
///
/// Winit reports preedit/commit updates, while Servo expects an
/// explicit `Start` -> `Update`* -> `End` sequence. The returned bool
/// is the composition state to retain for the next event.
pub(crate) fn translate(was_composing: bool, event: &Ime) -> (bool, Vec<CompositionEvent>) {
    match event {
        Ime::Enabled => (was_composing, Vec::new()),
        Ime::Preedit(text, _) if text.is_empty() && !was_composing => {
            // Wayland IMEs can emit a trailing empty preedit after a
            // commit. It must not start a phantom composition.
            (false, Vec::new())
        }
        Ime::Preedit(text, _) => {
            let mut events = Vec::with_capacity(if was_composing { 1 } else { 2 });
            if !was_composing {
                events.push(composition(CompositionState::Start, String::new()));
            }
            events.push(composition(CompositionState::Update, text.clone()));
            (true, events)
        }
        Ime::Commit(text) => {
            let mut events = Vec::with_capacity(if was_composing { 1 } else { 2 });
            // Some platforms can commit without first exposing a
            // preedit. Servo still requires a complete lifecycle.
            if !was_composing {
                events.push(composition(CompositionState::Start, String::new()));
            }
            events.push(composition(CompositionState::End, text.clone()));
            (false, events)
        }
        Ime::Disabled => {
            if was_composing {
                (
                    false,
                    vec![composition(CompositionState::End, String::new())],
                )
            } else {
                (false, Vec::new())
            }
        }
    }
}

/// Composition state for egui-owned text fields. This deliberately
/// mirrors [`translate`] without manufacturing Servo events.
pub(crate) fn is_composing_after(was_composing: bool, event: &Ime) -> bool {
    match event {
        Ime::Enabled => was_composing,
        Ime::Preedit(text, _) => was_composing || !text.is_empty(),
        Ime::Commit(_) | Ime::Disabled => false,
    }
}

pub(crate) fn cancel() -> CompositionEvent {
    composition(CompositionState::End, String::new())
}

fn composition(state: CompositionState, data: String) -> CompositionEvent {
    CompositionEvent { state, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(events: &[CompositionEvent]) -> Vec<CompositionState> {
        events.iter().map(|event| event.state).collect()
    }

    #[test]
    fn preedit_starts_then_updates_a_composition() {
        let (active, events) = translate(false, &Ime::Preedit("に".into(), Some((3, 3))));
        assert!(active);
        assert_eq!(
            states(&events),
            vec![CompositionState::Start, CompositionState::Update]
        );
        assert_eq!(events[0].data, "");
        assert_eq!(events[1].data, "に");

        let (active, events) = translate(true, &Ime::Preedit("日本".into(), Some((6, 6))));
        assert!(active);
        assert_eq!(states(&events), vec![CompositionState::Update]);
        assert_eq!(events[0].data, "日本");
    }

    #[test]
    fn commit_ends_exactly_once() {
        let (active, events) = translate(true, &Ime::Commit("日本".into()));
        assert!(!active);
        assert_eq!(states(&events), vec![CompositionState::End]);
        assert_eq!(events[0].data, "日本");
    }

    #[test]
    fn direct_commit_gets_a_complete_lifecycle() {
        let (active, events) = translate(false, &Ime::Commit("é".into()));
        assert!(!active);
        assert_eq!(
            states(&events),
            vec![CompositionState::Start, CompositionState::End]
        );
        assert_eq!(events[1].data, "é");
    }

    #[test]
    fn disabled_cancels_only_an_active_composition() {
        let (active, events) = translate(true, &Ime::Disabled);
        assert!(!active);
        assert_eq!(states(&events), vec![CompositionState::End]);
        assert_eq!(events[0].data, "");

        let (_, events) = translate(false, &Ime::Disabled);
        assert!(events.is_empty());
    }

    #[test]
    fn empty_preedit_only_updates_an_active_composition() {
        let event = Ime::Preedit(String::new(), None);

        let (active, events) = translate(false, &event);
        assert!(!active);
        assert!(events.is_empty());
        assert!(!is_composing_after(false, &event));

        let (active, events) = translate(true, &event);
        assert!(active);
        assert_eq!(states(&events), vec![CompositionState::Update]);
        assert!(events[0].data.is_empty());
        assert!(is_composing_after(true, &event));
    }
}
