//! The window shell: a compact sidebar and a spacious workspace (§3, §4).
//!
//! Sections are pages in one stack rather than separate windows, so moving
//! between them keeps state and the application feels continuous (§24).

use crate::theme;
use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Long enough to read as the rail folding up, short enough not to be a wait.
const COLLAPSE_MS: u32 = 340;
/// Crossfade between pages: enough to read as motion, not a wait (§23).
const STACK_MS: u32 = 160;

/// Whether to log every step the rail takes, for `CHEAPAZSLA_DEBUG_FOLD`.
///
/// A scripted resize is far lighter than a hand on a window border, and the
/// faults that only show up under a real drag cannot be reproduced from here.
/// This lets the real gesture be recorded and read back.
fn logging() -> bool {
    thread_local! {
        static ON: bool = std::env::var_os("CHEAPAZSLA_DEBUG_FOLD").is_some();
    }
    ON.with(|on| *on)
}

/// How quickly the rail closes the distance to where it is heading: the gap
/// left shrinks by about two thirds every tenth of a second, which settles in
/// roughly the same time the fixed-duration version took.
const RAIL_TAU: f64 = 0.11;
/// The least the rail will move per second, so the last few pixels arrive
/// rather than creeping.
const RAIL_MIN_SPEED: f64 = 110.0;
/// The most of the rail's total travel any one frame may cover. At a healthy
/// frame rate the steps are smaller than this and it never applies; it is here
/// for the frames that arrive late.
const RAIL_MAX_STEP: f64 = 0.16;
/// Labels and rail share one duration and start together, so unfolding is
/// folding run backwards rather than its own arrangement. They used to differ,
/// which is what made the two directions look unlike each other.
/// How often to check whether the rail is ready to animate, and how long to
/// keep checking before setting the labels without a slide.
const POLL_MS: u64 = 10;
const GIVE_UP_MS: u64 = 400;
/// The width of the icon rail once the labels have gone.
///
/// Chosen so the icon is centred in it rather than by eye: the row puts the
/// icon 24px in, past the nav item's margin and padding and the selection
/// marker, so 24 + 16 + 24 leaves the same gap on both sides.
const RAIL_WIDTH: i32 = 64;

/// A section of the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Convert,
    Preview,
    History,
    Settings,
}

impl Section {
    pub const ALL: [Section; 4] = [
        Section::Convert,
        Section::Preview,
        Section::History,
        Section::Settings,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Section::Convert => "convert",
            Section::Preview => "preview",
            Section::History => "history",
            Section::Settings => "settings",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Section::Convert => "Convert",
            Section::Preview => "Preview",
            Section::History => "History",
            Section::Settings => "Settings",
        }
    }

    /// One icon family throughout: Adwaita's symbolic set (§5). Consistent
    /// stroke weight, crisp at any scale, and already themed.
    pub fn icon(self) -> &'static str {
        match self {
            Section::Convert => "media-playlist-repeat-symbolic",
            Section::Preview => "view-reveal-symbolic",
            Section::History => "document-open-recent-symbolic",
            Section::Settings => "emblem-system-symbolic",
        }
    }
}

/// One row in the sidebar.
struct NavItem {
    button: gtk::Button,
    section: Section,
}

/// Called when the visible section changes.
type SectionHandler = Box<dyn Fn(Section)>;

/// Sidebar plus the stack it drives.
pub struct Shell {
    pub widget: gtk::Box,
    pub stack: gtk::Stack,
    items: RefCell<Vec<NavItem>>,
    current: RefCell<Section>,
    on_change: RefCell<Option<SectionHandler>>,
    sidebar: gtk::ScrolledWindow,
    /// One per navigation label, plus the wordmark, so the rail folds up
    /// rather than snapping between two layouts.
    reveals: RefCell<Vec<gtk::Revealer>>,
    /// Whether the rail is still moving, so its tick is not started twice.
    /// A flag rather than the callback's id: the id could only be stored after
    /// the callback was registered, leaving a window in which a tick that had
    /// already finished would be recorded as still running — after which
    /// nothing would ever start it again and the rail would only ever jump.
    rail_moving: Rc<Cell<bool>>,
    /// Where the rail is heading, and where it has got to. Kept as a float so
    /// small per-frame steps are not lost to rounding.
    rail_target: Rc<Cell<f64>>,
    rail_width: Rc<Cell<f64>>,
    /// Whether the labels should be showing, so a reveal held back for a
    /// moment does not fire into a rail that has folded again meanwhile.
    want_labels: Rc<Cell<bool>>,
    /// Whether to animate at all. Off, everything still happens — it just
    /// happens at once.
    animate: Rc<Cell<bool>>,
    /// The About button at the foot of the rail.
    about: RefCell<Option<gtk::Button>>,
    compact: Cell<bool>,
}

