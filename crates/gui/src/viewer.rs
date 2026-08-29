//! The layer viewport: zoom, pan and fit (§16, §18, §19).
//!
//! A scrolled window holding a picture whose size is set explicitly. Letting
//! the scrolled window do the scrolling means panning, keyboard scrolling and
//! the scrollbars all work without being reimplemented, and the only thing
//! this has to get right is the size and the anchor point.
//!
//! Two states rather than a continuum:
//!
//! * **Fit** — the layer is scaled to the pane and the zoom follows the window
//!   as it resizes. This is where inspection starts: you want the whole plate.
//! * **Zoomed** — an explicit factor, the picture is sized to match and the
//!   pane scrolls. This is where you check whether a support actually touches.
//!
//! Zooming with the wheel keeps the point under the pointer still. Zooming
//! about the centre instead makes a detail you are examining slide away, which
//! turns every zoom into a hunt for what you were already looking at.

use crate::theme;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// Zoom limits. Below the lower bound the layer is a speck; above the upper
/// one a single exposure pixel fills a large part of the screen, which is
/// occasionally exactly what you want when checking anti-aliasing.
const MIN_ZOOM: f64 = 0.05;
const MAX_ZOOM: f64 = 32.0;
/// One wheel notch. A ratio rather than an increment, so each step feels the
/// same at any magnification.
const STEP: f64 = 1.25;

pub struct LayerViewer {
    pub widget: gtk::Box,
    scroller: gtk::ScrolledWindow,
    picture: gtk::Picture,
    zoom_label: gtk::Label,
    /// None means fit to the pane.
    zoom: Cell<Option<f64>>,
    /// Natural size of the texture on show.
    natural: Cell<(i32, i32)>,
}

impl LayerViewer {
    pub fn new() -> Rc<Self> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .hexpand(true)
            .vexpand(true)
            .build();

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .vexpand(true)
            .child(&picture)
            .build();

        let frame = gtk::Frame::builder().child(&scroller).build();
        frame.add_css_class("cz-panel");
        frame.set_hexpand(true);
        frame.set_vexpand(true);

        let zoom_label = gtk::Label::new(Some("Fit"));
        zoom_label.add_css_class("caption");
        zoom_label.add_css_class("cz-dim");
        zoom_label.set_width_chars(6);

        let me = Rc::new(Self {
            widget: gtk::Box::new(gtk::Orientation::Vertical, 0),
            scroller: scroller.clone(),
            picture: picture.clone(),
            zoom_label,
            zoom: Cell::new(None),
            natural: Cell::new((0, 0)),
        });
        me.widget.append(&frame);

