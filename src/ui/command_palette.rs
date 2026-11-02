//! Ctrl+Shift+P fuzzy command palette.
//!
//! `CommandRegistry` holds a flat list of UI commands (label + category +
//! optional shortcut hint + optional detail line + handler). `show_palette`
//! builds a transient modal window with a SearchEntry over a filtered
//! ListBox. Selection is committed on Enter; Escape closes.
//!
//! Phase 2: the registry remembers the last `MAX_RECENT` commands that were
//! run (persisted to `palette-recent.json` through the `persist` hook, listed
//! first while the query is empty), and `window.rs` registers every
//! argument-free MCP tool as an "MCP" command whose JSON result is shown in a
//! dialog (`show_tool_result_dialog`).

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
    /// One dimmed line under the title (an MCP tool's description).
    pub detail: Option<String>,
    pub handler: Rc<dyn Fn()>,
}

/// How many recently run commands the palette remembers.
pub const MAX_RECENT: usize = 8;

#[derive(Default)]
pub struct CommandRegistry {
    commands: Vec<Command>,
    /// Titles of recently run commands, most recent first.
    recent: Vec<String>,
    /// Called with the recent list whenever it changes (persistence).
    persist: Option<Rc<dyn Fn(&[String])>>,
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
            detail: None,
            handler,
        });
    }

    /// `push` with a detail line.
    pub fn push_with_detail(
        &mut self,
        title: impl Into<String>,
        category: &'static str,
        shortcut: Option<&str>,
        detail: impl Into<String>,
        handler: Rc<dyn Fn()>,
    ) {
        self.commands.push(Command {
            title: title.into(),
            category,
            shortcut: shortcut.map(|s| s.to_string()),
            detail: Some(detail.into()),
            handler,
        });
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Index of the command with this exact (case-insensitive) title.
    pub fn index_of_title(&self, title: &str) -> Option<usize> {
        let t = title.trim();
        self.commands
            .iter()
            .position(|c| c.title.eq_ignore_ascii_case(t))
    }

    /// Handler for `title`: exact title first, else the best fuzzy match.
    /// Returns the matched title with the handler so callers can report it.
    pub fn resolve(&self, title: &str) -> Option<(String, Rc<dyn Fn()>)> {
        let idx = self
            .index_of_title(title)
            .or_else(|| self.fuzzy_match(title).into_iter().next())?;
        let c = &self.commands[idx];
        Some((c.title.clone(), c.handler.clone()))
    }

    pub fn recent(&self) -> &[String] {
        &self.recent
    }

    pub fn is_recent(&self, title: &str) -> bool {
        self.recent.iter().any(|r| r.eq_ignore_ascii_case(title))
    }

    /// Replace the recent list (startup load); unknown titles are kept so a
    /// command registered later (toolbar buttons) still shows as recent.
    pub fn set_recent(&mut self, recent: Vec<String>) {
        self.recent = recent;
        self.recent.truncate(MAX_RECENT);
    }

    pub fn set_persist(&mut self, persist: Rc<dyn Fn(&[String])>) {
        self.persist = Some(persist);
    }

    /// Move `title` to the front of the recent list (deduplicated, capped)
    /// and notify the persistence hook.
    pub fn record_use(&mut self, title: &str) {
        self.recent.retain(|r| !r.eq_ignore_ascii_case(title));
        self.recent.insert(0, title.to_string());
        self.recent.truncate(MAX_RECENT);
        if let Some(persist) = self.persist.clone() {
            persist(&self.recent);
        }
    }

    /// True when a command with this title (case-insensitive) is registered.
    pub fn has_title(&self, title: &str) -> bool {
        self.commands
            .iter()
            .any(|c| c.title.eq_ignore_ascii_case(title))
    }

    /// Drop every command in one of `categories` (used to re-register the
    /// shortcut registry's actions with fresh key labels).
    pub fn remove_categories(&mut self, categories: &[&str]) {
        self.commands.retain(|c| !categories.contains(&c.category));
    }

    /// Returns indices of commands matching `query`, ranked best-first.
    /// Empty query → recently run commands first (most recent first), then
    /// every other index sorted by `(category, title)`.
    pub fn fuzzy_match(&self, query: &str) -> Vec<usize> {
        if query.trim().is_empty() {
            let mut idxs: Vec<usize> = Vec::with_capacity(self.commands.len());
            for title in &self.recent {
                if let Some(i) = self.index_of_title(title) {
                    if !idxs.contains(&i) {
                        idxs.push(i);
                    }
                }
            }
            let mut rest: Vec<usize> = (0..self.commands.len())
                .filter(|i| !idxs.contains(i))
                .collect();
            rest.sort_by(|&a, &b| {
                let ca = self.commands[a].category;
                let cb = self.commands[b].category;
                ca.cmp(cb)
                    .then_with(|| self.commands[a].title.cmp(&self.commands[b].title))
            });
            idxs.extend(rest);
            return idxs;
        }
        let matcher = SkimMatcherV2::default();
        let mut scored: Vec<(i64, usize)> = self
            .commands
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let haystack = match &c.detail {
                    Some(d) => format!("{} {} {}", c.category, c.title, d),
                    None => format!("{} {}", c.category, c.title),
                };
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
                title_lbl.set_xalign(0.0);
                if let Some(detail) = cmd.detail.as_ref() {
                    let text_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    text_box.set_hexpand(true);
                    text_box.append(&title_lbl);
                    let detail_lbl = gtk::Label::new(Some(detail));
                    detail_lbl.add_css_class("cmd-detail");
                    detail_lbl.set_xalign(0.0);
                    detail_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    detail_lbl.set_max_width_chars(60);
                    text_box.append(&detail_lbl);
                    row_box.append(&text_box);
                } else {
                    title_lbl.set_hexpand(true);
                    row_box.append(&title_lbl);
                }

                if reg.is_recent(&cmd.title) {
                    let recent_lbl = gtk::Label::new(Some("recent"));
                    recent_lbl.add_css_class("cmd-recent");
                    row_box.append(&recent_lbl);
                }

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
                        let (title, handler) = {
                            let reg = registry.borrow();
                            let c = &reg.commands[cmd_idx];
                            (c.title.clone(), c.handler.clone())
                        };
                        // Record before running: the handler may re-enter the
                        // registry (it can even reopen the palette).
                        registry.borrow_mut().record_use(&title);
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

/// `$XDG_CONFIG_HOME/ultimateslice/palette-recent.json` (fallback
/// `~/.config/...`), next to `keybindings.json`.
pub fn recent_config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("ultimateslice").join("palette-recent.json"))
}

