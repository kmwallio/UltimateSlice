//! GTK glue for the keyboard shortcut registry (`crate::keybindings`).
//!
//! One `EventControllerKey` per context resolves key events through the
//! user's [`Keymap`] and invokes the handler registered in the [`ActionMap`].
//! Handlers are cloned out before being called so no `RefCell` borrow of the
//! keymap or the action map is held while they run (a handler may open the
//! Preferences dialog, which mutates the keymap).

use crate::keybindings::{ActionContext, ActionMap, Chord, Keymap};
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Pre-resolver hook: a context may handle a key itself (the timeline's
/// Precision Trim sub-mode and multicam angle keys) by returning
/// `Some(propagation)`; `None` continues to keymap resolution.
pub(crate) type PreHook = Rc<dyn Fn(gdk::Key, gdk::ModifierType) -> Option<glib::Propagation>>;

/// In GTK4, `Entry` delegates keyboard focus to an internal `gtk4::Text`
/// child, so `focused.is::<Entry>()` returns false when the user is typing
/// in an Entry. We check for `Text`, `Entry`, `SearchEntry`, `TextView`,
/// and `SpinButton` (which also contains a Text internally).
pub(crate) fn is_text_input_focused(focused: &gtk4::Widget) -> bool {
    focused.is::<gtk4::Text>()
        || focused.is::<gtk4::Entry>()
        || focused.is::<gtk4::SearchEntry>()
        || focused.is::<gtk4::TextView>()
        || focused.is::<gtk4::SpinButton>()
}

/// True when the key should go to a focused text field instead of a
/// shortcut: a text widget has focus and the chord carries neither Ctrl
/// nor Alt (so `Ctrl+Z`, `Ctrl+,` … keep working while typing, while
/// `Space`, letters and `Delete` reach the field).
pub(crate) fn text_input_blocks(ctrl: &gtk4::EventControllerKey, chord: &Chord) -> bool {
    if chord.mods.contains(gdk::ModifierType::CONTROL_MASK)
        || chord.mods.contains(gdk::ModifierType::ALT_MASK)
    {
        return false;
    }
    ctrl.widget()
        .and_then(|w| w.root())
        .and_then(|r| r.focus())
        .map(|focused| is_text_input_focused(&focused))
        .unwrap_or(false)
}

/// Install the key resolver for `ctx` on `widget`.
///
/// Order per key press: modifier-only keys are ignored → the chord is
/// normalised → focused text fields win (see [`text_input_blocks`]) → the
/// optional `pre` hook → `keymap.resolve(ctx, chord)` → the registered
/// handler. Propagation stops only when a handler reports it handled the
/// key, so Global (Capture phase on the window) never swallows a key that
/// a focused panel binds in its own context.
pub(crate) fn install_key_resolver(
    widget: &impl IsA<gtk4::Widget>,
    ctx: ActionContext,
    phase: gtk4::PropagationPhase,
    keymap: Rc<RefCell<Keymap>>,
    actions: Rc<RefCell<ActionMap>>,
    pre: Option<PreHook>,
) {
    let key_ctrl = gtk4::EventControllerKey::new();
    key_ctrl.set_propagation_phase(phase);
    key_ctrl.connect_key_pressed(move |ctrl, key, _, state| {
        if Chord::is_modifier_key(key) {
            return glib::Propagation::Proceed;
        }
        let chord = Chord::from_event(key, state);
        if text_input_blocks(ctrl, &chord) {
            return glib::Propagation::Proceed;
        }
        if let Some(pre) = pre.as_ref() {
            if let Some(result) = pre(key, state) {
                return result;
            }
        }
        let id = keymap.borrow().resolve(ctx, &chord);
        let Some(id) = id else {
            return glib::Propagation::Proceed;
        };
        let handler = actions.borrow().handler(id);
        let Some(handler) = handler else {
            log::debug!("key_dispatch: {} resolved to {:?} but no handler is bound", chord.label(), id);
            return glib::Propagation::Proceed;
        };
        if handler() {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    widget.add_controller(key_ctrl);
}
