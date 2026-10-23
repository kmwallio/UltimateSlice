//! Preferences → Shortcuts: a searchable table of every rebindable action.
//!
//! Click a key chip to re-record it (the next key combination pressed is
//! captured; `Esc` cancels), `×` removes a chip, `+` adds another key,
//! ↶ resets a row, **Reset All** restores the defaults, **Import…/Export…**
//! exchange `keybindings.json` files. Edits apply to a working copy and are
//! committed (live keymap + file) when the Preferences dialog is saved.

use crate::keybindings::{
    load_overrides_from_path, save_overrides, ActionContext, ActionId, Chord, Keymap, ACTIONS,
};
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, Button, Label, ListBox, Orientation, SearchEntry};
use std::cell::RefCell;
use std::rc::Rc;

/// Which chip is being re-recorded: `replace` is the chord the new key
/// replaces (`None` when adding a key).
#[derive(Clone, Copy)]
struct Capture {
    id: ActionId,
    replace: Option<Chord>,
}

struct PageState {
    working: Keymap,
    capture: Option<Capture>,
    filter: String,
    notice: Option<String>,
}

/// Build the page. Returns the widget and a `commit` closure the dialog
/// calls on Save (copies the working keymap into `live` and writes the
/// overrides file).
pub fn build_shortcuts_page(
    parent: &gtk::Window,
    live: Rc<RefCell<Keymap>>,
) -> (GBox, Rc<dyn Fn()>) {
    let state = Rc::new(RefCell::new(PageState {
        working: live.borrow().clone(),
        capture: None,
        filter: String::new(),
        notice: None,
    }));

    let page = GBox::new(Orientation::Vertical, 8);
    page.set_vexpand(true);
    page.set_hexpand(true);

    let title = Label::new(Some("Keyboard Shortcuts"));
    title.add_css_class("title-4");
    title.set_halign(gtk::Align::Start);
    page.append(&title);

    let hint = Label::new(Some(
        "Click a key to change it and press the new combination (Esc cancels). × removes a key, + adds one, ↶ restores the default. Changes apply when you Save.",
    ));
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    hint.add_css_class("dim-label");
    page.append(&hint);

    let toolbar = GBox::new(Orientation::Horizontal, 6);
    let search = SearchEntry::new();
    search.set_placeholder_text(Some("Search actions or keys…"));
    search.set_hexpand(true);
    let btn_reset_all = Button::with_label("Reset All");
    let btn_import = Button::with_label("Import…");
    let btn_export = Button::with_label("Export…");
    toolbar.append(&search);
    toolbar.append(&btn_reset_all);
    toolbar.append(&btn_import);
    toolbar.append(&btn_export);
    page.append(&toolbar);

    let notice = Label::new(None);
    notice.set_xalign(0.0);
    notice.set_wrap(true);
    notice.add_css_class("dim-label");
    notice.set_visible(false);
    page.append(&notice);

    let list = ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_child(Some(&list));
    page.append(&scroll);

    // Re-render hook (filled in below; handlers clone it out before calling).
    let rerender: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let fire: Rc<dyn Fn()> = {
        let rerender = rerender.clone();
        Rc::new(move || {
            let f = rerender.borrow().clone();
            if let Some(f) = f {
                f();
            }
        })
    };

    {
        let state = state.clone();
        let list = list.clone();
        let notice = notice.clone();
        let fire = fire.clone();
        *rerender.borrow_mut() = Some(Rc::new(move || {
            render_rows(&list, &notice, &state, &fire);
        }));
    }

    // Search filter.
    {
        let state = state.clone();
        let fire = fire.clone();
        search.connect_search_changed(move |e| {
            state.borrow_mut().filter = e.text().to_string().to_lowercase();
            fire();
        });
    }
    // Reset all.
    {
        let state = state.clone();
        let fire = fire.clone();
        btn_reset_all.connect_clicked(move |_| {
            {
                let mut st = state.borrow_mut();
                st.working.reset_all();
                st.capture = None;
                st.notice = Some("All shortcuts reset to defaults (applies on Save).".into());
            }
            fire();
        });
    }
    // Export.
    {
        let state = state.clone();
        let parent = parent.clone();
        btn_export.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Export Keyboard Shortcuts");
            dialog.set_initial_name(Some("keybindings.json"));
            let overrides = state.borrow().working.to_overrides();
            dialog.save(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        match serde_json::to_string_pretty(&overrides)
                            .map_err(|e| e.to_string())
                            .and_then(|t| std::fs::write(&path, t).map_err(|e| e.to_string()))
                        {
                            Ok(()) => log::info!("shortcuts: exported to {}", path.display()),
                            Err(e) => log::error!("shortcuts: export failed: {e}"),
                        }
                    }
                }
            });
        });
    }
    // Import.
    {
        let state = state.clone();
        let parent = parent.clone();
        let fire = fire.clone();
        btn_import.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Import Keyboard Shortcuts");
            let filter = gtk::FileFilter::new();
            filter.add_pattern("*.json");
            filter.set_name(Some("Keybindings JSON"));
            let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));
            let state = state.clone();
            let fire = fire.clone();
            dialog.open(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(path) = file.path() else { return };
                match load_overrides_from_path(&path) {
                    Ok(overrides) => {
                        let (km, warnings) = Keymap::with_overrides(&overrides);
                        let mut st = state.borrow_mut();
                        st.working = km;
                        st.capture = None;
                        st.notice = Some(if warnings.is_empty() {
                            format!(
                                "Imported {} override(s) from {} (applies on Save).",
                                overrides.bindings.len(),
                                path.display()
                            )
                        } else {
                            format!(
                                "Imported with {} skipped entr{}: {}",
                                warnings.len(),
                                if warnings.len() == 1 { "y" } else { "ies" },
                                warnings.join("; ")
                            )
                        });
                    }
                    Err(e) => {
                        state.borrow_mut().notice = Some(format!("Import failed: {e}"));
                    }
                }
                fire();
            });
        });
    }
    // Key capture: a Capture-phase controller on the dialog window records
    // the next chord while a chip is armed.
    {
        let state = state.clone();
        let fire = fire.clone();
        let ctrl = gtk::EventControllerKey::new();
        ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
        ctrl.connect_key_pressed(move |_, key, _, mods| {
            let Some(cap) = state.borrow().capture else {
                return glib::Propagation::Proceed;
            };
            if Chord::is_modifier_key(key) {
                return glib::Propagation::Stop;
            }
            if key == gdk::Key::Escape {
                state.borrow_mut().capture = None;
                fire();
                return glib::Propagation::Stop;
            }
            let chord = Chord::from_event(key, mods);
            {
                let mut st = state.borrow_mut();
                if let Some(old) = cap.replace {
                    st.working.remove_chord(cap.id, &old);
                }
                st.working.add_chord(cap.id, chord);
                st.capture = None;
                st.notice = match st.working.conflict(cap.id, &chord) {
                    Some(other) => Some(format!(
                        "{} is also bound to “{}” in the same context — only one of them will fire. Remove it from one of the two.",
                        chord.label(),
                        other.title()
                    )),
                    None => None,
                };
            }
            fire();
            glib::Propagation::Stop
        });
        parent.add_controller(ctrl);
    }

    fire();

    let commit: Rc<dyn Fn()> = {
        let state = state.clone();
        Rc::new(move || {
            let working = state.borrow().working.clone();
            if *live.borrow() != working {
                *live.borrow_mut() = working;
                if let Err(e) = save_overrides(&live.borrow().to_overrides()) {
                    log::error!("shortcuts: could not save keybindings.json: {e}");
                }
            }
        })
    };
    (page, commit)
}

