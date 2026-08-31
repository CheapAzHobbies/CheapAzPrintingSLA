//! The window shell: a compact sidebar and a spacious workspace (§3, §4).
//!
//! Sections are pages in one stack rather than separate windows, so moving
//! between them keeps state and the application feels continuous (§24).

use crate::theme;
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Long enough to read as the rail folding up, short enough not to be a wait.
const COLLAPSE_MS: u32 = 220;
/// The width of the icon rail once the labels have gone.
const RAIL_WIDTH: i32 = 56;

/// Drop a revealer out of the layout once it has finished folding away.
fn hide_when_folded(reveal: &gtk::Revealer) {
    reveal.connect_child_revealed_notify(|r| {
        if !r.is_child_revealed() && !r.reveals_child() {
            r.set_visible(false);
        }
    });
}

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
    sidebar: gtk::Box,
    /// One per navigation label, plus the wordmark, so the rail folds up
    /// rather than snapping between two layouts.
    reveals: RefCell<Vec<gtk::Revealer>>,
    /// Held so a second toggle mid-animation replaces the first rather than
    /// fighting it.
    width_anim: RefCell<Option<adw::TimedAnimation>>,
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

        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.add_css_class("cz-sidebar");
        sidebar.set_size_request(theme::SIDEBAR_WIDTH, -1);

        let brand = gtk::Box::new(gtk::Orientation::Vertical, 0);
        brand.set_margin_top(theme::SPACE_4);
        brand.set_margin_bottom(theme::SPACE_4);
        brand.set_margin_start(theme::SPACE_4);
        brand.set_margin_end(theme::SPACE_4);
        // Ellipsized so the wordmark cannot hold the rail open: a revealer
        // that slides vertically still reports its child's width, and the
        // title alone was keeping the collapsed rail at 120px.
        let name = gtk::Label::builder()
            .label("CheapAzSLA")
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        name.add_css_class("heading");
        let tag = gtk::Label::builder()
            .label("Resin print files")
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        tag.add_css_class("caption");
        tag.add_css_class("cz-dim");
        brand.append(&name);
        brand.append(&tag);
        let brand_reveal = gtk::Revealer::builder()
            .child(&brand)
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(COLLAPSE_MS)
            .reveal_child(true)
            .build();
        // A collapsed revealer still reports its child's width along the axis
        // it is not sliding on, and a box hands out spare space towards each
        // child's natural width before it considers expansion — so the folded
        // rail was being pulled back open to the width of the wordmark. Once
        // the slide has finished there is nothing to show, so it goes.
        hide_when_folded(&brand_reveal);
        sidebar.append(&brand_reveal);

        let shell = Rc::new(Self {
            widget: gtk::Box::new(gtk::Orientation::Horizontal, 0),
            stack: stack.clone(),
            items: RefCell::new(Vec::new()),
            current: RefCell::new(Section::Convert),
            on_change: RefCell::new(None),
            sidebar: sidebar.clone(),
            reveals: RefCell::new(vec![brand_reveal]),
            width_anim: RefCell::new(None),
            compact: Cell::new(false),
        });

        for section in Section::ALL {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
            // A 3px bar marks the active section, so selection is not carried
            // by colour alone (§15, §35).
            let marker = gtk::Box::new(gtk::Orientation::Vertical, 0);
            marker.add_css_class("cz-nav-marker");
            marker.set_size_request(3, 18);
            marker.set_valign(gtk::Align::Center);
            row.append(&marker);
            row.append(&gtk::Image::from_icon_name(section.icon()));
            let reveal = gtk::Revealer::builder()
                .child(&gtk::Label::new(Some(section.label())))
                .transition_type(gtk::RevealerTransitionType::SlideRight)
                .transition_duration(COLLAPSE_MS)
                .reveal_child(true)
                .build();
            hide_when_folded(&reveal);
            row.append(&reveal);
            shell.reveals.borrow_mut().push(reveal);

            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("flat");
            button.add_css_class("cz-nav-item");
            button.set_tooltip_text(Some(section.label()));
            let sh = shell.clone();
            button.connect_clicked(move |_| sh.show(section));
            sidebar.append(&button);
            shell.items.borrow_mut().push(NavItem { button, section });
        }

        // About sits at the foot, away from navigation (§4).
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_vexpand(true);
        sidebar.append(&spacer);

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
        for reveal in self.reveals.borrow().iter() {
            if !compact {
                // Made visible before it is told to reveal, or the slide has
                // nothing to run on.
                reveal.set_visible(true);
            }
            reveal.set_reveal_child(!compact);
        }

        // From wherever it is now, not from the nominal width, so a toggle
        // part way through the previous one carries on from there.
        let from = self.sidebar.width_request().max(RAIL_WIDTH) as f64;
        let to = if compact {
            RAIL_WIDTH
        } else {
            theme::SIDEBAR_WIDTH
        } as f64;

        // One animation, reused. Building a fresh one per toggle looked right
        // and was not: the second one never ran a single frame, and the rail
        // stayed at whatever width the first had left it.
        let anim = self.width_anim.borrow().clone();
        let anim = match anim {
            Some(a) => a,
            None => {
                let sidebar = self.sidebar.clone();
                let target = adw::CallbackAnimationTarget::new(move |value| {
                    sidebar.set_size_request(value.round() as i32, -1);
                });
                let a = adw::TimedAnimation::builder()
                    .widget(&self.sidebar)
                    .value_from(from)
                    .value_to(to)
                    .duration(COLLAPSE_MS)
                    .easing(adw::Easing::EaseOutCubic)
                    .target(&target)
                    .build();
                *self.width_anim.borrow_mut() = Some(a.clone());
                a
            }
        };
        anim.pause();
        anim.set_value_from(from);
        anim.set_value_to(to);
        anim.reset();
        anim.play();
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
