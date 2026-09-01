//! CheapAzSLA desktop application.
//!
//! A thin shell over cheapazsla-core. All parsing, decoding, validation and
//! conversion happen in the engine; this crate decides what to show and when.
//!
//! Anything the engine cannot do is presented as unavailable rather than
//! mocked up (§47 of the product specification).

mod drives;
mod format_picker;
mod palette;
mod penguin;
mod render;
mod shell;
mod theme;
mod viewer;

use adw::prelude::*;
use cheapazsla_core::history::{self, History};
use cheapazsla_core::remedy::{self, Suggestion};
use cheapazsla_core::settings::Settings;
use cheapazsla_core::{convert, registry, OpenedFile};
use gtk::glib;
use gtk::{gdk, gio};
use shell::Section;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

const APP_ID: &str = "com.cheapazhobbies.CheapAzSLA";

/// How a queued file is doing (§15). Every state carries an icon and a word,
/// never colour alone.
#[derive(Clone, PartialEq)]
enum Status {
    Reading,
    Ready,
    Warning(String),
    Converting,
    Complete(PathBuf),
    Failed(String),
}

impl Status {
    fn chip(&self) -> gtk::Widget {
        match self {
            Status::Reading => {
                let b = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_1);
                let s = gtk::Spinner::new();
                s.start();
                b.append(&s);
                let l = gtk::Label::new(Some("Reading"));
                l.add_css_class("caption");
                l.add_css_class("cz-dim");
                b.append(&l);
                b.upcast()
            }
            Status::Ready => {
                shell::status_chip("object-select-symbolic", "Ready", "cz-ok").upcast()
            }
            Status::Warning(_) => {
                shell::status_chip("dialog-warning-symbolic", "Warning", "cz-warn").upcast()
            }
            Status::Converting => {
                let b = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_1);
                let s = gtk::Spinner::new();
                s.start();
                b.append(&s);
                let l = gtk::Label::new(Some("Converting"));
                l.add_css_class("caption");
                b.append(&l);
                b.upcast()
            }
            Status::Complete(_) => {
                shell::status_chip("object-select-symbolic", "Complete", "cz-ok").upcast()
            }
            Status::Failed(_) => {
                shell::status_chip("dialog-error-symbolic", "Failed", "cz-error").upcast()
            }
        }
    }

    /// Extra detail for a tooltip, when there is any.
    fn detail(&self) -> Option<String> {
        match self {
            Status::Warning(w) | Status::Failed(w) => Some(w.clone()),
            Status::Complete(p) => Some(format!("Saved to {}", p.display())),
            _ => None,
        }
    }
}

/// One file in the queue.
struct Queued {
    path: PathBuf,
    size: u64,
    format: String,
    detection: String,
    extension_mismatch: bool,
    warnings: Vec<String>,
    opened: Option<Arc<OpenedFile>>,
    status: Status,
    /// What the user can try, when something went wrong.
    suggestions: Vec<Suggestion>,
    /// Set when the user has said what this file is, overriding detection.
    forced_format: Option<String>,
}

impl Queued {
    fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// Everything the window owns.
struct App {
    window: adw::ApplicationWindow,
    shell: Rc<shell::Shell>,
    toasts: adw::ToastOverlay,

    // convert page
    dropzone: gtk::Box,
    dropzone_title: gtk::Label,
    queue_panel: gtk::Box,
    queue_list: gtk::ListBox,
    controls: gtk::Box,
    input_label: gtk::Label,
    /// Opens the list of formats the input can be read as.
    input_button: gtk::MenuButton,
    output_picker: Rc<format_picker::FormatPicker>,
    swap_btn: gtk::Button,
    dest_button: gtk::MenuButton,
    dest_label: gtk::Label,
    dest_detail: gtk::Label,
    name_entry: gtk::Entry,
    name_row: gtk::Box,
    convert_btn: gtk::Button,
    convert_label: gtk::Label,
    progress: gtk::ProgressBar,
    penguin: Rc<penguin::Penguin>,
    problem: gtk::Box,
    problem_label: gtk::Label,

    // preview page
    viewer: Rc<viewer::LayerViewer>,
    preview_stack: gtk::Stack,
    layer_label: gtk::Label,
    layer_detail: gtk::Box,
    slider: gtk::Scale,
    play_btn: gtk::Button,
    info_panel: gtk::Box,
    /// The information column beside the preview, dropped when narrow.
    preview_side: gtk::Widget,
    /// The preview split, whose padding shrinks before anything is dropped.
    preview_split: gtk::Box,
    /// Input and output side by side, stacked when there is no room for two.
    format_row: gtk::Box,
    /// The swap button between them, which stacking leaves nothing to mean.
    swap_col: gtk::Box,
    /// First and last layer buttons, dropped at the narrowest step.
    preview_nav_ends: Vec<gtk::Widget>,
    /// True while the window is narrow enough that columns are being dropped.
    compact: Cell<bool>,

    // history page
    history_list: gtk::ListBox,
    history_stack: gtk::Stack,

    // state
    files: RefCell<Vec<Queued>>,
    selected: RefCell<usize>,
    out_dir: RefCell<Option<PathBuf>>,
    settings: RefCell<Settings>,
    history: RefCell<History>,
    playing: RefCell<Option<glib::SourceId>>,
    /// Incremented on every layer request. A decode that finishes after a
    /// newer one was asked for is discarded rather than drawn.
    layer_request: Cell<u64>,
    /// Built layer textures, so revisiting a layer is immediate.
    textures: RefCell<std::collections::HashMap<u32, Drawn>>,
    texture_order: RefCell<std::collections::VecDeque<u32>>,
    /// Layers currently being decoded, so the same one is not queued twice.
    in_flight: RefCell<std::collections::HashSet<u32>>,
    /// Which way the user is scrubbing, so prefetching reads ahead rather
    /// than behind.
    last_layer: Cell<u32>,
    /// When the previous layer was asked for, so the speed of scrubbing can
    /// be judged.
    last_request_at: RefCell<Option<std::time::Instant>>,
    converting: RefCell<bool>,
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|_| {
        theme::install();
    });

    let pending: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));
    let p = pending.clone();
    app.connect_open(move |app, files, _| {
        p.borrow_mut().extend(files.iter().filter_map(|f| f.path()));
        app.activate();
    });

    let p = pending.clone();
    app.connect_activate(move |app| {
        let ui = build(app);
        ui.window.present();
        let queued: Vec<PathBuf> = p.borrow_mut().drain(..).collect();
        if !queued.is_empty() {
            add_files(&ui, queued);
        }
    });

    app.run()
}

fn build(app: &adw::Application) -> Rc<App> {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("CheapAzSLA")
        .default_width(1440)
        .default_height(900)
        // Quarter of a 1920 display, which was the target, and not a pixel
        // narrower on purpose.
        //
        // The rail folds over a third of a second while the window keeps
        // moving, so for a moment the layout holds a rail that is still open
        // beside a workspace that has already narrowed. Fully open that is 152
        // plus 323, and a window of 420 could not hold it: the right-hand edge
        // went off screen for the length of the fold. Clamping the rail to
        // what fits was the other way out and is worse — it drags the rail to
        // its folded width the instant the window is small, which is a jump
        // rather than a fold. 480 leaves room for the widest the layout is
        // ever transiently in, so nothing has to be clamped and nothing goes
        // off screen.
        .width_request(480)
        .height_request(440)
        .build();
    window.add_css_class("cheapazsla");

    let shell = shell::Shell::new();
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&shell.widget));

    // Without this the window has no titlebar, and therefore no minimise,
    // maximise or close. A window that can only be shut with a keyboard
    // shortcut or a kill is broken however good the rest of it looks.
    let header = adw::HeaderBar::builder()
        .show_title(false)
        .css_classes(["flat"])
        .build();
    let palette_btn = shell::icon_button("system-search-symbolic", "Commands  (Ctrl+K)");
    header.pack_start(&palette_btn);

    // --- convert page -----------------------------------------------------
    let (dropzone, dropzone_title) = build_dropzone();
    let queue_list = gtk::ListBox::new();
    queue_list.set_selection_mode(gtk::SelectionMode::Single);
    queue_list.add_css_class("cz-queue");
    let queue_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    queue_panel.add_css_class("cz-panel");
    queue_panel.append(&queue_list);
    let add_more = gtk::Button::builder()
        .label("Add Files")
        .halign(gtk::Align::Start)
        .build();
    add_more.add_css_class("flat");
    add_more.set_child(Some(&labelled_icon("list-add-symbolic", "Add Files")));
    queue_panel.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    queue_panel.append(&add_more);
    queue_panel.set_visible(false);

    // Detected, but overridable (§21). Detection reads the contents rather
    // than the name, which is right almost always and wrong occasionally: a
    // format can be a container another format also uses, and a file can be
    // truncated before the part that identifies it. When someone knows better
    // than the detector they need a way to say so, and the place they will
    // look is the box that told them what it thinks the file is.
    // Ellipsized, and asking for very little. A plain label's minimum width is
    // its whole text, and "(Automatically Detect)" beside the format name took
    // this control's minimum from about 80 pixels to 260 — enough to push the
    // whole page wider than a narrow window and leave its right-hand side off
    // screen.
    let input_label = gtk::Label::builder()
        .label("—")
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .width_chars(3)
        .build();
    input_label.add_css_class("cz-value");
    let input_inner = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
    input_inner.append(&input_label);
    input_inner.append(&gtk::Image::from_icon_name("pan-down-symbolic"));

    // Built exactly like the output control, because it is now the same kind
    // of thing. It was a box carrying the field styling with a flat button
    // inside it, from when the input could only be read: the hover highlight
    // then belonged to the button and stopped short of the box's border, and
    // the two controls were sized by different rules and came out different
    // widths. One button, one set of metrics.
    let input_button = gtk::MenuButton::builder()
        .child(&input_inner)
        .hexpand(true)
        .build();
    input_button.add_css_class("cz-format-control");
    input_button.set_valign(gtk::Align::Center);
    input_button.set_tooltip_text(Some(
        "Detected from the file's contents. Click to read it as something else",
    ));
    let input_field = input_button.clone();
    let output_picker = format_picker::FormatPicker::new(format_picker::Direction::Write);
    let swap_btn = shell::icon_button("media-playlist-repeat-symbolic", "Swap formats");
    let output_info = format_picker::info_button({
        let picker = output_picker.clone();
        move || {
            picker
                .selected()
                .and_then(registry::by_id)
                .map(|h| h.info())
        }
    });

    // Ellipsized: a destination path is arbitrarily long and there is no
    // width at which it should be what stops the window narrowing.
    let dest_label = gtk::Label::builder()
        .label("Beside the original")
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let dest_detail = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .build();
    dest_detail.add_css_class("caption");
    dest_detail.add_css_class("cz-dim");
    let dest_inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
    dest_inner.append(&dest_label);
    dest_inner.append(&dest_detail);
    dest_inner.set_hexpand(true);
    let dest_content = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
    dest_content.append(&gtk::Image::from_icon_name("folder-symbolic"));
    dest_content.append(&dest_inner);
    dest_content.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
    let dest_button = gtk::MenuButton::builder().child(&dest_content).build();
    dest_button.set_tooltip_text(Some("Where converted files are saved"));

    let name_entry = gtk::Entry::builder().hexpand(true).build();
    name_entry.set_tooltip_text(Some("Name of the converted file"));

    let convert_label = gtk::Label::new(Some("Convert"));
    let convert_btn = gtk::Button::builder().child(&convert_label).build();
    convert_btn.add_css_class("cz-primary");
    convert_btn.set_sensitive(false);
    convert_btn.set_tooltip_text(Some("Convert  (Ctrl+Enter)"));

    let progress = gtk::ProgressBar::builder()
        .show_text(true)
        .visible(false)
        .build();
    let penguin = penguin::Penguin::new(76);

    // Problems are stated beside the control rather than in a modal (§30).
    let (problem, problem_label) = build_problem_bar();

    let convert_page = build_convert_page(
        &dropzone,
        &queue_panel,
        &input_field,
        &output_picker,
        &swap_btn,
        &output_info,
        &dest_button,
        &name_entry,
        &convert_btn,
        &progress,
        &penguin,
        &problem,
    );
    let controls = convert_page.1;
    let name_row = convert_page.2;
    let format_row = convert_page.3;
    let swap_col = convert_page.4;
    shell.add_page(Section::Convert, &convert_page.0);

    // --- preview page -----------------------------------------------------
    let viewer = viewer::LayerViewer::new();
    let layer_label = gtk::Label::builder().label("Layer — / —").build();
    layer_label.add_css_class("heading");
    // Tabular figures, so the number does not jitter as digits change.
    layer_label.add_css_class("cz-value");
    let layer_detail = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_1);
    let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
    slider.set_hexpand(true);
    slider.set_draw_value(false);
    // With the caption gone the slider has the row almost to itself. Scrubbing
    // a thousand-layer print is a job for a long control: every pixel of width
    // is roughly two layers of precision.
    slider.set_size_request(360, -1);
    // Clicking anywhere jumps straight there rather than stepping one page,
    // which on a long print means several clicks to reach where you pointed.
    slider.set_round_digits(0);
    slider.set_increments(1.0, 10.0);
    let play_btn = shell::icon_button("media-playback-start-symbolic", "Play  (Space)");
    let info_panel = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_2);
    let preview = build_preview_page(
        &viewer,
        &layer_label,
        &slider,
        &play_btn,
        &info_panel,
        &layer_detail,
    );
    let PreviewChrome {
        page: preview_page,
        stack: preview_stack,
        side: preview_side,
        split: preview_split,
        nav_ends: preview_nav_ends,
    } = preview;
    shell.add_page(Section::Preview, &preview_page);

    // --- history page -----------------------------------------------------
    let history_list = gtk::ListBox::new();
    history_list.set_selection_mode(gtk::SelectionMode::None);
    history_list.add_css_class("cz-queue");
    let (history_page, history_stack) = build_history_page(&history_list);
    shell.add_page(Section::History, &history_page);

    // --- settings page ----------------------------------------------------
    let settings_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shell.add_page(Section::Settings, &settings_page);

    let root = adw::ToolbarView::new();
    root.add_top_bar(&header);
    root.set_content(Some(&toasts));
    window.set_content(Some(&root));

    let ui = Rc::new(App {
        window: window.clone(),
        shell: shell.clone(),
        toasts,
        dropzone,
        dropzone_title,
        queue_panel,
        queue_list,
        controls,
        input_label,
        input_button: input_button.clone(),
        output_picker: output_picker.clone(),
        swap_btn: swap_btn.clone(),
        dest_button: dest_button.clone(),
        dest_label,
        dest_detail,
        name_entry: name_entry.clone(),
        name_row,
        convert_btn: convert_btn.clone(),
        convert_label,
        progress,
        penguin,
        problem,
        problem_label,
        viewer: viewer.clone(),
        preview_stack,
        layer_label,
        layer_detail,
        slider: slider.clone(),
        play_btn: play_btn.clone(),
        info_panel,
        preview_side,
        preview_split,
        format_row,
        swap_col,
        preview_nav_ends,
        compact: Cell::new(false),
        history_list,
        history_stack,
        files: RefCell::new(Vec::new()),
        selected: RefCell::new(0),
        out_dir: RefCell::new(None),
        settings: RefCell::new(Settings::load()),
        history: RefCell::new(History::load()),
        playing: RefCell::new(None),
        layer_request: Cell::new(0),
        textures: RefCell::new(std::collections::HashMap::new()),
        texture_order: RefCell::new(std::collections::VecDeque::new()),
        in_flight: RefCell::new(std::collections::HashSet::new()),
        last_layer: Cell::new(0),
        last_request_at: RefCell::new(None),
        converting: RefCell::new(false),
    });

    {
        let ui2 = ui.clone();
        palette_btn.connect_clicked(move |_| palette::show(&ui2));
    }
    build_settings_page(&ui, &settings_page);
    wire(&ui, &add_more);
    {
        let window = ui.window.clone();
        ui.shell.connect_about(move || show_about(&window));
    }
    ui.shell.set_animate(ui.settings.borrow().animations);
    wire_responsive(&ui);
    restore_session(&ui);
    refresh_history(&ui);
    ui
}

