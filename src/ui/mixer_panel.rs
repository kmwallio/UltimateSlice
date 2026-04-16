//! Audio Track Mixer Panel.
//!
//! Bottom-of-window panel (alongside Keyframes / Transcript / Markers) that
//! shows a traditional mixing-console layout: one vertical channel strip per
//! audio-producing track, plus a master strip at the right edge.
//!
//! Each channel strip contains:
//! - Track name label
//! - Audio role badge (D / E / M)
//! - Vertical stereo VU meter (green / yellow / red zones)
//! - Vertical gain fader (−∞ to +12 dB, default 0 dB)
//! - dB readout label
//! - Horizontal pan slider (−1.0 to +1.0, default center)
//! - Mute / Solo toggle buttons (synced with timeline header)
//!
//! All mutations go through undo commands so every fader drag is undoable.

use crate::model::project::Project;
use crate::model::track::{AudioRole, Track};
use crate::ui::timeline::TimelineState;
use gtk4::prelude::*;
use gtk4::{self as gtk, Box as GBox, DrawingArea, Label, Orientation, Separator};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const STRIP_WIDTH: i32 = 80;
const METER_WIDTH: i32 = 28;
const METER_HEIGHT: i32 = 100;
const GAIN_MIN_DB: f64 = -60.0;
const GAIN_MAX_DB: f64 = 12.0;

/// Per-track UI handles for updating meters and syncing state.
struct ChannelStrip {
    track_id: String,
    track_index: usize,
    label: Label,
    role_badge: Label,
    vu_meter: DrawingArea,
    peak_cell: Rc<Cell<[f64; 2]>>,
    gain_scale: gtk::Scale,
    db_readout: Label,
    pan_scale: gtk::Scale,
    mute_btn: gtk::ToggleButton,
    solo_btn: gtk::ToggleButton,
    root: GBox,
}

/// Per-bus UI handles (one per audio role that has tracks).
struct BusStrip {
    role: AudioRole,
    vu_meter: DrawingArea,
    peak_cell: Rc<Cell<[f64; 2]>>,
    gain_scale: gtk::Scale,
    db_readout: Label,
    mute_btn: gtk::ToggleButton,
    solo_btn: gtk::ToggleButton,
    root: GBox,
}

/// Shared view state for the mixer panel, held in an `Rc` by `window.rs`.
pub struct MixerPanelView {
    strips_box: GBox,
    bus_strips_box: GBox,
    master_strip: GBox,
    master_meter: DrawingArea,
    master_peak_cell: Rc<Cell<[f64; 2]>>,
    master_gain_scale: gtk::Scale,
    master_db_readout: Label,
    strips: RefCell<Vec<ChannelStrip>>,
    bus_strips: RefCell<Vec<BusStrip>>,
    project: Rc<RefCell<Project>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: Rc<dyn Fn()>,
    /// True while programmatically updating widgets (suppress signal handlers).
    updating: Rc<Cell<bool>>,
}

impl MixerPanelView {
    /// Rebuild channel strips from the current project tracks.
    pub fn rebuild_from_project(&self) {
        // Only tear down / recreate strips when the track list or bus roles changed.
        {
            let strips = self.strips.borrow();
            let bus_strips = self.bus_strips.borrow();
            let proj = self.project.borrow();
            let tracks_match = strips.len() == proj.tracks.len()
                && strips
                    .iter()
                    .zip(proj.tracks.iter())
                    .all(|(s, t)| s.track_id == t.id);
            let current_bus_roles: Vec<AudioRole> = {
                let mut roles: Vec<AudioRole> = proj
                    .tracks
                    .iter()
                    .filter(|t| t.is_audio() && t.audio_role != AudioRole::None)
                    .map(|t| t.audio_role)
                    .collect();
                roles.sort_by_key(|r| *r as u8);
                roles.dedup();
                roles
            };
            let buses_match = bus_strips.len() == current_bus_roles.len()
                && bus_strips
                    .iter()
                    .zip(current_bus_roles.iter())
                    .all(|(bs, r)| bs.role == *r);
            if tracks_match && buses_match {
                drop(strips);
                drop(bus_strips);
                drop(proj);
                self.sync_from_project();
                return;
            }
        }

        self.updating.set(true);

        // Remove old strips.
        while let Some(child) = self.strips_box.first_child() {
            self.strips_box.remove(&child);
        }

        let proj = self.project.borrow();
        let mut new_strips = Vec::new();

        for (t_idx, track) in proj.tracks.iter().enumerate() {
            let strip = self.build_channel_strip(track, t_idx);
            self.strips_box.append(&strip.root);
            new_strips.push(strip);
        }

        *self.strips.borrow_mut() = new_strips;

        // Rebuild bus strips: one per role that has audio tracks.
        while let Some(child) = self.bus_strips_box.first_child() {
            self.bus_strips_box.remove(&child);
        }
        let roles_in_use: Vec<AudioRole> = {
            let mut roles: Vec<AudioRole> = proj
                .tracks
                .iter()
                .filter(|t| t.is_audio() && t.audio_role != AudioRole::None)
                .map(|t| t.audio_role)
                .collect();
            roles.sort_by_key(|r| *r as u8);
            roles.dedup();
            roles
        };
        let mut new_bus_strips = Vec::new();
        for role in &roles_in_use {
            let bs = self.build_bus_strip(*role, &proj);
            self.bus_strips_box.append(&bs.root);
            new_bus_strips.push(bs);
        }
        // Show/hide bus section based on whether any buses exist.
        self.bus_strips_box.set_visible(!new_bus_strips.is_empty());
        *self.bus_strips.borrow_mut() = new_bus_strips;

        // Update master gain fader.
        self.master_gain_scale.set_value(proj.master_gain_db);
        self.master_db_readout
            .set_text(&format_db(proj.master_gain_db));

        self.updating.set(false);
    }

