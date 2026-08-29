//! CheapAzSLA desktop application.
//!
//! The interface is a thin shell over cheapazsla-core. All parsing, decoding
//! and validation happens in the engine; this crate decides what to show.
//!
//! Anything the engine cannot yet do is presented as unavailable rather than
//! mocked up (§47).

mod drives;
mod render;

use adw::prelude::*;
use cheapazsla_core::settings::Settings;
use cheapazsla_core::{convert, registry, OpenedFile};
use gtk::glib;
use gtk::{gdk, gio};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

const APP_ID: &str = "com.cheapazhobbies.CheapAzSLA";

/// Everything the window needs to know about the file on screen.
struct Loaded {
    path: PathBuf,
    size_bytes: u64,
    opened: Arc<OpenedFile>,
    detection: String,
    confidence: String,
    warnings: Vec<String>,
    extension_mismatch: bool,
}

struct Ui {
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    right: gtk::Stack,
    toasts: adw::ToastOverlay,
    // inspect page
    info_group: adw::PreferencesGroup,
    warn_group: adw::PreferencesGroup,
    // AdwPreferencesGroup wraps its children in an internal box, so walking
    // first_child() does not reach the rows. Track what was added instead.
    info_rows: RefCell<Vec<adw::ActionRow>>,
    warn_rows: RefCell<Vec<adw::ActionRow>>,
    picture: gtk::Picture,
    layer_label: gtk::Label,
    scale_label: gtk::Label,
    slider: gtk::Scale,
    nav_buttons: Vec<gtk::Button>,
    play_button: gtk::Button,
    spinner: gtk::Spinner,
    clear_btn: gtk::Button,
    title: adw::WindowTitle,
    // convert bar
    convert_bar: gtk::Box,
    format_row: adw::ComboRow,
    dest_row: adw::ActionRow,
    drive_box: gtk::Box,
    name_row: adw::EntryRow,
    convert_btn: gtk::Button,
    progress: gtk::ProgressBar,
    out_dir: RefCell<Option<PathBuf>>,
    settings: RefCell<Settings>,
    writable: RefCell<Vec<&'static str>>,
}

thread_local! {
    static LOADED: RefCell<Option<Loaded>> = const { RefCell::new(None) };
    static PLAYING: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|_| {
        // §36: dark by default. The user can still override in Settings later.
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    });

    let pending: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));

    let p = pending.clone();
    app.connect_open(move |app, files, _| {
        if let Some(f) = files.first().and_then(|f| f.path()) {
            *p.borrow_mut() = Some(f);
        }
        app.activate();
    });

    let p = pending.clone();
    app.connect_activate(move |app| {
        let ui = build_ui(app);
        ui.window.present();
        if let Some(path) = p.borrow_mut().take() {
            load_file(&ui, &path);
        }
    });

    app.run()
}