/// Read the recent list from `path` (missing or unreadable → empty).
pub fn load_recent_from_path(path: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut list: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
    list.truncate(MAX_RECENT);
    list
}

/// Write the recent list to `path` (creating the directory); errors are
/// logged, never fatal.
pub fn save_recent_to_path(path: &std::path::Path, recent: &[String]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(recent) {
        Ok(text) => {
            if let Err(e) = std::fs::write(path, text) {
                log::warn!("command palette: could not save {}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("command palette: could not encode recent list: {e}"),
    }
}

/// `get_project_health` → `Get Project Health`.
pub fn humanize_tool_name(name: &str) -> String {
    name.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(first) => first.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Classify an MCP tool's reply for the palette: a short status line to
/// toast, or `None` when the payload deserves the result dialog.
///
/// `{"success": true, ...small...}` → `Some(Ok(summary))`;
/// `{"success": false, "error": …}` → `Some(Err(message))`;
/// anything with real data (lists, nested objects, more than a few keys) → `None`.
pub fn summarize_tool_result(
    title: &str,
    result: &serde_json::Value,
) -> Option<Result<String, String>> {
    let obj = result.as_object()?;
    match obj.get("success").and_then(|v| v.as_bool()) {
        Some(false) => {
            let msg = obj
                .get("error")
                .map(|e| match e {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| "failed".to_string());
            Some(Err(format!("{title}: {msg}")))
        }
        Some(true) => {
            let rest: Vec<(&String, &serde_json::Value)> = obj
                .iter()
                .filter(|(k, _)| k.as_str() != "success")
                .collect();
            let simple =
                rest.len() <= 3 && rest.iter().all(|(_, v)| !(v.is_array() || v.is_object()));
            if !simple {
                return None;
            }
            let parts: Vec<String> = rest
                .iter()
                .map(|(k, v)| match v {
                    serde_json::Value::String(s) => format!("{k} {s}"),
                    other => format!("{k} {other}"),
                })
                .collect();
            Some(Ok(if parts.is_empty() {
                format!("{title}: done")
            } else {
                format!("{title}: {}", parts.join(", "))
            }))
        }
        None => None,
    }
}

/// Show a tool's JSON reply in a small modal window with a Copy button.
pub fn show_tool_result_dialog(parent: &gtk::Window, title: &str, result: &serde_json::Value) {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .default_width(640)
        .default_height(480)
        .title(title)
        .build();
    window.add_css_class("command-palette-result");
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);
    let text = serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string());
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_monospace(true);
    view.set_cursor_visible(false);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.buffer().set_text(&text);
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_child(Some(&view));
    vbox.append(&scrolled);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    let copy = gtk::Button::with_label("Copy");
    {
        let text = text.clone();
        copy.connect_clicked(move |b| {
            b.clipboard().set_text(&text);
        });
    }
    let close = gtk::Button::with_label("Close");
    {
        let window = window.clone();
        close.connect_clicked(move |_| window.close());
    }
    buttons.append(&copy);
    buttons.append(&close);
    vbox.append(&buttons);
    window.set_child(Some(&vbox));
    let key_ctrl = gtk::EventControllerKey::new();
    {
        let window = window.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            if key == Key::Escape {
                window.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    window.add_controller(key_ctrl);
    window.present();
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
    fn recent_commands_lead_the_empty_query_and_stay_deduplicated_and_capped() {
        let mut r = reg();
        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        {
            let seen = seen.clone();
            r.set_persist(Rc::new(move |list: &[String]| {
                *seen.borrow_mut() = list.to_vec();
            }));
        }
        r.record_use("Razor tool");
        r.record_use("Open…");
        r.record_use("Razor tool");
        assert_eq!(r.recent(), &["Razor tool".to_string(), "Open…".to_string()]);
        assert_eq!(seen.borrow().as_slice(), r.recent());
        let idxs = r.fuzzy_match("");
        assert_eq!(r.commands[idxs[0]].title, "Razor tool");
        assert_eq!(r.commands[idxs[1]].title, "Open…");
        assert_eq!(idxs.len(), r.len(), "every command listed exactly once");
        for i in 0..(MAX_RECENT + 3) {
            r.record_use(&format!("cmd {i}"));
        }
        assert_eq!(r.recent().len(), MAX_RECENT);
        assert!(r.is_recent(&format!("cmd {}", MAX_RECENT + 2)));
        assert!(!r.is_recent("cmd 0"));
    }

    #[test]
    fn resolve_prefers_exact_title_then_fuzzy() {
        let r = reg();
        assert_eq!(r.resolve("save project").unwrap().0, "Save Project");
        assert_eq!(r.resolve("ripdel").unwrap().0, "Ripple Delete");
        assert!(r.resolve("zzzz qqqq").is_none());
    }

    #[test]
    fn recent_list_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("palette-recent.json");
        assert!(load_recent_from_path(&path).is_empty());
        save_recent_to_path(&path, &["A".to_string(), "B".to_string()]);
        assert_eq!(
            load_recent_from_path(&path),
            vec!["A".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn tool_names_humanize_and_results_summarize() {
        assert_eq!(
            humanize_tool_name("get_project_health"),
            "Get Project Health"
        );
        assert_eq!(humanize_tool_name("play"), "Play");
        let ok = serde_json::json!({"success": true, "index": 2, "label": "Video 2"});
        assert_eq!(
            summarize_tool_result("Add Track", &ok),
            Some(Ok("Add Track: index 2, label Video 2".to_string()))
        );
        let bare = serde_json::json!({"success": true});
        assert_eq!(
            summarize_tool_result("Play", &bare),
            Some(Ok("Play: done".to_string()))
        );
        let err = serde_json::json!({"success": false, "error": "no project"});
        assert_eq!(
            summarize_tool_result("Stop", &err),
            Some(Err("Stop: no project".to_string()))
        );
        let data = serde_json::json!({"success": true, "tracks": [1, 2]});
        assert_eq!(summarize_tool_result("List Tracks", &data), None);
        let list = serde_json::json!([{"id": 1}]);
        assert_eq!(summarize_tool_result("List", &list), None);
    }

    #[test]
    fn rip_matches_ripple_delete() {
        let r = reg();
        let idxs = r.fuzzy_match("rip");
        let titles: Vec<_> = idxs.iter().map(|&i| r.commands[i].title.as_str()).collect();
        assert!(titles.contains(&"Ripple Delete"), "got {titles:?}");
    }
}
