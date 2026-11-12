//! Shared divider styling for the room list and mini-app workspace.
use makepad_widgets::*;

pub const WIDTH: f64 = 4.0;
pub const HIT_MARGIN: f64 = 6.0;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.RobrixSplitter = Splitter {
        // The short grip sits on the boundary, not beside it. The strip and
        // native hit margins keep it easy to drag without a second long bar.
        size: #(WIDTH)
        hit_margin: #(HIT_MARGIN)
        // The engine now holds a position the app writes to the first pane's
        // floor as well as a dragged one (50 by default). The room list snaps
        // shut by writing 0 (`MainDesktopUI::normalize_rooms_width`), which
        // the floor turned into a 50-point sliver. For a side-by-side split
        // the first pane's floor is `min_vertical`, named for the bar.
        min_vertical: 0.0

        draw_bg +: {
            bar_size: 32.0
            color_bg: mod.widgets.RINX_PAGE
            color: mix(mod.widgets.RINX_PAGE, mod.widgets.RINX_INK, 0.08)
            color_hover: mix(mod.widgets.RINX_PAGE, mod.widgets.RINX_INK, 0.35)
            color_drag: mix(mod.widgets.RINX_PAGE, mod.widgets.RINX_INK, 0.50)

            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.clear(self.color_bg)
                if self.is_vertical > 0.5 {
                    sdf.rect(self.rect_size.x * 0.5 - 0.5, 0.0, 1.0, self.rect_size.y)
                } else {
                    sdf.rect(0.0, self.rect_size.y * 0.5 - 0.5, self.rect_size.x, 1.0)
                }
                sdf.fill(self.color)
                if self.is_vertical > 0.5 {
                    sdf.box(self.rect_size.x * 0.5 - 1.5, self.rect_size.y * 0.5 - self.bar_size * 0.5, 3.0, self.bar_size, 1.5)
                } else {
                    sdf.box(self.rect_size.x * 0.5 - self.bar_size * 0.5, self.rect_size.y * 0.5 - 1.5, self.bar_size, 3.0, 1.5)
                }
                return sdf.fill(self.color_hover.mix(self.color_drag, self.drag) * vec4(1.0, 1.0, 1.0, max(self.hover, self.drag)))
            }
        }

        animator: Animator{
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.1}}
                    apply: {
                        draw_bg: {drag: 0.0, hover: 0.0}
                    }
                }

                on: AnimatorState{
                    from: {
                        all: Forward {duration: 0.1}
                        drag: Forward {duration: 0.01}
                    }
                    apply: {
                        draw_bg: {
                            drag: 0.0,
                            hover: snap(1.0)
                        }
                    }
                }

                drag: AnimatorState{
                    from: { all: Forward { duration: 0.1 }}
                    apply: {
                        draw_bg: {
                            drag: snap(1.0),
                            hover: 1.0
                        }
                    }
                }
            }
        }
    }

}
