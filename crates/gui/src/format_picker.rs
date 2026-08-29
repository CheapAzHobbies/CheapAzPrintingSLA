//! Searchable format selector (§8, §9, §11).
//!
//! A popover attached to its button, carrying a search field, a recently used
//! section and the full list. Recents come first because in practice a user
//! converts to the same one or two formats for months at a time, and making
//! them scroll past twenty they will never touch is the kind of small friction
//! that adds up.
//!
//! Formats come from the registry, never a list kept here (§10), so a new
//! handler appears with no change to this file.

use crate::theme;
use adw::prelude::*;
use cheapazsla_core::format::FormatInfo;
use cheapazsla_core::registry;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

/// Which direction the picker is listing.
///
/// Only Write exists so far. Manual input-format override is a real feature
/// but it is not built, and a variant nothing constructs is scaffolding
/// pretending to be a capability.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Write,
}

/// Called with the chosen format id.
type ChangeHandler = Box<dyn Fn(&'static str)>;

pub struct FormatPicker {
    pub button: gtk::MenuButton,
    popover: gtk::Popover,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    label: gtk::Label,
    direction: Direction,
    /// Format ids in the order they are shown, parallel to the list rows.
    shown: RefCell<Vec<&'static str>>,
    selected: RefCell<Option<&'static str>>,
    recents: RefCell<Vec<String>>,
    on_change: RefCell<Option<ChangeHandler>>,
}

impl FormatPicker {
    pub fn new(direction: Direction) -> Rc<Self> {
        let label = gtk::Label::builder()
            .label("—")
            .xalign(0.0)
            .hexpand(true)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
        content.append(&label);
        content.append(&gtk::Image::from_icon_name("pan-down-symbolic"));

        let button = gtk::MenuButton::builder().child(&content).build();
        button.set_tooltip_text(Some(match direction {
            Direction::Write => "Output format",
        }));

        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search formats…")
            .build();
        search.set_margin_top(theme::SPACE_2);
        search.set_margin_bottom(theme::SPACE_2);
        search.set_margin_start(theme::SPACE_2);
        search.set_margin_end(theme::SPACE_2);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("navigation-sidebar");

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(360) // never taller than the screen (§8)
            .propagate_natural_height(true)
            .child(&list)
            .build();

        let inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
        inner.append(&search);
        inner.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        inner.append(&scroller);

        let popover = gtk::Popover::builder()
            .child(&inner)
            .width_request(320)
            .has_arrow(false)
            .build();
        button.set_popover(Some(&popover));

        let me = Rc::new(Self {
            button,
            popover: popover.clone(),
            search: search.clone(),
            list: list.clone(),
            label,
            direction,
            shown: RefCell::new(Vec::new()),
            selected: RefCell::new(None),
            recents: RefCell::new(Vec::new()),
            on_change: RefCell::new(None),
        });

        {
            let me2 = me.clone();
            search.connect_search_changed(move |e| me2.rebuild(&e.text()));
        }
        {
            // Opening focuses the search field, so typing filters immediately.
            let me2 = me.clone();
            popover.connect_show(move |_| {
                me2.search.set_text("");
                me2.rebuild("");
                me2.search.grab_focus();
            });
        }
        {
            // Escape closes, as §8 asks. The popover already closes on an
            // outside click.
            let me2 = me.clone();
            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    me2.popover.popdown();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            popover.add_controller(keys);
        }
        me
    }

    pub fn set_recents(self: &Rc<Self>, recents: Vec<String>) {
        *self.recents.borrow_mut() = recents;
    }

    pub fn selected(&self) -> Option<&'static str> {
        *self.selected.borrow()
    }

    pub fn set_selected(self: &Rc<Self>, id: &str) {
        let Some(handler) = registry::by_id(id) else {
            return;
        };
        let info = handler.info();
        *self.selected.borrow_mut() = Some(info.id);
        self.label.set_text(info.extension.to_uppercase().as_str());
        self.button
            .set_tooltip_text(Some(&format!("{} (.{})", info.name, info.extension)));
    }

    pub fn connect_changed(&self, f: impl Fn(&'static str) + 'static) {
        *self.on_change.borrow_mut() = Some(Box::new(f));
    }

    fn candidates(&self) -> Vec<&'static FormatInfo> {
        match self.direction {
            Direction::Write => registry::writable(),
        }
    }

    /// Rebuild the list for the current filter.
    fn rebuild(self: &Rc<Self>, filter: &str) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        self.shown.borrow_mut().clear();

        let needle = filter.trim().to_lowercase();
        let matches = |i: &FormatInfo| {
            needle.is_empty()
                || i.name.to_lowercase().contains(&needle)
                || i.extension.contains(&needle)
                || i.id.contains(&needle)
        };

        let all: Vec<&'static FormatInfo> = self
            .candidates()
            .into_iter()
            .filter(|i| matches(i))
            .collect();

        // Recently used first, but only when not searching: while typing the
        // user is looking for a specific thing and grouping just gets in the way.
        if needle.is_empty() {
            let recents = self.recents.borrow().clone();
            let recent: Vec<&'static FormatInfo> = recents
                .iter()
                .filter_map(|id| registry::by_id(id).map(|h| h.info()))
                .filter(|i| all.iter().any(|a| a.id == i.id))
                .collect();
            if !recent.is_empty() {
                self.add_heading("Recently used");
                for info in &recent {
                    self.add_row(info);
                }
                if recent.len() < all.len() {
                    self.add_heading("All formats");
                }
            }
            for info in &all {
                if recent.iter().any(|r| r.id == info.id) {
                    continue;
                }
                self.add_row(info);
            }
        } else {
            for info in &all {
                self.add_row(info);
            }
            if all.is_empty() {
                let empty = gtk::Label::builder()
                    .label("No formats match")
                    .margin_top(theme::SPACE_4)
                    .margin_bottom(theme::SPACE_4)
                    .build();
                empty.add_css_class("cz-dim");
                let row = gtk::ListBoxRow::builder()
                    .child(&empty)
                    .selectable(false)
                    .activatable(false)
                    .build();
                self.list.append(&row);
            }
        }
    }

    fn add_heading(&self, text: &str) {
        let l = crate::shell::section_label(text);
        l.set_margin_top(theme::SPACE_2);
        l.set_margin_bottom(theme::SPACE_1);
        l.set_margin_start(theme::SPACE_3);
        let row = gtk::ListBoxRow::builder()
            .child(&l)
            .selectable(false)
            .activatable(false)
            .build();
        self.list.append(&row);
    }

    fn add_row(self: &Rc<Self>, info: &'static FormatInfo) {
        let box_ = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
        box_.set_margin_top(theme::SPACE_2);
        box_.set_margin_bottom(theme::SPACE_2);
        box_.set_margin_start(theme::SPACE_3);
        box_.set_margin_end(theme::SPACE_3);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let name = gtk::Label::builder().label(info.name).xalign(0.0).build();
        let ext = gtk::Label::builder()
            .label(format!(".{}", info.extension))
            .xalign(0.0)
            .build();
        ext.add_css_class("caption");
        ext.add_css_class("cz-dim");
        text.append(&name);
        text.append(&ext);
        text.set_hexpand(true);
        box_.append(&text);

        // A tick marks the current choice, so selection does not rely on a
        // highlight that a screen reader cannot convey.
        if self.selected().map(|s| s == info.id).unwrap_or(false) {
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.add_css_class("cz-ok");
            box_.append(&check);
        }

        let row = gtk::ListBoxRow::builder()
            .child(&box_)
            .activatable(true)
            .build();
        self.list.append(&row);
        self.shown.borrow_mut().push(info.id);

        let me = self.clone();
        let id = info.id;
        self.list.connect_row_activated(move |_, activated| {
            // Rows are appended in order, so the index maps to the id list.
            let idx = activated.index();
            if idx < 0 {
                return;
            }
            let ids = me.shown.borrow().clone();
            // Headings are rows too, so count only activatable ones.
            let mut seen = 0usize;
            let mut child = me.list.first_child();
            let mut target: Option<&'static str> = None;
            let mut i = 0;
            while let Some(c) = child {
                if let Some(r) = c.downcast_ref::<gtk::ListBoxRow>() {
                    if r.is_activatable() {
                        if i == idx {
                            target = ids.get(seen).copied();
                            break;
                        }
                        seen += 1;
                    }
                }
                i += 1;
                child = c.next_sibling();
            }
            let chosen = target.unwrap_or(id);
            me.set_selected(chosen);
            me.popover.popdown();
            if let Some(f) = me.on_change.borrow().as_ref() {
                f(chosen);
            }
        });
    }
}

