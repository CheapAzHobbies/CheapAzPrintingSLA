//! CheapAzSLA desktop application.
//!
//! A thin shell over cheapazsla-core. All parsing, decoding, validation and
//! conversion happen in the engine; this crate decides what to show and when.
//!
//! Anything the engine cannot do is presented as unavailable rather than
//! mocked up (§47 of the product specification).

mod auto;
mod drives;
mod format_picker;
mod nearby;
mod palette;
mod penguin;
mod render;
mod shell;
mod steps;
mod theme;
mod viewer;

/// How long the controls take to fade in once a file is queued. Short: this
/// is motion that explains a change, not a wait to sit through.
const CONTROLS_MS: u32 = 200;
/// Roughly how long an AdwExpanderRow takes to fold. libadwaita exposes no
/// "finished" signal for it, so the step that follows is timed to match
/// rather than chained to it: being slightly late looks fine, being early is
/// the clunk.
const EXPANDER_MS: u64 = 220;
/// How long the header's corners take to square off or round again. Must match
/// the border-radius transition in the stylesheet, because the list waits this
/// long before dropping so the two read as one thing after another.
const CORNER_MS: u64 = 120;
/// The corner's own radius, from the stylesheet. Once the folding list is
/// shorter than this, rounding the header cannot put a curve through anything.
const CORNER_RADIUS: f64 = 12.0;
/// WatchDog's picture: a folder with a search over it. Deliberately not an
/// eye - Preview is the eye, and one picture should mean one thing.
const WATCHDOG_ICON: &str = "folder-saved-search-symbolic";
/// How long the drop zone takes to become the queue, and back.
const MORPH_MS: u32 = 240;
/// And how long the form waits before following it down, so the two read as
/// one thing after another rather than everything moving at once.
const STAGGER_MS: u64 = 150;

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
    /// Whether this one goes when Convert is pressed.
    ///
    /// On by default: adding a file is asking for it to be converted. It comes
    /// off by itself once the file has been converted, so pressing Convert
    /// twice does not do the same work again.
    selected: bool,
    /// When the source was last written, as of the last time it was read.
    /// A file that has changed since is worth offering again.
    edited: Option<std::time::SystemTime>,
    /// Set when the source changed on disk after being converted.
    changed_since: bool,
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
    dropzone_sub: gtk::Label,
    /// The milestone chain, and what it is currently saying.
    watchdog_steps: Rc<steps::Steps>,
    /// The file being converted right now, for the chain's third stop.
    watchdog_doing: RefCell<Option<String>>,
    /// True while a converted file is being written onto the drive, which is
    /// the one leg of the chain that has no progress to report and so is the
    /// one that has to bounce.
    watchdog_sending: Cell<bool>,
    /// What WatchDog last finished, so the chain can say it is round again
    /// rather than only that it is waiting.
    watchdog_ready: RefCell<Option<String>>,
    /// Whether the chain was already holding a file last time it was drawn,
    /// so the leg into "New file" is filled once when one turns up rather than
    /// restarted on every redraw.
    watchdog_found: Cell<bool>,
    /// A file that turned up and could not be converted, and why. Kept until
    /// the next one arrives or it is dismissed, because a failure that clears
    /// itself in a second is a failure nobody reads.
    watchdog_trouble: RefCell<Option<(String, String)>>,
    /// Whether a file has actually reached the drive. The one thing that
    /// turns the last stop green, and it is cleared the moment the next file
    /// starts - a chain that stays green is a chain reporting an old success.
    watchdog_landed: Cell<bool>,
    /// The list of readable formats under the drop zone. First thing out when
    /// the window is squeezed: it is a reference, not an instruction, and the
    /// instruction above it still stands without it.
    dropzone_formats: Fold,
    /// WatchDog's chain, which appears and disappears with the mode and so
    /// moves everything below it when it does.
    watchdog_fold: Fold,
    /// "Found nearby": readable files in the open folder and on mounted
    /// drives, offered so a file can be picked without a file dialog.
    nearby_panel: gtk::Box,
    /// Collapsed by default: it is an alternative to the file dialog, not
    /// something to read every time the page is opened.
    nearby_expander: adw::ExpanderRow,
    /// Opens the list of folders and drives Quick Access may look in.
    nearby_sources: gtk::MenuButton,
    /// Holds the list at whatever height the animation is currently on.
    nearby_clip: gtk::ScrolledWindow,
    /// The header, outside the scroller so it stays put under the files.
    nearby_head_list: gtk::ListBox,
    /// The file rows, inside it.
    nearby_rows_list: gtk::ListBox,
    nearby_refresh: gtk::Button,
    /// Counts scans, so a slow one finishing after a later one started cannot
    /// overwrite it.
    scan_gen: Cell<u64>,
    /// When the running scan began, and whether it was asked for by hand.
    scan_since: Cell<Option<std::time::Instant>>,
    scan_asked: Cell<bool>,
    /// When the arrow actually started turning, which is not when the scan
    /// started - an automatic one waits to see whether it is slow enough to be
    /// worth saying anything about.
    spin_since: Cell<Option<std::time::Instant>>,
    /// Filters the rows already on screen. Never triggers a rescan: it is for
    /// finding a file among the ones offered, not for looking harder.
    nearby_search: gtk::Entry,
    /// The width the field is opening to, fixed when it starts moving.
    search_full: Rc<Cell<i32>>,
    /// How far open the field is, where it is heading, and whether a tick
    /// callback is walking it there.
    search_t: Rc<Cell<f64>>,
    search_open: Rc<Cell<bool>>,
    search_moving: Rc<Cell<bool>>,
    /// Whether searching is being offered at all, which it is not when there
    /// are no files to search.
    nearby_search_shown: Cell<bool>,
    /// What the expander says when nothing is being searched for, so the count
    /// can be put back when the box is cleared.
    nearby_subtitle: RefCell<String>,
    /// Where that height is now, where it is heading, and whether a tick
    /// callback is already walking it there.
    /// The height to come to rest at, and whether that height is a cap that
    /// should scroll. Held in a cell because the tick callback settles the
    /// list and deliberately holds no handle back to `App`.
    nearby_cap: Rc<Cell<i32>>,
    nearby_h: Rc<Cell<f64>>,
    nearby_from: Rc<Cell<f64>>,
    nearby_target: Rc<Cell<f64>>,
    nearby_elapsed: Rc<Cell<f64>>,
    nearby_moving: Rc<Cell<bool>>,
    /// Counts animations, so a safety net can tell whether the one it was
    /// watching is still the one running.
    nearby_gen: Rc<Cell<u64>>,
    /// The rows currently inside the expander, so a refresh can take them out
    /// again without rebuilding the row and losing whether it was open.
    nearby_rows: RefCell<Vec<adw::ActionRow>>,
    /// Lowercased name and facts for each row, in the same order, so the
    /// search can match what the row shows in columns rather than as text.
    nearby_keys: RefCell<Vec<String>>,
    /// The rows the filter is currently letting through.
    ///
    /// Kept rather than asked for, because a widget reports itself invisible
    /// when any ancestor is - and the whole list is hidden while it is shut,
    /// which made every row look filtered out and the list measure as nothing.
    nearby_shown: RefCell<Vec<adw::ActionRow>>,
    /// Held so the volume-monitor signal handlers outlive `wire`.
    volume_monitor: RefCell<Option<gio::VolumeMonitor>>,
    queue_list: gtk::ListBox,
    controls: gtk::Box,
    /// Wraps `controls` so the form swings down behind the queue rather than
    /// the whole page changing in one frame.
    controls_reveal: gtk::Revealer,
    /// The drop zone and the queue, as two faces of one place.
    page_faces: gtk::Stack,
    input_label: gtk::Label,
    /// Opens the list of formats the input can be read as.
    input_button: gtk::MenuButton,
    output_picker: Rc<format_picker::FormatPicker>,
    swap_btn: gtk::Button,
    dest_button: gtk::MenuButton,
    /// Eject, beside the destination. Shown only while the output is going to
    /// a drive that can be ejected, because that is the moment it is wanted:
    /// saving to a stick and then hunting through Settings to release it is
    /// the wrong shape for the task.
    eject_btn: gtk::Button,
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
    /// A tick box per history row, in the order the rows are shown, so several
    /// can be taken out in one go rather than one X at a time.
    history_ticks: RefCell<Vec<gtk::CheckButton>>,
    /// Held so the folder monitor outlives the function that made it.
    auto_watch: RefCell<Option<gio::FileMonitor>>,
    /// The eye in the title bar: the switch and the indicator in one.
    watchdog_eye: gtk::ToggleButton,
    /// And its row on the Convert page, which says the whole sentence the eye
    /// can only hint at.
    watchdog_row: adw::ActionRow,
    watchdog_switch: gtk::Switch,
    /// The two things it has to be told, on the page where it is switched on.
    watchdog_folder: adw::ActionRow,
    watchdog_drive: adw::ActionRow,

    watchdog_folder_btn: gtk::Button,
    /// The row saying what it converts into, and the button that changes it.
    watchdog_format: adw::ActionRow,
    watchdog_format_btn: gtk::MenuButton,
    /// Eject, shown only when the destination is a drive that is actually here.
    watchdog_eject: gtk::Button,
    /// Empties WatchDog's own entries from history.
    watchdog_clear: gtk::Button,
    /// Where the extra numbers live on WatchDog's own page.
    watchdog_more: adw::ExpanderRow,
    /// The list of what WatchDog has done on its own.
    watchdog_recent: gtk::ListBox,
    watchdog_drive_btn: gtk::MenuButton,
    /// Held, because nothing else holds it. A size group is a plain object
    /// and the widgets in it do not keep it alive - let the last reference go
    /// and the widths quietly stop being equal.
    watchdog_widths: gtk::SizeGroup,
    /// The same for the queue's columns, which are rebuilt on every refresh.
    queue_columns: RefCell<Vec<gtk::SizeGroup>>,
    /// The Settings switch, kept in step with the eye. The page is built once,
    /// so it cannot find out on its own that the eye has been pressed.
    auto_switch: RefCell<Option<adw::SwitchRow>>,
    /// Files seen but not yet finished being written.
    auto_settling: RefCell<Vec<auto::Settling>>,
    /// Files ready to convert, and whether one is being converted now.
    ///
    /// One at a time, always. Six files landing together on a four-core
    /// machine that is also running a slicer is how an automatic feature
    /// becomes the reason somebody's computer stopped responding.
    auto_queue: RefCell<std::collections::VecDeque<PathBuf>>,
    auto_busy: Cell<bool>,
    /// Whether the settling loop is already running, so a folder that fires
    /// twenty change events does not end up with twenty timers.
    auto_ticking: Cell<bool>,
    /// What has already been converted, so nothing is done twice. Keyed by
    /// path, size and modification time - a file re-sliced over the same name
    /// is a different file and is converted again.
    auto_done: RefCell<Vec<String>>,
    /// The Settings switch for the overwrite question, so turning the question
    /// off from the question itself is visible next time Settings is opened.
    /// The page is built once, so it cannot find out on its own.
    overwrite_switch: RefCell<Option<adw::SwitchRow>>,
    history_stack: gtk::Stack,

    // state
    files: RefCell<Vec<Queued>>,
    selected: RefCell<usize>,
    out_dir: RefCell<Option<PathBuf>>,
    /// The output is following whichever removable drive is connected, rather
    /// than a fixed folder. Re-resolved whenever a drive comes or goes.
    out_auto_drive: Cell<bool>,
    /// The removable drive the destination sits on, if it sits on one. Held by
    /// name, so the answer survives the drive being unplugged - which is the
    /// only moment it is wanted.
    out_drive: RefCell<Option<String>>,
    /// What the destination row says before anything is added about the drive
    /// being missing. Kept so the row can be redrawn when a drive comes or
    /// goes without working out its name and free space again.
    dest_base: RefCell<String>,
    dest_base_detail: RefCell<String>,
    /// Label of the removable drive that mounted most recently, so "connected
    /// drive" means the one just plugged in when several are attached.
    last_drive: RefCell<Option<String>>,
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
    // The overlay lives inside the shell, wrapping the page stack, so a toast
    // is centred on the page rather than on the window.
    let toasts = shell.toasts.clone();

    // Without this the window has no titlebar, and therefore no minimise,
    // maximise or close. A window that can only be shut with a keyboard
    // shortcut or a kill is broken however good the rest of it looks.
    let header = adw::HeaderBar::builder()
        .show_title(false)
        .css_classes(["flat"])
        .build();
    let palette_btn = shell::icon_button("system-search-symbolic", "Commands  (Ctrl+K)");
    header.pack_start(&palette_btn);

    // WatchDog in the title bar, on every page: the switch and the indicator
    // in one, so there is nothing to keep in step and nowhere else to look.
    //
    // A watched folder rather than an eye, because Preview already is an eye
    // and two of the same picture meaning two different things is worse than
    // either picture being wrong. This one says what it actually does.
    let watchdog_eye = gtk::ToggleButton::builder()
        .child(&gtk::Image::from_icon_name(WATCHDOG_ICON))
        .tooltip_text("WatchDog is off")
        .build();
    watchdog_eye.set_widget_name("watchdog-eye");
    header.pack_end(&watchdog_eye);

    // --- convert page -----------------------------------------------------
    // WatchDog has its own page now. It shared the Convert page for a while
    // and the seam showed: Convert is "I have files, do this to them"; this is
    // "watch that place and act without me". They want opposite things from
    // the same space - one wants a drop target, the other wants to say what it
    // is doing - and every layout that served both served neither. Two pages,
    // one job each.
    let watchdog_row = adw::ActionRow::builder()
        .title("WatchDog")
        .subtitle("Off")
        .title_lines(1)
        .subtitle_lines(1)
        .build();
    watchdog_row.add_prefix(&gtk::Image::from_icon_name(WATCHDOG_ICON));
    let watchdog_switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .tooltip_text("Watch a folder and convert what your slicer leaves there")
        .build();
    watchdog_row.add_suffix(&watchdog_switch);

    let watchdog_folder = adw::ActionRow::builder()
        .title("Folder to watch")
        .subtitle("Not chosen")
        .subtitle_lines(1)
        .build();
    let choose_folder = gtk::Button::with_label("Choose…");
    choose_folder.set_valign(gtk::Align::Center);
    watchdog_folder.add_suffix(&choose_folder);

    let watchdog_format = adw::ActionRow::builder()
        .title("Convert to")
        .subtitle("GOO")
        .subtitle_lines(1)
        .build();
    let choose_format = gtk::MenuButton::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .tooltip_text("Pick the format WatchDog converts into")
        .build();
    watchdog_format.add_suffix(&choose_format);

    let watchdog_drive = adw::ActionRow::builder()
        .title("Save into")
        .subtitle("Not chosen")
        .subtitle_lines(1)
        .build();
    // A menu of the drives that are actually plugged in, rather than a button
    // that silently takes the first one it finds. With two sticks attached
    // there was no way to say which, and with one there was no way to see that
    // it had understood which.
    let choose_drive = gtk::MenuButton::builder()
        .label("Choose…")
        .valign(gtk::Align::Center)
        .tooltip_text("Pick the drive or folder WatchDog saves into")
        .build();
    // Ejecting belongs next to the drive it would eject. WatchDog writes to
    // that stick without being asked, so the moment you want to take it to the
    // printer is the moment you need to be sure the writing has finished - and
    // that is this page, not a different one.
    let watchdog_eject = shell::icon_button("media-eject-symbolic", "Finish writing and eject");
    watchdog_eject.set_visible(false);
    watchdog_drive.add_suffix(&watchdog_eject);
    watchdog_drive.add_suffix(&choose_drive);

    let watchdog_widths = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);

    // Opens where it is rather than sending you to Settings. Being thrown to
    // another page to change a number, and then having to find your way back
    // to the thing the number was about, is two navigations to answer one
    // question. The same rows exist in Settings for anyone who looks there
    // first; both write the same values.
    let watchdog_more = adw::ExpanderRow::builder()
        .title("Everything else")
        .subtitle("Where files wait, and how much they may use")
        .subtitle_lines(1)
        .build();

    let (dropzone, dropzone_title, dropzone_sub, dropzone_formats) = build_dropzone();

    // The chain: where a file comes from, what happens to it, and where it
    // ends up. Shown in the drop zone because that is the empty middle of the
    // page and the place someone looks to find out what is going on.
    let watchdog_steps = steps::Steps::new(&[
        ("folder-saved-search-symbolic", "Folder"),
        ("text-x-generic-symbolic", "New file"),
        ("media-playlist-repeat-symbolic", "Convert"),
        ("drive-removable-media-symbolic", "Drive"),
    ]);
    // Folded rather than shown, so switching the mode on does not shove the
    // rows below it down the page in a single frame.
    let watchdog_fold = Fold::new(&watchdog_steps.widget);
    watchdog_fold.land(0);
    let nearby = build_nearby_panel();
    let nearby_panel = nearby.panel.clone();
    let queue_list = gtk::ListBox::new();
    queue_list.set_selection_mode(gtk::SelectionMode::Single);
    queue_list.add_css_class("cz-queue");
    let queue_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    queue_panel.add_css_class("cz-panel");
    queue_panel.append(&queue_list);
    // Fills the row rather than hugging its label, so the whole strip under
    // the queue is the click target. The contents still sit at the left; it
    // is only the button that is wide. A 110px target in an 834px row is a
    // needle to hit for the main way of adding a file.
    let add_more = gtk::Button::builder().label("Add Files").build();
    add_more.add_css_class("flat");
    add_more.set_hexpand(true);
    // Matched to the height of the Quick Access row that sits under it.
    let add_label = labelled_icon("list-add-symbolic", "Add Files");
    add_label.set_halign(gtk::Align::Start);
    add_label.set_margin_top(theme::SPACE_3);
    add_label.set_margin_bottom(theme::SPACE_3);
    add_more.set_child(Some(&add_label));

    // Emptying the queue, at the other end of the same strip. Taking files out
    // one X at a time is fine for the three you meant to add and unusable for
    // the hundred you did not, and the row under the queue is where anyone
    // looking to change what is in it will already be looking.
    let clear_all = gtk::Button::builder().build();
    clear_all.add_css_class("flat");
    clear_all.set_widget_name("queue-clear");
    let clear_label = labelled_icon("user-trash-symbolic", "Clear Files");
    clear_all.add_css_class("cz-bin");
    clear_label.set_halign(gtk::Align::End);
    clear_label.set_margin_top(theme::SPACE_3);
    clear_label.set_margin_bottom(theme::SPACE_3);
    clear_all.set_child(Some(&clear_label));
    clear_all.set_tooltip_text(Some("Take every file out of the list"));

    let add_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    add_row.append(&add_more);
    add_row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    add_row.append(&clear_all);
    queue_panel.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    queue_panel.append(&add_row);

    // Detected, but overridable (§21). Detection reads the contents rather
    // than the name, which is right almost always and wrong occasionally: a
    // format can be a container another format also uses, and a file can be
    // truncated before the part that identifies it. When someone knows better
    // than the detector they need a way to say so, and the place they will
    // look is the box that told them what it thinks the file is.
    // Ellipsized, and asking for very little. A plain label's minimum width is
    // its whole text, and "(Detect Automatically)" beside the format name took
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
    let eject_btn = shell::icon_button("media-eject-symbolic", "Eject this drive");
    eject_btn.set_valign(gtk::Align::Center);
    eject_btn.set_visible(false);
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
        &nearby_panel,
        &queue_panel,
        &input_field,
        &output_picker,
        &swap_btn,
        &output_info,
        &dest_button,
        &eject_btn,
        &name_entry,
        &convert_btn,
        &progress,
        &penguin,
        &problem,
    );
    let controls = convert_page.1;
    let controls_reveal = convert_page.5;
    let page_faces = convert_page.6.clone();
    let name_row = convert_page.2;
    let format_row = convert_page.3;
    let swap_col = convert_page.4;
    shell.add_page(Section::Convert, &convert_page.0);

    // --- watchdog page ----------------------------------------------------
    let watchdog_recent = gtk::ListBox::new();
    watchdog_recent.add_css_class("boxed-list");
    watchdog_recent.set_selection_mode(gtk::SelectionMode::None);
    let watchdog_recent_group = adw::PreferencesGroup::builder()
        .title("Recently, automatically")
        .description("What WatchDog has converted without being asked")
        .build();
    // Clears WatchDog's own entries, not the whole history.
    let clear_recent = gtk::Button::with_label("Clear");
    clear_recent.add_css_class("flat");
    clear_recent.set_valign(gtk::Align::Center);
    clear_recent.set_tooltip_text(Some("Forget what WatchDog converted on its own"));
    watchdog_recent_group.set_header_suffix(Some(&clear_recent));
    watchdog_recent_group.add(&watchdog_recent);
    let watchdog_page = build_watchdog_page(
        &watchdog_row,
        &watchdog_fold,
        &watchdog_folder,
        &watchdog_format,
        &watchdog_drive,
        &watchdog_more,
        &watchdog_recent_group,
    );
    shell.add_page(Section::WatchDog, &watchdog_page);

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
    root.set_content(Some(&shell.widget));
    window.set_content(Some(&root));

    let ui = Rc::new(App {
        window: window.clone(),
        shell: shell.clone(),
        toasts,
        dropzone,
        dropzone_title,
        dropzone_sub,
        watchdog_steps,
        watchdog_doing: RefCell::new(None),
        watchdog_sending: Cell::new(false),
        watchdog_ready: RefCell::new(None),
        watchdog_found: Cell::new(false),
        watchdog_trouble: RefCell::new(None),
        watchdog_landed: Cell::new(false),
        dropzone_formats,
        watchdog_fold,
        nearby_panel: nearby_panel.clone(),
        nearby_expander: nearby.expander,
        nearby_clip: nearby.clip,
        nearby_head_list: nearby.head_list,
        nearby_rows_list: nearby.rows_list,
        nearby_refresh: nearby.refresh,
        scan_gen: Cell::new(0),
        scan_since: Cell::new(None),
        scan_asked: Cell::new(false),
        spin_since: Cell::new(None),
        nearby_search: nearby.search,
        search_full: Rc::new(Cell::new(SEARCH_WIDTH)),
        search_t: Rc::new(Cell::new(0.0)),
        search_open: Rc::new(Cell::new(false)),
        search_moving: Rc::new(Cell::new(false)),
        nearby_search_shown: Cell::new(false),
        nearby_subtitle: RefCell::new(String::new()),
        nearby_cap: Rc::new(Cell::new(-1)),
        nearby_h: Rc::new(Cell::new(0.0)),
        nearby_from: Rc::new(Cell::new(0.0)),
        nearby_target: Rc::new(Cell::new(0.0)),
        nearby_elapsed: Rc::new(Cell::new(0.0)),
        nearby_moving: Rc::new(Cell::new(false)),
        nearby_gen: Rc::new(Cell::new(0)),
        nearby_sources: nearby.sources.clone(),
        nearby_rows: RefCell::new(Vec::new()),
        nearby_keys: RefCell::new(Vec::new()),
        nearby_shown: RefCell::new(Vec::new()),
        volume_monitor: RefCell::new(None),
        queue_list,
        controls,
        controls_reveal,
        page_faces,
        input_label,
        input_button: input_button.clone(),
        output_picker: output_picker.clone(),
        swap_btn: swap_btn.clone(),
        dest_button: dest_button.clone(),
        eject_btn: eject_btn.clone(),
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
        history_ticks: RefCell::new(Vec::new()),
        overwrite_switch: RefCell::new(None),
        auto_watch: RefCell::new(None),
        watchdog_eye: watchdog_eye.clone(),
        watchdog_row: watchdog_row.clone(),
        watchdog_switch: watchdog_switch.clone(),
        watchdog_folder: watchdog_folder.clone(),
        watchdog_drive: watchdog_drive.clone(),
        watchdog_more: watchdog_more.clone(),
        watchdog_folder_btn: choose_folder.clone(),
        watchdog_format: watchdog_format.clone(),
        watchdog_format_btn: choose_format.clone(),
        watchdog_eject: watchdog_eject.clone(),
        watchdog_clear: clear_recent.clone(),
        watchdog_recent: watchdog_recent.clone(),
        watchdog_drive_btn: choose_drive.clone(),
        watchdog_widths: watchdog_widths.clone(),
        queue_columns: RefCell::new(Vec::new()),
        auto_switch: RefCell::new(None),
        auto_settling: RefCell::new(Vec::new()),
        auto_queue: RefCell::new(std::collections::VecDeque::new()),
        auto_busy: Cell::new(false),
        auto_ticking: Cell::new(false),
        auto_done: RefCell::new(Vec::new()),
        history_stack,
        files: RefCell::new(Vec::new()),
        selected: RefCell::new(0),
        out_dir: RefCell::new(None),
        out_auto_drive: Cell::new(false),
        out_drive: RefCell::new(None),
        dest_base: RefCell::new(String::new()),
        dest_base_detail: RefCell::new(String::new()),
        last_drive: RefCell::new(None),
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
    let animate = ui.settings.borrow().animations;
    ui.shell.set_animate(animate);
    ui.controls_reveal
        .set_transition_duration(if animate { CONTROLS_MS } else { 0 });
    ui.page_faces
        .set_transition_duration(if animate { MORPH_MS } else { 0 });
    wire_responsive(&ui);
    restore_session(&ui);
    rearm_auto(&ui);
    auto_deliver(&ui);
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
            "Licence",
            "GNU GPL v3 or later — free to use, change and share, and it cannot be closed up and sold",
            "https://www.gnu.org/licenses/gpl-3.0.html",
        ),
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
            ui.watchdog_steps.set_compact(level as usize);
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
                // The Quick Access rows give up their columns at the same
                // width the queue rows do.
                refresh_nearby(&ui);
                let smooth = ui.settings.borrow().animations;
                ui.dropzone_formats.set(!narrow, smooth);
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

/// Draw a protection lock as the state it is in, not as the thing it does.
///
/// A padlock that looks identical locked and unlocked says nothing, and this
/// one guards whether a drive can be ejected at all - worth being able to read
/// at a glance rather than by hovering.
fn show_lock(button: &gtk::ToggleButton) {
    if button.is_active() {
        button.set_icon_name("changes-prevent-symbolic");
        button.set_tooltip_text(Some("Locked: this drive will not be ejected"));
    } else {
        button.set_icon_name("changes-allow-symbolic");
        button.set_tooltip_text(Some("Unlocked: this drive can be ejected"));
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

/// The "Quick Access" panel: readable files already sitting somewhere the
/// program knows about, offered as one-click alternatives to the file dialog.
///
/// Named for what it does rather than for what it contains. "Available
/// Files" read as a restatement of the drop zone above it, which also offers
/// files; the useful distinction is that this one skips the dialog.
///
/// It sits below the queue so that it falls directly under "Add Files" once
/// there are files, and directly under the drop zone when there are none -
/// one position that reads correctly in both states.
///
/// Collapsed by default, and hidden entirely when the scan finds nothing, so
/// it costs one row of height rather than a list. That matters at the small
/// window sizes this layout was fought into fitting.
fn build_nearby_panel() -> NearbyPanel {
    // No spacing: the header and the file list are two boxed lists drawn to
    // look like one card, and a gap between them would give that away.
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.set_visible(false);

    // The header is its own list, outside the scrolled window, so it stays
    // put while the files move under it. Keeping it inside meant scrolling to
    // five files scrolled the search box and the buttons off the top as well.
    let head_list = gtk::ListBox::new();
    head_list.add_css_class("boxed-list");
    head_list.add_css_class("cz-qa-head");
    head_list.set_selection_mode(gtk::SelectionMode::None);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.add_css_class("cz-qa-body");
    list.set_selection_mode(gtk::SelectionMode::None);

    // The file list sits in a scrolled window for two reasons: it is allowed
    // to be a height its contents are not, which is what lets the panel be
    // walked between two heights instead of cutting to the new one, and it is
    // what caps a long list at a few rows and scrolls the rest.
    let clip = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .vexpand(false)
        .build();
    clip.add_css_class("cz-qa-clip");
    // Clipped to its own rounded box, which is what makes the bottom corners
    // the view's rather than the last row's.
    clip.set_overflow(gtk::Overflow::Hidden);

    let expander = adw::ExpanderRow::builder()
        .title("Quick Access")
        .expanded(false)
        // One line each. Wrapped, the count of files and the places they came
        // from grew the header downwards as the window narrowed, which is the
        // one row on the page that should not move.
        .title_lines(1)
        .subtitle_lines(1)
        .build();
    expander.add_prefix(&gtk::Image::from_icon_name("folder-open-symbolic"));

    let folder = gtk::MenuButton::builder()
        .icon_name("folder-symbolic")
        .tooltip_text("Choose which folders and drives to look in")
        .valign(gtk::Align::Center)
        .build();
    folder.add_css_class("flat");
    folder.set_widget_name("nearby-sources");
    expander.add_suffix(&folder);

    let refresh = shell::icon_button("view-refresh-symbolic", "Scan again for files");
    refresh.set_widget_name("nearby-refresh");
    refresh.set_valign(gtk::Align::Center);
    refresh.add_css_class("cz-refresh");
    expander.add_suffix(&refresh);
    // Search lives in the header rather than in a row of its own, because a
    // row of its own is a row of the list spent on something that is not a
    // file.
    //
    // The magnifying glass never goes anywhere. It is the button while the
    // list is shut, and it stays exactly where it is while the field opens
    // out beside it, ending up where a search field's icon belongs anyway -
    // so nothing pops out of existence where something else appears. That is
    // also why this is a plain entry rather than a search entry: a search
    // entry brings its own glass, and two of them was the whole problem.
    let search = gtk::Entry::new();
    // No character width. It looks like the natural way to size a field, but
    // width-chars is a minimum, and a size request can only ever raise a
    // minimum - so with it set the field could not be animated narrower than
    // sixteen characters and the growth did nothing at all. The width is
    // driven entirely by the request instead.
    search.set_width_chars(0);
    search.set_max_width_chars(0);
    // Back inside the box, where a search field's glass belongs. It was a
    // button beside the field before, which is a different thing wearing the
    // same icon; there is nothing outside the field to press now.
    search.set_primary_icon_name(Some("system-search-symbolic"));
    search.set_primary_icon_activatable(false);
    search.set_primary_icon_sensitive(false);
    search.set_valign(gtk::Align::Center);
    search.set_visible(false);

    expander.add_suffix(&search);

    // Lighting the header is done here rather than left to :hover, because the
    // pointer is over the row inside and the list never sees the prelight.
    let lift = gtk::EventControllerMotion::new();
    {
        let head = head_list.clone();
        lift.connect_enter(move |_, _, _| head.add_css_class("cz-qa-lit"));
    }
    {
        let head = head_list.clone();
        lift.connect_leave(move |_| head.remove_css_class("cz-qa-lit"));
    }
    head_list.add_controller(lift);

    head_list.append(&expander);
    clip.set_child(Some(&list));
    panel.append(&head_list);
    panel.append(&clip);

    NearbyPanel {
        panel,
        expander,
        sources: folder,
        clip,
        head_list,
        rows_list: list,
        refresh,
        search,
    }
}

/// The pieces of the Quick Access panel that have to be wired up afterwards.
struct NearbyPanel {
    panel: gtk::Box,
    expander: adw::ExpanderRow,
    sources: gtk::MenuButton,
    clip: gtk::ScrolledWindow,
    head_list: gtk::ListBox,
    rows_list: gtk::ListBox,
    refresh: gtk::Button,
    search: gtk::Entry,
}

/// WatchDog's numbers: where files wait, how much room they may take, and how
/// long they may take it for.
///
/// Built on demand rather than once, because they appear in two places - the
/// expander on WatchDog's own page, and the section in Settings - and a widget
/// has one parent. Both sets write the same settings and tell the rest of the
/// interface to catch up, so whichever you reach for, the other agrees.
fn watchdog_detail_rows(ui: &Rc<App>) -> Vec<adw::PreferencesRow> {
    let current = ui.settings.borrow().clone();

    let staging_row = adw::ComboRow::builder()
        .title("Where files wait")
        .subtitle("Until the drive is plugged in")
        .model(&gtk::StringList::new(&[
            "On disk",
            "In memory",
            "Do not convert until the drive is in",
        ]))
        .selected(match auto::Staging::from_id(&current.auto_staging) {
            auto::Staging::Disk => 0,
            auto::Staging::Ram => 1,
            auto::Staging::OnDemand => 2,
        })
        .build();
    {
        let ui = ui.clone();
        staging_row.connect_selected_notify(move |r| {
            {
                let mut s = ui.settings.borrow_mut();
                s.auto_staging = match r.selected() {
                    1 => auto::Staging::Ram,
                    2 => auto::Staging::OnDemand,
                    _ => auto::Staging::Disk,
                }
                .id()
                .to_string();
                let _ = s.save();
            }
            refresh_auto_indicator(&ui);
        });
    }

    let ram_row = adw::SpinRow::builder()
        .title("Memory it may use for waiting files")
        .subtitle(match auto::available_ram_mb() {
            Some(free) => format!("This machine has about {free} MB free right now"),
            None => "In megabytes".to_string(),
        })
        .adjustment(&gtk::Adjustment::new(
            current.ram_budget_mb as f64,
            64.0,
            8192.0,
            64.0,
            256.0,
            0.0,
        ))
        .build();
    {
        let ui = ui.clone();
        ram_row.connect_value_notify(move |r| {
            let mut s = ui.settings.borrow_mut();
            s.ram_budget_mb = r.value() as u32;
            let _ = s.save();
        });
    }

    let cap_row = adw::SpinRow::builder()
        .title("Most it will keep waiting")
        .subtitle("In megabytes. Past this it stops converting rather than filling the disk.")
        .adjustment(&gtk::Adjustment::new(
            current.auto_cap_mb as f64,
            64.0,
            50_000.0,
            64.0,
            512.0,
            0.0,
        ))
        .build();
    {
        let ui = ui.clone();
        cap_row.connect_value_notify(move |r| {
            let mut s = ui.settings.borrow_mut();
            s.auto_cap_mb = r.value() as u32;
            let _ = s.save();
        });
    }

    let keep_row = adw::SpinRow::builder()
        .title("How long a file waits")
        .subtitle("In days. One nobody collected in that time is dropped.")
        .adjustment(&gtk::Adjustment::new(
            current.auto_keep_days as f64,
            1.0,
            365.0,
            1.0,
            7.0,
            0.0,
        ))
        .build();
    {
        let ui = ui.clone();
        keep_row.connect_value_notify(move |r| {
            let mut s = ui.settings.borrow_mut();
            s.auto_keep_days = r.value() as u32;
            let _ = s.save();
        });
    }

    // What is actually sitting there, because a queue nobody can see is how
    // somebody loses forty gigabytes without knowing where it went.
    let held = auto::staging_dir(auto::Staging::from_id(&current.auto_staging))
        .map(|d| auto::waiting(&d))
        .unwrap_or(auto::Waiting {
            files: Vec::new(),
            bytes: 0,
        });
    let waiting_row = adw::ActionRow::builder()
        .title("Waiting to be copied")
        .subtitle(match held.files.len() {
            0 => "Nothing".to_string(),
            n => format!("{n} files, {}", render::human_bytes(held.bytes)),
        })
        .build();

    vec![
        staging_row.upcast(),
        ram_row.upcast(),
        cap_row.upcast(),
        keep_row.upcast(),
        waiting_row.upcast(),
    ]
}

/// WatchDog's own page: what it is doing, and the three things it needs.
///
/// Laid out in the order the questions are asked. What is happening, at the
/// top and given room, because that is what the page is opened to find out.
/// Then the three settings that make it work, as one group of rows, because
/// they are one decision made three times: watch here, convert to that, put it
/// there. Then what it has done, because a mode that runs while nobody is
/// looking has to be able to show its work.
#[allow(clippy::too_many_arguments)]
fn build_watchdog_page(
    row: &adw::ActionRow,
    chain: &Fold,
    folder: &adw::ActionRow,
    format: &adw::ActionRow,
    into: &adw::ActionRow,
    more: &adw::ExpanderRow,
    recent: &adw::PreferencesGroup,
) -> gtk::Widget {
    let page = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_5);

    // The switch, on its own, above everything it governs.
    let arm = gtk::ListBox::new();
    arm.add_css_class("boxed-list");
    arm.set_selection_mode(gtk::SelectionMode::None);
    arm.append(row);
    page.append(&arm);

    // What the thing actually is, in the words someone would use to a person
    // who had not met it. The line at the top of the page names it; this says
    // what it would do for you, which is the part that decides whether the
    // switch above is worth touching. Plain enough to be read once and not
    // needed again - which is the standard the settings wording is held to,
    // and there is no reason this should be held to a lower one.
    // Built by joining separate lines rather than as one wrapped literal. The
    // literal was written with backslash continuations, the formatter joined
    // them keeping the indentation, and the paragraph reached the screen with
    // fourteen spaces between every sentence.
    let what = [
        "Leave this on and you can forget about it.",
        "WatchDog watches one folder.",
        "When your slicer saves a new file there, it converts the file to your printer's format",
        "and puts the result where you say - a USB stick, or any folder on this computer.",
        "Nothing to open, nothing to press.",
    ]
    .join(" ");
    let what = gtk::Label::builder().label(&what).xalign(0.0).build();
    what.set_wrap(true);
    what.set_wrap_mode(gtk::pango::WrapMode::Word);
    what.set_justify(gtk::Justification::Left);
    what.set_max_width_chars(66);
    what.add_css_class("caption");
    what.add_css_class("cz-dim");
    what.set_margin_start(theme::SPACE_2);
    what.set_margin_end(theme::SPACE_2);
    page.append(&what);

    // The chain, given the middle of the page rather than the corner of
    // another one. It is the answer to "what is it doing", which is the
    // question this page exists to answer.
    page.append(&chain.clip);

    let setup = adw::PreferencesGroup::builder()
        .title("What it does")
        .description("Where to look, what to make, and where to put it")
        .build();
    let rows = gtk::ListBox::new();
    rows.add_css_class("boxed-list");
    rows.set_selection_mode(gtk::SelectionMode::None);
    rows.append(folder);
    rows.append(format);
    rows.append(into);
    rows.append(more);
    setup.add(&rows);
    page.append(&setup);

    page.append(recent);

    page_frame(
        "WatchDog",
        "Watch a folder, convert what your slicer leaves there, and save it where you want it.",
        &page,
    )
}

/// A panel that folds open and shut by having its height driven.
///
/// Anything appearing or disappearing inside a column takes everything below
/// it up or down the page, and doing that in one frame reads as the layout
/// breaking rather than as it changing. A GtkRevealer is the obvious tool and
/// did not survive being asked to animate in the middle of a resize, so the
/// height is driven here instead: a scrolled window to allow a height below
/// the child's own minimum, and a tick callback to walk it there.
struct Fold {
    clip: gtk::ScrolledWindow,
    inner: gtk::Widget,
    /// Where the fold currently is. A widget's own height reads stale part-way
    /// through a resize, and this has to pick up wherever the last one left
    /// off. Negative means it has never been folded and is at its natural
    /// height.
    at: Rc<Cell<i32>>,
    /// Which fold is in charge. A panel toggled twice in quick succession
    /// starts a second animation before the first has finished, and the newer
    /// one has to win rather than fight.
    generation: Rc<Cell<u32>>,
    /// How tall this panel was the last time it could be measured properly.
    full: Rc<Cell<i32>>,
    /// Where a walk still in flight is heading. A re-measure that agrees with
    /// it is already being served and must not restart it from wherever it
    /// has got to, which would stall the panel short of its target.
    heading: Rc<Cell<i32>>,
}

impl Fold {
    /// Wrap a widget so its height can be driven. Nothing ever scrolls in it.
    fn new(inner: &impl IsA<gtk::Widget>) -> Self {
        let clip = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::External)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(false)
            .child(inner)
            .build();
        clip.set_overflow(gtk::Overflow::Hidden);
        // A fold cuts a panel off at a chosen height; it is not a thing to be
        // scrolled. Without this, clicking a button inside one - a milestone
        // in WatchDog's chain, say - focused it, and the viewport slid the
        // panel up to bring it into view. Nothing had moved, so the movement
        // read as the page losing its place.
        shell::dont_chase_focus(&clip);
        clip.vadjustment().connect_value_changed(|adj| {
            if adj.value() != 0.0 {
                adj.set_value(0.0);
            }
        });
        Self {
            clip,
            inner: inner.clone().upcast(),
            at: Rc::new(Cell::new(-1)),
            generation: Rc::new(Cell::new(0)),
            full: Rc::new(Cell::new(-1)),
            heading: Rc::new(Cell::new(-1)),
        }
    }

    /// How tall the panel wants to be, measured against the width it actually
    /// has.
    ///
    /// Asking at width -1 asks how tall it would be if nothing were wrapping,
    /// which is a different and smaller number for anything that wraps - and
    /// the line under WatchDog's chain is a whole sentence that does.
    fn wanted(&self) -> i32 {
        let width = self.clip.width();
        let for_width = if width > 0 { width } else { -1 };
        self.inner.measure(gtk::Orientation::Vertical, for_width).1
    }

    fn set(&self, open: bool, animate: bool) {
        // Shown before it is measured, not after. A widget inside a hidden
        // container measures as nothing, so opening a panel that had been
        // folded shut asked how tall it wanted to be while it was still
        // hidden, got zero, and opened to a single pixel. It then measured
        // correctly on the next try, which is why it took a shrink and a
        // widen to come back.
        //
        // A folded-shut panel is hidden outright at the end regardless, so it
        // does not leave its parent's spacing behind as a gap with nothing in
        // it.
        if open {
            self.clip.set_visible(true);
        }
        let measured = self.wanted();
        // And a remembered height as a second line of defence, for the moments
        // when a measurement is taken before layout has caught up. A panel
        // that has ever been open knows how tall it was.
        let full = match (measured, self.full.get()) {
            (m, _) if m > 1 => {
                self.full.set(m);
                m
            }
            (_, remembered) if remembered > 1 => remembered,
            (m, _) => m.max(1),
        };
        let target = if open { full } else { 0 };
        let from = match self.at.get() {
            n if n < 0 => full,
            n => n,
        };
        if from == target || !animate {
            self.land(target);
            return;
        }
        self.walk(from, target);
    }

    /// Take the panel's height again, for content that grew or shrank while it
    /// was already open.
    ///
    /// A fold pins a height. Anything the panel gains afterwards is simply cut
    /// off - which is what happened to the line under WatchDog's chain: the
    /// height was taken while that line was empty and hidden, so when it later
    /// had something to say there was no room left to say it in.
    ///
    /// The new height is walked to rather than snapped to. A line appearing
    /// under the chain takes the whole page below it down by its own height,
    /// and doing that in one frame is the jolt: the eye reads the page as
    /// having broken and then re-drawn, rather than as one line having been
    /// added. Over a fifth of a second it reads as the line making room for
    /// itself.
    fn refit(&self) {
        if self.at.get() <= 0 || !self.clip.is_visible() {
            return;
        }
        let measured = self.wanted();
        if measured <= 1 || measured == self.at.get() || measured == self.heading.get() {
            return;
        }
        self.full.set(measured);
        self.walk(self.at.get(), measured);
    }

    /// Drive the height from one value to the other over `FOLD_SECONDS`.
    fn walk(&self, from: i32, target: i32) {
        let mine = self.generation.get().wrapping_add(1);
        self.generation.set(mine);
        self.heading.set(target);
        let started = std::time::Instant::now();
        let at = self.at.clone();
        let generation = self.generation.clone();
        let heading = self.heading.clone();
        self.clip.add_tick_callback(move |w, _| {
            if generation.get() != mine {
                return glib::ControlFlow::Break;
            }
            let t = (started.elapsed().as_secs_f64() / FOLD_SECONDS).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - t) * (1.0 - t);
            let now = from + ((target - from) as f64 * eased).round() as i32;
            at.set(now);
            w.set_size_request(-1, now);
            if t >= 1.0 {
                heading.set(-1);
                w.set_visible(target > 0);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    fn land(&self, target: i32) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.heading.set(-1);
        self.at.set(target);
        self.clip.set_size_request(-1, target);
        self.clip.set_visible(target > 0);
    }
}

fn build_dropzone() -> (gtk::Box, gtk::Label, gtk::Label, Fold) {
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

    // The same list again, on the drop zone itself. Not instead of the panel:
    // "will this thing read my file" is the question someone has before they
    // touch anything, and an answer you have to hover to find is no answer at
    // all for a first-time reader - nor for anyone on a touchscreen, where
    // there is no hover to give. It is here so that the narrow window, which
    // hides the panel for want of room, still has somewhere to say it.
    let listed: Vec<String> = registry::readable()
        .iter()
        .map(|info| format!("{}  .{}", info.name, info.extension))
        .collect();
    zone.set_tooltip_text(Some(&format!("Opens\n{}", listed.join("\n"))));

    let formats = Fold::new(&formats);

    zone.append(&icon);
    zone.append(&title);
    zone.append(&sub);
    zone.append(&browse);
    zone.append(&formats.clip);
    (zone, title, sub, formats)
}

/// How long a panel takes to fold open or shut. Long enough to read as
/// movement, short enough not to lag a window being dragged.
const FOLD_SECONDS: f64 = 0.22;

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
    nearby: &gtk::Box,
    queue_panel: &gtk::Box,
    input_field: &gtk::MenuButton,
    output_picker: &Rc<format_picker::FormatPicker>,
    swap_btn: &gtk::Button,
    output_info: &gtk::MenuButton,
    dest_button: &gtk::MenuButton,
    eject_btn: &gtk::Button,
    name_entry: &gtk::Entry,
    convert_btn: &gtk::Button,
    progress: &gtk::ProgressBar,
    penguin: &Rc<penguin::Penguin>,
    problem: &gtk::Box,
) -> (
    gtk::Widget,
    gtk::Box,
    gtk::Box,
    gtk::Box,
    gtk::Box,
    gtk::Revealer,
    gtk::Stack,
) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_4);

    // The drop zone and the queue are two faces of the same place, so they are
    // two pages of a stack rather than two widgets taking turns at being
    // visible. A stack crossfades between them and, with interpolate-size,
    // carries its own height across at the same time - so a 240px invitation
    // becomes a one-row queue by shrinking into it, rather than by vanishing
    // and letting everything below jump up to fill the hole.
    let faces = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(MORPH_MS)
        .interpolate_size(true)
        .vhomogeneous(false)
        .build();
    faces.add_named(dropzone, Some("drop"));
    faces.add_named(queue_panel, Some("queue"));
    content.append(&faces);
    content.append(nearby);
    // Under Quick Access, which is the other thing on this page that watches
    // folders - and above the form, which is about the file in hand.

    // Controls stay hidden until there is a file, so a new user sees one
    // instruction rather than a form (§2, §36).
    let controls = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_4);
    // Revealed rather than shown, so the form fades in behind the queue
    // instead of the whole page changing in one frame.
    // Swings down rather than fading in. A fade gives the eye nothing to
    // follow - the form is simply there a moment later - where a downward
    // unfold arrives from the queue above it and says where it came from.
    let controls_reveal = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SwingDown)
        .transition_duration(CONTROLS_MS)
        .reveal_child(false)
        .child(&controls)
        .build();

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
    let dest_line = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
    dest_button.set_hexpand(true);
    dest_line.append(dest_button);
    dest_line.append(eject_btn);
    dest_col.append(&dest_line);
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

    content.append(&controls_reveal);
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
        controls_reveal,
        faces,
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
        browse.connect_clicked(move |_| {
            let needs_folder = {
                let s = ui.settings.borrow();
                s.auto_convert && s.auto_watch_dir.is_none()
            };
            if needs_folder {
                choose_watch_folder(&ui);
            } else {
                choose_files(&ui);
            }
        });
    }
    {
        let ui = ui.clone();
        add_more.connect_clicked(move |_| choose_files(&ui));
    }
    if let Some(clear) = find_named(&ui.page_faces, "queue-clear") {
        let ui = ui.clone();
        clear.connect_clicked(move |_| clear_files(&ui));
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

    // "Found nearby": manual refresh, plus the events that can change the
    // answer. Drives are watched through GIO rather than polled, so nothing
    // runs while the window sits idle.
    {
        let ui2 = ui.clone();
        ui.nearby_refresh.connect_clicked(move |_| {
            // Noted before the scan starts: a press is owed a visible answer
            // even when there was nothing to find.
            ui2.scan_asked.set(true);
            refresh_nearby(&ui2);
        });
    }
    {
        let monitor = gio::VolumeMonitor::get();
        {
            let ui = ui.clone();
            monitor.connect_mount_added(move |_, mount| {
                refresh_nearby(&ui);
                drive_arrived(&ui, mount);
                reresolve_auto_drive(&ui);
                reattach_out_drive(&ui);
                refresh_dest_label(&ui);
                update_eject_button(&ui);
                refresh_watchdog_steps(&ui);
                auto_deliver(&ui);
            });
        }
        {
            let ui = ui.clone();
            monitor.connect_mount_removed(move |_, _| {
                refresh_nearby(&ui);
                reresolve_auto_drive(&ui);
                refresh_dest_label(&ui);
                update_eject_button(&ui);
                // The chain claims a drive is there. Unplugging one is exactly
                // the moment that claim stops being true, so it is exactly the
                // moment to stop making it.
                refresh_watchdog_steps(&ui);
            });
        }
        // The monitor is a process-wide singleton; holding it for the life of
        // the window keeps the handlers alive without a static.
        ui.volume_monitor.replace(Some(monitor));
    }
    {
        // Coming back to the window is the moment a file sliced elsewhere
        // becomes interesting.
        let ui = ui.clone();
        ui.window.clone().connect_is_active_notify(move |w| {
            if w.is_active() {
                refresh_nearby(&ui);
                recheck_edits(&ui);
            }
        });
    }
    {
        let ui = ui.clone();
        ui.eject_btn.clone().connect_clicked(move |b| {
            let Some(drive) = ui.out_dir.borrow().as_deref().and_then(drives::containing) else {
                return;
            };
            b.set_sensitive(false);
            let ui2 = ui.clone();
            let btn = b.clone();
            let name = drive.name.clone();
            let sort = ui.settings.borrow().sort_drive_on_eject;
            drives::eject(&drive, sort, move |res| {
                btn.set_sensitive(true);
                match res {
                    Ok(()) => {
                        ui2.toasts
                            .add_toast(adw::Toast::new(&format!("{name} is safe to remove")));
                        // The destination just stopped existing; say so rather
                        // than leaving it pointing at a drive that has gone.
                        update_eject_button(&ui2);
                        revalidate(&ui2);
                        refresh_nearby(&ui2);
                    }
                    Err(e) => ui2
                        .toasts
                        .add_toast(adw::Toast::new(&format!("Could not eject {name}: {e}"))),
                }
            });
        });
    }

    // Choosing where the suggestions come from, at the point they are shown
    // rather than buried in Settings.
    {
        let ui2 = ui.clone();
        ui.nearby_search.connect_changed(move |e| {
            // The only icon the field carries, and only when it would do
            // something: an always-present one is decoration.
            e.set_secondary_icon_name(if e.text().is_empty() {
                None
            } else {
                Some("edit-clear-symbolic")
            });
            apply_nearby_filter(&ui2);
            animate_nearby_height(&ui2, ui2.nearby_clip.height());
        });
    }

    {
        let ui2 = ui.clone();
        ui.nearby_search
            .connect_icon_release(move |_, _| ui2.nearby_search.set_text(""));
    }

    {
        let ui2 = ui.clone();
        ui.watchdog_eye.connect_toggled(move |b| {
            let on = b.is_active();
            {
                let mut s = ui2.settings.borrow_mut();
                if s.auto_convert == on {
                    return;
                }
                s.auto_convert = on;
                let _ = s.save();
            }
            rearm_auto(&ui2);
            // To WatchDog's own page, which is where the answer to "what did
            // that just do" is written. The eye is visible from every page, so
            // pressing it has to land somewhere that shows what changed - and
            // that is not the page you happened to be on.
            ui2.shell.show(Section::WatchDog);
            watchdog_needs_setup(&ui2, on);
        });
    }
    {
        // The switch on the page and the eye in the title bar are the same
        // switch; either one sets the setting and the other follows it.
        let ui2 = ui.clone();
        ui.watchdog_switch.connect_active_notify(move |sw| {
            let on = sw.is_active();
            {
                let mut s = ui2.settings.borrow_mut();
                if s.auto_convert == on {
                    return;
                }
                s.auto_convert = on;
                let _ = s.save();
            }
            rearm_auto(&ui2);
            watchdog_needs_setup(&ui2, on);
        });
    }

    for row in watchdog_detail_rows(ui) {
        ui.watchdog_more.add_row(&row);
    }
    {
        // Only what WatchDog did. Somebody tidying away a run of automatic
        // conversions is not asking to lose the record of everything they
        // converted by hand, and the two are in the same file.
        let ui2 = ui.clone();
        ui.watchdog_clear.connect_clicked(move |_| {
            let gone = {
                let mut hist = ui2.history.borrow_mut();
                let before = hist.entries.len();
                hist.entries.retain(|e| !e.automatic);
                let _ = hist.save();
                before - hist.entries.len()
            };
            refresh_watchdog_recent(&ui2);
            refresh_history(&ui2);
            ui2.toasts.add_toast(adw::Toast::new(&match gone {
                0 => "There was nothing to clear".to_string(),
                1 => "Forgot 1 automatic conversion".to_string(),
                n => format!("Forgot {n} automatic conversions"),
            }));
        });
    }
    {
        // Finish writing before the stick is pulled. WatchDog copies without
        // being asked, so "is it safe to unplug" is a question this page owes
        // an answer to.
        let ui2 = ui.clone();
        ui.watchdog_eject.connect_clicked(move |b| {
            let Some(drive) = ui2
                .settings
                .borrow()
                .auto_target_uuid
                .as_deref()
                .and_then(drives::by_uuid)
            else {
                return;
            };
            b.set_sensitive(false);
            let name = drive.name.clone();
            let sort = ui2.settings.borrow().sort_drive_on_eject;
            let ui3 = ui2.clone();
            drives::eject(&drive, sort, move |res| {
                ui3.watchdog_eject.set_sensitive(true);
                ui3.toasts.add_toast(adw::Toast::new(&match &res {
                    Ok(()) => format!("{name} is safe to unplug"),
                    Err(why) => format!("Could not eject {name}: {why}"),
                }));
                refresh_auto_indicator(&ui3);
                refresh_watchdog_steps(&ui3);
            });
        });
    }
    // The same width, whichever is holding the longer name. Two controls that
    // do the same kind of thing, one above the other, should not read as two
    // different controls because their labels differ in length.
    ui.watchdog_widths.add_widget(&ui.watchdog_folder_btn);
    ui.watchdog_widths.add_widget(&ui.watchdog_drive_btn);
    {
        let ui2 = ui.clone();
        ui.watchdog_folder_btn
            .connect_clicked(move |_| choose_watch_folder(&ui2));
    }
    ui.watchdog_drive_btn
        .set_popover(Some(&watchdog_drive_menu(ui)));
    ui.watchdog_format_btn
        .set_popover(Some(&watchdog_format_menu(ui)));
    ui.watchdog_widths.add_widget(&ui.watchdog_format_btn);
    refresh_watchdog_recent(ui);
    wire_watchdog_steps(ui);

    build_sources_menu(ui, &ui.nearby_sources.clone());

    // Opening the list has to be caught as well as refreshing it. The expander
    // animates itself open, and with nothing to stop it a folder holding forty
    // files would push the rest of the page down by the length of all forty.
    // Only when there is a cap to impose: a list that fits is left entirely to
    // the expander's own animation, which is smoother than anything imposed
    // over the top of it.
    {
        let ui2 = ui.clone();
        ui.nearby_expander.connect_expanded_notify(move |e| {
            // Glass while shut, box while open.
            show_nearby_search(&ui2, ui2.nearby_search_shown.get());

            // Shut, the header is a card in its own right and keeps its
            // corners. Open, it is the top of one and gives up the bottom two,
            // so the seam with the file list underneath does not show.
            //
            // One thing after another, in both directions. Opening: the
            // corners square off, and only then does the list drop out from
            // under them. Shutting is the same sequence backwards - the list
            // folds away first and the corners round afterwards, which
            // `animate_nearby_height` does at the end of its last frame.
            // Done together, the header appears to change shape while the
            // thing it is changing shape for is still arriving; and rounding
            // them while rows were still on screen put a curve through the
            // middle of the list.
            if !e.is_expanded() {
                animate_nearby_height(&ui2, ui2.nearby_clip.height());
                return;
            }
            ui2.nearby_head_list.add_css_class("cz-qa-open");
            if !ui2.settings.borrow().animations {
                animate_nearby_height(&ui2, ui2.nearby_clip.height());
                return;
            }
            let ui3 = ui2.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(CORNER_MS), move || {
                animate_nearby_height(&ui3, ui3.nearby_clip.height());
            });
        });
    }

    refresh_nearby(ui);

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
        // A real exchange, both ways round. It used to set the output to the
        // input's format and stop there, which left the two boxes reading the
        // same thing and the input still saying it was detecting - a swap that
        // had visibly only done half of itself.
        //
        // Swapping pins the input, necessarily: detection would read the file
        // and put the old answer straight back, so the new one has to be told.
        // That is also the honest cost of the button - the input format is a
        // fact about the file on disk, not a preference, so asking to read an
        // SL1 as a GOO can fail. It says so when it does, and the input menu's
        // "Detect Automatically" puts it back.
        swap.connect_clicked(move |_| {
            let Some(input) = input_format(&ui) else {
                ui.toasts.add_toast(adw::Toast::new(
                    "Nothing to swap yet - the file is still being read",
                ));
                return;
            };
            let Some(output) = ui.output_picker.selected() else {
                return;
            };
            if input == output {
                ui.toasts
                    .add_toast(adw::Toast::new("Both sides are already the same format"));
                return;
            }
            if registry::by_id(&input).map(|h| h.info().capabilities.writes) != Some(true) {
                ui.toasts.add_toast(adw::Toast::new(
                    "That format cannot be written yet, so there is nothing to swap to",
                ));
                return;
            }
            if registry::by_id(output).map(|h| h.info().capabilities.reads) != Some(true) {
                ui.toasts.add_toast(adw::Toast::new(
                    "That format cannot be read yet, so there is nothing to swap from",
                ));
                return;
            }
            let reading = registry::by_id(output)
                .map(|h| h.info().name)
                .unwrap_or(output);
            ui.output_picker.set_selected(&input);
            force_input_format(&ui, Some(output.to_string()));
            suggest_name(&ui);
            revalidate(&ui);
            // Said once, at the moment of the swap, because the failure that
            // follows a swap onto the wrong format is a correct answer that
            // reads exactly like a broken button.
            ui.toasts.add_toast(adw::Toast::new(&format!(
                "Now reading as {reading}. If it will not open, set Input back to \
                 Detect Automatically."
            )));
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
    refresh_dropzone_text(ui);
}

/// What the empty page invites you to do.
///
/// Normally: drop a file. But WatchDog switched on with nowhere to watch is a
/// half-finished thing, and the largest, emptiest target on the page is a
/// better place to say so than a line of text further down. It goes back to
/// inviting files the moment a folder is chosen, because converting one file
/// by hand is still worth being able to do while WatchDog is running.
fn refresh_dropzone_text(ui: &Rc<App>) {
    // Convert is a manual page again. It used to change its heading to narrate
    // whatever WatchDog was up to, which made one page try to be two things:
    // a place to drop files, and a status display for a mode that runs on its
    // own. WatchDog has its own page for that now, and the eye in the title
    // bar says from anywhere whether it is armed.
    ui.dropzone_title.set_text("Drop files here");
    ui.dropzone_sub.set_text("or browse your computer");
    if let Some(browse) = find_named(&ui.dropzone, "dropzone-browse") {
        if let Some(label) = browse.child().and_downcast::<gtk::Label>() {
            label.set_text("Browse Files");
        }
    }
    refresh_watchdog_steps(ui);
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
    // What the dialog is offering, said from the reader's side: these are the
    // files this program can open. "Sliced resin files" named the category
    // instead, which is true of plenty of files it cannot open.
    //
    // Every readable format, including any switched off in Settings. That
    // setting shortens the menus; it does not make a file unopenable, and the
    // dialog is exactly where someone would go to open one by hand.
    filter.set_name(Some("Compatible files"));
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

/// React to a drive being plugged in.
///
/// Only removable drives are followed. Auto-locking onto a newly mounted
/// internal filesystem - a backup disk waking up, a network share
/// reconnecting - would move the output somewhere the user never asked for.
fn drive_arrived(ui: &Rc<App>, mount: &gio::Mount) {
    let removable = mount.can_eject()
        || mount.can_unmount()
        || mount.drive().map(|d| d.is_removable()).unwrap_or(false);
    if !removable {
        return;
    }
    let name = mount.name().to_string();
    // Remembered whatever the setting says: "connected drive" should mean the
    // one just plugged in even when the output is not following drives.
    *ui.last_drive.borrow_mut() = Some(name.clone());
    if !ui.settings.borrow().auto_lock_new_drives {
        return;
    }
    let sub = ui.settings.borrow().pinned_subfolder.clone();
    let Some(target) = drives::target_dir(&name, &sub).or_else(|| mount.root().path()) else {
        return;
    };
    set_out_dir(ui, Some(target));
    ui.toasts
        .add_toast(adw::Toast::new(&format!("Saving to {name}")));
}

/// The "look in" menu: every folder and drive Quick Access could scan, each
/// with a switch.
///
/// Rebuilt every time it opens, because drives appear and disappear while the
/// window is up and a stale list would offer somewhere that is no longer
/// there.
fn build_sources_menu(ui: &Rc<App>, button: &gtk::MenuButton) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_size_request(320, -1);
    content.set_margin_top(theme::SPACE_2);
    content.set_margin_bottom(theme::SPACE_2);
    let popover = gtk::Popover::builder().child(&content).build();
    button.set_popover(Some(&popover));

    let ui = ui.clone();
    popover.connect_show(move |_| fill_sources_menu(&ui, &content));
}

/// Build the body of the "Look in" menu.
///
/// Separate from the popover so it can be called again in place. Restoring
/// removed places adds rows, which redrawing the list cannot do on its own,
/// and shutting the menu to rebuild it would lose the user's place.
fn fill_sources_menu(ui: &Rc<App>, content: &gtk::Box) {
    {
        while let Some(child) = content.first_child() {
            content.remove(&child);
        }

        let heading = shell::section_label("Look in");
        heading.set_margin_start(theme::SPACE_3);
        heading.set_margin_bottom(theme::SPACE_2);
        content.append(&heading);

        let (open_dir, extra, off, hidden, drives_on) = {
            let s = ui.settings.borrow();
            (
                s.open_start_dir(),
                s.quick_access_folders.clone(),
                s.quick_access_off.clone(),
                s.quick_access_hidden.clone(),
                s.quick_access_drives_on.clone(),
            )
        };
        let sources = nearby::sources(open_dir.as_deref(), &extra, &off, &hidden, &drives_on);

        if sources.is_empty() {
            let none = gtk::Label::new(Some("Nowhere to look yet"));
            none.add_css_class("dim-label");
            none.set_margin_start(theme::SPACE_3);
            none.set_xalign(0.0);
            content.append(&none);
        }

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");
        list.set_margin_start(theme::SPACE_2);
        list.set_margin_end(theme::SPACE_2);
        for source in sources {
            let row = adw::SwitchRow::builder()
                .title(&source.label)
                .subtitle(source.path.display().to_string())
                .active(source.enabled)
                .build();
            if !source.enabled && source.opt_in {
                row.set_subtitle(&format!(
                    "{} - not read until switched on",
                    source.path.display()
                ));
            }
            {
                let ui = ui.clone();
                let key = source.key.clone();
                let opt_in = source.opt_in;
                row.connect_active_notify(move |r| {
                    {
                        let mut s = ui.settings.borrow_mut();
                        if opt_in {
                            // A drive is remembered when it is wanted. Nothing
                            // is recorded about the ones that are not, so a
                            // machine that sees a lot of drives does not
                            // accumulate a list of every stick ever attached.
                            s.quick_access_drives_on.retain(|k| *k != key);
                            if r.is_active() {
                                s.quick_access_drives_on.push(key.clone());
                            }
                        } else {
                            s.quick_access_off.retain(|k| *k != key);
                            if !r.is_active() {
                                s.quick_access_off.push(key.clone());
                            }
                        }
                        let _ = s.save();
                    }
                    refresh_nearby(&ui);
                });
            }
            // Switching a source off is not the same as being done with it:
            // off still leaves it sitting in the list. Drives can be taken off
            // as well as folders - one being plugged in is not the same as it
            // being wanted, and a drive that lives in the machine should not
            // have to keep offering itself.
            //
            // Removal is on the secondary click rather than a button in the
            // row. A button had to sit somewhere, and the only place for it
            // pushed the switch off the edge it lines up on - the row read as
            // cluttered for the sake of something used once. The tooltip is
            // what makes it findable, since a right-click nobody knows about
            // is the same as no right-click.
            if source.removable_entry {
                row.set_tooltip_text(Some("Right-click to remove from the list"));
                let menu = gtk::GestureClick::new();
                menu.set_button(gdk::BUTTON_SECONDARY);
                // Capture, so the switch does not swallow the press first.
                menu.set_propagation_phase(gtk::PropagationPhase::Capture);
                let ui2 = ui.clone();
                let path = source.path.clone();
                let key = source.key.clone();
                let label = source.label.clone();
                // Weak, because the row owns the gesture which owns this
                // closure: a strong handle back to the row is a cycle.
                let gone = row.downgrade();
                menu.connect_pressed(move |_, _, _, _| {
                    {
                        let mut s = ui2.settings.borrow_mut();
                        // Dropped as an added folder and recorded as hidden.
                        // The first covers a folder the user picked; the
                        // second covers the one they last opened a file from,
                        // which is offered automatically and would otherwise
                        // reappear on the next refresh. A folder can be both.
                        s.quick_access_folders.retain(|p| *p != path);
                        s.quick_access_off.retain(|k| *k != key);
                        s.quick_access_drives_on.retain(|k| *k != key);
                        if !s.quick_access_hidden.contains(&key) {
                            s.quick_access_hidden.push(key.clone());
                        }
                        let _ = s.save();
                    }
                    refresh_nearby(&ui2);
                    // Only the row goes, rather than the menu closing or
                    // rebuilding itself: several can be cleared out in one
                    // visit, and nothing flickers under the pointer.
                    if let Some(row) = gone.upgrade() {
                        if let Some(list) = row.parent().and_downcast::<gtk::ListBox>() {
                            list.remove(&row);
                        }
                    }
                    // Said out loud, because a right-click that silently makes
                    // a row disappear is indistinguishable from a misclick -
                    // and the way back is not obvious enough to leave unsaid.
                    ui2.toasts.add_toast(adw::Toast::new(&format!(
                        "{label} removed. Show removed places puts it back."
                    )));
                });
                row.add_controller(menu);
            }
            list.append(&row);
        }
        content.append(&list);

        let add = gtk::Button::builder().build();
        add.set_child(Some(&labelled_icon("list-add-symbolic", "Add Folder…")));
        add.add_css_class("flat");
        add.set_margin_top(theme::SPACE_2);
        {
            let ui = ui.clone();
            add.connect_clicked(move |_| {
                if let Some(pop) = ui.nearby_sources.popover() {
                    pop.popdown();
                }
                choose_scan_folder(&ui);
            });
        }
        content.append(&add);
    }
}

/// Add a folder to the places Quick Access looks.
///
/// Folders chosen here sit alongside the folder the file chooser starts from
/// and every mounted drive; the switches in the menu decide which of them are
/// actually scanned.
fn choose_scan_folder(ui: &Rc<App>) {
    let dialog = gtk::FileDialog::builder()
        .title("Look for files in")
        .modal(true)
        .build();
    if let Some(dir) = ui.settings.borrow().open_start_dir() {
        dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
    }
    let ui = ui.clone();
    dialog.select_folder(
        Some(&ui.window.clone()),
        gio::Cancellable::NONE,
        move |res| {
            if let Ok(folder) = res {
                if let Some(path) = folder.path() {
                    {
                        let mut s = ui.settings.borrow_mut();
                        // Added alongside the others rather than replacing
                        // them: the point of the picker is holding more than
                        // one place at a time.
                        if !s.quick_access_folders.contains(&path) {
                            s.quick_access_folders.push(path.clone());
                        }
                        // Adding a folder that was previously switched off,
                        // or taken off the list entirely, is a request to look
                        // in it again.
                        let key = path.to_string_lossy().into_owned();
                        s.quick_access_off.retain(|k| *k != key);
                        s.quick_access_hidden.retain(|k| *k != key);
                        let _ = s.save();
                    }
                    refresh_nearby(&ui);
                    // Opened, because choosing a folder is asking to see what
                    // is in it.
                    ui.nearby_expander.set_expanded(true);
                }
            }
        },
    );
}

/// Make the milestones pressable.
///
/// The chain already says which link is broken. Being able to press that link
/// to mend it is the difference between a diagnosis and a repair, and it saves
/// hunting for the same two controls in the panel below. Each stop offers what
/// WatchDog's own row offers for it, so there is one answer to each question
/// and two places to ask it.
fn wire_watchdog_steps(ui: &Rc<App>) {
    let chain = ui.watchdog_steps.clone();

    {
        let ui2 = ui.clone();
        chain.on_click(0, "Choose a folder to watch", move || {
            choose_watch_folder(&ui2)
        });
    }

    // Three jobs, in the order the stop is likely to be in when pressed: put
    // the failure away, put the waiting file away, or hand it one by hand.
    {
        let ui2 = ui.clone();
        chain.on_click(1, "Choose a file to convert", move || {
            if ui2.watchdog_trouble.borrow_mut().take().is_some() {
                refresh_watchdog_steps(&ui2);
                return;
            }
            if watchdog_skip(&ui2) {
                return;
            }
            watchdog_pick_file(&ui2);
        });
    }

    // What it converts to. The stop is called Convert, so the question it
    // raises is "into what" - and answering that by walking off to another
    // page was a page change nobody asked for.
    {
        let menu = watchdog_format_menu(ui);
        if let Some(anchor) = chain.anchor(2) {
            menu.set_parent(&anchor);
            menu.set_position(gtk::PositionType::Bottom);
            chain.on_click(2, "Choose what to convert to", move || menu.popup());
        }
    }

    // Parented to the stop rather than shown as a dialog, so it opens where it
    // was pressed. Built once and kept alive by the handler that shows it.
    {
        let menu = watchdog_drive_menu(ui);
        if let Some(anchor) = chain.anchor(3) {
            menu.set_parent(&anchor);
            menu.set_position(gtk::PositionType::Bottom);
            chain.on_click(3, "Choose a drive to copy to", move || menu.popup());
        }
    }
}

/// Put the milestone chain in step with what is actually true.
///
/// Four stops, each answering one question a person would ask in order: is it
/// watching somewhere, has anything turned up, is it converting, and did it
/// land. Read left to right the chain is one pass of the whole job, and it
/// resets when the next file arrives.
///
/// Grey is not done, white is done, breathing is live, and a red cross is the
/// broken link. Green appears once - the last stop, once a file has actually
/// reached the drive. Being told which drive to use is not the same as having
/// got something onto it, so it does not get the colour that says it did.
fn refresh_watchdog_steps(ui: &Rc<App>) {
    use steps::State;

    let (armed, dir) = {
        let s = ui.settings.borrow();
        (s.auto_convert, s.auto_watch_dir.clone())
    };
    let (target, here) = watchdog_where(ui);
    let smooth = ui.settings.borrow().animations;
    ui.watchdog_fold.set(armed, smooth);
    if !armed {
        ui.watchdog_steps.rest();
        return;
    }
    let chain = &ui.watchdog_steps;

    // 1: the folder. Set but gone is a different thing from never set, and
    // only the second is something being wrong.
    let watching = dir.as_deref().filter(|d| d.is_dir());
    match (&dir, &watching) {
        (None, _) => {
            // Breathing, not grey. A stop waiting on the user is not a stop
            // waiting its turn: nothing happens at all until this one is
            // answered, so it is the thing on the page that should catch the
            // eye rather than the thing that sits quietest.
            chain.set_state(0, State::Calling);
            chain.set_note(0, Some("click to choose"));
        }
        (Some(_), None) => {
            chain.set_state(0, State::Missing);
            chain.set_note(0, Some("folder is gone"));
        }
        (Some(d), Some(_)) => {
            chain.set_state(0, State::Done);
            chain.set_note(
                0,
                d.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .as_deref(),
            );
        }
    }
    chain.set_hint(
        0,
        if watching.is_some() {
            "Watch a different folder"
        } else {
            "Choose a folder to watch"
        },
    );

    // 2: something to do. Settling counts as arrived; there is no useful
    // difference to a reader between "seen" and "still being written".
    let waiting_on = ui.auto_settling.borrow().len() + ui.auto_queue.borrow().len();
    let doing = ui.watchdog_doing.borrow().clone();
    let holding = waiting_on > 0 || doing.is_some();
    let trouble = ui.watchdog_trouble.borrow().clone();
    // Which file, by name. "A file was found" is not news to anybody watching
    // a folder they put a file in; which one it was is.
    let found_name = doing.clone().or_else(|| {
        let named = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        ui.auto_queue
            .borrow()
            .front()
            .map(|p| named(p))
            .or_else(|| ui.auto_settling.borrow().first().map(|s| named(&s.path)))
    });
    if let Some((name, why)) = &trouble {
        // The stop that found it is the stop that crosses out. Nothing further
        // along was reached, so nothing further along should claim anything.
        chain.set_state(1, State::Missing);
        chain.set_note(1, Some(why));
        chain.set_hint(1, &format!("{name}: {why} - click to dismiss"));
    } else if watching.is_none() {
        // Nothing is being looked in, so nothing is being looked for.
        chain.set_state(1, State::Idle);
        chain.set_note(1, Some("no folder"));
    } else if holding {
        chain.set_state(1, State::Done);
        chain.set_note(1, found_name.as_deref().or(Some("file found")));
        chain.set_hint(1, "Click to skip this file");
    } else {
        chain.set_state(1, State::Live);
        chain.set_note(1, Some("looking"));
        chain.set_hint(1, "Choose a file to convert");
    }

    // 3: the conversion. Done stays white until the next file arrives, so the
    // chain finishes reading as a completed pass rather than emptying itself.
    //
    // With nowhere named to put the result, nothing converts at all, and that
    // has to be said here: a file found and then nothing happening, with no
    // reason given, is the exact thing this chain exists to prevent.
    let landed = ui.watchdog_landed.get();
    let held = target.is_none() && trouble.is_none();
    match (&doing, ui.watchdog_ready.borrow().is_some()) {
        (Some(_), _) => {
            chain.set_state(2, State::Live);
            chain.set_note(2, None);
        }
        (None, true) if landed => {
            chain.set_state(2, State::Done);
            chain.set_note(2, None);
        }
        (None, _) => {
            // Held for want of a drive is not idle: the file is here and this
            // is the step that would run next. It flashes for the same reason
            // "looking" does - something is pending - and it is safe to flash
            // beside the drive now that the two use different colours.
            chain.set_state(
                2,
                if held && holding {
                    State::Live
                } else {
                    State::Idle
                },
            );
            chain.set_note(2, (held && holding).then_some("nowhere to put it"));
            chain.set_link_note(3, None);
        }
    }

    // 4: where it ends up. Chosen and absent is the commonest reason nothing
    // arrives, and the one worth a cross.
    let sending = ui.watchdog_sending.get();
    // A folder and a drive are different destinations and should not wear the
    // same icon: which one it is decides whether "not there" means unplugged
    // or deleted, and the reader should not have to guess which.
    let to_folder = ui.settings.borrow().auto_target_dir.is_some();
    chain.set_icon(
        3,
        if to_folder {
            "folder-symbolic"
        } else {
            "drive-removable-media-symbolic"
        },
    );
    match (&target, &here) {
        (None, _) => {
            // Same reasoning as the folder: unset is not idle, it is blocking.
            chain.set_state(3, State::Calling);
            chain.set_note(3, Some("click to choose"));
        }
        (Some(name), None) => {
            chain.set_state(3, State::Missing);
            chain.set_note(3, Some(name.as_str()));
        }
        (Some(_), Some((_, name))) => {
            chain.set_state(
                3,
                if landed {
                    State::Landed
                } else if sending {
                    State::Live
                } else {
                    State::Idle
                },
            );
            chain.set_note(3, Some(name));
        }
    }
    chain.set_hint(
        3,
        if target.is_some() {
            "Send somewhere else"
        } else {
            "Choose where to send it"
        },
    );

    // The links. Bouncing is looking; filling is something crossing; empty is
    // a leg that is dead because the step before it is not satisfied.
    let mut bounce = Vec::new();
    let was_holding = ui.watchdog_found.replace(holding);
    if holding {
        // Run the file across the two legs it has actually travelled, one
        // after the other, rather than snapping them full. Nothing is measured
        // here - a file being found has no percentage - but it did move, and
        // showing it move is the difference between a chain that reports
        // states and one that shows a file passing through them. Started only
        // on the change, so a redraw part-way through does not restart it.
        chain.set_link(1, 1.0);
        if !was_holding {
            chain.clone().fill_link(2, 0.4, || {});
        }
    } else if landed {
        // The finished picture, held for a moment so the pass can be seen to
        // have completed. A chain that emptied the instant it succeeded would
        // never show anyone that it had.
        chain.set_link(1, 1.0);
        chain.set_link(2, 1.0);
    } else if watching.is_some() && trouble.is_none() {
        // Back to looking. The first leg stays solid, because the folder is
        // still there and that is all that leg has ever meant - it is the link
        // to the folder, not a search in progress. What the last file filled
        // beyond it is emptied, because a full bar means this file crossed
        // here and there is no this file any more. The looking is said by the
        // stop that is doing it, breathing on its own.
        chain.set_link(1, 1.0);
        chain.set_link(2, 0.0);
    } else {
        chain.set_link(1, 0.0);
        chain.set_link(2, 0.0);
    }
    // Link 3 carries the conversion: it is the leg with a real number to
    // report, and the one that ends where the file is meant to end up. Left
    // alone while converting, because the layer counter owns it then.
    if sending {
        bounce.push(3);
    } else if landed {
        chain.set_link(3, 1.0);
    } else if doing.is_none() {
        chain.set_link(3, 0.0);
    }
    chain.bounce(bounce);

    // And the line underneath: what it finished, and that it is round again.
    let ready = ui.watchdog_ready.borrow().clone();
    match (&doing, &ready) {
        _ if trouble.is_some() => {
            let (name, why) = trouble.as_ref().expect("just checked");
            chain.set_footer(Some(&format!("{name} could not be converted - {why}")))
        }
        (Some(name), _) => chain.set_footer(Some(&format!("Converting {name}\u{2026}"))),
        _ if held && holding => chain.set_footer(Some(
            "Found a file, but nowhere has been chosen to copy it to",
        )),
        (None, Some(last)) => chain.set_footer(Some(last)),
        (None, None) => chain.set_footer(None),
    }

    // The line above may have just appeared or gone. The fold holding the
    // chain was measured before it did, so it is measured again now.
    ui.watchdog_fold.refit();
}

/// How long is left, in the words a person would use.
///
/// Rounded hard on purpose. An estimate from a layer count is worth about one
/// significant figure, and printing "1m 47s left" claims a precision the
/// number does not have.
fn about_left(secs: f64) -> String {
    let secs = secs.max(0.0);
    if secs < 5.0 {
        return "almost done".into();
    }
    if secs < 60.0 {
        return format!("about {}s left", ((secs / 5.0).round() * 5.0) as u64);
    }
    let mins = (secs / 60.0).round() as u64;
    match mins {
        0 | 1 => "about a minute left".into(),
        n => format!("about {n}m left"),
    }
}

/// A name short enough to sit on a button without pushing the row apart.
fn shorten(name: &str) -> String {
    const MOST: usize = 18;
    if name.chars().count() <= MOST {
        return name.to_string();
    }
    let kept: String = name.chars().take(MOST - 1).collect();
    format!("{kept}\u{2026}")
}

/// Ask for the folder WatchDog should watch.
fn choose_watch_folder(ui: &Rc<App>) {
    let dialog = gtk::FileDialog::builder().title("Folder to watch").build();
    if let Some(start) = ui.settings.borrow().auto_watch_dir.clone() {
        dialog.set_initial_folder(Some(&gio::File::for_path(start)));
    }
    let ui = ui.clone();
    dialog.select_folder(
        Some(&ui.window.clone()),
        gio::Cancellable::NONE,
        move |res| {
            let Ok(folder) = res else { return };
            let Some(path) = folder.path() else { return };
            {
                let mut s = ui.settings.borrow_mut();
                s.auto_watch_dir = Some(path);
                let _ = s.save();
            }
            rearm_auto(&ui);
        },
    );
}

/// Hand WatchDog a file by hand, rather than waiting for one to appear.
///
/// Opening the watched folder in a file manager was no use: it showed the
/// files but there was nothing to do with them there, and the program was left
/// out of it. This puts the chosen file into the same queue a dropped-in file
/// goes to, so it is converted and delivered exactly as an automatic one is -
/// which is the point of picking it from this stop rather than from Browse
/// Files, where it would join the manual queue instead.
fn watchdog_pick_file(ui: &Rc<App>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Compatible files"));
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
        .title("File for WatchDog to convert")
        .filters(&filters)
        .modal(true)
        .build();
    // The watched folder is where the answer usually is, so that is where it
    // opens.
    if let Some(dir) = ui.settings.borrow().auto_watch_dir.clone() {
        if dir.is_dir() {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
    }

    let ui = ui.clone();
    dialog.open(
        Some(&ui.window.clone()),
        gio::Cancellable::NONE,
        move |res| {
            let Some(path) = res.ok().and_then(|f| f.path()) else {
                return;
            };
            if !path.is_file() {
                return;
            }
            // Asked for by name, so it goes whether or not it has been seen
            // before. The record of what has been converted exists to stop the
            // folder monitor doing the same file twice on its own; it has no
            // business refusing a file somebody just pointed at.
            if let Some(key) = auto_key(&path) {
                ui.auto_done.borrow_mut().retain(|k| *k != key);
            }
            *ui.watchdog_trouble.borrow_mut() = None;
            // Straight into the queue rather than through the settling watch: a
            // file chosen from a dialog has finished being written, or it would
            // not have been there to choose.
            ui.auto_queue.borrow_mut().push_back(path);
            auto_pump(&ui);
            refresh_watchdog_steps(&ui);
        },
    );
}

/// Drop whatever WatchDog is holding, without converting it.
///
/// A file dropped into the folder by accident, or one waiting on a drive that
/// is not coming, otherwise sits at the head of the queue for the rest of the
/// session. Recorded as done so the folder monitor does not offer it straight
/// back, which would make the button look broken.
fn watchdog_skip(ui: &Rc<App>) -> bool {
    let skipped = ui.auto_queue.borrow_mut().pop_front().or_else(|| {
        let mut settling = ui.auto_settling.borrow_mut();
        (!settling.is_empty()).then(|| settling.remove(0).path)
    });
    let Some(path) = skipped else {
        return false;
    };
    if let Some(key) = auto_key(&path) {
        ui.auto_done.borrow_mut().push(key);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    refresh_watchdog_steps(ui);
    ui.toasts
        .add_toast(adw::Toast::new(&format!("Skipped {name}")));
    true
}

/// The formats WatchDog could convert into.
///
/// Only formats that can be written, in alphabetical order, with the one in
/// force ticked. The same list the output picker offers, asked from the stop
/// that raises the question rather than from a page two clicks away.
fn watchdog_format_menu(ui: &Rc<App>) -> gtk::Popover {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("cz-menu");
    let popover = gtk::Popover::builder().child(&list).build();
    popover.add_css_class("menu");

    let ui = ui.clone();
    let pop = popover.clone();
    popover.connect_show(move |_| {
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        let now = ui.settings.borrow().auto_to_format.clone();
        let mut formats = registry::writable();
        formats.sort_by_key(|i| i.name);
        for info in formats {
            let row = adw::ActionRow::builder()
                .title(info.name)
                .subtitle(format!(".{}", info.extension))
                .activatable(true)
                .build();
            if info.id == now {
                row.add_suffix(&gtk::Image::from_icon_name("object-select-symbolic"));
            }
            let ui2 = ui.clone();
            let pop2 = pop.clone();
            let id = info.id.to_string();
            let name = info.name.to_string();
            row.connect_activated(move |_| {
                {
                    let mut s = ui2.settings.borrow_mut();
                    s.auto_to_format = id.clone();
                    let _ = s.save();
                }
                pop2.popdown();
                refresh_auto_indicator(&ui2);
                refresh_watchdog_steps(&ui2);
                ui2.toasts
                    .add_toast(adw::Toast::new(&format!("WatchDog will convert to {name}")));
            });
            list.append(&row);
        }
    });
    popover
}

/// Show a completed pass for a moment, then hand the chain back to looking.
///
/// Left as it was, a finished pass is four full bars and a green drive that
/// never change again - which reads as stuck, not as done, and says nothing
/// about whether the next file would be picked up. Held briefly it reads as a
/// result; released it reads as ready. The sentence underneath keeps the
/// record either way, because that is where a record belongs.
fn watchdog_take_a_bow(ui: &Rc<App>) {
    ui.watchdog_landed.set(true);
    refresh_watchdog_steps(ui);
    let ui = ui.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(2600), move || {
        // Unless another file has come along in the meantime, in which case it
        // owns the chain now and this one's finish is old news.
        if ui.watchdog_doing.borrow().is_some() {
            return;
        }
        ui.watchdog_landed.set(false);
        refresh_watchdog_steps(&ui);
    });
}

/// What WatchDog was told to write to, and where that is right now.
///
/// Two kinds of answer. A removable drive is remembered by filesystem UUID,
/// because its label is not unique and its mount point moves between plugs. A
/// folder is remembered by its path, because that is all a folder is.
///
/// The removable-only check still guards the drive case: that path picks a
/// destination from whatever happens to be mounted, so it has to be sure it is
/// a stick and not somebody's root partition. A folder named by hand needs no
/// such guard - it was not guessed at, it was chosen.
///
/// Returns the name it was given, and where to write if that place is here.
fn watchdog_where(ui: &Rc<App>) -> (Option<String>, Option<(PathBuf, String)>) {
    let (uuid, label, dir) = {
        let s = ui.settings.borrow();
        (
            s.auto_target_uuid.clone(),
            s.auto_target_label.clone(),
            s.auto_target_dir.clone(),
        )
    };
    if let Some(dir) = dir {
        let name = label.unwrap_or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.display().to_string())
        });
        let live = dir.is_dir().then(|| (dir, name.clone()));
        return (Some(name), live);
    }
    if let Some(uuid) = uuid {
        let live = drives::by_uuid(&uuid)
            .filter(|d| d.removable && !drives::is_system_mount(&d.path))
            .map(|d| (d.path, d.name));
        return (label.or_else(|| Some("a drive".into())), live);
    }
    (None, None)
}