    /// Sync widget state from model without rebuilding (gain/pan/mute/solo).
    pub fn sync_from_project(&self) {
        self.updating.set(true);
        let proj = self.project.borrow();

        for strip in self.strips.borrow().iter() {
            if let Some(track) = proj.tracks.iter().find(|t| t.id == strip.track_id) {
                strip.label.set_text(&track.label);
                strip
                    .role_badge
                    .set_text(role_badge_text(&track.audio_role));
                strip.gain_scale.set_value(track.gain_db);
                strip.db_readout.set_text(&format_db(track.gain_db));
                strip.pan_scale.set_value(track.pan);
                strip.mute_btn.set_active(track.muted);
                strip.solo_btn.set_active(track.soloed);
            }
        }

        self.master_gain_scale.set_value(proj.master_gain_db);
        self.master_db_readout
            .set_text(&format_db(proj.master_gain_db));

        // Sync bus strip state.
        for bs in self.bus_strips.borrow().iter() {
            if let Some(bus) = proj.bus_for_role(&bs.role) {
                bs.gain_scale.set_value(bus.gain_db);
                bs.db_readout.set_text(&format_db(bus.gain_db));
                bs.mute_btn.set_active(bus.muted);
                bs.solo_btn.set_active(bus.soloed);
            }
        }

        self.updating.set(false);
    }

