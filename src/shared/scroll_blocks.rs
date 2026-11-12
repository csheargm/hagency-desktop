//! Keeps scrolling alive after a popup that blocked it has gone.
//!
//! A popup blocks scrolling outside itself with `Cx::block_scrolling_except_within`. The
//! engine keeps those blocks as a stack and a modal releases its own block by the area of its
//! content. A block nobody releases stays for good: every scroll anywhere in the app is then
//! refused until restart.
//!
//! That happens when a panel closes its own modal while the panel is borrowed (`action(cx,
//! modal, Close)` on the panel's reference does that): the modal cannot reach its content, so
//! it closes without finding the area to release. It would happen the same way for any popup
//! that goes away without closing.

use makepad_widgets::*;

/// Releases scroll blocks whose owner is no longer drawn, innermost first, and stops at the
/// first block whose owner is still on screen.
///
/// `Area::Empty` is not an owner. It is the request to block scrolling everywhere and is left
/// to whoever made it.
pub fn release_undrawn(cx: &mut Cx) {
    while let Some(area) = cx.fingers.blocked_scrolling_exception_area() {
        if area.is_empty() || area.is_valid(cx) {
            break;
        }
        cx.unblock_scrolling();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The area of something that was drawn once and is not drawn any more.
    fn undrawn(cx: &mut Cx, draw_item_id: usize) -> Area {
        let list = DrawList2d::new(cx);
        Area::Instance(InstanceArea {
            draw_list_id: list.id(),
            draw_item_id,
            instance_offset: 0,
            instance_count: 1,
            redraw_id: u64::MAX,
        })
    }

    #[test]
    fn a_block_left_by_a_popup_that_is_gone_is_released() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let first = undrawn(&mut cx, 1);
        let second = undrawn(&mut cx, 2);
        cx.block_scrolling_except_within(first);
        cx.block_scrolling_except_within(second);
        assert_eq!(cx.fingers.blocked_scrolling_exception_area(), Some(second));

        release_undrawn(&mut cx);

        assert_eq!(
            cx.fingers.blocked_scrolling_exception_area(),
            None,
            "Both popups are gone, so nothing may keep scrolling blocked"
        );
    }

    #[test]
    fn a_request_to_block_everywhere_is_kept() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let gone = undrawn(&mut cx, 1);
        cx.block_scrolling_except_within(gone);
        cx.block_scrolling_except_within(Area::Empty);

        release_undrawn(&mut cx);

        assert_eq!(
            cx.fingers.blocked_scrolling_exception_area(),
            Some(Area::Empty),
            "Blocking everywhere has no area to be drawn; only its maker releases it"
        );
    }
}