/// Whether a folder is a sane place to have WatchDog write into.
///
/// Not a judgement about what the user wants - they picked it - only about
/// what would break. Writing output into the folder being watched would be
/// fine on extension grounds, but a folder that cannot be written to at all is
/// a destination that will fail silently on every file.
fn watchdog_dir_usable(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err("that folder does not exist".into());
    }
    let probe = dir.join(".cheapazsla-write-test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!("cannot write there: {e}")),
    }
}

/// Ask for a folder for WatchDog to write into.
fn choose_watchdog_dir(ui: &Rc<App>) {
    let dialog = gtk::FileDialog::builder()
        .title("Folder to save into")
        .modal(true)
        .build();
    if let Some(start) = ui.settings.borrow().auto_target_dir.clone() {
        dialog.set_initial_folder(Some(&gio::File::for_path(start)));
    }
    let ui = ui.clone();
    dialog.select_folder(
        Some(&ui.window.clone()),
        gio::Cancellable::NONE,
        move |res| {
            let Some(path) = res.ok().and_then(|f| f.path()) else {
                return;
            };
            // Checked before it is kept, because a destination that cannot be
            // written to fails once per file, quietly, for as long as it is set.
            if let Err(why) = watchdog_dir_usable(&path) {
                ui.toasts
                    .add_toast(adw::Toast::new(&format!("Not saved - {why}")));
                return;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            {
                let mut s = ui.settings.borrow_mut();
                // One or the other, never both: two destinations is a question
                // about which one wins, and there is no good answer to it.
                s.auto_target_dir = Some(path);
                s.auto_target_uuid = None;
                s.auto_target_label = Some(name.clone());
                let _ = s.save();
            }
            refresh_auto_indicator(&ui);
            refresh_watchdog_steps(&ui);
            auto_deliver(&ui);
            auto_pump(&ui);
            ui.toasts
                .add_toast(adw::Toast::new(&format!("WatchDog will save into {name}")));
        },
    );
}

/// The menu of drives WatchDog could copy to.
///
/// Rebuilt every time it opens, because drives come and go while the window is
/// up. Only removable ones, and only ones with a filesystem UUID - a drive
/// that cannot be told apart from another is not a drive this may write to
/// unattended.
///
/// Returned rather than attached, because the same menu is offered from two
/// places: the row in WatchDog's panel, and the Drive stop on the chain. They
/// are the same question, so they get the same answer.
fn watchdog_drive_menu(ui: &Rc<App>) -> gtk::Popover {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("cz-menu");
    let popover = gtk::Popover::builder().child(&list).build();
    popover.add_css_class("menu");

    let ui = ui.clone();
    let pop = popover.clone();
    popover.connect_show(move |_| {
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        let usable: Vec<(drives::Drive, String)> = drives::mounted()
            .into_iter()
            .filter(|d| d.removable && !drives::is_system_mount(&d.path))
            .filter_map(|d| drives::uuid_of(&d.path).map(|u| (d, u)))
            .collect();

        if usable.is_empty() {
            let empty = gtk::Label::new(Some("No drive is plugged in"));
            empty.add_css_class("cz-dim");
            empty.set_margin_top(theme::SPACE_3);
            empty.set_margin_bottom(theme::SPACE_3);
            empty.set_margin_start(theme::SPACE_3);
            empty.set_margin_end(theme::SPACE_3);
            list.append(
                &gtk::ListBoxRow::builder()
                    .child(&empty)
                    .activatable(false)
                    .build(),
            );
        }

        for (drive, uuid) in usable {
            let row = adw::ActionRow::builder()
                .title(&drive.name)
                // The path, because two unlabelled sticks are both called
                // "4.0 GB Volume" and the only thing telling them apart on
                // screen is where they are mounted.
                .subtitle(drive.path.display().to_string())
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(
                "drive-removable-media-symbolic",
            ));
            let ui2 = ui.clone();
            let pop2 = pop.clone();
            let name = drive.name.clone();
            row.connect_activated(move |_| {
                {
                    let mut s = ui2.settings.borrow_mut();
                    s.auto_target_uuid = Some(uuid.clone());
                    s.auto_target_dir = None;
                    s.auto_target_label = Some(name.clone());
                    let _ = s.save();
                }
                pop2.popdown();
                // Not a rearm: rearming reseeds the folder as already-seen,
                // which would throw away the very file that has been waiting
                // for a drive to be chosen. Only the destination changed, so
                // only what depends on the destination is put back in step.
                refresh_auto_indicator(&ui2);
                refresh_watchdog_steps(&ui2);
                auto_deliver(&ui2);
                auto_pump(&ui2);
                ui2.toasts
                    .add_toast(adw::Toast::new(&format!("WatchDog will copy to {name}")));
            });
            list.append(&row);
        }

        // Not every printer takes a stick, and not every workflow ends at one:
        // a network share, a second disk, a folder something else is watching.
        // The destination only has to be somewhere that can be written to.
        let pick = adw::ActionRow::builder()
            .title("Choose a folder\u{2026}")
            .subtitle("Any folder, network share or disk")
            .activatable(true)
            .build();
        pick.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
        let ui3 = ui.clone();
        let pop3 = pop.clone();
        pick.connect_activated(move |_| {
            pop3.popdown();
            choose_watchdog_dir(&ui3);
        });
        list.append(&pick);
    });
    popover
}

