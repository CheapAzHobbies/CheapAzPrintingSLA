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

/* The output control is a button and the input is not, so their metrics are
   set together rather than left to two different defaults. Otherwise the two
   columns sit at different heights and the row looks misaligned. */
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