impl Shell {
    pub fn new() -> Rc<Self> {
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(STACK_MS) // §23: motion, not a wait
            .hexpand(true)
            .vexpand(true)
            .build();
        stack.add_css_class("cz-workspace");

        // The rail is a clip, not a box that resizes.
        //
        // A GtkBox hands spare space to each child up to that child's natural
        // width, and a revealer reports its child's full natural width from
        // the instant it is told to reveal — before its transition has moved
        // at all. So the rail jumped by the width of a label the moment the
        // labels were told to appear, and no amount of ordering fixed it:
        // delaying the labels only moved the jump later, which is the stagger
        // it was trying to avoid.
        //
        // The contents are instead laid out at full width always, inside
        // something whose own width says nothing about them. A scrolled window
        // asked not to propagate its child's natural width is exactly that: it
        // reports what it is told and clips the rest, so the width animation
        // is the only thing deciding how wide the rail is.
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_size_request(theme::SIDEBAR_WIDTH, -1);
        let sidebar = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_width(false)
            .child(&content)
            .build();
        // A scrolled window scrolls, and this one is only ever meant to clip.
        // Left to itself it scrolled sideways to keep the focused navigation
        // button in view as the rail narrowed, then unwound that as the rail
        // opened again — which is the whole rail's contents sliding left and
        // rubber-banding back, icons and all, on every expand.
        sidebar.set_kinetic_scrolling(false);
        let hadj = sidebar.hadjustment();
        hadj.connect_value_changed(|a| {
            if a.value() != 0.0 {
                a.set_value(0.0);
            }
        });
        // On the clip rather than the contents, so the rail's right-hand
        // border stays at the rail's edge instead of being clipped away.
        sidebar.add_css_class("cz-sidebar");
        sidebar.set_size_request(theme::SIDEBAR_WIDTH, -1);

        let brand = gtk::Box::new(gtk::Orientation::Vertical, 0);
        brand.set_margin_top(theme::SPACE_4);
        brand.set_margin_bottom(theme::SPACE_4);
        brand.set_margin_start(theme::SPACE_4);
        brand.set_margin_end(theme::SPACE_4);
        // Ellipsized rather than wrapped, so an overlong title would cut
        // rather than push the rail's layout around.
        let name = gtk::Label::builder()
            .label("CheapAzSLA")
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(1)
            .build();
        name.add_css_class("heading");
        // Wrapped rather than ellipsized: this one is a phrase, and half of a
        // phrase followed by an ellipsis says less than the same phrase over
        // two lines. GTK only takes the second line if it needs it.
        let tag = gtk::Label::builder()
            .label("Convert and inspect")
            .xalign(0.0)
            .wrap(true)
            .build();
        tag.add_css_class("caption");
        tag.add_css_class("cz-dim");
        brand.append(&name);
        brand.append(&tag);
        // Straight up and out of the way. This used to be two revealers, one
        // per axis, because a vertical one still reported the wordmark's full
        // width and that width held the rail open; now that the rail is a clip
        // its contents' width is nobody's business but their own, and the
        // wordmark can just leave the way it reads best.
        let brand_reveal = gtk::Revealer::builder()
            .child(&brand)
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(COLLAPSE_MS)
            .reveal_child(true)
            .build();
        content.append(&brand_reveal);

        let shell = Rc::new(Self {
            widget: gtk::Box::new(gtk::Orientation::Horizontal, 0),
            stack: stack.clone(),
            items: RefCell::new(Vec::new()),
            current: RefCell::new(Section::Convert),
            on_change: RefCell::new(None),
            sidebar: sidebar.clone(),
            reveals: RefCell::new(vec![brand_reveal]),
            rail_moving: Rc::new(Cell::new(false)),
            rail_target: Rc::new(Cell::new(theme::SIDEBAR_WIDTH as f64)),
            rail_width: Rc::new(Cell::new(theme::SIDEBAR_WIDTH as f64)),
            want_labels: Rc::new(Cell::new(true)),
            animate: Rc::new(Cell::new(true)),
            about: RefCell::new(None),
            compact: Cell::new(false),
        });

        for section in Section::ALL {
            let button = shell.rail_button(section.icon(), section.label());
            let sh = shell.clone();
            button.connect_clicked(move |_| sh.show(section));
            content.append(&button);
            shell.items.borrow_mut().push(NavItem { button, section });
        }

        // The guide sits at the foot, away from navigation (§4).
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_vexpand(true);
        content.append(&spacer);
        let about = shell.rail_button("help-about-symbolic", "About");
        about.set_margin_bottom(theme::SPACE_2);
        content.append(&about);
        *shell.about.borrow_mut() = Some(about);

        shell.widget.append(&sidebar);
        shell.widget.append(&stack);
        shell.select_visual(Section::Convert);
        shell
    }