/// Say what is missing, rather than walking off to another page to show it.
///
/// Switching this on used to jump straight to Settings, which is a page change
/// nobody asked for in answer to a switch they did press - and the row on the
/// Convert page already says what is not set up yet. So it says it again here,
/// once, and stays where it is.
fn watchdog_needs_setup(ui: &Rc<App>, on: bool) {
    if !on {
        return;
    }
    let (dir, target) = {
        let s = ui.settings.borrow();
        (s.auto_watch_dir.is_some(), s.auto_target_uuid.is_some())
    };
    let missing = match (dir, target) {
        (true, true) => return,
        (false, true) => "a folder to watch",
        (true, false) => "a drive to copy to",
        (false, false) => "a folder to watch and a drive to copy to",
    };
    ui.toasts.add_toast(adw::Toast::new(&format!(
        "WatchDog is on, but still needs {missing}. Settings, WatchDog mode."
    )));
}

/// Say, on screen, whether automatic mode is running and what it will do.
///
/// Two places, on purpose. The badge in the title bar is there from every page
/// so the answer to "is this thing armed" never needs looking for. The banner
/// is over the Convert page, where the files it acts on appear, and it spells
/// out the whole sentence - which folder, which format, which drive - because
/// a light that says only "on" leaves the reader to guess at the rest.
fn refresh_auto_indicator(ui: &Rc<App>) {
    let (on, dir, to) = {
        let s = ui.settings.borrow();
        (
            s.auto_convert,
            s.auto_watch_dir.clone(),
            s.auto_to_format.clone(),
        )
    };
    // Resolved the same way the chain resolves it. This read auto_target_label
    // straight out of the settings, and that field is only ever filled in for
    // a drive - so choosing a plain folder left the row, the button and the
    // status line all saying nothing had been chosen while WatchDog was
    // already copying into it.
    let target = watchdog_where(ui).0;
    // The eye, and everything that has to agree with it. Set rather than
    // toggled, and only when it disagrees, so nothing here can start a loop
    // with the handler that brought it about.
    if ui.watchdog_eye.is_active() != on {
        ui.watchdog_eye.set_active(on);
    }
    if on {
        ui.watchdog_eye.add_css_class("cz-armed");
    } else {
        ui.watchdog_eye.remove_css_class("cz-armed");
    }
    if let Some(row) = ui.auto_switch.borrow().as_ref() {
        if row.is_active() != on {
            row.set_active(on);
        }
    }

    if ui.watchdog_switch.is_active() != on {
        ui.watchdog_switch.set_active(on);
    }
    // Short enough to take in without reading. The row is a status line, not
    // an explanation - "Downloads to GOO to SATURN" says the whole of it, and
    // the long version was a sentence nobody finishes.
    let where_ = dir
        .as_ref()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()));
    // Where the chosen drive is right now, if it is here at all. Said out
    // loud because a drive that is set but unplugged looks identical to one
    // that is set and ready, and they are not the same situation.
    let drive_at = {
        let s = ui.settings.borrow();
        s.auto_target_uuid
            .as_deref()
            .and_then(drives::by_uuid)
            .map(|d| d.path.display().to_string())
    };
    refresh_dropzone_text(ui);
    // The buttons carry the answer, the rows carry the detail. A control that
    // still says "Choose..." after something has been chosen looks like it did
    // not take - which is exactly what it looked like.
    ui.watchdog_folder.set_subtitle(&match &dir {
        Some(d) => d.display().to_string(),
        None => "Not chosen".into(),
    });
    ui.watchdog_folder_btn.set_label(&match &where_ {
        Some(name) => shorten(name),
        None => "Choose…".to_string(),
    });
    let to_folder = ui.settings.borrow().auto_target_dir.clone();
    ui.watchdog_drive
        .set_subtitle(&match (&target, &to_folder) {
            // A folder is either there or it is not; "not plugged in" is a thing
            // only a drive can be.
            (Some(_), Some(path)) => match path.is_dir() {
                true => path.display().to_string(),
                false => format!("{} - folder is gone", path.display()),
            },
            (Some(label), None) => match &drive_at {
                Some(mount) => format!("{label} - {mount}"),
                None => format!("{label} - not plugged in"),
            },
            (None, _) => "Not chosen".into(),
        });
    ui.watchdog_format.set_subtitle(
        &registry::by_id(&to)
            .map(|h| h.info().name.to_string())
            .unwrap_or_else(|| to.to_uppercase()),
    );
    // Only for a real, present, removable drive: a folder cannot be ejected,
    // and an unplugged stick is already out.
    ui.watchdog_eject.set_visible(
        to_folder.is_none()
            && ui
                .settings
                .borrow()
                .auto_target_uuid
                .as_deref()
                .and_then(drives::by_uuid)
                .is_some_and(|d| d.removable),
    );
    ui.watchdog_format_btn.set_label(&shorten(
        &registry::by_id(&to)
            .map(|h| h.info().extension.to_uppercase())
            .unwrap_or_else(|| to.to_uppercase()),
    ));
    ui.watchdog_drive_btn.set_label(&match &target {
        Some(label) => shorten(label),
        None => "Choose…".to_string(),
    });

    if !on {
        ui.watchdog_row.set_subtitle("Off");
        ui.watchdog_eye.set_tooltip_text(Some("WatchDog is off"));
        return;
    }

    let said = match (&where_, &target) {
        (None, _) => "On - choose a folder to watch".to_string(),
        (Some(w), None) => format!("{w} to {} - nowhere to save it", to.to_uppercase()),
        (Some(w), Some(t)) => format!("{w} to {} to {t}", to.to_uppercase()),
    };
    ui.watchdog_row.set_subtitle(&said);
    ui.watchdog_eye
        .set_tooltip_text(Some(&format!("WatchDog: {said}")));
}

