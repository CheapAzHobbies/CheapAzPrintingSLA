//! The guide: short walkthroughs, in a panel beside the page.
//!
//! Beside rather than over, because a guide you have to close to follow is a
//! guide you read once and then work from memory. The panel sits on the right
//! of the content and the program carries on underneath it, so a step can be
//! read and done in the same moment. On a window too narrow for two columns it
//! slides over the top instead - see `Shell::set_compact`.
//!
//! Written for somebody who has not done this before. Short sentences, one
//! action to a step, and every term explained the first time it is used. The
//! test it has to pass is a fifteen year old with a printer and no idea what a
//! slicer is, reading it at the machine.
//!
//! The content is data - a list of guides, each a list of steps - so adding a
//! walkthrough is adding an entry, not writing widget code. Screenshots are
//! embedded in the binary for the same reason the save indicator is: a copied
//! binary with no asset folder still has to work.

use crate::theme;

/// The pictures. Cropped to the control being talked about and all made the
/// same size, so the panel does not lurch as you scroll from one step to the
/// next. Embedded rather than loaded from disk, like the save indicator: a
/// copied binary with no asset folder beside it still has to work.
/// Every guide picture is cropped to this, so the panel does not lurch as you
/// scroll from one step to the next and one height request serves them all.
const SHOT_HEIGHT: i32 = 104;

const SHOT_ADD_FILES: &[u8] = include_bytes!("../../../assets/guide/add-files.png");
const SHOT_OUTPUT: &[u8] = include_bytes!("../../../assets/guide/output.png");
const SHOT_SAVE_TO: &[u8] = include_bytes!("../../../assets/guide/save-to.png");
const SHOT_WD_SWITCH: &[u8] = include_bytes!("../../../assets/guide/watchdog-switch.png");
const SHOT_WD_ROWS: &[u8] = include_bytes!("../../../assets/guide/watchdog-rows.png");
const SHOT_WD_CHAIN: &[u8] = include_bytes!("../../../assets/guide/watchdog-chain.png");
use adw::prelude::*;
use gtk::glib;
use std::rc::Rc;

/// One thing to do, and what happens when you do it.
struct Step {
    /// What to do, as an instruction: "Add your file", not "Adding files".
    heading: &'static str,
    /// One to three short sentences. What to press, and what you should see.
    body: &'static str,
    /// A picture of the thing being described, cropped to just that thing.
    /// A screenshot of a whole window makes the reader hunt for the control
    /// twice: once in the picture and once on screen.
    shot: Option<&'static [u8]>,
}

/// A walkthrough: a title, a line saying who it is for, and the steps.
struct Guide {
    title: &'static str,
    /// Shown under the title in the list. Says what you end up with.
    blurb: &'static str,
    icon: &'static str,
    steps: &'static [Step],
    /// Anything worth knowing that is not a step. Shown at the end.
    notes: &'static [&'static str],
}

const FIRST_FILE: &[Step] = &[
    Step {
        heading: "Add your file",
        body: "Go to Convert and click Add Files. Pick the file your slicer \
               made. A slicer is the program that turns a 3D model into the \
               layers a printer prints - PrusaSlicer, Chitubox and Lychee are \
               all slicers. You can also drag a file straight onto the window.",
        shot: Some(SHOT_ADD_FILES),
    },
    Step {
        heading: "Check what it found",
        body: "The Input box names the format it found. CheapAzSLA reads the \
               inside of the file rather than trusting its name, so this is \
               almost always right. Leave it on Detect Automatically unless \
               you know it is wrong.",
        shot: None,
    },
    Step {
        heading: "Pick what to make",
        body: "Open the Output menu and choose the format your printer reads. \
               Elegoo Saturn and Mars printers read GOO. Most Anycubic and \
               Phrozen printers read CTB. If you are not sure, check what \
               your slicer normally exports for that printer.",
        shot: Some(SHOT_OUTPUT),
    },
    Step {
        heading: "Say where it goes",
        body: "Use Save to and choose a folder, or a USB drive if one is \
               plugged in. Drives you have plugged in before are listed by \
               name.",
        shot: Some(SHOT_SAVE_TO),
    },
    Step {
        heading: "Convert",
        body: "Click Convert All. A message appears at the bottom when it is \
               done, with a button that opens the finished file.",
        shot: None,
    },
];

