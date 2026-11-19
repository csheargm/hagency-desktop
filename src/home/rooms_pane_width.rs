//! The rooms list's width as the home screen's layout math needs it: the
//! dock's root splitter when it is laid out, else the saved layout's align
//! (a just-restored dock has no splitter until it draws), else the default.
use makepad_widgets::{DockItem, SplitterAlign};
use std::collections::HashMap;

/// The rooms list's width that `align` asks for in a dock `total` points wide.
pub fn align_width(align: SplitterAlign, total: f64) -> f64 {
    match align {
        SplitterAlign::FromA(width) => width,
        SplitterAlign::FromB(width) => total - width,
        SplitterAlign::Weighted(weight) => total * weight,
    }
}

/// The rooms pane's width: the laid-out `position`, else the root splitter's
/// align in `items` (a restored layout), else `fallback`. Never negative.
pub fn rooms_pane_width(position: Option<f64>, items: Option<&HashMap<makepad_widgets::LiveId, DockItem>>, root: makepad_widgets::LiveId, total: f64, fallback: f64) -> f64 {
    let saved = || match items?.get(&root)? {
        DockItem::Splitter { align, .. } => Some(align_width(*align, total)),
        _ => None,
    };
    position.or_else(saved).filter(|w| w.is_finite()).unwrap_or(fallback).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::{id, LiveId, SplitterAxis};

    fn layout(align: SplitterAlign) -> HashMap<LiveId, DockItem> {
        HashMap::from([(id!(root), DockItem::Splitter { axis: SplitterAxis::Horizontal, align, a: id!(rooms), b: id!(chat) })])
    }

    #[test]
    fn the_laid_out_splitter_wins_then_the_saved_align_then_the_fallback() {
        let saved = layout(SplitterAlign::FromA(380.0));
        assert_eq!(rooms_pane_width(Some(250.0), Some(&saved), id!(root), 1400.0, 300.0), 250.0);
        assert_eq!(rooms_pane_width(None, Some(&saved), id!(root), 1400.0, 300.0), 380.0, "restored: the saved width, not the fallback");
        assert_eq!(rooms_pane_width(None, Some(&layout(SplitterAlign::FromB(1000.0))), id!(root), 1400.0, 300.0), 400.0);
        assert_eq!(rooms_pane_width(None, Some(&layout(SplitterAlign::Weighted(0.25))), id!(root), 1200.0, 300.0), 300.0);
        assert_eq!(rooms_pane_width(None, None, id!(root), 1400.0, 300.0), 300.0);
        assert_eq!(rooms_pane_width(None, Some(&layout(SplitterAlign::FromA(0.0))), id!(root), 1400.0, 300.0), 0.0, "collapsed");
    }
}