/// The last few things WatchDog converted on its own.
///
/// A short list, not the whole history - the point is "has it been working",
/// which the last handful answers and two hundred rows do not. History is one
/// click away for the rest.
fn refresh_watchdog_recent(ui: &Rc<App>) {
    while let Some(row) = ui.watchdog_recent.first_child() {
        ui.watchdog_recent.remove(&row);
    }
    let entries: Vec<history::Entry> = ui
        .history
        .borrow()
        .entries
        .iter()
        .filter(|e| e.automatic)
        .take(5)
        .cloned()
        .collect();
    if entries.is_empty() {
        let row = adw::ActionRow::builder()
            .title("Nothing yet")
            .subtitle("Files WatchDog converts will be listed here")
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("document-open-recent-symbolic"));
        ui.watchdog_recent.append(&row);
        return;
    }
    for e in entries {
        let row = adw::ActionRow::builder()
            .title(e.destination_name())
            .subtitle(format!("from {}  ·  {}", e.source_name(), ago(e.when)))
            .title_lines(1)
            .subtitle_lines(1)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(match e.outcome {
            history::Outcome::Complete => "object-select-symbolic",
            history::Outcome::Failed => "dialog-error-symbolic",
        }));
        if e.output_exists() {
            let open = shell::icon_button("folder-open-symbolic", "Open containing folder");
            let dest = e.destination.clone();
            open.connect_clicked(move |_| {
                if let Some(parent) = dest.parent() {
                    let uri = gio::File::for_path(parent).uri();
                    let _ = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
                }
            });
            row.add_suffix(&open);
        }
        ui.watchdog_recent.append(&row);
    }
}

