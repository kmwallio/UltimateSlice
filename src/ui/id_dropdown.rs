//! GTK4 `DropDown` wrapper whose rows carry stable string ids.
//!
//! Replacement for the deprecated `ComboBoxText` id API: selection state is
//! read and written by *id* (what persisted preferences and model objects
//! store), never by row index, so repopulating the rows can't silently
//! re-target a selection.

use gtk4 as gtk;
use gtk4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct IdDropDown {
    pub dropdown: gtk::DropDown,
    ids: Rc<RefCell<Vec<String>>>,
}

impl IdDropDown {
    /// Empty dropdown; populate with [`IdDropDown::set_entries`].
    pub fn new() -> Self {
        Self {
            dropdown: gtk::DropDown::from_strings(&[]),
            ids: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Replace all rows with `(id, label)` entries and select `active_id`
    /// (first row when the id is unknown). Like the `ComboBoxText` it
    /// replaces, this fires selection notifications for the programmatic
    /// change — callers keep their `updating` re-entrancy guards.
    pub fn set_entries(&self, entries: &[(String, String)], active_id: &str) {
        let labels: Vec<&str> = entries.iter().map(|(_, l)| l.as_str()).collect();
        let model = gtk::StringList::new(&labels);
        *self.ids.borrow_mut() = entries.iter().map(|(id, _)| id.clone()).collect();
        self.dropdown.set_model(Some(&model));
        let pos = entries
            .iter()
            .position(|(id, _)| id == active_id)
            .unwrap_or(0);
        self.dropdown.set_selected(pos as u32);
    }

    /// Select the row with `id`; falls back to the first row when unknown.
    pub fn set_active_id(&self, id: &str) {
        let pos = self.ids.borrow().iter().position(|i| i == id).unwrap_or(0);
        self.dropdown.set_selected(pos as u32);
    }

    /// Stable id of the selected row (`None` when the dropdown is empty).
    pub fn active_id(&self) -> Option<String> {
        self.ids
            .borrow()
            .get(self.dropdown.selected() as usize)
            .cloned()
    }

    /// Display label of the selected row.
    pub fn active_label(&self) -> Option<String> {
        self.dropdown
            .model()?
            .item(self.dropdown.selected())?
            .downcast::<gtk::StringObject>()
            .ok()
            .map(|s| s.string().to_string())
    }

    /// Notify on selection change. Fires for programmatic changes too, like
    /// `ComboBoxText::connect_changed` did.
    pub fn connect_selected(&self, f: impl Fn(&IdDropDown) + 'static) {
        let this = self.clone();
        self.dropdown.connect_selected_notify(move |_| f(&this));
    }
}

impl Default for IdDropDown {
    fn default() -> Self {
        Self::new()
    }
}
