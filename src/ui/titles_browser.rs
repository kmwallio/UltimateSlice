//! Titles Browser — side panel listing built-in title templates.
//!
//! Displays all templates grouped by category with a search filter.
//! "Add to Timeline" creates a standalone title clip at the playhead;
//! "Apply to Clip" applies the style to the selected clip's title overlay.

use super::title_templates::TEMPLATES;
use gtk4::prelude::*;
use gtk4::{self as gtk, Orientation};
use std::cell::RefCell;
use std::rc::Rc;

type GBox = gtk::Box;

/// Build the Titles Browser panel widget.
///
/// Returns the widget. Callbacks are invoked with the template_id string.
pub fn build_titles_browser(
    on_add_title: Rc<dyn Fn(String)>,
    on_apply_to_clip: Rc<dyn Fn(String)>,
) -> GBox {
    let vbox = GBox::new(Orientation::Vertical, 4);
    vbox.set_width_request(240);
    vbox.set_vexpand(true);

    // ── Header ──────────────────────────────────────────────────────────
    let header_row = GBox::new(Orientation::Horizontal, 4);
    header_row.set_margin_start(8);
    header_row.set_margin_end(8);
    header_row.set_margin_top(6);
    header_row.set_margin_bottom(2);

    let header = gtk::Label::new(Some("Titles"));
    header.add_css_class("browser-header");
    header.set_hexpand(true);
    header.set_halign(gtk::Align::Start);
    header_row.append(&header);
    vbox.append(&header_row);

    // ── Search entry ────────────────────────────────────────────────────
    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Search titles\u{2026}"));
    search_entry.set_margin_start(8);
    search_entry.set_margin_end(8);
    search_entry.set_margin_bottom(4);
    vbox.append(&search_entry);

    // ── Buttons ─────────────────────────────────────────────────────────
    let btn_row = GBox::new(Orientation::Horizontal, 4);
    btn_row.set_margin_start(8);
    btn_row.set_margin_end(8);
    btn_row.set_margin_bottom(4);

    let add_btn = gtk::Button::with_label("Add to Timeline");
    add_btn.set_sensitive(false);
    add_btn.set_tooltip_text(Some("Create a standalone title clip at the playhead"));
    add_btn.set_hexpand(true);
    btn_row.append(&add_btn);

    let apply_btn = gtk::Button::with_label("Apply to Clip");
    apply_btn.set_sensitive(false);
    apply_btn.set_tooltip_text(Some(
        "Apply the selected title style to the selected timeline clip",
    ));
    apply_btn.set_hexpand(true);
    btn_row.append(&apply_btn);

    vbox.append(&btn_row);

    // ── Scrollable list ─────────────────────────────────────────────────
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    // Allow the scroll area to shrink to zero so the outer box's minimum
    // height stays small — prevents GTK "Trying to measure" warnings when
    // the left panel is short.
    scroll.set_min_content_height(0);
    scroll.set_propagate_natural_height(false);

    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::Single);
    list_box.add_css_class("effects-list");
    scroll.set_child(Some(&list_box));
    vbox.append(&scroll);

    // Populate immediately (static data).
    populate_list(&list_box);

    // ── State ───────────────────────────────────────────────────────────
    let selected_template: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // ── Row selection → enable buttons ──────────────────────────────────
    {
        let selected_template = selected_template.clone();
        let add_btn = add_btn.clone();
        let apply_btn = apply_btn.clone();
        list_box.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                let name = row.widget_name().to_string();
                let valid = !name.is_empty() && name != "__category__";
                add_btn.set_sensitive(valid);
                apply_btn.set_sensitive(valid);
                if valid {
                    *selected_template.borrow_mut() = Some(name);
                } else {
                    *selected_template.borrow_mut() = None;
                }
            } else {
                add_btn.set_sensitive(false);
                apply_btn.set_sensitive(false);
                *selected_template.borrow_mut() = None;
            }
        });
    }

    // ── Double-click → add to timeline ──────────────────────────────────
    {
        let on_add = on_add_title.clone();
        let selected_template = selected_template.clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(1);
        list_box.add_controller(gesture.clone());
        gesture.connect_released(move |gesture, n_press, _x, _y| {
            if n_press == 2 {
                if let Some(ref id) = *selected_template.borrow() {
                    on_add(id.clone());
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
    }

    // ── Add button click ────────────────────────────────────────────────
    {
        let on_add = on_add_title;
        let selected_template = selected_template.clone();
        add_btn.connect_clicked(move |_| {
            if let Some(ref id) = *selected_template.borrow() {
                on_add(id.clone());
            }
        });
    }

    // ── Apply button click ──────────────────────────────────────────────
    {
        let on_apply = on_apply_to_clip;
        let selected_template = selected_template.clone();
        apply_btn.connect_clicked(move |_| {
            if let Some(ref id) = *selected_template.borrow() {
                on_apply(id.clone());
            }
        });
    }

    // ── Search filtering ────────────────────────────────────────────────
    {
        let list_box = list_box.clone();
        search_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string().to_ascii_lowercase();
            filter_list(&list_box, &query);
        });
    }

    vbox
}

fn populate_list(list_box: &gtk::ListBox) {
    let mut current_category = "";
    for template in TEMPLATES {
        if template.category != current_category {
            current_category = template.category;
            let cat_label = gtk::Label::new(Some(current_category));
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
        }

        let row_box = GBox::new(Orientation::Vertical, 1);
        row_box.set_margin_start(12);
        row_box.set_margin_end(8);
        row_box.set_margin_top(4);
        row_box.set_margin_bottom(4);

        let name_label = gtk::Label::new(Some(template.display_name));
        name_label.add_css_class("effect-name");
        name_label.set_halign(gtk::Align::Start);
        name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        row_box.append(&name_label);

        if !template.description.is_empty() {
            let desc_label = gtk::Label::new(Some(template.description));
            desc_label.add_css_class("effect-hint");
            desc_label.set_halign(gtk::Align::Start);
            desc_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            desc_label.set_max_width_chars(30);
            row_box.append(&desc_label);
        }

        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&row_box));
        row.set_widget_name(template.id);
        list_box.append(&row);
    }
}