/// Start or stop watching the folder that automatic mode reads.
///
/// A folder monitor rather than a timer: the interesting moment is a slicer
/// finishing an export, and asking the filesystem to say when that happens
/// costs nothing while nothing is happening.
fn rearm_auto(ui: &Rc<App>) {
    refresh_auto_indicator(ui);
    *ui.auto_watch.borrow_mut() = None;
    ui.auto_settling.borrow_mut().clear();
    ui.auto_queue.borrow_mut().clear();
    ui.auto_ticking.set(false);

    let (on, dir) = {
        let s = ui.settings.borrow();
        (s.auto_convert, s.auto_watch_dir.clone())
    };
    let (true, Some(dir)) = (on, dir) else { return };
    if !dir.is_dir() {
        return;
    }
    // A folder that is also where output goes would convert its own results
    // round and round. The extension check catches the usual case; this
    // catches the arrangement itself, before it has a chance to happen once.
    for mode in [auto::Staging::Disk, auto::Staging::Ram] {
        if auto::staging_dir(mode).is_some_and(|s| dir.starts_with(&s) || s.starts_with(&dir)) {
            return;
        }
    }
    // And a folder on a removable drive is a folder that can vanish under the
    // monitor, which is a folder worth not watching.
    if drives::containing(&dir).is_some_and(|d| d.removable) {
        return;
    }

    // Only what arrives from now on. Switching this on should not convert
    // every file that has ever been left in the folder.
    let known: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        // Bounded, because somebody will point this at a folder with a hundred
        // thousand files in it and the seed list is only there to stop old
        // files being converted the moment this is switched on.
        .take(20_000)
        .filter_map(|e| auto_key(&e.path()))
        .collect();
    *ui.auto_done.borrow_mut() = known;

    let Ok(monitor) = gio::File::for_path(&dir)
        .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
    else {
        return;
    };
    let ui2 = ui.clone();
    monitor.connect_changed(move |_, file, _, _| {
        let Some(path) = file.path() else { return };
        auto_saw(&ui2, path);
    });
    *ui.auto_watch.borrow_mut() = Some(monitor);
}

/// What identifies a particular version of a particular file.
fn auto_key(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let when = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(format!("{}|{}|{when}", path.display(), meta.len()))
}

/// A file has appeared or changed. Start watching it settle.
fn auto_saw(ui: &Rc<App>, path: PathBuf) {
    if !path.is_file() {
        return;
    }
    // Only formats that can be read, and never the format being written - a
    // folder that is both watched and written to would otherwise convert its
    // own output round and round.
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return;
    };
    let to = ui.settings.borrow().auto_to_format.clone();
    let readable = registry::by_extension(ext).is_some_and(|h| h.info().capabilities.reads);
    let is_output = registry::by_extension(ext).map(|h| h.info().id) == Some(to.as_str());
    if !readable || is_output {
        return;
    }
    {
        let mut settling = ui.auto_settling.borrow_mut();
        if settling.len() >= AUTO_WATCH_MOST || settling.iter().any(|s| s.path == path) {
            return;
        }
        let Some(watch) = auto::Settling::watch(path) else {
            return;
        };
        settling.push(watch);
    }
    // Start the loop that checks whether it has finished being written. It
    // used to be started only when the folder was armed, which is the one
    // moment nothing is settling - so the first file to arrive was watched and
    // then never looked at again.
    auto_start_ticking(ui);
    // Something turning up is the whole event this chain is about, so say so
    // now rather than at the next thing that happens to redraw it. Without
    // this the chain read "looking" from the drop right through to the
    // conversion starting, and said nothing at all when the file could not
    // proceed for want of a drive.
    refresh_watchdog_steps(ui);
}

/// Begin checking settling files, if that is not already happening.
fn auto_start_ticking(ui: &Rc<App>) {
    if ui.auto_ticking.replace(true) {
        return;
    }
    let ui2 = ui.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
        auto_tick(&ui2)
    });
}

/// How long a file has to stop changing before it is taken as finished.
const AUTO_QUIET: std::time::Duration = std::time::Duration::from_secs(3);
/// And how long it is given to manage that before it is taken as something
/// that is never going to.
const AUTO_PATIENCE: std::time::Duration = std::time::Duration::from_secs(600);
/// How many files may be waiting to settle at once. A folder someone drops a
/// thousand files into should not become a thousand timers.
const AUTO_WATCH_MOST: usize = 64;

/// Look at everything waiting to settle, and queue what has.
///
/// This only runs while something is actually settling, and stops the moment
/// nothing is. The rest of the time the folder monitor is doing the watching,
/// and a monitor that nothing has happened to costs nothing at all - which is
/// the whole reason to use one rather than look every few seconds.
fn auto_tick(ui: &Rc<App>) {
    let ready: Vec<PathBuf> = {
        let mut settling = ui.auto_settling.borrow_mut();
        let mut ready = Vec::new();
        settling.retain_mut(|s| {
            if s.given_up(AUTO_PATIENCE) {
                return false;
            }
            if s.settled(AUTO_QUIET) {
                ready.push(s.path.clone());
                return false;
            }
            s.path.exists()
        });
        ready
    };
    for path in ready {
        ui.auto_queue.borrow_mut().push_back(path);
    }
    auto_pump(ui);
    refresh_watchdog_steps(ui);

    // Kept ticking only while there is something to tick for. The rest of the
    // time this costs nothing at all, which on a slow machine is the point.
    if ui.auto_settling.borrow().is_empty() {
        ui.auto_ticking.set(false);
        return;
    }
    let ui2 = ui.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
        auto_tick(&ui2)
    });
}

/// Convert the next queued file, if nothing else is being converted.
fn auto_pump(ui: &Rc<App>) {
    if ui.auto_busy.get() {
        return;
    }
    // Nowhere to send it, so it waits. Converting into a staging folder for a
    // drive nobody has named is work that was never asked for, against a
    // destination that may never exist - and it happens quietly, which is the
    // worst way for a program to do something surprising. The file stays in
    // the queue instead, so the chain can show it found something and is held
    // up, and choosing a drive starts it moving.
    //
    // A drive that is chosen but unplugged is a different case and keeps its
    // old behaviour: convert now, copy when it turns up. There the answer to
    // "where is this going" is known.
    if watchdog_where(ui).0.is_none() {
        return;
    }
    let Some(next) = ui.auto_queue.borrow_mut().pop_front() else {
        return;
    };
    ui.auto_busy.set(true);
    auto_convert_one(ui, next);
}

/// Convert one settled file, and either stage it or send it straight over.
///
/// Every way out of this releases the queue, including the ones that give up
/// without converting anything. A path that forgets to would leave automatic
/// mode looking switched on and doing nothing for the rest of the session.
fn auto_convert_one(ui: &Rc<App>, source: PathBuf) {
    let release = |ui: &Rc<App>| {
        ui.auto_busy.set(false);
        let ui2 = ui.clone();
        // Next one on a fresh turn of the loop rather than straight down the
        // stack, so a folder of fifty files cannot recurse into the ground.
        glib::idle_add_local_once(move || auto_pump(&ui2));
    };

    let Some(key) = auto_key(&source) else {
        release(ui);
        return;
    };
    if ui.auto_done.borrow().contains(&key) {
        release(ui);
        return;
    }

    let (to, asked, cap_mb, keep_days, budget) = {
        let s = ui.settings.borrow();
        (
            s.auto_to_format.clone(),
            auto::Staging::from_id(&s.auto_staging),
            s.auto_cap_mb as u64,
            s.auto_keep_days as u64,
            s.ram_budget_mb as u64,
        )
    };
    let drive = watchdog_where(ui).1;

    let coming = std::fs::metadata(&source).map(|m| m.len()).unwrap_or(0);
    let mode = auto::resolve_staging(asked, budget, coming);

    // Nowhere named to put the result: leave the file alone and
    // leave it unrecorded, so plugging the drive in and re-saving still works.
    if drive.is_none() && mode == auto::Staging::OnDemand {
        release(ui);
        return;
    }

    let into = match (&drive, auto::usable_dir(mode)) {
        // The destination is here, so there is nothing to stage: convert
        // straight onto it.
        (Some((path, _)), _) => path.clone(),
        (None, Some(dir)) => dir,
        (None, None) => {
            release(ui);
            return;
        }
    };

    if drive.is_none() {
        auto::prune(
            &into,
            cap_mb * 1024 * 1024,
            std::time::Duration::from_secs(keep_days * 86_400),
        );
        // Refusing rather than overflowing: a cap that gives way under
        // pressure is a cap in name only. Not recorded as done either, so it
        // is converted after the drive has been emptied.
        if auto::waiting(&into).bytes + coming > cap_mb * 1024 * 1024 {
            ui.toasts.add_toast(adw::Toast::new(
                "Files waiting for the drive have filled their limit, so nothing was converted",
            ));
            release(ui);
            return;
        }
    }

    let Some(dest) = convert::destination_for(&source, &to, Some(&into)) else {
        release(ui);
        return;
    };
    // Never over the top of something already there. This is the one place
    // that writes files nobody asked it to write, so it does not get to
    // replace anything.
    let dest = if dest.exists() {
        convert::unique_path(&dest)
    } else {
        dest
    };
    let plan = match convert::plan(&source, &to, &dest) {
        Ok(p) => p,
        Err(e) => {
            let name = source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Held on the chain rather than only thrown at a toast. A toast
            // is gone in five seconds; the question "why did nothing happen to
            // my file" is asked long after that, and the chain is where it
            // gets asked.
            *ui.watchdog_trouble.borrow_mut() = Some((name.clone(), e.headline().to_string()));
            ui.toasts
                .add_toast(adw::Toast::new(&format!("{name}: {e}")));
            // Recorded, so a file that cannot be converted is not retried on
            // every touch for the rest of the session.
            ui.auto_done.borrow_mut().push(key);
            release(ui);
            refresh_watchdog_steps(ui);
            return;
        }
    };
    {
        // Bounded. A long session converting a great many files should not
        // turn its own memory of what it has done into the thing that grows.
        let mut done = ui.auto_done.borrow_mut();
        done.push(key);
        if done.len() > 5_000 {
            done.drain(..1_000);
        }
    }

    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Layer counts as they happen, so the chain can show how far in it is
    // rather than only that something is going on.
    let (from_id, to_id, layer_count) = (
        plan.from.id.to_string(),
        plan.to.id.to_string(),
        plan.layer_count,
    );
    let (ptx, prx) = async_channel::unbounded::<(u32, u32)>();
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = convert::run_with_progress(&plan, move |done, total| {
            let _ = ptx.send_blocking((done, total));
        })
        .map_err(|e| e.to_string());
        let _ = tx.send_blocking(result);
    });
    *ui.watchdog_doing.borrow_mut() = Some(name.clone());
    // A new file starts the chain over. Whatever finished last is no longer
    // the news, and the green at the end belonged to that one, not this one.
    *ui.watchdog_ready.borrow_mut() = None;
    *ui.watchdog_trouble.borrow_mut() = None;
    ui.watchdog_landed.set(false);
    ui.watchdog_steps.set_link(3, 0.0);
    refresh_watchdog_steps(ui);
    {
        // Layers done against layers left, turned into the time a person would
        // say. Timed from the first report rather than from here, so the cost
        // of opening the file does not get spread over every estimate after
        // it.
        let ui = ui.clone();
        glib::spawn_future_local(async move {
            let mut began: Option<(std::time::Instant, u32)> = None;
            while let Ok((done, total)) = prx.recv().await {
                if total == 0 {
                    continue;
                }
                ui.watchdog_steps.set_link(3, done as f64 / total as f64);
                let (started, from) = *began.get_or_insert((std::time::Instant::now(), done));
                let made = done.saturating_sub(from);
                // Nothing is said until there is enough behind us to say it
                // from. An estimate off the first layer is a guess wearing a
                // number.
                if made < 8 {
                    continue;
                }
                let each = started.elapsed().as_secs_f64() / made as f64;
                let left = total.saturating_sub(done) as f64 * each;
                ui.watchdog_steps.set_link_note(3, Some(&about_left(left)));
            }
            ui.watchdog_steps.set_link_note(3, None);
        });
    }
    let ui = ui.clone();
    let source = source.clone();
    let landed = drive.is_some();
    // What the place is called, so the sentence names it. "Copied to the
    // drive" is wrong the moment the destination is a folder, and it is a
    // folder whenever somebody chose one.
    let where_to = drive
        .map(|(_, name)| name)
        .or_else(|| ui.settings.borrow().auto_target_label.clone())
        .unwrap_or_else(|| "the drive".into());
    glib::spawn_future_local(async move {
        let result = rx.recv().await;
        ui.auto_busy.set(false);
        *ui.watchdog_doing.borrow_mut() = None;
        refresh_dropzone_text(&ui);
        let ui2 = ui.clone();
        glib::idle_add_local_once(move || auto_pump(&ui2));
        let Ok(result) = result else { return };
        // Recorded like any other conversion, and marked as WatchDog's. This
        // is the one kind of entry nobody remembers making - it can happen
        // while the window is not even being looked at - so history saying it
        // happened without saying who asked is worse than not saying at all.
        {
            let mut hist = ui.history.borrow_mut();
            hist.record(history::Entry {
                when: history::now(),
                source: source.clone(),
                destination: dest.clone(),
                from_format: from_id.clone(),
                to_format: to_id.clone(),
                layers: layer_count,
                outcome: match &result {
                    Ok(()) => history::Outcome::Complete,
                    Err(_) => history::Outcome::Failed,
                },
                detail: result.as_ref().err().cloned().unwrap_or_default(),
                automatic: true,
            });
        }
        refresh_history(&ui);
        refresh_watchdog_recent(&ui);
        match result {
            Ok(_) => {
                *ui.watchdog_ready.borrow_mut() = Some(if landed {
                    format!("Copied {name} to {where_to} - ready for the next file")
                } else {
                    // Not finished, so not offered as finished. The file is
                    // converted and sitting in the staging folder, and saying
                    // "ready for the next file" here would read as the job
                    // having ended where it has not.
                    format!("Converted {name} - waiting for {where_to}")
                });
                ui.toasts.add_toast(adw::Toast::new(&if landed {
                    format!("{name} converted into {where_to}")
                } else {
                    format!("{name} converted and waiting for {where_to}")
                }));
                refresh_nearby(&ui);
                if landed {
                    // Converting straight onto the drive means the last leg
                    // has already happened by the time we hear about it. Run
                    // the bar across anyway and turn the drive green at the
                    // end of it: the chain is a story of where the file went,
                    // and that leg is part of the story.
                    let ui2 = ui.clone();
                    ui.watchdog_steps
                        .clone()
                        .fill_link(3, 0.6, move || watchdog_take_a_bow(&ui2));
                }
            }
            Err(e) => {
                ui.toasts
                    .add_toast(adw::Toast::new(&format!("{name}: {e}")));
            }
        }
        refresh_watchdog_steps(&ui);
    });
}

/// Send anything waiting to the drive that has just turned up.
///
/// The copying happens off the main thread. These are print files, which run
/// to tens of megabytes onto a USB stick, and a copy done here would freeze
/// the window for the whole of it - including the chain that is supposed to be
/// showing that the copy is happening.
fn auto_deliver(ui: &Rc<App>) {
    let (on, mode) = {
        let s = ui.settings.borrow();
        (s.auto_convert, auto::Staging::from_id(&s.auto_staging))
    };
    if !on || ui.watchdog_sending.get() {
        return;
    }
    let Some(dir) = auto::staging_dir(mode) else {
        return;
    };
    let Some((into, name)) = watchdog_where(ui).1 else {
        return;
    };
    let waiting = auto::waiting(&dir);
    if waiting.files.is_empty() {
        return;
    }

    ui.watchdog_sending.set(true);
    *ui.watchdog_ready.borrow_mut() = None;
    ui.watchdog_landed.set(false);
    ui.watchdog_steps.set_link(3, 0.0);
    refresh_watchdog_steps(ui);

    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let mut sent = 0usize;
        let mut trouble = None;
        for staged in waiting.files {
            match auto::deliver(&staged, &into) {
                Ok(_) => sent += 1,
                Err(e) => {
                    trouble = Some(e);
                    break;
                }
            }
        }
        let _ = tx.send_blocking((sent, trouble));
    });

    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let got = rx.recv().await;
        ui.watchdog_sending.set(false);
        let (sent, trouble) = got.unwrap_or((0, None));
        if let Some(e) = trouble {
            ui.toasts.add_toast(adw::Toast::new(&format!(
                "Could not copy to the drive: {e}"
            )));
        }
        if sent > 0 {
            *ui.watchdog_ready.borrow_mut() = Some(match sent {
                1 => format!("Copied 1 file to {name} - ready for the next file"),
                n => format!("Copied {n} files to {name} - ready for the next file"),
            });
            ui.toasts.add_toast(adw::Toast::new(&match sent {
                1 => format!("1 file copied to {name}"),
                n => format!("{n} files copied to {name}"),
            }));
            refresh_nearby(&ui);
            let ui2 = ui.clone();
            ui.watchdog_steps
                .clone()
                .fill_link(3, 0.5, move || watchdog_take_a_bow(&ui2));
        }
        refresh_watchdog_steps(&ui);
    });
}

/// Notice sources that have been written again since they were converted.
///
/// Re-slicing over the same filename is the ordinary way of working: fix the
/// supports, export again, convert again. Without this the row still says
/// Complete and the tick is still off, so the file that has just changed is
/// the one file the button will not touch.
fn recheck_edits(ui: &Rc<App>) {
    let mut woken: Vec<String> = Vec::new();
    {
        let mut files = ui.files.borrow_mut();
        for f in files.iter_mut() {
            if !matches!(f.status, Status::Complete(_)) || f.changed_since {
                continue;
            }
            let Some(now) = std::fs::metadata(&f.path)
                .ok()
                .and_then(|m| m.modified().ok())
            else {
                continue;
            };
            if f.edited.is_some_and(|was| now <= was) {
                continue;
            }
            f.edited = Some(now);
            f.changed_since = true;
            f.selected = true;
            woken.push(f.name());
        }
    }
    if woken.is_empty() {
        return;
    }
    refresh_queue(ui);
    revalidate(ui);
    ui.toasts.add_toast(adw::Toast::new(&match woken.len() {
        1 => format!("{} changed on disk - ticked to convert again", woken[0]),
        n => format!("{n} files changed on disk - ticked to convert again"),
    }));
}

/// Rebuild the "Quick Access" list.
///
/// Cheap enough to call on any event that might have changed the answer: a
/// drive appearing, the window regaining focus, a file being queued. It reads
/// directory entries and no file contents, so there is no reason to debounce
/// it or push it onto a thread.
///
/// The expander itself is reused rather than rebuilt, so a refresh does not
/// snap it shut while the user is reading it.
fn refresh_nearby(ui: &Rc<App>) {
    if !ui.settings.borrow().show_nearby_files {
        for row in ui.nearby_rows.borrow_mut().drain(..) {
            ui.nearby_rows_list.remove(&row);
        }
        ui.nearby_panel.set_visible(false);
        return;
    }

    let (open_dir, extra, off, hidden, drives_on) = {
        let s = ui.settings.borrow();
        (
            s.open_start_dir(),
            s.quick_access_folders.clone(),
            s.quick_access_off.clone(),
            s.quick_access_hidden.clone(),
            s.quick_access_drives_on.clone(),
        )
    };
    // Which places to look is a question about mounted drives, so it is
    // answered here. Reading them is not, and a drive that has gone to sleep
    // can take seconds to answer - long enough that doing it here froze the
    // window, and long enough that a spinning arrow is worth having.
    let sources = nearby::sources(open_dir.as_deref(), &extra, &off, &hidden, &drives_on);
    let queued: Vec<PathBuf> = ui.files.borrow().iter().map(|f| f.path.clone()).collect();
    let limit = ui.settings.borrow().quick_access_limit as usize;

    let generation = ui.scan_gen.get().wrapping_add(1);
    ui.scan_gen.set(generation);
    ui.scan_since.set(Some(std::time::Instant::now()));
    scan_spin(ui, true);

    let (tx, rx) = async_channel::bounded(1);
    let looking = sources.clone();
    std::thread::spawn(move || {
        let _ = tx.send_blocking(nearby::scan(&looking, &queued, limit));
    });

    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let Ok(found) = rx.recv().await else { return };
        // A scan that finishes after a later one has started is stale, and
        // writing its answer over the newer one would undo the change that
        // asked for it.
        if ui.scan_gen.get() != generation {
            return;
        }
        scan_spin(&ui, false);
        show_nearby(&ui, sources, found);
    });
}

/// How long a scan has to run before the arrow starts turning, how long a turn
/// lasts once someone has pressed for it, and how long one revolution takes.
const SCAN_SPIN_AFTER: u128 = 150;
const SCAN_SPIN_LEAST: u128 = 480;
/// Must match the animation in the stylesheet.
const SCAN_SPIN_TURN: u128 = 900;

/// Turn the refresh arrow while a scan is running.
///
/// A scan of one folder is over in a few milliseconds and a spin that brief is
/// a flicker, so an automatic refresh only shows one if it is actually taking
/// time. A press is different: someone who pressed a button is owed an answer
/// whether or not there was anything to do, so that one always turns, and goes
/// on turning until the scan is finished however long that is.
///
/// However long it runs, it stops on a whole revolution. Stopping wherever the
/// scan happened to finish leaves the arrow at some arbitrary angle and then
/// slides it back to rest, which is the one moment in the whole gesture that
/// looks like a mistake.
fn scan_spin(ui: &Rc<App>, on: bool) {
    if on {
        if ui.scan_asked.get() {
            ui.spin_since.set(Some(std::time::Instant::now()));
            ui.nearby_refresh.add_css_class("cz-turning");
            return;
        }
        let ui2 = ui.clone();
        let generation = ui.scan_gen.get();
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(SCAN_SPIN_AFTER as u64),
            move || {
                if ui2.scan_gen.get() == generation && ui2.scan_since.get().is_some() {
                    ui2.spin_since.set(Some(std::time::Instant::now()));
                    ui2.nearby_refresh.add_css_class("cz-turning");
                }
            },
        );
        return;
    }

    let asked = ui.scan_asked.replace(false);
    let ran = ui
        .scan_since
        .replace(None)
        .map(|t| t.elapsed().as_millis())
        .unwrap_or(0);
    let Some(turning_since) = ui.spin_since.get() else {
        // Never started turning: the scan beat the delay.
        ui.nearby_refresh.remove_css_class("cz-turning");
        return;
    };

    // The earliest it is allowed to stop, and then the end of whatever
    // revolution that lands in.
    let least = if asked {
        SCAN_SPIN_LEAST.saturating_sub(ran)
    } else {
        0
    };
    let turned = turning_since.elapsed().as_millis();
    let earliest = turned + least;
    let whole = earliest.div_ceil(SCAN_SPIN_TURN) * SCAN_SPIN_TURN;
    let owed = whole.saturating_sub(turned).min(u128::from(u64::MAX)) as u64;

    let ui2 = ui.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(owed), move || {
        // Only if nothing has started up again in the meantime.
        if ui2.scan_since.get().is_none() {
            ui2.spin_since.set(None);
            ui2.nearby_refresh.remove_css_class("cz-turning");
        }
    });
}