fn build_ui(app: &adw::Application) -> Rc<Ui> {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("CheapAzSLA")
        .default_width(1120)
        .default_height(720)
        .build();

    let title = adw::WindowTitle::new("CheapAzSLA", "Resin print file converter & inspector");
    let header = adw::HeaderBar::builder().title_widget(&title).build();

    let open_btn = gtk::Button::builder()
        .label("Open File…")
        .tooltip_text("Open a resin print file  (Ctrl+O)")
        .build();
    open_btn.add_css_class("suggested-action");
    header.pack_start(&open_btn);

    let clear_btn = gtk::Button::builder()
        .icon_name("edit-clear-all-symbolic")
        .tooltip_text("Clear the loaded file  (Ctrl+W)")
        .build();
    clear_btn.add_css_class("flat");
    clear_btn.set_visible(false);
    header.pack_start(&clear_btn);

    let spinner = gtk::Spinner::new();
    header.pack_end(&spinner);

    let about_btn = gtk::Button::builder()
        .icon_name("help-about-symbolic")
        .tooltip_text("About CheapAzSLA")
        .build();
    about_btn.add_css_class("flat");
    header.pack_end(&about_btn);

    let prefs_btn = gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text("Settings")
        .build();
    prefs_btn.add_css_class("flat");
    header.pack_end(&prefs_btn);

    // ---- drop target (§38) ----
    // Lives inside the viewer pane, so clearing a file swaps only the image
    // area and leaves the rest of the window where it was.
    // "Sliced" rather than "resin": it names what the file is, and quietly
    // rules out the STL someone would otherwise try to drop here.
    let readable: Vec<String> = registry::readable()
        .iter()
        .map(|i| i.extension.to_uppercase())
        .collect();
    let writable_names: Vec<String> = registry::writable()
        .iter()
        .map(|i| i.extension.to_uppercase())
        .collect();
    let empty = adw::StatusPage::builder()
        .icon_name("document-open-symbolic")
        .title("Drop a sliced file here")
        .description(format!(
            "or browse your computer\n\nOpens {}   ·   Converts to {}",
            readable.join(", "),
            writable_names.join(", ")
        ))
        .build();
    empty.set_vexpand(true);
    let empty_btn = gtk::Button::builder()
        .label("Browse Files…")
        .halign(gtk::Align::Center)
        .build();
    empty_btn.add_css_class("pill");
    empty_btn.add_css_class("suggested-action");
    empty.set_child(Some(&empty_btn));

    // ---- inspect page ----
    let info_group = adw::PreferencesGroup::builder().title("File").build();
    info_group.set_visible(false);
    let warn_group = adw::PreferencesGroup::builder().title("Validation").build();
    warn_group.set_visible(false);

    // ---- convert controls (§21, §27) ----
    let convert_group = adw::PreferencesGroup::builder().title("Convert").build();
    let format_row = adw::ComboRow::builder()
        .title("Output format")
        .subtitle("What to write")
        .build();
    let fmt_model = gtk::StringList::new(&[]);
    let mut writable_ids: Vec<&'static str> = Vec::new();
    for info in registry::writable() {
        fmt_model.append(&format!("{}  ·  .{}", info.name, info.extension));
        writable_ids.push(info.id);
    }
    format_row.set_model(Some(&fmt_model));
    convert_group.add(&format_row);

    let dest_row = adw::ActionRow::builder()
        .title("Save to")
        .subtitle("Beside the original")
        .build();
    let pick_dir = gtk::Button::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .tooltip_text("Pick a folder, including a USB drive or SD card")
        .build();
    let reset_dir = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text("Back to saving beside the original")
        .build();
    reset_dir.add_css_class("flat");
    dest_row.add_suffix(&pick_dir);
    dest_row.add_suffix(&reset_dir);
    convert_group.add(&dest_row);

    // Quick access to pinned drives. Rebuilt whenever the pin list changes or
    // a drive is plugged in or out.
    let drive_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    drive_box.set_margin_top(4);
    drive_box.set_margin_start(12);
    drive_box.set_margin_end(12);
    drive_box.set_margin_bottom(4);
    drive_box.set_visible(false);
    convert_group.add(&drive_box);

    let name_row = adw::EntryRow::builder().title("Save as").build();
    name_row.set_show_apply_button(false);
    convert_group.add(&name_row);

    let convert_btn = gtk::Button::with_label("Convert");
    convert_btn.add_css_class("suggested-action");
    convert_btn.add_css_class("pill");
    convert_btn.set_halign(gtk::Align::Fill);
    convert_btn.set_tooltip_text(Some("Convert this file  (Ctrl+Enter)"));
    convert_btn.set_sensitive(false); // nothing loaded yet

    let progress = gtk::ProgressBar::builder()
        .show_text(true)
        .visible(false)
        .build();

    let convert_bar = gtk::Box::new(gtk::Orientation::Vertical, 8);
    convert_bar.append(&convert_group);
    convert_bar.append(&convert_btn);
    convert_bar.append(&progress);

    let side = gtk::Box::new(gtk::Orientation::Vertical, 12);
    side.set_margin_top(12);
    side.set_margin_bottom(12);
    side.set_margin_start(12);
    side.set_margin_end(6);
    side.append(&convert_bar);
    side.append(&info_group);
    side.append(&warn_group);
    let side_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .width_request(340)
        .hexpand(false)
        .vexpand(true)
        .child(&side)
        .build();

    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .hexpand(true)
        .vexpand(true)
        .build();
    let frame = gtk::Frame::builder().child(&picture).build();
    frame.add_css_class("view");
    frame.set_margin_top(12);
    frame.set_margin_bottom(6);
    frame.set_margin_start(6);
    frame.set_margin_end(12);

    let layer_label = gtk::Label::new(Some("Layer — / —"));
    layer_label.add_css_class("heading");
    let scale_label = gtk::Label::new(None);
    scale_label.add_css_class("dim-label");
    scale_label.add_css_class("caption");

    let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
    slider.set_hexpand(true);
    slider.set_draw_value(false);

    let mk = |icon: &str, tip: &str| {
        let b = gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tip)
            .build();
        b.add_css_class("flat");
        b
    };
    let first = mk("go-first-symbolic", "First layer  (Home)");
    let prev = mk("go-previous-symbolic", "Previous layer  (Left)");
    let play_button = mk(
        "media-playback-start-symbolic",
        "Play through layers  (Space)",
    );
    let next = mk("go-next-symbolic", "Next layer  (Right)");
    let last = mk("go-last-symbolic", "Last layer  (End)");

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.set_margin_start(6);
    controls.set_margin_end(12);
    controls.set_margin_bottom(12);
    for b in [&first, &prev, &play_button, &next, &last] {
        controls.append(b);
    }
    controls.append(&slider);

    let labels = gtk::Box::new(gtk::Orientation::Vertical, 0);
    labels.set_halign(gtk::Align::End);
    labels.append(&layer_label);
    labels.append(&scale_label);
    controls.append(&labels);

    let viewer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    viewer.append(&frame);
    viewer.append(&controls);

    let right = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .hexpand(true)
        .vexpand(true)
        .build();
    right.add_named(&empty, Some("drop"));
    right.add_named(&viewer, Some("view"));
    right.set_visible_child_name("drop");

    let split = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    split.append(&side_scroll);
    split.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    split.append(&right);

    // ---- convert page: honest about not existing yet (§47) ----
    let convert = adw::StatusPage::builder()
        .icon_name("emblem-synchronizing-symbolic")
        .title("Conversion is not available yet")
        .description(
            "The engine can read files but cannot write any format yet, so there is \
             nothing to convert to. This screen will list output formats as soon as \
             the first writer exists.\n\nReading and inspection work now.",
        )
        .build();

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(&split, Some("inspect"));
    stack.add_named(&convert, Some("convert"));
    stack.set_visible_child_name("inspect");

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    window.set_content(Some(&toolbar));

    let ui = Rc::new(Ui {
        window: window.clone(),
        stack,
        right: right.clone(),
        toasts,
        info_group,
        warn_group,
        info_rows: RefCell::new(Vec::new()),
        warn_rows: RefCell::new(Vec::new()),
        picture,
        layer_label,
        scale_label,
        slider,
        nav_buttons: vec![first.clone(), prev.clone(), next.clone(), last.clone()],
        play_button: play_button.clone(),
        spinner,
        clear_btn: clear_btn.clone(),
        title,
        convert_bar,
        format_row,
        dest_row,
        drive_box: drive_box.clone(),
        name_row: name_row.clone(),
        convert_btn: convert_btn.clone(),
        progress,
        out_dir: RefCell::new(None),
        settings: RefCell::new(Settings::load()),
        writable: RefCell::new(writable_ids),
    });

    wire(
        &ui,
        &open_btn,
        &empty_btn,
        &about_btn,
        &first,
        &prev,
        &next,
        &last,
        &play_button,
    );
    wire_convert(&ui, &convert_btn, &pick_dir, &reset_dir);
    {
        let ui2 = ui.clone();
        clear_btn.connect_clicked(move |_| clear_file(&ui2));
    }
    {
        let ui2 = ui.clone();
        prefs_btn.connect_clicked(move |_| show_settings(&ui2));
    }

    // Restore last session's choices, but only if they are still valid: a USB
    // drive that has since been unplugged must not silently become the target.
    refresh_drive_buttons(&ui);

    // Rebuild the quick buttons when a drive appears or disappears, so a stick
    // plugged in after launch shows up without restarting.
    {
        let monitor = gio::VolumeMonitor::get();
        for signal in ["mount-added", "mount-removed"] {
            let ui2 = ui.clone();
            monitor.connect_local(signal, false, move |_| {
                refresh_drive_buttons(&ui2);
                None
            });
        }
        // Keep the monitor alive for the life of the window.
        unsafe { ui.window.set_data("volume-monitor", monitor) };
    }

    {
        let saved = ui.settings.borrow().clone();
        if let Some(dir) = saved.last_output_dir.filter(|d| d.is_dir()) {
            set_out_dir(&ui, Some(dir));
        }
        if let Some(fmt) = saved.last_output_format {
            if let Some(idx) = ui.writable.borrow().iter().position(|id| *id == fmt) {
                ui.format_row.set_selected(idx as u32);
            }
        }
    }
    ui
}