    /// Refresh VU meters from current peak data. Called from the 33ms poll tick.
    pub fn update_meters(&self) {
        let ts = self.timeline_state.borrow();
        let proj = self.project.borrow();
        for strip in self.strips.borrow().iter() {
            let peaks = ts
                .track_audio_peak_db
                .get(strip.track_index)
                .copied()
                .unwrap_or([-60.0, -60.0]);
            strip.peak_cell.set(peaks);
            strip.vu_meter.queue_draw();
        }
        // Bus meters — aggregate peak across all tracks in the bus's role.
        for bs in self.bus_strips.borrow().iter() {
            let bus_peaks = proj
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.is_audio() && t.audio_role == bs.role)
                .fold([-60.0_f64, -60.0_f64], |acc, (i, _)| {
                    let p = ts
                        .track_audio_peak_db
                        .get(i)
                        .copied()
                        .unwrap_or([-60.0, -60.0]);
                    [acc[0].max(p[0]), acc[1].max(p[1])]
                });
            bs.peak_cell.set(bus_peaks);
            bs.vu_meter.queue_draw();
        }
        // Master meter — use the main program monitor peak data.
        // TimelineState doesn't expose master peaks directly; the mixer will
        // show the max across all track peaks as a rough master indication.
        let master_peaks = ts
            .track_audio_peak_db
            .iter()
            .fold([-60.0_f64, -60.0_f64], |acc, p| {
                [acc[0].max(p[0]), acc[1].max(p[1])]
            });
        self.master_peak_cell.set(master_peaks);
        self.master_meter.queue_draw();
    }

    fn build_channel_strip(&self, track: &Track, track_index: usize) -> ChannelStrip {
        let root = GBox::new(Orientation::Vertical, 2);
        root.set_width_request(STRIP_WIDTH);
        root.add_css_class("mixer-channel-strip");
        root.set_margin_start(2);
        root.set_margin_end(2);
        root.set_margin_top(4);
        root.set_margin_bottom(4);

        // Track name label.
        let label = Label::new(Some(&track.label));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(8);
        label.add_css_class("mixer-label");
        root.append(&label);

        // Audio role badge.
        let role_badge = Label::new(Some(role_badge_text(&track.audio_role)));
        role_badge.add_css_class("mixer-role-badge");
        role_badge.set_halign(gtk::Align::Center);
        root.append(&role_badge);

        // VU meter.
        let peak_cell: Rc<Cell<[f64; 2]>> = Rc::new(Cell::new([-60.0, -60.0]));
        let vu_meter = DrawingArea::new();
        vu_meter.set_content_width(METER_WIDTH);
        vu_meter.set_content_height(METER_HEIGHT);
        vu_meter.set_halign(gtk::Align::Center);
        vu_meter.add_css_class("mixer-vu-meter");
        {
            let pc = peak_cell.clone();
            vu_meter.set_draw_func(move |_da, cr, width, height| {
                draw_vu_meter(cr, width, height, pc.get());
            });
        }
        root.append(&vu_meter);

        // Gain fader (vertical).
        let gain_scale =
            gtk::Scale::with_range(Orientation::Vertical, GAIN_MIN_DB, GAIN_MAX_DB, 0.1);
        gain_scale.set_inverted(true); // higher values at top
        gain_scale.set_value(track.gain_db);
        gain_scale.set_size_request(-1, 80);
        gain_scale.set_halign(gtk::Align::Center);
        gain_scale.add_css_class("mixer-fader");
        // Mark 0 dB.
        gain_scale.add_mark(0.0, gtk::PositionType::Right, Some("0"));
        root.append(&gain_scale);

        // dB readout.
        let db_readout = Label::new(Some(&format_db(track.gain_db)));
        db_readout.add_css_class("mixer-db-readout");
        root.append(&db_readout);

        // Pan slider (horizontal).
        let pan_scale = gtk::Scale::with_range(Orientation::Horizontal, -1.0, 1.0, 0.01);
        pan_scale.set_value(track.pan);
        pan_scale.set_size_request(STRIP_WIDTH - 8, -1);
        pan_scale.add_css_class("mixer-pan");
        pan_scale.add_mark(0.0, gtk::PositionType::Top, None);
        root.append(&pan_scale);

        // Mute / Solo buttons.
        let btn_row = GBox::new(Orientation::Horizontal, 2);
        btn_row.set_halign(gtk::Align::Center);
        let mute_btn = gtk::ToggleButton::with_label("M");
        mute_btn.set_active(track.muted);
        mute_btn.add_css_class("mixer-mute");
        mute_btn.set_size_request(28, 24);
        let solo_btn = gtk::ToggleButton::with_label("S");
        solo_btn.set_active(track.soloed);
        solo_btn.add_css_class("mixer-solo");
        solo_btn.set_size_request(28, 24);
        btn_row.append(&mute_btn);
        btn_row.append(&solo_btn);
        root.append(&btn_row);

        // --- Signal handlers ---
        let track_id = track.id.clone();

        // Gain fader change.
        {
            let proj = self.project.clone();
            let tid = track_id.clone();
            let readout = db_readout.clone();
            let on_changed = self.on_project_changed.clone();
            let updating = self.updating.clone();
            // Debounce: only commit undo command on button release. During drag,
            // update model directly for live preview.
            let drag_start_db: Rc<Cell<f64>> = Rc::new(Cell::new(track.gain_db));
            let ds = drag_start_db.clone();
            gain_scale.connect_value_changed(move |scale| {
                if updating.get() {
                    return;
                }
                let new_db = scale.value();
                readout.set_text(&format_db(new_db));
                // Live update model for immediate audio feedback.
                let mut p = proj.borrow_mut();
                if let Some(t) = p.tracks.iter_mut().find(|t| t.id == tid) {
                    t.gain_db = new_db;
                }
                drop(p);
                on_changed();
            });
            let proj2 = self.project.clone();
            let tid2 = track_id.clone();
            let on_changed2 = self.on_project_changed.clone();
            let updating2 = self.updating.clone();
            // On press, record the starting value for the undo command.
            let gesture_click = gtk::GestureClick::new();
            gesture_click.set_button(0);
            let ds2 = drag_start_db.clone();
            gesture_click.connect_pressed(move |_, _, _, _| {
                let p = proj2.borrow();
                if let Some(t) = p.tracks.iter().find(|t| t.id == tid2) {
                    ds2.set(t.gain_db);
                }
            });
            gain_scale.add_controller(gesture_click);

            let ts3 = self.timeline_state.clone();
            let proj3 = self.project.clone();
            let tid3 = track_id.clone();
            let updating3 = self.updating.clone();
            let gesture_release = gtk::GestureClick::new();
            gesture_release.set_button(0);
            gesture_release.connect_released(move |_, _, _, _| {
                if updating3.get() {
                    return;
                }
                let old_db = ds.get();
                let p = proj3.borrow();
                let new_db = p
                    .tracks
                    .iter()
                    .find(|t| t.id == tid3)
                    .map(|t| t.gain_db)
                    .unwrap_or(old_db);
                drop(p);
                if (old_db - new_db).abs() > f64::EPSILON {
                    let cmd = crate::undo::set_track_gain_cmd(tid3.clone(), old_db, new_db);
                    let mut ts = ts3.borrow_mut();
                    ts.history.undo_stack.push(Box::new(cmd));
                    ts.history.redo_stack.clear();
                }
            });
            gain_scale.add_controller(gesture_release);
        }

        // Pan slider change.
        {
            let proj = self.project.clone();
            let tid = track_id.clone();
            let on_changed = self.on_project_changed.clone();
            let updating = self.updating.clone();
            let drag_start_pan: Rc<Cell<f64>> = Rc::new(Cell::new(track.pan));
            let ds = drag_start_pan.clone();
            pan_scale.connect_value_changed(move |scale| {
                if updating.get() {
                    return;
                }
                let new_pan = scale.value();
                let mut p = proj.borrow_mut();
                if let Some(t) = p.tracks.iter_mut().find(|t| t.id == tid) {
                    t.pan = new_pan;
                }
                drop(p);
                on_changed();
            });
            let proj2 = self.project.clone();
            let tid2 = track_id.clone();
            let gesture_click = gtk::GestureClick::new();
            gesture_click.set_button(0);
            let ds2 = drag_start_pan.clone();
            gesture_click.connect_pressed(move |_, _, _, _| {
                let p = proj2.borrow();
                if let Some(t) = p.tracks.iter().find(|t| t.id == tid2) {
                    ds2.set(t.pan);
                }
            });
            pan_scale.add_controller(gesture_click);

            let ts3 = self.timeline_state.clone();
            let proj3 = self.project.clone();
            let tid3 = track_id.clone();
            let updating3 = self.updating.clone();
            let gesture_release = gtk::GestureClick::new();
            gesture_release.set_button(0);
            gesture_release.connect_released(move |_, _, _, _| {
                if updating3.get() {
                    return;
                }
                let old_pan = ds.get();
                let p = proj3.borrow();
                let new_pan = p
                    .tracks
                    .iter()
                    .find(|t| t.id == tid3)
                    .map(|t| t.pan)
                    .unwrap_or(old_pan);
                drop(p);
                if (old_pan - new_pan).abs() > f64::EPSILON {
                    let cmd = crate::undo::set_track_pan_cmd(tid3.clone(), old_pan, new_pan);
                    let mut ts = ts3.borrow_mut();
                    ts.history.undo_stack.push(Box::new(cmd));
                    ts.history.redo_stack.clear();
                }
            });
            pan_scale.add_controller(gesture_release);
        }

        // Mute button.
        {
            let proj = self.project.clone();
            let ts = self.timeline_state.clone();
            let tid = track_id.clone();
            let on_changed = self.on_project_changed.clone();
            let updating = self.updating.clone();
            mute_btn.connect_toggled(move |btn| {
                if updating.get() {
                    return;
                }
                let new_val = btn.is_active();
                let old_val = {
                    let p = proj.borrow();
                    p.tracks
                        .iter()
                        .find(|t| t.id == tid)
                        .map(|t| t.muted)
                        .unwrap_or(false)
                };
                let cmd = crate::undo::set_track_mute_cmd(tid.clone(), old_val, new_val);
                {
                    let mut proj_mut = proj.borrow_mut();
                    ts.borrow_mut()
                        .history
                        .execute(Box::new(cmd), &mut proj_mut);
                }
                on_changed();
            });
        }

        // Solo button.
        {
            let proj = self.project.clone();
            let ts = self.timeline_state.clone();
            let tid = track_id.clone();
            let on_changed = self.on_project_changed.clone();
            let updating = self.updating.clone();
            solo_btn.connect_toggled(move |btn| {
                if updating.get() {
                    return;
                }
                let new_val = btn.is_active();
                let old_val = {
                    let p = proj.borrow();
                    p.tracks
                        .iter()
                        .find(|t| t.id == tid)
                        .map(|t| t.soloed)
                        .unwrap_or(false)
                };
                let cmd = crate::undo::set_track_solo_cmd(tid.clone(), old_val, new_val);
                {
                    let mut proj_mut = proj.borrow_mut();
                    ts.borrow_mut()
                        .history
                        .execute(Box::new(cmd), &mut proj_mut);
                }
                on_changed();
            });
        }

        // Double-click gain fader to reset to 0 dB.
        {
            let proj = self.project.clone();
            let ts = self.timeline_state.clone();
            let tid = track_id.clone();
            let scale = gain_scale.clone();
            let readout = db_readout.clone();
            let on_changed = self.on_project_changed.clone();
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            gesture.connect_released(move |g, n_press, _, _| {
                if n_press < 2 {
                    return;
                }
                let _ = g;
                let old = {
                    let p = proj.borrow();
                    p.tracks
                        .iter()
                        .find(|t| t.id == tid)
                        .map(|t| t.gain_db)
                        .unwrap_or(0.0)
                };
                if (old - 0.0).abs() > f64::EPSILON {
                    let cmd = crate::undo::set_track_gain_cmd(tid.clone(), old, 0.0);
                    {
                        let mut proj_mut = proj.borrow_mut();
                        ts.borrow_mut()
                            .history
                            .execute(Box::new(cmd), &mut proj_mut);
                    }
                    scale.set_value(0.0);
                    readout.set_text(&format_db(0.0));
                    on_changed();
                }
            });
            gain_scale.add_controller(gesture);
        }

        // Double-click pan to reset to center.
        {
            let proj = self.project.clone();
            let ts = self.timeline_state.clone();
            let tid = track_id.clone();
            let scale = pan_scale.clone();
            let on_changed = self.on_project_changed.clone();
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            gesture.connect_released(move |g, n_press, _, _| {
                if n_press < 2 {
                    return;
                }
                let _ = g;
                let old = {
                    let p = proj.borrow();
                    p.tracks
                        .iter()
                        .find(|t| t.id == tid)
                        .map(|t| t.pan)
                        .unwrap_or(0.0)
                };
                if old.abs() > f64::EPSILON {
                    let cmd = crate::undo::set_track_pan_cmd(tid.clone(), old, 0.0);
                    {
                        let mut proj_mut = proj.borrow_mut();
                        ts.borrow_mut()
                            .history
                            .execute(Box::new(cmd), &mut proj_mut);
                    }
                    scale.set_value(0.0);
                    on_changed();
                }
            });
            pan_scale.add_controller(gesture);
        }

        ChannelStrip {
            track_id: track.id.clone(),
            track_index,
            label,
            role_badge,
            vu_meter,
            peak_cell,
            gain_scale,
            db_readout,
            pan_scale,
            mute_btn,
            solo_btn,
            root,
        }
    }

    fn build_bus_strip(&self, role: AudioRole, proj: &Project) -> BusStrip {
        let bus = proj
            .bus_for_role(&role)
            .expect("build_bus_strip called with None role");

        let root = GBox::new(Orientation::Vertical, 2);
        root.set_width_request(STRIP_WIDTH);
        root.add_css_class("mixer-channel-strip");
        root.add_css_class("mixer-bus-strip");
        root.set_margin_start(2);
        root.set_margin_end(2);
        root.set_margin_top(4);
        root.set_margin_bottom(4);

        // Bus name label.
        let label = Label::new(Some(role.label()));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(8);
        label.add_css_class("mixer-label");
        label.add_css_class("mixer-bus-label");
        root.append(&label);

        // Role badge.
        let role_badge = Label::new(Some(role_badge_text(&role)));
        role_badge.add_css_class("mixer-role-badge");
        role_badge.set_halign(gtk::Align::Center);
        root.append(&role_badge);

        // VU meter.
        let peak_cell: Rc<Cell<[f64; 2]>> = Rc::new(Cell::new([-60.0, -60.0]));
        let vu_meter = DrawingArea::new();
        vu_meter.set_content_width(METER_WIDTH);
        vu_meter.set_content_height(METER_HEIGHT);
        vu_meter.set_halign(gtk::Align::Center);
        vu_meter.add_css_class("mixer-vu-meter");
        {
            let pc = peak_cell.clone();
            vu_meter.set_draw_func(move |_da, cr, width, height| {
                draw_vu_meter(cr, width, height, pc.get());
            });
        }
        root.append(&vu_meter);

        // Gain fader (vertical).
        let gain_scale =
            gtk::Scale::with_range(Orientation::Vertical, GAIN_MIN_DB, GAIN_MAX_DB, 0.1);
        gain_scale.set_inverted(true);
        gain_scale.set_value(bus.gain_db);
        gain_scale.set_size_request(-1, 80);
        gain_scale.set_halign(gtk::Align::Center);
        gain_scale.add_css_class("mixer-fader");
        gain_scale.add_mark(0.0, gtk::PositionType::Right, Some("0"));
        root.append(&gain_scale);

        // dB readout.
        let db_readout = Label::new(Some(&format_db(bus.gain_db)));
        db_readout.add_css_class("mixer-db-readout");
        root.append(&db_readout);

        // Mute / Solo buttons.
        let btn_row = GBox::new(Orientation::Horizontal, 2);
        btn_row.set_halign(gtk::Align::Center);
        let mute_btn = gtk::ToggleButton::with_label("M");
        mute_btn.set_active(bus.muted);
        mute_btn.add_css_class("mixer-mute");
        mute_btn.set_size_request(28, 24);
        let solo_btn = gtk::ToggleButton::with_label("S");
        solo_btn.set_active(bus.soloed);
        solo_btn.add_css_class("mixer-solo");
        solo_btn.set_size_request(28, 24);
        btn_row.append(&mute_btn);
        btn_row.append(&solo_btn);
        root.append(&btn_row);

        // --- Signal handlers ---

        // Gain fader — live update + undo on release.
        {
            let proj = self.project.clone();
            let r = role;
            let readout = db_readout.clone();
            let on_changed = self.on_project_changed.clone();
            let updating = self.updating.clone();
            let drag_start_db: Rc<Cell<f64>> = Rc::new(Cell::new(bus.gain_db));
            let ds = drag_start_db.clone();
            gain_scale.connect_value_changed(move |scale| {
                if updating.get() {
                    return;
                }
                let new_db = scale.value();
                readout.set_text(&format_db(new_db));
                let mut p = proj.borrow_mut();
                if let Some(b) = p.bus_for_role_mut(&r) {
                    b.gain_db = new_db;
                }
                drop(p);
                on_changed();
            });
            let proj2 = self.project.clone();
            let r2 = role;
            let gesture_click = gtk::GestureClick::new();
            gesture_click.set_button(0);
            let ds2 = drag_start_db.clone();
            gesture_click.connect_pressed(move |_, _, _, _| {
                let p = proj2.borrow();
                if let Some(b) = p.bus_for_role(&r2) {
                    ds2.set(b.gain_db);
                }
            });
            gain_scale.add_controller(gesture_click);

            let ts3 = self.timeline_state.clone();
            let proj3 = self.project.clone();
            let r3 = role;
            let updating3 = self.updating.clone();
            let gesture_release = gtk::GestureClick::new();
            gesture_release.set_button(0);
            gesture_release.connect_released(move |_, _, _, _| {
                if updating3.get() {
                    return;
                }
                let old_db = ds.get();
                let p = proj3.borrow();
                let new_db = p.bus_for_role(&r3).map(|b| b.gain_db).unwrap_or(old_db);
                drop(p);
                if (old_db - new_db).abs() > f64::EPSILON {
                    let cmd = crate::undo::set_bus_gain_cmd(r3, old_db, new_db);
                    let mut ts = ts3.borrow_mut();
                    ts.history.undo_stack.push(Box::new(cmd));
                    ts.history.redo_stack.clear();
                }
            });
            gain_scale.add_controller(gesture_release);
        }

        // Mute button.
        {
            let proj = self.project.clone();
            let ts = self.timeline_state.clone();
            let r = role;
            let on_changed = self.on_project_changed.clone();
            let updating = self.updating.clone();
            mute_btn.connect_toggled(move |btn| {
                if updating.get() {
                    return;
                }
                let new_val = btn.is_active();
                let old_val = {
                    let p = proj.borrow();
                    p.bus_for_role(&r).map(|b| b.muted).unwrap_or(false)
                };
                let cmd = crate::undo::set_bus_mute_cmd(r, old_val, new_val);
                {
                    let mut proj_mut = proj.borrow_mut();
                    ts.borrow_mut()
                        .history
                        .execute(Box::new(cmd), &mut proj_mut);
                }
                on_changed();
            });
        }

        // Solo button.
        {
            let proj = self.project.clone();
            let ts = self.timeline_state.clone();
            let r = role;
            let on_changed = self.on_project_changed.clone();
            let updating = self.updating.clone();
            solo_btn.connect_toggled(move |btn| {
                if updating.get() {
                    return;
                }
                let new_val = btn.is_active();
                let old_val = {
                    let p = proj.borrow();
                    p.bus_for_role(&r).map(|b| b.soloed).unwrap_or(false)
                };
                let cmd = crate::undo::set_bus_solo_cmd(r, old_val, new_val);
                {
                    let mut proj_mut = proj.borrow_mut();
                    ts.borrow_mut()
                        .history
                        .execute(Box::new(cmd), &mut proj_mut);
                }
                on_changed();
            });
        }

        // Double-click gain fader to reset to 0 dB.
        {
            let proj = self.project.clone();
            let ts = self.timeline_state.clone();
            let r = role;
            let scale = gain_scale.clone();
            let readout = db_readout.clone();
            let on_changed = self.on_project_changed.clone();
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            gesture.connect_released(move |g, n_press, _, _| {
                if n_press < 2 {
                    return;
                }
                let _ = g;
                let old = {
                    let p = proj.borrow();
                    p.bus_for_role(&r).map(|b| b.gain_db).unwrap_or(0.0)
                };
                if (old - 0.0).abs() > f64::EPSILON {
                    let cmd = crate::undo::set_bus_gain_cmd(r, old, 0.0);
                    {
                        let mut proj_mut = proj.borrow_mut();
                        ts.borrow_mut()
                            .history
                            .execute(Box::new(cmd), &mut proj_mut);
                    }
                    scale.set_value(0.0);
                    readout.set_text(&format_db(0.0));
                    on_changed();
                }
            });
            gain_scale.add_controller(gesture);
        }

        BusStrip {
            role,
            vu_meter,
            peak_cell,
            gain_scale,
            db_readout,
            mute_btn,
            solo_btn,
            root,
        }
    }
}

