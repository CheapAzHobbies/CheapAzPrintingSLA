//! A chain of milestones with the broken link crossed out.
//!
//! The shape is the old network-status dialog: a row of things that have to be
//! true, joined left to right. Its whole value is that a glance tells you
//! which link is the broken one, rather than a single line of text that says
//! something is wrong and leaves you to work out where.
//!
//! Three appearances and one exception, which is the whole language:
//!
//! * **grey and still** - not done, and nothing is expected of it yet;
//! * **breathing** - live: looking for something, or working on it;
//! * **solid white** - done;
//! * **dull under a red cross** - the broken link.
//!
//! Green appears once, on the last stop, and only when a file has actually
//! landed on the drive. A drive that is merely chosen is not an achievement,
//! so it does not get the colour that means one.
//!
//! The connectors follow the same rule. A full bar means something crossed it,
//! not that the two ends are connected. A leg being waited on bounces; a leg
//! whose length is known fills, with the time left written above it.
//!
//! Each stop is a button, because the place the chain says a link is broken is
//! the obvious place to press to mend it.

use crate::theme;
use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Where one milestone has got to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Grey and still. Not this stop's turn, and nothing is wrong with it.
    Idle,
    /// Breathing between grey and white: looking, or working.
    Live,
    /// Solid white. Behind us.
    Done,
    /// Solid green. The end of the chain, actually reached.
    Landed,
    /// Dull, under a red cross. What this stop needs is not there.
    Missing,
}

const CLASSES: [&str; 5] = [
    "cz-step-idle",
    "cz-step-live",
    "cz-step-done",
    "cz-step-landed",
    "cz-step-missing",
];

/// How wide a stop's second line may be before it starts sliding instead, at
/// each width the window is allowed to be. A fixed number rather than the
/// allocated width, because a widget's width reads as zero until it has been
/// laid out and this has to be right the first time it is asked.
const NOTE_WIDTH: [i32; 3] = [92, 68, 52];
/// And how long the connectors are at those widths. The chain has to fit in a
/// tiled half-screen window, and the legs are the part of it carrying no words
/// - so they are the part that gives up room first.
const LINK_WIDTH: [i32; 3] = [96, 48, 22];
const NOTE_SPEED: f64 = 34.0;
const NOTE_GAP: i32 = 28;

struct Step {
    button: gtk::Button,
    icon: gtk::Image,
    cross: gtk::DrawingArea,
    label: gtk::Label,
    note: Note,
}

/// Two strokes, struck corner to corner over a stop's icon.
///
/// Drawn rather than taken from the icon theme, because `window-close` is a
/// small X inside a button on some themes and a bare one on others, and this
/// has to read as a cross through the icon on every desktop it lands on. It
/// is deliberately larger than the icon: the whole point of the cross is to be
/// the thing you find without looking for it.
fn cross_area() -> gtk::DrawingArea {
    const SIZE: i32 = 34;
    let area = gtk::DrawingArea::new();
    area.set_content_width(SIZE);
    area.set_content_height(SIZE);
    area.set_halign(gtk::Align::Center);
    area.set_valign(gtk::Align::Center);
    area.set_can_target(false);
    area.set_visible(false);
    area.set_draw_func(|_, cr, w, h| {
        let (r, g, b) = theme::error_rgb();
        let inset = 4.0;
        let (w, h) = (w as f64, h as f64);
        cr.set_line_width(4.0);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        // A dark backing stroke first, so the red reads over a light icon as
        // well as a dark one without having to know which it is over.
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
        cr.set_line_width(6.5);
        for (x1, y1, x2, y2) in [
            (inset, inset, w - inset, h - inset),
            (w - inset, inset, inset, h - inset),
        ] {
            cr.move_to(x1, y1);
            cr.line_to(x2, y2);
            let _ = cr.stroke();
        }
        cr.set_source_rgb(r, g, b);
        cr.set_line_width(4.0);
        for (x1, y1, x2, y2) in [
            (inset, inset, w - inset, h - inset),
            (w - inset, inset, inset, h - inset),
        ] {
            cr.move_to(x1, y1);
            cr.line_to(x2, y2);
            let _ = cr.stroke();
        }
    });
    area
}