#[allow(clippy::too_many_arguments)]
fn wire(
    ui: &Rc<Ui>,
    open_btn: &gtk::Button,
    empty_btn: &gtk::Button,
    about_btn: &gtk::Button,
    first: &gtk::Button,
    prev: &gtk::Button,
    next: &gtk::Button,
    last: &gtk::Button,
    play: &gtk::Button,
) {
    for b in [open_btn, empty_btn] {
        let ui = ui.clone();
        b.connect_clicked(move |_| choose_file(&ui));
    }

    {
        let ui = ui.clone();
        about_btn.connect_clicked(move |_| show_about(&ui));
    }

    // Drag and drop from the file manager, desktop, or a mounted drive.
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    {
        let ui = ui.clone();
        drop.connect_drop(move |_, value, _, _| {
            if let Ok(list) = value.get::<gdk::FileList>() {
                if let Some(path) = list.files().first().and_then(|f| f.path()) {
                    load_file(&ui, &path);
                    return true;
                }
            }
            false
        });
    }
    ui.window.add_controller(drop);

    // Layer navigation.
    let jump = |ui: &Rc<Ui>, f: Box<dyn Fn(u32, u32) -> u32>| {
        let count = with_loaded(|l| l.opened.print.layer_count()).unwrap_or(0);
        if count == 0 {
            return;
        }
        let cur = ui.slider.value() as u32;
        ui.slider.set_value(f(cur, count) as f64);
    };
    {
        let ui = ui.clone();
        first.connect_clicked(move |_| jump(&ui, Box::new(|_, _| 0)));
    }
    {
        let ui = ui.clone();
        prev.connect_clicked(move |_| jump(&ui, Box::new(|c, _| c.saturating_sub(1))));
    }
    {
        let ui = ui.clone();
        next.connect_clicked(move |_| jump(&ui, Box::new(|c, n| (c + 1).min(n - 1))));
    }
    {
        let ui = ui.clone();
        last.connect_clicked(move |_| jump(&ui, Box::new(|_, n| n - 1)));
    }
    {
        let ui = ui.clone();
        play.connect_clicked(move |_| toggle_play(&ui));
    }
    {
        let ui = ui.clone();
        ui.slider.clone().connect_value_changed(move |s| {
            show_layer(&ui, s.value() as u32);
        });
    }

    // Keyboard (§34).
    let keys = gtk::EventControllerKey::new();
    {
        let ui = ui.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            let count = with_loaded(|l| l.opened.print.layer_count()).unwrap_or(0);
            let cur = ui.slider.value() as u32;
            match key {
                gdk::Key::o if ctrl => {
                    choose_file(&ui);
                    glib::Propagation::Stop
                }
                gdk::Key::Return if ctrl => {
                    start_convert(&ui);
                    glib::Propagation::Stop
                }
                gdk::Key::w if ctrl => {
                    clear_file(&ui);
                    glib::Propagation::Stop
                }
                gdk::Key::Left if count > 0 => {
                    ui.slider.set_value(cur.saturating_sub(1) as f64);
                    glib::Propagation::Stop
                }
                gdk::Key::Right if count > 0 => {
                    ui.slider.set_value((cur + 1).min(count - 1) as f64);
                    glib::Propagation::Stop
                }
                gdk::Key::Home if count > 0 => {
                    ui.slider.set_value(0.0);
                    glib::Propagation::Stop
                }
                gdk::Key::End if count > 0 => {
                    ui.slider.set_value((count - 1) as f64);
                    glib::Propagation::Stop
                }
                gdk::Key::space if count > 0 => {
                    toggle_play(&ui);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
    }
    ui.window.add_controller(keys);
}

/// Rebuild the row of pinned drive shortcuts.
///
/// A pinned drive that is not plugged in stays visible but disabled, so the
/// user can see it is expected and simply absent rather than wondering where
/// the button went.
fn refresh_drive_buttons(ui: &Rc<Ui>) {
    while let Some(child) = ui.drive_box.first_child() {
        ui.drive_box.remove(&child);
    }
    let (pinned, sub) = {
        let s = ui.settings.borrow();
        (s.pinned_volumes.clone(), s.pinned_subfolder.clone())
    };
    if pinned.is_empty() {
        ui.drive_box.set_visible(false);
        return;
    }
    for name in pinned {
        let target = drives::target_dir(&name, &sub);
        let btn = gtk::Button::builder()
            .label(&name)
            .sensitive(target.is_some())
            .build();
        btn.add_css_class("pill");
        match &target {
            Some(dir) => {
                let space = drives::space(dir)
                    .map(|(free, total)| {
                        format!(
                            "\n{} free of {}",
                            render::human_bytes(free),
                            render::human_bytes(total)
                        )
                    })
                    .unwrap_or_default();
                btn.set_tooltip_text(Some(&format!("Save to {}{space}", dir.display())));
                btn.add_css_class("suggested-action");
                let ui2 = ui.clone();
                let d = dir.clone();
                btn.connect_clicked(move |_| set_out_dir(&ui2, Some(d.clone())));
            }
            None => {
                btn.set_tooltip_text(Some(&format!("{name} is not connected")));
                btn.add_css_class("dim-label");
            }
        }
        ui.drive_box.append(&btn);
    }
    ui.drive_box.set_visible(true);
}

/// Unload the current file and go back to the drop target.
fn clear_file(ui: &Rc<Ui>) {
    stop_play();
    LOADED.with(|l| *l.borrow_mut() = None);
    for r in ui.info_rows.borrow_mut().drain(..) {
        ui.info_group.remove(&r);
    }
    for r in ui.warn_rows.borrow_mut().drain(..) {
        ui.warn_group.remove(&r);
    }
    ui.warn_group.set_visible(false);
    // The convert controls are settings, not file data: the chosen format,
    // destination and pinned drives should survive clearing a file. Only the
    // Convert button is disabled, since there is nothing to convert.
    ui.convert_btn.set_sensitive(false);
    ui.clear_btn.set_visible(false);
    ui.picture.set_paintable(gdk::Paintable::NONE);
    ui.layer_label.set_text("Layer — / —");
    ui.scale_label.set_text("");
    ui.name_row.set_text("");
    ui.play_button
        .set_icon_name("media-playback-start-symbolic");
    ui.title.set_title("CheapAzSLA");
    ui.title
        .set_subtitle("Resin print file converter & inspector");
    // An empty File panel would just be a titled box with nothing in it.
    ui.info_group.set_visible(false);
    ui.right.set_visible_child_name("drop");
}

fn with_loaded<T>(f: impl FnOnce(&Loaded) -> T) -> Option<T> {
    LOADED.with(|l| l.borrow().as_ref().map(f))
}

fn choose_file(ui: &Rc<Ui>) {
    // Native picker: it handles USB, SD cards and network shares through the
    // desktop portal, so there is nothing here to hardcode.
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Resin print files"));
    for info in registry::readable() {
        filter.add_pattern(&format!("*.{}", info.extension));
        filter.add_pattern(&format!("*.{}", info.extension.to_uppercase()));
        for a in info.aliases {
            filter.add_pattern(&format!("*.{a}"));
        }
    }
    let all = gtk::FileFilter::new();
    all.set_name(Some("All files"));
    all.add_pattern("*");

    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    filters.append(&all);

    let dialog = gtk::FileDialog::builder()
        .title("Open a resin print file")
        .filters(&filters)
        .modal(true)
        .build();
    if let Some(dir) = ui.settings.borrow().open_start_dir() {
        dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
    }

    let ui = ui.clone();
    dialog.open(
        Some(&ui.window.clone()),
        gio::Cancellable::NONE,
        move |res| {
            if let Ok(file) = res {
                if let Some(path) = file.path() {
                    load_file(&ui, &path);
                }
            }
        },
    );
}

/// Open a file on a worker thread so the interface never blocks (§17).
fn load_file(ui: &Rc<Ui>, path: &Path) {
    stop_play();
    ui.spinner.set_spinning(true);
    ui.spinner.set_visible(true);
    ui.title.set_subtitle("Reading…");

    let (tx, rx) = async_channel::bounded(1);
    let p = path.to_path_buf();
    std::thread::spawn(move || {
        let result = read_file(&p);
        let _ = tx.send_blocking(result);
    });

    let ui = ui.clone();
    glib::spawn_future_local(async move {
        match rx.recv().await {
            Ok(Ok(loaded)) => present(&ui, loaded),
            Ok(Err(msg)) => {
                ui.spinner.set_spinning(false);
                ui.spinner.set_visible(false);
                ui.title
                    .set_subtitle("Resin print file converter & inspector");
                error_dialog(&ui, &msg.0, &msg.1);
            }
            Err(_) => {}
        }
    });
}

/// Message shown to the user, plus the technical detail behind it (§28).
struct Failure(String, String);

fn read_file(path: &Path) -> Result<Loaded, Failure> {
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let id = registry::identify(path).map_err(|e| {
        Failure(
            format!("CheapAzSLA does not recognise {}.", name_of(path)),
            e.to_string(),
        )
    })?;

    let handler = registry::by_id(id.detection.format_id)
        .ok_or_else(|| Failure("No handler for this format.".into(), String::new()))?;

    let warnings = handler.validate(path).unwrap_or_default();

    let opened = handler.open(path).map_err(|e| {
        Failure(
            format!("{} could not be opened.", name_of(path)),
            e.to_string(),
        )
    })?;

    Ok(Loaded {
        path: path.to_path_buf(),
        size_bytes,
        opened: Arc::new(opened),
        detection: id.detection.reason.clone(),
        confidence: format!("{:?}", id.detection.confidence),
        warnings,
        extension_mismatch: id.extension_mismatch,
    })
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Build a labelled row. Values the file did not record are shown as such
/// rather than filled with a plausible-looking default (§13, §24).
fn row(title: &str, value: Option<String>) -> adw::ActionRow {
    let r = adw::ActionRow::builder().title(title).build();
    match value {
        Some(v) => r.set_subtitle(&v),
        None => {
            r.set_subtitle("not recorded");
            r.add_css_class("dim-label");
        }
    }
    r
}

/// Add a row to the file panel and remember it so it can be removed later.
fn add_info(ui: &Rc<Ui>, r: adw::ActionRow) {
    ui.info_group.add(&r);
    ui.info_rows.borrow_mut().push(r);
}

fn present(ui: &Rc<Ui>, loaded: Loaded) {
    ui.spinner.set_spinning(false);
    ui.spinner.set_visible(false);

    let p = &loaded.opened.print;
    let count = p.layer_count();

    // --- file panel ---
    for r in ui.info_rows.borrow_mut().drain(..) {
        ui.info_group.remove(&r);
    }
    ui.info_group.set_title(&name_of(&loaded.path));
    add_info(ui, row("Format", Some(p.source_format.to_uppercase())));
    add_info(ui, row("Detected by", Some(loaded.detection.clone())));
    add_info(ui, row("Confidence", Some(loaded.confidence.clone())));
    add_info(
        ui,
        row("Size", Some(render::human_bytes(loaded.size_bytes))),
    );
    add_info(
        ui,
        row(
            "Location",
            loaded.path.parent().map(|d| d.display().to_string()),
        ),
    );
    add_info(
        ui,
        row(
            "Resolution",
            Some(format!(
                "{} x {} px",
                p.geometry.resolution_x, p.geometry.resolution_y
            )),
        ),
    );
    add_info(
        ui,
        row(
            "Pixel size",
            p.geometry
                .pixel_size_um()
                .map(|(x, y)| format!("{x:.2} x {y:.2} um")),
        ),
    );
    add_info(ui, row("Layers", Some(count.to_string())));
    add_info(
        ui,
        row(
            "Layer height",
            Some(format!("{} mm", p.exposure.layer_height_mm)),
        ),
    );
    add_info(
        ui,
        row("Height", p.height_mm().map(|h| format!("{h:.2} mm"))),
    );
    add_info(
        ui,
        row("Exposure", Some(format!("{} s", p.exposure.exposure_s))),
    );
    add_info(
        ui,
        row(
            "Bottom exposure",
            p.exposure.bottom_exposure_s.map(|v| format!("{v} s")),
        ),
    );
    add_info(
        ui,
        row(
            "Bottom layers",
            p.exposure.bottom_layers.map(|v| v.to_string()),
        ),
    );
    add_info(
        ui,
        row("Print time", p.print_time_s.map(render::human_time)),
    );
    add_info(
        ui,
        row("Material", p.material_volume_ml.map(|v| format!("{v} ml"))),
    );
    add_info(ui, row("Material name", p.material_name.clone()));
    add_info(ui, row("Printer", p.machine_name.clone()));

    // --- validation panel ---
    for r in ui.warn_rows.borrow_mut().drain(..) {
        ui.warn_group.remove(&r);
    }
    let mut notes: Vec<String> = loaded.warnings.clone();
    if loaded.extension_mismatch {
        notes
            .push("The file extension disagrees with the contents. The contents were used.".into());
    }
    if notes.is_empty() {
        ui.warn_group.set_visible(false);
    } else {
        for n in &notes {
            let r = adw::ActionRow::builder().title(n.as_str()).build();
            r.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            r.set_title_lines(0);
            ui.warn_group.add(&r);
            ui.warn_rows.borrow_mut().push(r);
        }
        ui.warn_group.set_visible(true);
    }

    if let Some(dir) = loaded.path.parent() {
        let mut st = ui.settings.borrow_mut();
        if st.last_open_dir.as_deref() != Some(dir) {
            st.last_open_dir = Some(dir.to_path_buf());
            let _ = st.save();
        }
    }

    ui.title.set_title(&name_of(&loaded.path));
    ui.title.set_subtitle(&format!(
        "{}  ·  {} layers  ·  {}",
        p.source_format.to_uppercase(),
        count,
        render::human_bytes(loaded.size_bytes)
    ));

    ui.slider
        .set_range(0.0, (count.saturating_sub(1)).max(1) as f64);
    ui.slider.set_value(0.0);
    for b in &ui.nav_buttons {
        b.set_sensitive(count > 1);
    }
    ui.play_button.set_sensitive(count > 1);

    ui.convert_bar.set_visible(true);
    ui.convert_btn.set_sensitive(true);
    ui.clear_btn.set_visible(true);
    LOADED.with(|l| *l.borrow_mut() = Some(loaded));
    suggest_name(ui);
    ui.info_group.set_visible(true);
    ui.right.set_visible_child_name("view");
    ui.stack.set_visible_child_name("inspect");
    show_layer(ui, 0);
}

/// Decode and display one layer, off the main thread.
fn show_layer(ui: &Rc<Ui>, index: u32) {
    let Some((layers, count)) = with_loaded(|l| (l.opened.clone(), l.opened.print.layer_count()))
    else {
        return;
    };
    if count == 0 {
        return;
    }
    let index = index.min(count - 1);
    ui.layer_label
        .set_text(&format!("Layer {} / {}", index + 1, count));

    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = layers.layers.layer(index).map(|img| {
            let exposed = img.exposed_pixels(0);
            let total = img.width as u64 * img.height as u64;
            (render::texture_for(&img), exposed, total)
        });
        let _ = tx.send_blocking(result);
    });

    let ui = ui.clone();
    glib::spawn_future_local(async move {
        match rx.recv().await {
            Ok(Ok(((texture, factor), exposed, total))) => {
                ui.picture.set_paintable(Some(&texture));
                let pct = if total > 0 {
                    exposed as f64 / total as f64 * 100.0
                } else {
                    0.0
                };
                let scale = if factor > 1 {
                    format!("shown at 1/{factor}  ·  {exposed} px exposed ({pct:.3}%)")
                } else {
                    format!("{exposed} px exposed ({pct:.3}%)")
                };
                ui.scale_label.set_text(&scale);
            }
            Ok(Err(e)) => {
                ui.scale_label.set_text("layer could not be decoded");
                ui.toasts.add_toast(adw::Toast::new(&e.to_string()));
            }
            Err(_) => {}
        }
    });
}

fn toggle_play(ui: &Rc<Ui>) {
    let running = PLAYING.with(|p| p.borrow().is_some());
    if running {
        stop_play();
        ui.play_button
            .set_icon_name("media-playback-start-symbolic");
        return;
    }
    ui.play_button
        .set_icon_name("media-playback-pause-symbolic");
    let ui2 = ui.clone();
    let id = glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
        let count = with_loaded(|l| l.opened.print.layer_count()).unwrap_or(0);
        if count == 0 {
            return glib::ControlFlow::Break;
        }
        let cur = ui2.slider.value() as u32;
        if cur + 1 >= count {
            ui2.slider.set_value(0.0);
        } else {
            ui2.slider.set_value((cur + 1) as f64);
        }
        glib::ControlFlow::Continue
    });
    PLAYING.with(|p| *p.borrow_mut() = Some(id));
}