const TO_A_DRIVE: &[Step] = &[
    Step {
        heading: "Plug the drive in first",
        body: "Plug in the USB stick or SD card your printer reads. Wait a \
               second for the computer to notice it.",
        shot: None,
    },
    Step {
        heading: "Choose it under Save to",
        body: "Open Save to. The drive is listed by its name. Pick it, and \
               converted files are written straight onto it - there is no \
               second copying step to forget.",
        shot: Some(SHOT_SAVE_TO),
    },
    Step {
        heading: "Eject before you pull it out",
        body: "Click the eject button and wait until it says the drive is \
               safe to remove. Pulling a drive out while it is still being \
               written leaves a file the printer cannot read, and the file \
               usually looks fine until the print fails.",
        shot: None,
    },
];

const WATCHDOG: &[Step] = &[
    Step {
        heading: "Turn it on",
        body: "Open WatchDog in the sidebar and switch it on. WatchDog \
               watches one folder for you. When your slicer saves a new file \
               there, it converts the file and puts the result where you say. \
               You do not have to open anything or press anything.",
        shot: Some(SHOT_WD_SWITCH),
    },
    Step {
        heading: "Choose the folder to watch",
        body: "Click Choose beside Folder to watch and pick the folder your \
               slicer saves into. For most people that is Downloads, or \
               wherever you told the slicer to export.",
        shot: Some(SHOT_WD_ROWS),
    },
    Step {
        heading: "Choose the format",
        body: "Set Convert to whatever your printer reads. This is the same \
               choice as Output on the Convert page.",
        shot: None,
    },
    Step {
        heading: "Choose where results go",
        body: "Set Save into to a folder or a USB drive. Nothing is converted \
               until you have chosen this, so WatchDog cannot quietly fill up \
               a folder you did not mean.",
        shot: None,
    },
    Step {
        heading: "Read the row of milestones",
        body: "The four milestones show where a file has got to. Grey means \
               not yet. A stop that fades in and out is working, or waiting \
               for you. Solid white means done. Green on the last one means \
               the file has landed. A red cross means something it needs is \
               missing - click that milestone to fix it.",
        shot: Some(SHOT_WD_CHAIN),
    },
];

const BEFORE_YOU_PRINT: &[Step] = &[
    Step {
        heading: "Look at the layers",
        body: "Open Preview and drag the slider. Each frame is one layer, \
               exactly as the printer will cure it. White is resin that will \
               harden, black is resin that will not.",
        shot: None,
    },
    Step {
        heading: "Look for islands",
        body: "An island is a patch of white with nothing under it on the \
               layer before. It has nothing to stick to, so it drops off into \
               the vat and can ruin the rest of the print. If you find one, \
               go back to your slicer and add supports there.",
        shot: None,
    },
    Step {
        heading: "Read the warning if you get one",
        body: "Formats do not all store the same things. If the format you \
               are converting to cannot hold something the original had, you \
               are told what will be lost before anything is written. Read \
               it, then decide - it is not an error.",
        shot: None,
    },
];

const TIPS: &[Step] = &[
    Step {
        heading: "Drag files straight onto the window",
        body: "You do not have to use Add Files. Dropping a file anywhere on \
               the Convert page adds it.",
        shot: None,
    },
    Step {
        heading: "Convert several files at once",
        body: "Add as many as you like. They all convert to the same format, \
               into the same place, one after another.",
        shot: None,
    },
    Step {
        heading: "Look inside a file",
        body: "Convert to UVJ. A UVJ file is an ordinary zip: open it with \
               your file manager and you get one picture per layer, plus a \
               text file of the settings. It is the fastest way to find out \
               what actually went to the printer when a print has failed.",
        shot: None,
    },
    Step {
        heading: "Everything you converted is in History",
        body: "History lists what you converted, when, and where it went, \
               with a button to open the folder. Anything WatchDog did on its \
               own is marked as WatchDog's.",
        shot: None,
    },
    Step {
        heading: "Press Ctrl+K to search",
        body: "It searches settings and pages by name. Faster than hunting \
               through the sidebar when you half remember what something is \
               called.",
        shot: None,
    },
];

