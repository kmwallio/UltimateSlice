//! Effects Browser — side panel listing available frei0r filter plugins.
//!
//! Displays all discovered plugins grouped by category with a search filter.
//! Double-click (or "Apply" button) adds the selected effect to the currently
//! selected timeline clip.

use crate::media::frei0r_registry::Frei0rRegistry;
use gtk4::prelude::*;
use gtk4::{self as gtk, Orientation};
use std::cell::RefCell;
use std::rc::Rc;

type GBox = gtk::Box;

/// Build the Effects Browser panel widget.
///
/// Returns `(widget, set_registry_fn)` where `set_registry_fn` populates the
/// list once the registry is ready (called from window.rs after GStreamer init).
pub fn build_effects_browser(
    on_apply_effect: Rc<dyn Fn(String)>,
) -> (GBox, Rc<dyn Fn(Rc<Frei0rRegistry>)>) {
    let vbox = GBox::new(Orientation::Vertical, 4);
    vbox.set_width_request(240);
    vbox.set_vexpand(true);

    // ── Header ──────────────────────────────────────────────────────────
    let header_row = GBox::new(Orientation::Horizontal, 4);
    header_row.set_margin_start(8);
    header_row.set_margin_end(8);
    header_row.set_margin_top(6);
    header_row.set_margin_bottom(2);

    let header = gtk::Label::new(Some("Effects"));
    header.add_css_class("browser-header");
    header.set_hexpand(true);
    header.set_halign(gtk::Align::Start);
    header_row.append(&header);
    vbox.append(&header_row);

    // ── Search entry ────────────────────────────────────────────────────
    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Search effects…"));
    search_entry.set_margin_start(8);
    search_entry.set_margin_end(8);
    search_entry.set_margin_bottom(4);
    vbox.append(&search_entry);

    // ── Apply button ────────────────────────────────────────────────────
    let apply_btn = gtk::Button::with_label("Apply to Clip");
    apply_btn.set_margin_start(8);
    apply_btn.set_margin_end(8);
    apply_btn.set_margin_bottom(4);
    apply_btn.set_sensitive(false);
    apply_btn.set_tooltip_text(Some("Apply the selected effect to the selected timeline clip"));
    vbox.append(&apply_btn);

    // ── Empty state ─────────────────────────────────────────────────────
    let empty_hint = gtk::Label::new(Some(
        "No frei0r plugins found.\nInstall the frei0r-plugins package.",
    ));
    empty_hint.set_wrap(true);
    empty_hint.add_css_class("panel-empty-state");
    empty_hint.set_margin_start(8);
    empty_hint.set_margin_end(8);
    vbox.append(&empty_hint);

    // ── Scrollable list ─────────────────────────────────────────────────
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::Single);
    list_box.add_css_class("effects-list");
    scroll.set_child(Some(&list_box));
    vbox.append(&scroll);

    // ── State ───────────────────────────────────────────────────────────
    let registry: Rc<RefCell<Option<Rc<Frei0rRegistry>>>> = Rc::new(RefCell::new(None));
    let selected_plugin: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // ── Row selection → enable Apply button ─────────────────────────────
    {
        let selected_plugin = selected_plugin.clone();
        let apply_btn = apply_btn.clone();
        list_box.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                let name = row.widget_name().to_string();
                apply_btn.set_sensitive(!name.is_empty());
                *selected_plugin.borrow_mut() = Some(name);
            } else {
                apply_btn.set_sensitive(false);
                *selected_plugin.borrow_mut() = None;
            }
        });
    }

    // ── Double-click → apply ────────────────────────────────────────────
    {
        let on_apply = on_apply_effect.clone();
        let selected_plugin = selected_plugin.clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(1); // left button
        list_box.add_controller(gesture.clone());
        gesture.connect_released(move |gesture, n_press, _x, _y| {
            if n_press == 2 {
                if let Some(ref name) = *selected_plugin.borrow() {
                    on_apply(name.clone());
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
    }

    // ── Apply button click ──────────────────────────────────────────────
    {
        let on_apply = on_apply_effect;
        let selected_plugin = selected_plugin.clone();
        apply_btn.connect_clicked(move |_| {
            if let Some(ref name) = *selected_plugin.borrow() {
                on_apply(name.clone());
            }
        });
    }

    // ── Search filtering ────────────────────────────────────────────────
    {
        let list_box = list_box.clone();
        let registry = registry.clone();
        search_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string().to_ascii_lowercase();
            filter_list(&list_box, &registry.borrow(), &query);
        });
    }

    // ── Populate callback (called once registry is ready) ───────────────
    let set_registry: Rc<dyn Fn(Rc<Frei0rRegistry>)> = {
        let registry = registry.clone();
        let list_box = list_box.clone();
        let empty_hint = empty_hint.clone();
        let scroll = scroll.clone();
        Rc::new(move |reg: Rc<Frei0rRegistry>| {
            populate_list(&list_box, &reg);
            let has_plugins = !reg.plugins.is_empty();
            empty_hint.set_visible(!has_plugins);
            scroll.set_visible(has_plugins);
            *registry.borrow_mut() = Some(reg);
        })
    };

    (vbox, set_registry)
}

/// Populate the ListBox with all plugins from the registry, grouped by category.
fn populate_list(list_box: &gtk::ListBox, registry: &Frei0rRegistry) {
    // Remove all existing children.
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    let categories = registry.categories();
    for cat in &categories {
        // Category header row.
        let cat_label = gtk::Label::new(Some(cat));
        cat_label.add_css_class("effects-category-header");
        cat_label.set_halign(gtk::Align::Start);
        cat_label.set_margin_start(8);
        cat_label.set_margin_top(8);
        cat_label.set_margin_bottom(2);

        let cat_row = gtk::ListBoxRow::new();
        cat_row.set_selectable(false);
        cat_row.set_activatable(false);
        cat_row.set_child(Some(&cat_label));
        cat_row.set_widget_name("__category__");
        list_box.append(&cat_row);

        // Plugin rows for this category.
        if let Some(indices) = registry.by_category.get(cat.as_str()) {
            for &idx in indices {
                let plugin = &registry.plugins[idx];
                let row = make_plugin_row(plugin);
                list_box.append(&row);
            }
        }
    }
}

/// Create a ListBoxRow for a single frei0r plugin.
fn make_plugin_row(plugin: &crate::media::frei0r_registry::Frei0rPluginInfo) -> gtk::ListBoxRow {
    let row_box = GBox::new(Orientation::Vertical, 1);
    row_box.set_margin_start(12);
    row_box.set_margin_end(8);
    row_box.set_margin_top(4);
    row_box.set_margin_bottom(4);

    let name_label = gtk::Label::new(Some(&plugin.display_name));
    name_label.add_css_class("effect-name");
    name_label.set_halign(gtk::Align::Start);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    row_box.append(&name_label);

    if !plugin.description.is_empty() {
        let desc_label = gtk::Label::new(Some(&plugin.description));
        desc_label.add_css_class("effect-hint");
        desc_label.set_halign(gtk::Align::Start);
        desc_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        desc_label.set_max_width_chars(30);
        row_box.append(&desc_label);
    }

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&row_box));
    // Store the frei0r_name as the widget name for retrieval on selection.
    row.set_widget_name(&plugin.frei0r_name);
    row
}