    /// A row in the rail: marker, icon, and a label that slides away when the
    /// rail folds. Used for the sections and for the About button, so the one
    /// at the foot folds exactly like the ones above it.
    fn rail_button(self: &Rc<Self>, icon: &str, label: &str) -> gtk::Button {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
        // A 3px bar marks the active section, so selection is not carried by
        // colour alone (§15, §35). The About row keeps a transparent one so
        // its icon lines up with the others.
        let marker = gtk::Box::new(gtk::Orientation::Vertical, 0);
        marker.add_css_class("cz-nav-marker");
        marker.set_size_request(3, 18);
        marker.set_valign(gtk::Align::Center);
        row.append(&marker);
        row.append(&gtk::Image::from_icon_name(icon));
        // Ellipsized down to a single character, so the label's own minimum
        // can never hold the rail wider than the width animation has reached.
        let text = gtk::Label::builder()
            .label(label)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(1)
            .build();
        let reveal = gtk::Revealer::builder()
            .child(&text)
            .transition_type(gtk::RevealerTransitionType::SlideRight)
            .transition_duration(COLLAPSE_MS)
            .reveal_child(true)
            .build();
        row.append(&reveal);
        self.reveals.borrow_mut().push(reveal);

        let button = gtk::Button::builder().child(&row).build();
        button.add_css_class("flat");
        button.add_css_class("cz-nav-item");
        button.set_tooltip_text(Some(label));
        button
    }

    /// Called when the About button at the foot of the rail is pressed.
    pub fn connect_about(&self, f: impl Fn() + 'static) {
        if let Some(button) = self.about.borrow().as_ref() {
            button.connect_clicked(move |_| f());
        }
    }

    /// Add a page. Called once per section during construction.
    pub fn add_page(&self, section: Section, child: &impl IsA<gtk::Widget>) {
        self.stack.add_named(child, Some(section.id()));
    }

    /// Move to a section.
    pub fn show(self: &Rc<Self>, section: Section) {
        if *self.current.borrow() == section {
            return;
        }
        *self.current.borrow_mut() = section;
        self.stack.set_visible_child_name(section.id());
        self.select_visual(section);
        if let Some(f) = self.on_change.borrow().as_ref() {
            f(section);
        }
    }

    pub fn current(&self) -> Section {
        *self.current.borrow()
    }

    /// Keep the icons, drop the labels. At narrow widths the sidebar's text is
    /// most of its width and the icons carry the meaning on their own.
    ///
    /// Animated rather than switched: the labels slide away in revealers and
    /// the rail's width is driven down alongside them, so narrowing the window
    /// folds the sidebar up instead of making it jump between two layouts.
    pub fn set_compact(&self, compact: bool) {
        if self.compact.replace(compact) == compact {
            return;
        }
        // The labels and the rail's width are two mechanisms moving the same
        // edge, and whichever finishes second decides what the fold looks
        // like. Folding, the labels have to be out of the way before the rail
        // can narrow past them, so they go first. Unfolding, the rail has to
        // open before there is anywhere for a label to appear, so they trail.
        // Equal durations put them in each other's way in both directions.
        self.want_labels.set(!compact);
        self.slide_labels(!compact);

        self.aim(compact);
    }