/// Build the mixer panel widget and its shared view state.
pub fn build_mixer_panel(
    project: Rc<RefCell<Project>>,
    timeline_state: Rc<RefCell<TimelineState>>,
    on_project_changed: Rc<dyn Fn()>,
) -> (GBox, Rc<MixerPanelView>) {
    let root = GBox::new(Orientation::Horizontal, 0);
    root.set_margin_start(4);
    root.set_margin_end(4);
    root.set_margin_top(4);
    root.set_margin_bottom(4);
    root.add_css_class("mixer-panel");

    // Scrollable area for channel strips.
    let strips_box = GBox::new(Orientation::Horizontal, 0);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    scroller.set_hexpand(true);
    scroller.set_child(Some(&strips_box));
    root.append(&scroller);

    // Separator before bus strips.
    let bus_sep = Separator::new(Orientation::Vertical);
    bus_sep.set_margin_start(4);
    bus_sep.set_margin_end(4);
    root.append(&bus_sep);

    // Bus strips area (between track strips and master).
    let bus_strips_box = GBox::new(Orientation::Horizontal, 0);
    bus_strips_box.set_visible(false); // hidden until buses are populated
    root.append(&bus_strips_box);

    // Separator before master strip.
    let sep = Separator::new(Orientation::Vertical);
    sep.set_margin_start(4);
    sep.set_margin_end(4);
    root.append(&sep);

    // Master strip.
    let (master_strip, master_meter, master_peak_cell, master_gain_scale, master_db_readout) =
        build_master_strip(project.clone(), on_project_changed.clone());
    root.append(&master_strip);

    let view = Rc::new(MixerPanelView {
        strips_box,
        bus_strips_box,
        master_strip,
        master_meter,
        master_peak_cell,
        master_gain_scale,
        master_db_readout,
        strips: RefCell::new(Vec::new()),
        bus_strips: RefCell::new(Vec::new()),
        project,
        timeline_state,
        on_project_changed,
        updating: Rc::new(Cell::new(false)),
    });

    // Initial build.
    view.rebuild_from_project();

    (root, view)
}

