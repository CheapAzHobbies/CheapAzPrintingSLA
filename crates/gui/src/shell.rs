//! The window shell: a compact sidebar and a spacious workspace (§3, §4).
//!
//! Sections are pages in one stack rather than separate windows, so moving
//! between them keeps state and the application feels continuous (§24).

use crate::theme;
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

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
        let name = gtk::Label::builder()
            .label("CheapAzSLA")
            .xalign(0.0)
            .build();
        name.add_css_class("heading");
        let tag = gtk::Label::builder()
            .label("Resin print files")
            .xalign(0.0)
            .build();
        tag.add_css_class("caption");
        tag.add_css_class("cz-dim");
        brand.append(&name);
        brand.append(&tag);
        sidebar.append(&brand);

        let shell = Rc::new(Self {
            widget: gtk::Box::new(gtk::Orientation::Horizontal, 0),
            stack: stack.clone(),
            items: RefCell::new(Vec::new()),
            current: RefCell::new(Section::Convert),
            on_change: RefCell::new(None),
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
            row.append(&gtk::Label::new(Some(section.label())));

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
        .width_request(140)
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
