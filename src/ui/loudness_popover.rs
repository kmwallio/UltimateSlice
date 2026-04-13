//! Loudness Radar popover — shown when the user clicks the **Loudness**
//! button in the Program Monitor header. Presents a full EBU R128 report
//! (integrated, short-term max, momentary max, LRA, true peak), a target
//! preset dropdown with custom override, and buttons to Analyze /
//! Normalize to Target / Reset Gain.
//!
//! The popover itself is a pure view — it does not run any analysis or
//! mutate the project directly. `window.rs` wires up the three callbacks:
//!
//! * `on_analyze` — spawn a background thread, render the project audio
//!   to a temp file, analyze with `analyze_loudness_full`, call
//!   `set_report` on the view with the result.
//! * `on_normalize` — push a `SetProjectMasterGainCommand` through the
//!   undo history and update the preview via `set_master_gain_db`.
//! * `on_reset_gain` — same as above but with `new_db = 0.0`.

use crate::media::export::LoudnessReport;
use crate::ui_state::{loudness_target_preset_to_lufs, PreferencesState};
use gtk4::prelude::*;
use gtk4::{
    self as gtk, Box as GBox, Button, ComboBoxText, Grid, Label, Orientation, Popover, SpinButton,
    Spinner, ToggleButton,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Widgets + shared state for the Loudness popover. Construction lives in
/// `build_loudness_popover`; use the returned struct's methods to push state
/// updates from window.rs.
pub struct LoudnessPopoverView {
    pub popover: Popover,
    pub button: ToggleButton,
    pub analyze_btn: Button,
    pub normalize_btn: Button,
    pub reset_gain_btn: Button,
    pub preset_combo: ComboBoxText,
    pub custom_spin: SpinButton,
    /// Live spinner widget in the popover header — starts when the
    /// background analyze thread is spawned and stops when the result
    /// (or error) comes back.
    pub spinner: Spinner,
    pub status_label: Label,
    pub delta_label: Label,
    pub warning_label: Label,
    pub current_gain_label: Label,
    // Result value labels (em-dash until first analyze).
    pub integrated_value: Label,
    pub short_term_value: Label,
    pub momentary_value: Label,
    pub lra_value: Label,
    pub true_peak_value: Label,
    /// Holds the latest analyzed report so `on_normalize` can compute the
    /// delta without re-analyzing.
    pub last_report: Rc<RefCell<Option<LoudnessReport>>>,
    /// Tracks whether an analysis is currently running so the UI can
    /// disable the Analyze button to prevent double-spawns.
    pub analyzing: Rc<Cell<bool>>,
}

impl LoudnessPopoverView {
    /// Populate the results grid + delta label with the new report and
    /// enable the Normalize button. Called from the background-thread
    /// drain on the window-level poll timer.
    pub fn set_report(&self, report: LoudnessReport, current_master_gain_db: f64) {
        self.integrated_value
            .set_text(&format!("{:.1} LUFS", report.integrated_lufs));
        self.short_term_value
            .set_text(&format!("{:.1} LUFS", report.short_term_max_lufs));
        self.momentary_value
            .set_text(&format!("{:.1} LUFS", report.momentary_max_lufs));
        self.lra_value
            .set_text(&format!("{:.1} LU", report.loudness_range_lu));
        self.true_peak_value
            .set_text(&format!("{:.1} dBTP", report.true_peak_dbtp));

        *self.last_report.borrow_mut() = Some(report.clone());
        self.analyzing.set(false);
        self.analyze_btn.set_sensitive(true);
        self.normalize_btn.set_sensitive(true);
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.status_label.set_text("");
        self.update_delta_and_warning(current_master_gain_db);
    }

    /// Update the current-gain label from the project state.
    pub fn set_current_gain(&self, db: f64) {
        self.current_gain_label.set_text(&format!("{:+.2} dB", db));
        self.update_delta_and_warning(db);
    }

    /// Recompute the delta / warning labels from the last analyzed report
    /// and the current target.
    fn update_delta_and_warning(&self, current_master_gain_db: f64) {
        let report = match self.last_report.borrow().clone() {
            Some(r) => r,
            None => {
                self.delta_label.set_text("");
                self.warning_label.set_text("");
                return;
            }
        };
        let target_lufs = self.current_target_lufs();
        // The analysis was taken at 0 dB master gain, so the delta that
        // needs to be applied is simply (target - measured). The UI shows
        // the *new* master gain the user would land on if they click
        // Normalize — current + delta, clamped.
        let delta = target_lufs - report.integrated_lufs;
        let new_gain = (current_master_gain_db + delta).clamp(-24.0, 24.0);
        self.delta_label.set_text(&format!(
            "Delta: {:+.2} dB → new master gain {:+.2} dB",
            delta, new_gain,
        ));
        let projected_true_peak = report.true_peak_dbtp + delta;
        if projected_true_peak > -1.0 {
            self.warning_label.set_text(&format!(
                "⚠ True peak would reach {:.1} dBTP — consider a limiter.",
                projected_true_peak,
            ));
        } else {
            self.warning_label.set_text("");
        }
    }

    /// Resolve the currently-selected preset + custom spin to a LUFS value.
    pub fn current_target_lufs(&self) -> f64 {
        let id = self
            .preset_combo
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "ebu_r128".to_string());
        loudness_target_preset_to_lufs(&id).unwrap_or_else(|| self.custom_spin.value())
    }

    /// Resolve the currently-selected preset id (for persistence).
    pub fn current_preset_id(&self) -> String {
        self.preset_combo
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "ebu_r128".to_string())
    }

    /// Set the popover into "analyzing" state — disable Analyze, start
    /// the spinner, and show a status label. Called from `on_analyze`
    /// before the background thread is spawned.
    pub fn set_analyzing(&self) {
        self.analyzing.set(true);
        self.analyze_btn.set_sensitive(false);
        self.normalize_btn.set_sensitive(false);
        self.spinner.set_visible(true);
        self.spinner.start();
        self.status_label.set_text("Analyzing…");
    }

    /// Surface a failure on a previous analyze attempt.
    pub fn set_analyze_error(&self, message: &str) {
        self.analyzing.set(false);
        self.analyze_btn.set_sensitive(true);
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.status_label
            .set_text(&format!("Analyze failed: {message}"));
    }
}