/// Put the result of a scan on screen.
fn show_nearby(ui: &Rc<App>, sources: Vec<nearby::Source>, found: Vec<nearby::Found>) {
    // Read before the rows go, so the animation knows where it is starting
    // from. Mid-flight this is the animated height rather than the content's,
    // which is what makes a second change during the first one continue from
    // where the panel actually is instead of snapping back.
    let was = ui.nearby_clip.height();

    for row in ui.nearby_rows.borrow_mut().drain(..) {
        ui.nearby_rows_list.remove(&row);
    }

    if found.is_empty() {
        // Still shown, collapsed and empty. Hiding it took the folder picker
        // with it, which is precisely the control wanted at the moment there
        // is nothing to offer - including right after the only suggestion has
        // been queued.
        let on: Vec<String> = sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.label.clone())
            .collect();
        let said = if on.is_empty() {
            "Nothing selected to look in".to_string()
        } else {
            format!("Nothing to convert in {}", on.join(", "))
        };
        say_nearby(ui, &said);
        *ui.nearby_subtitle.borrow_mut() = said;

        // An empty list that only says "empty" leaves the user to work out
        // that the fix is behind the folder button. When nothing is selected
        // the row says so and opens the picker; when sources are selected but
        // hold nothing, there is no choice to make, so it only reports.
        let row = if on.is_empty() {
            // No subtitle. The expander directly above already says nothing is
            // selected, and repeating it underneath in longer words reads as
            // the panel labouring the point rather than offering the fix.
            let row = adw::ActionRow::builder()
                .title("Choose a folder or drive to look in")
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
            let ui2 = ui.clone();
            row.connect_activated(move |_| ui2.nearby_sources.popup());
            row
        } else {
            let row = adw::ActionRow::builder()
                .title("No convertible files found")
                .subtitle("Add a folder, or refresh after your slicer writes one")
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
            row
        };
        ui.nearby_rows_list.append(&row);
        ui.nearby_rows.borrow_mut().push(row);
        show_nearby_search(ui, false);
        // Deliberately not collapsed. A refresh happens while the user is
        // working the "Look in" switches, and folding the list under them -
        // then leaving it folded when they switch a source back on - makes
        // the panel feel like it is fighting the controls attached to it.
        // Expansion is the user's state to hold, and only choosing a file
        // hands it back.
        ui.nearby_panel.set_visible(true);
        animate_nearby_height(ui, was);
        return;
    }

    // Naming the places is what makes the collapsed row worth reading: it
    // says where the files are without being opened.
    let mut places: Vec<String> = Vec::new();
    for f in &found {
        if !places.contains(&f.source) {
            places.push(f.source.clone());
        }
    }
    let count = match found.len() {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    };
    let said = format!("{count} in {}", places.join(", "));
    say_nearby(ui, &said);
    *ui.nearby_subtitle.borrow_mut() = said;

    // Searching is offered whenever there is something to search, and the
    // placeholder names the places rather than saying "these files": the one
    // question a search box in a list of found files raises is which folders
    // it is looking through, and it may as well answer it.
    show_nearby_search(ui, true);
    ui.nearby_search.set_placeholder_text(Some(SEARCH_HINT));

    // One size group per column, so the four facts about a file start in the
    // same place down the list instead of each row setting its own margins.
    // Only built when they are going to be used.
    // Narrow, a row is the name and when it arrived. The format, the size and
    // the folder are all things you can find out by opening the file, and the
    // name is the thing being chosen between.
    let narrow = ui.compact.get();
    let in_columns = ui.settings.borrow().quick_access_columns;
    let columns: Vec<gtk::SizeGroup> = (0..if in_columns && !narrow { 4 } else { 1 })
        .map(|_| gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal))
        .collect();
    // Kept past the end of this function, or they are finalised before the
    // rows they are meant to be lining up have been given a size.
    *ui.queue_columns.borrow_mut() = columns.clone();

    let mut keys: Vec<String> = Vec::new();
    for item in found {
        let name = item
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| item.path.display().to_string());
        let age = item
            .modified
            .map(render::human_age)
            .unwrap_or_else(|| "date unknown".to_string());
        let facts: Vec<String> = if narrow {
            vec![render::short_age(
                item.modified.unwrap_or(std::time::UNIX_EPOCH),
            )]
        } else {
            vec![
                item.format.clone(),
                render::human_bytes(item.size),
                item.source.clone(),
                age,
            ]
        };
        keys.push(format!("{name} {}", facts.join(" ")).to_lowercase());

        // The name is carried by a marquee rather than by the row's own title,
        // so a name too long for the row can be read by hovering it. The title
        // is left empty and the marquee added as a prefix, which keeps the
        // row's styling, its padding and its activation while taking over the
        // one part that needed to move.
        let row = if in_columns {
            adw::ActionRow::builder().activatable(true).build()
        } else {
            adw::ActionRow::builder()
                .subtitle(facts.join("  \u{b7}  "))
                .subtitle_lines(1)
                .activatable(true)
                .build()
        };
        // Added in reverse: a prefix goes in ahead of the ones already there,
        // so the marquee first and the icon second leaves the icon leading.
        if in_columns {
            let name = marquee(&name);
            name.set_margin_start(theme::SPACE_2);
            marquee_auto(&name);
            row.add_prefix(&name);
        } else {
            row.set_title(&name);
            row.set_title_lines(1);
        }
        row.add_prefix(&gtk::Image::from_icon_name("document-open-symbolic"));

        if in_columns {
            let meta = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_4);
            meta.set_valign(gtk::Align::Center);
            for (i, fact) in facts.iter().enumerate() {
                let cell = gtk::Label::new(Some(fact));
                cell.add_css_class("caption");
                cell.add_css_class("cz-dim");
                // Sizes read as numbers, so they are set against the right
                // edge of their column; the words against the left of theirs.
                cell.set_xalign(if !narrow && i == 1 { 1.0 } else { 0.0 });
                cell.set_ellipsize(gtk::pango::EllipsizeMode::End);
                columns[i].add_widget(&cell);
                meta.append(&cell);
            }
            row.add_suffix(&meta);
        }
        let ui2 = ui.clone();
        let path = item.path.clone();
        row.connect_activated(move |r| {
            // Choosing is the end of browsing: fold the list away again so the
            // page returns to the queue rather than staying open behind it.
            if let Some(exp) = r.ancestor(adw::ExpanderRow::static_type()) {
                if let Ok(exp) = exp.downcast::<adw::ExpanderRow>() {
                    exp.set_expanded(false);
                }
            }
            // One change at a time. Folding the list and swapping the page in
            // the same frame reads as everything moving at once, which is the
            // clunk; letting the fold finish first makes the two steps legible
            // as cause and effect.
            if !ui2.settings.borrow().animations {
                add_files(&ui2, vec![path.clone()]);
                return;
            }
            let ui3 = ui2.clone();
            let queued = path.clone();
            glib::timeout_add_local_once(
                std::time::Duration::from_millis(EXPANDER_MS),
                move || add_files(&ui3, vec![queued]),
            );
        });
        ui.nearby_rows_list.append(&row);
        ui.nearby_rows.borrow_mut().push(row);
    }
    *ui.nearby_keys.borrow_mut() = keys;
    ui.nearby_panel.set_visible(true);
    // A rebuild makes every row visible again; whatever is in the search box
    // still applies to them.
    apply_nearby_filter(ui);
    animate_nearby_height(ui, was);
}

/// How long the list takes to move between two heights, in seconds.
///
/// A fixed duration rather than a rate. The first attempt eased by a fraction
/// of the distance left each frame, which looks right over fifty pixels and
/// wrong over a thousand: a list of forty files covered most of its travel in
/// the opening frame and read as a snap, while a short list drifted. Holding
/// the duration instead is also what the expander above it does, so opening
/// the list and emptying it now move at the same pace.
const NEARBY_SECONDS: f64 = EXPANDER_MS as f64 / 1000.0;

/// Hand the list's height back to the list, or hold it at a cap and let it
/// scroll inside itself.
///
/// A scrolled window that may scroll can be any height it likes, which is what
/// makes both the clipping and the cap possible, and is also the whole problem
/// when it is doing neither: it will happily take zero, or stay at the height
/// it had while the expander opens underneath it. `Never` makes it measure
/// exactly as the list inside it does, so an uncapped list is not really in a
/// scrolled window at all - it is the list, growing and shrinking with its own
/// contents.
fn settle_nearby(clip: &gtk::ScrolledWindow, cap: i32) {
    // A negative cap means release: the list is free to be as tall as it
    // wants. Zero is not release, it is a cap of nothing - the shut state -
    // and confusing the two is what made shutting the list animate down to
    // nothing and then spring straight back to full height.
    if cap < 0 {
        clip.set_vscrollbar_policy(gtk::PolicyType::Never);
        clip.set_min_content_height(-1);
        clip.set_max_content_height(-1);
        return;
    }
    clip.set_vscrollbar_policy(if cap == 0 {
        gtk::PolicyType::External
    } else {
        gtk::PolicyType::Automatic
    });
    pin_nearby(clip, cap);
}

/// Pin the scroller to exactly one height.
///
/// Cleared first, and the maximum set before the minimum. GTK quietly lowers a
/// minimum that is asked to exceed the current maximum, so raising the pin -
/// from nothing, which is every time the list opens - set the minimum back to
/// what it already was and left the list allocated at the height it had. The
/// symptom was a list that opened one row tall.
fn pin_nearby(clip: &gtk::ScrolledWindow, height: i32) {
    clip.set_min_content_height(-1);
    clip.set_max_content_height(-1);
    clip.set_max_content_height(height);
    clip.set_min_content_height(height);
}

/// Where the file list should come to rest: its height, and the cap if any.
///
/// Only the rows are measured now. The header sits outside the scrolled
/// window so it can stay put while the files move under it, which also means
/// it is no longer part of the height being animated - and the awkward part
/// of the old version, deriving the header's height while it was in the
/// middle of an animation, went with it.
fn nearby_rest(ui: &Rc<App>, for_width: i32) -> (i32, i32) {
    if !ui.nearby_expander.is_expanded() {
        return (0, 0);
    }
    let on = ui.nearby_shown.borrow();
    let limit = ui.settings.borrow().quick_access_visible as usize;
    // Only the rows the filter is letting through. A search that hides four of
    // seven files should shrink the list, not hold room for them.
    let height: i32 = on
        .iter()
        .take(limit)
        .map(|r| r.measure(gtk::Orientation::Vertical, for_width).1)
        .sum();
    if on.len() > limit {
        (height, height)
    } else {
        (height, -1)
    }
}

/// Offer searching, or take it away.
///
/// Shut, it is the magnifying glass; open, the box itself. Taking it away
/// clears whatever was typed, so a list that comes back later comes back
/// whole rather than still filtered by something invisible.
fn show_nearby_search(ui: &Rc<App>, offer: bool) {
    ui.nearby_search_shown.set(offer);
    if !offer {
        ui.nearby_search.set_text("");
    }
    open_nearby_search(ui, offer && ui.nearby_expander.is_expanded());
}

/// How long the field takes to open or shut, in seconds, and the share of that
/// spent fading the box in before it starts to widen.
const SEARCH_SECONDS: f64 = 0.28;
const SEARCH_FADE: f64 = 0.34;
/// The width the box shrinks to before it fades, and grows from. Enough for
/// the glass and an ellipsis and no more: the field leaves as something
/// recognisable, without stopping on its way out to show the first letter of
/// a word nobody is reading.
const SEARCH_SEED: i32 = 38;
/// What the field says when it is empty. One word: the row above it already
/// names the folders being looked through, and repeating them inside the box
/// made a long line that had to be ellipsized to fit anyway.
const SEARCH_HINT: &str = "Search";
/// Below this width the placeholder is taken away rather than left to
/// ellipsize. Font metrics decide how much of a word fits in a given number of
/// pixels, and the answer is not the same on every machine; taking the text
/// out is the only way to be sure the field goes out as the glass and nothing
/// else, wherever it is running.
const SEARCH_HINT_MIN: i32 = 96;
/// And what it grows to. A number rather than the field's own natural width,
/// which is nothing now that it has no character width to claim one from.
const SEARCH_WIDTH: i32 = 190;
/// And what it grows to when the window has no width to spare.
const SEARCH_WIDTH_NARROW: i32 = 104;

/// Open or shut the search field.
///
/// The box fades in first, at a width of almost nothing, and then widens.
/// Deliberately a real width rather than a revealer sliding it into view: a
/// revealer clips its child, so the rounded corner on the leading edge would
/// be sheared off for the whole of the animation. Growing the widget itself
/// means the corners are drawn, correctly, in every frame.
fn open_nearby_search(ui: &Rc<App>, open: bool) {
    let entry = ui.nearby_search.clone();
    ui.search_open.set(open);

    // Mapping is deliberately not the test. The field is hidden while shut,
    // so it is never mapped at the moment it is asked to open, and checking
    // for it meant every opening snapped instead of animating. Being in a
    // window at all is the real question.
    ui.search_full.set(if ui.compact.get() {
        SEARCH_WIDTH_NARROW
    } else {
        SEARCH_WIDTH
    });
    let full = ui.search_full.get();

    let snap = !ui.settings.borrow().animations || entry.root().is_none();
    if snap {
        ui.search_t.set(if open { 1.0 } else { 0.0 });
        entry.set_size_request(if open { full } else { SEARCH_SEED }, -1);
        entry.set_opacity(1.0);
        entry.set_visible(open);
        return;
    }
    if open {
        entry.set_visible(true);
    }
    if ui.search_moving.replace(true) {
        return;
    }
    let Some(root) = entry.root() else {
        ui.search_moving.set(false);
        return;
    };
    let root: gtk::Widget = root.upcast();
    let t = ui.search_t.clone();
    let want = ui.search_open.clone();
    let moving = ui.search_moving.clone();
    let full = ui.search_full.clone();
    let last = Cell::new(None::<i64>);
    root.add_tick_callback(move |_, clock| {
        let now = clock.frame_time();
        let dt = match last.replace(Some(now)) {
            Some(previous) => ((now - previous) as f64 / 1e6).clamp(0.0, 0.1),
            None => 0.0,
        };
        let opening = want.get();
        let step = dt / SEARCH_SECONDS;
        let at = (t.get() + if opening { step } else { -step }).clamp(0.0, 1.0);
        t.set(at);

        // The box arrives before it grows, and leaves after it has shrunk.
        entry.set_opacity((at / SEARCH_FADE).clamp(0.0, 1.0));
        let grown = ((at - SEARCH_FADE) / (1.0 - SEARCH_FADE)).clamp(0.0, 1.0);
        // Eased in the direction of travel. One curve run backwards is the
        // other curve: opening slowed as it arrived, but shutting then sped up
        // into the close, which is the opposite of what it should feel like.
        // Both now leave quickly and arrive slowly.
        let eased = if opening {
            1.0 - (1.0 - grown).powi(3)
        } else {
            grown.powi(3)
        };
        let wide = SEARCH_SEED + ((full.get() - SEARCH_SEED) as f64 * eased).round() as i32;
        entry.set_size_request(wide, -1);
        // On the way out the word goes first and stays gone, so the whole
        // shrink is the glass travelling rather than a letter being squeezed
        // out of a shortening box. On the way in it comes back as soon as
        // there is room for it, so the box fills as it grows.
        if !opening || wide < SEARCH_HINT_MIN {
            entry.set_placeholder_text(None);
        } else {
            entry.set_placeholder_text(Some(SEARCH_HINT));
        }

        if (opening && at >= 1.0) || (!opening && at <= 0.0) {
            if opening {
                entry.set_placeholder_text(Some(SEARCH_HINT));
                entry.set_opacity(1.0);
            } else {
                entry.set_visible(false);
            }
            moving.set(false);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

/// What the header says under "Quick Access", or nothing when the window is
/// too narrow to say it.
///
/// Narrow, the title and the three controls beside it are the whole of the
/// room there is. The count and the places are the first thing to go: they are
/// a convenience for reading the list without opening it, and opening it says
/// the same thing better.
fn say_nearby(ui: &Rc<App>, text: &str) {
    ui.nearby_expander
        .set_subtitle(if ui.compact.get() { "" } else { text });
}

/// Show only the rows matching what has been typed, and say so.
///
/// Filtering never rescans. The rows are already built and the question is
/// which of them the user wants to look at, so this is a visibility pass and
/// a subtitle, nothing more.
fn apply_nearby_filter(ui: &Rc<App>) {
    let typed = ui.nearby_search.text().trim().to_string();
    let needle = typed.to_lowercase();
    let rows = ui.nearby_rows.borrow();
    let keys = ui.nearby_keys.borrow();
    let total = rows.len();
    let mut shown: Vec<adw::ActionRow> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        // Matched against the name and all four facts, because the format or
        // the drive a file came from is a perfectly good way to say what you
        // are after. They are no longer text on the row, so they are kept
        // beside it for exactly this.
        let hit = needle.is_empty()
            || keys.get(i).is_some_and(|k| k.contains(&needle))
            || row.title().to_lowercase().contains(&needle);
        row.set_visible(hit);
        if hit {
            shown.push(row.clone());
        }
    }
    let hits = shown.len();
    *ui.nearby_shown.borrow_mut() = shown;
    drop(keys);
    drop(rows);

    let said = if typed.is_empty() {
        ui.nearby_subtitle.borrow().clone()
    } else if hits == 0 {
        format!("Nothing here matches \u{201c}{typed}\u{201d}")
    } else {
        format!("{hits} of {total} match \u{201c}{typed}\u{201d}")
    };
    say_nearby(ui, &said);
}

/// Hold the list at one height, whatever its contents now measure.
fn hold_nearby(clip: &gtk::ScrolledWindow, height: i32) {
    clip.set_vscrollbar_policy(gtk::PolicyType::External);
    pin_nearby(clip, height);
}

/// Walk the Quick Access list from the height it had to the height it wants.
///
/// Switching a source off takes its rows out on the next frame and everything
/// below jumps up to meet the gap. Nothing in GTK animates that: a list box is
/// exactly as tall as its rows. So the list is held inside a scrolled window,
/// which is allowed to be a height its child is not, and that height is walked
/// from the old value to the new one and then released.
fn animate_nearby_height(ui: &Rc<App>, from: i32) {
    let clip = ui.nearby_clip.clone();
    if clip.child().is_none() {
        return;
    }

    let width = clip.width();
    let for_width = if width > 0 { width } else { -1 };
    let (to, cap) = nearby_rest(ui, for_width);
    ui.nearby_cap.set(cap);
    if to > 0 {
        ui.nearby_rows_list.set_visible(true);
    }

    if ui.nearby_moving.get() {
        // Already walking. Aim it somewhere else from wherever it has got to,
        // rather than starting again from where it set off.
        if ui.nearby_target.get() != to as f64 {
            ui.nearby_from.set(ui.nearby_h.get());
            ui.nearby_target.set(to as f64);
            ui.nearby_elapsed.set(0.0);
        }
        return;
    }
    // Note `from` is not tested for zero. Opening from nothing is what every
    // opening looks like now that the list is not inside the expander's own
    // revealer, and treating it as "not laid out yet" made every opening
    // teleport to full height.
    if from == to || !clip.is_mapped() || !ui.settings.borrow().animations {
        settle_nearby(&clip, cap);
        if to == 0 {
            ui.nearby_head_list.remove_css_class("cz-qa-open");
            ui.nearby_rows_list.set_visible(false);
        }
        return;
    }
    let Some(root) = clip.root() else {
        settle_nearby(&clip, cap);
        if to == 0 {
            ui.nearby_head_list.remove_css_class("cz-qa-open");
            ui.nearby_rows_list.set_visible(false);
        }
        return;
    };

    let generation = ui.nearby_gen.get().wrapping_add(1);
    ui.nearby_gen.set(generation);
    ui.nearby_from.set(from as f64);
    ui.nearby_h.set(from as f64);
    ui.nearby_target.set(to as f64);
    ui.nearby_elapsed.set(0.0);
    hold_nearby(&clip, from);
    ui.nearby_moving.set(true);

    // A tick callback only runs while the frame clock does, and a clock that
    // is throttled - an occluded window, a session that has stopped drawing -
    // takes the animation with it and leaves `moving` set for good. Every
    // later change then does nothing but move a target nothing is chasing,
    // and the list sits at whatever height it had reached: shut, refusing to
    // open, with the refresh button apparently doing nothing. This is the
    // floor under that. It only acts on its own animation, and only if that
    // one is somehow still running long after it should have finished.
    {
        let ui = ui.clone();
        glib::timeout_add_local_once(
            std::time::Duration::from_millis((NEARBY_SECONDS * 4000.0) as u64),
            move || {
                if !ui.nearby_moving.get() || ui.nearby_gen.get() != generation {
                    return;
                }
                ui.nearby_moving.set(false);
                let width = ui.nearby_clip.width();
                let (to, cap) = nearby_rest(&ui, if width > 0 { width } else { -1 });
                settle_nearby(&ui.nearby_clip, cap);
                if to == 0 {
                    ui.nearby_head_list.remove_css_class("cz-qa-open");
                    ui.nearby_rows_list.set_visible(false);
                }
            },
        );
    }

    let root: gtk::Widget = root.upcast();
    let start = ui.nearby_from.clone();
    let target = ui.nearby_target.clone();
    let height = ui.nearby_h.clone();
    let elapsed = ui.nearby_elapsed.clone();
    let moving = ui.nearby_moving.clone();
    let rest = ui.nearby_cap.clone();
    let head = ui.nearby_head_list.clone();
    let body = ui.nearby_rows_list.clone();
    let last = Cell::new(None::<i64>);
    root.add_tick_callback(move |_, clock| {
        let now = clock.frame_time();
        // A long gap means the clock was stopped, not that a long step is
        // owed; clamped so coming back to the window does not teleport it.
        let dt = match last.replace(Some(now)) {
            Some(previous) => ((now - previous) as f64 / 1e6).clamp(0.0, 0.1),
            None => 0.0,
        };
        elapsed.set(elapsed.get() + dt);

        let t = (elapsed.get() / NEARBY_SECONDS).clamp(0.0, 1.0);
        // Ease out: quick to leave, slow to arrive, which is the shape every
        // other movement in the window uses.
        let eased = 1.0 - (1.0 - t).powi(3);
        let at = start.get() + (target.get() - start.get()) * eased;
        height.set(at);

        if t >= 1.0 {
            settle_nearby(&clip, rest.get());
            if target.get() <= 0.0 {
                // Fully shut at last: the header may have its corners back,
                // and the list stops being drawn entirely. At zero height it
                // still painted its own border, which read as a stripe of a
                // slightly different grey tucked under the header.
                head.remove_css_class("cz-qa-open");
                body.set_visible(false);
            }
            moving.set(false);
            return glib::ControlFlow::Break;
        }
        // Once what is left of the list is shorter than the corner itself,
        // there is nothing left for a curve to cut through - so the rounding
        // starts here rather than after the fold has finished. Waiting until
        // the end left a gap between the list going and the corners moving,
        // which read as a hesitation.
        if target.get() <= 0.0 && at < CORNER_RADIUS {
            head.remove_css_class("cz-qa-open");
        }
        hold_nearby(&clip, at.round() as i32);
        glib::ControlFlow::Continue
    });
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
            selected: true,
            edited: std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok()),
            changed_since: false,
        });
        added += 1;
        read_in_background(ui, path, None);
    }
    if added > 0 {
        refresh_queue(ui);
        // A queued file should stop being offered as a suggestion.
        refresh_nearby(ui);
        // Morph rather than jump: the drop zone gives way to the queue (§24).
        swap_page(ui, true);
        if ui.files.borrow().len() == added {
            select_file(ui, 0);
        }
    }
}

/// Move between the two faces of the Convert page.
///
/// The page has two states - an invitation to open something, and a queue with
/// a form under it - and they used to change over in a single frame. That is
/// most of the page rearranging at once, with nothing to tell the eye which
/// part to follow.
///
/// It resolves in one direction now, one thing after another. Filling: the
/// drop zone shrinks into the queue where it stands, and once that has settled
/// the form swings down underneath it - so the movement runs top to bottom and
/// ends where the next thing to do is. Emptying is the same sequence
/// backwards: the form folds away first, and only then does the queue give the
/// space back. Neither direction has two things moving at once.
fn swap_page(ui: &Rc<App>, to_queue: bool) {
    let animate = ui.settings.borrow().animations;
    if to_queue {
        ui.page_faces.set_visible_child_name("queue");
        ui.controls.set_visible(true);
        if !animate {
            ui.controls_reveal.set_reveal_child(true);
            return;
        }
        let ui2 = ui.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(STAGGER_MS), move || {
            // Checked again on arrival: the file may have been taken back
            // out while the drop zone was still shrinking.
            if !ui2.files.borrow().is_empty() {
                ui2.controls_reveal.set_reveal_child(true);
            }
        });
        return;
    }

    ui.controls_reveal.set_reveal_child(false);
    if !animate {
        ui.page_faces.set_visible_child_name("drop");
        ui.controls.set_visible(false);
        return;
    }
    let ui2 = ui.clone();
    glib::timeout_add_local_once(
        std::time::Duration::from_millis(CONTROLS_MS as u64),
        move || {
            if ui2.files.borrow().is_empty() {
                ui2.page_faces.set_visible_child_name("drop");
                ui2.controls.set_visible(false);
            }
        },
    );
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
/// "GOO (Detect Automatically)" answers both questions at once, where a bare
/// "GOO" left the menu's tick on "Detect Automatically" looking like it
/// disagreed with the field. It stays at every width: the label is ellipsized
/// and asks for three characters, so the suffix costs nothing it can be
/// squeezed out of, and there is room for it even at the narrowest.
///
/// The same words as the destination's, deliberately. Two names for one idea
/// reads as two ideas.
fn refresh_input_label(ui: &Rc<App>) {
    let files = ui.files.borrow();
    let text = match files.get(*ui.selected.borrow()) {
        // What it has been set to, ahead of what was last read successfully.
        // A forced format that the file turns out not to be leaves the old
        // detected one sitting in `format`, and showing that made the control
        // look like it had ignored the instruction - the swap button appeared
        // to leave the input untouched when it had in fact changed it.
        Some(f) if f.forced_format.is_some() => {
            f.forced_format.clone().unwrap_or_default().to_uppercase()
        }
        Some(f) if !f.format.is_empty() => {
            format!("{} (Detect Automatically)", f.format.to_uppercase())
        }
        // A file that has not been read yet, or could not be. It still has a
        // setting, and "—" alone said nothing about what that setting was -
        // the row looked the same whether the format was being worked out or
        // had been chosen by hand.
        Some(f) if f.forced_format.is_none() => "Detect Automatically".to_string(),
        _ => "—".to_string(),
    };
    ui.input_label.set_text(&text);
}

/// How fast a name slides past, in pixels per second, and the gap left between
/// the end of one pass and the start of the next.
const MARQUEE_SPEED: f64 = 46.0;
const MARQUEE_GAP: i32 = 48;
/// How often a name is re-asked whether it still fits.
const MARQUEE_CHECK: std::time::Duration = std::time::Duration::from_millis(500);

