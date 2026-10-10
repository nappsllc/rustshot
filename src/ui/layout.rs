//! The layout cursor: controls stack top to bottom in a column; inside
//! [`Ui::row`] they flow left to right (or right to left for button rows),
//! all in logical px. A control asks for its natural width (or the rest of
//! the row/column) and its height; `width`/`height`/`place` override the
//! next control.

use super::Ui;
use crate::objects::FRect;

/// Gap between stacked rows and between controls in a row.
pub const GAP: f32 = 8.0;
/// Height of a single-line control (the toolbar button size).
pub const H: f32 = crate::editor::style::BTN;

#[derive(Clone, Copy, Debug)]
struct RowState {
    /// Next free x (from the left), or the right edge still free (rtl).
    x: f32,
    y: f32,
    h: f32,
    rtl: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    area: FRect,
    y: f32,
    row: Option<RowState>,
    next_w: Option<f32>,
    next_h: Option<f32>,
    next: Option<FRect>,
    pub gap: f32,
}

impl Layout {
    pub fn new(area: FRect) -> Layout {
        Layout { area, y: area.y, row: None, next_w: None, next_h: None, next: None, gap: GAP }
    }

    pub fn in_row(&self) -> bool {
        self.row.is_some()
    }

    /// A `w` (None = the rest) × `h` box at the cursor; advances it.
    fn alloc(&mut self, w: Option<f32>, h: f32) -> FRect {
        let w = self.next_w.take().or(w);
        let h = self.next_h.take().unwrap_or(h);
        if let Some(r) = self.next.take() {
            return r;
        }
        let a = self.area;
        match self.row.as_mut() {
            Some(row) if row.rtl => {
                let w = w.unwrap_or(f32::MAX).min(row.x - a.x).max(0.0);
                let r = FRect { x: row.x - w, y: row.y, w, h };
                row.x -= w + self.gap;
                row.h = row.h.max(h);
                r
            }
            Some(row) => {
                let w = w.unwrap_or(f32::MAX).min(a.x1() - row.x).max(0.0);
                let r = FRect { x: row.x, y: row.y, w, h };
                row.x += w + self.gap;
                row.h = row.h.max(h);
                r
            }
            None => {
                let r = FRect { x: a.x, y: self.y, w: w.unwrap_or(a.w).min(a.w), h };
                self.y += h + self.gap;
                r
            }
        }
    }
}

impl Ui<'_> {
    /// Allocate the next control's box; returned in physical px.
    pub fn alloc(&mut self, w: Option<f32>, h: f32) -> FRect {
        let r = self.lay.alloc(w, h);
        let k = self.k;
        FRect { x: (r.x * k).round(), y: (r.y * k).round(), w: (r.w * k).round(), h: (r.h * k).round() }
    }

    /// Lay out the controls drawn by `f` left to right on one line.
    pub fn row<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.row_dir(false, f)
    }

    fn row_dir<R>(&mut self, rtl: bool, f: impl FnOnce(&mut Self) -> R) -> R {
        let a = self.lay.area;
        let x = if rtl { a.x1() } else { a.x };
        let outer = self.lay.row.replace(RowState { x, y: self.lay.y, h: 0.0, rtl });
        let out = f(self);
        let row = std::mem::replace(&mut self.lay.row, outer).expect("row");
        if row.h > 0.0 {
            self.lay.y += row.h + self.lay.gap;
        }
        out
    }

    /// Lay out `f` inside `r` (logical px), then restore the cursor.
    pub fn area<R>(&mut self, r: FRect, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = std::mem::replace(&mut self.lay, Layout::new(r));
        self.lay.gap = saved.gap;
        let out = f(self);
        self.lay = saved;
        out
    }

    /// Vertical space (or horizontal, inside a row).
    pub fn space(&mut self, v: f32) {
        match self.lay.row.as_mut() {
            Some(row) if row.rtl => row.x -= v,
            Some(row) => row.x += v,
            None => self.lay.y += v,
        }
    }

    /// Width (logical px) of the next control.
    pub fn width(&mut self, w: f32) -> &mut Self {
        self.lay.next_w = Some(w);
        self
    }

    /// Height (logical px) of the next control.
    pub fn height(&mut self, h: f32) -> &mut Self {
        self.lay.next_h = Some(h);
        self
    }

    /// Put the next control exactly at `r` (logical px; the cursor stays).
    pub fn place(&mut self, r: FRect) -> &mut Self {
        self.lay.next = Some(r);
        self
    }

    /// The column's next free y (logical px).
    pub fn cursor_y(&self) -> f32 {
        self.lay.y
    }

    /// The layout area (logical px).
    pub fn bounds(&self) -> FRect {
        self.lay.area
    }
}