    /// Slide the labels in or out, once the rail is in a state to show it.
    ///
    /// A revealer told to change while it is unmapped skips its transition and
    /// jumps, and at the moment a breakpoint fires during a window resize the
    /// sidebar is briefly unmapped — the same thing that was stopping the
    /// width animating. So this waits for the rail to be mapped rather than
    /// acting immediately, and gives up and sets the state anyway if that
    /// never comes, since a label in the wrong state is worse than one that
    /// arrived without sliding.
    ///
    /// A revealer interpolates from wherever it currently is, so reversing
    /// part way through needs nothing special here.
    fn slide_labels(&self, revealed: bool) {
        let reveals: Vec<gtk::Revealer> = self.reveals.borrow().clone();
        let want = self.want_labels.clone();
        let sidebar = self.sidebar.clone();
        let animate = self.animate.clone();
        let mut waited = 0u64;
        glib::timeout_add_local(std::time::Duration::from_millis(POLL_MS), move || {
            waited += POLL_MS;
            // The window went the other way again while we were waiting.
            if want.get() != revealed {
                return glib::ControlFlow::Break;
            }
            if !sidebar.is_mapped() && waited < GIVE_UP_MS {
                return glib::ControlFlow::Continue;
            }
            if logging() {
                eprintln!(
                    "  labels -> {revealed} after {waited}ms{}",
                    if sidebar.is_mapped() {
                        ""
                    } else {
                        " (GAVE UP, will snap)"
                    }
                );
            }
            for reveal in &reveals {
                reveal.set_transition_duration(if animate.get() { COLLAPSE_MS } else { 0 });
                reveal.set_reveal_child(revealed);
            }
            glib::ControlFlow::Break
        });
    }

    /// Turn the interface's movement on or off.
    ///
    /// Everything still reaches the same state; without animation it simply
    /// arrives there in one step.
    pub fn set_animate(&self, on: bool) {
        self.animate.set(on);
        self.stack
            .set_transition_duration(if on { STACK_MS } else { 0 });
        for reveal in self.reveals.borrow().iter() {
            reveal.set_transition_duration(if on { COLLAPSE_MS } else { 0 });
        }
    }

    /// Point the rail at its folded or open width, without touching anything
    /// else.
    ///
    /// The rest of the work a step involves is deferred to an idle, because
    /// changing size requests while a breakpoint is being evaluated makes the
    /// breakpoints oscillate. This part changes no widget — it sets a number
    /// the next frame will read — so it is safe to do immediately, and doing
    /// it immediately is what stops a fast drag getting several frames ahead
    /// of the fold.
    pub fn aim(&self, compact: bool) {
        if logging() {
            eprintln!(
                "aim compact={compact} window={} rail={} moving={}",
                self.sidebar.root().map(|r| r.width()).unwrap_or(-1),
                self.rail_width.get().round(),
                self.rail_moving.get(),
            );
        }
        self.set_rail_target(if compact {
            RAIL_WIDTH as f64
        } else {
            theme::SIDEBAR_WIDTH as f64
        });
    }

