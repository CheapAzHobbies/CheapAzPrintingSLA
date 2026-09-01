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
/// Labels and rail move together, so the fold reads as one movement.
const LABELS_OUT_MS: u32 = COLLAPSE_MS;
const LABELS_IN_MS: u32 = COLLAPSE_MS;
const LABELS_IN_DELAY_MS: u64 = 40;
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
    /// Held so a second fold mid-animation replaces the first rather than
    /// fighting it.
    width_tick: RefCell<Option<gtk::TickCallbackId>>,
    /// Whether the labels should be showing, so a reveal held back for a
    /// moment does not fire into a rail that has folded again meanwhile.
    want_labels: Rc<Cell<bool>>,
    compact: Cell<bool>,
}

impl Shell {
    pub fn new() -> Rc<Self> {
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(160) // §23: enough to read as motion, not a wait
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
        let tag = gtk::Label::builder()
            .label("Resin print files")
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .width_chars(1)
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
            width_tick: RefCell::new(None),
            want_labels: Rc::new(Cell::new(true)),
            compact: Cell::new(false),
        });

        for section in Section::ALL {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
            // A 3px bar marks the active section, so selection is not carried
            // by colour alone (§15, §35).
            let marker = gtk::Box::new(gtk::Orientation::Vertical, 0);
            marker.add_css_class("cz-nav-marker");
            marker.set_size_request(3, 18);
            marker.set_valign(gtk::Align::Center);
            row.append(&marker);
            row.append(&gtk::Image::from_icon_name(section.icon()));
            // Ellipsized down to a single character, so the label's own
            // minimum can never hold the rail wider than the width animation
            // has reached. Without that a revealer's first frame, which
            // allocates its child at full size, pinned the rail open and the
            // labels had to be held back to hide it — which is what made the
            // two move at different times.
            let label = gtk::Label::builder()
                .label(section.label())
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .width_chars(1)
                .build();
            let reveal = gtk::Revealer::builder()
                .child(&label)
                .transition_type(gtk::RevealerTransitionType::SlideRight)
                .transition_duration(COLLAPSE_MS)
                .reveal_child(true)
                .build();
            row.append(&reveal);
            shell.reveals.borrow_mut().push(reveal);

            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("flat");
            button.add_css_class("cz-nav-item");
            button.set_tooltip_text(Some(section.label()));
            let sh = shell.clone();
            button.connect_clicked(move |_| sh.show(section));
            content.append(&button);
            shell.items.borrow_mut().push(NavItem { button, section });
        }

        // About sits at the foot, away from navigation (§4).
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_vexpand(true);
        content.append(&spacer);

        shell.widget.append(&sidebar);
        shell.widget.append(&stack);
        shell.select_visual(Section::Convert);
        shell
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

        // From wherever it is now, not from the nominal width, so a toggle
        // part way through the previous one carries on from there.
        let from = self.sidebar.width_request().max(RAIL_WIDTH) as f64;
        let to = if compact {
            RAIL_WIDTH
        } else {
            theme::SIDEBAR_WIDTH
        } as f64;
        self.animate_width(from, to);
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
    /// Revealing also waits out a short delay on top: a revealer allocates its
    /// child at full size for a frame or two before its transition takes over,
    /// and from a folded rail that flash is most of the way open. By the time
    /// it fires the rail is wider than the flash, so there is nothing to see.
    fn slide_labels(&self, revealed: bool) {
        let reveals: Vec<gtk::Revealer> = self.reveals.borrow().clone();
        let want = self.want_labels.clone();
        let sidebar = self.sidebar.clone();
        let (delay, duration) = if revealed {
            (LABELS_IN_DELAY_MS, LABELS_IN_MS)
        } else {
            (0, LABELS_OUT_MS)
        };
        let mut waited = 0u64;
        glib::timeout_add_local(std::time::Duration::from_millis(POLL_MS), move || {
            waited += POLL_MS;
            // The window went the other way again while we were waiting.
            if want.get() != revealed {
                return glib::ControlFlow::Break;
            }
            if waited < delay || (!sidebar.is_mapped() && waited < GIVE_UP_MS) {
                return glib::ControlFlow::Continue;
            }
            for reveal in &reveals {
                reveal.set_transition_duration(duration);
                reveal.set_reveal_child(revealed);
            }
            glib::ControlFlow::Break
        });
    }

    /// Drive the rail's width from the window's frame clock.
    ///
    /// This was an `AdwTimedAnimation` and looked right in isolation, but
    /// libadwaita skips an animation whose widget is not mapped, and at the
    /// moment a breakpoint fires during a window resize the sidebar reports
    /// itself unmapped — realized, 207px wide, its window mapped, and still
    /// unmapped. So the fold animated when it was triggered on its own and
    /// jumped when it was triggered by dragging the window, which is the only
    /// way anyone actually triggers it.
    ///
    /// The window is mapped throughout, so its clock is what this runs on.
    fn animate_width(&self, from: f64, to: f64) {
        if let Some(id) = self.width_tick.borrow_mut().take() {
            id.remove();
        }
        let Some(root) = self.sidebar.root() else {
            self.sidebar.set_size_request(to as i32, -1);
            return;
        };
        if (to - from).abs() < 1.0 {
            self.sidebar.set_size_request(to as i32, -1);
            return;
        }
        let root: gtk::Widget = root.upcast();
        let sidebar = self.sidebar.clone();
        let began = Cell::new(None::<i64>);
        let span = (COLLAPSE_MS as i64) * 1000;
        let id = root.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            let start = match began.get() {
                Some(t) => t,
                None => {
                    began.set(Some(now));
                    now
                }
            };
            let t = ((now - start) as f64 / span as f64).clamp(0.0, 1.0);
            // Ease out cubic: quick to leave, gentle to arrive.
            let eased = 1.0 - (1.0 - t).powi(3);
            sidebar.set_size_request((from + (to - from) * eased).round() as i32, -1);
            if t >= 1.0 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        *self.width_tick.borrow_mut() = Some(id);
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
