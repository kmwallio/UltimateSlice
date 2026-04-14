//! Ctrl+Shift+P fuzzy command palette.
//!
//! `CommandRegistry` holds a flat list of UI commands (label + category +
//! optional shortcut hint + handler). `show_palette` builds a transient modal
//! window with a SearchEntry over a filtered ListBox. Selection is committed
//! on Enter; Escape closes.

use std::cell::RefCell;
use std::rc::Rc;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use gtk4 as gtk;
use gtk4::gdk::Key;
use gtk4::glib;
use gtk4::prelude::*;

pub struct Command {
    pub title: String,
    pub category: &'static str,
    pub shortcut: Option<String>,
    pub handler: Rc<dyn Fn()>,
}

#[derive(Default)]
pub struct CommandRegistry {
    commands: Vec<Command>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(
        &mut self,
        title: impl Into<String>,
        category: &'static str,
        shortcut: Option<&str>,
        handler: Rc<dyn Fn()>,
    ) {
        self.commands.push(Command {
            title: title.into(),
            category,
            shortcut: shortcut.map(|s| s.to_string()),
            handler,
        });
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Returns indices of commands matching `query`, ranked best-first.
    /// Empty query → every index, sorted by `(category, title)`.
    pub fn fuzzy_match(&self, query: &str) -> Vec<usize> {
        if query.trim().is_empty() {
            let mut idxs: Vec<usize> = (0..self.commands.len()).collect();
            idxs.sort_by(|&a, &b| {
                let ca = self.commands[a].category;
                let cb = self.commands[b].category;
                ca.cmp(cb)
                    .then_with(|| self.commands[a].title.cmp(&self.commands[b].title))
            });
            return idxs;
        }
        let matcher = SkimMatcherV2::default();
        let mut scored: Vec<(i64, usize)> = self
            .commands
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let haystack = format!("{} {}", c.category, c.title);
                matcher.fuzzy_match(&haystack, query).map(|s| (s, i))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| {
                let ca = self.commands[a.1].category;
                let cb = self.commands[b.1].category;
                ca.cmp(cb)
                    .then_with(|| self.commands[a.1].title.cmp(&self.commands[b.1].title))
            })
        });
        scored.into_iter().map(|(_, i)| i).collect()
    }
}

/// Open (or re-focus) the palette as a modal window transient to `parent`.
pub fn show_palette(parent: &gtk::Window, registry: Rc<RefCell<CommandRegistry>>) {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .decorated(false)
        .default_width(560)
        .default_height(420)
        .title("Command Palette")
        .build();
    window.add_css_class("command-palette");

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Type a command…"));
    vbox.append(&search);

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Browse);
    list.add_css_class("command-palette-list");
    scrolled.set_child(Some(&list));
    vbox.append(&scrolled);
    window.set_child(Some(&vbox));

    // Map from ListBoxRow index → command index; rebuilt on every refilter.
    let mapping: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    let refill = {
        let registry = registry.clone();
        let list = list.clone();
        let mapping = mapping.clone();
        Rc::new(move |query: &str| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let reg = registry.borrow();
            let matches = reg.fuzzy_match(query);
            let mut new_mapping = Vec::with_capacity(matches.len());
            for &i in matches.iter().take(200) {
                let cmd = &reg.commands[i];
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row_box.set_margin_start(4);
                row_box.set_margin_end(4);

                let cat_lbl = gtk::Label::new(Some(cmd.category));
                cat_lbl.add_css_class("cmd-category");
                cat_lbl.set_width_chars(10);
                cat_lbl.set_xalign(0.0);
                row_box.append(&cat_lbl);

                let title_lbl = gtk::Label::new(Some(&cmd.title));
                title_lbl.set_hexpand(true);
                title_lbl.set_xalign(0.0);
                row_box.append(&title_lbl);

                if let Some(sc) = cmd.shortcut.as_ref() {
                    let sc_lbl = gtk::Label::new(Some(sc));
                    sc_lbl.add_css_class("cmd-shortcut");
                    row_box.append(&sc_lbl);
                }

                let row = gtk::ListBoxRow::new();
                row.set_child(Some(&row_box));
                list.append(&row);
                new_mapping.push(i);
            }
            *mapping.borrow_mut() = new_mapping;
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
            }
        })
    };

    refill("");

    {
        let refill = refill.clone();
        search.connect_search_changed(move |entry| {
            refill(&entry.text());
        });
    }

    // Invoke currently selected command and close.
    let invoke = {
        let list = list.clone();
        let mapping = mapping.clone();
        let registry = registry.clone();
        let window = window.clone();
        Rc::new(move || {
            if let Some(row) = list.selected_row() {
                let idx = row.index();
                if idx >= 0 {
                    if let Some(&cmd_idx) = mapping.borrow().get(idx as usize) {
                        let handler = registry.borrow().commands[cmd_idx].handler.clone();
                        window.close();
                        handler();
                        return;
                    }
                }
            }
            window.close();
        })
    };

    {
        let invoke = invoke.clone();
        search.connect_activate(move |_| invoke());
    }
    {
        let invoke = invoke.clone();
        list.connect_row_activated(move |_, _| invoke());
    }

    // Key handling: Up/Down route to the list, Esc closes, typing stays in entry.
    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let window = window.clone();
        let list = list.clone();
        let invoke = invoke.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, _mods| match key {
            Key::Escape => {
                window.close();
                glib::Propagation::Stop
            }
            Key::Down => {
                let next = list.selected_row().map(|r| r.index() + 1).unwrap_or(0);
                if let Some(row) = list.row_at_index(next) {
                    list.select_row(Some(&row));
                    row.grab_focus();
                }
                glib::Propagation::Stop
            }
            Key::Up => {
                let cur = list.selected_row().map(|r| r.index()).unwrap_or(0);
                if cur > 0 {
                    if let Some(row) = list.row_at_index(cur - 1) {
                        list.select_row(Some(&row));
                        row.grab_focus();
                    }
                }
                glib::Propagation::Stop
            }
            Key::Return | Key::KP_Enter => {
                invoke();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
    }
    window.add_controller(key_ctrl);

    window.present();
    search.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        let noop: Rc<dyn Fn()> = Rc::new(|| {});
        r.push("Save Project", "Project", Some("Ctrl+S"), noop.clone());
        r.push("Save As…", "Project", Some("Ctrl+Shift+S"), noop.clone());
        r.push("Open…", "Project", Some("Ctrl+O"), noop.clone());
        r.push("Razor tool", "Tools", Some("B"), noop.clone());
        r.push("Ripple Delete", "Edit", Some("Shift+Del"), noop);
        r
    }

    #[test]
    fn empty_query_returns_all_sorted() {
        let r = reg();
        let idxs = r.fuzzy_match("");
        assert_eq!(idxs.len(), r.len());
    }

    #[test]
    fn save_pr_ranks_save_commands_top() {
        let r = reg();
        let idxs = r.fuzzy_match("save pr");
        assert!(idxs.len() >= 1);
        let top = &r.commands[idxs[0]].title;
        assert!(
            top.starts_with("Save"),
            "expected a Save command on top, got {top}"
        );
    }

    #[test]
    fn rip_matches_ripple_delete() {
        let r = reg();
        let idxs = r.fuzzy_match("rip");
        let titles: Vec<_> = idxs.iter().map(|&i| r.commands[i].title.as_str()).collect();
        assert!(titles.contains(&"Ripple Delete"), "got {titles:?}");
    }
}