/// Print the smallest size each part of the window will accept.
///
/// A stack takes the largest minimum of every page it holds, shown or not, so
/// one wide page holds the whole window open. Walking the tree is the only way
/// to find which.
/// Walk the window in and back out, reporting the sidebar each step.
///
/// Set `CHEAPAZSLA_DEBUG_FOLD=1`. Whether the rail folds smoothly cannot be
/// seen from a still, and it behaves differently when a resize drives it than
/// when it is toggled on its own — three separate bugs only appeared under a
/// real resize, so this drives one. Numbers should descend and climb evenly;
/// a repeated value is a stall and a large gap is a jump.
fn debug_fold(window: &adw::ApplicationWindow) {
    let window = window.clone();
    window.clone().connect_map(move |_| {
        let w = window.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
            // Unmaximising only takes effect on a later frame, so the walk has
            // to wait for it.
            w.unmaximize();
            let w = w.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                for step in 0..84u64 {
                    let w = w.clone();
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(400 + 30 * step),
                        move || {
                            // Three phases. Walk in slowly; settle back out
                            // and walk in seven times as fast, since the fold
                            // behaves differently when the window outruns it;
                            // then cross the step back and forth every few
                            // frames, which is what catches a fold that
                            // restarts instead of reversing.
                            let width = if step < 30 {
                                900 - (step as i32 * 6)
                            } else if step < 44 {
                                830
                            } else if step < 52 {
                                (830 - ((step as i32 - 44) * 140)).max(410)
                            } else if (step - 52) % 8 < 4 {
                                830
                            } else {
                                700
                            };
                            w.set_default_size(width, 700);
                            let (rail, brand, label) = fold_parts(&w);
                            // The content's own minimum beside the width it
                            // has been given: anything larger is off screen.
                            let need = content_minimum(&w);
                            eprintln!(
                                "fold {width} got {} needs {need}{} rail {rail} icon_x {brand} \
                                 label_x {label}",
                                w.width(),
                                if need > w.width() { "  OVERFLOWS" } else { "" }
                            );
                            if need > w.width() && w.width() < 500 {
                                report_minimums(&w);
                            }
                        },
                    );
                }
            });
        });
    });
}

/// The narrowest the window's contents will fit into.
///
/// Measured on the toolbar view rather than the window: since the window
/// gained a breakpoint it reports its own width request as its minimum, which
/// says nothing about whether the contents fit inside it.
fn content_minimum(window: &adw::ApplicationWindow) -> i32 {
    fn find(w: &gtk::Widget) -> Option<gtk::Widget> {
        if w.type_().name() == "AdwToolbarView" {
            return Some(w.clone());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(f) = find(&c) {
                return Some(f);
            }
            child = c.next_sibling();
        }
        None
    }
    find(&window.clone().upcast())
        .map(|v| v.measure(gtk::Orientation::Horizontal, -1).0)
        .unwrap_or(-1)
}

/// The rail's drawn width, and where the first navigation row's icon and
/// label sit inside it.
///
/// The icon's position is the useful one: it should never move. When it does,
/// the whole rail is sliding rather than the labels animating within it.
fn fold_parts(window: &adw::ApplicationWindow) -> (i32, i32, i32) {
    fn find(w: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
        if w.has_css_class(class) {
            return Some(w.clone());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(f) = find(&c, class) {
                return Some(f);
            }
            child = c.next_sibling();
        }
        None
    }
    let Some(rail) = find(&window.clone().upcast(), "cz-sidebar") else {
        return (-1, -1, -1);
    };
    // ScrolledWindow -> Viewport -> the box holding the rail's contents.
    let content = rail
        .first_child()
        .and_then(|vp| vp.first_child())
        .unwrap_or_else(|| rail.clone());
    // The label inside the revealer, not the revealer: a label that is being
    // re-ellipsized as its box grows shifts around inside it, which reads as
    // the text rubber-banding into place.
    let label = content
        .first_child()
        .and_then(|b| b.next_sibling())
        .and_then(|btn| btn.first_child())
        .and_then(|row| row.last_child())
        .and_then(|rev| rev.first_child())
        .and_then(|l| l.compute_bounds(&rail))
        .map(|b| b.x().round() as i32)
        .unwrap_or(-1);
    // The icon's position too: if it moves, the whole rail is scrolling
    // rather than the label sliding inside its row.
    let icon = content
        .first_child()
        .and_then(|b| b.next_sibling())
        .and_then(|btn| btn.first_child())
        .and_then(|row| row.first_child())
        .and_then(|marker| marker.next_sibling())
        .and_then(|img| img.compute_bounds(&rail))
        .map(|b| b.x().round() as i32)
        .unwrap_or(-1);
    (rail.width(), icon, label)
}

/// The guide and the project's details, from the button at the foot of the rail.
///
/// The guide itself is deliberately empty: the interface it would describe is
/// about to be reworked, and a walkthrough of the old one would be worse than
/// none. The section says so rather than pretending to be a page that failed
/// to load.
fn show_about(parent: &adw::ApplicationWindow) {
    let page = adw::PreferencesPage::new();

    let guide = adw::PreferencesGroup::builder()
        .title("How to use CheapAzSLA")
        .description("A step-by-step guide will live here.")
        .build();
    let placeholder = gtk::Label::builder()
        .label("Not written yet — the interface it describes is being reworked.")
        .wrap(true)
        .xalign(0.0)
        .build();
    placeholder.add_css_class("cz-dim");
    placeholder.add_css_class("caption");
    placeholder.set_margin_top(theme::SPACE_2);
    guide.add(&placeholder);
    page.add(&guide);

    let about = adw::PreferencesGroup::builder().title("About").build();
    about.add(
        &adw::ActionRow::builder()
            .title("Version")
            .subtitle(cheapazsla_core::VERSION)
            .build(),
    );
    for (title, subtitle, url) in [
        (
            "Project on GitHub",
            "github.com/CheapAzHobbies/CheapAzPrintingSLA",
            "https://github.com/CheapAzHobbies/CheapAzPrintingSLA",
        ),
        (
            "CheapAzHobbies on GitHub",
            "github.com/CheapAzHobbies",
            "https://github.com/CheapAzHobbies",
        ),
    ] {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
        row.connect_activated(move |_| {
            let _ = gio::AppInfo::launch_default_for_uri(url, gio::AppLaunchContext::NONE);
        });
        about.add(&row);
    }
    page.add(&about);

    let header = adw::HeaderBar::new();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));

    let window = adw::Window::builder()
        .title("About CheapAzSLA")
        .transient_for(parent)
        .modal(true)
        .default_width(460)
        .default_height(420)
        .content(&view)
        .build();
    window.add_css_class("cheapazsla");
    window.present();
}

/// Report what is holding the window open, widest branch first.
///
/// Set `CHEAPAZSLA_DEBUG_SIZE=1` to print it a moment after the window opens.
/// The layout has deadlocked at a too-large minimum more than once, always
/// because of one widget nobody suspected, and guessing at which is slower
/// than measuring. libadwaita's own "exceeds AdwApplicationWindow width"
/// warning says that it happened; this says what did it.
fn report_minimums(window: &adw::ApplicationWindow) {
    fn walk(w: &gtk::Widget, depth: usize) {
        let (min_w, _, _, _) = w.measure(gtk::Orientation::Horizontal, -1);
        if min_w >= 120 {
            let classes = w.css_classes().join(".");
            eprintln!(
                "{:indent$}{:<44} min {min_w} got {}",
                "",
                format!("{} .{classes}", w.type_().name()),
                w.width(),
                indent = depth * 2
            );
        }
        if depth > 20 {
            return;
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            walk(&c, depth + 1);
            child = c.next_sibling();
        }
    }
    let root = window.clone().upcast::<gtk::Widget>();
    let (min, _, _, _) = root.measure(gtk::Orientation::Horizontal, -1);
    eprintln!("window minimum width: {min}, currently {}", window.width());
    walk(&root, 0);
}

/// Let the window narrow instead of refusing to (§25).
///
/// A GtkWindow will not be resized below the minimum width its contents ask
/// for, and the contents cannot be told to shrink until the window has
/// narrowed, so on its own the layout deadlocks at whatever its widest row
/// happens to need — here 1238px, which is more than half of a 1920 display.
/// An AdwBreakpoint breaks that: once a window has one, libadwaita stops
/// passing the child minimum up, the window honours its own width request
/// instead, and the breakpoint tells us when to put the layout into a state
/// that actually fits the width we have been given.
///
/// Three states. Wide shows everything. Below the first step the information
/// column beside the image goes, since the image is the point and the same
/// numbers are on the Convert page. Below the second the sidebar keeps its
/// icons and loses its labels, the layer scale gives up its fixed width, and
/// the first/last buttons go — they are the two of the five that a keyboard
/// Home and End already cover.
fn wire_responsive(ui: &Rc<App>) {
    // Each threshold sits above the width the layout actually needs in the
    // state below it, measured rather than guessed: the full sidebar and a
    // full-padding page fit down to about 630, the icon rail and tight
    // padding to about 460, and stacked columns to about 380.
    const WIDE_BELOW: &str = "max-width: 1140px";
    const NARROW_BELOW: &str = "max-width: 760px";
    const STACKED_BELOW: &str = "max-width: 480px";

    let apply = {
        let ui = ui.clone();
        Rc::new(move |level: u8| {
            if std::env::var_os("CHEAPAZSLA_DEBUG_FOLD").is_some() {
                eprintln!("apply level={level} window={}", ui.window.width());
            }
            ui.preview_side.set_visible(level == 0);
            let pad = match level {
                0 => theme::SPACE_6,
                1 => theme::SPACE_4,
                _ => theme::SPACE_3,
            };
            // Sides only: the top and bottom stay put, so crossing a step
            // does not shift everything on the page up or down.
            ui.preview_split.set_margin_start(pad);
            ui.preview_split.set_margin_end(pad);

            let narrow = level >= 2;
            let stacked = level >= 3;
            ui.format_row.set_orientation(if stacked {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            });
            // Stacked, an arrow between two things that are now above and
            // below each other says nothing, and the swap is still there at
            // any width with room for the two columns.
            ui.swap_col.set_visible(!stacked);
            if narrow {
                ui.window.add_css_class("compact");
            } else {
                ui.window.remove_css_class("compact");
            }
            ui.shell.set_compact(narrow);
            // Everything with a fixed width gives it up before anything can
            // overlap, so the window can be tiled rather than refusing to
            // shrink past whatever its widest row happens to need.
            // A minimum, not a width: the scale expands into whatever the row
            // has left, so a smaller floor costs nothing at full size and is
            // what lets the window keep narrowing.
            ui.slider.set_size_request(
                match level {
                    0 => 240,
                    1 => 160,
                    _ => 80,
                },
                -1,
            );
            refresh_input_label(&ui);
            let chars = if narrow { 8 } else { 14 };
            ui.layer_label.set_width_chars(chars);
            ui.layer_label.set_max_width_chars(chars);
            for b in &ui.preview_nav_ends {
                b.set_visible(!narrow);
            }
            if ui.compact.get() != narrow {
                ui.compact.set(narrow);
                if !ui.files.borrow().is_empty() {
                    refresh_queue(&ui);
                }
            }
        })
    };
    apply(0);

    // Each breakpoint records whether it is currently in force, and the level
    // is the deepest one that is. Reading them that way makes the order the
    // signals arrive in irrelevant: going from one step to the next libadwaita
    // unapplies the old and applies the new, either can come first, and the
    // deepest flag set is the right answer whichever does.
    //
    // Deferred to an idle rather than applied in the signal. Applying it there
    // changes size requests while a breakpoint is being evaluated, and the
    // breakpoints then oscillate — the trace reads apply, unapply, apply,
    // unapply for as long as the drag lasts. The lateness that costs is dealt
    // with in the fold itself, which will not let the rail be wider than the
    // window can fit.
    let hit = Rc::new([Cell::new(false), Cell::new(false), Cell::new(false)]);
    let pending = Rc::new(Cell::new(false));
    for (index, condition) in [WIDE_BELOW, NARROW_BELOW, STACKED_BELOW]
        .into_iter()
        .enumerate()
    {
        let bp = adw::Breakpoint::new(
            adw::BreakpointCondition::parse(condition).expect("breakpoint condition"),
        );
        for state in [true, false] {
            let (hit, apply, pending) = (hit.clone(), apply.clone(), pending.clone());
            let ui_for_aim = ui.clone();
            let handler = move |_: &adw::Breakpoint| {
                hit[index].set(state);
                let level = hit.iter().rposition(|h| h.get()).map_or(0, |i| i as u8 + 1);
                // Aim the rail now and do the rest on the idle: a fast drag is
                // several frames past the step by the time an idle runs, and
                // those are the frames the fold should have started in.
                ui_for_aim.shell.aim(level >= 2);
                if pending.replace(true) {
                    return;
                }
                let (hit, apply, pending) = (hit.clone(), apply.clone(), pending.clone());
                glib::idle_add_local_once(move || {
                    pending.set(false);
                    let level = hit.iter().rposition(|h| h.get()).map_or(0, |i| i as u8 + 1);
                    apply(level);
                });
            };
            if state {
                bp.connect_apply(handler);
            } else {
                bp.connect_unapply(handler);
            }
        }
        ui.window.add_breakpoint(bp);
    }

    // "1" drives a scripted resize; anything else just records what a real
    // drag does, which is the only way to see the faults a script cannot
    // provoke.
    if std::env::var_os("CHEAPAZSLA_DEBUG_FOLD").is_some_and(|v| v == "1") {
        debug_fold(&ui.window);
    }
    if let Some(path) = std::env::var_os("CHEAPAZSLA_DEBUG_SHOT") {
        // Render the window to a file and quit, so a layout can be looked at
        // rather than reasoned about.
        let window = ui.window.clone();
        window.clone().connect_map(move |_| {
            let w = window.clone();
            let path = path.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                let Some(renderer) = w.native().and_then(|n| n.renderer()) else {
                    return;
                };
                let paintable = gtk::WidgetPaintable::new(Some(&w));
                let snapshot = gtk::Snapshot::new();
                paintable.snapshot(&snapshot, w.width() as f64, w.height() as f64);
                if let Some(node) = snapshot.to_node() {
                    let _ = renderer
                        .render_texture(&node, None)
                        .save_to_png(path.to_string_lossy().as_ref());
                }
                w.close();
            });
        });
    }
    if std::env::var_os("CHEAPAZSLA_DEBUG_SIZE").is_some() {
        let window = ui.window.clone();
        window.clone().connect_map(move |_| {
            let w = window.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
                report_minimums(&w)
            });
        });
    }
}

