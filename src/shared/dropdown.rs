//! Shared settings dropdown face and popup styles.
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // A single item within a Rinx-styled settings DropDown popup menu.
    mod.widgets.RobrixSettingsPopupMenuItem = PopupMenuItem {
        width: Fill, height: Fit
        align: Align{y: 0.5}
        padding: Inset{top: 8, bottom: 8, left: 28, right: 14}

        draw_text +: {
            color: (MESSAGE_TEXT_COLOR),
            color_hover: (MESSAGE_TEXT_COLOR),
            color_active: (COLOR_ACTIVE_PRIMARY_DARKER),
            text_style: SETTINGS_REGULAR_TEXT_STYLE {},
        }

        draw_bg +: {
            color: (COLOR_PRIMARY),
            color_hover: (COLOR_BG_PREVIEW),
            color_active: (COLOR_BG_PREVIEW),
            border_color: vec4(0.0, 0.0, 0.0, 0.0),
            border_color_hover: vec4(0.0, 0.0, 0.0, 0.0),
            border_color_active: vec4(0.0, 0.0, 0.0, 0.0),
            border_size: 0.0,
            border_radius: 3.0,
            mark_color: vec4(0.0, 0.0, 0.0, 0.0),
            mark_color_active: (COLOR_ACTIVE_PRIMARY_DARKER),
        }
    }

    // The popup list shown when a RobrixSettingsDropDown is opened.
    mod.widgets.RobrixSettingsPopupMenu = PopupMenu {
        width: 260, height: Fit
        padding: 4,

        menu_item: mod.widgets.RobrixSettingsPopupMenuItem{}

        draw_bg +: {
            color: (COLOR_PRIMARY),
            border_color: (COLOR_SECONDARY_DARKER),
            border_size: 1.0,
            border_radius: 4.0,
        }
    }

    // A DropDown styled to match other Rinx settings controls.
    mod.widgets.RobrixSettingsDropDown = DropDownFlat {
        width: 218, height: (mod.widgets.SETTINGS_BUTTON_HEIGHT),
        padding: Inset{top: 8, bottom: 8, left: 12, right: 30}
        margin: Inset{left: 5, top: 5, bottom: 5}
        align: Align{x: 0.0, y: 0.5}

        popup_menu: mod.widgets.RobrixSettingsPopupMenu {}

        draw_text +: {
            color: (MESSAGE_TEXT_COLOR),
            color_hover: (MESSAGE_TEXT_COLOR),
            color_focus: (MESSAGE_TEXT_COLOR),
            color_down: (MESSAGE_TEXT_COLOR),
            text_style: SETTINGS_REGULAR_TEXT_STYLE {},
        }

        draw_bg +: {
            color: (COLOR_PRIMARY),
            color_hover: (COLOR_PRIMARY),
            color_down: (COLOR_PRIMARY),
            color_focus: (COLOR_PRIMARY),
            border_color: (COLOR_SECONDARY_DARKER),
            border_color_hover: (COLOR_ACTIVE_PRIMARY),
            border_color_focus: (COLOR_ACTIVE_PRIMARY_DARKER),
            border_color_down: (COLOR_ACTIVE_PRIMARY_DARKER),
            border_size: 1.0,
            border_radius: 4.0,
            arrow_color: (MESSAGE_TEXT_COLOR),
            arrow_color_hover: (COLOR_ACTIVE_PRIMARY_DARKER),
            arrow_color_focus: (COLOR_ACTIVE_PRIMARY_DARKER),
            arrow_color_down: (COLOR_ACTIVE_PRIMARY_DARKER),

            // The base DropDownFlat shader draws the arrow BEFORE the box,
            // so the box fill paints over it. Override to draw the rounded
            // rect first and then the arrow on top.
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)

                sdf.box(
                    self.border_size
                    self.border_size
                    self.rect_size.x - self.border_size * 2.
                    self.rect_size.y - self.border_size * 2.
                    self.border_radius
                )

                let fill = self.color
                    .mix(self.color_focus, self.focus)
                    .mix(self.color_hover, self.hover)
                    .mix(self.color_down, self.down * self.hover)
                    .mix(self.color_disabled, self.disabled)

                let stroke = self.border_color
                    .mix(self.border_color_focus, self.focus)
                    .mix(self.border_color_hover, self.hover)
                    .mix(self.border_color_down, self.down * self.hover)
                    .mix(self.border_color_disabled, self.disabled)

                sdf.fill_keep(fill)
                sdf.stroke(stroke, self.border_size)

                // Draw the down-arrow triangle on top of the filled box.
                let c = vec2(self.rect_size.x - 14.0, self.rect_size.y * 0.5)
                let sz = 3.5
                sdf.move_to(c.x - sz, c.y - sz * 0.5)
                sdf.line_to(c.x + sz, c.y - sz * 0.5)
                sdf.line_to(c.x, c.y + sz * 0.75)
                sdf.close_path()

                let arrow = self.arrow_color
                    .mix(self.arrow_color_focus, self.focus)
                    .mix(self.arrow_color_hover, self.hover)
                    .mix(self.arrow_color_down, self.down * self.hover)
                    .mix(self.arrow_color_disabled, self.disabled)

                sdf.fill(arrow)

                return sdf.result
            }
        }
    }
}
