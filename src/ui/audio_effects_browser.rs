//! Audio Effects Browser — side panel listing available LADSPA audio plugins.
//!
//! Parallel to the frei0r effects browser but for audio plugins.
//! Double-click (or "Apply" button) adds the selected LADSPA effect to the
//! currently selected timeline clip.

use crate::media::ladspa_registry::LadspaRegistry;
use gtk4::prelude::*;
use gtk4::{self as gtk, Orientation};
use std::cell::RefCell;
use std::rc::Rc;

type GBox = gtk::Box;

/// Build the Audio Effects Browser panel widget.
///
/// Returns `(widget, set_registry_fn)` where `set_registry_fn` populates the
/// list once the registry is ready.
pub fn build_audio_effects_browser(
    on_apply_effect: Rc<dyn Fn(String)>,
) -> (GBox, Rc<dyn Fn(Rc<LadspaRegistry>)>) {
    let vbox = GBox::new(Orientation::Vertical, 4);
    vbox.set_width_request(240);
    vbox.set_vexpand(true);

    let header = gtk::Label::new(Some("Audio Effects"));
    header.add_css_class("browser-header");
    header.set_hexpand(true);
    header.set_halign(gtk::Align::Start);
    header.set_margin_start(8);
    header.set_margin_top(6);
    vbox.append(&header);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Search audio effects\u{2026}"));
    search_entry.set_margin_start(8);
    search_entry.set_margin_end(8);
    search_entry.set_margin_bottom(4);
    vbox.append(&search_entry);

    let apply_btn = gtk::Button::with_label("Apply to Clip");
    apply_btn.set_margin_start(8);
    apply_btn.set_margin_end(8);
    apply_btn.set_margin_bottom(4);
    apply_btn.set_sensitive(false);
    apply_btn.set_tooltip_text(Some("Apply the selected LADSPA effect to the selected clip"));
    vbox.append(&apply_btn);

    let empty_hint = gtk::Label::new(Some(
        "No LADSPA audio plugins found.\nInstall ladspa-sdk or rubberband-ladspa.",
    ));
    empty_hint.set_wrap(true);
    empty_hint.add_css_class("panel-empty-state");
    empty_hint.set_margin_start(8);
    empty_hint.set_margin_end(8);
    vbox.append(&empty_hint);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::Single);
    list_box.add_css_class("effects-list");
    scroll.set_child(Some(&list_box));
    vbox.append(&scroll);

    let registry: Rc<RefCell<Option<Rc<LadspaRegistry>>>> = Rc::new(RefCell::new(None));
    let selected_plugin: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Row selection
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

    // Double-click
    {
        let on_apply = on_apply_effect.clone();
        let selected_plugin = selected_plugin.clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(1);
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

    // Apply button
    {
        let on_apply = on_apply_effect;
        let selected_plugin = selected_plugin;
        apply_btn.connect_clicked(move |_| {
            if let Some(ref name) = *selected_plugin.borrow() {
                on_apply(name.clone());
            }
        });
    }

    // Search
    {
        let list_box = list_box.clone();
        let registry = registry.clone();
        search_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string().to_ascii_lowercase();
            filter_list(&list_box, &registry.borrow(), &query);
        });
    }

    let set_registry: Rc<dyn Fn(Rc<LadspaRegistry>)> = {
        let registry = registry.clone();
        let list_box = list_box.clone();
        let empty_hint = empty_hint.clone();
        let scroll = scroll.clone();
        Rc::new(move |reg: Rc<LadspaRegistry>| {
            populate_list(&list_box, &reg);
            let has_plugins = !reg.plugins.is_empty();
            empty_hint.set_visible(!has_plugins);
            scroll.set_visible(has_plugins);
            *registry.borrow_mut() = Some(reg);
        })
    };

    (vbox, set_registry)
}

fn populate_list(list_box: &gtk::ListBox, registry: &LadspaRegistry) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    for cat in registry.categories() {
        // Category header
        let cat_label = gtk::Label::new(Some(&cat));
        cat_label.add_css_class("browser-category");
        cat_label.set_halign(gtk::Align::Start);
        cat_label.set_margin_start(8);
        cat_label.set_margin_top(6);
        let cat_row = gtk::ListBoxRow::new();
        cat_row.set_selectable(false);
        cat_row.set_activatable(false);
        cat_row.set_child(Some(&cat_label));
        cat_row.set_widget_name(&format!("__cat_{cat}"));
        list_box.append(&cat_row);

        if let Some(indices) = registry.by_category.get(&cat) {
            for &idx in indices {
                let plugin = &registry.plugins[idx];
                let row = make_plugin_row(plugin);
                list_box.append(&row);
            }
        }
    }
}

fn make_plugin_row(
    plugin: &crate::media::ladspa_registry::LadspaPluginInfo,
) -> gtk::ListBoxRow {
    let vbox = gtk::Box::new(Orientation::Vertical, 2);
    vbox.set_margin_start(12);
    vbox.set_margin_end(8);
    vbox.set_margin_top(4);
    vbox.set_margin_bottom(4);

    let name_label = gtk::Label::new(Some(&plugin.display_name));
    name_label.set_halign(gtk::Align::Start);
    name_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    vbox.append(&name_label);

    if !plugin.description.is_empty() {
        let desc = gtk::Label::new(Some(&plugin.description));
        desc.add_css_class("dim-label");
        desc.set_halign(gtk::Align::Start);
        desc.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        vbox.append(&desc);
    }

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&vbox));
    row.set_widget_name(&plugin.ladspa_name);
    row
}

fn filter_list(
    list_box: &gtk::ListBox,
    registry: &Option<Rc<LadspaRegistry>>,
    query: &str,
) {
    let Some(reg) = registry else {
        return;
    };
    if query.is_empty() {
        // Show all
        let mut idx = 0;
        while let Some(row) = list_box.row_at_index(idx) {
            row.set_visible(true);
            idx += 1;
        }
        return;
    }
    let matches: std::collections::HashSet<String> = reg
        .search(query)
        .iter()
        .map(|p| p.ladspa_name.clone())
        .collect();

    let mut idx = 0;
    let mut last_cat_row: Option<gtk::ListBoxRow> = None;
    let mut cat_has_visible = false;
    while let Some(row) = list_box.row_at_index(idx) {
        let name = row.widget_name().to_string();
        if name.starts_with("__cat_") {
            // Category header — decide visibility after scanning children.
            if let Some(ref prev_cat) = last_cat_row {
                prev_cat.set_visible(cat_has_visible);
            }
            last_cat_row = Some(row.clone());
            cat_has_visible = false;
        } else {
            let vis = matches.contains(&name);
            row.set_visible(vis);
            if vis {
                cat_has_visible = true;
            }
        }
        idx += 1;
    }
    if let Some(ref prev_cat) = last_cat_row {
        prev_cat.set_visible(cat_has_visible);
    }
}
