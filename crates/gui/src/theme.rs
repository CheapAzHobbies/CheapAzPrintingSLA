//! Design tokens and stylesheet (§31–§35).
//!
//! One accent, a spacing scale everything aligns to, and both themes defined
//! deliberately rather than one being an inversion of the other.
//!
//! The accent is a desaturated teal. It reads as instrument panel rather than
//! consumer product, it is distinct from the stock GNOME blue so the
//! application has an identity, and it stays legible on both grounds. Red is
//! reserved for destructive actions, which is the reason not to spend it here.
//!
//! GTK's CSS is not the web's. Custom properties (`--x`) arrived in GTK 4.16
//! and `:root` is not a selector at all, so the palette is built with
//! `@define-color` and the whole sheet is reloaded when the scheme changes.
//! Targeting 4.12 keeps the application buildable on distributions that are
//! not shipping the newest GTK yet.

use gtk::gdk;
use std::cell::RefCell;

/// Spacing scale. Every margin and gap is one of these, so controls line up
/// without anyone measuring.
pub const SPACE_1: i32 = 4;
pub const SPACE_2: i32 = 8;
pub const SPACE_3: i32 = 12;
pub const SPACE_4: i32 = 16;
pub const SPACE_5: i32 = 24;
pub const SPACE_6: i32 = 32;

/// Sidebar width: wide enough for the longest label at larger text sizes,
/// narrow enough to leave the workspace dominant.
pub const SIDEBAR_WIDTH: i32 = 152;

struct Palette {
    bg: &'static str,
    panel: &'static str,
    border: &'static str,
    text: &'static str,
    dim: &'static str,
    hover: &'static str,
}

/// Deep neutral charcoal, panels a step lighter, borders barely there.
const DARK: Palette = Palette {
    bg: "#1c1d1f",
    panel: "#232427",
    border: "#34363b",
    text: "#e8e9ea",
    dim: "#9aa0a6",
    hover: "rgba(255,255,255,0.06)",
};

/// Warm off-white, not a flipped dark theme. Borders carry the structure
/// here, because shadows read as heavy on a pale ground.
const LIGHT: Palette = Palette {
    bg: "#faf9f7",
    panel: "#ffffff",
    border: "#dcdad5",
    text: "#1f2124",
    dim: "#6b7075",
    hover: "rgba(0,0,0,0.05)",
};

const ACCENT: &str = "#2f8f8f";
const ACCENT_HOVER: &str = "#37a5a5";
const OK: &str = "#4caf7d";
const WARN: &str = "#d9a441";
const ERROR: &str = "#e05a5a";

