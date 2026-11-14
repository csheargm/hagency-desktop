use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    // The base Rinx button widget.
    // Uses COLOR_ACTIVE_PRIMARY (blue) background with white text by default.
    // See also the preset variants below:
    //   RobrixPositiveIconButton, RobrixNegativeIconButton, RobrixNeutralIconButton.
    mod.widgets.RobrixIconButton = Button {
        width: Fit,
        height: Fit,
        spacing: 10,
        padding: 10,
        margin: 0,
        align: Align{x: 0.5, y: 0.5}
        // A click takes no key focus (as RinxButton). The focus colors below are one
        // pale face and dark text for every variant, and the icon has no focus color,
        // so a clicked button used to stay a white icon on a pale face.
        grab_key_focus: false

        animator +: {
            focus: {
                default: @off
                off: AnimatorState {from: {all: Snap} apply: {draw_bg: {focus: 0.0} draw_text: {focus: 0.0}}}
                on: AnimatorState {from: {all: Snap} apply: {draw_bg: {focus: 1.0} draw_text: {focus: 1.0}}}
            }
        }

        draw_bg +: {
            border_size: 0.0
            border_radius: 4.0

            color: (COLOR_ACTIVE_PRIMARY)
            color_hover: (COLOR_ACTIVE_PRIMARY_DARKER)
            color_down: mod.widgets.RINX_ACCENT_DOWN
            color_focus: mod.widgets.RINX_SELECTED
            color_disabled: (COLOR_BG_DISABLED)

            border_color: #0000
            border_color_hover: #0000
            border_color_down: #0000
            border_color_focus: #0000
            border_color_disabled: #0000

            // Disable gradient (color_2) by default
            color_2_hover: vec4(-1.0, -1.0, -1.0, -1.0)
            color_2_down: vec4(-1.0, -1.0, -1.0, -1.0)
            color_2_focus: vec4(-1.0, -1.0, -1.0, -1.0)
            color_2: vec4(-1.0, -1.0, -1.0, -1.0)
            border_color_2: vec4(-1.0, -1.0, -1.0, -1.0)
        }

        draw_icon.color: (COLOR_PRIMARY)
        icon_walk: Walk{width: 16, height: 16}

        draw_text +: {
            color: (COLOR_PRIMARY)
            color_hover: (COLOR_PRIMARY)
            color_down: (COLOR_PRIMARY)
            color_focus: mod.widgets.RINX_INK
            color_disabled: (COLOR_FG_DISABLED)
            text_style: mod.widgets.REGULAR_TEXT {font_size: (10 * mod.widgets.RINX_TEXT_SCALE)},
        }
        text: ""
    }

    // Green button for positive/accept actions: joining a room, confirming, accepting an invite.
    mod.widgets.RobrixPositiveIconButton = mod.widgets.RobrixIconButton {
        draw_bg +: {
            border_color: (COLOR_FG_ACCEPT_GREEN)
            border_color_hover: (COLOR_FG_ACCEPT_GREEN)
            border_color_down: (COLOR_FG_ACCEPT_GREEN)
            color: (COLOR_BG_ACCEPT_GREEN)
            color_hover: mod.widgets.RINX_SUCCESS_BG
            color_down: mod.widgets.RINX_SUCCESS_BG
        }
        draw_icon.color: (COLOR_FG_ACCEPT_GREEN)
        draw_text +: {
            color: (COLOR_FG_ACCEPT_GREEN)
            color_hover: (COLOR_FG_ACCEPT_GREEN)
            color_down: (COLOR_FG_ACCEPT_GREEN)
        }
    }

    // Red button for negative/dangerous actions: rejecting, leaving, deleting, blocking.
    mod.widgets.RobrixNegativeIconButton = mod.widgets.RobrixIconButton {
        draw_bg +: {
            border_color: (COLOR_FG_DANGER_RED)
            border_color_hover: (COLOR_FG_DANGER_RED)
            border_color_down: (COLOR_FG_DANGER_RED)
            color: (COLOR_BG_DANGER_RED)
            color_hover: mod.widgets.RINX_DANGER_BG
            color_down: mod.widgets.RINX_DANGER_BG
        }
        draw_icon.color: (COLOR_FG_DANGER_RED)
        draw_text +: {
            color: (COLOR_FG_DANGER_RED)
            color_hover: (COLOR_FG_DANGER_RED)
            color_down: (COLOR_FG_DANGER_RED)
        }
    }

    // Gray button for cancel/dismiss actions: canceling, closing, going back.
    mod.widgets.RobrixNeutralIconButton = mod.widgets.RobrixIconButton {
        draw_bg +: {
            border_color: (COLOR_BG_DISABLED)
            border_color_hover: (COLOR_BG_DISABLED)
            border_color_down: (COLOR_BG_DISABLED)
            color: (COLOR_SECONDARY)
            color_hover: mod.widgets.RINX_PRESSED
            color_down: mod.widgets.RINX_PRESSED
        }
        draw_icon.color: (COLOR_TEXT)
        draw_text +: {
            color: (COLOR_TEXT)
            color_hover: (COLOR_TEXT)
            color_down: (COLOR_TEXT)
        }
    }

    mod.widgets.RinxBackButton = mod.widgets.RobrixNeutralIconButton {
        text: #(crate::i18n::tr("Back")) i18n_text: "Back"
        height: 40 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} spacing: 6
        icon_walk: Walk{width: 16 height: 16} draw_icon.svg: ICON_ARROW_BACK
    }

    mod.widgets.RinxChatInfoButton = mod.widgets.RobrixNeutralIconButton {
        width: 44 height: 44 padding: 0 spacing: 0
        align: Align{x: 0.5 y: 0.5}
        text: "···"
        draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold {font_size: (18 * mod.widgets.RINX_TEXT_SCALE)}}
        draw_bg +: {color: #0000 color_hover: mod.widgets.RINX_HOVER color_down: mod.widgets.RINX_PRESSED border_size: 0}
        icon_walk: Walk{width: 0 height: 0}
    }

    mod.widgets.RinxRoomInfoToggleButton = mod.widgets.NavigationBarButton {
        tooltip_below: true
        width: 36 height: 36 padding: 0 spacing: 0
        align: Align{x: 0.5 y: 0.5}
        tooltip_text: #(crate::i18n::tr("Show room information"))
        icon := Icon {icon_walk: Walk{width: 20 height: 20} draw_icon +: {svg: ICON_PANEL_RIGHT color: mod.widgets.RINX_INK}}
        draw_bg +: {color_hover: mod.widgets.RINX_HOVER color_active: mod.widgets.RINX_SELECTED border_size: 0}
    }
}