fn stop_play() {
    PLAYING.with(|p| {
        if let Some(id) = p.borrow_mut().take() {
            id.remove();
        }
    });
}

/// A readable message with the technical detail tucked behind Details (§28).
fn error_dialog(ui: &Rc<Ui>, message: &str, detail: &str) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .heading("Could not open this file")
        .body(message)
        .build();
    if !detail.is_empty() {
        let expander = gtk::Expander::builder().label("Details").build();
        let label = gtk::Label::builder()
            .label(detail)
            .wrap(true)
            .selectable(true)
            .xalign(0.0)
            .margin_top(8)
            .build();
        label.add_css_class("monospace");
        label.add_css_class("caption");
        expander.set_child(Some(&label));
        dialog.set_extra_child(Some(&expander));
    }
    dialog.add_response("ok", "Close");
    dialog.present();
}

/// Settings (§31). Deliberately short: only choices that change behaviour.
fn show_settings(ui: &Rc<Ui>) {
    let win = adw::PreferencesWindow::builder()
        .transient_for(&ui.window)
        .modal(true)
        .title("Settings")
        .search_enabled(false)
        .build();

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("Conversion")
        .description("What CheapAzSLA checks before writing a file")
        .build();

    let current = ui.settings.borrow().clone();

    let warn = adw::SwitchRow::builder()
        .title("Warn before dropping information")
        .subtitle("Ask first when the output format cannot hold everything the source has")
        .active(current.warn_on_information_loss)
        .build();
    {
        let ui = ui.clone();
        warn.connect_active_notify(move |row| {
            let mut s = ui.settings.borrow_mut();
            s.warn_on_information_loss = row.is_active();
            let _ = s.save();
        });
    }
    group.add(&warn);

    let overwrite = adw::SwitchRow::builder()
        .title("Confirm before replacing a file")
        .subtitle("Ask when a file of the same name is already there")
        .active(current.confirm_overwrite)
        .build();
    {
        let ui = ui.clone();
        overwrite.connect_active_notify(move |row| {
            let mut s = ui.settings.borrow_mut();
            s.confirm_overwrite = row.is_active();
            let _ = s.save();
        });
    }
    group.add(&overwrite);
    page.add(&group);

    // Recent output folders, filtered to those still present, so a drive that
    // has been unplugged is not offered.
    let recent = current.available_recent_dirs();
    if !recent.is_empty() {
        let rg = adw::PreferencesGroup::builder()
            .title("Recent output folders")
            .description("Folders converted files were saved to")
            .build();
        for dir in recent {
            let row = adw::ActionRow::builder()
                .title(dir.display().to_string())
                .activatable(true)
                .build();
            let ui2 = ui.clone();
            let d = dir.clone();
            let w = win.clone();
            row.connect_activated(move |_| {
                set_out_dir(&ui2, Some(d.clone()));
                w.close();
            });
            rg.add(&row);
        }
        page.add(&rg);
    }

    let og = adw::PreferencesGroup::builder()
        .title("Opening files")
        .description("Where the Open dialog starts")
        .build();
    let open_dir_row = adw::ActionRow::builder()
        .title("Default folder")
        .subtitle(
            current
                .default_open_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Wherever the last file was opened from".into()),
        )
        .build();
    let choose = gtk::Button::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .build();
    let clear_default = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text("Go back to using the last folder used")
        .build();
    clear_default.add_css_class("flat");
    {
        let ui = ui.clone();
        let row = open_dir_row.clone();
        let parent = win.clone();
        choose.connect_clicked(move |_| {
            let dlg = gtk::FileDialog::builder()
                .title("Default folder for opening files")
                .build();
            let ui2 = ui.clone();
            let row2 = row.clone();
            dlg.select_folder(Some(&parent), gio::Cancellable::NONE, move |res| {
                if let Ok(f) = res {
                    if let Some(path) = f.path() {
                        row2.set_subtitle(&path.display().to_string());
                        let mut st = ui2.settings.borrow_mut();
                        st.default_open_dir = Some(path);
                        let _ = st.save();
                    }
                }
            });
        });
    }
    {
        let ui = ui.clone();
        let row = open_dir_row.clone();
        clear_default.connect_clicked(move |_| {
            let mut st = ui.settings.borrow_mut();
            st.default_open_dir = None;
            let _ = st.save();
            row.set_subtitle("Wherever the last file was opened from");
        });
    }
    open_dir_row.add_suffix(&choose);
    open_dir_row.add_suffix(&clear_default);
    og.add(&open_dir_row);
    page.add(&og);

    let dg = adw::PreferencesGroup::builder()
        .title("Drives")
        .description(
            "Pin a drive to get a one-click shortcut when converting. Drives are \
             remembered by name, so it still works when the mount point changes.",
        )
        .build();

    let sub_row = adw::EntryRow::builder()
        .title("Subfolder on pinned drives")
        .build();
    sub_row.set_text(&current.pinned_subfolder);
    {
        let ui = ui.clone();
        sub_row.connect_changed(move |row| {
            let mut st = ui.settings.borrow_mut();
            st.pinned_subfolder = row.text().trim().trim_matches('/').to_string();
            let _ = st.save();
            drop(st);
            refresh_drive_buttons(&ui);
        });
    }
    dg.add(&sub_row);

    let mounted = drives::mounted();
    if mounted.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No drives detected")
            .subtitle("Connect a USB drive or SD card and it will appear here")
            .build();
        dg.add(&row);
    } else {
        for d in &mounted {
            let space = drives::space(&d.path)
                .map(|(free, total)| {
                    format!(
                        "{}  ·  {} free of {}",
                        d.path.display(),
                        render::human_bytes(free),
                        render::human_bytes(total)
                    )
                })
                .unwrap_or_else(|| d.path.display().to_string());
            let row = adw::SwitchRow::builder()
                .title(&d.name)
                .subtitle(&space)
                .active(current.is_pinned(&d.name))
                .build();
            if d.removable {
                row.add_prefix(&gtk::Image::from_icon_name(
                    "drive-removable-media-symbolic",
                ));
            } else {
                row.add_prefix(&gtk::Image::from_icon_name("drive-harddisk-symbolic"));
            }
            let ui2 = ui.clone();
            let name = d.name.clone();
            row.connect_active_notify(move |r| {
                {
                    let mut st = ui2.settings.borrow_mut();
                    if r.is_active() {
                        st.pin_volume(&name);
                    } else {
                        st.unpin_volume(&name);
                    }
                    let _ = st.save();
                }
                refresh_drive_buttons(&ui2);
            });
            dg.add(&row);
        }
    }

    // Drives pinned earlier that are not connected now.
    for name in &current.pinned_volumes {
        if mounted.iter().any(|d| &d.name == name) {
            continue;
        }
        let row = adw::ActionRow::builder()
            .title(name.as_str())
            .subtitle("Not connected")
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(
            "drive-removable-media-symbolic",
        ));
        row.add_css_class("dim-label");
        let unpin = gtk::Button::builder()
            .icon_name("list-remove-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Forget this drive")
            .build();
        unpin.add_css_class("flat");
        let ui2 = ui.clone();
        let n = name.clone();
        let w = win.clone();
        unpin.connect_clicked(move |_| {
            {
                let mut st = ui2.settings.borrow_mut();
                st.unpin_volume(&n);
                let _ = st.save();
            }
            refresh_drive_buttons(&ui2);
            w.close();
        });
        row.add_suffix(&unpin);
        dg.add(&row);
    }
    page.add(&dg);

    let sg = adw::PreferencesGroup::builder().title("Storage").build();
    let loc = adw::ActionRow::builder()
        .title("Settings file")
        .subtitle(
            Settings::path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not available".into()),
        )
        .build();
    sg.add(&loc);
    page.add(&sg);

    win.add(&page);
    win.present();
}