fn sheet(p: &Palette) -> String {
    format!(
        r#"
@define-color cz_bg {bg};
@define-color cz_panel {panel};
@define-color cz_border {border};
@define-color cz_text {text};
@define-color cz_dim {dim};
@define-color cz_accent {accent};

/* ---- shell ---- */
.cz-sidebar {{
  background-color: @cz_panel;
  border-right: 1px solid @cz_border;
}}
.cz-workspace {{ background-color: @cz_bg; }}

.cz-nav-item {{
  padding: 9px 8px;
  margin: 2px 5px;
  border-radius: 6px;
  color: @cz_dim;
  transition: background-color 140ms ease, color 140ms ease;
}}
/* Folded, the rail is a clip over contents still laid out at full width, so a
   highlight with a margin on its right ran under the rail's border and was cut
   off mid-corner. Edge to edge instead, which is what a selected row in an icon
   rail should look like anyway. */
.compact .cz-nav-item {{
  margin-right: 0;
  border-radius: 6px 0 0 6px;
}}
.cz-nav-item:hover {{ background-color: {hover}; color: @cz_text; }}
.cz-nav-item.selected {{
  background-color: alpha(@cz_accent, 0.16);
  color: @cz_text;
  font-weight: bold;
}}
/* An accent bar marks the active section, so selection is not carried by
   colour alone. */
.cz-nav-item.selected .cz-nav-marker {{ background-color: @cz_accent; }}
.cz-nav-marker {{ background-color: transparent; border-radius: 2px; }}

/* ---- page padding ----
   Set here rather than as margins in code so the narrow state is one class on
   the window rather than a handle to every page. */
.cz-page-body {{ padding: {space_6}px; }}
/* Only the sides give way. Vertical padding is not what is scarce, and
   changing it made the whole page jump up as the window crossed the step. */
.compact .cz-page-body {{ padding: {space_6}px {space_3}px; }}

/* ---- surfaces ---- */
.cz-panel {{
  background-color: @cz_panel;
  border: 1px solid @cz_border;
  border-radius: 8px;
}}

/* ---- drop zone ---- */
.cz-dropzone {{
  border: 1px dashed @cz_border;
  border-radius: 8px;
  transition: border-color 160ms ease, background-color 160ms ease;
}}
.cz-dropzone.active {{
  border-color: @cz_accent;
  border-style: solid;
  background-color: alpha(@cz_accent, 0.08);
}}

/* ---- quick access ----
   The header and the file list are two boxed lists rather than one, because
   the header has to stay put while the files scroll under it. These make the
   seam invisible: the header loses its bottom corners while the list is open,
   and the list loses its top corners and its top border, so the two read as
   one card with a single line across it. */
/* Linear, not eased. Eased, the radius held square for most of its time and
   then swung round at the end, which does not read as a corner rounding - it
   reads as the corner changing colour and then moving. Acceleration belongs
   to the search box, which travels far enough for it to mean something.

   Short, because it is a step in a sequence rather than the movement itself:
   the corners square off, then the list drops; the list folds, then they
   round. Anything longer and the second half is left waiting on the first.
   CORNER_MS in main.rs is this number and has to stay this number. */
.cz-qa-head, .cz-qa-head > row {{
  transition: border-radius 120ms linear;
}}
/* The header was exactly the shade of the files under it, so open, the two
   read as one undifferentiated slab and the seam between them was doing all
   the work. Lifting it a step separates the control strip from the contents
   without turning it into a second colour in the window. The row carries it
   Painted once, by the list, with the row left transparent on top of it. Both
   draw a corner and both transition it, so if each carried its own tint the
   rounding would show one shade over the other - and two tints stacked came
   out twice as strong as intended anyway.

   A tint laid over the background rather than a colour replacing it: the card
   shade comes from the platform stylesheet and is not ours to name, and the
   first attempt at naming it landed a shade darker than the rows instead of
   lighter. */
.cz-qa-head {{
  background-image: linear-gradient(alpha(@cz_text, 0.05), alpha(@cz_text, 0.05));
}}
/* Every row inside, not just the outer one. An expander row keeps a header
   row of its own further in, and that one paints a background while it is
   expanded and not while it is shut - so the header came out two different
   shades depending on whether the list was open. */
.cz-qa-head row {{
  background-color: transparent;
  background-image: none;
}}
/* Put the hover back, on the list rather than on the row. The row does not
   draw the corners - the list does - so lighting the row left a ring of the
   resting shade around the lit area, widest at the corners, which is what
   made them look wrong on the way past. Lit here, the corners light with it.

   Carried by a class the pointer handlers set, not by :hover. The pointer is
   over the row, and prelight does not reach the list from there. */
.cz-qa-head.cz-qa-lit {{
  background-image: linear-gradient(alpha(@cz_text, 0.1), alpha(@cz_text, 0.1));
}}
/* The row, not just the list: a boxed list draws its rounded corners on its
   first and last rows, so squaring off the list alone changed nothing that
   could be seen. */
.cz-qa-head.cz-qa-open,
.cz-qa-head.cz-qa-open > row {{
  border-bottom-left-radius: 0;
  border-bottom-right-radius: 0;
}}
.cz-qa-body,
.cz-qa-body > row:first-child {{
  border-top-left-radius: 0;
  border-top-right-radius: 0;
}}
.cz-qa-body {{ border-top: none; }}
/* The bottom corners belong to the window the files are seen through, not to
   the last of them. On the list they are drawn by the final row, which is off
   the bottom of the view until it is scrolled to - so the visible bottom edge
   was square for the whole of the scroll and only rounded on arrival. Rounding
   the scroller and clipping to it makes them round throughout. */
.cz-qa-clip {{
  border-bottom-left-radius: 12px;
  border-bottom-right-radius: 12px;
}}

/* A status that can be pressed. Sized like the label it wraps rather than
   like a button, so the column still lines up with the rows that have nothing
   to show. */
.cz-chip-button {{
  padding: 2px 4px;
  min-height: 0;
  min-width: 0;
}}

/* The bin's lid lifts as the pointer arrives. A symbolic icon is one piece,
   so there is no lid to raise on its own - tipping the whole can reads as the
   same gesture and needs no artwork that does not exist. */
.cz-bin image {{
  transition: -gtk-icon-transform 140ms ease-out;
}}
.cz-bin:hover image {{
  -gtk-icon-transform: rotate(-14deg);
}}

/* The arrow leans a quarter turn under the pointer and settles back when it
   leaves - the control saying which way it is about to go before it goes.
   Pressed, it turns for as long as the scan runs: a drive that has gone to
   sleep can take seconds to answer, and the question a spinning arrow settles
   is not "did my press land" but "is it still going". It always stops on a
   whole revolution, which is why the rotation begins and ends on the eighth
   the hover leaves it at - the arrow never has to be dragged back from an
   angle it stopped at by accident. */
.cz-refresh image {{
  transition: -gtk-icon-transform 180ms ease-out;
}}
.cz-refresh:hover image {{
  -gtk-icon-transform: rotate(45deg);
}}
/* Starts where the hover left it. The pointer is on the button at the moment
   it is clicked, so the arrow is already a quarter turn round; a rotation
   starting from zero would snap it backwards before setting off. Ending a full
   turn later puts it back on the quarter, which is where the hover wants it
   anyway - so stopping is as seamless as starting. */
@keyframes cz-turn {{
  from {{ -gtk-icon-transform: rotate(45deg); }}
  to {{ -gtk-icon-transform: rotate(405deg); }}
}}
/* No transition while it is turning: a 180ms ease fighting a 900ms rotation
   drags the arrow backwards every time the keyframe wraps. */
.cz-refresh.cz-turning image,
.cz-refresh.cz-turning:hover image {{
  transition: none;
  animation: cz-turn 900ms linear infinite;
}}

/* WatchDog's eye while it is watching. Tinted rather than shouted: it has to
   be noticed without becoming the loudest thing in a window that is mostly
   not about it. */
.cz-armed {{
  background-color: alpha(@cz_accent, 0.18);
  color: @cz_accent;
}}
.cz-armed:hover {{ background-color: alpha(@cz_accent, 0.28); }}

/* ---- queue ---- */
.cz-queue > row {{
  border-bottom: 1px solid @cz_border;
  transition: background-color 140ms ease;
}}
.cz-queue > row:last-child {{ border-bottom: none; }}
.cz-queue > row:hover {{ background-color: {hover}; }}
/* The stock selection colour on this row reads as an error state. The queue
   selection only decides which file the preview shows, so it is quiet. */
.cz-queue > row:selected {{
  background-color: alpha(@cz_accent, 0.18);
  color: @cz_text;
}}
.cz-queue > row:selected:hover {{ background-color: alpha(@cz_accent, 0.24); }}

/* ---- typography ---- */
.cz-title {{ font-size: 1.45rem; font-weight: bold; }}
.cz-subtitle {{ color: @cz_dim; }}
.cz-section {{
  font-size: 0.78rem;
  font-weight: bold;
  letter-spacing: 0.07em;
  color: @cz_dim;
}}
.cz-value {{ font-feature-settings: "tnum" 1; }}
.cz-dim {{ color: @cz_dim; }}

/* A read-only field shaped like the controls beside it. The input format is
   detected rather than chosen, so it must not look clickable, but leaving it
   as bare text next to a boxed dropdown reads as unfinished. */
.cz-field {{
  background-color: alpha(@cz_border, 0.35);
  border: 1px solid @cz_border;
  border-radius: 6px;
  padding: 0 12px;
  min-height: 34px;
}}

/* Both format controls, so they are one shape rather than two that happen to
   be near each other. The input was once a styled box with a flat button
   inside it, which put the hover highlight on the button and left it stopping
   short of the box's border. */
.cz-format-control {{
  min-height: 34px;
  padding: 0 12px;
  border-radius: 6px;
}}

/* A header-sized icon button. A full-height button in a section header makes
   that header taller than a plain label, so the control beneath it starts
   lower than its neighbour and the two columns stop lining up. */
button.cz-inline {{
  min-height: 18px;
  min-width: 18px;
  padding: 0;
  margin: 0;
}}

/* A control bar floating over the image. Opaque enough to stay readable over
   whatever layer is behind it. */
.cz-overlay-bar {{
  background-color: alpha(@cz_panel, 0.92);
  border: 1px solid @cz_border;
  border-radius: 6px;
  padding: 2px 4px;
}}

/* The arrow says one format becomes another, which is the meaning of the row.
   Dimmed at 12px it read as decoration. */
.cz-arrow {{ color: @cz_accent; }}

/* ---- buttons ---- */
button.cz-primary {{
  background-image: none;
  background-color: @cz_accent;
  color: #ffffff;
  font-weight: bold;
  border: none;
  border-radius: 6px;
  min-height: 34px;
  transition: background-color 140ms ease, opacity 140ms ease;
}}
button.cz-primary:hover:not(:disabled) {{ background-color: {accent_hover}; }}
button.cz-primary:disabled {{ opacity: 0.4; }}

/* Destructive must not look like the primary action. */
button.cz-destructive {{ color: {error}; }}

/* ---- status ---- */
/* Every status carries an icon and a word as well as a colour, so it stays
   readable without colour vision. */
.cz-ok {{ color: {ok}; }}
.cz-warn {{ color: {warn}; }}
.cz-error {{ color: {error}; }}
"#,
        bg = p.bg,
        panel = p.panel,
        border = p.border,
        text = p.text,
        dim = p.dim,
        hover = p.hover,
        accent = ACCENT,
        space_6 = SPACE_6,
        space_3 = SPACE_3,
        accent_hover = ACCENT_HOVER,
        ok = OK,
        warn = WARN,
        error = ERROR,
    )
}

thread_local! {
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

/// Install the stylesheet and keep it in step with the system theme.
pub fn install() {
    let provider = gtk::CssProvider::new();
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    PROVIDER.with(|p| *p.borrow_mut() = Some(provider));

    let manager = adw::StyleManager::default();
    apply(manager.is_dark());
    manager.connect_dark_notify(|m| apply(m.is_dark()));
}

fn apply(dark: bool) {
    let css = sheet(if dark { &DARK } else { &LIGHT });
    PROVIDER.with(|p| {
        if let Some(provider) = p.borrow().as_ref() {
            provider.load_from_string(&css);
        }
    });
}
