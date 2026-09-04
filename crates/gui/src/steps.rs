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
    /// Breathing between grey and white: looking, or working. The chain is
    /// getting on with something and nobody need do anything.
    Live,
    /// Breathing in the accent colour: this stop is waiting on the user, and
    /// nothing at all happens until it is answered. Deliberately a different
    /// colour from `Live` - two stops pulsing identically for two different
    /// reasons is worse than neither pulsing, because it reads as one thing
    /// happening in two places.
    Calling,
    /// Solid white. Behind us.
    Done,
    /// Solid green. The end of the chain, actually reached.
    Landed,
    /// Dull, under a red cross. What this stop needs is not there.
    Missing,
}

const CLASSES: [&str; 6] = [
    "cz-step-idle",
    "cz-step-live",
    "cz-step-calling",
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
/// How wide the line under the chain may run before it wraps, at each width.
const FOOTER_CHARS: [i32; 3] = [52, 40, 30];
const NOTE_SPEED: f64 = 34.0;
const NOTE_GAP: i32 = 28;

struct Step {
    button: gtk::Button,
    /// Everything the stop is made of, so one opacity drives the lot.
    column: gtk::Box,
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
    /// Whether second lines are shown at all at the current window width.
    /// Shared with the chain, like `room`.
    allowed: Rc<Cell<bool>>,
    /// What this line would say if it were being shown.
    text: RefCell<String>,
}

impl Note {
    fn new(room: Rc<Cell<i32>>, allowed: Rc<Cell<bool>>, slides: bool) -> Self {
        let first = gtk::Label::builder().label("").build();
        let second = gtk::Label::builder().label("").build();
        // Cut short where the line does not slide, and left whole where it
        // does. An ellipsised label inside the marquee gets squeezed to share
        // the view with its second copy, and two four-character stubs is not a
        // name being read - it is a name being destroyed twice. The column
        // cannot grow either way: the scrolled window's content width is
        // pinned at both ends below.
        if !slides {
            for l in [&first, &second] {
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                l.set_width_chars(0);
            }
        }
        for l in [&first, &second] {
            l.add_css_class("caption");
        }
        second.set_visible(false);

        let train = gtk::Box::new(gtk::Orientation::Horizontal, NOTE_GAP);
        train.append(&first);
        train.append(&second);
        train.set_halign(gtk::Align::Center);

        // Both bounds, not just the minimum. A width request is a floor, so
        // with only that a longer name makes its column wider and the whole
        // chain spreads out the moment a file is found - which is movement
        // that says nothing, in a row whose job is to hold still while its
        // states change. max_content_width is the ceiling that stops it.
        let view = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(false)
            .min_content_width(room.get())
            .max_content_width(room.get())
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
            allowed,
            text: RefCell::new(String::new()),
        }
    }

    fn set(&self, text: Option<&str>) {
        *self.text.borrow_mut() = text.unwrap_or("").to_string();
        self.apply();
    }

    /// Put the line on screen, or not, according to what it says and whether
    /// there is room for it at this window width.
    fn apply(&self) {
        let text = self.text.borrow().clone();
        // Shown whenever there is room for it, even with nothing to say. A
        // line that disappears when it empties takes its column's width with
        // it, and the chain shuffles sideways every time a stop gains or loses
        // a caption - which is the row moving to report that nothing moved.
        if !self.allowed.get() {
            self.sliding.set(false);
            self.view.set_visible(false);
            return;
        }
        self.view.set_visible(true);
        if text.is_empty() {
            self.sliding.set(false);
            self.first.set_text("");
            self.second.set_text("");
            self.second.set_visible(false);
            return;
        }
        let text = text.as_str();
        if self.first.text() == text {
            return;
        }
        self.sliding.set(false);
        self.first.set_text(text);
        self.second.set_text(text);
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
    /// Whether second lines are shown at all at this width.
    notes_allowed: Rc<Cell<bool>>,
    /// The width band currently in force, so a repeat is not re-animated.
    level: Cell<usize>,
    /// Where each connector's width is heading, so a glide already under way
    /// is not fought by a second one.
    gliding: Cell<bool>,
    /// Which stops are breathing. They are driven together from one clock
    /// rather than each from its own CSS animation, because animations start
    /// when their class is applied and stops light up at different moments -
    /// which leaves four things pulsing out of phase, and a row of lights
    /// blinking independently reads as decoration rather than as one state.
    breathing: RefCell<Vec<usize>>,
    beating: Cell<bool>,
}

impl Steps {
    /// `stops` is the icon and label for each milestone, in order.
    pub fn new(stops: &[(&str, &str)]) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_2);
        widget.set_halign(gtk::Align::Center);
        widget.set_margin_bottom(theme::SPACE_3);
        widget.add_css_class("cz-steps");

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.set_halign(gtk::Align::Center);

        let room = Rc::new(Cell::new(NOTE_WIDTH[0]));
        let notes_allowed = Rc::new(Cell::new(true));
        let mut steps = Vec::new();
        let mut links = Vec::new();
        for (i, (icon_name, text)) in stops.iter().enumerate() {
            if i > 0 {
                // The time left sits above its own bar rather than in the
                // middle of the chain, so it is obvious which leg it is about.
                // Always present, even with nothing to say. Showing and
                // hiding it would make the connectors jump down the moment a
                // file was found, which is exactly when the chain most needs
                // to look like it is holding still.
                let note = gtk::Label::new(None);
                note.add_css_class("caption");
                note.add_css_class("cz-step-eta");
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
            let note = Note::new(room.clone(), notes_allowed.clone(), i == 1);
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
                column,
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
        // Wrapped rather than ellipsised: this is a whole sentence, and it is
        // the line carrying the detail once the stops have given theirs up at
        // narrow widths. Losing the end of it there is losing the only place
        // the reason is written.
        let footer = gtk::Label::new(None);
        footer.add_css_class("caption");
        footer.add_css_class("cz-step-footer");
        footer.set_wrap(true);
        footer.set_justify(gtk::Justification::Center);
        footer.set_max_width_chars(FOOTER_CHARS[0]);
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
            notes_allowed,
            level: Cell::new(0),
            gliding: Cell::new(false),
            breathing: RefCell::new(Vec::new()),
            beating: Cell::new(false),
        })
    }

    /// Give up room as the window narrows.
    ///
    /// `level` is the window's width band, 0 being widest. The connectors go
    /// first, because they carry no words. At the narrowest the second lines
    /// go too: which folder and which drive are details, and the line under
    /// the chain is already saying the one that matters. What has to survive
    /// every width is the thing the chain is for - four icons, their state,
    /// and which link is broken.
    ///
    /// The change is glided rather than snapped. A window being dragged
    /// narrower is a continuous gesture, and a layout that jumps part-way
    /// through reads as something breaking rather than something fitting.
    pub fn set_compact(self: &Rc<Self>, level: usize) {
        let level = level.min(LINK_WIDTH.len() - 1);
        if self.level.replace(level) == level {
            return;
        }
        self.glide_links(LINK_WIDTH[level]);

        self.footer.set_max_width_chars(FOOTER_CHARS[level]);
        self.room.set(NOTE_WIDTH[level]);
        self.notes_allowed.set(level < LINK_WIDTH.len() - 1);
        for step in &self.steps {
            step.note.view.set_width_request(NOTE_WIDTH[level]);
            step.note.view.set_min_content_width(NOTE_WIDTH[level]);
            step.note.view.set_max_content_width(NOTE_WIDTH[level]);
            // Re-measured against the new room: a name that fitted before may
            // now have to slide, and one that was sliding may now fit.
            step.note.sliding.set(false);
            step.note.first.set_text("");
            step.note.apply();
        }
    }

    /// Walk the connectors to a new length over a couple of frames.
    fn glide_links(self: &Rc<Self>, to: i32) {
        let from = self
            .links
            .first()
            .map(|l| l.bar.width_request())
            .unwrap_or(to);
        if from == to || self.gliding.replace(true) {
            // Already moving: the running glide reads the target each frame,
            // so it will arrive at the new one without a second timer.
            if from == to {
                self.gliding.set(false);
            }
            return;
        }
        let steps = self.clone();
        let started = std::time::Instant::now();
        const OVER: f64 = 0.18;
        glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
            let target = LINK_WIDTH[steps.level.get()];
            let t = (started.elapsed().as_secs_f64() / OVER).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - t) * (1.0 - t);
            let at = from as f64 + (target - from) as f64 * eased;
            for link in &steps.links {
                link.bar.set_width_request(at.round() as i32);
            }
            if t >= 1.0 {
                for link in &steps.links {
                    link.bar.set_width_request(target);
                }
                steps.gliding.set(false);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
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

    /// Change what a stop is a picture of.
    ///
    /// The last stop is whatever the output was pointed at, and a folder and a
    /// drive are not the same thing: which it is decides whether "not there"
    /// means unplugged or deleted.
    pub fn set_icon(&self, at: usize, icon: &str) {
        if let Some(step) = self.steps.get(at) {
            step.icon.set_icon_name(Some(icon));
        }
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

    pub fn set_state(self: &Rc<Self>, at: usize, state: State) {
        let Some(step) = self.steps.get(at) else {
            return;
        };
        // Which stops are alive, so the shared beat knows what to drive. Held
        // still at a fixed opacity when they are not.
        {
            let mut breathing = self.breathing.borrow_mut();
            breathing.retain(|i| *i != at);
            if matches!(state, State::Live | State::Calling) {
                breathing.push(at);
            }
        }
        step.column.set_opacity(match state {
            State::Idle => 0.5,
            State::Missing => 0.55,
            State::Done | State::Landed => 1.0,
            // Left to the beat, which is about to set it.
            State::Live | State::Calling => step.column.opacity(),
        });
        for class in CLASSES {
            step.icon.remove_css_class(class);
            step.label.remove_css_class(class);
        }
        step.icon.add_css_class(match state {
            State::Idle => "cz-step-idle",
            State::Live => "cz-step-live",
            State::Calling => "cz-step-calling",
            State::Done => "cz-step-done",
            State::Landed => "cz-step-landed",
            State::Missing => "cz-step-missing",
        });
        let words = match state {
            State::Idle | State::Missing => "cz-step-idle",
            State::Live => "cz-step-live",
            State::Calling => "cz-step-calling",
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
        self.beat();
    }

    /// One clock for every breathing stop, so they rise and fall together.
    ///
    /// Stops the moment nothing is breathing, and picks up again from the
    /// phase it left off, so a stop joining an existing beat falls into step
    /// with it rather than starting its own.
    fn beat(self: &Rc<Self>) {
        if self.breathing.borrow().is_empty() || self.beating.replace(true) {
            return;
        }
        let steps = self.clone();
        let started = std::time::Instant::now();
        const PERIOD: f64 = 1.4;
        glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
            let which = steps.breathing.borrow().clone();
            if which.is_empty() {
                steps.beating.set(false);
                for step in &steps.steps {
                    step.column.set_opacity(1.0);
                }
                return glib::ControlFlow::Break;
            }
            let phase = (started.elapsed().as_secs_f64() / PERIOD).fract();
            // A cosine rather than a triangle: it lingers at the ends, which
            // is what makes it read as breathing rather than as blinking.
            let swell = 0.5 - 0.5 * (phase * std::f64::consts::TAU).cos();
            let opacity = 0.35 + 0.65 * swell;
            for i in which {
                if let Some(step) = steps.steps.get(i) {
                    step.column.set_opacity(opacity);
                }
            }
            glib::ControlFlow::Continue
        });
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
        // Emptied rather than hidden, so the row keeps its height either way.
        link.note.set_text(note.unwrap_or(""));
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
        self.breathing.borrow_mut().clear();
        self.bouncing.borrow_mut().clear();
        self.filling.borrow_mut().clear();
        for link in &self.links {
            link.bar.set_fraction(0.0);
            link.note.set_text("");
        }
        for step in &self.steps {
            step.note.sliding.set(false);
        }
    }
}