/// An icon and a label side by side, for buttons that deserve both (§6).
fn labelled_icon(icon: &str, text: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
    b.set_margin_top(theme::SPACE_2);
    b.set_margin_bottom(theme::SPACE_2);
    b.set_margin_start(theme::SPACE_3);
    b.set_margin_end(theme::SPACE_3);
    b.append(&gtk::Image::from_icon_name(icon));
    b.append(&gtk::Label::new(Some(text)));
    b
}

fn build_dropzone() -> (gtk::Box, gtk::Label) {
    let zone = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_3);
    zone.add_css_class("cz-dropzone");
    zone.set_valign(gtk::Align::Center);
    zone.set_halign(gtk::Align::Fill);
    zone.set_margin_top(theme::SPACE_5);
    zone.set_margin_bottom(theme::SPACE_5);
    zone.set_size_request(-1, 240);

    let icon = gtk::Image::from_icon_name("document-open-symbolic");
    icon.set_pixel_size(48);
    icon.add_css_class("cz-dim");
    icon.set_margin_top(theme::SPACE_5);

    let title = gtk::Label::new(Some("Drop files here"));
    title.add_css_class("cz-title");
    title.set_wrap(true);
    title.set_justify(gtk::Justification::Center);
    title.set_max_width_chars(38);

    let sub = gtk::Label::new(Some("or browse your computer"));
    sub.add_css_class("cz-subtitle");
    sub.set_wrap(true);
    sub.set_justify(gtk::Justification::Center);
    sub.set_max_width_chars(38);

    let browse = gtk::Button::with_label("Browse Files");
    browse.set_halign(gtk::Align::Center);
    browse.add_css_class("pill");
    browse.set_margin_bottom(theme::SPACE_5);
    browse.set_widget_name("dropzone-browse");

    // One format per line rather than a row of names separated by dots. There
    // were two when that was written and there are four now, and a run-on list
    // of extensions is harder to read than a short column of them. The line
    // has room for the format's real name as well, which the dots never did.
    let formats = gtk::Box::new(gtk::Orientation::Vertical, 0);
    formats.set_halign(gtk::Align::Center);
    let opens = gtk::Label::new(Some("Opens"));
    opens.add_css_class("caption");
    opens.add_css_class("cz-dim");
    opens.set_margin_bottom(theme::SPACE_1);
    formats.append(&opens);

    // A grid rather than one label per line, so the names end and the
    // extensions begin in the same place down the column. Centred lines of
    // differing length leave both edges ragged, which is the sort of thing
    // that reads as untidy without it being obvious why.
    let grid = gtk::Grid::new();
    grid.set_column_spacing(theme::SPACE_2 as u32);
    grid.set_halign(gtk::Align::Center);
    for (row, info) in registry::readable().iter().enumerate() {
        let name = gtk::Label::builder().label(info.name).xalign(1.0).build();
        let ext = gtk::Label::builder()
            .label(format!(".{}", info.extension))
            .xalign(0.0)
            .build();
        for l in [&name, &ext] {
            l.add_css_class("caption");
            l.add_css_class("cz-dim");
        }
        grid.attach(&name, 0, row as i32, 1, 1);
        grid.attach(&ext, 1, row as i32, 1, 1);
    }
    formats.append(&grid);

    zone.append(&icon);
    zone.append(&title);
    zone.append(&sub);
    zone.append(&browse);
    zone.append(&formats);
    (zone, title)
}

fn build_problem_bar() -> (gtk::Box, gtk::Label) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
    bar.set_visible(false);
    let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
    icon.add_css_class("cz-warn");
    let label = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    label.add_css_class("cz-warn");
    label.add_css_class("caption");
    bar.append(&icon);
    bar.append(&label);
    (bar, label)
}

/// A page with a heading, a subtitle and content, at a readable measure.
fn page_frame(title: &str, subtitle: &str, content: &impl IsA<gtk::Widget>) -> gtk::Widget {
    let head = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_1);
    let t = gtk::Label::builder().label(title).xalign(0.0).build();
    t.add_css_class("cz-title");
    t.set_wrap(true);
    t.set_justify(gtk::Justification::Center);
    t.set_max_width_chars(38);
    let s = gtk::Label::builder().label(subtitle).xalign(0.0).build();
    s.add_css_class("cz-subtitle");
    s.set_wrap(true);
    s.set_justify(gtk::Justification::Center);
    s.set_max_width_chars(38);
    head.append(&t);
    head.append(&s);
    head.set_margin_bottom(theme::SPACE_5);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&head);
    body.append(content);
    // Padding in CSS rather than margins in code, so the narrow state is one
    // class on the window instead of a handle to every page.
    body.add_css_class("cz-page-body");

    // Clamped so text never runs to an uncomfortable measure on a wide screen,
    // while the workspace still takes the extra width (§25).
    let clamp = adw::Clamp::builder()
        .maximum_size(900)
        .tightening_threshold(700)
        .child(&body)
        .build();
    // Automatic rather than Never. Never makes the content's minimum width the
    // window's minimum width, which is what stopped the window being tiled to
    // a quarter of the screen: the widest row in the page set a floor under
    // the whole application. Automatic lets the page scroll instead, which is
    // a safety valve that should rarely be reached now the columns give way.
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_width(true)
        .child(&clamp)
        .build()
        .upcast()
}

#[allow(clippy::too_many_arguments)]
fn build_convert_page(
    dropzone: &gtk::Box,
    queue_panel: &gtk::Box,
    input_field: &gtk::MenuButton,
    output_picker: &Rc<format_picker::FormatPicker>,
    swap_btn: &gtk::Button,
    output_info: &gtk::MenuButton,
    dest_button: &gtk::MenuButton,
    name_entry: &gtk::Entry,
    convert_btn: &gtk::Button,
    progress: &gtk::ProgressBar,
    penguin: &Rc<penguin::Penguin>,
    problem: &gtk::Box,
) -> (gtk::Widget, gtk::Box, gtk::Box, gtk::Box, gtk::Box) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_4);
    content.append(dropzone);
    content.append(queue_panel);

    // Controls stay hidden until there is a file, so a new user sees one
    // instruction rather than a form (§2, §36).
    let controls = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_4);
    controls.set_visible(false);

    // INPUT  ⇄  OUTPUT
    let formats = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_4);
    formats.set_homogeneous(false);

    // Both headers are a row rather than a bare label, and both are held to
    // the same height by a size group. The information button used to sit
    // beside the dropdown instead, because putting it in the header made that
    // header taller than the plain label opposite it and the two columns no
    // longer lined up. A size group settles that properly: the button can go
    // where it belongs, next to the thing it explains.
    let headers = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);

    let in_header = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
    in_header.append(&shell::section_label("Input"));
    headers.add_widget(&in_header);

    let in_col = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_1);
    in_col.set_hexpand(true);
    in_col.set_valign(gtk::Align::Start);
    in_col.append(&in_header);
    in_col.append(input_field);

    // An empty header of its own, held to the same height as the other two by
    // the size group, so the button below it starts level with the controls
    // rather than at a guessed offset. It used to be a bare label, which
    // matched only while both headers were bare labels too.
    let swap_col = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_1);
    swap_col.set_valign(gtk::Align::Start);
    swap_col.set_halign(gtk::Align::Center);
    let swap_header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    swap_header.append(&shell::section_label(""));
    headers.add_widget(&swap_header);
    swap_col.append(&swap_header);
    swap_btn.set_valign(gtk::Align::Center);
    swap_btn.set_size_request(34, 34);
    swap_col.append(swap_btn);

    // And the three controls share a height, so the row reads as one line
    // rather than three things that happen to be near each other.
    let controls_height = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
    controls_height.add_widget(input_field);
    controls_height.add_widget(&output_picker.button);
    controls_height.add_widget(swap_btn);

    let out_header = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
    out_header.append(&shell::section_label("Output"));
    output_info.set_valign(gtk::Align::Center);
    output_info.set_halign(gtk::Align::Start);
    out_header.append(output_info);
    headers.add_widget(&out_header);

    let out_col = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_1);
    out_col.set_hexpand(true);
    out_col.set_valign(gtk::Align::Start);
    out_col.append(&out_header);

    // The dropdown now has the row to itself, so it is the same shape as the
    // input control opposite it.
    output_picker.button.set_hexpand(true);
    out_col.append(&output_picker.button);

    // hexpand alone divides the leftover space, which is not the same as
    // making the two columns equal when their contents differ in width.
    let equal = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    equal.add_widget(&in_col);
    equal.add_widget(&out_col);

    formats.append(&in_col);
    formats.append(&swap_col);
    formats.append(&out_col);
    controls.append(&formats);
    // Side by side needs both columns' width at once. Narrow, they stack.
    formats.set_widget_name("format-row");

    // Destination and filename.
    let dest_col = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_1);
    dest_col.append(&shell::section_label("Save to"));
    dest_col.append(dest_button);
    controls.append(&dest_col);

    let name_row = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_1);
    name_row.append(&shell::section_label("Save as"));
    name_row.append(name_entry);
    controls.append(&name_row);

    controls.append(problem);

    let action = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_2);
    action.append(convert_btn);
    if penguin.is_available() {
        action.append(&penguin.widget);
    }
    action.append(progress);
    controls.append(&action);

    content.append(&controls);
    (
        page_frame(
            "Convert",
            "Open a file from your slicer, save it in your printer's format.",
            &content,
        ),
        controls,
        name_row,
        formats,
        swap_col,
    )
}

/// The parts of the preview page that have to give way as the window narrows.
struct PreviewChrome {
    page: gtk::Widget,
    stack: gtk::Stack,
    /// The information column beside the image.
    side: gtk::Widget,
    /// The padding around the split, which is the cheapest width to give up.
    split: gtk::Box,
    /// First and last layer buttons, the two least used of the five.
    nav_ends: Vec<gtk::Widget>,
}

fn build_preview_page(
    viewer: &Rc<viewer::LayerViewer>,
    layer_label: &gtk::Label,
    slider: &gtk::Scale,
    play_btn: &gtk::Button,
    info_panel: &gtk::Box,
    layer_detail: &gtk::Box,
) -> PreviewChrome {
    // Only the layer number sits beside the slider, in a cell wide enough for
    // the largest count it will ever show. Anything whose width follows its
    // content cannot share a row with the widget that expands.
    layer_label.set_xalign(1.0);
    layer_label.set_width_chars(14);
    layer_label.set_max_width_chars(14);
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 0);
    labels.set_hexpand(false);
    labels.set_halign(gtk::Align::End);
    labels.set_valign(gtk::Align::Center);
    labels.append(layer_label);

    let nav = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_1);
    let first = shell::icon_button("go-first-symbolic", "First layer  (Home)");
    let last = shell::icon_button("go-last-symbolic", "Last layer  (End)");
    nav.append(&first);
    nav.append(&shell::icon_button(
        "go-previous-symbolic",
        "Previous layer  (Left)",
    ));
    nav.append(play_btn);
    nav.append(&shell::icon_button(
        "go-next-symbolic",
        "Next layer  (Right)",
    ));
    nav.append(&last);
    let nav_ends = vec![first.upcast::<gtk::Widget>(), last.upcast::<gtk::Widget>()];

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
    bar.set_margin_top(theme::SPACE_3);
    bar.append(&nav);
    bar.append(slider);
    bar.append(&labels);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&viewer.widget);
    column.append(&bar);
    column.set_hexpand(true);
    column.set_vexpand(true);

    let side = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_3);
    side.set_size_request(260, -1);
    side.append(&shell::section_label("File information"));
    side.append(info_panel);
    let layer_head = shell::section_label("This layer");
    layer_head.set_margin_top(theme::SPACE_4);
    side.append(&layer_head);
    side.append(layer_detail);
    let side_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(false)
        .child(&side)
        .build();

    let split = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_5);
    split.append(&column);
    split.append(&side_scroll);
    split.set_hexpand(true);
    split.set_vexpand(true);
    side_scroll.set_hexpand(false);
    split.set_margin_top(theme::SPACE_5);
    split.set_margin_bottom(theme::SPACE_5);
    split.set_margin_start(theme::SPACE_6);
    split.set_margin_end(theme::SPACE_6);

    let empty = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_3);
    empty.set_valign(gtk::Align::Center);
    empty.set_halign(gtk::Align::Center);
    let icon = gtk::Image::from_icon_name("view-reveal-symbolic");
    icon.set_pixel_size(48);
    icon.add_css_class("cz-dim");
    let t = gtk::Label::new(Some("Nothing to preview yet"));
    t.add_css_class("cz-title");
    t.set_wrap(true);
    t.set_justify(gtk::Justification::Center);
    t.set_max_width_chars(38);
    let s = gtk::Label::new(Some(
        "Add a file on the Convert page to look through its layers.",
    ));
    s.add_css_class("cz-subtitle");
    s.set_wrap(true);
    s.set_justify(gtk::Justification::Center);
    s.set_max_width_chars(38);
    empty.append(&icon);
    empty.append(&t);
    empty.append(&s);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(160)
        .build();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&split, Some("view"));
    stack.set_visible_child_name("empty");
    PreviewChrome {
        page: stack.clone().upcast(),
        stack,
        side: side_scroll.upcast(),
        split,
        nav_ends,
    }
}