fn filter_list(list_box: &gtk::ListBox, query: &str) {
    if query.is_empty() {
        let mut child = list_box.first_child();
        while let Some(widget) = child {
            widget.set_visible(true);
            child = widget.next_sibling();
        }
        return;
    }

    let mut category_rows: Vec<(gtk::Widget, bool)> = Vec::new();
    let mut current_category_widget: Option<gtk::Widget> = None;
    let mut current_category_has_visible = false;

    let mut child = list_box.first_child();
    while let Some(widget) = child {
        let next = widget.next_sibling();
        if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
            let name = row.widget_name().to_string();
            if name == "__category__" {
                if let Some(ref cat_widget) = current_category_widget {
                    category_rows.push((cat_widget.clone(), current_category_has_visible));
                }
                current_category_widget = Some(widget.clone());
                current_category_has_visible = false;
            } else {
                // Search in template id, display name, and description.
                let template = TEMPLATES.iter().find(|t| t.id == name);
                let visible = template.map_or(false, |t| {
                    t.id.contains(query)
                        || t.display_name.to_ascii_lowercase().contains(query)
                        || t.description.to_ascii_lowercase().contains(query)
                        || t.category.to_ascii_lowercase().contains(query)
                });
                widget.set_visible(visible);
                if visible {
                    current_category_has_visible = true;
                }
            }
        }
        child = next;
    }
    if let Some(ref cat_widget) = current_category_widget {
        category_rows.push((cat_widget.clone(), current_category_has_visible));
    }

    for (cat_widget, has_visible) in category_rows {
        cat_widget.set_visible(has_visible);
    }
}