fn show_about(ui: &Rc<Ui>) {
    let formats: Vec<String> = registry::readable()
        .iter()
        .map(|i| format!("{} (.{})", i.name, i.extension))
        .collect();
    let about = adw::AboutWindow::builder()
        .transient_for(&ui.window)
        .application_name("CheapAzSLA")
        .application_icon("document-open-symbolic")
        .developer_name("CheapAzHobbies")
        .version(cheapazsla_core::VERSION)
        .comments(format!(
            "Resin print file converter and inspector.\n\nReadable formats: {}\n\nWriting is not implemented yet.",
            formats.join(", ")
        ))
        .website("https://github.com/CheapAzHobbies/CheapAzSLA")
        .license_type(gtk::License::Gpl30)
        .build();
    about.present();
}

/// Wire up the conversion controls.
fn wire_convert(
    ui: &Rc<Ui>,
    convert_btn: &gtk::Button,
    pick_dir: &gtk::Button,
    reset_dir: &gtk::Button,
) {
    {
        let ui = ui.clone();
        convert_btn.connect_clicked(move |_| start_convert(&ui));
    }
    {
        let ui = ui.clone();
        pick_dir.connect_clicked(move |_| {
            // Native folder picker: USB drives, SD cards and network shares
            // all appear through the desktop portal, so nothing is hardcoded.
            let dialog = gtk::FileDialog::builder()
                .title("Save converted files to")
                .modal(true)
                .build();
            let ui2 = ui.clone();
            dialog.select_folder(
                Some(&ui.window.clone()),
                gio::Cancellable::NONE,
                move |res| {
                    if let Ok(folder) = res {
                        if let Some(path) = folder.path() {
                            set_out_dir(&ui2, Some(path));
                        }
                    }
                },
            );
        });
    }
    {
        let ui = ui.clone();
        reset_dir.connect_clicked(move |_| set_out_dir(&ui, None));
    }
    {
        // Changing the output format re-suggests the filename, unless the user
        // has already typed something of their own.
        let ui = ui.clone();
        ui.format_row.clone().connect_selected_notify(move |_| {
            let current = ui.name_row.text().to_string();
            let untouched = with_loaded(|l| l.path.clone())
                .and_then(|src| {
                    let idx_ids = ui.writable.borrow();
                    idx_ids
                        .iter()
                        .filter_map(|id| convert::destination_for(&src, id, None))
                        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                        .find(|n| *n == current)
                        .map(|_| ())
                })
                .is_some();
            if current.is_empty() || untouched {
                suggest_name(&ui);
            }
        });
    }
}