fn build_history_page(list: &gtk::ListBox) -> (gtk::Widget, gtk::Stack) {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("cz-panel");
    panel.append(list);

    let empty = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_3);
    empty.set_valign(gtk::Align::Center);
    empty.set_halign(gtk::Align::Center);
    empty.set_margin_top(theme::SPACE_6);
    let icon = gtk::Image::from_icon_name("document-open-recent-symbolic");
    icon.set_pixel_size(48);
    icon.add_css_class("cz-dim");
    let t = gtk::Label::new(Some("No conversions yet"));
    t.add_css_class("cz-title");
    t.set_wrap(true);
    t.set_justify(gtk::Justification::Center);
    t.set_max_width_chars(38);
    let s = gtk::Label::new(Some("Files you convert will be listed here."));
    s.add_css_class("cz-subtitle");
    s.set_wrap(true);
    s.set_justify(gtk::Justification::Center);
    s.set_max_width_chars(38);
    empty.append(&icon);
    empty.append(&t);
    empty.append(&s);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&panel, Some("list"));

    (
        page_frame(
            "History",
            "Conversions from this and previous sessions.",
            &stack,
        ),
        stack,
    )
}

// ---------------------------------------------------------------------------
// wiring
// ---------------------------------------------------------------------------

fn wire(ui: &Rc<App>, add_more: &gtk::Button) {
    // Browse, from either entry point.
    if let Some(browse) = find_named(&ui.dropzone, "dropzone-browse") {
        let ui = ui.clone();
        browse.connect_clicked(move |_| choose_files(&ui));
    }
    {
        let ui = ui.clone();
        add_more.connect_clicked(move |_| choose_files(&ui));
    }

    // Drag and drop, with the accent feedback §13 asks for.
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    {
        let ui = ui.clone();
        drop.connect_enter(move |_, _, _| {
            ui.dropzone.add_css_class("active");
            ui.dropzone_title.set_text("Release to add");
            gdk::DragAction::COPY
        });
    }
    {
        let ui = ui.clone();
        drop.connect_leave(move |_| reset_dropzone(&ui));
    }
    {
        let ui = ui.clone();
        drop.connect_drop(move |_, value, _, _| {
            reset_dropzone(&ui);
            if let Ok(list) = value.get::<gdk::FileList>() {
                let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
                if !paths.is_empty() {
                    add_files(&ui, paths);
                    return true;
                }
            }
            false
        });
    }
    ui.window.add_controller(drop);

    // Output format.
    {
        let picker = ui.output_picker.clone();
        let ui = ui.clone();
        picker.connect_changed(move |id| {
            {
                let mut s = ui.settings.borrow_mut();
                s.last_output_format = Some(id.to_string());
                let _ = s.save();
            }
            suggest_name(&ui);
            refresh_queue(&ui);
            revalidate(&ui);
        });
    }
    {
        // Swapping puts the current input format in the output slot, which is
        // what "convert it back" means in practice.
        let swap = ui.swap_btn.clone();
        let ui = ui.clone();
        swap.connect_clicked(move |_| {
            let input = ui
                .files
                .borrow()
                .get(*ui.selected.borrow())
                .map(|f| f.format.clone());
            if let Some(id) = input {
                if registry::by_id(&id).map(|h| h.info().capabilities.writes) == Some(true) {
                    ui.output_picker.set_selected(&id);
                    suggest_name(&ui);
                    revalidate(&ui);
                } else {
                    ui.toasts.add_toast(adw::Toast::new(
                        "That format cannot be written yet, so there is nothing to swap to",
                    ));
                }
            }
        });
    }

    // Destination menu.
    build_destination_menu(ui);

    // Filename edits revalidate as you type.
    {
        let entry = ui.name_entry.clone();
        let ui = ui.clone();
        entry.connect_changed(move |_| revalidate(&ui));
    }

    {
        let button = ui.convert_btn.clone();
        let ui = ui.clone();
        button.connect_clicked(move |_| start_convert(&ui));
    }

    // Queue selection drives the preview.
    {
        let list = ui.queue_list.clone();
        let ui = ui.clone();
        list.connect_row_selected(move |_, row| {
            if let Some(r) = row {
                let idx = r.index();
                if idx >= 0 {
                    select_file(&ui, idx as usize);
                }
            }
        });
    }

    // Preview navigation.
    {
        let slider = ui.slider.clone();
        let ui = ui.clone();
        slider.connect_value_changed(move |s| {
            show_layer(&ui, s.value() as u32);
        });
    }
    {
        let play = ui.play_btn.clone();
        let ui = ui.clone();
        play.connect_clicked(move |_| toggle_play(&ui));
    }
    wire_preview_nav(ui);
    wire_keys(ui);
}

