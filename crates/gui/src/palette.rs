//! Command palette (§28).
//!
//! A power-user shortcut, deliberately out of the way: Ctrl+K and nothing in
//! the main interface points at it, because a beginner should never need it.
//! Every action here is reachable by other means.

use crate::theme;
use adw::prelude::*;
use gtk::glib;
use std::rc::Rc;

/// What an action does when chosen.
type Run = Box<dyn Fn(&Rc<crate::App>)>;

/// One thing the palette can do.
struct Action {
    label: &'static str,
    detail: &'static str,
    icon: &'static str,
    run: Run,
}

fn actions() -> Vec<Action> {
    vec![
        Action {
            label: "Open files",
            detail: "Ctrl+O",
            icon: "document-open-symbolic",
            run: Box::new(crate::choose_files),
        },
        Action {
            label: "Convert",
            detail: "Ctrl+Enter",
            icon: "media-playlist-repeat-symbolic",
            run: Box::new(crate::start_convert),
        },
        Action {
            label: "Choose output folder",
            detail: "Where converted files are saved",
            icon: "folder-symbolic",
            run: Box::new(crate::choose_folder),
        },
        Action {
            label: "Save beside the original",
            detail: "Reset the output folder",
            icon: "folder-symbolic",
            run: Box::new(|ui| crate::set_out_dir(ui, None)),
        },
        Action {
            label: "Preview layers",
            detail: "Look through the file",
            icon: "view-reveal-symbolic",
            run: Box::new(|ui| ui.shell.show(crate::Section::Preview)),
        },
        Action {
            label: "Convert",
            detail: "Go to the Convert page",
            icon: "media-playlist-repeat-symbolic",
            run: Box::new(|ui| ui.shell.show(crate::Section::Convert)),
        },
        Action {
            label: "History",
            detail: "Past conversions",
            icon: "document-open-recent-symbolic",
            run: Box::new(|ui| ui.shell.show(crate::Section::History)),
        },
        Action {
            label: "Settings",
            detail: "Preferences",
            icon: "emblem-system-symbolic",
            run: Box::new(|ui| ui.shell.show(crate::Section::Settings)),
        },
        Action {
            label: "Remove selected file",
            detail: "Delete",
            icon: "window-close-symbolic",
            run: Box::new(crate::remove_selected),
        },
    ]
}

pub fn show(ui: &Rc<crate::App>) {
    let window = adw::Window::builder()
        .transient_for(&ui.window)
        .modal(true)
        .default_width(520)
        .default_height(420)
        .title("Commands")
        .build();
    window.add_css_class("cheapazsla");

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search CheapAzSLA…")
        .build();
    search.set_margin_top(theme::SPACE_3);
    search.set_margin_bottom(theme::SPACE_2);
    search.set_margin_start(theme::SPACE_3);
    search.set_margin_end(theme::SPACE_3);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("navigation-sidebar");
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&search);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&scroller);
    window.set_content(Some(&content));

    let all = Rc::new(actions());
    let visible: Rc<std::cell::RefCell<Vec<usize>>> = Rc::new(std::cell::RefCell::new(Vec::new()));

    let rebuild = {
        let list = list.clone();
        let all = all.clone();
        let visible = visible.clone();
        move |filter: &str| {
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            visible.borrow_mut().clear();
            let needle = filter.trim().to_lowercase();
            for (i, a) in all.iter().enumerate() {
                if !needle.is_empty()
                    && !a.label.to_lowercase().contains(&needle)
                    && !a.detail.to_lowercase().contains(&needle)
                {
                    continue;
                }
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
                row_box.set_margin_top(theme::SPACE_2);
                row_box.set_margin_bottom(theme::SPACE_2);
                row_box.set_margin_start(theme::SPACE_3);
                row_box.set_margin_end(theme::SPACE_3);
                row_box.append(&gtk::Image::from_icon_name(a.icon));
                let l = gtk::Label::builder()
                    .label(a.label)
                    .xalign(0.0)
                    .hexpand(true)
                    .build();
                row_box.append(&l);
                let d = gtk::Label::new(Some(a.detail));
                d.add_css_class("caption");
                d.add_css_class("cz-dim");
                row_box.append(&d);
                list.append(&gtk::ListBoxRow::builder().child(&row_box).build());
                visible.borrow_mut().push(i);
            }
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
            }
        }
    };
    rebuild("");

    {
        let rebuild = rebuild.clone();
        search.connect_search_changed(move |e| rebuild(&e.text()));
    }

    let activate = {
        let ui = ui.clone();
        let all = all.clone();
        let visible = visible.clone();
        let window = window.clone();
        move |index: i32| {
            if index < 0 {
                return;
            }
            let Some(&which) = visible.borrow().get(index as usize) else {
                return;
            };
            window.close();
            // Run after the window is gone so a dialog it opens is not
            // transient for something that is closing.
            let ui = ui.clone();
            let all = all.clone();
            glib::idle_add_local_once(move || {
                (all[which].run)(&ui);
            });
        }
    };

    {
        let activate = activate.clone();
        list.connect_row_activated(move |_, row| activate(row.index()));
    }
    {
        // Enter runs the highlighted action; Escape closes; arrows move.
        let list2 = list.clone();
        let activate = activate.clone();
        let window2 = window.clone();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| match key {
            gtk::gdk::Key::Escape => {
                window2.close();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                if let Some(row) = list2.selected_row() {
                    activate(row.index());
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Down => {
                let next = list2.selected_row().map(|r| r.index() + 1).unwrap_or(0);
                if let Some(r) = list2.row_at_index(next) {
                    list2.select_row(Some(&r));
                }
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Up => {
                let prev = list2.selected_row().map(|r| r.index() - 1).unwrap_or(0);
                if let Some(r) = list2.row_at_index(prev.max(0)) {
                    list2.select_row(Some(&r));
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        window.add_controller(keys);
    }

    window.present();
    search.grab_focus();
}