/// Build the popover and its toggle button. Returns the view wrapped in
/// an `Rc` so window.rs can keep a single shared handle and push updates
/// from the background-thread poll.
pub fn build_loudness_popover(
    preferences: &PreferencesState,
    current_master_gain_db: f64,
) -> Rc<LoudnessPopoverView> {
    let button = ToggleButton::with_label("Loudness");
    button.set_tooltip_text(Some(
        "Loudness Radar — measure the timeline mixdown (EBU R128) and normalize to a target",
    ));

    let popover = Popover::new();
    popover.set_parent(&button);
    popover.set_autohide(true);

    let outer = GBox::new(Orientation::Vertical, 10);
    outer.set_margin_start(14);
    outer.set_margin_end(14);
    outer.set_margin_top(14);
    outer.set_margin_bottom(14);

    // Header row: title + spinner + status.
    let header_row = GBox::new(Orientation::Horizontal, 8);
    let title = Label::new(Some("Project Loudness"));
    title.add_css_class("title-4");
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    let spinner = Spinner::new();
    spinner.set_halign(gtk::Align::End);
    spinner.set_visible(false);
    let status_label = Label::new(Some(""));
    status_label.add_css_class("dim-label");
    status_label.set_halign(gtk::Align::End);
    header_row.append(&title);
    header_row.append(&spinner);
    header_row.append(&status_label);
    outer.append(&header_row);

    // Analyze button.
    let analyze_btn = Button::with_label("Analyze Project");
    analyze_btn.set_halign(gtk::Align::Start);
    outer.append(&analyze_btn);

    // Results grid.
    let results_grid = Grid::new();
    results_grid.set_row_spacing(4);
    results_grid.set_column_spacing(18);
    results_grid.set_halign(gtk::Align::Fill);
    results_grid.set_hexpand(true);

    let dash = "—";
    let integrated_value = Label::new(Some(dash));
    integrated_value.add_css_class("title-4");
    integrated_value.set_halign(gtk::Align::Start);
    let short_term_value = Label::new(Some(dash));
    short_term_value.add_css_class("title-4");
    short_term_value.set_halign(gtk::Align::Start);
    let momentary_value = Label::new(Some(dash));
    momentary_value.add_css_class("title-4");
    momentary_value.set_halign(gtk::Align::Start);
    let lra_value = Label::new(Some(dash));
    lra_value.add_css_class("title-4");
    lra_value.set_halign(gtk::Align::Start);
    let true_peak_value = Label::new(Some(dash));
    true_peak_value.add_css_class("title-4");
    true_peak_value.set_halign(gtk::Align::Start);
    let current_gain_value = Label::new(Some(&format!("{:+.2} dB", current_master_gain_db)));
    current_gain_value.add_css_class("title-4");
    current_gain_value.set_halign(gtk::Align::Start);

    let integrated_cap = Label::new(Some("Integrated"));
    integrated_cap.add_css_class("dim-label");
    integrated_cap.set_halign(gtk::Align::Start);
    let short_term_cap = Label::new(Some("Short-term max"));
    short_term_cap.add_css_class("dim-label");
    short_term_cap.set_halign(gtk::Align::Start);
    let momentary_cap = Label::new(Some("Momentary max"));
    momentary_cap.add_css_class("dim-label");
    momentary_cap.set_halign(gtk::Align::Start);
    let lra_cap = Label::new(Some("LRA"));
    lra_cap.add_css_class("dim-label");
    lra_cap.set_halign(gtk::Align::Start);
    let true_peak_cap = Label::new(Some("True Peak"));
    true_peak_cap.add_css_class("dim-label");
    true_peak_cap.set_halign(gtk::Align::Start);
    let current_gain_cap = Label::new(Some("Current gain"));
    current_gain_cap.add_css_class("dim-label");
    current_gain_cap.set_halign(gtk::Align::Start);

    results_grid.attach(&integrated_cap, 0, 0, 1, 1);
    results_grid.attach(&short_term_cap, 1, 0, 1, 1);
    results_grid.attach(&momentary_cap, 2, 0, 1, 1);
    results_grid.attach(&integrated_value, 0, 1, 1, 1);
    results_grid.attach(&short_term_value, 1, 1, 1, 1);
    results_grid.attach(&momentary_value, 2, 1, 1, 1);
    results_grid.attach(&lra_cap, 0, 2, 1, 1);
    results_grid.attach(&true_peak_cap, 1, 2, 1, 1);
    results_grid.attach(&current_gain_cap, 2, 2, 1, 1);
    results_grid.attach(&lra_value, 0, 3, 1, 1);
    results_grid.attach(&true_peak_value, 1, 3, 1, 1);
    results_grid.attach(&current_gain_value, 2, 3, 1, 1);
    outer.append(&results_grid);

    // Target row: preset dropdown + custom spin (latter hidden unless custom).
    let target_row = GBox::new(Orientation::Horizontal, 8);
    let target_label = Label::new(Some("Target"));
    target_row.append(&target_label);
    let preset_combo = ComboBoxText::new();
    preset_combo.append(Some("ebu_r128"), "EBU R128 (−23 LUFS)");
    preset_combo.append(Some("atsc_a85"), "ATSC A/85 (−24 LUFS)");
    preset_combo.append(Some("netflix"), "Netflix (−27 LUFS)");
    preset_combo.append(Some("apple_pod"), "Apple Podcasts (−16 LUFS)");
    preset_combo.append(Some("streaming"), "Streaming (−14 LUFS)");
    preset_combo.append(Some("custom"), "Custom");
    preset_combo.set_active_id(Some(&preferences.loudness_target_preset));
    target_row.append(&preset_combo);
    let custom_spin = SpinButton::with_range(-30.0, 0.0, 0.1);
    custom_spin.set_value(preferences.loudness_target_lufs);
    custom_spin.set_digits(1);
    custom_spin.set_sensitive(preferences.loudness_target_preset == "custom");
    target_row.append(&custom_spin);
    outer.append(&target_row);

    // Delta + warning labels.
    let delta_label = Label::new(Some(""));
    delta_label.set_halign(gtk::Align::Start);
    delta_label.add_css_class("dim-label");
    outer.append(&delta_label);
    let warning_label = Label::new(Some(""));
    warning_label.set_halign(gtk::Align::Start);
    warning_label.set_wrap(true);
    warning_label.set_max_width_chars(46);
    outer.append(&warning_label);

    // Action button row.
    let action_row = GBox::new(Orientation::Horizontal, 8);
    let normalize_btn = Button::with_label("Normalize to Target");
    normalize_btn.set_sensitive(false);
    let reset_gain_btn = Button::with_label("Reset Gain");
    action_row.append(&normalize_btn);
    action_row.append(&reset_gain_btn);
    outer.append(&action_row);

    popover.set_child(Some(&outer));

    // Button toggles the popover open/closed.
    {
        let pop = popover.clone();
        button.connect_toggled(move |btn| {
            if btn.is_active() {
                pop.popup();
            } else {
                pop.popdown();
            }
        });
    }
    // Keep the toggle button state in sync with the popover.
    {
        let btn = button.clone();
        popover.connect_closed(move |_| {
            btn.set_active(false);
        });
    }

    let view = Rc::new(LoudnessPopoverView {
        popover,
        button,
        analyze_btn,
        normalize_btn,
        reset_gain_btn,
        preset_combo,
        custom_spin,
        spinner,
        status_label,
        delta_label,
        warning_label,
        current_gain_label: current_gain_value,
        integrated_value,
        short_term_value,
        momentary_value,
        lra_value,
        true_peak_value,
        last_report: Rc::new(RefCell::new(None)),
        analyzing: Rc::new(Cell::new(false)),
    });

    // Preset dropdown — toggle custom spin sensitivity, push the preset's
    // canonical LUFS value into the spin, and refresh the delta/warning.
    {
        let view_cb = view.clone();
        view.preset_combo.connect_changed(move |combo| {
            let id = combo
                .active_id()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "ebu_r128".to_string());
            view_cb.custom_spin.set_sensitive(id == "custom");
            if let Some(lufs) = loudness_target_preset_to_lufs(&id) {
                view_cb.custom_spin.set_value(lufs);
            }
            // Re-compute delta/warning using the current master gain parsed
            // back out of the label.
            let current = parse_current_gain_label(&view_cb.current_gain_label);
            view_cb.update_delta_and_warning(current);
        });
    }
    // Custom spin — refresh delta when user types a custom target.
    {
        let view_cb = view.clone();
        view.custom_spin.connect_value_changed(move |_| {
            let current = parse_current_gain_label(&view_cb.current_gain_label);
            view_cb.update_delta_and_warning(current);
        });
    }

    view
}

/// Tiny helper: parse the `+X.YZ dB` form of the current-gain label back
/// into an f64 so the popover can recompute delta without window.rs
/// pushing every change through.
fn parse_current_gain_label(label: &Label) -> f64 {
    let text = label.text();
    text.trim()
        .trim_end_matches(" dB")
        .trim_start_matches('+')
        .parse::<f64>()
        .unwrap_or(0.0)
}