/// Find a child by widget name, so the drop zone's button can be reached
/// without threading it through several constructors.
fn find_named(root: &impl IsA<gtk::Widget>, name: &str) -> Option<gtk::Button> {
    let mut child = root.as_ref().first_child();
    while let Some(c) = child {
        if c.widget_name() == name {
            return c.downcast::<gtk::Button>().ok();
        }
        if let Some(found) = find_named(&c, name) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

fn reset_dropzone(ui: &Rc<App>) {
    ui.dropzone.remove_css_class("active");
    ui.dropzone_title.set_text("Drop files here");
}

fn wire_preview_nav(ui: &Rc<App>) {
    // The navigation buttons live inside the preview bar; wire them by tooltip
    // so the layout can be rearranged without rewiring.
    type Jump = Box<dyn Fn(u32, u32) -> u32>;
    let jump: Vec<(&str, Jump)> = vec![
        ("First layer  (Home)", Box::new(|_, _| 0)),
        (
            "Previous layer  (Left)",
            Box::new(|c, _| c.saturating_sub(1)),
        ),
        (
            "Next layer  (Right)",
            Box::new(|c, n| (c + 1).min(n.saturating_sub(1))),
        ),
        ("Last layer  (End)", Box::new(|_, n| n.saturating_sub(1))),
    ];
    for (tooltip, f) in jump {
        if let Some(button) = find_by_tooltip(&ui.preview_stack, tooltip) {
            let ui = ui.clone();
            button.connect_clicked(move |_| {
                let count = layer_count(&ui);
                if count == 0 {
                    return;
                }
                let cur = ui.slider.value() as u32;
                ui.slider.set_value(f(cur, count) as f64);
            });
        }
    }
}

fn find_by_tooltip(root: &impl IsA<gtk::Widget>, tooltip: &str) -> Option<gtk::Button> {
    let mut child = root.as_ref().first_child();
    while let Some(c) = child {
        if c.tooltip_text().map(|t| t == tooltip).unwrap_or(false) {
            if let Ok(b) = c.clone().downcast::<gtk::Button>() {
                return Some(b);
            }
        }
        if let Some(found) = find_by_tooltip(&c, tooltip) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

fn wire_keys(ui: &Rc<App>) {
    let keys = gtk::EventControllerKey::new();
    let ui2 = ui.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let count = layer_count(&ui2);
        let cur = ui2.slider.value() as u32;
        // Zoom keys belong to the viewer, but only while it is on screen.
        if ui2.shell.current() == Section::Preview && !ctrl && ui2.viewer.handle_key(key) {
            return glib::Propagation::Stop;
        }
        match key {
            gdk::Key::o if ctrl => {
                choose_files(&ui2);
                glib::Propagation::Stop
            }
            gdk::Key::k if ctrl => {
                palette::show(&ui2);
                glib::Propagation::Stop
            }
            gdk::Key::Return if ctrl => {
                start_convert(&ui2);
                glib::Propagation::Stop
            }
            gdk::Key::Delete => {
                remove_selected(&ui2);
                glib::Propagation::Stop
            }
            gdk::Key::space if count > 0 && ui2.shell.current() == Section::Preview => {
                toggle_play(&ui2);
                glib::Propagation::Stop
            }
            gdk::Key::Left if count > 0 && ui2.shell.current() == Section::Preview => {
                ui2.slider.set_value(cur.saturating_sub(1) as f64);
                glib::Propagation::Stop
            }
            gdk::Key::Right if count > 0 && ui2.shell.current() == Section::Preview => {
                ui2.slider.set_value((cur + 1).min(count - 1) as f64);
                glib::Propagation::Stop
            }
            gdk::Key::Home if count > 0 && ui2.shell.current() == Section::Preview => {
                ui2.slider.set_value(0.0);
                glib::Propagation::Stop
            }
            gdk::Key::End if count > 0 && ui2.shell.current() == Section::Preview => {
                ui2.slider.set_value((count - 1) as f64);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    ui.window.add_controller(keys);
}

fn layer_count(ui: &Rc<App>) -> u32 {
    ui.files
        .borrow()
        .get(*ui.selected.borrow())
        .and_then(|f| f.opened.as_ref())
        .map(|o| o.print.layer_count())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// files
// ---------------------------------------------------------------------------

fn choose_files(ui: &Rc<App>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Sliced resin files"));
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
        .title("Add sliced files")
        .filters(&filters)
        .modal(true)
        .build();
    if let Some(dir) = ui.settings.borrow().open_start_dir() {
        dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
    }
    let ui = ui.clone();
    dialog.open_multiple(
        Some(&ui.window.clone()),
        gio::Cancellable::NONE,
        move |res| {
            if let Ok(files) = res {
                let paths: Vec<PathBuf> = (0..files.n_items())
                    .filter_map(|i| files.item(i))
                    .filter_map(|o| o.downcast::<gio::File>().ok())
                    .filter_map(|f| f.path())
                    .collect();
                if !paths.is_empty() {
                    add_files(&ui, paths);
                }
            }
        },
    );
}

fn add_files(ui: &Rc<App>, paths: Vec<PathBuf>) {
    let existing: Vec<PathBuf> = ui.files.borrow().iter().map(|f| f.path.clone()).collect();
    let mut added = 0;
    for path in paths {
        if existing.contains(&path) || !path.is_file() {
            continue;
        }
        if let Some(dir) = path.parent() {
            let mut s = ui.settings.borrow_mut();
            if s.last_open_dir.as_deref() != Some(dir) {
                s.last_open_dir = Some(dir.to_path_buf());
                let _ = s.save();
            }
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        ui.files.borrow_mut().push(Queued {
            path: path.clone(),
            size,
            format: String::new(),
            detection: String::new(),
            extension_mismatch: false,
            warnings: Vec::new(),
            opened: None,
            status: Status::Reading,
            suggestions: Vec::new(),
            forced_format: None,
        });
        added += 1;
        read_in_background(ui, path, None);
    }
    if added > 0 {
        refresh_queue(ui);
        // Morph rather than jump: the drop zone gives way to the queue (§24).
        ui.dropzone.set_visible(false);
        ui.queue_panel.set_visible(true);
        ui.controls.set_visible(true);
        if ui.files.borrow().len() == added {
            select_file(ui, 0);
        }
    }
}

fn read_in_background(ui: &Rc<App>, path: PathBuf, forced: Option<String>) {
    ui.penguin.start();
    let (tx, rx) = async_channel::bounded(1);
    let p = path.clone();
    std::thread::spawn(move || {
        let _ = tx.send_blocking(read_file(&p, forced.as_deref()));
    });

    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let result = rx.recv().await;
        ui.penguin.stop();
        let Ok(result) = result else { return };
        let mut files = ui.files.borrow_mut();
        let Some(entry) = files.iter_mut().find(|f| f.path == path) else {
            return;
        };
        match result {
            Ok(read) => {
                entry.format = read.format.clone();
                entry.detection = read.detection;
                entry.extension_mismatch = read.extension_mismatch;
                entry.warnings = read.warnings.clone();
                entry.opened = Some(read.opened);
                entry.status = if read.warnings.is_empty() && !read.extension_mismatch {
                    Status::Ready
                } else {
                    let mut notes = read.warnings;
                    if read.extension_mismatch {
                        notes.push(
                            "The file extension disagrees with the contents; the contents were used."
                                .into(),
                        );
                    }
                    Status::Warning(notes.join("\n"))
                };
            }
            Err((message, suggestions)) => {
                entry.status = Status::Failed(message);
                entry.suggestions = suggestions;
            }
        }
        drop(files);
        refresh_queue(&ui);
        let sel = *ui.selected.borrow();
        select_file(&ui, sel);
    });
}

struct ReadFile {
    format: String,
    detection: String,
    extension_mismatch: bool,
    warnings: Vec<String>,
    opened: Arc<OpenedFile>,
}

/// A message for the user, and what they can try (§28).
type ReadFailure = (String, Vec<Suggestion>);

/// What the Input control says: the format, and how it was arrived at.
///
/// "GOO (Automatically Detect)" answers both questions at once, where a bare
/// "GOO" left the menu's tick on "Detect automatically" looking like it
/// disagreed with the field. Narrow, the suffix goes rather than being cut
/// off in the middle of itself.
fn refresh_input_label(ui: &Rc<App>) {
    let files = ui.files.borrow();
    let text = match files.get(*ui.selected.borrow()) {
        Some(f) if !f.format.is_empty() => {
            let name = f.format.to_uppercase();
            if f.forced_format.is_some() || ui.compact.get() {
                name
            } else {
                format!("{name} (Automatically Detect)")
            }
        }
        _ => "—".to_string(),
    };
    ui.input_label.set_text(&text);
}

/// The list of formats the selected file can be read as (§21).
///
/// "Detect automatically" first, then every format that can read, so the
/// normal case is the default and the override is a deliberate act.
fn build_input_menu(ui: &Rc<App>) {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("cz-menu");

    let popover = gtk::Popover::builder().child(&list).build();
    popover.add_css_class("menu");

    let (current, reading_as) = {
        let files = ui.files.borrow();
        let f = files.get(*ui.selected.borrow());
        (
            f.and_then(|f| f.forced_format.clone()),
            f.map(|f| f.format.clone()).unwrap_or_default(),
        )
    };

    // Say what automatic detection settled on, not just that it is on. The
    // field beside this reads "GOO" while the tick sits on "Detect
    // automatically", and without this the two look like they disagree.
    let detected = registry::by_id(&reading_as)
        .map(|h| h.info().name)
        .filter(|_| current.is_none());
    let mut entries: Vec<(Option<String>, String, String)> = vec![(
        None,
        "Detect automatically".into(),
        match detected {
            Some(name) => format!("Reading it as {name}"),
            None => "Read the contents and work it out".into(),
        },
    )];
    for info in registry::readable() {
        entries.push((
            Some(info.id.to_string()),
            info.name.to_string(),
            format!("Read the file as {}", info.name),
        ));
    }

    for (id, title, subtitle) in entries {
        let row = gtk::ListBoxRow::new();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
        line.set_margin_top(theme::SPACE_2);
        line.set_margin_bottom(theme::SPACE_2);
        line.set_margin_start(theme::SPACE_3);
        line.set_margin_end(theme::SPACE_3);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let t = gtk::Label::builder().label(&title).xalign(0.0).build();
        let s = gtk::Label::builder().label(&subtitle).xalign(0.0).build();
        s.add_css_class("caption");
        s.add_css_class("cz-dim");
        text.append(&t);
        text.append(&s);
        text.set_hexpand(true);
        line.append(&text);
        if id == current {
            line.append(&gtk::Image::from_icon_name("object-select-symbolic"));
        }
        row.set_child(Some(&line));
        list.append(&row);
    }

    // Clicks land on the list, not the row: connect_activate on a row is a
    // keyboard signal, which is why an earlier menu here did nothing at all.
    let ui2 = ui.clone();
    let popover2 = popover.clone();
    list.connect_row_activated(move |_, row| {
        popover2.popdown();
        let index = row.index();
        let chosen = if index <= 0 {
            None
        } else {
            registry::readable()
                .get(index as usize - 1)
                .map(|i| i.id.to_string())
        };
        force_input_format(&ui2, chosen);
    });

    ui.input_button.set_popover(Some(&popover));
}

/// Re-read the selected file as a named format, or by detection again.
fn force_input_format(ui: &Rc<App>, format: Option<String>) {
    let (path, already) = {
        let files = ui.files.borrow();
        let Some(f) = files.get(*ui.selected.borrow()) else {
            return;
        };
        (f.path.clone(), f.forced_format.clone())
    };
    if already == format {
        return;
    }
    {
        let mut files = ui.files.borrow_mut();
        if let Some(f) = files.get_mut(*ui.selected.borrow()) {
            f.forced_format = format.clone();
            f.status = Status::Reading;
            f.opened = None;
            f.suggestions.clear();
        }
    }
    refresh_queue(ui);
    let index = *ui.selected.borrow();
    select_file(ui, index);
    read_in_background(ui, path, format);
}

fn read_file(path: &Path, forced: Option<&str>) -> Result<ReadFile, ReadFailure> {
    let facts = remedy::FileFacts::observe(path);
    let explain = |e: cheapazsla_core::Error| -> ReadFailure {
        (e.to_string(), remedy::for_error(&e, &facts))
    };

    // Detection still runs even when the format is being forced, so the file
    // can still be described and an extension mismatch still reported. Only
    // the choice of handler changes.
    let id = registry::identify(path).map_err(explain)?;
    let chosen = forced.unwrap_or(id.detection.format_id);
    let handler =
        registry::by_id(chosen).ok_or_else(|| explain(cheapazsla_core::Error::UnknownFormat))?;
    let warnings = handler.validate(path).unwrap_or_default();
    let opened = handler.open(path).map_err(explain)?;
    Ok(ReadFile {
        format: chosen.to_string(),
        detection: if forced.is_some() {
            format!("read as {} because you said so", handler.info().name)
        } else {
            id.detection.reason
        },
        extension_mismatch: id.extension_mismatch,
        warnings,
        opened: Arc::new(opened),
    })
}

fn refresh_queue(ui: &Rc<App>) {
    let selected = *ui.selected.borrow();
    while let Some(row) = ui.queue_list.first_child() {
        ui.queue_list.remove(&row);
    }
    for (i, f) in ui.files.borrow().iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
        row.set_margin_top(theme::SPACE_2);
        row.set_margin_bottom(theme::SPACE_2);
        row.set_margin_start(theme::SPACE_3);
        row.set_margin_end(theme::SPACE_2);

        row.append(&gtk::Image::from_icon_name("text-x-generic-symbolic"));

        let name = gtk::Label::builder()
            .label(f.name())
            .xalign(0.0)
            .hexpand(true)
            .build();
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        row.append(&name);

        // Source and target on every row. The output format applies to the
        // whole queue, so without this a file's fate is only visible while it
        // happens to be the selected one.
        let from = if f.format.is_empty() {
            "—".to_string()
        } else {
            f.format.to_uppercase()
        };
        let conversion = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_1);
        conversion.set_width_request(if ui.compact.get() { 0 } else { 112 });
        let from_label = gtk::Label::new(Some(&from));
        from_label.add_css_class("cz-dim");
        conversion.append(&from_label);
        if let Some(target) = ui.output_picker.selected() {
            let same = !f.format.is_empty() && f.format == target;
            // go-next-symbolic is a chevron, which reads as "more" rather
            // than "becomes". In a data row an arrow glyph is typography, not
            // an icon standing in for a control.
            let arrow = gtk::Label::new(Some("→"));
            arrow.add_css_class(if same { "cz-warn" } else { "cz-arrow" });
            conversion.append(&arrow);
            let to_label = gtk::Label::new(Some(&target.to_uppercase()));
            if same {
                // Already this format: say so rather than implying work.
                to_label.add_css_class("cz-warn");
                shell::set_tooltip_deep(
                    &conversion,
                    "This file is already in the output format, so converting it would achieve nothing.",
                );
            } else {
                to_label.add_css_class("cz-value");
            }
            conversion.append(&to_label);
        }
        row.append(&conversion);

        // The size is the first thing to go: it is the least useful column
        // when space is short, and it is in the file panel anyway.
        let size = gtk::Label::new(Some(&render::human_bytes(f.size)));
        size.add_css_class("cz-dim");
        size.add_css_class("cz-value");
        size.set_width_chars(9);
        size.set_xalign(1.0);
        size.set_visible(!ui.compact.get());
        row.append(&size);

        let chip = f.status.chip();
        chip.set_width_request(if ui.compact.get() { 0 } else { 104 });
        row.append(&chip);

        // Full technical text behind Details, as §28 asks.
        if matches!(f.status, Status::Failed(_) | Status::Warning(_)) {
            if let Some(detail) = f.status.detail() {
                let details = gtk::Button::with_label("Details");
                details.add_css_class("flat");
                details.set_valign(gtk::Align::Center);
                let win = ui.window.clone();
                let heading = if matches!(f.status, Status::Failed(_)) {
                    "This file could not be opened"
                } else {
                    "Worth knowing about this file"
                };
                let name = f.name();
                let suggestions = f.suggestions.clone();
                details.connect_clicked(move |_| {
                    show_details(&win, heading, &name, &detail, &suggestions);
                });
                row.append(&details);
            }
        }

        let remove = shell::icon_button("window-close-symbolic", "Remove from list");
        let ui2 = ui.clone();
        let path = f.path.clone();
        remove.connect_clicked(move |_| remove_file(&ui2, &path));
        row.append(&remove);

        let list_row = gtk::ListBoxRow::builder().child(&row).build();
        // The reason is on hover, anywhere on the row. GTK resolves a tooltip
        // against the widget under the pointer, so setting it only on the
        // container leaves dead spots over every child.
        if let Some(detail) = f.status.detail() {
            shell::set_tooltip_deep(&list_row, &detail);
        }
        ui.queue_list.append(&list_row);
        if i == selected {
            ui.queue_list.select_row(Some(&list_row));
        }
    }
    revalidate(ui);
}

/// What happened, then what to try, with the technical text last.
///
/// The order is deliberate: a person who has just been told their file failed
/// wants to know what to do about it, not to read a parser message first.
fn show_details(
    parent: &adw::ApplicationWindow,
    heading: &str,
    name: &str,
    detail: &str,
    suggestions: &[Suggestion],
) {
    let body = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_3);
    body.set_margin_top(theme::SPACE_2);

    let what = gtk::Label::builder()
        .label(detail)
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(56)
        .build();
    body.append(&what);

    if !suggestions.is_empty() {
        let head = shell::section_label("What you can try");
        head.set_margin_top(theme::SPACE_2);
        body.append(&head);

        for (i, s) in suggestions.iter().enumerate() {
            let item = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
            item.set_valign(gtk::Align::Start);
            let n = gtk::Label::builder()
                .label(format!("{}.", i + 1))
                .xalign(1.0)
                .width_chars(2)
                .valign(gtk::Align::Start)
                .build();
            n.add_css_class("cz-dim");
            item.append(&n);

            let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
            let action = gtk::Label::builder()
                .label(&s.action)
                .xalign(0.0)
                .wrap(true)
                .max_width_chars(50)
                .build();
            text.append(&action);
            if !s.because.is_empty() {
                let why = gtk::Label::builder()
                    .label(&s.because)
                    .xalign(0.0)
                    .wrap(true)
                    .max_width_chars(50)
                    .build();
                why.add_css_class("caption");
                why.add_css_class("cz-dim");
                text.append(&why);
            }
            item.append(&text);
            body.append(&item);
        }
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .max_content_height(420)
        .propagate_natural_height(true)
        .child(&body)
        .build();

    let d = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading(heading)
        .body(name)
        .extra_child(&scroller)
        .build();
    d.add_response("ok", "Close");
    d.present();
}

fn remove_file(ui: &Rc<App>, path: &Path) {
    ui.files.borrow_mut().retain(|f| f.path != path);
    let len = ui.files.borrow().len();
    if len == 0 {
        stop_play(ui);
        *ui.selected.borrow_mut() = 0;
        ui.dropzone.set_visible(true);
        ui.queue_panel.set_visible(false);
        ui.controls.set_visible(false);
        ui.preview_stack.set_visible_child_name("empty");
        // Drop the texture as well, so the last layer of a removed file is
        // not still sitting in memory waiting to reappear.
        ui.viewer.clear();
        ui.viewer.fit();
        refresh_queue(ui);
        return;
    }
    let sel = (*ui.selected.borrow()).min(len - 1);
    *ui.selected.borrow_mut() = sel;
    refresh_queue(ui);
    select_file(ui, sel);
}

fn remove_selected(ui: &Rc<App>) {
    let path = ui
        .files
        .borrow()
        .get(*ui.selected.borrow())
        .map(|f| f.path.clone());
    if let Some(p) = path {
        remove_file(ui, &p);
    }
}

// ---------------------------------------------------------------------------
// selection, preview
// ---------------------------------------------------------------------------

fn select_file(ui: &Rc<App>, index: usize) {
    let len = ui.files.borrow().len();
    if len == 0 {
        return;
    }
    let index = index.min(len - 1);
    *ui.selected.borrow_mut() = index;
    stop_play(ui);
    // Keyed by layer number, so it must not outlive the file it came from or
    // the wrong image is shown instantly and convincingly.
    ui.textures.borrow_mut().clear();
    ui.texture_order.borrow_mut().clear();
    ui.in_flight.borrow_mut().clear();

    let (count, ready) = {
        let files = ui.files.borrow();
        let f = &files[index];
        let count = f
            .opened
            .as_ref()
            .map(|o| o.print.layer_count())
            .unwrap_or(0);
        (count, f.opened.is_some())
    };
    refresh_input_label(ui);
    // Rebuilt per selection so the tick sits beside whatever this file is
    // being read as.
    build_input_menu(ui);

    if ready && count > 0 {
        build_overview(ui, count);
        ui.slider
            .set_range(0.0, (count.saturating_sub(1)).max(1) as f64);
        ui.slider.set_value(0.0);
        ui.preview_stack.set_visible_child_name("view");
        show_layer(ui, 0);
    } else {
        ui.preview_stack.set_visible_child_name("empty");
    }
    refresh_info_panel(ui);
    suggest_name(ui);
    revalidate(ui);
}

fn refresh_info_panel(ui: &Rc<App>) {
    while let Some(c) = ui.info_panel.first_child() {
        ui.info_panel.remove(&c);
    }
    let files = ui.files.borrow();
    let Some(f) = files.get(*ui.selected.borrow()) else {
        return;
    };
    let Some(opened) = f.opened.as_ref() else {
        ui.info_panel
            .append(&shell::info_row("Status", "Not readable", true));
        return;
    };
    let p = &opened.print;
    let add = |label: &str, value: Option<String>| {
        // §13 and §24: absent is stated, never filled with a plausible zero.
        let (text, dim) = match value {
            Some(v) => (v, false),
            None => ("not recorded".to_string(), true),
        };
        ui.info_panel.append(&shell::info_row(label, &text, dim));
    };
    add("Format", Some(p.source_format.to_uppercase()));
    add("Detected by", Some(f.detection.clone()));
    add("File size", Some(render::human_bytes(f.size)));
    add(
        "Resolution",
        Some(format!(
            "{} × {}",
            p.geometry.resolution_x, p.geometry.resolution_y
        )),
    );
    add(
        "Pixel size",
        p.geometry
            .pixel_size_um()
            .map(|(x, y)| format!("{x:.2} × {y:.2} µm")),
    );
    add("Layers", Some(p.layer_count().to_string()));
    add(
        "Layer height",
        Some(format!("{} mm", p.exposure.layer_height_mm)),
    );
    add("Print height", p.height_mm().map(|h| format!("{h:.2} mm")));
    add("Exposure", Some(format!("{} s", p.exposure.exposure_s)));
    add(
        "Bottom exposure",
        p.exposure.bottom_exposure_s.map(|v| format!("{v} s")),
    );
    add(
        "Bottom layers",
        p.exposure.bottom_layers.map(|v| v.to_string()),
    );
    add("Print time", p.print_time_s.map(render::human_time));
    add(
        "Resin volume",
        p.material_volume_ml.map(|v| format!("{v} ml")),
    );
    add("Printer", p.machine_name.clone());
}

fn show_layer(ui: &Rc<App>, index: u32) {
    let opened = ui
        .files
        .borrow()
        .get(*ui.selected.borrow())
        .and_then(|f| f.opened.clone());
    let Some(opened) = opened else { return };
    let count = opened.print.layer_count();
    if count == 0 {
        return;
    }
    let index = index.min(count - 1);

    // Dragging the slider across a long print asks for a great many layers in
    // quick succession, each of which is a multi-megapixel decode. Only the
    // most recent request is worth drawing: the rest have already been
    // superseded before they finish, and painting them makes the preview lag
    // behind the slider and appear stuck.
    let request = ui.layer_request.get().wrapping_add(1);
    ui.layer_request.set(request);

    // Panels are often not square-pixelled, so the preview is corrected to the
    // physical proportions of the build area rather than drawn one bitmap
    // pixel to one screen pixel.
    let pixel = opened
        .print
        .geometry
        .pixel_size_um()
        .map(|(x_um, y_um)| render::PixelSize { x_um, y_um });
    // Whether the correction was applied, so the caption can say so.
    let square = pixel
        .map(|p| (p.y_um / p.x_um - 1.0).abs() < 0.01)
        .unwrap_or(true);

    // Already built: draw it now. Scrubbing revisits layers constantly, and
    // rebuilding a texture still in hand makes the slider feel like it is
    // dragging something heavy.
    // How fast the slider is moving, judged from the gap between requests.
    let moving_fast = {
        let mut last = ui.last_request_at.borrow_mut();
        let fast = last
            .map(|t| t.elapsed() < std::time::Duration::from_millis(90))
            .unwrap_or(false);
        *last = Some(std::time::Instant::now());
        fast
    };

    let cached = ui.textures.borrow().get(&index).cloned();
    if let Some(cached) = cached {
        draw_layer(ui, &cached, square, index, count, false);
        prefetch_around(ui, index, count, pixel);
        ui.last_layer.set(index);
        return;
    }

    // Not built yet. Rather than leave the last layer sitting there while a
    // decode runs, show the nearest one already built so the view keeps up
    // with the slider. It is replaced by the real layer as soon as that
    // arrives, and the caption says plainly that what is on screen is not the
    // layer asked for, because an inspection tool that quietly shows you a
    // different layer than the one you selected is worse than a slow one.
    let mut stood_in = false;
    if moving_fast {
        if let Some((near, drawn)) = nearest_built(ui, index) {
            draw_layer(ui, &drawn, square, near, count, near != index);
            stood_in = true;
        }
    }

    // Moving fast with something on screen: hold off. Every layer being flown
    // past costs a full decode, and by the time it finishes the slider has
    // moved on, so the work is thrown away and the machine is busy doing it.
    // Waiting a moment means only the layer actually landed on is decoded.
    //
    // This is what makes fast scrubbing feel smooth rather than being smooth:
    // the picture keeps changing from what is already built, and the exact
    // layer resolves the instant the user slows down.
    if stood_in {
        let ui_later = ui.clone();
        let opened_later = opened.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(70), move || {
            if ui_later.layer_request.get() != request {
                return; // still moving; this is not where they stopped
            }
            decode_and_show(
                &ui_later,
                opened_later,
                index,
                count,
                pixel,
                square,
                request,
            );
        });
        ui.last_layer.set(index);
        return;
    }

    decode_and_show(ui, opened, index, count, pixel, square, request);
    ui.last_layer.set(index);
}

/// Start the decode for one layer and show it when it arrives.
#[allow(clippy::too_many_arguments)]
fn decode_and_show(
    ui: &Rc<App>,
    opened: Arc<OpenedFile>,
    index: u32,
    count: u32,
    pixel: Option<render::PixelSize>,
    square: bool,
    request: u64,
) {
    // Already built while we were waiting.
    if let Some(cached) = ui.textures.borrow().get(&index).cloned() {
        draw_layer(ui, &cached, square, index, count, false);
        prefetch_around(ui, index, count, pixel);
        return;
    }
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = opened.layers.layer(index).map(|img| {
            let total = img.width as u64 * img.height as u64;
            (render::texture_for(&img, pixel), total)
        });
        let _ = tx.send_blocking(result);
    });
    ui.in_flight.borrow_mut().insert(index);
    finish_layer(ui, rx, request, square, index);
    prefetch_around(ui, index, count, pixel);
}

/// How many layers to read ahead once the user settles.
const PREFETCH: u32 = 3;

/// Most decodes to have running at once.
///
/// This cap, not a delay, is what stops reading ahead from swamping the
/// machine. An earlier version waited for the user to stop moving before
/// reading ahead at all, which meant every layer during a drag cost a full
/// decode and the slider paused on each one. Starting immediately and
/// limiting how many run together keeps the reads-ahead useful without them
/// competing with the layer on screen.
const MAX_DECODES: usize = 3;

/// Decode the layers the user is about to want.
///
/// Scrubbing stutters wherever the cache runs out, because a cached layer
/// draws immediately and an uncached one costs a multi-megapixel decode.
/// Reading ahead in the direction of travel keeps that boundary in front of
/// where the slider actually is.
fn prefetch_around(ui: &Rc<App>, index: u32, count: u32, pixel: Option<render::PixelSize>) {
    let Some(opened) = ui
        .files
        .borrow()
        .get(*ui.selected.borrow())
        .and_then(|f| f.opened.clone())
    else {
        return;
    };
    let forward = index >= ui.last_layer.get();
    let mut wanted: Vec<u32> = Vec::new();
    for step in 1..=PREFETCH {
        let ahead = if forward {
            index.checked_add(step)
        } else {
            index.checked_sub(step)
        };
        if let Some(i) = ahead.filter(|i| *i < count) {
            wanted.push(i);
        }
    }
    // One behind as well, so reversing direction is not a cliff.
    if let Some(i) = if forward {
        index.checked_sub(1)
    } else {
        index.checked_add(1).filter(|i| *i < count)
    } {
        wanted.push(i);
    }

    for i in wanted {
        if ui.in_flight.borrow().len() >= MAX_DECODES {
            break;
        }
        if ui.textures.borrow().contains_key(&i) || ui.in_flight.borrow().contains(&i) {
            continue;
        }
        ui.in_flight.borrow_mut().insert(i);
        let (tx, rx) = async_channel::bounded(1);
        let layers = opened.clone();
        std::thread::spawn(move || {
            let result = layers.layers.layer(i).map(|img| {
                let total = img.width as u64 * img.height as u64;
                (render::texture_for(&img, pixel), total)
            });
            let _ = tx.send_blocking(result);
        });
        let ui = ui.clone();
        glib::spawn_future_local(async move {
            if let Ok(Ok(((texture, factor, exposed), total))) = rx.recv().await {
                remember_texture(
                    &ui,
                    i,
                    Drawn {
                        texture,
                        factor,
                        exposed,
                        total,
                    },
                );
            }
            ui.in_flight.borrow_mut().remove(&i);
        });
    }
}

/// A built layer, ready to draw.
#[derive(Clone)]
struct Drawn {
    texture: gdk::Texture,
    factor: u32,
    exposed: u64,
    total: u64,
}

/// Put a built layer on screen and update the labels beneath it.
///
/// `shown` is the layer actually on screen, which during fast scrubbing may
/// not yet be the one selected. When they differ the labels say so rather
/// than letting the number claim something the image does not show.
fn draw_layer(ui: &Rc<App>, d: &Drawn, square: bool, shown: u32, count: u32, approximate: bool) {
    ui.viewer.set_texture(&d.texture);
    ui.layer_label
        .set_text(&format!("Layer {} / {}", shown + 1, count));

    if approximate {
        // Dimming the number says "not settled yet" without changing any
        // width, which is what caused the slider to move about before.
        set_layer_detail(ui, &[("Status", "still loading…".into())]);
        ui.layer_label.add_css_class("cz-dim");
        return;
    }
    ui.layer_label.remove_css_class("cz-dim");

    let pct = if d.total > 0 {
        d.exposed as f64 / d.total as f64 * 100.0
    } else {
        0.0
    };
    let mut rows: Vec<(&str, String)> = vec![
        ("Exposed", format!("{} px", d.exposed)),
        ("Coverage", format!("{pct:.3}%")),
    ];
    if d.factor > 1 {
        rows.push(("Preview scale", format!("1/{}", d.factor)));
    }
    if !square {
        rows.push(("Aspect", "corrected for non-square pixels".into()));
    }
    set_layer_detail(ui, &rows);
}

/// Fill the per-layer panel. Rows rather than one long caption, so nothing
/// here can change the width of anything beside it.
fn set_layer_detail(ui: &Rc<App>, rows: &[(&str, String)]) {
    while let Some(c) = ui.layer_detail.first_child() {
        ui.layer_detail.remove(&c);
    }
    for (label, value) in rows {
        ui.layer_detail
            .append(&shell::info_row(label, value, false));
    }
}

/// Build a sparse set of layers spread across the whole print.
///
/// Fast scrubbing shows the nearest already-built layer so the picture keeps
/// up, but early on nothing is built anywhere near where the slider is, so
/// there is nothing to show and it appears to freeze. Laying down a coarse
/// index means any point on the slider has something within a few layers from
/// the moment the file opens.
///
/// One at a time, and only while nothing else is decoding, so it never
/// competes with the layer actually being looked at.
fn build_overview(ui: &Rc<App>, count: u32) {
    /// Roughly how many layers apart the index is. Chosen against the
    /// stand-in's own reach, so every position has one within range.
    const SPREAD: u32 = 12;

    if count == 0 {
        return;
    }
    let wanted: Vec<u32> = (0..count).step_by(SPREAD as usize).collect();
    schedule_overview(ui, wanted, 0);
}

fn schedule_overview(ui: &Rc<App>, wanted: Vec<u32>, at: usize) {
    if at >= wanted.len() {
        return;
    }
    let ui = ui.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(60), move || {
        // Abandoned if the file changed underneath.
        let Some(opened) = ui
            .files
            .borrow()
            .get(*ui.selected.borrow())
            .and_then(|f| f.opened.clone())
        else {
            return;
        };
        let count = opened.print.layer_count();
        if wanted.last().map(|i| *i >= count).unwrap_or(true) {
            return;
        }

        // Wait rather than pile on while the user is being served.
        if !ui.in_flight.borrow().is_empty() {
            schedule_overview(&ui, wanted, at);
            return;
        }

        let index = wanted[at];
        if ui.textures.borrow().contains_key(&index) {
            schedule_overview(&ui, wanted, at + 1);
            return;
        }

        let pixel = opened
            .print
            .geometry
            .pixel_size_um()
            .map(|(x_um, y_um)| render::PixelSize { x_um, y_um });
        ui.in_flight.borrow_mut().insert(index);
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = opened.layers.layer(index).map(|img| {
                let total = img.width as u64 * img.height as u64;
                (render::texture_for(&img, pixel), total)
            });
            let _ = tx.send_blocking(result);
        });
        glib::spawn_future_local(async move {
            if let Ok(Ok(((texture, factor, exposed), total))) = rx.recv().await {
                remember_texture(
                    &ui,
                    index,
                    Drawn {
                        texture,
                        factor,
                        exposed,
                        total,
                    },
                );
            }
            ui.in_flight.borrow_mut().remove(&index);
            schedule_overview(&ui, wanted, at + 1);
        });
    });
}

