//! A chain of milestones with the broken link crossed out.
//!
//! The shape is the old network-status dialog: a row of things that have to be
//! true, joined left to right, each either done, being worked on, waiting its
//! turn, or crossed out. Its whole value is that a glance tells you which link
//! is the broken one, rather than a single line of text that says something is
//! wrong and leaves you to work out where.
//!
//! The connectors are thin progress bars. Between two finished steps one sits
//! full; while something is actually moving across it, it pulses - which is
//! the same trick the old file-copy dialogs used, and reads as movement
//! without anything having to be drawn by hand.

use crate::theme;
use adw::prelude::*;
use gtk::glib;

/// Where one milestone has got to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Not reached yet. Nothing is wrong; it is simply not this step's turn.
    Waiting,
    /// Happening now.
    Working,
    /// Behind us.
    Done,
    /// The broken link: something this step needs is not there.
    Missing,
}

struct Step {
    icon: gtk::Image,
    badge: gtk::Image,
    label: gtk::Label,
    note: gtk::Label,
}

pub struct Steps {
    pub widget: gtk::Box,
    steps: Vec<Step>,
    links: Vec<gtk::ProgressBar>,
}

impl Steps {
    /// `stops` is the icon and label for each milestone, in order.
    pub fn new(stops: &[(&str, &str)]) -> std::rc::Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        widget.set_halign(gtk::Align::Center);
        widget.add_css_class("cz-steps");

        let mut steps = Vec::new();
        let mut links = Vec::new();
        for (i, (icon_name, text)) in stops.iter().enumerate() {
            if i > 0 {
                // The link between this stop and the one before it. Given a
                // fixed width so the chain does not reflow every time a label
                // underneath it changes length.
                let link = gtk::ProgressBar::builder()
                    .valign(gtk::Align::Center)
                    .width_request(56)
                    .margin_bottom(theme::SPACE_5)
                    .build();
                link.add_css_class("cz-step-link");
                widget.append(&link);
                links.push(link);
            }

            let column = gtk::Box::new(gtk::Orientation::Vertical, theme::SPACE_1);
            column.set_valign(gtk::Align::Start);
            column.set_size_request(96, -1);

            // The badge sits on the corner of the icon rather than replacing
            // it, so a crossed-out step still says which step it was.
            let stack = gtk::Overlay::new();
            let icon = gtk::Image::from_icon_name(icon_name);
            icon.set_pixel_size(28);
            icon.add_css_class("cz-step-icon");
            stack.set_child(Some(&icon));
            let badge = gtk::Image::from_icon_name("emblem-ok-symbolic");
            badge.set_pixel_size(14);
            badge.set_halign(gtk::Align::End);
            badge.set_valign(gtk::Align::End);
            badge.set_visible(false);
            stack.add_overlay(&badge);
            column.append(&stack);

            let label = gtk::Label::new(Some(text));
            label.add_css_class("caption");
            label.set_wrap(true);
            label.set_justify(gtk::Justification::Center);
            label.set_max_width_chars(12);
            column.append(&label);

            let note = gtk::Label::new(None);
            note.add_css_class("caption");
            note.add_css_class("cz-dim");
            note.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            note.set_max_width_chars(14);
            note.set_visible(false);
            column.append(&note);

            widget.append(&column);
            steps.push(Step {
                icon,
                badge,
                label,
                note,
            });
        }

        std::rc::Rc::new(Self {
            widget,
            steps,
            links,
        })
    }

    pub fn set_state(&self, at: usize, state: State) {
        let Some(step) = self.steps.get(at) else {
            return;
        };
        for class in [
            "cz-step-waiting",
            "cz-step-working",
            "cz-step-done",
            "cz-step-missing",
        ] {
            step.icon.remove_css_class(class);
            step.label.remove_css_class(class);
        }
        let class = match state {
            State::Waiting => "cz-step-waiting",
            State::Working => "cz-step-working",
            State::Done => "cz-step-done",
            State::Missing => "cz-step-missing",
        };
        step.icon.add_css_class(class);
        step.label.add_css_class(class);

        match state {
            State::Done => {
                step.badge.set_icon_name(Some("emblem-ok-symbolic"));
                step.badge.set_css_classes(&["cz-ok"]);
                step.badge.set_visible(true);
            }
            State::Missing => {
                step.badge.set_icon_name(Some("window-close-symbolic"));
                step.badge.set_css_classes(&["cz-error"]);
                step.badge.set_visible(true);
            }
            _ => step.badge.set_visible(false),
        }
    }

    /// A second line under a milestone: which folder, which drive, which file.
    pub fn set_note(&self, at: usize, note: Option<&str>) {
        let Some(step) = self.steps.get(at) else {
            return;
        };
        match note {
            Some(text) if !text.is_empty() => {
                step.note.set_text(text);
                step.note.set_visible(true);
            }
            _ => {
                step.note.set_text("");
                step.note.set_visible(false);
            }
        }
    }

    /// How full the link into a step is: `None` means nothing is crossing it,
    /// `Some(f)` a known fraction, and `pulse` for movement of unknown length.
    pub fn set_link(&self, into: usize, fraction: Option<f64>) {
        let Some(link) = into.checked_sub(1).and_then(|i| self.links.get(i)) else {
            return;
        };
        link.set_fraction(fraction.unwrap_or(0.0).clamp(0.0, 1.0));
    }

    pub fn pulse_link(&self, into: usize) {
        if let Some(link) = into.checked_sub(1).and_then(|i| self.links.get(i)) {
            link.pulse();
        }
    }
}

/// Keep the pulsing links moving while something is crossing them.
///
/// One timer for the whole chain rather than one per link, and it stops the
/// moment nothing is moving - a bar that pulses forever is a bar that says
/// nothing, and costs something to say it.
pub fn pulse_while(
    steps: &std::rc::Rc<Steps>,
    which: Vec<usize>,
    going: std::rc::Rc<std::cell::Cell<bool>>,
) {
    if which.is_empty() {
        return;
    }
    let steps = steps.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
        if !going.get() {
            return glib::ControlFlow::Break;
        }
        for i in &which {
            steps.pulse_link(*i);
        }
        glib::ControlFlow::Continue
    });
}