        me.install_wheel();
        me.install_drag();
        me
    }

    /// The zoom controls, so the caller can place them in its own bar.
    pub fn controls(self: &Rc<Self>) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_1);

        let out = crate::shell::icon_button("zoom-out-symbolic", "Zoom out  (−)");
        let inn = crate::shell::icon_button("zoom-in-symbolic", "Zoom in  (+)");
        let fit = crate::shell::icon_button("zoom-fit-best-symbolic", "Fit to window  (0)");
        let one = crate::shell::icon_button("zoom-original-symbolic", "Actual size  (1)");

        {
            let me = self.clone();
            out.connect_clicked(move |_| me.zoom_by(1.0 / STEP, None));
        }
        {
            let me = self.clone();
            inn.connect_clicked(move |_| me.zoom_by(STEP, None));
        }
        {
            let me = self.clone();
            fit.connect_clicked(move |_| me.fit());
        }
        {
            let me = self.clone();
            one.connect_clicked(move |_| me.set_zoom(1.0, None));
        }

        row.append(&out);
        row.append(&self.zoom_label);
        row.append(&inn);
        row.append(&fit);
        row.append(&one);
        row
    }

    /// Show a texture. Keeps the current zoom, so stepping through layers does
    /// not throw away the magnification you set to look at something.
    pub fn set_texture(self: &Rc<Self>, texture: &gdk::Texture) {
        self.natural.set((texture.width(), texture.height()));
        self.picture.set_paintable(Some(texture));
        self.apply();
    }

    pub fn clear(&self) {
        self.picture.set_paintable(gdk::Paintable::NONE);
        self.natural.set((0, 0));
    }

    pub fn fit(self: &Rc<Self>) {
        self.zoom.set(None);
        self.apply();
    }

    pub fn is_fit(&self) -> bool {
        self.zoom.get().is_none()
    }

    /// Current factor, resolving fit against the pane it is in.
    pub fn effective_zoom(&self) -> f64 {
        match self.zoom.get() {
            Some(z) => z,
            None => {
                let (w, h) = self.natural.get();
                if w == 0 || h == 0 {
                    return 1.0;
                }
                let aw = self.scroller.width().max(1) as f64;
                let ah = self.scroller.height().max(1) as f64;
                (aw / w as f64).min(ah / h as f64).min(1.0)
            }
        }
    }

    pub fn set_zoom(self: &Rc<Self>, zoom: f64, anchor: Option<(f64, f64)>) {
        let clamped = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let previous = self.effective_zoom();
        self.zoom.set(Some(clamped));
        self.apply();
        if let Some((ax, ay)) = anchor {
            self.keep_anchor(ax, ay, previous, clamped);
        }
    }

    fn zoom_by(self: &Rc<Self>, factor: f64, anchor: Option<(f64, f64)>) {
        self.set_zoom(self.effective_zoom() * factor, anchor);
    }

    /// Hold the point under the pointer still across a zoom.
    fn keep_anchor(&self, ax: f64, ay: f64, from: f64, to: f64) {
        if from <= 0.0 {
            return;
        }
        let h = self.scroller.hadjustment();
        let v = self.scroller.vadjustment();
        let ratio = to / from;
        // Where the anchor sits in the image, then where it must sit after.
        let nx = (h.value() + ax) * ratio - ax;
        let ny = (v.value() + ay) * ratio - ay;
        // Applied after the size request has been acted on, or the adjustment
        // is still clamped to the old extent and the anchor drifts.
        let (h2, v2) = (h.clone(), v.clone());
        glib::idle_add_local_once(move || {
            h2.set_value(nx.clamp(h2.lower(), (h2.upper() - h2.page_size()).max(h2.lower())));
            v2.set_value(ny.clamp(v2.lower(), (v2.upper() - v2.page_size()).max(v2.lower())));
        });
    }

    fn apply(self: &Rc<Self>) {
        let (w, h) = self.natural.get();
        match self.zoom.get() {
            None => {
                self.picture.set_content_fit(gtk::ContentFit::Contain);
                self.picture.set_size_request(-1, -1);
                self.scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
                self.scroller.set_vscrollbar_policy(gtk::PolicyType::Never);
                self.zoom_label.set_text("Fit");
            }
            Some(z) => {
                self.picture.set_content_fit(gtk::ContentFit::Fill);
                self.picture.set_size_request(
                    ((w as f64 * z).round() as i32).max(1),
                    ((h as f64 * z).round() as i32).max(1),
                );
                self.scroller
                    .set_hscrollbar_policy(gtk::PolicyType::Automatic);
                self.scroller
                    .set_vscrollbar_policy(gtk::PolicyType::Automatic);
                self.zoom_label.set_text(&format!("{:.0}%", z * 100.0));
            }
        }
    }

    fn install_wheel(self: &Rc<Self>) {
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE,
        );
        // Where the pointer is, tracked separately: a scroll event carries a
        // delta but not a position.
        let pointer = Rc::new(Cell::new((0.0, 0.0)));
        let motion = gtk::EventControllerMotion::new();
        {
            let pointer = pointer.clone();
            motion.connect_motion(move |_, x, y| pointer.set((x, y)));
        }
        self.scroller.add_controller(motion);

        let me = self.clone();
        scroll.connect_scroll(move |_, _, dy| {
            if dy == 0.0 {
                return glib::Propagation::Proceed;
            }
            let factor = if dy < 0.0 { STEP } else { 1.0 / STEP };
            me.zoom_by(factor, Some(pointer.get()));
            glib::Propagation::Stop
        });
        self.scroller.add_controller(scroll);
    }

    fn install_drag(self: &Rc<Self>) {
        let drag = gtk::GestureDrag::new();
        let start = Rc::new(Cell::new((0.0, 0.0)));
        {
            let me = self.clone();
            let start = start.clone();
            drag.connect_drag_begin(move |_, _, _| {
                start.set((
                    me.scroller.hadjustment().value(),
                    me.scroller.vadjustment().value(),
                ));
            });
        }
        {
            let me = self.clone();
            let start = start.clone();
            drag.connect_drag_update(move |_, dx, dy| {
                // Nothing to pan when the whole layer already fits.
                if me.is_fit() {
                    return;
                }
                let (sx, sy) = start.get();
                let h = me.scroller.hadjustment();
                let v = me.scroller.vadjustment();
                h.set_value((sx - dx).clamp(h.lower(), (h.upper() - h.page_size()).max(h.lower())));
                v.set_value((sy - dy).clamp(v.lower(), (v.upper() - v.page_size()).max(v.lower())));
            });
        }
        self.scroller.add_controller(drag);
    }

    /// Keyboard zoom, returning whether the key was one of ours.
    pub fn handle_key(self: &Rc<Self>, key: gdk::Key) -> bool {
        match key {
            gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => {
                self.zoom_by(STEP, None);
                true
            }
            gdk::Key::minus | gdk::Key::KP_Subtract => {
                self.zoom_by(1.0 / STEP, None);
                true
            }
            gdk::Key::_0 | gdk::Key::KP_0 => {
                self.fit();
                true
            }
            gdk::Key::_1 | gdk::Key::KP_1 => {
                self.set_zoom(1.0, None);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    // Zoom arithmetic, kept free of widgets so it can be checked without a
    // display.

    fn clamp(z: f64) -> f64 {
        z.clamp(super::MIN_ZOOM, super::MAX_ZOOM)
    }

    #[test]
    fn zoom_is_bounded_at_both_ends() {
        assert_eq!(clamp(1000.0), super::MAX_ZOOM);
        assert_eq!(clamp(0.0001), super::MIN_ZOOM);
        assert_eq!(clamp(1.0), 1.0);
    }

    #[test]
    fn a_step_is_reversible() {
        // Stepping in then out must land back where it started, or repeated
        // wheel movements drift.
        let z = 1.0_f64;
        let there = z * super::STEP;
        let back = there / super::STEP;
        assert!((back - z).abs() < 1e-12);
    }

    #[test]
    fn the_anchor_formula_holds_the_point_still() {
        // A point 100px into the view at 1x must still be under the pointer
        // once the image is twice the size.
        let (scroll, anchor, from, to) = (0.0_f64, 100.0_f64, 1.0_f64, 2.0_f64);
        let new_scroll = (scroll + anchor) * (to / from) - anchor;
        // The image coordinate under the pointer before and after.
        let before = (scroll + anchor) / from;
        let after = (new_scroll + anchor) / to;
        assert!((before - after).abs() < 1e-9, "{before} vs {after}");
    }
}