/// The built layer closest to the one asked for, if there is one near enough
/// to be a useful stand-in.
fn nearest_built(ui: &Rc<App>, index: u32) -> Option<(u32, Drawn)> {
    /// Beyond this the image on screen has nothing to do with the layer
    /// selected, and showing it is worse than showing nothing new. Matched to
    /// the spacing of the overview index, so there is always one in range.
    const NEAR: u32 = 12;
    let cache = ui.textures.borrow();
    cache
        .iter()
        .filter(|(i, _)| i.abs_diff(index) <= NEAR)
        .min_by_key(|(i, _)| i.abs_diff(index))
        .map(|(i, d)| (*i, d.clone()))
}

/// Draw a decoded layer, unless a newer one has been asked for since.
/// A decoded layer on its way to the screen.
type DecodedLayer = Result<((gdk::Texture, u32, u64), u64), cheapazsla_core::Error>;

fn finish_layer(
    ui: &Rc<App>,
    rx: async_channel::Receiver<DecodedLayer>,
    request: u64,
    square: bool,
    index: u32,
) {
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let received = rx.recv().await;
        match received {
            Ok(Ok(((texture, factor, exposed), total))) => {
                let drawn = Drawn {
                    texture,
                    factor,
                    exposed,
                    total,
                };
                // Cached whether or not it is still wanted: the work is done,
                // and the user is very likely to scrub back over it.
                remember_texture(&ui, index, drawn.clone());
                ui.in_flight.borrow_mut().remove(&index);
                if ui.layer_request.get() == request {
                    let count = ui
                        .files
                        .borrow()
                        .get(*ui.selected.borrow())
                        .and_then(|f| f.opened.as_ref())
                        .map(|o| o.print.layer_count())
                        .unwrap_or(index + 1);
                    draw_layer(&ui, &drawn, square, index, count, false);
                }
            }
            Ok(Err(e)) => {
                if ui.layer_request.get() == request {
                    set_layer_detail(&ui, &[("Status", "could not be decoded".into())]);
                    ui.toasts.add_toast(adw::Toast::new(&e.to_string()));
                }
            }
            Err(_) => {}
        }
    });
}

/// Memory the layer cache may use.
///
/// Counted in bytes rather than layers because a layer is not a fixed size: a
/// preview of a 12K panel is around 6 MB while a small printer's is a
/// fraction of that, so a fixed count would mean a few hundred megabytes on
/// one machine and almost nothing on another.
const TEXTURE_BUDGET: usize = 192 * 1024 * 1024;

/// Roughly what a texture costs, from its dimensions. Three bytes a pixel,
/// which is what the renderer builds.
fn texture_bytes(d: &Drawn) -> usize {
    (d.texture.width() as usize) * (d.texture.height() as usize) * 3
}

fn remember_texture(ui: &Rc<App>, index: u32, drawn: Drawn) {
    let mut cache = ui.textures.borrow_mut();
    let mut order = ui.texture_order.borrow_mut();
    if cache.insert(index, drawn).is_none() {
        order.push_back(index);
    }
    let mut used: usize = cache.values().map(texture_bytes).sum();
    // Always keep at least a few, so a printer with an enormous panel does not
    // end up with a cache that holds nothing and stutters on every layer.
    while used > TEXTURE_BUDGET && order.len() > 4 {
        let Some(oldest) = order.pop_front() else {
            break;
        };
        if let Some(removed) = cache.remove(&oldest) {
            used -= texture_bytes(&removed).min(used);
        }
    }
}

fn toggle_play(ui: &Rc<App>) {
    if ui.playing.borrow().is_some() {
        stop_play(ui);
        return;
    }
    ui.play_btn.set_icon_name("media-playback-pause-symbolic");
    ui.play_btn.set_tooltip_text(Some("Pause  (Space)"));
    let ui2 = ui.clone();
    let id = glib::timeout_add_local(std::time::Duration::from_millis(110), move || {
        let count = layer_count(&ui2);
        if count == 0 {
            return glib::ControlFlow::Break;
        }
        let cur = ui2.slider.value() as u32;
        ui2.slider.set_value(if cur + 1 >= count {
            0.0
        } else {
            (cur + 1) as f64
        });
        glib::ControlFlow::Continue
    });
    *ui.playing.borrow_mut() = Some(id);
}

fn stop_play(ui: &Rc<App>) {
    if let Some(id) = ui.playing.borrow_mut().take() {
        id.remove();
    }
    ui.play_btn.set_icon_name("media-playback-start-symbolic");
    ui.play_btn.set_tooltip_text(Some("Play  (Space)"));
}