/// Fill the filename box with a name derived from the source and the chosen
/// output format. Only the extension changes; the stem is preserved (§27).
fn suggest_name(ui: &Rc<Ui>) {
    let Some(source) = with_loaded(|l| l.path.clone()) else {
        return;
    };
    let idx = ui.format_row.selected() as usize;
    let Some(&format_id) = ui.writable.borrow().get(idx) else {
        return;
    };
    if let Some(p) = convert::destination_for(&source, format_id, None) {
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            ui.name_row.set_text(name);
        }
    }
}

fn set_out_dir(ui: &Rc<Ui>, dir: Option<PathBuf>) {
    match &dir {
        Some(d) => {
            let free = free_space(d)
                .map(|b| format!("  ·  {} free", render::human_bytes(b)))
                .unwrap_or_default();
            ui.dest_row.set_subtitle(&format!("{}{free}", d.display()));
        }
        None => ui.dest_row.set_subtitle("Beside the original"),
    }
    *ui.out_dir.borrow_mut() = dir;
}

/// Free space on the filesystem holding `dir`, when the OS will say.
fn free_space(dir: &Path) -> Option<u64> {
    let info = gio::File::for_path(dir)
        .query_filesystem_info("filesystem::free", gio::Cancellable::NONE)
        .ok()?;
    Some(info.attribute_uint64("filesystem::free"))
}

