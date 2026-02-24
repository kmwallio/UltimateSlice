pub mod widget;
pub use widget::{TimelineState, ActiveTool, build_timeline};

/// Build the full timeline panel: timeline DrawingArea + track management bar.
/// The returned widget is a VBox containing both.
pub fn build_timeline_panel(
    state: std::rc::Rc<std::cell::RefCell<TimelineState>>,
    on_project_changed: std::rc::Rc<dyn Fn()>,
) -> gtk4::Box {
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
    let btn_remove    = Button::with_label("✕ Remove Track");
    btn_add_video.add_css_class("small-btn");
    btn_add_audio.add_css_class("small-btn");
    btn_remove.add_css_class("small-btn");

    {
        let state = state.clone();
        let on_project_changed = on_project_changed.clone();
        let area = area.clone();
        btn_add_video.connect_clicked(move |_| {
            let count = {
                let proj = state.borrow().project.borrow().tracks.len();
                proj
            };
            let label = format!("Video {}", count + 1);
            state.borrow().project.borrow_mut().tracks.push(
                crate::model::track::Track::new_video(label)
            );
            state.borrow().project.borrow_mut().dirty = true;
            area.set_content_height(
                (24.0 + 60.0 * state.borrow().project.borrow().tracks.len() as f64) as i32
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
            let count = {
                state.borrow().project.borrow().tracks.len()
            };
            let label = format!("Audio {}", count + 1);
            state.borrow().project.borrow_mut().tracks.push(
                crate::model::track::Track::new_audio(label)
            );
            state.borrow().project.borrow_mut().dirty = true;
            area.set_content_height(
                (24.0 + 60.0 * state.borrow().project.borrow().tracks.len() as f64) as i32
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
                let st = state.borrow();
                let mut proj = st.project.borrow_mut();
                if let Some(ref tid) = selected_track {
                    if proj.tracks.len() > 1 {
                        proj.tracks.retain(|t| &t.id != tid);
                        proj.dirty = true;
                    }
                } else if proj.tracks.len() > 1 {
                    proj.tracks.pop();
                    proj.dirty = true;
                }
            }
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

    vbox
}