// ---------------------------------------------------------------------------
// destination and validation
// ---------------------------------------------------------------------------

/// What a row in the Save to menu does when chosen.
#[derive(Clone)]
enum Destination {
    /// Write next to each source file.
    BesideOriginal,
    /// Write into a specific folder.
    Folder(PathBuf),
    /// Ask for a folder.
    Choose,
}

fn build_destination_menu(ui: &Rc<App>) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_size_request(340, -1);
    let popover = gtk::Popover::builder()
        .child(&content)
        .has_arrow(false)
        .build();
    ui.dest_button.set_popover(Some(&popover));

    let ui2 = ui.clone();
    let content2 = content.clone();
    let pop2 = popover.clone();
    popover.connect_show(move |_| {
        while let Some(c) = content2.first_child() {
            content2.remove(&c);
        }
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("navigation-sidebar");

        // Actions in the same order as the rows. A row's own "activate" signal
        // is a keyboard action and never fires on a click; the list's
        // "row-activated" is what a click emits, and it reports an index.
        let actions: Rc<RefCell<Vec<Destination>>> = Rc::new(RefCell::new(Vec::new()));

        {
            let actions = actions.clone();
            let add = |icon: &str, title: String, sub: Option<String>, what: Destination| {
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
                row_box.set_margin_top(theme::SPACE_2);
                row_box.set_margin_bottom(theme::SPACE_2);
                row_box.set_margin_start(theme::SPACE_3);
                row_box.set_margin_end(theme::SPACE_3);
                row_box.append(&gtk::Image::from_icon_name(icon));
                let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
                text.set_hexpand(true);
                text.append(&gtk::Label::builder().label(&title).xalign(0.0).build());
                if let Some(s) = &sub {
                    let l = gtk::Label::builder().label(s).xalign(0.0).build();
                    l.add_css_class("caption");
                    l.add_css_class("cz-dim");
                    text.append(&l);
                }
                row_box.append(&text);
                list.append(
                    &gtk::ListBoxRow::builder()
                        .child(&row_box)
                        .activatable(true)
                        .build(),
                );
                actions.borrow_mut().push(what);
            };

            add(
                "folder-symbolic",
                "Beside the original".into(),
                Some("Same folder as each source file".into()),
                Destination::BesideOriginal,
            );

            for dir in ui2.settings.borrow().available_recent_dirs() {
                add(
                    "document-open-recent-symbolic",
                    dir.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| dir.display().to_string()),
                    Some(dir.display().to_string()),
                    Destination::Folder(dir.clone()),
                );
            }

            let sub = ui2.settings.borrow().pinned_subfolder.clone();
            for d in drives::mounted() {
                let icon = if d.removable {
                    "drive-removable-media-symbolic"
                } else {
                    "drive-harddisk-symbolic"
                };
                // A pinned drive with a subfolder set targets that folder,
                // creating it if needed, rather than the drive root.
                let target = if ui2.settings.borrow().is_pinned(&d.name) {
                    drives::target_dir(&d.name, &sub).unwrap_or_else(|| d.path.clone())
                } else {
                    d.path.clone()
                };
                let space = drives::space(&target)
                    .map(|(free, total)| {
                        format!(
                            "{} free of {}",
                            render::human_bytes(free),
                            render::human_bytes(total)
                        )
                    })
                    .unwrap_or_else(|| target.display().to_string());
                add(
                    icon,
                    d.name.clone(),
                    Some(space),
                    Destination::Folder(target),
                );
            }

            add(
                "folder-open-symbolic",
                "Choose another location…".into(),
                None,
                Destination::Choose,
            );
        }

        let ui3 = ui2.clone();
        let pop3 = pop2.clone();
        list.connect_row_activated(move |_, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            let chosen = actions.borrow().get(index as usize).cloned();
            pop3.popdown();
            match chosen {
                Some(Destination::BesideOriginal) => set_out_dir(&ui3, None),
                Some(Destination::Folder(d)) => set_out_dir(&ui3, Some(d)),
                Some(Destination::Choose) => choose_folder(&ui3),
                None => {}
            }
        });
        content2.append(&list);
    });
}

fn choose_folder(ui: &Rc<App>) {
    let dialog = gtk::FileDialog::builder()
        .title("Save converted files to")
        .modal(true)
        .build();
    let ui = ui.clone();
    dialog.select_folder(
        Some(&ui.window.clone()),
        gio::Cancellable::NONE,
        move |res| {
            if let Ok(folder) = res {
                if let Some(path) = folder.path() {
                    set_out_dir(&ui, Some(path));
                }
            }
        },
    );
}

fn set_out_dir(ui: &Rc<App>, dir: Option<PathBuf>) {
    match &dir {
        Some(d) => {
            ui.dest_label.set_text(
                &d.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| d.display().to_string()),
            );
            let detail = drives::space(d)
                .map(|(free, _)| {
                    format!(
                        "{}  ·  {} available",
                        d.display(),
                        render::human_bytes(free)
                    )
                })
                .unwrap_or_else(|| d.display().to_string());
            ui.dest_detail.set_text(&detail);
        }
        None => {
            ui.dest_label.set_text("Beside the original");
            ui.dest_detail.set_text("Same folder as each source file");
        }
    }
    *ui.out_dir.borrow_mut() = dir;
    suggest_name(ui);
    revalidate(ui);
}

fn suggest_name(ui: &Rc<App>) {
    let files = ui.files.borrow();
    let Some(f) = files.get(*ui.selected.borrow()) else {
        return;
    };
    let Some(format) = ui.output_picker.selected() else {
        return;
    };
    // A single file gets an editable name; a batch is named per source, so the
    // field would be meaningless (§17).
    let single = files.len() == 1;
    ui.name_row.set_visible(single);
    if !single {
        return;
    }
    if let Some(p) = convert::destination_for(&f.path, format, None) {
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            ui.name_entry.set_text(name);
        }
    }
}

/// Work out the destination for one queued file.
fn destination_for(ui: &Rc<App>, file: &Queued, format: &str) -> Option<PathBuf> {
    let out_dir = ui.out_dir.borrow().clone();
    let generated = convert::destination_for(&file.path, format, out_dir.as_deref())?;
    if ui.files.borrow().len() != 1 {
        return Some(generated);
    }
    let typed = ui.name_entry.text().trim().to_string();
    if typed.is_empty() {
        return Some(generated);
    }
    let want = registry::by_id(format)?.info().extension;
    let name = if Path::new(&typed)
        .extension()
        .map(|e| e.eq_ignore_ascii_case(want))
        .unwrap_or(false)
    {
        typed
    } else {
        format!("{typed}.{want}")
    };
    Some(generated.parent()?.join(name))
}

/// Check everything that could stop a conversion and say so beside the
/// control, rather than waiting for the user to press Convert (§30).
fn revalidate(ui: &Rc<App>) {
    if *ui.converting.borrow() {
        return;
    }
    let problem = check(ui);
    match &problem {
        Some(msg) => {
            ui.problem_label.set_text(msg);
            ui.problem.set_visible(true);
            ui.convert_btn.set_sensitive(false);
        }
        None => {
            ui.problem.set_visible(false);
            ui.convert_btn.set_sensitive(true);
        }
    }
    let n = ui.files.borrow().len();
    ui.convert_label.set_text(&if n > 1 {
        format!("Convert {n} Files")
    } else {
        "Convert".to_string()
    });
}

fn check(ui: &Rc<App>) -> Option<String> {
    let files = ui.files.borrow();
    if files.is_empty() {
        return Some("Add a file to convert.".into());
    }
    if files.iter().all(|f| f.opened.is_none()) {
        return Some("Waiting for the file to be read.".into());
    }
    let format = ui.output_picker.selected()?;

    let typed = ui.name_entry.text();
    if files.len() == 1 && typed.contains('/') {
        return Some("The file name cannot contain a slash.".into());
    }

    // Where would the output go, and can it?
    let first = files.iter().find(|f| f.opened.is_some())?;
    let dest = destination_for(ui, first, format)?;
    let dir = dest.parent()?.to_path_buf();
    if !dir.is_dir() {
        return Some(format!(
            "{} is not available. If it is a removable drive, reconnect it or choose another location.",
            dir.display()
        ));
    }
    if !writable(&dir) {
        return Some(format!(
            "Cannot write to {}. Choose another location.",
            dir.display()
        ));
    }

    // Rough size estimate: an output is broadly the size of its input, so this
    // catches the obviously impossible without pretending to be exact.
    if let Some((free, _)) = drives::space(&dir) {
        let needed: u64 = files.iter().map(|f| f.size).sum();
        if needed > free {
            return Some(format!(
                "Not enough space: about {} needed, {} available.",
                render::human_bytes(needed),
                render::human_bytes(free)
            ));
        }
    }

    // Same format in and out is not an error, but it is almost never intended.
    if files.len() == 1 && first.format == format {
        return Some(format!(
            "This file is already {}. Choose a different output format.",
            format.to_uppercase()
        ));
    }
    None
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

// ---------------------------------------------------------------------------
// conversion
// ---------------------------------------------------------------------------

fn start_convert(ui: &Rc<App>) {
    if *ui.converting.borrow() || check(ui).is_some() {
        return;
    }
    let Some(format) = ui.output_picker.selected() else {
        return;
    };

    // Build every plan up front so information loss and existing files can be
    // dealt with once, rather than interrupting halfway through a batch.
    let mut plans = Vec::new();
    let mut losses: Vec<String> = Vec::new();
    let mut existing: Vec<PathBuf> = Vec::new();
    {
        let files = ui.files.borrow();
        for f in files.iter() {
            if f.opened.is_none() {
                continue;
            }
            let Some(dest) = destination_for(ui, f, format) else {
                continue;
            };
            match convert::plan(&f.path, format, &dest) {
                Ok(p) => {
                    for l in &p.losses {
                        let line = format!("{} — {}", l.what, l.because);
                        if !losses.contains(&line) {
                            losses.push(line);
                        }
                    }
                    if dest.exists() {
                        existing.push(dest.clone());
                    }
                    plans.push((f.path.clone(), p));
                }
                Err(e) => {
                    ui.toasts
                        .add_toast(adw::Toast::new(&format!("{}: {e}", f.name())));
                }
            }
        }
    }
    if plans.is_empty() {
        return;
    }

    let warn = ui.settings.borrow().warn_on_information_loss;
    let ui2 = ui.clone();
    let proceed = move |plans: Vec<(PathBuf, convert::Plan)>| run_batch(&ui2, plans);

    if !existing.is_empty() && ui.settings.borrow().confirm_overwrite {
        ask_overwrite(ui, plans, existing, losses, warn, Box::new(proceed));
    } else if warn && !losses.is_empty() {
        ask_losses(ui, plans, losses, Box::new(proceed));
    } else {
        proceed(plans);
    }
}

type Proceed = Box<dyn Fn(Vec<(PathBuf, convert::Plan)>)>;

fn ask_overwrite(
    ui: &Rc<App>,
    plans: Vec<(PathBuf, convert::Plan)>,
    existing: Vec<PathBuf>,
    losses: Vec<String>,
    warn: bool,
    proceed: Proceed,
) {
    let body = if existing.len() == 1 {
        format!("{} is already in that folder.", name_of(&existing[0]))
    } else {
        format!("{} files are already in that folder.", existing.len())
    };
    let d = adw::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .heading("Some files already exist")
        .body(body)
        .build();
    d.add_response("cancel", "Cancel");
    d.add_response("both", "Keep Both");
    d.add_response("replace", "Replace");
    d.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("both"));

    let ui2 = ui.clone();
    let proceed = Rc::new(proceed);
    d.connect_response(None, move |dlg, resp| {
        dlg.close();
        let mut plans = plans.clone();
        match resp {
            "replace" => {}
            "both" => {
                for (_, p) in plans.iter_mut() {
                    p.destination = convert::unique_path(&p.destination);
                }
            }
            _ => return,
        }
        if warn && !losses.is_empty() {
            ask_losses(
                &ui2,
                plans,
                losses.clone(),
                Box::new({
                    let p = proceed.clone();
                    move |plans| p(plans)
                }),
            );
        } else {
            proceed(plans);
        }
    });
    d.present();
}