    /// Move the rail toward a width, and keep moving toward whatever the
    /// width becomes.
    ///
    /// This was an animation from a start to an end over a fixed duration,
    /// restarted whenever the target changed. Dragging an edge back and forth
    /// across the step, that restarts the curve from a standstill several
    /// times a second: the rail carried on the old way for a moment after the
    /// window turned around, never reached either end, and hovered somewhere
    /// in the middle — measured, it oscillated between 69 and 128 without ever
    /// touching 63 or 151.
    ///
    /// A target the rail chases instead has no start and nothing to restart.
    /// Changing direction is a new target, and the next frame already moves
    /// the other way.
    fn set_rail_target(&self, to: f64) {
        self.rail_target.set(to);
        if !self.animate.get() {
            self.rail_width.set(to);
            self.sidebar.set_size_request(to as i32, -1);
            return;
        }
        if self.rail_moving.replace(true) {
            return;
        }
        let Some(root) = self.sidebar.root() else {
            if logging() {
                eprintln!("  no window yet, snapping to {to}");
            }
            self.rail_moving.set(false);
            self.rail_width.set(to);
            self.sidebar.set_size_request(to as i32, -1);
            return;
        };
        let root: gtk::Widget = root.upcast();
        let sidebar = self.sidebar.clone();
        let target = self.rail_target.clone();
        let width = self.rail_width.clone();
        let moving = self.rail_moving.clone();
        let last = Cell::new(None::<i64>);
        root.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            // A long gap means the clock was not running, not that a long
            // step is owed; clamp it so returning to the window does not
            // teleport the rail.
            let dt = match last.replace(Some(now)) {
                Some(previous) => ((now - previous) as f64 / 1e6).clamp(0.0, 0.1),
                None => 0.0,
            };

            let want = target.get();
            let mut next = width.get();
            let travel = (theme::SIDEBAR_WIDTH - RAIL_WIDTH) as f64;
            let remaining = want - next;

            // Exponential approach: the distance left shrinks by the same
            // proportion every second, whichever way it is going.
            let mut step = remaining * (1.0 - (-dt / RAIL_TAU).exp());

            // A floor, because an exponential never quite arrives on its own:
            // the last dozen pixels crawled, and the rail sat a little short
            // of folded for as long again as the fold had taken.
            let floor = (RAIL_MIN_SPEED * dt).min(travel * RAIL_MAX_STEP);
            if step.abs() < floor {
                step = floor * remaining.signum();
            }

            // And a ceiling, which is what keeps this from being a jump when
            // the frame clock is starved. The step is worked out from elapsed
            // time, so one late frame during a heavy drag would otherwise move
            // the rail most of the way in a single visible increment. Capped,
            // a stretch of slow frames makes the fold take longer rather than
            // making it teleport.
            let ceiling = travel * RAIL_MAX_STEP;
            step = step.clamp(-ceiling, ceiling);

            if logging() {
                eprintln!(
                    "  tick dt={:.0}ms want={want:.0} at={next:.0} step={step:.1} \
                     drawn={} win={}",
                    dt * 1000.0,
                    sidebar.width(),
                    sidebar.root().map(|r| r.width()).unwrap_or(-1),
                );
            }
            if step.abs() >= remaining.abs() || remaining.abs() < 0.5 {
                next = want;
            } else {
                next += step;
            }
            width.set(next);

            // Deliberately not clamped to what the window has room for. That
            // was here to stop a fast drag outrunning the fold, and it did,
            // by dragging the rail straight to its folded width the moment
            // the window got small — a teleport rather than a fold. The fold
            // takes the time it takes whatever the window does; being briefly
            // wider than the window is worth far less than the animation is.
            sidebar.set_size_request(next.round() as i32, -1);

            if next == want {
                if logging() {
                    eprintln!("  tick arrived at {want:.0}");
                }
                moving.set(false);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    fn select_visual(&self, section: Section) {
        for item in self.items.borrow().iter() {
            if item.section == section {
                item.button.add_css_class("selected");
            } else {
                item.button.remove_css_class("selected");
            }
        }
    }
}

/// A section heading: small, uppercase, muted (§34).
pub fn section_label(text: &str) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).build();
    l.add_css_class("cz-section");
    l
}

/// A label and value on one line, values aligned down the column (§20).
pub fn info_row(label: &str, value: &str, dim: bool) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
    let l = gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        // Wide enough to align the values, but allowed to give way: a hard
        // width here sets a floor under every panel that uses these rows,
        // and therefore under the window itself.
        .width_chars(14)
        .max_width_chars(14)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    l.add_css_class("cz-dim");
    let v = gtk::Label::builder()
        .label(value)
        .xalign(0.0)
        .hexpand(true)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    v.add_css_class("cz-value");
    if dim {
        v.add_css_class("cz-dim");
    }
    row.append(&l);
    row.append(&v);
    row
}

/// An icon-only button with the tooltip and accessible label §6 requires.
pub fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::builder().icon_name(icon).build();
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tooltip));
    b.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    b
}

/// Apply a tooltip to a widget and everything inside it.
///
/// GTK resolves a tooltip against the widget under the pointer. A container
/// whose children have none does not reliably answer for them, so hovering the
/// icon in a status chip showed nothing while hovering the gap showed the
/// text. Setting it throughout removes the dead spots.
pub fn set_tooltip_deep(widget: &impl IsA<gtk::Widget>, text: &str) {
    let w = widget.as_ref();
    w.set_tooltip_text(Some(text));
    let mut child = w.first_child();
    while let Some(c) = child {
        set_tooltip_deep(&c, text);
        child = c.next_sibling();
    }
}

/// Status shown as an icon and a word, never colour alone (§15).
pub fn status_chip(icon: &str, text: &str, class: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_1);
    let i = gtk::Image::from_icon_name(icon);
    i.add_css_class(class);
    let l = gtk::Label::new(Some(text));
    l.add_css_class(class);
    l.add_css_class("caption");
    b.append(&i);
    b.append(&l);
    b
}