const GUIDES: &[Guide] = &[
    Guide {
        title: "Convert your first file",
        blurb: "Slicer file in, printer file out",
        icon: "document-open-symbolic",
        steps: FIRST_FILE,
        notes: &[
            "Nothing is changed about the file you started with. Converting \
             always writes a new file and leaves the original alone.",
        ],
    },
    Guide {
        title: "Save straight to a USB drive",
        blurb: "Write straight to the stick, and eject it safely",
        icon: "drive-removable-media-symbolic",
        steps: TO_A_DRIVE,
        notes: &[
            "If the drive is not listed, it is not plugged in properly. \
             Unplug it and plug it back in.",
        ],
    },
    Guide {
        title: "Let WatchDog do it for you",
        blurb: "Save from your slicer, find it already converted",
        icon: "folder-saved-search-symbolic",
        steps: WATCHDOG,
        notes: &[
            "WatchDog forgets the folder when you close the app. It never \
             starts watching anything on its own - you choose the folder each \
             time you want it working, and leave the app open while it does.",
            "It will not convert a file it made itself, so pointing it at a \
             folder and saving results into that same folder is safe.",
        ],
    },
    Guide {
        title: "Check a file before you print",
        blurb: "Catch the faults that waste a night and a tank of resin",
        icon: "view-reveal-symbolic",
        steps: BEFORE_YOU_PRINT,
        notes: &[],
    },
    Guide {
        title: "Tips",
        blurb: "Small things that save time",
        icon: "starred-symbolic",
        steps: TIPS,
        notes: &[],
    },
];

/// The guide panel: a list of walkthroughs that opens onto one at a time.
pub struct Help {
    pub widget: adw::NavigationView,
}

impl Help {
    pub fn new(close: impl Fn() + 'static + Clone) -> Rc<Self> {
        let close2 = close.clone();
        let view = adw::NavigationView::new();
        view.set_width_request(320);

        let list = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title("Step by step")
            .description("Pick one. Each is a few steps, in order.")
            .build();

        for (i, guide) in GUIDES.iter().enumerate() {
            let row = adw::ActionRow::builder()
                .title(guide.title)
                .subtitle(guide.blurb)
                .title_lines(2)
                .subtitle_lines(3)
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(guide.icon));
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let view2 = view.clone();
            let close3 = close.clone();
            row.connect_activated(move |_| {
                view2.push(&page_for(&GUIDES[i], close3.clone()));
            });
            group.add(&row);
        }
        list.add(&group);

        let header = adw::HeaderBar::new();
        header.set_show_end_title_buttons(false);
        let shut = crate::shell::icon_button("window-close-symbolic", "Close the guide");
        let close_here = close.clone();
        shut.connect_clicked(move |_| close_here());
        header.pack_end(&shut);

        let bar = adw::ToolbarView::new();
        bar.add_top_bar(&header);
        bar.set_content(Some(&list));

        view.add(
            &adw::NavigationPage::builder()
                .title("Guide")
                .child(&bar)
                .build(),
        );

        // Escape goes back a page, and closes the panel when there is no page
        // to go back to. AdwNavigationView already pops on Escape, so this
        // only has to answer for the root - stealing it everywhere would take
        // the back gesture away.
        let keys = gtk::EventControllerKey::new();
        let view2 = view.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape && view2.navigation_stack().n_items() <= 1 {
                close2();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        view.add_controller(keys);

        Rc::new(Self { widget: view })
    }
}