fn ask_losses(
    ui: &Rc<App>,
    plans: Vec<(PathBuf, convert::Plan)>,
    losses: Vec<String>,
    proceed: Proceed,
) {
    let body = losses
        .iter()
        .map(|l| format!("• {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    let d = adw::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .heading("Some information cannot be preserved")
        .body(format!("Converting will drop:\n\n{body}"))
        .build();
    let dont_ask = gtk::CheckButton::with_label("Do not ask me again");
    dont_ask.set_tooltip_text(Some("You can turn this back on in Settings."));
    dont_ask.set_margin_top(theme::SPACE_3);
    d.set_extra_child(Some(&dont_ask));
    d.add_response("cancel", "Cancel");
    d.add_response("go", "Convert Anyway");
    d.set_response_appearance("go", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("go"));

    let ui2 = ui.clone();
    d.connect_response(None, move |dlg, resp| {
        dlg.close();
        if resp != "go" {
            return;
        }
        // Only remembered when the user actually proceeded: ticking the box
        // then cancelling should not silence future warnings.
        if dont_ask.is_active() {
            let mut s = ui2.settings.borrow_mut();
            s.warn_on_information_loss = false;
            let _ = s.save();
        }
        proceed(plans.clone());
    });
    d.present();
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn run_batch(ui: &Rc<App>, plans: Vec<(PathBuf, convert::Plan)>) {
    *ui.converting.borrow_mut() = true;
    ui.convert_btn.set_sensitive(false);
    ui.convert_label.set_text("Converting…");
    ui.problem.set_visible(false);
    ui.progress.set_visible(true);
    ui.progress.set_fraction(0.0);
    ui.penguin.start();

    let total_files = plans.len();
    {
        // Mark everything queued as in progress, so the list reflects what is
        // happening rather than staying on "Ready" throughout.
        let sources: Vec<PathBuf> = plans.iter().map(|(s, _)| s.clone()).collect();
        let mut files = ui.files.borrow_mut();
        for f in files.iter_mut() {
            if sources.contains(&f.path) {
                f.status = Status::Converting;
            }
        }
    }
    refresh_queue(ui);
    let (ptx, prx) = async_channel::unbounded::<(usize, String, u32, u32)>();
    let (dtx, drx) = async_channel::bounded(1);

    std::thread::spawn(move || {
        let mut results: Vec<(PathBuf, convert::Plan, Result<std::time::Duration, String>)> =
            Vec::new();
        for (i, (source, plan)) in plans.into_iter().enumerate() {
            let name = plan
                .destination
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let tx = ptx.clone();
            let n = name.clone();
            let started = std::time::Instant::now();
            let outcome = convert::run_with_progress(&plan, move |done, total| {
                let _ = tx.try_send((i, n.clone(), done, total));
            })
            .map(|_| started.elapsed())
            .map_err(|e| e.to_string());
            results.push((source, plan, outcome));
        }
        drop(ptx);
        let _ = dtx.send_blocking(results);
    });

    {
        let ui = ui.clone();
        let started = std::time::Instant::now();
        glib::spawn_future_local(async move {
            while let Ok((index, name, done, total)) = prx.recv().await {
                if total == 0 {
                    continue;
                }
                let within = done as f64 / total as f64;
                let overall = (index as f64 + within) / total_files as f64;
                ui.progress.set_fraction(overall);
                let elapsed = started.elapsed().as_secs_f64();
                let text = if overall > 0.02 {
                    let remaining = elapsed / overall - elapsed;
                    format!(
                        "{name}  ·  layer {done} of {total}  ·  about {} left",
                        render::human_time(remaining.max(0.0).round() as u64)
                    )
                } else {
                    format!("{name}  ·  layer {done} of {total}")
                };
                ui.progress.set_text(Some(&text));
                if total_files > 1 {
                    ui.convert_label
                        .set_text(&format!("Converting {} of {total_files}…", index + 1));
                }
            }
        });
    }

    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let Ok(results) = drx.recv().await else {
            return;
        };
        ui.penguin.stop();
        ui.progress.set_visible(false);
        *ui.converting.borrow_mut() = false;

        let mut ok = 0usize;
        let mut failed = 0usize;
        let mut last_dest: Option<PathBuf> = None;
        {
            let mut files = ui.files.borrow_mut();
            let mut hist = ui.history.borrow_mut();
            for (source, plan, outcome) in &results {
                let entry_status = match outcome {
                    Ok(_) => {
                        ok += 1;
                        last_dest = Some(plan.destination.clone());
                        Status::Complete(plan.destination.clone())
                    }
                    Err(e) => {
                        failed += 1;
                        Status::Failed(e.clone())
                    }
                };
                if let Some(f) = files.iter_mut().find(|f| &f.path == source) {
                    f.status = entry_status;
                }
                hist.record(history::Entry {
                    when: history::now(),
                    source: source.clone(),
                    destination: plan.destination.clone(),
                    from_format: plan.from.id.to_string(),
                    to_format: plan.to.id.to_string(),
                    layers: plan.layer_count,
                    outcome: match outcome {
                        Ok(_) => history::Outcome::Complete,
                        Err(_) => history::Outcome::Failed,
                    },
                    detail: outcome.as_ref().err().cloned().unwrap_or_default(),
                });
            }
        }

        if let Some(dest) = last_dest.as_ref().and_then(|d| d.parent()) {
            let mut s = ui.settings.borrow_mut();
            s.remember_output_dir(dest);
            let _ = s.save();
        }

        refresh_queue(&ui);
        refresh_history(&ui);
        ui.convert_label.set_text("Convert");
        revalidate(&ui);

        let title = match (ok, failed) {
            (1, 0) => "Conversion complete".to_string(),
            (n, 0) => format!("{n} files converted"),
            (0, n) => format!("{n} failed"),
            (a, b) => format!("{a} converted, {b} failed"),
        };
        let toast = adw::Toast::builder().title(title).timeout(6).build();
        if let Some(dest) = last_dest {
            // The file itself rather than the folder it is in (§27). Opening
            // the folder was all there was, which left the last step —
            // checking the thing that was just made — to be done by hand.
            // Falls back to the folder when nothing claims the format, which
            // is likely: few desktops know what a .goo is.
            let single = ok == 1 && failed == 0;
            toast.set_button_label(Some(if single { "Open File" } else { "Open Folder" }));
            toast.connect_button_clicked(move |_| {
                let opened = single
                    && gio::AppInfo::launch_default_for_uri(
                        &gio::File::for_path(&dest).uri(),
                        gio::AppLaunchContext::NONE,
                    )
                    .is_ok();
                if !opened {
                    if let Some(parent) = dest.parent() {
                        let uri = gio::File::for_path(parent).uri();
                        let _ =
                            gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
                    }
                }
            });
        }
        ui.toasts.add_toast(toast);
    });
}

// ---------------------------------------------------------------------------
// history
// ---------------------------------------------------------------------------

fn refresh_history(ui: &Rc<App>) {
    while let Some(row) = ui.history_list.first_child() {
        ui.history_list.remove(&row);
    }
    let entries = ui.history.borrow().entries.clone();
    if entries.is_empty() {
        ui.history_stack.set_visible_child_name("empty");
        return;
    }
    ui.history_stack.set_visible_child_name("list");

    for (i, e) in entries.iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
        row.set_margin_top(theme::SPACE_2);
        row.set_margin_bottom(theme::SPACE_2);
        row.set_margin_start(theme::SPACE_3);
        row.set_margin_end(theme::SPACE_2);

        let icon = gtk::Image::from_icon_name(match e.outcome {
            history::Outcome::Complete => "object-select-symbolic",
            history::Outcome::Failed => "dialog-error-symbolic",
        });
        icon.add_css_class(match e.outcome {
            history::Outcome::Complete => "cz-ok",
            history::Outcome::Failed => "cz-error",
        });
        row.append(&icon);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_hexpand(true);
        let title = gtk::Label::builder()
            .label(format!("{} → {}", e.source_name(), e.destination_name()))
            .xalign(0.0)
            .build();
        title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let sub = gtk::Label::builder()
            .label(format!(
                "{} → {}  ·  {} layers  ·  {}",
                e.from_format.to_uppercase(),
                e.to_format.to_uppercase(),
                e.layers,
                ago(e.when)
            ))
            .xalign(0.0)
            .build();
        sub.add_css_class("caption");
        sub.add_css_class("cz-dim");
        text.append(&title);
        text.append(&sub);
        row.append(&text);

        // A deleted output is stated rather than offered and then failing.
        if e.output_exists() {
            let open = shell::icon_button("folder-open-symbolic", "Open containing folder");
            let dest = e.destination.clone();
            open.connect_clicked(move |_| {
                if let Some(parent) = dest.parent() {
                    let uri = gio::File::for_path(parent).uri();
                    let _ = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
                }
            });
            row.append(&open);
        } else {
            let missing = shell::status_chip("dialog-warning-symbolic", "Moved", "cz-dim");
            missing.set_tooltip_text(Some(&format!(
                "{} is not there any more",
                e.destination.display()
            )));
            row.append(&missing);
        }

        let remove = shell::icon_button("window-close-symbolic", "Remove from history");
        let ui2 = ui.clone();
        remove.connect_clicked(move |_| {
            ui2.history.borrow_mut().remove(i);
            refresh_history(&ui2);
        });
        row.append(&remove);

        ui.history_list
            .append(&gtk::ListBoxRow::builder().child(&row).build());
    }

    let clear = gtk::Button::builder().halign(gtk::Align::Start).build();
    clear.set_child(Some(&labelled_icon("user-trash-symbolic", "Clear History")));
    clear.add_css_class("flat");
    clear.add_css_class("cz-destructive");
    let ui2 = ui.clone();
    clear.connect_clicked(move |_| {
        ui2.history.borrow_mut().clear();
        refresh_history(&ui2);
    });
    ui.history_list.append(
        &gtk::ListBoxRow::builder()
            .child(&clear)
            .selectable(false)
            .build(),
    );
}

/// Rough, readable relative time. Exactness is not the point here.
fn ago(when: u64) -> String {
    let now = history::now();
    let d = now.saturating_sub(when);
    match d {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86399 => format!("{}h ago", d / 3600),
        86400..=172_799 => "yesterday".to_string(),
        _ => format!("{} days ago", d / 86400),
    }
}

// ---------------------------------------------------------------------------
// settings page
// ---------------------------------------------------------------------------

fn build_settings_page(ui: &Rc<App>, container: &gtk::Box) {
    let page = adw::PreferencesPage::new();

    let conversion = adw::PreferencesGroup::builder()
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
        warn.connect_active_notify(move |r| {
            let mut s = ui.settings.borrow_mut();
            s.warn_on_information_loss = r.is_active();
            let _ = s.save();
        });
    }
    conversion.add(&warn);

    let overwrite = adw::SwitchRow::builder()
        .title("Confirm before replacing a file")
        .subtitle("Ask when a file of the same name is already there")
        .active(current.confirm_overwrite)
        .build();
    {
        let ui = ui.clone();
        overwrite.connect_active_notify(move |r| {
            let mut s = ui.settings.borrow_mut();
            s.confirm_overwrite = r.is_active();
            let _ = s.save();
        });
    }
    conversion.add(&overwrite);
    page.add(&conversion);

    let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
    let animate = adw::SwitchRow::builder()
        .title("Animate the interface")
        .subtitle("The sidebar folding and pages changing. Off, they happen at once")
        .active(current.animations)
        .build();
    {
        let ui = ui.clone();
        animate.connect_active_notify(move |r| {
            let on = r.is_active();
            ui.shell.set_animate(on);
            let mut s = ui.settings.borrow_mut();
            s.animations = on;
            let _ = s.save();
        });
    }
    appearance.add(&animate);
    page.add(&appearance);

    let opening = adw::PreferencesGroup::builder()
        .title("Opening files")
        .description("Where the file chooser starts")
        .build();
    let open_row = adw::ActionRow::builder()
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
    let reset = shell::icon_button("edit-undo-symbolic", "Use the last folder instead");
    {
        let ui = ui.clone();
        let row = open_row.clone();
        choose.connect_clicked(move |_| {
            let dlg = gtk::FileDialog::builder().title("Default folder").build();
            let ui2 = ui.clone();
            let row2 = row.clone();
            dlg.select_folder(
                Some(&ui.window.clone()),
                gio::Cancellable::NONE,
                move |res| {
                    if let Ok(f) = res {
                        if let Some(p) = f.path() {
                            row2.set_subtitle(&p.display().to_string());
                            let mut s = ui2.settings.borrow_mut();
                            s.default_open_dir = Some(p);
                            let _ = s.save();
                        }
                    }
                },
            );
        });
    }
    {
        let ui = ui.clone();
        let row = open_row.clone();
        reset.connect_clicked(move |_| {
            let mut s = ui.settings.borrow_mut();
            s.default_open_dir = None;
            let _ = s.save();
            row.set_subtitle("Wherever the last file was opened from");
        });
    }
    open_row.add_suffix(&choose);
    open_row.add_suffix(&reset);
    opening.add(&open_row);
    page.add(&opening);

    let drives_group = adw::PreferencesGroup::builder()
        .title("Drives")
        .description(
            "Pinned drives appear in the Save to menu. They are remembered by name, \
             so they still work when the mount point changes.",
        )
        .build();
    let sub_row = adw::EntryRow::builder()
        .title("Subfolder on pinned drives")
        .build();
    sub_row.set_text(&current.pinned_subfolder);
    {
        let ui = ui.clone();
        sub_row.connect_changed(move |r| {
            let mut s = ui.settings.borrow_mut();
            s.pinned_subfolder = r.text().trim().trim_matches('/').to_string();
            let _ = s.save();
        });
    }
    drives_group.add(&sub_row);

    let mounted = drives::mounted();
    if mounted.is_empty() {
        drives_group.add(
            &adw::ActionRow::builder()
                .title("No drives detected")
                .subtitle("Connect a USB drive or SD card and it will appear here")
                .build(),
        );
    }
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
        row.add_prefix(&gtk::Image::from_icon_name(if d.removable {
            "drive-removable-media-symbolic"
        } else {
            "drive-harddisk-symbolic"
        }));
        let ui2 = ui.clone();
        let name = d.name.clone();
        row.connect_active_notify(move |r| {
            let mut s = ui2.settings.borrow_mut();
            if r.is_active() {
                s.pin_volume(&name);
            } else {
                s.unpin_volume(&name);
            }
            let _ = s.save();
        });
        drives_group.add(&row);
    }
    page.add(&drives_group);

    let about = adw::PreferencesGroup::builder().title("About").build();
    about.add(
        &adw::ActionRow::builder()
            .title("CheapAzSLA")
            .subtitle(format!("Version {}", cheapazsla_core::VERSION))
            .build(),
    );
    let formats: Vec<String> = registry::handlers()
        .iter()
        .map(|h| {
            let i = h.info();
            let mut caps = Vec::new();
            if i.capabilities.reads {
                caps.push("read");
            }
            if i.capabilities.writes {
                caps.push("write");
            }
            format!("{} ({})", i.name, caps.join(", "))
        })
        .collect();
    about.add(
        &adw::ActionRow::builder()
            .title("Supported formats")
            .subtitle(formats.join("\n"))
            .build(),
    );
    let repo = adw::ActionRow::builder()
        .title("Source code")
        .subtitle("github.com/CheapAzHobbies/CheapAzPrintingSLA")
        .activatable(true)
        .build();
    repo.connect_activated(|_| {
        let _ = gio::AppInfo::launch_default_for_uri(
            "https://github.com/CheapAzHobbies/CheapAzPrintingSLA",
            gio::AppLaunchContext::NONE,
        );
    });
    about.add(&repo);
    let settings_file = adw::ActionRow::builder()
        .title("Settings file")
        .subtitle(
            Settings::path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not available".into()),
        )
        .build();
    about.add(&settings_file);
    page.add(&about);

    container.append(&page);
}

/// Restore what the last session was doing (§37: does it remember?).
fn restore_session(ui: &Rc<App>) {
    let saved = ui.settings.borrow().clone();

    // Recently used formats feed the picker's own section.
    let mut recents: Vec<String> = Vec::new();
    if let Some(f) = saved.last_output_format.clone() {
        recents.push(f);
    }
    for e in ui.history.borrow().entries.iter().take(12) {
        if !recents.contains(&e.to_format) {
            recents.push(e.to_format.clone());
        }
    }
    ui.output_picker.set_recents(recents);

    let chosen = saved
        .last_output_format
        .filter(|id| registry::by_id(id).map(|h| h.info().capabilities.writes) == Some(true))
        .or_else(|| registry::writable().first().map(|i| i.id.to_string()));
    if let Some(id) = chosen {
        ui.output_picker.set_selected(&id);
    }

    // A remembered folder is only restored if it is still there.
    if let Some(dir) = saved.last_output_dir.filter(|d| d.is_dir()) {
        set_out_dir(ui, Some(dir));
    } else {
        set_out_dir(ui, None);
    }
    revalidate(ui);
}
