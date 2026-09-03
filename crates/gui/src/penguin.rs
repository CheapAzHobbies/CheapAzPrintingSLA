//! The save indicator.
//!
//! A sprite sheet of a dancing penguin, recoloured to a dark silhouette,
//! shown while a conversion is running. Carried over from CheapAzHobbies'
//! `lens`, where the sheet was built by `tools/make_penguin_sheet.py`.
//!
//! The sheet is a grid rather than one row: 141 frames side by side would
//! otherwise be a 35000 pixel image. Frames are authored at 20fps and one full
//! loop is 7.05 seconds.
//!
//! Cells are 248x240, rebuilt from the source at twice the size lens used.
//! That project drew the indicator at about 40 pixels, so 124x120 cells were
//! ample there; here it is drawn at 88 and the smaller cells were being
//! stretched past their resolution and looked soft.
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
const FRAME_W: u32 = 248;
const FRAME_H: u32 = 240;
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

/// Decode the sheet and cut it into per-frame RGBA buffers.
///
/// Kept free of GTK so it can be tested without a display, which is how the
/// silent-failure bug in the first version was found: it returned None and the
/// indicator simply never appeared, with nothing to say why.
pub fn slice_pixels() -> Result<Vec<Vec<u8>>, String> {
    let mut reader = png::Decoder::new(SHEET)
        .read_info()
        .map_err(|e| format!("sheet header: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("sheet data: {e}"))?;
    let (sheet_w, sheet_h) = (info.width as usize, info.height as usize);
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(format!(
            "sheet is {:?}/{:?}, expected Rgba/Eight",
            info.color_type, info.bit_depth
        ));
    }
    let stride = sheet_w * 4;
    let (fw, fh) = (FRAME_W as usize, FRAME_H as usize);
    if sheet_w < fw * COLS {
        return Err(format!(
            "sheet is {sheet_w}px wide, needs {} for {COLS} columns",
            fw * COLS
        ));
    }

    let mut out = Vec::with_capacity(FRAMES);
    for i in 0..FRAMES {
        let (cx, cy) = ((i % COLS) * fw, (i / COLS) * fh);
        if cx + fw > sheet_w || cy + fh > sheet_h {
            return Err(format!(
                "frame {i} at ({cx},{cy}) falls outside the {sheet_w}x{sheet_h} sheet"
            ));
        }
        let mut px = Vec::with_capacity(fw * fh * 4);
        for row in 0..fh {
            let start = (cy + row) * stride + cx * 4;
            px.extend_from_slice(&buf[start..start + fw * 4]);
        }
        out.push(px);
    }
    Ok(out)
}

fn slice() -> Option<Vec<gdk::Texture>> {
    let frames = match slice_pixels() {
        Ok(f) => f,
        Err(e) => {
            // Say why rather than quietly showing nothing.
            eprintln!("cheapazsla: save indicator unavailable: {e}");
            return None;
        }
    };
    let (fw, fh) = (FRAME_W as usize, FRAME_H as usize);
    Some(
        frames
            .into_iter()
            .map(|px| {
                gdk::MemoryTexture::new(
                    fw as i32,
                    fh as i32,
                    gdk::MemoryFormat::R8g8b8a8,
                    &Bytes::from_owned(px),
                    fw * 4,
                )
                .upcast()
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sheet_slices_into_every_frame() {
        let frames = slice_pixels().expect("sheet must decode");
        assert_eq!(frames.len(), FRAMES, "expected {FRAMES} frames");
        let expect = (FRAME_W * FRAME_H * 4) as usize;
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.len(), expect, "frame {i} is the wrong size");
        }
    }

    #[test]
    fn frames_are_not_all_empty() {
        // A sheet that decodes to fully transparent pixels would animate
        // invisibly, which looks identical to the indicator being broken.
        let frames = slice_pixels().expect("decode");
        let opaque = frames
            .iter()
            .filter(|f| f.chunks_exact(4).any(|p| p[3] > 0))
            .count();
        assert!(
            opaque > FRAMES / 2,
            "only {opaque} of {FRAMES} frames have any opaque pixels"
        );
    }
}

/// An animated save indicator.
///
/// Returns a widget plus handles to start and stop it. Nothing runs while it
/// is stopped, so an idle window costs nothing.
/// How long work must last before the indicator appears at all.
///
/// Reading a small file finishes in a few tens of milliseconds. Showing a
/// busy animation for that long is worse than showing nothing: it registers
/// as a flicker, which reads as a fault rather than as progress. Nothing is
/// drawn unless the work outlives this.
const SHOW_AFTER_MS: u64 = 400;

pub struct Penguin {
    pub widget: gtk::Picture,
    timer: RefCell<Option<gtk::glib::SourceId>>,
    /// Waiting to find out whether this job is slow enough to be worth
    /// showing. Cancelled by `stop` if it is not.
    pending: RefCell<Option<gtk::glib::SourceId>>,
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
            pending: RefCell::new(None),
            frames,
            index: RefCell::new(0),
        })
    }

    /// True when the sheet decoded and there is something to show.
    pub fn is_available(&self) -> bool {
        self.frames.is_some()
    }

    /// Arm the indicator. It appears only if the work is still going after
    /// `SHOW_AFTER_MS`; a fast job shows nothing at all.
    pub fn start(self: &Rc<Self>) {
        if self.frames.is_none() || self.timer.borrow().is_some() || self.pending.borrow().is_some()
        {
            return;
        }
        let me = self.clone();
        let id = gtk::glib::timeout_add_local_once(
            std::time::Duration::from_millis(SHOW_AFTER_MS),
            move || {
                me.pending.borrow_mut().take();
                me.show_now();
            },
        );
        *self.pending.borrow_mut() = Some(id);
    }

    fn show_now(self: &Rc<Self>) {
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

    /// Sit still and be visible: here, but not working.
    ///
    /// Twenty frames a second forever is not a status light, it is a heater -
    /// and this program is meant to run on machines somebody is not proud of.
    /// So waiting is one frame, and only actual work moves.
    pub fn rest(&self) {
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
        *self.index.borrow_mut() = 0;
        if let Some(first) = self.frames.as_ref().and_then(|f| f.first()) {
            self.widget.set_paintable(Some(first));
            self.widget.set_visible(true);
        }
    }

    pub fn stop(&self) {
        // Cancelling before it is shown is the common case, and the point:
        // the work finished quickly and nothing should have appeared.
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
        self.widget.set_visible(false);
        *self.index.borrow_mut() = 0;
    }
}