fn start_convert(ui: &Rc<Ui>) {
    let Some(source) = with_loaded(|l| l.path.clone()) else {
        return;
    };
    let idx = ui.format_row.selected() as usize;
    let Some(&format_id) = ui.writable.borrow().get(idx) else {
        return;
    };

    let out_dir = ui.out_dir.borrow().clone();
    let Some(generated) = convert::destination_for(&source, format_id, out_dir.as_deref()) else {
        ui.toasts
            .add_toast(adw::Toast::new("Could not work out a destination filename"));
        return;
    };
    // Whatever the user typed wins, but a name is required and it must stay a
    // filename rather than becoming a path.
    let typed = ui.name_row.text().trim().to_string();
    let desired = if typed.is_empty() || typed.contains('/') {
        if typed.contains('/') {
            ui.toasts
                .add_toast(adw::Toast::new("The file name cannot contain a slash"));
            return;
        }
        generated
    } else {
        let dir = generated
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        // Keep the extension matching the chosen format if the user dropped it.
        let want_ext = registry::by_id(format_id)
            .map(|h| h.info().extension)
            .unwrap_or("");
        let name = if Path::new(&typed)
            .extension()
            .map(|e| e.eq_ignore_ascii_case(want_ext))
            .unwrap_or(false)
        {
            typed
        } else {
            format!("{typed}.{want_ext}")
        };
        dir.join(name)
    };

    // Destination must exist and be writable before anything else (§ storage).
    let dir = desired.parent().unwrap_or(Path::new("."));
    if !dir.exists() {
        unavailable_dialog(ui, dir);
        return;
    }
    if !writable(dir) {
        let d = adw::MessageDialog::builder()
            .transient_for(&ui.window)
            .modal(true)
            .heading("Cannot write to this location")
            .body(format!(
                "CheapAzSLA does not have permission to save files in {}.\n\nChoose another location.",
                dir.display()
            ))
            .build();
        d.add_response("ok", "Close");
        d.present();
        return;
    }

    let plan = match convert::plan(&source, format_id, &desired) {
        Ok(p) => p,
        Err(e) => {
            error_dialog(ui, "This conversion is not possible.", &e.to_string());
            return;
        }
    };

    // Existing file: replace, keep both, or cancel (§27).
    if desired.exists() {
        let d = adw::MessageDialog::builder()
            .transient_for(&ui.window)
            .modal(true)
            .heading("File already exists")
            .body(format!("{} is already in that folder.", name_of(&desired)))
            .build();
        d.add_response("cancel", "Cancel");
        d.add_response("both", "Keep Both");
        d.add_response("replace", "Replace");
        d.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("both"));
        let ui2 = ui.clone();
        let plan2 = plan.clone();
        d.connect_response(None, move |dlg, resp| {
            dlg.close();
            let mut p = plan2.clone();
            match resp {
                "replace" => {}
                "both" => p.destination = convert::unique_path(&p.destination),
                _ => return,
            }
            confirm_losses_then_run(&ui2, p);
        });
        d.present();
        return;
    }

    confirm_losses_then_run(ui, plan);
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(".cheapazsla-write-test");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn unavailable_dialog(ui: &Rc<Ui>, dir: &Path) {
    let d = adw::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .heading("Output location unavailable")
        .body(format!(
            "{} is not there any more. If it was a removable drive, reconnect it or choose another location.",
            dir.display()
        ))
        .build();
    d.add_response("ok", "Close");
    d.present();
}