fn build_master_strip(
    project: Rc<RefCell<Project>>,
    on_project_changed: Rc<dyn Fn()>,
) -> (GBox, DrawingArea, Rc<Cell<[f64; 2]>>, gtk::Scale, Label) {
    let root = GBox::new(Orientation::Vertical, 2);
    root.set_width_request(STRIP_WIDTH);
    root.add_css_class("mixer-channel-strip");
    root.add_css_class("mixer-master");
    root.set_margin_top(4);
    root.set_margin_bottom(4);

    let label = Label::new(Some("Master"));
    label.add_css_class("mixer-label");
    root.append(&label);

    // Master VU meter.
    let peak_cell: Rc<Cell<[f64; 2]>> = Rc::new(Cell::new([-60.0, -60.0]));
    let vu_meter = DrawingArea::new();
    vu_meter.set_content_width(METER_WIDTH);
    vu_meter.set_content_height(METER_HEIGHT);
    vu_meter.set_halign(gtk::Align::Center);
    vu_meter.add_css_class("mixer-vu-meter");
    {
        let pc = peak_cell.clone();
        vu_meter.set_draw_func(move |_da, cr, width, height| {
            draw_vu_meter(cr, width, height, pc.get());
        });
    }
    root.append(&vu_meter);

    // Master gain fader.
    let proj = project.borrow();
    let master_db = proj.master_gain_db;
    drop(proj);
    let gain_scale = gtk::Scale::with_range(Orientation::Vertical, GAIN_MIN_DB, GAIN_MAX_DB, 0.1);
    gain_scale.set_inverted(true);
    gain_scale.set_value(master_db);
    gain_scale.set_size_request(-1, 80);
    gain_scale.set_halign(gtk::Align::Center);
    gain_scale.add_css_class("mixer-fader");
    gain_scale.add_mark(0.0, gtk::PositionType::Right, Some("0"));
    root.append(&gain_scale);

    let db_readout = Label::new(Some(&format_db(master_db)));
    db_readout.add_css_class("mixer-db-readout");
    root.append(&db_readout);

    // Master gain fader handler.
    {
        let proj = project.clone();
        let readout = db_readout.clone();
        let on_changed = on_project_changed.clone();
        gain_scale.connect_value_changed(move |scale| {
            let new_db = scale.value();
            readout.set_text(&format_db(new_db));
            let mut p = proj.borrow_mut();
            p.master_gain_db = new_db;
            drop(p);
            on_changed();
        });
    }

    (root, vu_meter, peak_cell, gain_scale, db_readout)
}