/// A name that slides itself past a fixed width while the pointer is over it.
///
/// Only when it has to. A name that fits does not move, because movement
/// carrying no information is noise; it is the crushed ones - and only while
/// they are being looked at - that have something left to say.
///
/// At rest it is an ordinary ellipsized label, because the ellipsis is what
/// says the name is longer than the space. On hover the ellipsis goes, a
/// second copy of the text appears behind the first, and the pair slides by
/// exactly one copy's width before starting again - so the loop has no rewind
/// in it, and a name can be read round and round without waiting.
fn marquee(text: &str) -> gtk::ScrolledWindow {
    let first = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .build();
    let second = gtk::Label::builder().label(text).xalign(0.0).build();
    second.set_visible(false);

    let train = gtk::Box::new(gtk::Orientation::Horizontal, MARQUEE_GAP);
    train.append(&first);
    train.append(&second);

    let view = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .child(&train)
        .build();
    view.set_overflow(gtk::Overflow::Hidden);
    // The position here belongs to the marquee. Selecting the row would
    // otherwise scroll the name to its end and leave it there.
    shell::dont_chase_focus(&view);
    view
}

/// The pair of labels inside a marquee.
///
/// A GtkScrolledWindow given a child that cannot scroll itself puts a viewport
/// in between, and `child()` hands back that viewport rather than what was put
/// in. Reaching straight for the box therefore found nothing and every marquee
/// quietly did nothing at all - names were ellipsised and stayed that way.
fn marquee_train(view: &gtk::ScrolledWindow) -> Option<gtk::Box> {
    match view.child() {
        Some(child) => match child.downcast::<gtk::Box>() {
            Ok(train) => Some(train),
            Err(other) => other
                .downcast::<gtk::Viewport>()
                .ok()
                .and_then(|port| port.child())
                .and_then(|inner| inner.downcast::<gtk::Box>().ok()),
        },
        None => None,
    }
}

/// Slide the name in `view` whenever it is too long for the room it has.
///
/// Not on hover, and not only when the window is narrow. A name gets cut short
/// at whatever width the row happens to have, the widest included, and a row
/// that will only tell you what it says once you have found it and pointed at
/// it has hidden the answer behind a gesture. If it does not fit, it moves.
///
/// Only if it does not fit: a name with room to spare sits still, because
/// movement carrying no information is noise.
///
/// Whether it fits is asked a couple of times a second rather than once. A
/// name that fits at one window width does not at another, and these rows are
/// rebuilt when the layout changes bands, not for every pixel of a drag.
fn marquee_auto(view: &gtk::ScrolledWindow) {
    let Some(train) = marquee_train(view) else {
        return;
    };
    let Some(first) = train.first_child().and_downcast::<gtk::Label>() else {
        return;
    };
    let Some(second) = train.last_child().and_downcast::<gtk::Label>() else {
        return;
    };

    let running = Rc::new(Cell::new(false));
    let stop = {
        let view = view.clone();
        let first = first.clone();
        let second = second.clone();
        let running = running.clone();
        move || {
            if !running.replace(false) {
                return;
            }
            first.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            second.set_visible(false);
            view.hadjustment().set_value(0.0);
        }
    };
    let start = {
        let view = view.clone();
        let first = first.clone();
        let second = second.clone();
        let running = running.clone();
        move || {
            if running.replace(true) {
                return;
            }
            // The ellipsis goes, a second copy appears behind the first, and
            // the pair slides by exactly one copy's width before starting
            // again - so the loop has no rewind in it and the name can be read
            // round and round without waiting.
            first.set_ellipsize(gtk::pango::EllipsizeMode::None);
            second.set_visible(true);

            let adj = view.hadjustment();
            let lead = first.clone();
            let running = running.clone();
            let first = first.clone();
            let second = second.clone();
            let view2 = view.clone();
            let last = Cell::new(None::<i64>);
            view.add_tick_callback(move |w, clock| {
                if !running.get() {
                    first.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                    second.set_visible(false);
                    view2.hadjustment().set_value(0.0);
                    return glib::ControlFlow::Break;
                }
                let now = clock.frame_time();
                let dt = match last.replace(Some(now)) {
                    Some(previous) => ((now - previous) as f64 / 1e6).clamp(0.0, 0.1),
                    None => 0.0,
                };
                // One copy plus the gap: at that point the second copy is
                // exactly where the first started, so resetting is invisible.
                let lap = (lead.width() + MARQUEE_GAP) as f64;
                if lap <= 1.0 || w.width() == 0 {
                    return glib::ControlFlow::Continue;
                }
                let next = adj.value() + MARQUEE_SPEED * dt;
                adj.set_value(if next >= lap { next - lap } else { next });
                glib::ControlFlow::Continue
            });
        }
    };

    // A weak handle, so the row being taken out of the list is what ends this
    // rather than the timer being what keeps the row alive.
    let weak = view.downgrade();
    let lead = first.clone();
    glib::timeout_add_local(MARQUEE_CHECK, move || {
        let Some(view) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let width = view.width();
        // Off screen, or not laid out yet. Nothing to measure against, and
        // sliding a row nobody is looking at is work for its own sake.
        if width == 0 || !view.is_mapped() {
            stop();
            return glib::ControlFlow::Continue;
        }
        // The natural width is the whole name; ellipsising only lowers what
        // the label will settle for, not what it wants.
        if lead.measure(gtk::Orientation::Horizontal, -1).1 > width {
            start();
        } else {
            stop();
        }
        glib::ControlFlow::Continue
    });
}