/// A second line, which on one stop slides when it is too long to fit.
///
/// Two copies of the same text with a gap between them: scroll by one copy
/// plus the gap and the second is exactly where the first began, so going back
/// to the start is invisible.
///
/// Only the stop naming the file gets to do this. A filename is the one thing
/// here that has to be read in full and cannot be shortened without losing
/// what it says; a folder or a drive is recognisable from its beginning. Four
/// lines sliding at once is four things moving and nothing being read.
struct Note {
    view: gtk::ScrolledWindow,
    first: gtk::Label,
    second: gtk::Label,
    labels: [gtk::Label; 2],
    sliding: Rc<Cell<bool>>,
    /// Shared with the chain, so a change of width is one write rather than
    /// one per stop.
    room: Rc<Cell<i32>>,
    /// Whether overflowing text slides or is simply cut short.
    slides: bool,
}

impl Note {
    fn new(room: Rc<Cell<i32>>, slides: bool) -> Self {
        let first = gtk::Label::builder().label("").build();
        let second = gtk::Label::builder().label("").build();
        if !slides {
            first.set_ellipsize(gtk::pango::EllipsizeMode::End);
        }
        for l in [&first, &second] {
            l.add_css_class("caption");
        }
        second.set_visible(false);

        let train = gtk::Box::new(gtk::Orientation::Horizontal, NOTE_GAP);
        train.append(&first);
        train.append(&second);
        train.set_halign(gtk::Align::Center);

        let view = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .width_request(room.get())
            .child(&train)
            .build();
        view.set_overflow(gtk::Overflow::Hidden);
        view.set_visible(false);

        Self {
            labels: [first.clone(), second.clone()],
            view,
            first,
            second,
            sliding: Rc::new(Cell::new(false)),
            room,
            slides,
        }
    }

    fn set(&self, text: Option<&str>) {
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            self.sliding.set(false);
            self.view.set_visible(false);
            return;
        };
        if self.first.text() == text && self.view.is_visible() {
            return;
        }
        self.sliding.set(false);
        self.first.set_text(text);
        self.second.set_text(text);
        self.view.set_visible(true);
        self.view.hadjustment().set_value(0.0);

        // Only what does not fit gets to move, and only where moving is what
        // this stop does. A name that is already readable sliding about would
        // be motion carrying no information.
        let wanted = self.first.measure(gtk::Orientation::Horizontal, -1).1;
        if !self.slides || wanted <= self.room.get() {
            self.second.set_visible(false);
            self.first.set_halign(gtk::Align::Center);
            return;
        }
        self.first.set_halign(gtk::Align::Start);
        self.second.set_visible(true);
        self.sliding.set(true);

        let sliding = self.sliding.clone();
        let adj = self.view.hadjustment();
        let lead = self.first.clone();
        let second = self.second.clone();
        let last: Cell<Option<i64>> = Cell::new(None);
        self.view.add_tick_callback(move |_, clock| {
            if !sliding.get() {
                second.set_visible(false);
                adj.set_value(0.0);
                return glib::ControlFlow::Break;
            }
            let now = clock.frame_time();
            let dt = match last.replace(Some(now)) {
                Some(previous) => ((now - previous) as f64 / 1e6).clamp(0.0, 0.1),
                None => 0.0,
            };
            let lap = (lead.width() + NOTE_GAP) as f64;
            if lap <= 1.0 {
                return glib::ControlFlow::Continue;
            }
            let next = adj.value() + NOTE_SPEED * dt;
            adj.set_value(if next >= lap { next - lap } else { next });
            glib::ControlFlow::Continue
        });
    }
}

/// One connector, and the line of text above it.
struct Link {
    bar: gtk::ProgressBar,
    note: gtk::Label,
}

