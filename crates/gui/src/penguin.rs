//! The save indicator.
//!
//! A sprite sheet of a dancing penguin, recoloured to a dark silhouette,
//! shown while a conversion is running. Carried over from CheapAzHobbies'
//! `lens`, where the sheet was built by `tools/make_penguin_sheet.py`.
//!
//! The sheet is a grid rather than one row: 141 frames at 124 pixels wide
//! would otherwise be a 17000 pixel image. Frames are authored at 20fps and
//! one full loop is 7.05 seconds.
//!
//! It is embedded in the binary rather than loaded from disk, so an AppImage
//! or a single copied binary keeps working with no asset path to get wrong.

use gtk::gdk;
use gtk::glib::Bytes;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

const SHEET: &[u8] = include_bytes!("../../../assets/penguin_saving.png");
const FRAMES: usize = 141;
const COLS: usize = 12;
const FRAME_W: u32 = 124;
const FRAME_H: u32 = 120;
/// 20fps, the rate the frames were authored at.
const FRAME_MS: u64 = 50;

thread_local! {
    /// Sliced once and reused. Decoding the sheet costs a few milliseconds and
    /// there is no reason to pay it every time a conversion starts.
    static CACHE: RefCell<Option<Rc<Vec<gdk::Texture>>>> = const { RefCell::new(None) };
}

/// Slice the sheet into one texture per frame.
fn frames() -> Option<Rc<Vec<gdk::Texture>>> {
    CACHE.with(|c| {
        if let Some(f) = c.borrow().as_ref() {
            return Some(f.clone());
        }
        let sliced = slice()?;
        let rc = Rc::new(sliced);
        *c.borrow_mut() = Some(rc.clone());
        Some(rc)
    })
}

fn slice() -> Option<Vec<gdk::Texture>> {
    let mut reader = png::Decoder::new(SHEET).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (sheet_w, sheet_h) = (info.width as usize, info.height as usize);
    // The sheet is authored RGBA; anything else means the asset was replaced
    // with something unexpected, and silently guessing would look worse than
    // simply not showing the indicator.
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let stride = sheet_w * 4;
    let (fw, fh) = (FRAME_W as usize, FRAME_H as usize);

    let mut out = Vec::with_capacity(FRAMES);
    for i in 0..FRAMES {
        let (cx, cy) = ((i % COLS) * fw, (i / COLS) * fh);
        if cx + fw > sheet_w || cy + fh > sheet_h {
            break;
        }
        let mut px = Vec::with_capacity(fw * fh * 4);
        for row in 0..fh {
            let start = (cy + row) * stride + cx * 4;
            px.extend_from_slice(&buf[start..start + fw * 4]);
        }
        let t = gdk::MemoryTexture::new(
            fw as i32,
            fh as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &Bytes::from_owned(px),
            fw * 4,
        );
        out.push(t.upcast());
    }
    (!out.is_empty()).then_some(out)
}

/// An animated save indicator.
///
/// Returns a widget plus handles to start and stop it. Nothing runs while it
/// is stopped, so an idle window costs nothing.
pub struct Penguin {
    pub widget: gtk::Picture,
    timer: RefCell<Option<gtk::glib::SourceId>>,
    frames: Option<Rc<Vec<gdk::Texture>>>,
    index: RefCell<usize>,
}

impl Penguin {
    pub fn new(height: i32) -> Rc<Self> {
        let frames = frames();
        let widget = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .height_request(height)
            .width_request((FRAME_W as f32 * height as f32 / FRAME_H as f32).round() as i32)
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        if let Some(f) = frames.as_ref().and_then(|f| f.first()) {
            widget.set_paintable(Some(f));
        }
        Rc::new(Self {
            widget,
            timer: RefCell::new(None),
            frames,
            index: RefCell::new(0),
        })
    }

    /// True when the sheet decoded and there is something to show.
    pub fn is_available(&self) -> bool {
        self.frames.is_some()
    }

    pub fn start(self: &Rc<Self>) {
        if self.frames.is_none() || self.timer.borrow().is_some() {
            return;
        }
        self.widget.set_visible(true);
        let me = self.clone();
        let id =
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(FRAME_MS), move || {
                let Some(frames) = me.frames.as_ref() else {
                    return gtk::glib::ControlFlow::Break;
                };
                let mut i = me.index.borrow_mut();
                *i = (*i + 1) % frames.len();
                me.widget.set_paintable(Some(&frames[*i]));
                gtk::glib::ControlFlow::Continue
            });
        *self.timer.borrow_mut() = Some(id);
    }

    pub fn stop(&self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
        self.widget.set_visible(false);
        *self.index.borrow_mut() = 0;
    }
}