/// Draw a vertical stereo VU meter with green/yellow/red zones.
fn draw_vu_meter(cr: &gtk::cairo::Context, width: i32, height: i32, peaks: [f64; 2]) {
    let [left_db, right_db] = peaks;
    let w = width as f64;
    let h = height as f64;
    let bar_w = (w / 2.0 - 2.0).max(4.0);
    let db_to_frac = |db: f64| -> f64 { ((db + 60.0) / 60.0).clamp(0.0, 1.0) };

    // Background.
    cr.set_source_rgb(0.13, 0.13, 0.13);
    cr.rectangle(0.0, 0.0, w, h);
    let _ = cr.fill();

    for (ch, db) in [(0, left_db), (1, right_db)] {
        let x = ch as f64 * (bar_w + 2.0) + 1.0;
        let frac = db_to_frac(db);
        let bar_h = frac * h;

        // Green zone: below −18 dBFS.
        let green_frac = db_to_frac(-18.0);
        let green_h = (green_frac * h).min(bar_h);
        if green_h > 0.0 {
            let (r, g, b) = crate::ui::colors::COLOR_LEVEL_GOOD;
            cr.set_source_rgb(r, g, b);
            cr.rectangle(x, h - green_h, bar_w, green_h);
            let _ = cr.fill();
        }

        // Yellow zone: −18 to −6 dBFS.
        let yellow_frac = db_to_frac(-6.0);
        let yellow_top = green_frac * h;
        let yellow_h = ((yellow_frac - green_frac) * h).min((bar_h - green_h).max(0.0));
        if yellow_h > 0.0 {
            let (r, g, b) = crate::ui::colors::COLOR_LEVEL_WARN;
            cr.set_source_rgb(r, g, b);
            cr.rectangle(x, h - yellow_top - yellow_h, bar_w, yellow_h);
            let _ = cr.fill();
        }

        // Red zone: above −6 dBFS.
        let red_top = yellow_frac * h;
        let red_h = (bar_h - red_top).max(0.0);
        if red_h > 0.0 {
            let (r, g, b) = crate::ui::colors::COLOR_LEVEL_CLIP;
            cr.set_source_rgb(r, g, b);
            cr.rectangle(x, h - bar_h, bar_w, red_h);
            let _ = cr.fill();
        }
    }
}

fn role_badge_text(role: &AudioRole) -> &'static str {
    match role {
        AudioRole::None => "",
        AudioRole::Dialogue => "D",
        AudioRole::Effects => "E",
        AudioRole::Music => "M",
    }
}

fn format_db(db: f64) -> String {
    if db <= GAIN_MIN_DB {
        "−∞ dB".to_string()
    } else {
        format!("{db:+.1} dB")
    }
}