/// The list of formats the selected file can be read as (§21).
///
/// "Detect Automatically" first, then every format that can read, so the
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
    // Automatically", and without this the two look like they disagree.
    let detected = registry::by_id(&reading_as)
        .map(|h| h.info().name)
        .filter(|_| current.is_none());
    let mut entries: Vec<(Option<String>, String, String)> = vec![(
        None,
        "Detect Automatically".into(),
        match detected {
            Some(name) => format!("Reading it as {name}"),
            None => "Read the contents and work it out".into(),
        },
    )];
    // Alphabetical. Registry order is whatever order the handlers happen to be
    // declared in, which is a fact about the source and not about the list -
    // and a list nobody can predict the order of has to be read end to end
    // every time.
    let hidden = ui.settings.borrow().hidden_input_formats.clone();
    let mut readable: Vec<_> = registry::readable()
        .into_iter()
        .filter(|i| !hidden.iter().any(|h| h == i.id))
        .collect();
    readable.sort_by_key(|i| i.name.to_lowercase());
    for info in readable {
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

/// The format the Input control is set to.
///
/// What was asked for if anything was, and otherwise what detection found.
/// The two can disagree - a forced format the file turns out not to be leaves
/// the last detected one in place - and anything reading the control has to
/// read the same one the control is showing, or it ends up arguing with what
/// is on screen.
fn input_format(ui: &Rc<App>) -> Option<String> {
    let files = ui.files.borrow();
    let f = files.get(*ui.selected.borrow())?;
    f.forced_format
        .clone()
        .or_else(|| (!f.format.is_empty()).then(|| f.format.clone()))
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
    // The headline first, then the particulars underneath. What comes back is
    // shown on hover, where the first line is all anyone reads, and again in
    // the panel behind it, where the rest is what they came for.
    let explain = |e: cheapazsla_core::Error| -> ReadFailure {
        (
            format!("{}\n{e}", e.headline()),
            remedy::for_error(&e, &facts),
        )
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

        // Ticked by default, and the only thing it decides is whether this
        // file goes when Convert is pressed. It comes off by itself once the
        // file is done, so a second press does not redo finished work.
        let tick = gtk::CheckButton::new();
        tick.set_valign(gtk::Align::Center);
        tick.set_active(f.selected);
        tick.set_tooltip_text(Some("Convert this one"));
        {
            let ui = ui.clone();
            let path = f.path.clone();
            tick.connect_toggled(move |t| {
                let on = t.is_active();
                if let Some(f) = ui.files.borrow_mut().iter_mut().find(|f| f.path == path) {
                    if f.selected == on {
                        return;
                    }
                    f.selected = on;
                }
                revalidate(&ui);
            });
        }
        row.append(&tick);

        row.append(&gtk::Image::from_icon_name("text-x-generic-symbolic"));

        let name = marquee(&f.name());
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

        // Full technical text behind the status itself, as §28 asks. It used
        // to be behind a Details button next to it, which is a second control
        // saying the same thing as the first: "Failed" is already the thing
        // you want to know more about, so it is the thing to press.
        let chip = if f.changed_since {
            shell::status_chip("document-edit-symbolic", "Edited", "cz-warn").upcast()
        } else {
            f.status.chip()
        };
        let detail = match f.status {
            Status::Failed(_) | Status::Warning(_) => f.status.detail(),
            _ => None,
        };
        let width = if ui.compact.get() { 0 } else { 104 };
        match detail {
            Some(detail) => {
                let failed = matches!(f.status, Status::Failed(_));
                let press = gtk::Button::builder().child(&chip).build();
                press.add_css_class("flat");
                press.add_css_class("cz-chip-button");
                press.set_valign(gtk::Align::Center);
                press.set_width_request(width);
                // The reason, on the status itself rather than on every
                // widget in the row. It used to be on all of them, which meant
                // a tooltip came up over the file's name as well - in the way
                // of the one thing the row is there to show.
                //
                // The first line only. That is the plain-words headline; the
                // particulars behind it are what the panel is for, and for
                // some failures the two would otherwise say the same thing
                // twice in a row.
                press.set_tooltip_text(Some(detail.lines().next().unwrap_or(&detail)));
                press.set_cursor_from_name(Some("pointer"));
                let win = ui.window.clone();
                let heading = if failed {
                    "This file could not be opened"
                } else {
                    "Worth knowing about this file"
                };
                let name = f.name();
                let suggestions = f.suggestions.clone();
                press.connect_clicked(move |_| {
                    show_details(&win, heading, &name, &detail, &suggestions);
                });
                row.append(&press);
            }
            None => {
                chip.set_width_request(width);
                row.append(&chip);
            }
        }

        let remove = shell::icon_button("window-close-symbolic", "Remove from list");
        let ui2 = ui.clone();
        let path = f.path.clone();
        remove.connect_clicked(move |_| remove_file(&ui2, &path));
        row.append(&remove);

        marquee_auto(&name);
        let list_row = gtk::ListBoxRow::builder().child(&row).build();
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

/// Take every file out of the list.
///
/// The files are untouched on disk - this is a list, not a folder - so it asks
/// nothing before doing it. What it does say is how many went, because a
/// button that empties a hundred rows without a word is indistinguishable from
/// one that has gone wrong.
fn clear_files(ui: &Rc<App>) {
    let count = ui.files.borrow().len();
    if count == 0 {
        return;
    }
    ui.files.borrow_mut().clear();
    queue_emptied(ui);
    refresh_nearby(ui);
    ui.toasts.add_toast(adw::Toast::new(&match count {
        1 => "1 file taken off the list".to_string(),
        n => format!("{n} files taken off the list"),
    }));
}

/// Put the page back to its empty state.
fn queue_emptied(ui: &Rc<App>) {
    stop_play(ui);
    *ui.selected.borrow_mut() = 0;
    swap_page(ui, false);
    ui.preview_stack.set_visible_child_name("empty");
    // Drop the texture as well, so the last layer of a removed file is not
    // still sitting in memory waiting to reappear.
    ui.viewer.clear();
    ui.viewer.fit();
    refresh_queue(ui);
}

fn remove_file(ui: &Rc<App>, path: &Path) {
    ui.files.borrow_mut().retain(|f| f.path != path);
    let len = ui.files.borrow().len();
    if len == 0 {
        queue_emptied(ui);
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
    /// Follow whichever removable drive is connected.
    AutoDrive,
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

            // Ordered by how often each is the answer, not by kind. Following
            // the drive is the standing instruction someone sets once; beside
            // the original is the other standing answer; recents are for
            // coming back to a particular job, and there is normally one worth
            // coming back to. Everything else is a decision, and decisions go
            // at the bottom.
            //
            // Offered whether or not a drive is attached: choosing it is a
            // statement about where output should go from now on, which is
            // useful to make before plugging the drive in.
            add(
                "drive-removable-media-symbolic",
                "Connected drive (Detect Automatically)".into(),
                Some(match auto_drive(&ui2) {
                    Some(d) => format!(
                        "{} · {}",
                        d.name,
                        drives::space(&d.path)
                            .map(|(free, _)| format!("{} available", render::human_bytes(free)))
                            .unwrap_or_else(|| d.path.display().to_string())
                    ),
                    None => "Uses the drive you plug in next".into(),
                }),
                Destination::AutoDrive,
            );

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
                Some(Destination::AutoDrive) => set_out_auto_drive(&ui3),
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

/// The drive "connected drive" currently means.
///
/// The most recently mounted removable drive if it is still there, otherwise
/// whichever removable drive is attached. Falling back matters on startup,
/// when a drive plugged in before launch produced no mount event to remember.
fn auto_drive(ui: &Rc<App>) -> Option<drives::Drive> {
    let remembered = ui.last_drive.borrow().clone();
    if let Some(name) = remembered {
        if let Some(d) = drives::by_name(&name) {
            if d.removable {
                return Some(d);
            }
        }
    }
    drives::mounted().into_iter().find(|d| d.removable)
}

/// Point the output at whichever drive is connected, now and as that changes.
fn set_out_auto_drive(ui: &Rc<App>) {
    let sub = ui.settings.borrow().pinned_subfolder.clone();
    let resolved = auto_drive(ui);
    let dir = resolved
        .as_ref()
        .and_then(|d| drives::target_dir(&d.name, &sub).or_else(|| Some(d.path.clone())));
    set_out_dir(ui, dir);
    // set_out_dir clears the flag, so it goes back on afterwards.
    ui.out_auto_drive.set(true);
    {
        let mut s = ui.settings.borrow_mut();
        if !s.follow_drive {
            s.follow_drive = true;
            let _ = s.save();
        }
    }
    match resolved {
        Some(d) => {
            *ui.dest_base.borrow_mut() = format!("{} (Detect Automatically)", d.name);
        }
        None => {
            *ui.dest_base.borrow_mut() = "Connected drive (Detect Automatically)".to_string();
            *ui.dest_base_detail.borrow_mut() =
                "No drive connected. Plug one in and it will be used.".to_string();
        }
    }
    refresh_dest_label(ui);
    revalidate(ui);
}

/// Put the destination back on screen, saying so if its drive has gone.
///
/// Unplugging a drive deliberately leaves the destination pointing at it: it
/// is usually about to be plugged back in, and losing the choice every time
/// would be worse than keeping it. But saying nothing is worse again - the row
/// reads exactly as it did when the drive was there, and the first sign
/// anything is wrong is a conversion that cannot write.
fn refresh_dest_label(ui: &Rc<App>) {
    let gone = !ui.out_auto_drive.get()
        && ui
            .out_drive
            .borrow()
            .as_deref()
            .is_some_and(|name| drives::by_name(name).is_none());
    if gone {
        ui.dest_label
            .set_text(&format!("{} (Disconnected)", ui.dest_base.borrow()));
        ui.dest_detail
            .set_text("Plug this drive back in, or choose somewhere else");
    } else {
        ui.dest_label.set_text(&ui.dest_base.borrow());
        ui.dest_detail.set_text(&ui.dest_base_detail.borrow());
    }
}

/// A drive that comes back need not come back in the same place.
fn reattach_out_drive(ui: &Rc<App>) {
    if ui.out_auto_drive.get() {
        return;
    }
    let Some(name) = ui.out_drive.borrow().clone() else {
        return;
    };
    if drives::by_name(&name).is_none() {
        return;
    }
    let stale = ui.out_dir.borrow().as_deref().is_some_and(|p| !p.is_dir());
    if !stale {
        return;
    }
    let sub = ui.settings.borrow().pinned_subfolder.clone();
    if let Some(dir) = drives::target_dir(&name, &sub) {
        set_out_dir(ui, Some(dir));
    }
}

/// Re-point the output after a drive came or went, when following one.
fn reresolve_auto_drive(ui: &Rc<App>) {
    if ui.out_auto_drive.get() {
        set_out_auto_drive(ui);
    }
}

fn set_out_dir(ui: &Rc<App>, dir: Option<PathBuf>) {
    let (base, detail) = match &dir {
        Some(d) => {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| d.display().to_string());
            let detail = drives::space(d)
                .map(|(free, _)| {
                    format!(
                        "{}  ·  {} available",
                        d.display(),
                        render::human_bytes(free)
                    )
                })
                .unwrap_or_else(|| d.display().to_string());
            (name, detail)
        }
        None => (
            "Beside the original".to_string(),
            "Same folder as each source file".to_string(),
        ),
    };
    // Noted while the drive is still here, because once it is gone there is
    // nothing left to ask which drive the folder was on.
    *ui.out_drive.borrow_mut() = dir
        .as_deref()
        .and_then(drives::containing)
        .filter(|d| d.removable)
        .map(|d| d.name);
    *ui.dest_base.borrow_mut() = base;
    *ui.dest_base_detail.borrow_mut() = detail;
    // Recorded here, not only after a conversion. Choosing where output goes
    // and then closing the window used to lose the choice entirely, because
    // the only thing that wrote it down was a finished convert - so the answer
    // was always the last place something was written, never the last place
    // that was asked for.
    {
        let mut s = ui.settings.borrow_mut();
        let changed = s.follow_drive || s.last_output_dir != dir;
        s.follow_drive = false;
        s.last_output_dir = dir.clone();
        if changed {
            let _ = s.save();
        }
    }
    *ui.out_dir.borrow_mut() = dir;
    ui.out_auto_drive.set(false);
    refresh_dest_label(ui);
    update_eject_button(ui);
    suggest_name(ui);
    revalidate(ui);
}

/// Show eject beside the destination only when there is something to eject.
fn update_eject_button(ui: &Rc<App>) {
    let protected = ui.settings.borrow().never_eject.clone();
    let drive = ui
        .out_dir
        .borrow()
        .as_deref()
        .and_then(drives::containing)
        .filter(|d| d.removable && drives::is_ejectable(&d.name, &protected));
    match drive {
        Some(d) => {
            ui.eject_btn
                .set_tooltip_text(Some(&format!("Eject {}", d.name)));
            ui.eject_btn.set_visible(true);
        }
        None => ui.eject_btn.set_visible(false),
    }
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
    // The button says which files it means. "Convert 6 Files" when every one is
    // ticked is the same sentence as "Convert All" and shorter to read; the
    // moment some are not, the count is the whole point.
    let (total, picked) = {
        let files = ui.files.borrow();
        (files.len(), files.iter().filter(|f| f.selected).count())
    };
    ui.convert_label.set_text(&match (total, picked) {
        (_, 0) => "Convert".to_string(),
        (1, _) => "Convert".to_string(),
        (t, p) if p == t => "Convert All".to_string(),
        (_, p) => format!("Convert {p} Selected"),
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
    if !files.iter().any(|f| f.selected) {
        return Some("Tick a file to convert it.".into());
    }
    if files
        .iter()
        .filter(|f| f.selected)
        .all(|f| f.opened.is_none())
    {
        return Some("Waiting for the ticked files to be read.".into());
    }
    let format = ui.output_picker.selected()?;

    let typed = ui.name_entry.text();
    if files.len() == 1 && typed.contains('/') {
        return Some("The file name cannot contain a slash.".into());
    }

    // Following a drive with nothing plugged in has no destination at all.
    // Saying so beats falling through to "beside the original", which would
    // put the file somewhere the user did not choose and would not look.
    if ui.out_auto_drive.get() && ui.out_dir.borrow().is_none() {
        return Some(
            "No drive connected. Plug in the drive to save to, or choose another location."
                .to_string(),
        );
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
            if f.opened.is_none() || !f.selected {
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
    // Turning the question off from inside the question itself, which is where
    // anyone who is tired of it actually is. It only counts if they answer:
    // ticking it and then cancelling is not a decision about future files.
    let never = gtk::CheckButton::with_label("Do not ask me again");
    never.set_margin_top(theme::SPACE_2);
    never.set_tooltip_text(Some(
        "Files of the same name will be replaced without asking. \
         Settings can turn the question back on.",
    ));
    d.set_extra_child(Some(&never));

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
        if never.is_active() {
            {
                let mut s = ui2.settings.borrow_mut();
                s.confirm_overwrite = false;
                let _ = s.save();
            }
            // The switch in Settings is put in step here rather than left to
            // find out later: a setting that disagrees with what the program
            // is doing is worse than no setting at all.
            if let Some(row) = ui2.overwrite_switch.borrow().as_ref() {
                row.set_active(false);
            }
            ui2.toasts.add_toast(adw::Toast::new(
                "Files of the same name will be replaced from now on. Settings can undo this.",
            ));
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
                    let done = matches!(f.status, Status::Complete(_))
                        || matches!(entry_status, Status::Complete(_));
                    f.status = entry_status;
                    // A finished file unticks itself. Converting six and then
                    // adding a seventh should convert the seventh, not all
                    // seven again - and the tick is the thing that says so.
                    // A failure stays ticked, because it has not been done.
                    if done {
                        f.selected = false;
                        f.changed_since = false;
                        // Noted as of now, so a later edit is measured against
                        // the version that was actually converted.
                        f.edited = std::fs::metadata(&f.path)
                            .ok()
                            .and_then(|m| m.modified().ok());
                    }
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
                    automatic: false,
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
    ui.history_ticks.borrow_mut().clear();

    // Built before the rows so their tick boxes can tell it how many are
    // ticked, and added after them so it sits at the foot of the list.
    let selected = gtk::Button::builder().halign(gtk::Align::Start).build();
    selected.add_css_class("flat");
    selected.add_css_class("cz-destructive");
    selected.set_visible(false);

    for (i, e) in entries.iter().enumerate() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
        row.set_margin_top(theme::SPACE_2);
        row.set_margin_bottom(theme::SPACE_2);
        row.set_margin_start(theme::SPACE_3);
        row.set_margin_end(theme::SPACE_2);

        // The tick comes first, where a list of things to be picked from puts
        // it, and does nothing on its own - it only tells the button at the
        // foot of the list what it would be removing.
        let tick = gtk::CheckButton::new();
        tick.set_valign(gtk::Align::Center);
        tick.set_tooltip_text(Some("Pick this one to remove"));
        {
            let ui = ui.clone();
            let selected = selected.clone();
            tick.connect_toggled(move |_| {
                let ticked = ui
                    .history_ticks
                    .borrow()
                    .iter()
                    .filter(|t| t.is_active())
                    .count();
                selected.set_visible(ticked > 0);
                selected.set_child(Some(&labelled_icon(
                    "user-trash-symbolic",
                    &match ticked {
                        1 => "Remove 1 Selected".to_string(),
                        n => format!("Remove {n} Selected"),
                    },
                )));
            });
        }
        row.append(&tick);
        ui.history_ticks.borrow_mut().push(tick);

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

        // Whose doing it was. Every other row here is something the reader
        // pressed a button for and can be expected to remember; a WatchDog row
        // may have happened while they were in another room.
        if e.automatic {
            let by = shell::status_chip(WATCHDOG_ICON, "WatchDog", "cz-dim");
            by.set_tooltip_text(Some("Converted automatically by WatchDog"));
            row.append(&by);
        }

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

    {
        let ui2 = ui.clone();
        selected.connect_clicked(move |_| {
            // Highest index first: removing from the front would shift every
            // row after it and take the wrong ones out.
            let mut going: Vec<usize> = ui2
                .history_ticks
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, t)| t.is_active())
                .map(|(i, _)| i)
                .collect();
            going.sort_unstable_by(|a, b| b.cmp(a));
            let count = going.len();
            {
                let mut history = ui2.history.borrow_mut();
                for i in going {
                    history.remove(i);
                }
            }
            refresh_history(&ui2);
            ui2.toasts.add_toast(adw::Toast::new(&match count {
                1 => "1 entry removed".to_string(),
                n => format!("{n} entries removed"),
            }));
        });
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

    let foot = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_2);
    foot.append(&selected);
    foot.append(&clear);
    ui.history_list.append(
        &gtk::ListBoxRow::builder()
            .child(&foot)
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

/// A settings section that starts closed.
///
/// The page had grown to seven headings of switches, all open, all the time -
/// which is a lot to scroll past to reach the one thing being looked for, and
/// no help at all in finding it. Closed, the page is its own table of
/// contents; a heading is a small enough thing to read that a reader can pick
/// the one they want without reading anything else.
fn settings_section(
    found: &mut Vec<SettingsSection>,
    title: &str,
    subtitle: &str,
    keywords: &str,
) -> adw::ExpanderRow {
    let group = adw::PreferencesGroup::new();
    let row = adw::ExpanderRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    group.add(&row);
    found.push(SettingsSection {
        group,
        row: row.clone(),
        terms: format!("{title} {subtitle} {keywords}").to_lowercase(),
    });
    row
}

/// A section of the settings page, and the words that find it.
///
/// The words are written out rather than scraped off the labels, because what
/// someone types is what they call the thing and not what the page happens to
/// call it. Nothing on the Drives section says "USB" and nothing on Converting
/// says "overwrite", and those are the first words anyone would reach for.
struct SettingsSection {
    group: adw::PreferencesGroup,
    row: adw::ExpanderRow,
    terms: String,
}

/// Show the sections matching what has been typed, and open them.
///
/// Opening them is the point. A search that only narrowed the list would leave
/// the reader to find the heading, open it, and then look for the setting
/// inside; the whole reason to type is to be shown the thing itself.
fn filter_settings(sections: &[SettingsSection], typed: &str) {
    let needle = typed.trim().to_lowercase();
    for section in sections {
        if needle.is_empty() {
            // Shown again, but not shut. Which sections are open is the
            // reader's state to hold: leaving Settings to go and look at what
            // a switch actually did, and coming back to find the page folded
            // up again, means finding the place a second time.
            section.group.set_visible(true);
            continue;
        }
        let hit = needle
            .split_whitespace()
            .all(|word| section.terms.contains(word));
        section.group.set_visible(hit);
        section.row.set_expanded(hit);
    }
}

fn build_settings_page(ui: &Rc<App>, container: &gtk::Box) {
    let page = adw::PreferencesPage::new();
    let mut sections: Vec<SettingsSection> = Vec::new();

    let conversion = settings_section(
        &mut sections,
        "Converting",
        "Checks to run before a file is written",
        "warn overwrite replace lose information confirm quality",
    );
    let current = ui.settings.borrow().clone();

    let warn = adw::SwitchRow::builder()
        .title("Warn if something will be lost")
        .subtitle("Some formats cannot store everything. Ask me before dropping anything.")
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
    conversion.add_row(&warn);

    let overwrite = adw::SwitchRow::builder()
        .title("Ask before overwriting")
        .subtitle("Check with me when a file of that name is already there")
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
    conversion.add_row(&overwrite);
    *ui.overwrite_switch.borrow_mut() = Some(overwrite.clone());

    let appearance = settings_section(
        &mut sections,
        "Appearance",
        "How the window looks and moves",
        "animation animate motion speed slide",
    );
    let animate = adw::SwitchRow::builder()
        .title("Animations")
        .subtitle("Menus and pages slide. Turn off and they change instantly.")
        .active(current.animations)
        .build();
    {
        let ui = ui.clone();
        animate.connect_active_notify(move |r| {
            let on = r.is_active();
            ui.shell.set_animate(on);
            ui.controls_reveal
                .set_transition_duration(if on { CONTROLS_MS } else { 0 });
            ui.page_faces
                .set_transition_duration(if on { MORPH_MS } else { 0 });
            let mut s = ui.settings.borrow_mut();
            s.animations = on;
            let _ = s.save();
        });
    }
    appearance.add_row(&animate);

    let opening = settings_section(
        &mut sections,
        "Opening files",
        "Where browsing starts, and what Quick Access lists",
        "quick access browse default folder rows scroll columns details removed places",
    );
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
    opening.add_row(&open_row);

    let nearby_row = adw::SwitchRow::builder()
        .title("Show Quick Access")
        .subtitle(
            "Lists files from your folders on the Convert page, so you can pick one \
             without browsing. Drives are listed but not read until you switch them on.",
        )
        .active(current.show_nearby_files)
        .build();
    {
        let ui = ui.clone();
        nearby_row.connect_active_notify(move |r| {
            {
                let mut s = ui.settings.borrow_mut();
                s.show_nearby_files = r.is_active();
                let _ = s.save();
            }
            refresh_nearby(&ui);
        });
    }
    opening.add_row(&nearby_row);

    let visible_row = adw::SpinRow::builder()
        .title("Rows before it scrolls")
        .subtitle("How tall Quick Access gets before you scroll inside it instead")
        .adjustment(&gtk::Adjustment::new(
            current.quick_access_visible as f64,
            1.0,
            40.0,
            1.0,
            5.0,
            0.0,
        ))
        .build();
    {
        let ui = ui.clone();
        visible_row.connect_value_notify(move |r| {
            {
                let mut s = ui.settings.borrow_mut();
                s.quick_access_visible = r.value() as u32;
                let _ = s.save();
            }
            refresh_nearby(&ui);
        });
    }
    opening.add_row(&visible_row);

    let limit_row = adw::SpinRow::builder()
        .title("How many files to find")
        .subtitle("Quick Access lists the newest this many, across all your folders")
        .adjustment(&gtk::Adjustment::new(
            current.quick_access_limit as f64,
            1.0,
            200.0,
            1.0,
            10.0,
            0.0,
        ))
        .build();
    {
        let ui = ui.clone();
        limit_row.connect_value_notify(move |r| {
            {
                let mut s = ui.settings.borrow_mut();
                s.quick_access_limit = r.value() as u32;
                let _ = s.save();
            }
            refresh_nearby(&ui);
        });
    }
    opening.add_row(&limit_row);

    let layout_row = adw::ComboRow::builder()
        .title("Where file details go")
        .subtitle("The format, size, folder and age of each file")
        // Short enough to be read whole. A combo row shows its value on the
        // right of the same line as its title, in whatever is left over, and
        // the long form was being cut in half exactly where the difference
        // between the two answers lives.
        .model(&gtk::StringList::new(&["In columns", "Under the name"]))
        .selected(if current.quick_access_columns { 0 } else { 1 })
        .build();
    {
        let ui = ui.clone();
        layout_row.connect_selected_notify(move |r| {
            {
                let mut s = ui.settings.borrow_mut();
                s.quick_access_columns = r.selected() == 0;
                let _ = s.save();
            }
            refresh_nearby(&ui);
        });
    }
    opening.add_row(&layout_row);

    // The three rows above only describe a list that is being shown. With
    // Quick Access off they are settings for something that is not there, so
    // they go with it.
    {
        let rows: Vec<gtk::Widget> = vec![
            visible_row.clone().upcast(),
            limit_row.clone().upcast(),
            layout_row.clone().upcast(),
        ];
        let show = move |on: bool| {
            for r in &rows {
                r.set_visible(on);
            }
        };
        show(current.show_nearby_files);
        nearby_row.connect_active_notify(move |r| show(r.is_active()));
    }

    // A removed folder can be added again through the picker; a removed drive
    // is only ever offered because it is attached, so without this there would
    // be no way back at all. It lives here rather than in the menu it undoes,
    // which is a menu for choosing where to look and not for administering it.
    //
    // Listed one per row rather than counted, because a count leaves the
    // reader to take on trust both which places are hidden and that more than
    // one can be - and each one is put back on its own, which a single "show
    // them all" button could not do.
    if !current.quick_access_hidden.is_empty() {
        let removed = adw::ExpanderRow::builder()
            .title("Folders you removed")
            .subtitle(match current.quick_access_hidden.len() {
                1 => "One place is no longer offered".to_string(),
                n => format!("{n} places are no longer offered"),
            })
            .build();
        for key in &current.quick_access_hidden {
            // A drive is stored as "drive:LABEL"; a folder as its path.
            let (name, where_) = match key.strip_prefix("drive:") {
                Some(label) => (label.to_string(), "Drive".to_string()),
                None => (
                    std::path::Path::new(key)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| key.clone()),
                    key.clone(),
                ),
            };
            let row = adw::ActionRow::builder()
                .title(&name)
                .subtitle(&where_)
                .build();
            let back = gtk::Button::with_label("Show");
            back.set_valign(gtk::Align::Center);
            {
                let ui = ui.clone();
                let key = key.clone();
                let row = row.clone();
                let removed = removed.clone();
                back.connect_clicked(move |b| {
                    {
                        let mut s = ui.settings.borrow_mut();
                        s.quick_access_hidden.retain(|k| *k != key);
                        let _ = s.save();
                    }
                    refresh_nearby(&ui);
                    b.set_sensitive(false);
                    removed.remove(&row);
                    let left = ui.settings.borrow().quick_access_hidden.len();
                    removed.set_subtitle(&match left {
                        0 => "All of them are offered again".to_string(),
                        1 => "One place is no longer offered".to_string(),
                        n => format!("{n} places are no longer offered"),
                    });
                });
            }
            row.add_suffix(&back);
            removed.add_row(&row);
        }
        opening.add_row(&removed);
    }

    // WatchDog. Its own section, because it is the one thing here that acts
    // without being asked and should be findable, readable and stoppable in
    // one place.
    let automatic = settings_section(
        &mut sections,
        "WatchDog mode",
        "Watches a folder and converts what your slicer leaves there, then copies it to your printer's drive",
        "watchdog auto automatic watch folder unattended background staging ram memory eye",
    );

    let auto_on = adw::SwitchRow::builder()
        .title("Let WatchDog watch")
        .subtitle(
            "The same switch as the eye in the title bar. Only runs while this window is open.",
        )
        .active(current.auto_convert)
        .build();
    automatic.add_row(&auto_on);
    *ui.auto_switch.borrow_mut() = Some(auto_on.clone());

    // Which folder, which format, which destination - the three that need a
    // picker rather than a number, and that belong beside the chain that shows
    // what they are doing.
    let where_row = adw::ActionRow::builder()
        .title("Folder, format and destination")
        .subtitle("Set on WatchDog's own page, beside what it is doing")
        .activatable(true)
        .build();
    where_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let ui = ui.clone();
        where_row.connect_activated(move |_| ui.shell.show(Section::WatchDog));
    }
    automatic.add_row(&where_row);

    let detail = watchdog_detail_rows(ui);
    for row in &detail {
        automatic.add_row(row);
    }

    // The rows above only describe a thing that is running.
    {
        let mut rows: Vec<gtk::Widget> = vec![where_row.clone().upcast()];
        rows.extend(detail.iter().map(|r| r.clone().upcast::<gtk::Widget>()));
        let show = move |on: bool| {
            for r in &rows {
                r.set_visible(on);
            }
        };
        show(current.auto_convert);
        let ui = ui.clone();
        auto_on.connect_active_notify(move |r| {
            let on = r.is_active();
            show(on);
            {
                let mut s = ui.settings.borrow_mut();
                s.auto_convert = on;
                let _ = s.save();
            }
            rearm_auto(&ui);
        });
    }

    let saving = settings_section(
        &mut sections,
        "Saving files",
        "What the Save to menu lists, and in what order",
        "save to destination output recent folders startup launch default",
    );
    let recents_row = adw::SpinRow::builder()
        .title("Recent folders to list")
        .subtitle("Extra places in the Save to menu, on top of the two always there")
        .adjustment(&gtk::Adjustment::new(
            current.recent_output_shown as f64,
            0.0,
            5.0,
            1.0,
            1.0,
            0.0,
        ))
        .build();
    {
        let ui = ui.clone();
        recents_row.connect_value_notify(move |r| {
            let mut s = ui.settings.borrow_mut();
            s.recent_output_shown = r.value() as u32;
            let _ = s.save();
        });
    }
    saving.add_row(&recents_row);

    let pin_row = adw::SwitchRow::builder()
        .title("Always start here")
        .subtitle(
            "Every launch uses the Save to and output format you have set right now. \
             Turn off and it carries on from wherever you left it.",
        )
        .active(current.startup_pinned)
        .build();
    {
        let ui = ui.clone();
        pin_row.connect_active_notify(move |r| {
            let on = r.is_active();
            // Captured at the moment it is switched on, from what is on screen
            // - which is the only reading of "these" that matches the switch
            // the user just looked at.
            let follow = ui.out_auto_drive.get();
            let dir = ui.out_dir.borrow().clone();
            let format = ui.output_picker.selected().map(|s| s.to_string());
            let mut s = ui.settings.borrow_mut();
            s.startup_pinned = on;
            if on {
                s.startup_follow_drive = follow;
                s.startup_output_dir = dir;
                s.startup_output_format = format;
            }
            let _ = s.save();
        });
    }
    saving.add_row(&pin_row);

    // Two sections rather than one heading over two lists. Being readable and
    // being writable are separate facts - receiving .ctb files without ever
    // writing them is a real position to be in - and as sections of their own
    // they can be searched for separately as well.
    for (title, subtitle, listed, hidden_now, writing, keywords) in [
        (
            "Formats it opens",
            "Turn off the ones you never read. Nothing is lost - you can still pick one by hand.",
            registry::readable(),
            current.hidden_input_formats.clone(),
            false,
            "input read import sl1 goo ctb phz uvj hide menu",
        ),
        (
            "Formats it saves",
            "Turn off the ones you never write, to keep the output menu short",
            registry::writable(),
            current.hidden_output_formats.clone(),
            true,
            "output write export sl1 goo ctb phz uvj hide menu",
        ),
    ] {
        let expander = settings_section(&mut sections, title, subtitle, keywords);
        let mut sorted = listed;
        sorted.sort_by_key(|i| i.name.to_lowercase());
        for info in sorted {
            let row = adw::SwitchRow::builder()
                .title(info.name)
                .subtitle(format!(".{}", info.extension))
                .active(!hidden_now.iter().any(|h| h == info.id))
                .build();
            let ui = ui.clone();
            let id = info.id.to_string();
            row.connect_active_notify(move |r| {
                {
                    let mut s = ui.settings.borrow_mut();
                    let list = if writing {
                        &mut s.hidden_output_formats
                    } else {
                        &mut s.hidden_input_formats
                    };
                    list.retain(|h| *h != id);
                    if !r.is_active() {
                        list.push(id.clone());
                    }
                    let _ = s.save();
                }
                if writing {
                    let hidden = ui.settings.borrow().hidden_output_formats.clone();
                    ui.output_picker.set_hidden(hidden);
                }
            });
            expander.add_row(&row);
        }
    }

    let drives = settings_section(
        &mut sections,
        "Drives",
        "USB drives and SD cards, and where files go on them",
        "usb sd card stick removable eject pin pinned subfolder mount",
    );
    let sub_row = adw::EntryRow::builder()
        .title("Folder to save into on a drive")
        .build();
    // An entry row has no room for a second line, and the title alone does not
    // say what an empty box means.
    sub_row.set_tooltip_text(Some(
        "Made on the drive if it is not there. Leave empty to save to the top level.",
    ));
    sub_row.set_text(&current.pinned_subfolder);
    {
        let ui = ui.clone();
        sub_row.connect_changed(move |r| {
            let mut s = ui.settings.borrow_mut();
            s.pinned_subfolder = r.text().trim().trim_matches('/').to_string();
            let _ = s.save();
        });
    }
    drives.add_row(&sub_row);

    let follow_row = adw::SwitchRow::builder()
        .title("Switch to a drive when I plug it in")
        .subtitle("Sets Save to that drive the moment it appears")
        .active(current.auto_lock_new_drives)
        .build();
    {
        let ui = ui.clone();
        follow_row.connect_active_notify(move |r| {
            let mut s = ui.settings.borrow_mut();
            s.auto_lock_new_drives = r.is_active();
            let _ = s.save();
        });
    }
    drives.add_row(&follow_row);

    let sort_row = adw::SwitchRow::builder()
        .title("Put the newest file first when ejecting")
        .subtitle(
            "Printers list files in the order they were copied, so the newest ends up \
             at the bottom. This reorders the drive as it is ejected. Needs fatsort.",
        )
        .active(current.sort_drive_on_eject)
        .build();
    if drives::fatsort_missing() {
        sort_row.set_subtitle(
            "Needs the fatsort tool, which is not installed. \
             Install it with: sudo apt install fatsort",
        );
        sort_row.set_sensitive(false);
    }
    {
        let ui = ui.clone();
        sort_row.connect_active_notify(move |r| {
            let mut s = ui.settings.borrow_mut();
            s.sort_drive_on_eject = r.is_active();
            let _ = s.save();
        });
    }
    drives.add_row(&sort_row);

    let mounted = drives::mounted();
    if mounted.is_empty() {
        drives.add_row(
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
        // An action row with the switch added by hand rather than a switch
        // row. A switch row owns its switch and puts it first among the
        // suffixes, so anything else added lands to the right of it and pushes
        // it in off the edge the other switches on the page line up on.
        let row = adw::ActionRow::builder()
            .title(&d.name)
            .subtitle(&space)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(if d.removable {
            "drive-removable-media-symbolic"
        } else {
            "drive-harddisk-symbolic"
        }));
        let pin = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .active(current.is_pinned(&d.name))
            .tooltip_text("Offer this drive in the Save to menu")
            .build();
        let ui2 = ui.clone();
        let name = d.name.clone();
        pin.connect_active_notify(move |r| {
            let mut s = ui2.settings.borrow_mut();
            if r.is_active() {
                s.pin_volume(&name);
            } else {
                s.unpin_volume(&name);
            }
            let _ = s.save();
        });
        // Eject appears only on drives that can actually be ejected. A drive
        // the system depends on never qualifies, whatever the desktop says
        // about it, and neither does one the user has locked.
        if drives::can_remove(&d.name) {
            let protected = current.never_eject.contains(&d.name);

            let eject = shell::icon_button("media-eject-symbolic", "Eject this drive");
            eject.set_valign(gtk::Align::Center);
            // One function decides this, so the button and the action that
            // follows it can never disagree about whether it is allowed.
            eject.set_visible(drives::is_ejectable(&d.name, &current.never_eject));

            // The lock is what makes the blacklist reachable: a drive you
            // never want ejected is marked here rather than in a text file.
            let lock = gtk::ToggleButton::builder()
                .valign(gtk::Align::Center)
                .active(protected)
                .build();
            lock.add_css_class("flat");
            show_lock(&lock);
            {
                let ui = ui.clone();
                let name = d.name.clone();
                let eject = eject.clone();
                lock.connect_toggled(move |b| {
                    {
                        let mut s = ui.settings.borrow_mut();
                        s.never_eject.retain(|p| *p != name);
                        if b.is_active() {
                            s.never_eject.push(name.clone());
                        }
                        let _ = s.save();
                    }
                    show_lock(b);
                    eject.set_visible(!b.is_active());
                });
            }
            row.add_suffix(&lock);
            let ui3 = ui.clone();
            let name = d.name.clone();
            let drive = d.clone();
            let btn = eject.clone();
            eject.connect_clicked(move |_| {
                // Ejecting can take a moment while buffers flush; disabling
                // the button says so and stops a second request.
                btn.set_sensitive(false);
                let ui4 = ui3.clone();
                let name2 = name.clone();
                let btn2 = btn.clone();
                let sort = ui3.settings.borrow().sort_drive_on_eject;
                drives::eject(&drive, sort, move |res| {
                    btn2.set_sensitive(true);
                    match res {
                        Ok(()) => {
                            ui4.toasts
                                .add_toast(adw::Toast::new(&format!("{name2} is safe to remove")));
                            refresh_nearby(&ui4);
                        }
                        Err(e) => {
                            ui4.toasts.add_toast(adw::Toast::new(&format!(
                                "Could not eject {name2}: {e}"
                            )));
                        }
                    }
                });
            });
            row.add_suffix(&eject);
        }
        // Last, so it sits where every other switch on the page sits.
        row.add_suffix(&pin);
        row.set_activatable_widget(Some(&pin));
        drives.add_row(&row);
    }

    // Last on the page, because it is the one control here that undoes every
    // other one and nobody should meet it on the way to something else.
    let reset_section = settings_section(
        &mut sections,
        "Start over",
        "Put everything on this page back to how it came",
        "reset defaults factory clear wipe",
    );
    let reset_row = adw::ActionRow::builder()
        .title("Reset all settings")
        .subtitle("Only settings. Your files, history and drives are untouched.")
        .build();
    let reset_btn = gtk::Button::with_label("Reset");
    reset_btn.add_css_class("destructive-action");
    reset_btn.set_valign(gtk::Align::Center);
    {
        let ui = ui.clone();
        let container = container.clone();
        reset_btn.connect_clicked(move |_| {
            let ask = adw::MessageDialog::builder()
                .transient_for(&ui.window)
                .modal(true)
                .heading("Reset all settings?")
                .body(
                    "Every setting on this page goes back to how it arrived. \
                     Nothing you have converted is affected.",
                )
                .build();
            ask.add_response("cancel", "Cancel");
            ask.add_response("reset", "Reset");
            ask.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
            ask.set_default_response(Some("cancel"));
            let ui = ui.clone();
            let container = container.clone();
            ask.connect_response(None, move |d, response| {
                d.close();
                if response != "reset" {
                    return;
                }
                {
                    let mut s = ui.settings.borrow_mut();
                    *s = Settings::default();
                    let _ = s.save();
                }
                // Everything the settings feed, put back by hand: the page is
                // built once from the values it was given, and so is most of
                // what it controls.
                let fresh = ui.settings.borrow().clone();
                ui.shell.set_animate(fresh.animations);
                ui.controls_reveal
                    .set_transition_duration(if fresh.animations { CONTROLS_MS } else { 0 });
                ui.page_faces
                    .set_transition_duration(if fresh.animations { MORPH_MS } else { 0 });
                ui.output_picker.set_hidden(Vec::new());
                set_out_auto_drive(&ui);
                refresh_nearby(&ui);
                // WatchDog is a running thing, not only a stored one. Putting
                // the setting back to off while leaving the folder monitor
                // armed would give a program that says it is not watching and
                // is. This drops the monitor, clears what was queued, and puts
                // the eye and the switch back in step with the setting.
                rearm_auto(&ui);
                refresh_dropzone_text(&ui);
                while let Some(child) = container.first_child() {
                    container.remove(&child);
                }
                build_settings_page(&ui, &container);
                ui.toasts
                    .add_toast(adw::Toast::new("Settings are back to their defaults"));
            });
            ask.present();
        });
    }
    reset_row.add_suffix(&reset_btn);
    reset_section.add_row(&reset_row);

    let about = settings_section(
        &mut sections,
        "About",
        "Version, formats, and where the code lives",
        "version licence license gpl open source github copyright",
    );
    about.add_row(
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
    about.add_row(
        &adw::ActionRow::builder()
            .title("Supported formats")
            .subtitle(formats.join("\n"))
            .build(),
    );
    let licence = adw::ActionRow::builder()
        .title("Licence")
        .subtitle(
            "GNU GPL v3 or later. Free to use, change and share - and anyone who \
             shares it has to share the source too, so it cannot be turned into a \
             paid, closed program.",
        )
        .activatable(true)
        .build();
    licence.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    licence.connect_activated(|_| {
        let _ = gio::AppInfo::launch_default_for_uri(
            "https://www.gnu.org/licenses/gpl-3.0.html",
            gio::AppLaunchContext::NONE,
        );
    });
    about.add_row(&licence);

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
    about.add_row(&repo);
    let settings_file = adw::ActionRow::builder()
        .title("Settings file")
        .subtitle(
            Settings::path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not available".into()),
        )
        .build();
    about.add_row(&settings_file);

    // Added at the end, in the order they were made, so the page reads the way
    // it always has and only the search knows there is a list of them.
    for section in &sections {
        page.add(&section.group);
    }

    // The search sits above the page rather than in it, because a field that
    // scrolls away with the thing it is filtering is no use once you have
    // scrolled.
    let find = gtk::SearchEntry::new();
    find.set_placeholder_text(Some("Search settings"));
    find.set_margin_start(theme::SPACE_5);
    find.set_margin_end(theme::SPACE_5);
    find.set_margin_top(theme::SPACE_4);
    let sections = Rc::new(sections);
    {
        let sections = sections.clone();
        find.connect_search_changed(move |e| filter_settings(&sections, &e.text()));
    }
    // The search is cleared when the page is come back to, so it never opens
    // already filtered by something typed a week ago. Clearing it shows every
    // section again and leaves them however they were left.
    {
        let find = find.clone();
        let sections = sections.clone();
        container.connect_map(move |_| {
            find.set_text("");
            filter_settings(&sections, "");
        });
    }

    container.append(&find);
    container.append(&page);
}

/// Restore what the last session was doing (§37: does it remember?).
fn restore_session(ui: &Rc<App>) {
    let saved = ui.settings.borrow().clone();

    // Recently used formats feed the picker's own section.
    ui.output_picker
        .set_hidden(saved.hidden_output_formats.clone());

    // A remembered format that has since been switched off is not a format
    // the picker can show as chosen, so it falls back like any other.
    let hidden = saved.hidden_output_formats.clone();
    let usable = |id: &String| {
        registry::by_id(id).map(|h| h.info().capabilities.writes) == Some(true)
            && !hidden.iter().any(|h| h == id)
    };
    let remembered = if saved.startup_pinned {
        saved.startup_output_format.clone()
    } else {
        saved.last_output_format.clone()
    };
    let chosen = remembered.filter(usable).or_else(|| {
        let mut writable: Vec<_> = registry::writable()
            .into_iter()
            .filter(|i| !hidden.iter().any(|h| h == i.id))
            .collect();
        writable.sort_by_key(|i| i.name.to_lowercase());
        writable.first().map(|i| i.id.to_string())
    });
    if let Some(id) = chosen {
        ui.output_picker.set_selected(&id);
    }

    // A pinned answer is what the window opens with, whatever happened last
    // time. Otherwise it carries on where it left off.
    //
    // Following a drive is restored ahead of any remembered folder, because it
    // is a standing instruction and the folder is only where following
    // happened to land. A remembered folder is restored only if it is still
    // there.
    let (follow, folder) = if saved.startup_pinned {
        (saved.startup_follow_drive, saved.startup_output_dir.clone())
    } else {
        (saved.follow_drive, saved.last_output_dir.clone())
    };
    if follow {
        set_out_auto_drive(ui);
    } else if let Some(dir) = folder.filter(|d| d.is_dir()) {
        set_out_dir(ui, Some(dir));
    } else {
        set_out_dir(ui, None);
    }
    revalidate(ui);
}
