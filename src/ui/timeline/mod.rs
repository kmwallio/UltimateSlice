pub mod widget;
pub use widget::{build_timeline, ActiveTool, TimelineState};

/// Build the full timeline panel: timeline DrawingArea + track management bar.
/// Returns `(panel_box, drawing_area)` — call `drawing_area.queue_draw()` to repaint.
pub fn build_timeline_panel(
    state: std::rc::Rc<std::cell::RefCell<TimelineState>>,
    on_project_changed: std::rc::Rc<dyn Fn()>,
) -> (gtk4::Box, gtk4::DrawingArea) {
    use gtk4::prelude::*;
    use gtk4::{self as gtk, Box as GBox, Button, Orientation};

    let vbox = GBox::new(Orientation::Vertical, 0);

    let area = build_timeline(state.clone());
    vbox.append(&area);

    // ── Track management bar ─────────────────────────────────────────────
    let bar = GBox::new(Orientation::Horizontal, 4);
    bar.set_margin_start(8);
    bar.set_margin_end(8);
    bar.set_margin_top(4);
    bar.set_margin_bottom(4);

    let btn_add_video = Button::with_label("＋ Video Track");
    let btn_add_audio = Button::with_label("＋ Audio Track");
    let btn_remove = Button::with_label("✕ Remove Track");
    btn_add_video.add_css_class("small-btn");
    btn_add_audio.add_css_class("small-btn");
    btn_remove.add_css_class("small-btn");

    {
        let state = state.clone();
        let on_project_changed = on_project_changed.clone();
        let area = area.clone();
        btn_add_video.connect_clicked(move |_| {
            let label = {
                let count = state.borrow().project.borrow().tracks.len();
                format!("Video {}", count + 1)
            };
            let track = crate::model::track::Track::new_video(label);
            {
                let mut st = state.borrow_mut();
                let index = st.project.borrow().tracks.len();
                let cmd = crate::undo::AddTrackCommand { track, index };
                let project_rc = st.project.clone();
                let mut proj = project_rc.borrow_mut();
                st.history.execute(Box::new(cmd), &mut proj);
            }
            area.set_content_height(
                (24.0 + 60.0 * state.borrow().project.borrow().tracks.len() as f64) as i32,
            );
            on_project_changed();
            area.queue_draw();
        });
    }
    {
        let state = state.clone();
        let on_project_changed = on_project_changed.clone();
        let area = area.clone();
        btn_add_audio.connect_clicked(move |_| {
            let label = {
                let count = state.borrow().project.borrow().tracks.len();
                format!("Audio {}", count + 1)
            };
            let track = crate::model::track::Track::new_audio(label);
            {
                let mut st = state.borrow_mut();
                let index = st.project.borrow().tracks.len();
                let cmd = crate::undo::AddTrackCommand { track, index };
                let project_rc = st.project.clone();
                let mut proj = project_rc.borrow_mut();
                st.history.execute(Box::new(cmd), &mut proj);
            }
            area.set_content_height(
                (24.0 + 60.0 * state.borrow().project.borrow().tracks.len() as f64) as i32,
            );
            on_project_changed();
            area.queue_draw();
        });
    }
    {
        let state = state.clone();
        let on_project_changed = on_project_changed.clone();
        let area = area.clone();
        btn_remove.connect_clicked(move |_| {
            let selected_track = state.borrow().selected_track_id.clone();
            {
                let mut st = state.borrow_mut();
                let project_rc = st.project.clone();
                let proj = project_rc.borrow();
                if proj.tracks.len() > 1 {
                    let remove_idx = if let Some(ref tid) = selected_track {
                        proj.tracks.iter().position(|t| &t.id == tid)
                    } else {
                        Some(proj.tracks.len() - 1)
                    };
                    if let Some(idx) = remove_idx {
                        let track = proj.tracks[idx].clone();
                        drop(proj);
                        let cmd = crate::undo::DeleteTrackCommand { track, index: idx };
                        let mut proj = project_rc.borrow_mut();
                        st.history.execute(Box::new(cmd), &mut proj);
                    }
                }
            }
            // Clear stale selection after removal
            state.borrow_mut().selected_track_id = None;
            state.borrow_mut().selected_clip_id = None;
            let n = state.borrow().project.borrow().tracks.len();
            area.set_content_height((24.0 + 60.0 * n as f64) as i32);
            on_project_changed();
            area.queue_draw();
        });
    }

    bar.append(&btn_add_video);
    bar.append(&btn_add_audio);
    bar.append(&btn_remove);
    vbox.append(&bar);

    (vbox, area)
}