/// Show what the destination format cannot carry, then convert (§14, §29).
fn confirm_losses_then_run(ui: &Rc<Ui>, plan: convert::Plan) {
    // Lossless, or the user has said they do not want to be asked again.
    if plan.is_lossless() || !ui.settings.borrow().warn_on_information_loss {
        run_convert(ui, plan);
        return;
    }
    let body = plan
        .losses
        .iter()
        .map(|l| format!("• {}\n   {}", l.what, l.because))
        .collect::<Vec<_>>()
        .join("\n\n");
    let d = adw::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .heading("Some information cannot be preserved")
        .body(format!(
            "Converting {} to {} will drop:\n\n{body}",
            plan.from.name, plan.to.name
        ))
        .build();

    let dont_ask = gtk::CheckButton::with_label("Do not ask me again");
    dont_ask.set_tooltip_text(Some(
        "Future conversions will go ahead without this warning. \
         You can turn it back on in Settings.",
    ));
    dont_ask.set_margin_top(12);
    d.set_extra_child(Some(&dont_ask));

    d.add_response("cancel", "Cancel");
    d.add_response("go", "Convert Anyway");
    d.set_response_appearance("go", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("go"));
    let ui2 = ui.clone();
    let check = dont_ask.clone();
    d.connect_response(None, move |dlg, resp| {
        dlg.close();
        if resp != "go" {
            return;
        }
        // Only remember the choice when the user actually proceeded. Ticking
        // the box and then cancelling should not silence future warnings.
        if check.is_active() {
            let mut s = ui2.settings.borrow_mut();
            s.warn_on_information_loss = false;
            let _ = s.save();
        }
        run_convert(&ui2, plan.clone());
    });
    d.present();
}

fn run_convert(ui: &Rc<Ui>, plan: convert::Plan) {
    let format_id = plan.to.id;
    ui.convert_btn.set_sensitive(false);
    ui.progress.set_visible(true);
    ui.progress.set_fraction(0.0);
    ui.progress
        .set_text(Some(&format!("Preparing {} layers…", plan.layer_count)));

    // Progress arrives from the worker as (done, total). The channel is
    // unbounded and sent to without blocking, so reporting can never slow the
    // conversion down; dropping an update just means one fewer redraw.
    let (ptx, prx) = async_channel::unbounded::<(u32, u32)>();
    let (dtx, drx) = async_channel::bounded(1);

    let dest = plan.destination.clone();
    let total = plan.layer_count;
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let result = convert::run_with_progress(&plan, move |done, total| {
            let _ = ptx.try_send((done, total));
        })
        .map(|_| started.elapsed());
        let _ = dtx.send_blocking(result);
    });

    // Progress updates.
    {
        let ui = ui.clone();
        let started = std::time::Instant::now();
        glib::spawn_future_local(async move {
            while let Ok((done, total)) = prx.recv().await {
                if total == 0 {
                    continue;
                }
                let fraction = done as f64 / total as f64;
                ui.progress.set_fraction(fraction);
                let elapsed = started.elapsed().as_secs_f64();
                // Only estimate once there is enough signal to be worth
                // showing; an estimate from one layer is noise.
                let text = if done >= 3 && fraction > 0.0 {
                    let remaining = elapsed / fraction - elapsed;
                    format!(
                        "Layer {done} of {total}  ·  {:.0}%  ·  about {} left",
                        fraction * 100.0,
                        render::human_time(remaining.max(0.0).round() as u64)
                    )
                } else {
                    format!("Layer {done} of {total}  ·  {:.0}%", fraction * 100.0)
                };
                ui.progress.set_text(Some(&text));
            }
        });
    }

    // Completion.
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let outcome = drx.recv().await;
        ui.progress.set_visible(false);
        ui.convert_btn.set_sensitive(true);
        match outcome {
            Ok(Ok(elapsed)) => {
                {
                    let mut s = ui.settings.borrow_mut();
                    if let Some(parent) = dest.parent() {
                        s.remember_output_dir(parent);
                    }
                    s.last_output_format = Some(format_id.to_string());
                    let _ = s.save();
                }
                let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                let rate = if elapsed.as_secs_f32() > 0.0 {
                    format!(", {:.0} layers/s", total as f32 / elapsed.as_secs_f32())
                } else {
                    String::new()
                };
                let toast = adw::Toast::builder()
                    .title(format!(
                        "Converted to {} ({}, {:.1}s{rate})",
                        name_of(&dest),
                        render::human_bytes(size),
                        elapsed.as_secs_f32()
                    ))
                    .button_label("Open Folder")
                    .timeout(8)
                    .build();
                let d = dest.clone();
                toast.connect_button_clicked(move |_| {
                    if let Some(parent) = d.parent() {
                        let uri = gio::File::for_path(parent).uri();
                        let _ =
                            gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
                    }
                });
                ui.toasts.add_toast(toast);
            }
            Ok(Err(e)) => error_dialog(&ui, "The conversion failed.", &e.to_string()),
            Err(_) => {}
        }
    });
}
