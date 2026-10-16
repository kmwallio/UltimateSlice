//! `TimelineArea` — the timeline's track-area widget.
//!
//! A `gtk::DrawingArea` always hands GSK one full-size *cairo* render node,
//! and the GPU renderers re-rasterise (clear + replay + upload) every cairo
//! node intersecting the frame's damage region on every frame. During
//! playback the Program Monitor damages the window ~30×/s and the playhead
//! redraws the timeline ~20×/s, so a 1500×300 cairo node cost ~10 ms of
//! main-thread CPU per redraw regardless of how cheap our own draw code was.
//!
//! This widget subclass owns its `snapshot()` so the timeline can append a
//! cached **texture** node for the track content (uploaded once per content
//! change; a GPU quad thereafter) plus only small cairo/colour nodes for the
//! live parts (label column with level meters, playhead). The full cairo
//! draw path is still used whenever anything interactive is going on.

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::RefCell;

type SnapshotFn = dyn Fn(&TimelineArea, &gtk::Snapshot, i32, i32);

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TimelineArea {
        pub(super) snapshot_func: RefCell<Option<Box<SnapshotFn>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TimelineArea {
        const NAME: &'static str = "UltimateSliceTimelineArea";
        type Type = super::TimelineArea;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TimelineArea {}

    impl WidgetImpl for TimelineArea {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width(), obj.height());
            if w <= 0 || h <= 0 {
                return;
            }
            // Take the closure out for the duration of the call so a
            // re-entrant `set_snapshot_func` can never double-borrow.
            let f = self.snapshot_func.borrow_mut().take();
            if let Some(f) = f {
                f(&obj, snapshot, w, h);
                let mut slot = self.snapshot_func.borrow_mut();
                if slot.is_none() {
                    *slot = Some(f);
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct TimelineArea(ObjectSubclass<imp::TimelineArea>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TimelineArea {
    fn default() -> Self {
        Self::new()
    }
}

impl TimelineArea {
    pub fn new() -> Self {
        let obj: Self = glib::Object::new();
        // Our nodes are explicitly bounded, but never let a stray node bleed
        // outside the allocation (DrawingArea semantics).
        obj.set_overflow(gtk::Overflow::Hidden);
        obj
    }

    /// Install the snapshot callback (`(widget, snapshot, width, height)`),
    /// replacing any previous one. Mirrors `DrawingArea::set_draw_func`.
    pub fn set_snapshot_func(&self, f: impl Fn(&Self, &gtk::Snapshot, i32, i32) + 'static) {
        *self.imp().snapshot_func.borrow_mut() = Some(Box::new(f));
    }

    /// Minimum height, mirroring `DrawingArea::set_content_height`.
    pub fn set_content_height(&self, height: i32) {
        self.set_size_request(-1, height.max(0));
    }
}