pub struct Steps {
    pub widget: gtk::Box,
    steps: Vec<Step>,
    links: Vec<Link>,
    footer: gtk::Label,
    /// Which links are bouncing. Held here rather than handed to a timer, so a
    /// refresh can change the set without leaving an old timer behind.
    bouncing: RefCell<Vec<usize>>,
    ticking: Cell<bool>,
    /// Links being driven from one fraction to another, so a second animation
    /// on the same link replaces the first instead of fighting it.
    filling: RefCell<Vec<usize>>,
    /// How much room a stop's second line has, at the current window width.
    room: Rc<Cell<i32>>,
}

impl Steps {
    /// `stops` is the icon and label for each milestone, in order.
    pub fn new(stops: &[(&str, &str)]) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_2);
        widget.set_halign(gtk::Align::Center);
        widget.add_css_class("cz-steps");

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_halign(gtk::Align::Center);

        let room = Rc::new(Cell::new(NOTE_WIDTH[0]));
        let mut steps = Vec::new();
        let mut links = Vec::new();
        for (i, (icon_name, text)) in stops.iter().enumerate() {
            if i > 0 {
                // The time left sits above its own bar rather than in the
                // middle of the chain, so it is obvious which leg it is about.
                let note = gtk::Label::new(None);
                note.add_css_class("caption");
                note.add_css_class("cz-step-eta");
                note.set_visible(false);
                let bar = gtk::ProgressBar::builder()
                    .width_request(LINK_WIDTH[0])
                    .build();
                bar.add_css_class("cz-step-link");

                let leg = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_1);
                leg.set_valign(gtk::Align::Center);
                leg.set_margin_bottom(theme::SPACE_5);
                leg.append(&note);
                leg.append(&bar);
                row.append(&leg);
                links.push(Link { bar, note });
            }

            let column = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_1);
            column.set_valign(gtk::Align::Start);

            // The cross covers the icon rather than sitting on its corner: a
            // broken link should be findable without looking for it, and a
            // 14px badge is not that.
            let stack = gtk::Overlay::new();
            let icon = gtk::Image::from_icon_name(icon_name);
            icon.set_pixel_size(28);
            icon.add_css_class("cz-step-icon");
            stack.set_child(Some(&icon));
            let cross = cross_area();
            stack.add_overlay(&cross);
            column.append(&stack);

            let label = gtk::Label::new(Some(text));
            label.add_css_class("caption");
            label.set_wrap(true);
            label.set_justify(gtk::Justification::Center);
            label.set_max_width_chars(12);
            column.append(&label);

            // The second stop is the one that names the file, and the only one
            // whose second line is worth sliding to read in full.
            let note = Note::new(room.clone(), i == 1);
            column.append(&note.view);

            let button = gtk::Button::builder()
                .child(&column)
                .valign(gtk::Align::Start)
                .build();
            button.add_css_class("flat");
            button.add_css_class("cz-step-button");
            // Inert until something is attached. A stop that does nothing must
            // not light up under the pointer as though it would.
            button.set_can_target(false);
            button.set_can_focus(false);

            row.append(&button);
            steps.push(Step {
                button,
                icon,
                cross,
                label,
                note,
            });
        }

        // The chain is wider than a narrow window, and a fixed row inside a
        // window is a window that cannot be made narrow. Its natural width is
        // still asked for, so nothing is clipped until there is genuinely no
        // room - at which point clipping beats refusing to resize.
        let clip = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .propagate_natural_height(true)
            .child(&row)
            .build();
        widget.append(&clip);

        // What just finished, and that the chain is round again. The only line
        // that says a thing has been completed rather than that a thing is
        // true, so it sits under the chain rather than on it.
        let footer = gtk::Label::new(None);
        footer.add_css_class("caption");
        footer.add_css_class("cz-step-footer");
        footer.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        footer.set_max_width_chars(48);
        footer.set_visible(false);
        widget.append(&footer);

        Rc::new(Self {
            widget,
            steps,
            links,
            footer,
            bouncing: RefCell::new(Vec::new()),
            ticking: Cell::new(false),
            filling: RefCell::new(Vec::new()),
            room,
        })
    }

    /// Give up room as the window narrows.
    ///
    /// `level` is the window's width band, 0 being widest. The connectors go
    /// first because they carry no words; the second lines follow, and slide
    /// what no longer fits rather than hiding it.
    pub fn set_compact(&self, level: usize) {
        let level = level.min(LINK_WIDTH.len() - 1);
        for link in &self.links {
            link.bar.set_width_request(LINK_WIDTH[level]);
        }
        if self.room.replace(NOTE_WIDTH[level]) == NOTE_WIDTH[level] {
            return;
        }
        for step in &self.steps {
            step.note.view.set_width_request(NOTE_WIDTH[level]);
            // Re-measured against the new room: a name that fitted before may
            // now have to slide, and one that was sliding may now fit.
            let text = step.note.first.text();
            step.note.sliding.set(false);
            step.note.first.set_text("");
            step.note.set(Some(text.as_str()));
        }
    }

    /// Make a stop pressable, with `hint` as its tooltip.
    pub fn on_click<F: Fn() + 'static>(&self, at: usize, hint: &str, f: F) {
        let Some(step) = self.steps.get(at) else {
            return;
        };
        step.button.set_can_target(true);
        step.button.set_can_focus(true);
        step.button.set_tooltip_text(Some(hint));
        step.button.connect_clicked(move |_| f());
    }

    /// The widget to hang a popover off, for a stop whose action is a menu.
    pub fn anchor(&self, at: usize) -> Option<gtk::Widget> {
        self.steps.get(at).map(|s| s.button.clone().upcast())
    }

    /// Reword a pressable stop's tooltip as what it would do now.
    pub fn set_hint(&self, at: usize, hint: &str) {
        if let Some(step) = self.steps.get(at) {
            if step.button.can_target() {
                step.button.set_tooltip_text(Some(hint));
            }
        }
    }

    pub fn set_state(&self, at: usize, state: State) {
        let Some(step) = self.steps.get(at) else {
            return;
        };
        for class in CLASSES {
            step.icon.remove_css_class(class);
            step.label.remove_css_class(class);
        }
        step.icon.add_css_class(match state {
            State::Idle => "cz-step-idle",
            State::Live => "cz-step-live",
            State::Done => "cz-step-done",
            State::Landed => "cz-step-landed",
            State::Missing => "cz-step-missing",
        });
        let words = match state {
            State::Idle | State::Missing => "cz-step-idle",
            State::Live => "cz-step-live",
            State::Done => "cz-step-done",
            State::Landed => "cz-step-landed",
        };
        step.label.add_css_class(words);
        // The second line is part of the stop, not a caption beside it: a grey
        // stop with a white name under it reads as two different states.
        for l in &step.note.labels {
            for class in CLASSES {
                l.remove_css_class(class);
            }
            l.add_css_class(words);
        }
        step.cross.set_visible(state == State::Missing);
    }

    /// A second line under a milestone: which folder, which drive, which file.
    /// It slides if it is too long to fit.
    pub fn set_note(&self, at: usize, note: Option<&str>) {
        if let Some(step) = self.steps.get(at) {
            step.note.set(note);
        }
    }

    /// The line under the chain: what it just finished.
    pub fn set_footer(&self, text: Option<&str>) {
        match text.filter(|t| !t.is_empty()) {
            Some(t) => {
                self.footer.set_text(t);
                self.footer.set_visible(true);
            }
            None => {
                self.footer.set_text("");
                self.footer.set_visible(false);
            }
        }
    }

    /// How far across the link into a step something has got.
    ///
    /// Calling this stops the link bouncing: a known amount and an unknown one
    /// are two different answers and a bar cannot give both at once.
    pub fn set_link(&self, into: usize, fraction: f64) {
        let Some(link) = into.checked_sub(1).and_then(|i| self.links.get(i)) else {
            return;
        };
        self.bouncing.borrow_mut().retain(|i| *i != into);
        self.filling.borrow_mut().retain(|i| *i != into);
        link.bar.set_fraction(fraction.clamp(0.0, 1.0));
    }

    /// The time left, written above a link. `None` clears it.
    pub fn set_link_note(&self, into: usize, note: Option<&str>) {
        let Some(link) = into.checked_sub(1).and_then(|i| self.links.get(i)) else {
            return;
        };
        match note.filter(|t| !t.is_empty()) {
            Some(t) => {
                link.note.set_text(t);
                link.note.set_visible(true);
            }
            None => {
                link.note.set_text("");
                link.note.set_visible(false);
            }
        }
    }

    /// Run a link smoothly up to full, then call `then`.
    ///
    /// For the last leg, where the work is real but reports no progress. A bar
    /// that jumps from empty to full says the copy took no time; one left
    /// sitting part-full says it never finished. Neither is true, and this is
    /// the shape that is.
    pub fn fill_link<F: Fn() + 'static>(self: &Rc<Self>, into: usize, over: f64, then: F) {
        if into
            .checked_sub(1)
            .and_then(|i| self.links.get(i))
            .is_none()
        {
            return;
        }
        self.bouncing.borrow_mut().retain(|i| *i != into);
        self.filling.borrow_mut().push(into);
        let from = self
            .links
            .get(into - 1)
            .map(|l| l.bar.fraction())
            .unwrap_or(0.0);

        let steps = self.clone();
        let started = std::time::Instant::now();
        glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
            // Superseded: another call has taken this link over, or something
            // set it outright.
            if !steps.filling.borrow().contains(&into) {
                return glib::ControlFlow::Break;
            }
            let t = (started.elapsed().as_secs_f64() / over).clamp(0.0, 1.0);
            // Ease out, so it arrives rather than stopping dead.
            let eased = 1.0 - (1.0 - t) * (1.0 - t);
            let at = from + (1.0 - from) * eased;
            if let Some(link) = steps.links.get(into - 1) {
                link.bar.set_fraction(at);
            }
            if t >= 1.0 {
                steps.filling.borrow_mut().retain(|i| *i != into);
                then();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    /// Set exactly which links are bouncing, and start or stop the timer.
    ///
    /// One timer for the whole chain rather than one per link, and it stops
    /// the moment nothing is bouncing. Replacing the whole set each time is
    /// what keeps a link from being left moving after the thing it was waiting
    /// for has arrived.
    pub fn bounce(self: &Rc<Self>, which: Vec<usize>) {
        let filling = self.filling.borrow().clone();
        for (i, link) in self.links.iter().enumerate() {
            let at = i + 1;
            if !which.contains(&at)
                && self.bouncing.borrow().contains(&at)
                && !filling.contains(&at)
            {
                link.bar.set_fraction(0.0);
            }
        }
        *self.bouncing.borrow_mut() = which;
        if self.bouncing.borrow().is_empty() || self.ticking.replace(true) {
            return;
        }
        let steps = self.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(90), move || {
            let which = steps.bouncing.borrow().clone();
            if which.is_empty() {
                steps.ticking.set(false);
                return glib::ControlFlow::Break;
            }
            for into in which {
                if let Some(link) = into.checked_sub(1).and_then(|i| steps.links.get(i)) {
                    link.bar.pulse();
                }
            }
            glib::ControlFlow::Continue
        });
    }

    /// Stop everything moving, for when the chain is put away.
    pub fn rest(&self) {
        self.bouncing.borrow_mut().clear();
        self.filling.borrow_mut().clear();
        for link in &self.links {
            link.bar.set_fraction(0.0);
            link.note.set_visible(false);
        }
        for step in &self.steps {
            step.note.sliding.set(false);
        }
    }
}