/// Filter the list by showing/hiding rows based on search query.
fn filter_list(
    list_box: &gtk::ListBox,
    registry: &Option<Rc<Frei0rRegistry>>,
    query: &str,
) {
    let registry = match registry {
        Some(r) => r,
        None => return,
    };

    if query.is_empty() {
        // Show all rows.
        let mut child = list_box.first_child();
        while let Some(widget) = child {
            widget.set_visible(true);
            child = widget.next_sibling();
        }
        return;
    }

    // Build a set of matching plugin names.
    let matches: std::collections::HashSet<String> = registry
        .search(query)
        .into_iter()
        .map(|p| p.frei0r_name.clone())
        .collect();

    // Walk all rows, hiding non-matching ones.
    // Also track which categories have visible children.
    let mut category_rows: Vec<(gtk::Widget, bool)> = Vec::new();
    let mut current_category_widget: Option<gtk::Widget> = None;
    let mut current_category_has_visible = false;

    let mut child = list_box.first_child();
    while let Some(widget) = child {
        let next = widget.next_sibling();
        if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
            let name = row.widget_name().to_string();
            if name == "__category__" {
                // Flush previous category.
                if let Some(ref cat_widget) = current_category_widget {
                    category_rows.push((cat_widget.clone(), current_category_has_visible));
                }
                current_category_widget = Some(widget.clone());
                current_category_has_visible = false;
            } else {
                let visible = matches.contains(&name);
                widget.set_visible(visible);
                if visible {
                    current_category_has_visible = true;
                }
            }
        }
        child = next;
    }
    // Flush last category.
    if let Some(ref cat_widget) = current_category_widget {
        category_rows.push((cat_widget.clone(), current_category_has_visible));
    }

    // Show/hide category headers.
    for (cat_widget, has_visible) in category_rows {
        cat_widget.set_visible(has_visible);
    }
}