fn render_rows(
    list: &ListBox,
    notice: &Label,
    state: &Rc<RefCell<PageState>>,
    fire: &Rc<dyn Fn()>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let st = state.borrow();
    match &st.notice {
        Some(text) => {
            notice.set_text(text);
            notice.set_visible(true);
        }
        None => notice.set_visible(false),
    }
    let mut last_ctx: Option<ActionContext> = None;
    for info in ACTIONS {
        let chords = st.working.chords_for(info.id).to_vec();
        if !st.filter.is_empty() {
            let hay = format!(
                "{} {} {} {}",
                info.title,
                info.category,
                info.context.label(),
                chords.iter().map(|c| c.label()).collect::<Vec<_>>().join(" ")
            )
            .to_lowercase();
            if !hay.contains(&st.filter) {
                continue;
            }
        }
        if last_ctx != Some(info.context) {
            last_ctx = Some(info.context);
            let header = Label::new(Some(info.context.label()));
            header.add_css_class("title-4");
            header.set_xalign(0.0);
            header.set_margin_top(8);
            header.set_margin_bottom(2);
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            row.set_selectable(false);
            row.set_child(Some(&header));
            list.append(&row);
        }

        let row_box = GBox::new(Orientation::Vertical, 2);
        row_box.set_margin_top(4);
        row_box.set_margin_bottom(4);
        row_box.set_margin_start(6);
        row_box.set_margin_end(6);
        let line = GBox::new(Orientation::Horizontal, 8);

        let title = Label::new(Some(info.title));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_tooltip_text(Some(if info.doc.is_empty() { info.title } else { info.doc }));
        line.append(&title);

        let cat = Label::new(Some(info.category));
        cat.add_css_class("dim-label");
        cat.set_width_chars(10);
        cat.set_xalign(0.0);
        line.append(&cat);

        let chips = GBox::new(Orientation::Horizontal, 4);
        for chord in &chords {
            let capturing_this = matches!(
                st.capture,
                Some(Capture { id, replace: Some(c) }) if id == info.id && c == *chord
            );
            let chip = Button::with_label(&if capturing_this {
                "Press keys… (Esc cancels)".to_string()
            } else {
                chord.label()
            });
            chip.add_css_class("shortcut-key");
            if capturing_this {
                chip.add_css_class("suggested-action");
            }
            chip.set_tooltip_text(Some("Click, then press the new key combination"));
            {
                let state = state.clone();
                let fire = fire.clone();
                let id = info.id;
                let chord = *chord;
                chip.connect_clicked(move |_| {
                    state.borrow_mut().capture = Some(Capture {
                        id,
                        replace: Some(chord),
                    });
                    fire();
                });
            }
            chips.append(&chip);
            let remove = Button::from_icon_name("window-close-symbolic");
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some("Remove this key"));
            {
                let state = state.clone();
                let fire = fire.clone();
                let id = info.id;
                let chord = *chord;
                remove.connect_clicked(move |_| {
                    {
                        let mut st = state.borrow_mut();
                        st.working.remove_chord(id, &chord);
                        st.capture = None;
                    }
                    fire();
                });
            }
            chips.append(&remove);
        }
        let adding = matches!(st.capture, Some(Capture { id, replace: None }) if id == info.id);
        let add = if adding {
            let b = Button::with_label("Press keys… (Esc cancels)");
            b.add_css_class("suggested-action");
            b
        } else {
            let b = Button::from_icon_name("list-add-symbolic");
            b.add_css_class("flat");
            b.set_tooltip_text(Some(if chords.is_empty() {
                "Assign a key"
            } else {
                "Add another key"
            }));
            b
        };
        {
            let state = state.clone();
            let fire = fire.clone();
            let id = info.id;
            add.connect_clicked(move |_| {
                state.borrow_mut().capture = Some(Capture { id, replace: None });
                fire();
            });
        }
        chips.append(&add);
        if chords.is_empty() && !adding {
            let unbound = Label::new(Some("unbound"));
            unbound.add_css_class("dim-label");
            chips.prepend(&unbound);
        }
        line.append(&chips);

        let reset = Button::from_icon_name("edit-undo-symbolic");
        reset.add_css_class("flat");
        reset.set_tooltip_text(Some("Reset to default"));
        reset.set_sensitive(!st.working.is_default(info.id));
        {
            let state = state.clone();
            let fire = fire.clone();
            let id = info.id;
            reset.connect_clicked(move |_| {
                {
                    let mut st = state.borrow_mut();
                    st.working.reset(id);
                    st.capture = None;
                }
                fire();
            });
        }
        line.append(&reset);
        row_box.append(&line);

        // Conflict (same context) / shadow (Global ↔ panel) notes.
        for chord in &chords {
            if let Some(other) = st.working.conflict(info.id, chord) {
                let note_line = GBox::new(Orientation::Horizontal, 6);
                let note = Label::new(Some(&format!(
                    "⚠ {} is also bound to “{}” — only one will fire",
                    chord.label(),
                    other.title()
                )));
                note.add_css_class("error");
                note.set_xalign(0.0);
                note.set_hexpand(true);
                note.set_wrap(true);
                note_line.append(&note);
                let fix = Button::with_label(&format!("Remove from “{}”", other.title()));
                fix.add_css_class("flat");
                {
                    let state = state.clone();
                    let fire = fire.clone();
                    let chord = *chord;
                    fix.connect_clicked(move |_| {
                        state.borrow_mut().working.remove_chord(other, &chord);
                        fire();
                    });
                }
                note_line.append(&fix);
                row_box.append(&note_line);
            } else if let Some(other) = st.working.shadows(info.id, chord) {
                let (winner, loser) = if info.context == ActionContext::Global {
                    (other, info.id)
                } else {
                    (info.id, other)
                };
                let note = Label::new(Some(&format!(
                    "{}: “{}” ({}) takes precedence over “{}” (Global) while that panel has focus",
                    chord.label(),
                    winner.title(),
                    winner.context().label(),
                    loser.title()
                )));
                note.add_css_class("dim-label");
                note.set_xalign(0.0);
                note.set_wrap(true);
                row_box.append(&note);
            }
        }

        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.set_child(Some(&row_box));
        list.append(&row);
    }
    drop(st);
}
