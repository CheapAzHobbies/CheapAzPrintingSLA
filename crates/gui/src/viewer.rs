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
use std::cell::{Cell, RefCell};
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
    /// Where the zoom is heading. The displayed zoom eases toward this rather
    /// than jumping, which turns a burst of wheel notches into one motion
    /// instead of a stack of separate relayouts.
    target: Cell<Option<f64>>,
    /// The point being held still, as (image x, image y, view x, view y).
    /// Kept in image coordinates so it stays correct across every frame of the
    /// animation rather than being recomputed from a moving scroll position.
    anchor: Cell<Option<(f64, f64, f64, f64)>>,
    animation: RefCell<Option<gtk::TickCallbackId>>,
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

        let overlay = gtk::Overlay::builder().child(&scroller).build();
        let frame = gtk::Frame::builder().child(&overlay).build();
        frame.add_css_class("cz-panel");
        frame.set_hexpand(true);
        frame.set_vexpand(true);

        let zoom_label = gtk::Label::new(Some("Fit"));
        zoom_label.add_css_class("caption");
        zoom_label.add_css_class("cz-value");
        zoom_label.set_width_chars(6);

        let me = Rc::new(Self {
            widget: gtk::Box::new(gtk::Orientation::Vertical, 0),
            scroller: scroller.clone(),
            picture: picture.clone(),
            zoom_label,
            zoom: Cell::new(None),
            target: Cell::new(None),
            anchor: Cell::new(None),
            animation: RefCell::new(None),
            natural: Cell::new((0, 0)),
        });
        me.widget.append(&frame);
        overlay.add_overlay(&me.controls());

        me.install_wheel();
        me.install_drag();
        me.install_double_click();
        me
    }

    /// The zoom controls, as an overlay in the corner of the image.
    ///
    /// They were briefly a row in the layer bar, which squeezed the layer
    /// slider down to almost nothing. They belong over the thing they act on.
    fn controls(self: &Rc<Self>) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_1);

        let out = crate::shell::icon_button("zoom-out-symbolic", "Zoom out  (−)");
        let inn = crate::shell::icon_button("zoom-in-symbolic", "Zoom in  (+)");
        let fit = gtk::Button::with_label("Fit");
        fit.add_css_class("flat");
        fit.set_tooltip_text(Some("Show the whole plate  (0, or double click)"));
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
        row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        row.append(&fit);
        row.append(&one);
        row.add_css_class("cz-overlay-bar");
        row.set_halign(gtk::Align::End);
        row.set_valign(gtk::Align::End);
        row.set_margin_end(theme::SPACE_3);
        row.set_margin_bottom(theme::SPACE_3);
        row
    }

    /// Show a texture. Keeps the current zoom, so stepping through layers does
    /// not throw away the magnification you set to look at something.
    pub fn set_texture(self: &Rc<Self>, texture: &gdk::Texture) {
        let changed = self.natural.get() != (texture.width(), texture.height());
        if changed {
            // A different size means the anchor no longer refers to anything.
            self.stop_animation();
            self.anchor.set(None);
        }
        self.natural.set((texture.width(), texture.height()));
        self.picture.set_paintable(Some(texture));
        self.apply();
    }

    pub fn clear(&self) {
        self.picture.set_paintable(gdk::Paintable::NONE);
        self.natural.set((0, 0));
    }

    pub fn fit(self: &Rc<Self>) {
        self.stop_animation();
        self.zoom.set(None);
        self.anchor.set(None);
        self.apply();
    }

    fn stop_animation(&self) {
        if let Some(id) = self.animation.borrow_mut().take() {
            id.remove();
        }
        self.target.set(None);
    }

    pub fn is_fit(&self) -> bool {
        self.zoom.get().is_none()
    }

    /// Current factor, resolving fit against the pane it is in.
    pub fn effective_zoom(&self) -> f64 {
        match self.zoom.get() {
            Some(z) => z,
            None => self.fit_zoom(),
        }
    }

    pub fn set_zoom(self: &Rc<Self>, zoom: f64, anchor: Option<(f64, f64)>) {
        let clamped = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        // Record the anchor in image coordinates once, at the start. Deriving
        // it from the scroll position each frame would chase a value the
        // animation is itself moving.
        if let Some((vx, vy)) = anchor {
            let now = self.effective_zoom();
            let h = self.scroller.hadjustment().value();
            let v = self.scroller.vadjustment().value();
            self.anchor
                .set(Some(((h + vx) / now, (v + vy) / now, vx, vy)));
        } else {
            self.anchor.set(None);
        }
        self.target.set(Some(clamped));
        self.start_animation();
    }

    /// Ease the displayed zoom toward the target, one frame at a time.
    ///
    /// Exponential rather than a fixed duration: a burst of wheel notches
    /// keeps moving the target and the motion simply continues, where a
    /// timed tween would restart and stutter on every notch.
    fn start_animation(self: &Rc<Self>) {
        if self.animation.borrow().is_some() {
            return;
        }
        let me = self.clone();
        let id = self.scroller.add_tick_callback(move |_, _| {
            let Some(target) = me.target.get() else {
                return glib::ControlFlow::Break;
            };
            let current = me.zoom.get().unwrap_or_else(|| me.fit_zoom());
            // About 150ms to settle at 60fps, which reads as movement without
            // feeling like a wait (§23).
            let next = current + (target - current) * 0.28;
            let done = (target / next).ln().abs() < 0.002;
            let value = if done { target } else { next };
            me.zoom.set(Some(value));
            me.apply();
            me.hold_anchor(value);
            if done {
                me.target.set(None);
                *me.animation.borrow_mut() = None;
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.animation.borrow_mut() = Some(id);
    }

    /// Put the anchored image point back under the pointer at this zoom.
    fn hold_anchor(&self, zoom: f64) {
        let Some((ix, iy, vx, vy)) = self.anchor.get() else {
            return;
        };
        let h = self.scroller.hadjustment();
        let v = self.scroller.vadjustment();
        let nx = ix * zoom - vx;
        let ny = iy * zoom - vy;
        h.set_value(nx.clamp(h.lower(), (h.upper() - h.page_size()).max(h.lower())));
        v.set_value(ny.clamp(v.lower(), (v.upper() - v.page_size()).max(v.lower())));
    }

    fn zoom_by(self: &Rc<Self>, factor: f64, anchor: Option<(f64, f64)>) {
        // From where it is going, not where it is, so a quick second notch
        // adds to the first instead of restarting from a half-finished value.
        let from = self.target.get().unwrap_or_else(|| self.effective_zoom());
        let target = from * factor;
        // Zooming out lands on Fit and stops there. The whole plate is the
        // view people return to after examining something, and stepping past
        // it into a postage stamp helps nobody. Fit is also the state the
        // window keeps in step as it resizes, so landing on it exactly rather
        // than near it matters.
        if factor < 1.0 && target <= self.fit_zoom() {
            self.fit();
            return;
        }
        self.set_zoom(target, anchor);
    }

    /// The factor at which the whole layer is visible in the current pane.
    fn fit_zoom(&self) -> f64 {
        let (w, h) = self.natural.get();
        if w == 0 || h == 0 {
            return 1.0;
        }
        let aw = self.scroller.width().max(1) as f64;
        let ah = self.scroller.height().max(1) as f64;
        (aw / w as f64).min(ah / h as f64).min(1.0)
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
                // Contain, not Fill. Fill ignores the aspect ratio, so any
                // moment the allocation does not exactly match the request the
                // layer is stretched, which is what made zooming look wrong.
                self.picture.set_content_fit(gtk::ContentFit::Contain);
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
        // Capture, not bubble. A scrolled window with something to scroll
        // handles the wheel itself, so a controller in the default phase never
        // sees the event once the layer is larger than the pane, which leaves
        // the view stuck at whatever magnification it reached.
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        self.scroller.add_controller(scroll);
    }

    fn install_double_click(self: &Rc<Self>) {
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        let me = self.clone();
        click.connect_pressed(move |_, n, x, y| {
            if n == 2 {
                if me.is_fit() {
                    me.set_zoom(1.0, Some((x, y)));
                } else {
                    me.fit();
                }
            }
        });
        self.scroller.add_controller(click);
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
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
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
