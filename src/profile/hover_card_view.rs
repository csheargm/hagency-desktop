//! The hover card's layout (see [`super::hover_card`]): avatar, name, a
//! small Matrix ID with Copy, the Agent tag, detail lines and the buttons.

use makepad_widgets::*;
use super::hover_card::AvatarHoverCard;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.AvatarHoverCard = #(AvatarHoverCard::register_widget(vm)) {
        width: Fill, height: Fill, flow: Overlay, align: Align{x: 0.0, y: 0.0}
        popup_frame := View {
            width: 280, height: Fit
            card := RoundedView {
                width: Fill, height: Fit, flow: Down, spacing: 10, padding: 12
                show_bg: true
                draw_bg +: {
                    color: (mod.widgets.RBX_BG_SURFACE)
                    border_radius: (mod.widgets.RBX_RADIUS_SM)
                    border_size: 1.0
                    border_color: (mod.widgets.RBX_STROKE_STRONG)
                }
                View {
                    width: Fill, height: Fit, flow: Right, spacing: 10, align: Align{y: 0.5}
                    card_avatar := Avatar { width: 40, height: 40 }
                    View {
                        width: Fill, height: Fit, flow: Down, spacing: 3
                        View {
                            width: Fill, height: Fit, flow: Right, spacing: 6, align: Align{y: 0.5}
                            presence_dot := RoundedView {
                                visible: false, width: 8, height: 8
                                draw_bg +: {color: #x2e9e5b border_radius: 4.0}
                            }
                            card_name := Label {
                                width: Fill, height: Fit, max_lines: 1, text_overflow: Ellipsis
                                draw_text +: {color: (mod.widgets.RBX_FG_PRIMARY) text_style: (mod.widgets.RBX_TEXT_BODY_STRONG)}
                                text: ""
                            }
                        }
                        // The raw Matrix ID is secondary: small, with Copy.
                        View {
                            width: Fill, height: Fit, flow: Right, spacing: 4, align: Align{y: 0.5}
                            card_user_id := Label {
                                width: Fill, height: Fit, max_lines: 1, text_overflow: Ellipsis
                                draw_text +: {color: (mod.widgets.RBX_FG_SECONDARY) text_style: theme.font_regular{font_size: (8.5 * mod.widgets.RINX_TEXT_SCALE)}}
                                text: ""
                            }
                            copy_id_button := RobrixNeutralIconButton {
                                width: 20, height: 20, margin: 0, padding: 3, spacing: 0
                                draw_icon.svg: mod.widgets.ICON_COPY
                                icon_walk: Walk{width: 12 height: 12}
                                text: ""
                            }
                        }
                        agent_tag := RoundedView {
                            visible: false, width: Fit, height: Fit
                            padding: Inset{left: 6 right: 6 top: 2 bottom: 2}
                            draw_bg +: {color: mod.widgets.RINX_SELECTED border_radius: 4}
                            Label {
                                width: Fit, height: Fit
                                draw_text +: {color: mod.widgets.RINX_ACCENT text_style: theme.font_regular{font_size: (9.5 * mod.widgets.RINX_TEXT_SCALE)}}
                                text: #(crate::i18n::tr("Agent")) i18n_text: "Agent"
                            }
                        }
                    }
                }
                card_details := Label {
                    width: Fill, height: Fit, flow: Flow.Right{wrap: true}
                    draw_text +: {color: (mod.widgets.RBX_FG_SECONDARY) text_style: (mod.widgets.RBX_TEXT_META)}
                    text: ""
                }
                View {
                    width: Fill, height: Fit, flow: Right, spacing: 8
                    view_profile_button := RobrixNeutralIconButton {
                        width: Fill, height: 32, margin: 0, align: Align{x: 0.5 y: 0.5}
                        icon_walk: Walk{width: 0 height: 0}
                        text: #(crate::i18n::tr("View profile")) i18n_text: "View profile"
                    }
                    // This account's own Agents: opens the agent panel instead.
                    agent_details_button := RobrixNeutralIconButton {
                        visible: false, width: Fill, height: 32, margin: 0, align: Align{x: 0.5 y: 0.5}
                        icon_walk: Walk{width: 0 height: 0}
                        text: #(crate::i18n::tr("Agent details")) i18n_text: "Agent details"
                    }
                    message_button := RobrixIconButton {
                        width: Fill, height: 32, margin: 0, spacing: 6, align: Align{x: 0.5 y: 0.5}
                        draw_icon.svg: mod.widgets.ICON_DOUBLE_CHAT
                        icon_walk: Walk{width: 14 height: 14}
                        text: #(crate::i18n::tr("Message")) i18n_text: "Message"
                    }
                }
            }
        }
    }
}