/// One walkthrough, as a page that can be pushed onto the panel.
fn page_for(guide: &'static Guide, close: impl Fn() + 'static) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();

    let steps = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_5);
    steps.set_margin_top(theme::SPACE_2);
    for (n, step) in guide.steps.iter().enumerate() {
        steps.append(&step_widget(n + 1, step, guide.steps.len() > 1));
    }
    group.add(&steps);
    page.add(&group);

    if !guide.notes.is_empty() {
        let extra = adw::PreferencesGroup::builder()
            .title("Worth knowing")
            .build();
        let notes = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_3);
        notes.set_margin_top(theme::SPACE_2);
        for note in guide.notes {
            let label = gtk::Label::builder().label(*note).xalign(0.0).build();
            label.set_wrap(true);
            label.add_css_class("cz-dim");
            notes.append(&label);
        }
        extra.add(&notes);
        page.add(&extra);
    }

    // No window controls in here. The panel is inside the window, not a
    // window of its own, and a close button in its header that shuts the whole
    // program would be a trap.
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    let shut = crate::shell::icon_button("window-close-symbolic", "Close the guide");
    shut.connect_clicked(move |_| close());
    header.pack_end(&shut);

    let bar = adw::ToolbarView::new();
    bar.add_top_bar(&header);
    bar.set_content(Some(&page));
    adw::NavigationPage::builder()
        .title(guide.title)
        .child(&bar)
        .build()
}

/// A numbered step: the number, the instruction, the explanation, the picture.
///
/// The number is drawn as its own thing to the left rather than written into
/// the heading, so the eye can count the steps without reading them and see
/// how much is left.
fn step_widget(number: usize, step: &'static Step, numbered: bool) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, theme::SPACE_3);
    row.set_valign(gtk::Align::Start);

    if numbered {
        let badge = gtk::Label::new(Some(&number.to_string()));
        badge.add_css_class("cz-step-number");
        badge.set_valign(gtk::Align::Start);
        badge.set_width_request(24);
        row.append(&badge);
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_2);
    column.set_hexpand(true);

    let heading = gtk::Label::builder()
        .label(step.heading)
        .xalign(0.0)
        .build();
    heading.add_css_class("heading");
    heading.set_wrap(true);
    column.append(&heading);

    // Not dimmed. Dim is for captions beside something else that carries the
    // meaning; here the sentence is the thing, and somebody is reading it at
    // the machine with resin on their hands.
    let body = gtk::Label::builder().label(step.body).xalign(0.0).build();
    body.set_wrap(true);
    column.append(&body);

    if let Some(bytes) = step.shot {
        if let Some(picture) = shot_widget(bytes) {
            column.append(&picture);
        }
    }

    row.append(&column);
    row.upcast()
}

/// A screenshot, at its own size, with a border so it reads as a picture of
/// the program rather than as part of the panel.
fn shot_widget(bytes: &'static [u8]) -> Option<gtk::Widget> {
    let texture = gtk::gdk::Texture::from_bytes(&glib::Bytes::from_static(bytes)).ok()?;
    let picture = gtk::Picture::for_paintable(&texture);
    picture.set_content_fit(gtk::ContentFit::ScaleDown);
    // Filling the width rather than hugging its own. A picture allowed to
    // shrink and told to hug takes the smallest size it can, which is nothing
    // at all - the first attempt rendered every screenshot as a dot.
    picture.set_halign(gtk::Align::Fill);
    picture.set_hexpand(true);
    picture.set_can_shrink(true);
    // And an explicit height. A picture that may shrink has a natural height
    // of nothing, so in a vertical box it is given nothing - which is why the
    // first two attempts drew every screenshot as a dot and then as a line.
    // Every picture here is the same height by construction, so one number
    // holds for all of them.
    picture.set_size_request(-1, SHOT_HEIGHT);
    picture.add_css_class("cz-help-shot");
    picture.set_margin_top(theme::SPACE_1);
    Some(picture.upcast())
}
