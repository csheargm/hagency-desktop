//! DSL for the navigation rail: the button styles and the Desktop/Mobile layouts.
//! Split out of `navigation_tab_bar.rs` (R4) with no behavior change.

use makepad_widgets::*;
use super::NavigationTabBar;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*


    // The base style definition for icon buttons in the NavigationTabBar.
    //
    // Dark navy nav-rail item (robrix2 visual spec §5.6 / RBX_NAV_* tokens):
    // transparent when idle so the navy rail shows through, a navy "pill" on
    // hover/active, plus a teal accent bar on the left edge of the *active* item
    // to echo the app-wide teal selection language. The icon itself is recolored
    // white when selected via the animator below (DrawSvg has no per-state color).
    mod.widgets.NavigationTabButton = mod.widgets.NavigationBarButton {
        width: Fill,
        // Normally (NAVIGATION_TAB_BAR_SIZE - 12) tall and never taller. In a short
        // window every rail button gives up height evenly, down to a 44-point
        // target, before the bottom buttons would be pushed off the rail.
        height: Fill{weight: 0.0 basis: FitBound.Abs((NAVIGATION_TAB_BAR_SIZE - 12)) shrink: 1.0},
        min_height: FitBound.Abs(44.0),
        padding: (SPACE_XS),
        margin: Inset{top: 2, bottom: 2, left: (SPACE_XS), right: (SPACE_XS)},
        align: Align{x: 0.5, y: 0.5}
        flow: Down,

        draw_bg +: {
            color_hover: (RBX_NAV_ITEM_HOVER_BG)
            color_active: (RBX_NAV_ITEM_ACTIVE_BG)
            accent_color: instance((RBX_ACCENT))
            border_radius: (RBX_RADIUS_SM)

            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(
                    self.border_inset.x + self.border_size,
                    self.border_inset.y + self.border_size,
                    self.rect_size.x - (self.border_inset.x + self.border_inset.z + self.border_size * 2.0),
                    self.rect_size.y - (self.border_inset.y + self.border_inset.w + self.border_size * 2.0),
                    max(1.0, self.border_radius)
                )
                // `fill_keep` leaves the pill in the sdf shape, and `box` unions with it,
                // so the accent bar below would flood the whole pill. `fill`/`stroke` reset it.
                if self.border_size > 0.0 {
                    sdf.fill_keep(self.get_color())
                    sdf.stroke(self.border_color, self.border_size)
                } else {
                    sdf.fill(self.get_color())
                }
                // Teal selection bar on the left edge, shown only when active.
                let bar_inset = 12.0
                sdf.box(
                    0.0,
                    bar_inset,
                    3.0,
                    self.rect_size.y - bar_inset * 2.0,
                    1.5
                )
                sdf.fill(mix(vec4(0.0, 0.0, 0.0, 0.0), self.accent_color, self.active))
                return sdf.result;
            }
        }

        icon := Icon {
            margin: 0,
            icon_walk: Walk {
                margin: 0,
                width: (RBX_ICON_LG),
                height: (RBX_ICON_LG)
            }
            draw_icon +: {
                color: (RBX_NAV_FG)
            }
        }

        // Same hover/active tracks as the base NavigationBarButton, plus the
        // icon recolor: white (RBX_NAV_FG_ACTIVE) when selected, muted
        // RBX_NAV_FG otherwise.
        animator: Animator {
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: 0.15}}
                    apply: { draw_bg: {hover: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {hover: 1.0} }
                }
                down: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {hover: 1.0} }
                }
            }
            active: {
                default: @off
                off: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {active: 0.0} icon: { draw_icon: { color: (RBX_NAV_FG) } } }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {active: 1.0} icon: { draw_icon: { color: (RBX_NAV_FG_ACTIVE) } } }
                }
            }
        }
    }


    // A rail button with a short caption under its icon (Chats, Agents, More).
    mod.widgets.RailLabeledButton = mod.widgets.NavigationTabButton {
        spacing: 2
        label := Label {
            draw_text +: {color: RBX_NAV_FG text_style: theme.font_regular{font_size: (9 * mod.widgets.RINX_TEXT_SCALE)}}
        }
    }

    mod.widgets.HomeButton = mod.widgets.RailLabeledButton {
        tooltip_text: #(crate::i18n::tr("Chats"))
        icon +: {
            icon_walk: Walk{width: 22 height: 22}
            draw_icon +: { svg: crate_resource("self://resources/icons/double_chat.svg") }
        }
        label.text: #(crate::i18n::tr("Chats")) label.i18n_text: "Chats"
    }

    // The bottom rail item that opens the account menu (ported from robrix2). A plain
    // icon button, NOT a second ProfileIcon: the menu is an action, not the user's
    // avatar, and this keeps the rail's icon language consistent with Home and the tools.
    // The top avatar keeps opening Settings directly.
    mod.widgets.AccountSwitcherButton = mod.widgets.NavigationTabButton {
        tooltip_text: #(crate::i18n::tr("Account"))
        icon +: {
            draw_icon +: { svg: (mod.widgets.ICON_PEOPLE) }
        }
    }


    mod.widgets.Separator = LineH {
        margin: Inset{top: (SPACE_SM), bottom: (SPACE_SM), left: (SPACE_MD), right: (SPACE_MD)}
        draw_bg.color: (RBX_NAV_DIVIDER)
    }

    mod.widgets.MobileTabButton = NavigationBarButton {
        width: Fill height: 56 flow: Down spacing: 4
        padding: Inset{top: 7 bottom: 4} align: Align{x: 0.5 y: 0.5}
        draw_bg +: {color_hover: #x00000000 color_active: #x00000000 border_radius: 0}
        icon := Icon {
            icon_walk: Walk{width: 24 height: 24}
            draw_icon.color: mod.widgets.RINX_INK
        }
        label := Label {
            draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular {font_size: (8 * mod.widgets.RINX_TEXT_SCALE)}}
        }
    }

    mod.widgets.NavigationTabBar = #(NavigationTabBar::register_widget(vm)) {
        // Dark navy anchor rail (robrix2 visual spec §2/§5.6). SolidView fills its
        // column edge-to-edge (no rounded-SDF anti-aliased border), so the navy is
        // perfectly flush to the window's left edge AND to the rooms list.
        Desktop := SolidView {
            new_batch: true,
            flow: Down,
            align: Align{x: 0.5}
            padding: Inset{
                top: (SPACE_SM),
                bottom: (SPACE_SM + mod.widgets.SAFE_INSET_PAD_BOTTOM),
                left: (mod.widgets.SAFE_INSET_PAD_LEFT),
            }
            width: (mod.widgets.NAVIGATION_TAB_BAR_SIZE + mod.widgets.SAFE_INSET_PAD_LEFT),
            height: Fill

            show_bg: true
            draw_bg.color: (RBX_NAV_BG)

            // Two primary items, Chats and Agents (the Buzz-style rail, UX item A1).
            CachedWidget {
                home_button := mod.widgets.HomeButton {}
            }
            agent_button := mod.widgets.RailLabeledButton {
                tooltip_text: #(crate::i18n::tr("Agents"))
                icon +: {icon_walk: Walk{width: 22 height: 22} draw_icon.svg: ICON_ROBOT}
                label.text: #(crate::i18n::tr("Agents")) label.i18n_text: "Agents"
            }

            // Empty while "More" is closed, so the bottom group stays pinned down.
            rail_spacer := View {width: Fill height: Fill}

            // Everything else, one click away behind "More": the spaces and the
            // secondary tools, right above the button. They are direct rail children
            // (not one nested group) so short windows shrink them with the rest.
            top_separator := mod.widgets.Separator {visible: false}
            more_spaces := View {
                visible: false width: Fill height: Fill
                CachedWidget {
                    root_spaces_bar := mod.widgets.SpacesBar {}
                }
            }
            contacts_button := mod.widgets.NavigationTabButton {
                visible: false
                tooltip_text: #(crate::i18n::tr("Contacts"))
                icon.draw_icon.svg: ICON_PEOPLE
            }
            moments_button := mod.widgets.NavigationTabButton {
                visible: false
                tooltip_text: #(crate::i18n::tr("Moments"))
                icon.draw_icon.svg: ICON_GLOBE
            }
            octoscript_apps_button := mod.widgets.NavigationTabButton {
                visible: false
                tooltip_text: #(crate::i18n::tr("Mini apps"))
                icon.draw_icon.svg: ICON_SQUARES
            }
            // The one desktop entry to File Transfer (mobile: Discover).
            file_transfer_button := mod.widgets.NavigationTabButton {
                visible: false
                tooltip_text: #(crate::i18n::tr("File Transfer"))
                icon.draw_icon.svg: ICON_FILE
            }

            more_button := mod.widgets.RailLabeledButton {
                tooltip_text: #(crate::i18n::tr("More"))
                icon +: {draw_icon.svg: ICON_MORE_VERT}
                label.text: #(crate::i18n::tr("More")) label.i18n_text: "More"
            }

            bottom_separator := mod.widgets.Separator {}

            // Settings (the avatar) and the account menu stay at the bottom.
            CachedWidget {
                profile_icon := mod.widgets.ProfileIcon {}
            }
            CachedWidget {
                account_switcher_button := mod.widgets.AccountSwitcherButton {}
            }
        }

        Mobile := SolidView {
            new_batch: true flow: Right align: Align{y: 0.5}
            width: Fill height: (56 + mod.widgets.SAFE_INSET_PAD_BOTTOM)
            padding: Inset{bottom: (mod.widgets.SAFE_INSET_PAD_BOTTOM)}
            draw_bg.color: mod.widgets.RINX_PAGE
            chats_tab := mod.widgets.MobileTabButton {
                icon.draw_icon.svg: crate_resource("self://resources/icons/double_chat.svg")
                label.text: #(crate::i18n::tr("Chats")) label.i18n_text: "Chats"
            }
            contacts_tab := mod.widgets.MobileTabButton {icon.draw_icon.svg: ICON_PEOPLE label.text: #(crate::i18n::tr("Contacts")) label.i18n_text: "Contacts"}
            discover_tab := mod.widgets.MobileTabButton {icon.draw_icon.svg: ICON_GLOBE label.text: #(crate::i18n::tr("Discover")) label.i18n_text: "Discover"}
            me_tab := mod.widgets.MobileTabButton {icon.draw_icon.svg: crate_resource("self://resources/icons/person.svg") label.text: #(crate::i18n::tr("Me")) label.i18n_text: "Me"}
        }
    }
}