/// The information popover beside a format control (§11, §23).
pub fn info_button(get: impl Fn() -> Option<&'static FormatInfo> + 'static) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .icon_name("help-about-symbolic")
        .tooltip_text("About this format")
        .build();
    button.add_css_class("flat");

    let content = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_2);
    content.set_margin_top(theme::SPACE_3);
    content.set_margin_bottom(theme::SPACE_3);
    content.set_margin_start(theme::SPACE_3);
    content.set_margin_end(theme::SPACE_3);
    content.set_size_request(300, -1);

    let popover = gtk::Popover::builder().child(&content).build();
    button.set_popover(Some(&popover));

    popover.connect_show(move |_| {
        while let Some(c) = content.first_child() {
            content.remove(&c);
        }
        let Some(info) = get() else {
            content.append(&gtk::Label::new(Some("No format selected")));
            return;
        };
        let title = gtk::Label::builder().label(info.name).xalign(0.0).build();
        title.add_css_class("heading");
        content.append(&title);

        let ext = gtk::Label::builder()
            .label(format!(".{}", info.extension))
            .xalign(0.0)
            .build();
        ext.add_css_class("caption");
        ext.add_css_class("cz-dim");
        content.append(&ext);

        let desc = gtk::Label::builder()
            .label(info.description)
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(42)
            .build();
        desc.set_margin_top(theme::SPACE_2);
        content.append(&desc);

        let caps = info.capabilities;
        let supports = crate::shell::section_label("Stores");
        supports.set_margin_top(theme::SPACE_3);
        content.append(&supports);
        for (ok, text) in [
            (true, "Layer images"),
            (caps.thumbnails, "Preview images"),
            (caps.per_layer_exposure, "Per-layer exposure"),
            (caps.per_layer_lift, "Per-layer lift settings"),
            (caps.print_time, "Estimated print time"),
            (caps.material_volume, "Resin volume"),
        ] {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
            let icon = gtk::Image::from_icon_name(if ok {
                "object-select-symbolic"
            } else {
                "window-close-symbolic"
            });
            icon.add_css_class(if ok { "cz-ok" } else { "cz-dim" });
            let l = gtk::Label::builder().label(text).xalign(0.0).build();
            if !ok {
                l.add_css_class("cz-dim");
            }
            row.append(&icon);
            row.append(&l);
            content.append(&row);
        }

        if !info.limitations.is_empty() {
            let lim = crate::shell::section_label("Worth knowing");
            lim.set_margin_top(theme::SPACE_3);
            content.append(&lim);
            for text in info.limitations {
                let l = gtk::Label::builder()
                    .label(*text)
                    .xalign(0.0)
                    .wrap(true)
                    .max_width_chars(42)
                    .build();
                l.add_css_class("caption");
                l.add_css_class("cz-dim");
                content.append(&l);
            }
        }
    });
    button
}
