//! `OwnerDashboard`: the hosting dashboard widget (ADR 0012, ADR-192).
//!
//! The widget renders [`DashboardState`] and emits [`DashboardAction`]s. It
//! never performs IO. While the host reports that hosting must be set up, a
//! setup page replaces the dashboard. Updates are applied in place: labels, visibility and
//! keyed rows change, but no page or row is remounted, so refreshes keep
//! scroll, focus and unaffected bounds.
//!
//! Layout adapts to the widget's own width (not the window): below
//! [`WIDE_MIN`] cards stack in one column with 16-point gutters; at or above
//! it the Overview uses two columns with 24-point gutters.
use crate::controls::{
    OdGlyphWidgetExt, OdGlyphWidgetRefExt, OdListWidgetExt, OdListWidgetRefExt, OdMeterWidgetRefExt,
};
use crate::format::Clock;
use crate::picker::{OdFilterPickerWidgetExt, OdFilterPickerWidgetRefExt};
use crate::i18n::{Locale, Msg, tr};
use crate::model::{
    BannerAction, CommandKind, CommandPhase, CommandResult, ConnectionStatus, DashboardAction, DashboardState, Glyph,
    HostOptions, Presentation, ResourceRow, SetupView, SnapshotOutcome, Status, Tab, Tone, UsageCard, ViewState,
};
use hagency_metering::dashboard::{DashboardSnapshot, period::UsagePeriod};
use makepad_widgets::*;

/// Content width at which the Overview switches to two columns.
pub const WIDE_MIN: f64 = 720.0;
/// Rows drawn per list. Larger lists are bounded to keep frames responsive.
pub const PAGE_ROWS: usize = 20;
const AGE_REFRESH_SECONDS: f64 = 30.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeaderTarget { None, Notice, Command, Connection, Inventory, Refresh }
struct HeaderStatus { status: Status, target: HeaderTarget, #[cfg(feature = "native-accessibility")] detail: Option<String> }

fn compact_navigation(width: f64, scale: f64) -> bool {
    // Four translated labels need more than four equal 90-point segments
    // once the host or accessibility typography is enlarged.
    scale > 1.3 || width < 360.0 * scale.max(1.0)
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.OdText = Label {
        width: Fill height: Fit padding: 0 flow: Flow.Right{wrap: true}
        draw_text +: {color: OD_INK text_style: theme.font_regular{font_size: OD_SIZE_BODY}}
    }
    mod.widgets.OdCaption = mod.widgets.OdText {draw_text +: {color: OD_MUTED text_style: theme.font_regular{font_size: OD_SIZE_CAPTION}}}
    mod.widgets.OdHeadline = mod.widgets.OdText {draw_text +: {text_style: theme.font_bold{font_size: OD_SIZE_HEAD}}}
    mod.widgets.OdTitle = mod.widgets.OdText {draw_text +: {text_style: theme.font_bold{font_size: OD_SIZE_TITLE}}}
    mod.widgets.OdLargeTitle = mod.widgets.OdText {draw_text +: {text_style: theme.font_bold{font_size: OD_SIZE_LARGE}}}
    mod.widgets.OdMetric = mod.widgets.OdText {draw_text +: {text_style: theme.font_bold{font_size: OD_SIZE_METRIC}}}
    mod.widgets.OdSectionHeader = mod.widgets.OdText {
        margin: Inset{left: 4 top: 4}
        draw_text +: {color: OD_MUTED text_style: theme.font_bold{font_size: OD_SIZE_CAPTION}}
    }
    mod.widgets.OdNumber = mod.widgets.OdText {width: Fit flow: Right draw_text +: {text_style: theme.font_bold{font_size: OD_SIZE_BODY}}}
    mod.widgets.OdScrollY = ScrollYView {
        scroll_bars +: {scroll_bar_y +: {bar_size: 8 bar_side_margin: 1 draw_bg +: {size: 4 border_size: 0 border_radius: 2 color: OD_MUTED color_hover: OD_INK color_drag: OD_INK}}}
    }

    mod.widgets.OdCard = RoundedView {
        width: Fill height: Fit flow: Down spacing: 10
        padding: Inset{left: 16 right: 16 top: 14 bottom: 14}
        draw_bg +: {color: OD_SURFACE border_color: OD_SEPARATOR border_size: 0.0 border_radius: OD_RADIUS}
    }
    mod.widgets.OdSeparator = SolidView {width: Fill height: 1 draw_bg.color: OD_SEPARATOR}

    mod.widgets.OdButton = Button {
        width: Fit height: OD_CONTROL margin: 0
        padding: Inset{left: 16 right: 16 top: 0 bottom: 0}
        grab_key_focus: false
        animator.hover.off.from.all.duration: OD_MOTION
        animator.hover.on.from.all.duration: OD_MOTION
        draw_bg +: {
            color: OD_FIELD color_hover: OD_HOVER color_down: OD_PRESSED color_focus: OD_FIELD color_disabled: OD_FIELD
            color_2: vec4(-1., -1., -1., -1.)
            border_size: 1.0 border_radius: OD_RADIUS_SMALL
            border_color: OD_FIELD border_color_hover: OD_HOVER border_color_down: OD_PRESSED
            border_color_focus: OD_ACCENT border_color_disabled: OD_FIELD
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, self.border_radius)
                sdf.fill_keep(self.color.mix(self.color_hover, self.hover).mix(self.color_down, self.down).mix(self.color_disabled, self.disabled))
                sdf.stroke(self.border_color_focus, 2.0 * self.focus * (1.0 - self.disabled))
                return sdf.result
            }
        }
        draw_text +: {
            color: OD_ACCENT color_hover: OD_ACCENT color_down: OD_ACCENT color_focus: OD_ACCENT color_disabled: OD_MUTED
            text_style: theme.font_bold{font_size: OD_SIZE_BODY}
        }
    }
    // Keep the native DropDown behavior, with a disclosure painted after the
    // face. The framework's default arrow is overpainted by its opaque fill.
    mod.widgets.OdDropDown = DropDown {
        margin: 0
        padding: Inset{left: 14 right: 40 top: 0 bottom: 0}
        draw_bg +: {
            color: OD_SURFACE color_hover: OD_HOVER color_down: OD_PRESSED color_disabled: OD_FIELD
            border_color: OD_SEPARATOR border_color_focus: OD_ACCENT border_radius: OD_RADIUS_SMALL
            arrow_color: OD_INK arrow_color_disabled: OD_MUTED
            arrow_size: uniform(4.0 * OD_TEXT_SCALE)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, self.border_radius)
                sdf.fill_keep(self.color.mix(self.color_hover, self.hover).mix(self.color_down, self.down).mix(self.color_disabled, self.disabled))
                sdf.stroke(self.border_color.mix(self.border_color_focus, self.focus), 1.0 + self.focus)
                let c = vec2(self.rect_size.x - 20.0, self.rect_size.y * 0.5)
                sdf.move_to(c.x - self.arrow_size, c.y - self.arrow_size * 0.4)
                sdf.line_to(c.x, c.y + self.arrow_size * 0.4)
                sdf.line_to(c.x + self.arrow_size, c.y - self.arrow_size * 0.4)
                sdf.stroke(self.arrow_color.mix(self.arrow_color_disabled, self.disabled), 2.0)
                return sdf.result
            }
        }
        draw_text +: {color: OD_INK color_hover: OD_INK color_focus: OD_INK color_down: OD_INK color_disabled: OD_MUTED}
    }
    mod.widgets.OdPrimaryButton = mod.widgets.OdButton {
        draw_bg +: {
            color: OD_ACCENT_FILL color_hover: OD_ACCENT_HOVER color_down: OD_ACCENT_DOWN color_focus: OD_ACCENT_FILL color_disabled: OD_TRACK
            border_color: OD_ACCENT_FILL border_color_hover: OD_ACCENT_HOVER border_color_down: OD_ACCENT_DOWN
            border_color_focus: OD_ON_ACCENT border_color_disabled: OD_TRACK
        }
        draw_text +: {color: OD_ON_ACCENT color_hover: OD_ON_ACCENT color_down: OD_ON_ACCENT color_focus: OD_ON_ACCENT color_disabled: OD_MUTED}
    }
    mod.widgets.OdDestructiveButton = mod.widgets.OdButton {
        draw_text +: {color: OD_CRI_FG color_hover: OD_CRI_FG color_down: OD_CRI_FG color_focus: OD_CRI_FG}
    }
    // An iOS-style switch on the platform CheckBox, so its state, keyboard
    // activation and native accessibility role are the toggle's own.
    mod.widgets.OdSwitch = ToggleFlat {
        width: 56 height: OD_CONTROL text: "" padding: 0 margin: 0
        label_walk: Walk{width: 0 height: 0 margin: Inset{}}
        draw_bg +: {
            color: OD_TRACK color_active: OD_POS_FG
            knob_color: uniform(#fff)
            ring_color: uniform(OD_ACCENT)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let p = (self.rect_size - vec2(46.0, 28.0)) * 0.5
                sdf.box(p.x, p.y, 46.0, 28.0, 14.0)
                sdf.fill_keep(self.color.mix(self.color_active, self.active))
                sdf.stroke(self.ring_color, 2.0 * self.focus)
                sdf.circle(p.x + 14.0 + 18.0 * self.active, p.y + 14.0, 11.5)
                sdf.fill(self.knob_color)
                return sdf.result * (1.0 - 0.5 * self.disabled)
            }
        }
    }
    mod.widgets.OdDestructivePrimaryButton = mod.widgets.OdButton {
        draw_bg +: {
            color: OD_CRI_FG color_hover: OD_CRI_FG color_down: OD_CRI_FG color_focus: OD_CRI_FG
            border_color: OD_CRI_FG border_color_hover: OD_CRI_FG border_color_down: OD_CRI_FG border_color_focus: OD_INK
        }
        draw_text +: {color: OD_SURFACE color_hover: OD_SURFACE color_down: OD_SURFACE color_focus: OD_SURFACE}
    }

    // A segment: a selection plate behind a transparent button. Selection is
    // a visibility toggle, so it survives theme reapply without restyling.
    let Pager = View {
        visible: false width: Fill height: Fit flow: Down spacing: 8
        count := mod.widgets.OdCaption {}
        View {width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 8
            previous := mod.widgets.OdButton {text: "Previous"}
            next := mod.widgets.OdButton {text: "Next"}
        }
    }

    let Segment = View {
        width: Fill height: OD_CONTROL flow: Overlay
        sel := RoundedView {
            visible: false width: Fill height: Fill
            draw_bg +: {color: OD_SURFACE border_color: OD_SEPARATOR border_size: 0.0 border_radius: OD_RADIUS_SMALL}
        }
        button := mod.widgets.OdButton {
            width: Fill height: Fill padding: Inset{left: 6 right: 6}
            draw_bg +: {color: #0000 color_focus: #0000 color_disabled: #0000 border_color: #0000 border_color_disabled: #0000}
            draw_text +: {color: OD_INK color_hover: OD_INK color_down: OD_INK color_focus: OD_INK}
        }
    }
    let SegmentTrack = RoundedView {
        width: Fill height: Fit flow: Right spacing: 2 padding: 2
        draw_bg +: {color: OD_TRACK border_radius: OD_RADIUS_SMALL}
    }
    let Chip = View {
        width: Fit height: OD_CONTROL flow: Overlay
        sel := RoundedView {
            visible: false width: Fill height: Fill
            draw_bg +: {color: OD_SELECTED border_color: OD_ACCENT border_size: 1.0 border_radius: 18.0}
        }
        chip := mod.widgets.OdButton {
            height: Fill padding: Inset{left: 14 right: 14}
            draw_bg +: {border_radius: 18.0}
            draw_text +: {text_style: theme.font_regular{font_size: OD_SIZE_CAPTION}}
        }
    }
    let StatusRow = View {
        width: Fill height: Fit flow: Right spacing: 10 align: Align{y: 0.0}
        glyph := OdGlyph {}
        text := mod.widgets.OdText {}
    }
    let Badge = RoundedView {
        width: Fit height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
        padding: Inset{left: 6 right: 10 top: 4 bottom: 4}
        draw_bg +: {color: OD_FIELD border_radius: 12.0}
        glyph := OdGlyph {width: 16 height: 16}
        text := mod.widgets.OdCaption {width: Fit flow: Right}
    }
    let KeyValue = View {
        width: Fill height: Fit flow: Right spacing: 12 align: Align{y: 0.5} padding: Inset{top: 6 bottom: 6}
        key := mod.widgets.OdText {}
        value := mod.widgets.OdNumber {flow: Flow.Right{wrap: true}}
    }
    let TokenCell = View {
        width: 150 height: Fit flow: Down spacing: 2 padding: Inset{top: 4 bottom: 4}
        label := mod.widgets.OdCaption {}
        value := mod.widgets.OdNumber {}
    }
    let ResourceRow = View {
        width: Fill height: Fit flow: Right spacing: 12 align: Align{y: 0.5} padding: Inset{top: 8 bottom: 8}
        resource_meta := View {
            width: Fill height: Fit flow: Down spacing: 3
            name := mod.widgets.OdHeadline {}
            line := mod.widgets.OdCaption {}
            View {
                width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 8 align: Align{y: 0.5}
                badge := Badge {}
            }
            budget := mod.widgets.OdCaption {}
        }
        resource_actions := View {
            width: Fit height: Fit flow: Right spacing: 12 align: Align{y: 0.5}
            tokens := mod.widgets.OdNumber {height: OD_CONTROL align: Align{y: 0.5} draw_text.ink_centered: true}
            od_resource_open := mod.widgets.OdButton {text: "Details"}
        }
    }
    let UsageTotals = View {
        width: Fill height: Fit flow: Down spacing: 8
        title := mod.widgets.OdHeadline {}
        View {
            width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 8 align: Align{y: 1.0}
            total := mod.widgets.OdMetric {width: Fit flow: Right}
            total_label := mod.widgets.OdCaption {width: Fit flow: Right margin: Inset{bottom: 3}}
        }
        cells := View {
            width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 4
            input := TokenCell {}
            output := TokenCell {}
            cache_read := TokenCell {}
            cache_write := TokenCell {}
            ratio := TokenCell {}
        }
        completeness := StatusRow {}
        observed := mod.widgets.OdCaption {}
    }

    mod.widgets.OwnerDashboardBase = #(OwnerDashboard::register_widget(vm))
    mod.widgets.OwnerDashboard = set_type_default() do mod.widgets.OwnerDashboardBase {
        width: Fill height: Fill flow: Down
        show_bg: true
        draw_bg +: {color: OD_PAGE}

        od_header := View {
            width: Fill height: Fit flow: Down spacing: 10
            padding: Inset{left: 16 right: 16 top: 14 bottom: 10}
            od_owner_menu := mod.widgets.OdButton {
                visible: false width: Fill height: OD_COMPACT_CONTROL
                draw_text +: {text_style: theme.font_bold{font_size: OD_SIZE_CAPTION}}
            }
            od_identity_actions := View {
                width: Fill height: Fit flow: Down spacing: 8
                View {
                    width: Fill height: Fit flow: Down spacing: 2
                    od_title := mod.widgets.OdLargeTitle {text: "Hosting"}
                    od_owner := mod.widgets.OdCaption {}
                }
                View {
                    width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 8 align: Align{y: 0.5}
                    od_refresh := mod.widgets.OdButton {text: "Refresh"}
                }
            }
            od_freshness := View {
                width: Fill height: OD_COMPACT_CONTROL flow: Right spacing: 6 align: Align{y: 0.5}
                od_sync_spinner := LoadingSpinner {visible: false width: 12 height: 12 draw_bg +: {color: OD_MUTED stroke_width: 1.5}}
                od_sync_glyph := OdGlyph {visible: false width: 14 height: 14}
                od_sync := mod.widgets.OdCaption {max_lines: 1 text_overflow: Ellipsis}
                od_status_details := mod.widgets.OdButton {
                    visible: false height: OD_COMPACT_CONTROL padding: Inset{left: 10 right: 10}
                    draw_text +: {text_style: theme.font_bold{font_size: OD_SIZE_CAPTION}}
                }
            }
            od_destination_row := View {
                visible: false width: Fill height: Fit
                od_destination := mod.widgets.OdDropDown {
                    width: Fill height: OD_CONTROL
                    draw_text +: {text_style: theme.font_bold{font_size: OD_SIZE_BODY}}
                    popup_menu: PopupMenu {
                        width: 300
                        menu_item: PopupMenuItem {
                            height: OD_CONTROL
                            draw_text +: {text_style: theme.font_regular{font_size: OD_SIZE_BODY}}
                        }
                    }
                }
            }
            od_tabs := SegmentTrack {
                od_tab_overview := Segment {}
                od_tab_resources := Segment {}
                od_tab_usage := Segment {}
                od_tab_activity := Segment {}
            }
        }

        od_content := mod.widgets.OdScrollY {
            width: Fill height: Fill flow: Down spacing: 16
            padding: Inset{left: 16 right: 16 top: 4 bottom: 28}

            od_scroll_origin := View {width: Fill height: 0 margin: Inset{bottom: -16}}
            od_setup_page := mod.widgets.OdCard {
                visible: false align: Align{x: 0.0}
                View {
                    width: Fill height: Fit flow: Right spacing: 12 align: Align{y: 0.0}
                    od_setup_glyph := OdGlyph {width: 34 height: 34}
                    View {
                        width: Fill height: Fit flow: Down spacing: 4
                        od_setup_title := mod.widgets.OdTitle {}
                        od_setup_body := mod.widgets.OdCaption {}
                    }
                }
                od_setup_progress := View {
                    visible: false width: Fill height: Fit flow: Right spacing: 10 align: Align{y: 0.5}
                    od_setup_spinner := LoadingSpinner {width: 18 height: 18 draw_bg +: {color: OD_ACCENT stroke_width: 2.0}}
                    od_setup_progress_glyph := OdGlyph {visible: false width: 18 height: 18}
                    od_setup_progress_text := mod.widgets.OdText {}
                }
                View {
                    width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 8
                    od_setup_start := mod.widgets.OdPrimaryButton {text: "Set up hosting"}
                    od_setup_cancel := mod.widgets.OdButton {text: "Cancel"}
                }
            }
            od_empty_page := mod.widgets.OdCard {
                visible: false align: Align{x: 0.0}
                View {
                    width: Fill height: Fit flow: Right spacing: 10 align: Align{y: 0.5}
                    od_empty_spinner := LoadingSpinner {width: 18 height: 18 draw_bg +: {color: OD_ACCENT stroke_width: 2.0}}
                    od_empty_glyph := OdGlyph {visible: false}
                    od_empty_title := mod.widgets.OdHeadline {}
                }
                od_empty_body := mod.widgets.OdCaption {}
            }

            od_overview := View {
                width: Fill height: Fit flow: Down spacing: 16
                od_columns := View {
                    width: Fill height: Fit flow: Down spacing: 16
                    od_col_main := View {
                        width: Fill height: Fit flow: Down spacing: 16
                        od_hosting_card := mod.widgets.OdCard {
                            od_hosting_summary := View {
                                width: Fill height: Fit flow: Down spacing: 10
                            View {
                                width: Fill height: Fit flow: Right spacing: 12 align: Align{y: 0.0}
                                od_hosting_glyph := OdGlyph {width: 34 height: 34}
                                View {
                                    width: Fill height: Fit flow: Down spacing: 3
                                    od_hosting_state := mod.widgets.OdTitle {}
                                    od_hosting_body := mod.widgets.OdCaption {}
                                }
                            }
                            od_hosting_detail := mod.widgets.OdCaption {visible: false}
                            mod.widgets.OdSeparator {}
                            View {
                                width: Fill height: Fit flow: Right spacing: 12
                                View {
                                    width: Fill height: Fit flow: Down spacing: 2
                                    od_runs := mod.widgets.OdMetric {}
                                    od_runs_label := mod.widgets.OdCaption {}
                                }
                                View {
                                    width: Fill height: Fit flow: Down spacing: 2
                                    od_approvals := mod.widgets.OdMetric {}
                                    od_approvals_label := mod.widgets.OdCaption {}
                                }
                            }
                            }
                            od_command_row := RoundedView {
                                visible: false width: Fill height: Fit flow: Down spacing: 8 padding: 12
                                draw_bg +: {color: OD_FIELD border_radius: OD_RADIUS_SMALL}
                                View {
                                    width: Fill height: Fit flow: Right spacing: 10
                                    od_command_glyph := OdGlyph {}
                                    od_command_status := mod.widgets.OdText {}
                                }
                                View {
                                    width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 8
                                    od_check_command := mod.widgets.OdPrimaryButton {text: "Check status"}
                                    od_retry_command := mod.widgets.OdButton {text: "Retry"}
                                    od_dismiss_command := mod.widgets.OdButton {text: "Dismiss"}
                                }
                            }
                            od_stop_sheet := RoundedView {
                                visible: false width: Fill height: Fit flow: Down spacing: 8 padding: 14
                                draw_bg +: {color: OD_CRI_BG border_radius: OD_RADIUS_SMALL}
                                od_stop_title := mod.widgets.OdHeadline {}
                                od_stop_body := mod.widgets.OdText {}
                                View {
                                    width: Fill height: Fit flow: Right spacing: 8
                                    od_stop_cancel := mod.widgets.OdButton {width: Fill text: "Cancel" padding: Inset{left: 8 right: 8}}
                                    od_stop_confirm := mod.widgets.OdDestructivePrimaryButton {width: Fill text: "Stop" padding: Inset{left: 8 right: 8}}
                                }
                            }
                            od_hosting_actions := View {
                                width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 8
                                od_pause := mod.widgets.OdButton {text: "Pause hosting"}
                                od_resume := mod.widgets.OdPrimaryButton {text: "Resume hosting"}
                                od_stop := mod.widgets.OdDestructiveButton {text: "Stop hosting"}
                                od_hosting_setup := mod.widgets.OdPrimaryButton {text: "Set up hosting"}
                            }
                        }
                        od_options_card := mod.widgets.OdCard {
                            visible: false
                            od_keep_running_row := View {
                                width: Fill height: Fit flow: Right spacing: 12 align: Align{y: 0.5}
                                View {
                                    width: Fill height: Fit flow: Down spacing: 3
                                    od_keep_running_title := mod.widgets.OdText {}
                                    od_keep_running_body := mod.widgets.OdCaption {}
                                }
                                od_keep_running := mod.widgets.OdSwitch {}
                            }
                            mod.widgets.OdSeparator {}
                            od_foreign_service := mod.widgets.OdCaption {visible: false}
                            od_uninstall_sheet := RoundedView {
                                visible: false width: Fill height: Fit flow: Down spacing: 8 padding: 14
                                draw_bg +: {color: OD_CRI_BG border_radius: OD_RADIUS_SMALL}
                                od_uninstall_title := mod.widgets.OdHeadline {}
                                od_uninstall_body := mod.widgets.OdText {}
                                View {
                                    width: Fill height: Fit flow: Right spacing: 8
                                    od_uninstall_cancel := mod.widgets.OdButton {width: Fill text: "Cancel" padding: Inset{left: 8 right: 8}}
                                    od_uninstall_confirm := mod.widgets.OdDestructivePrimaryButton {width: Fill text: "Uninstall" padding: Inset{left: 8 right: 8}}
                                }
                            }
                            od_service_actions := View {
                                width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 8
                                od_use_this_service := mod.widgets.OdButton {visible: false text: "Use this app's service"}
                                od_uninstall := mod.widgets.OdDestructiveButton {text: "Uninstall hosting service"}
                            }
                        }
                        od_attention_card := mod.widgets.OdCard {
                            od_attention_header := mod.widgets.OdHeadline {}
                            od_attention := OdList {
                                spacing: 8
                                Row := View {
                                    width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                                    StatusRow {}
                                    od_attention_open := mod.widgets.OdButton {text: "Details"}
                                }
                            }
                            od_attention_none := StatusRow {}
                        }
                        od_summary_card := mod.widgets.OdCard {
                            od_summary := UsageTotals {}
                        }
                    }
                    od_col_side := View {
                        width: Fill height: Fit flow: Down spacing: 12
                        od_plans_header := mod.widgets.OdSectionHeader {}
                        od_plans_empty := mod.widgets.OdCard {
                            visible: false
                            od_plans_empty_text := mod.widgets.OdCaption {}
                        }
                        od_accounts_pager := Pager {}
                        od_accounts := OdList {
                            spacing: 12
                            Account := mod.widgets.OdCard {
                                View {
                                    width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 8 align: Align{y: 0.5}
                                    View {
                                        width: Fill height: Fit flow: Down spacing: 2
                                        title := mod.widgets.OdHeadline {}
                                        subtitle := mod.widgets.OdCaption {}
                                    }
                                    badge := Badge {}
                                }
                                note := mod.widgets.OdCaption {visible: false}
                                quotas := OdList {
                                    spacing: 12
                                    Quota := View {
                                        width: Fill height: Fit flow: Down spacing: 5
                                        View {
                                            width: Fill height: Fit flow: Right spacing: 8
                                            label := mod.widgets.OdText {}
                                            value := mod.widgets.OdNumber {}
                                        }
                                        meter := OdMeter {}
                                        reset := mod.widgets.OdCaption {}
                                    }
                                }
                                observed := mod.widgets.OdCaption {}
                                res_section := View {
                                    width: Fill height: Fit flow: Down spacing: 0
                                    mod.widgets.OdSeparator {}
                                    res_header := mod.widgets.OdSectionHeader {margin: Inset{top: 8 left: 0}}
                                    pager := Pager {}
                                    resources := OdList {Res := ResourceRow {}}
                                }
                            }
                        }
                        od_unassigned_card := mod.widgets.OdCard {
                            visible: false
                            od_unassigned_title := mod.widgets.OdHeadline {}
                            od_unassigned_note := mod.widgets.OdCaption {}
                            od_unassigned_pager := Pager {}
                            od_unassigned := OdList {Res := ResourceRow {}}
                        }
                    }
                }
            }

            od_resources_page := View {
                visible: false width: Fill height: Fit flow: Down spacing: 12
                od_detail := mod.widgets.OdCard {
                    visible: false
                    od_detail_back := mod.widgets.OdButton {text: "Back"}
                    od_detail_title := mod.widgets.OdTitle {}
                    od_detail_line := mod.widgets.OdCaption {}
                    od_detail_status := StatusRow {}
                    mod.widgets.OdSeparator {}
                    od_detail_tokens := OdList {Line := KeyValue {}}
                    od_detail_ratio := KeyValue {}
                    od_detail_facts_header := mod.widgets.OdSectionHeader {margin: Inset{top: 8 left: 0}}
                    od_detail_facts := OdList {Fact := KeyValue {}}
                }
                od_resources_empty := mod.widgets.OdCard {
                    visible: false
                    od_resources_empty_text := mod.widgets.OdCaption {}
                }
                od_resources_card := mod.widgets.OdCard {
                    spacing: 0
                    od_resources_pager := Pager {}
                    od_resources := OdList {Res := ResourceRow {}}
                }
            }

            od_usage_page := View {
                visible: false width: Fill height: Fit flow: Down spacing: 12
                od_period := SegmentTrack {
                    od_period_day := Segment {}
                    od_period_week := Segment {}
                    od_period_month := Segment {}
                }
                od_period_picker_row := View {visible: false width: Fill height: Fit
                od_period_picker := mod.widgets.OdDropDown {width: Fill height: OD_CONTROL
                    draw_text +: {text_style: theme.font_regular{font_size: OD_SIZE_BODY}}
                    popup_menu: PopupMenu {width: 300 menu_item: PopupMenuItem {height: OD_CONTROL draw_text +: {text_style: theme.font_regular{font_size: OD_SIZE_BODY}}}}
                }
                }
                od_usage_card := mod.widgets.OdCard {
                    od_usage := UsageTotals {}
                    mod.widgets.OdSeparator {}
                    od_cost_row := KeyValue {}
                    od_cost_note := mod.widgets.OdCaption {}
                }
                od_filters := OdList {
                    flow: Flow.Right{wrap: true} spacing: 8
                    Chip := Chip {}
                }
                od_usage_filters_toggle := mod.widgets.OdButton {text: "Filters & dates"}
                od_usage_filters_card := mod.widgets.OdCard {
                    visible: false
                    od_filter_account := mod.widgets.ODFilterPicker {}
                    od_filter_resource := mod.widgets.ODFilterPicker {}
                    od_filter_model := mod.widgets.ODFilterPicker {}
                    od_filter_requester := mod.widgets.ODFilterPicker {}
                    od_clear_filters := mod.widgets.OdButton {}
                    mod.widgets.OdSeparator {}
                    od_custom_dates := mod.widgets.OdHeadline {}
                    od_date_start_label := mod.widgets.OdCaption {}
                    od_date_start := TextInput {width: Fill height: OD_CONTROL empty_text: "YYYY-MM-DD" draw_text +: {text_style: theme.font_regular{font_size: OD_SIZE_BODY}}}
                    od_date_end_label := mod.widgets.OdCaption {}
                    od_date_end := TextInput {width: Fill height: OD_CONTROL empty_text: "YYYY-MM-DD" draw_text +: {text_style: theme.font_regular{font_size: OD_SIZE_BODY}}}
                    od_date_error := mod.widgets.OdCaption {visible: false draw_text.color: OD_CRI_FG}
                    od_apply_dates := mod.widgets.OdPrimaryButton {}
                }
                od_usage_boundary := mod.widgets.OdCaption {margin: Inset{left: 4 right: 4}}
                od_usage_pager := Pager {}
                od_by_resource_header := mod.widgets.OdSectionHeader {}
                od_by_resource_card := mod.widgets.OdCard {
                    od_by_resource := OdList {
                        spacing: 4
                        Row := View {
                            width: Fill height: Fit flow: Down spacing: 4 padding: Inset{top: 8 bottom: 8}
                            View {
                                width: Fill height: Fit flow: Right spacing: 12 align: Align{y: 0.5}
                                name := mod.widgets.OdHeadline {}
                                total := mod.widgets.OdNumber {}
                            }
                            badge := Badge {}
                            cells := View {
                                width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 4
                                input := TokenCell {}
                                output := TokenCell {}
                                cache_read := TokenCell {}
                                cache_write := TokenCell {}
                                ratio := TokenCell {}
                            }
                            mod.widgets.OdSeparator {}
                        }
                    }
                }
            }


            od_activity_page := View {
                visible: false width: Fill height: Fit flow: Down spacing: 12
                od_activity_empty := mod.widgets.OdCard {
                    visible: false
                    od_activity_empty_text := mod.widgets.OdCaption {}
                }
                od_activity_pager := Pager {}
                od_activity_card := mod.widgets.OdCard {
                    spacing: 0
                    od_activity := OdList {
                        Row := View {
                            width: Fill height: Fit flow: Right spacing: 12 padding: Inset{top: 10 bottom: 10}
                            glyph := OdGlyph {}
                            View {
                                width: Fill height: Fit flow: Down spacing: 3
                                title := mod.widgets.OdHeadline {}
                                detail := mod.widgets.OdCaption {}
                                meta := mod.widgets.OdCaption {}
                            }
                        }
                    }
                }
            }
            od_connection_banner := mod.widgets.OdCard {
                visible: false
                View {
                    width: Fill height: Fit flow: Right spacing: 12
                    od_banner_glyph := OdGlyph {}
                    View {
                        width: Fill height: Fit flow: Down spacing: 4
                        od_banner_title := mod.widgets.OdHeadline {}
                        od_banner_body := mod.widgets.OdText {}
                    }
                }
                View {
                    width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 8
                    od_setup := mod.widgets.OdPrimaryButton {text: "Set up hosting"}
                    od_reconnect := mod.widgets.OdButton {text: "Try again"}
                }
            }

            od_inventory_notice := mod.widgets.OdCard {
                visible: false
                View {width: Fill height: Fit flow: Right spacing: 10
                    glyph := OdGlyph {}
                    title := mod.widgets.OdHeadline {}
                }
                detail := mod.widgets.OdCaption {}
            }

            // A new host notice must not insert height above a reader's
            // current position. The pinned status row links to this full text.
            od_notice := StatusRow {visible: false}
        }
    }
}

#[derive(Script, Widget)]
pub struct OwnerDashboard {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    state: DashboardState,
    #[rust]
    clock_override: Option<Clock>,
    #[rust]
    dirty: bool,
    #[rust]
    wide: Option<bool>,
    #[rust]
    reduced_motion: bool,
    #[rust]
    pending_scroll: Option<f64>,
    #[rust]
    pending_account: Option<String>,
    #[rust]
    pending_focus: Option<LiveId>,
    #[rust]
    header_expanded: bool,
    #[rust]
    compact_navigation: bool,
    #[rust]
    pending_reveal: Option<LiveId>,
    #[rust]
    age_timer: Timer,
    #[rust]
    sync_count: u64,
    #[rust]
    notice: Option<Status>,
    #[rust]
    date_error: bool,
    #[rust]
    pagers: Vec<(String, WidgetRef, usize)>,
}

impl ScriptHook for OwnerDashboard {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, _apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        // New or reapplied DSL (theme change) resets imperative widget state;
        // the next draw re-syncs every label, row and visibility from state.
        self.dirty = true;
        self.wide = None;
    }
}

impl Widget for OwnerDashboard {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let content = self.view.widget(cx, ids!(od_content));
        let scroll_y = self.scroll_y(cx);
        crate::keyboard::reveal_focus(cx, event, &content, scroll_y);
        self.view.handle_event(cx, event, scope);
        if self.age_timer.is_event(event).is_some() {
            self.sync(cx);
        }
        if let Event::KeyDown(ke) = event {
            if self.view.visible && self.view.area().is_valid(cx) && crate::keyboard::contains_focus(&self.view, cx) {
                self.handle_key(cx, ke);
            }
        }
        if let Event::Actions(actions) = event {
            self.handle_actions(cx, actions);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.age_timer.is_empty() {
            self.age_timer = cx.start_interval(AGE_REFRESH_SECONDS);
        }
        let width = cx.peek_walk_turtle(walk).size.x;
        let text_scale = crate::theme::effective_text_scale(cx).max(1.0);
        let compact = compact_navigation(width, text_scale);
        if self.compact_navigation != compact {
            self.compact_navigation = compact;
            self.dirty = true;
        }
        let wide = width >= WIDE_MIN * text_scale;
        if self.wide != Some(wide) {
            self.wide = Some(wide);
            self.apply_layout(cx, wide);
            self.dirty = true;
        }
        if self.dirty {
            self.sync(cx);
        }
        let step = self.view.draw_walk(cx, scope, walk);
        if !step.is_step() {
            if let Some(target) = self.pending_focus.take() {
                let area = self.view.widget(cx, &[target]).area();
                if self.pending_reveal.is_some() {
                    // Reveal the whole confirmation, not just its focused button.
                    cx.set_key_focus(area);
                } else {
                    crate::keyboard::focus(cx, area);
                }
            }
            if let Some(target) = self.pending_reveal.take() {
                let content = self.view.widget(cx, ids!(od_content)).area().rect(cx);
                let target = self.view.widget(cx, &[target]).area().rect(cx);
                let y = (self.scroll_y(cx) + target.pos.y - content.pos.y - 8.0).max(0.0);
                self.view.view(cx, ids!(od_content)).set_scroll_pos(cx, dvec2(0.0, y));
                self.view.redraw(cx);
            }
        }
        if let Some(y) = self.pending_scroll.take() {
            self.view.view(cx, ids!(od_content)).set_scroll_pos(cx, dvec2(0.0, y));
            self.view.redraw(cx);
        }
        if let Some(account) = self.pending_account.take() {
            if let Some((_, row)) = self.view.od_list(cx, ids!(od_accounts)).rows().into_iter().find(|(id, _)| id == &account) {
                let content = self.view.widget(cx, ids!(od_content)).area().rect(cx);
                let y = (self.scroll_y(cx) + row.area().rect(cx).pos.y - content.pos.y).max(0.0);
                self.view.view(cx, ids!(od_content)).set_scroll_pos(cx, dvec2(0.0, y));
                self.view.redraw(cx);
            }
        }
        step
    }
}

fn tab_path(tab: Tab) -> &'static [LiveId] {
    match tab {
        Tab::Overview => ids!(od_tab_overview),
        Tab::Resources => ids!(od_tab_resources),
        Tab::Usage => ids!(od_tab_usage),
        Tab::Activity => ids!(od_tab_activity),
    }
}

const ALL_FILTER: &str = "__all";

impl OwnerDashboard {
    fn reset_owner_ui(&mut self, cx: &mut Cx) {
        for path in [ids!(od_filter_account), ids!(od_filter_resource), ids!(od_filter_model), ids!(od_filter_requester)] {
            self.view.od_filter_picker(cx, path).reset(cx);
        }
        self.view.text_input(cx, ids!(od_date_start)).set_text(cx, "");
        self.view.text_input(cx, ids!(od_date_end)).set_text(cx, "");
        self.date_error = false;
        self.notice = None;
        self.header_expanded = false;
        self.pending_focus = None;
        self.pending_reveal = None;
        self.pending_scroll = None;
        self.pending_account = None;
    }

    fn clock(&self) -> Clock {
        self.clock_override.unwrap_or_else(Clock::system)
    }

    fn emit(&self, cx: &mut Cx, action: DashboardAction) {
        cx.widget_action(self.widget_uid(), action);
    }

    fn changed(&mut self, cx: &mut Cx) {
        self.sync(cx);
    }

    fn apply_layout(&mut self, cx: &mut Cx, wide: bool) {
        let gutter = if wide { 24.0 } else { 16.0 };
        if let Some(mut header) = self.view.view(cx, ids!(od_header)).borrow_mut() {
            header.layout.padding.left = gutter;
            header.layout.padding.right = gutter;
        }
        if let Some(mut content) = self.view.view(cx, ids!(od_content)).borrow_mut() {
            content.layout.padding.left = gutter;
            content.layout.padding.right = gutter;
        }
        if let Some(mut columns) = self.view.view(cx, ids!(od_columns)).borrow_mut() {
            columns.layout.flow = if wide { Flow::right() } else { Flow::Down };
            columns.layout.spacing = if wide { 20.0 } else { 16.0 };
        }
        self.view.redraw(cx);
    }

    fn handle_key(&mut self, cx: &mut Cx, ke: &KeyEvent) {
        // The host app owns shortcuts; Escape is always local.
        if ke.key_code != KeyCode::Escape {
            return;
        }
        if self.state.confirm_stop {
            self.state.cancel_stop();
            self.pending_focus = Some(id!(od_stop));
            self.changed(cx);
        } else if self.state.confirm_uninstall {
            self.state.cancel_uninstall();
            self.pending_focus = Some(id!(od_uninstall));
            self.changed(cx);
        } else if self.state.view.selected_resource.is_some() {
            self.state.select_resource(None);
            self.changed(cx);
        }
    }

    fn select_tab(&mut self, cx: &mut Cx, tab: Tab) {
        if self.state.select_tab(tab) {
            self.view.view(cx, ids!(od_content)).set_scroll_pos(cx, dvec2(0.0, 0.0));
            self.emit(cx, DashboardAction::Navigated(self.state.view.clone()));
            self.changed(cx);
        }
    }

    fn clicked(&self, cx: &Cx, path: &[LiveId], actions: &Actions) -> bool {
        self.view.button(cx, path).clicked(actions)
    }

    fn segment_clicked(&self, cx: &Cx, path: &[LiveId], actions: &Actions) -> bool {
        self.view.view(cx, path).button(cx, ids!(button)).clicked(actions)
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.clicked(cx, ids!(od_status_details), actions) {
            let presented = self.state.present(self.clock());
            let target = self.header_status(&presented).target;
            match target {
                HeaderTarget::Notice => self.pending_reveal = Some(id!(od_notice)),
                HeaderTarget::Command => {
                    self.select_tab(cx, Tab::Overview);
                    self.pending_reveal = Some(id!(od_command_row));
                }
                HeaderTarget::Connection => self.pending_reveal = Some(id!(od_connection_banner)),
                HeaderTarget::Inventory => self.pending_reveal = Some(id!(od_inventory_notice)),
                HeaderTarget::Refresh => {
                    let action = self.state.begin_refresh();
                    self.emit(cx, action);
                    self.changed(cx);
                }
                HeaderTarget::None => {}
            }
            self.view.redraw(cx);
        }
        if self.clicked(cx, ids!(od_owner_menu), actions) {
            self.header_expanded = !self.header_expanded;
            self.changed(cx);
        }
        if let Some(index) = self.view.drop_down(cx, ids!(od_period_picker)).changed(actions) {
            if let Some(period) = [UsagePeriod::Day, UsagePeriod::Week, UsagePeriod::Month].get(index) {
                let action = self.state.select_period(*period);
                self.emit(cx, action);
            } else { self.state.view.filters_open = true; }
            self.emit(cx, DashboardAction::Navigated(self.state.view.clone()));
            self.changed(cx);
        }
        for (key, pager, total) in self.pagers.clone() {
            let direction = if pager.button(cx, ids!(previous)).clicked(actions) { -1_i32 }
                else if pager.button(cx, ids!(next)).clicked(actions) { 1 } else { continue };
            let current = self.page_start(&key, total) / PAGE_ROWS;
            let next = if direction < 0 { current.saturating_sub(1) } else { (current + 1).min(total.saturating_sub(1) / PAGE_ROWS) };
            self.state.view.pages.insert(key, next);
            self.pending_scroll = Some(0.0);
            self.emit(cx, DashboardAction::Navigated(self.state.view.clone()));
            self.changed(cx);
        }
        if self.clicked(cx, ids!(od_usage_filters_toggle), actions) {
            self.state.view.filters_open = !self.state.view.filters_open;
            self.pending_focus = Some(id!(od_usage_filters_toggle));
            if self.state.view.filters_open { self.pending_reveal = Some(id!(od_usage_filters_toggle)); }
            self.emit(cx, DashboardAction::Navigated(self.state.view.clone()));
            self.changed(cx);
        }
        for (path, start) in [(ids!(od_date_start),true),(ids!(od_date_end),false)] {
            if let Some(text) = self.view.text_input(cx,path).changed(actions) {
                if start { self.state.view.date_start = text; } else { self.state.view.date_end = text; }
                self.emit(cx, DashboardAction::Navigated(self.state.view.clone()));
            }
        }
        if self.clicked(cx, ids!(od_apply_dates), actions) {
            let start = self.view.text_input(cx, ids!(od_date_start)).text();
            let end = self.view.text_input(cx, ids!(od_date_end)).text();
            if let Some(period) = crate::date_range::parse(&start, &end) {
                self.date_error = false;
                let action = self.state.select_period(period);
                self.emit(cx, action);
                self.emit(cx, DashboardAction::Navigated(self.state.view.clone()));
            } else {
                self.date_error = true;
                crate::keyboard::focus(cx, self.view.text_input(cx, ids!(od_date_start)).area());
            }
            self.changed(cx);
        }
        let picker_paths = [ids!(od_filter_account), ids!(od_filter_resource), ids!(od_filter_model), ids!(od_filter_requester)];
        for path in picker_paths {
            let widget = self.view.widget(cx, path);
            if widget.button(cx, ids!(picker_toggle)).clicked(actions) && widget.as_od_filter_picker().is_open() {
                for other in picker_paths {
                    if other != path {
                        let picker = self.view.od_filter_picker(cx, other);
                        if picker.is_open() { picker.collapse(cx); }
                    }
                }
            }
        }
        for (path, dimension) in [(ids!(od_filter_account), 3), (ids!(od_filter_resource), 0), (ids!(od_filter_model), 1), (ids!(od_filter_requester), 2)] {
            if let Some(choice) = self.view.od_filter_picker(cx, path).changed(actions) {
                match dimension {
                    0 => self.state.view.resource_filter = choice,
                    1 => self.state.view.model_filter = choice,
                    3 => self.state.view.account_filter = choice,
                    _ => self.state.view.requester_filter = choice,
                }
                self.refresh_filters(cx);
            }
        }
        if self.clicked(cx, ids!(od_clear_filters), actions) {
            self.state.view.account_filter = None;
            self.state.view.resource_filter = None;
            self.state.view.model_filter = None;
            self.state.view.requester_filter = None;
            self.refresh_filters(cx);
        }
        let attention = self.state.present(self.clock()).attention;
        for (key, row) in self.view.od_list(cx, ids!(od_attention)).rows() {
            if row.button(cx, ids!(od_attention_open)).clicked(actions) {
                if let Some(item) = attention.iter().find(|item| item.key == key) {
                    use crate::model::AttentionTarget;
                    match &item.target {
                        AttentionTarget::Approvals => {
                            if self.state.owner_scope.as_deref().is_some_and(|scope| self.state.snapshot.as_ref()
                                .and_then(|snapshot| snapshot.owner.as_ref()).is_some_and(|owner| owner.user_id == scope)) {
                                self.emit(cx, DashboardAction::ReviewApprovals);
                            }
                        }
                        AttentionTarget::Activity => self.select_tab(cx, Tab::Activity),
                        AttentionTarget::Usage => self.select_tab(cx, Tab::Usage),
                        AttentionTarget::Resources => self.select_tab(cx, Tab::Resources),
                        AttentionTarget::Account(account) => {
                            if let Some(index) = self.state.present(self.clock()).accounts.iter().position(|row| row.id == *account) {
                                self.state.view.pages.insert("accounts".into(), index / PAGE_ROWS);
                            }
                            self.select_tab(cx, Tab::Overview);
                            self.pending_account = Some(account.clone());
                            self.changed(cx);
                        }
                        AttentionTarget::Command => {
                            if let Some(action) = self.state.check_command() { self.emit(cx, action); self.changed(cx); }
                        }
                    }
                }
            }
        }
        if let Some(index) = self.view.drop_down(cx, ids!(od_destination)).changed(actions) {
            if let Some(tab) = Tab::ALL.get(index) { self.select_tab(cx, *tab); }
        }
        let now = self.clock().now_ms;
        for tab in Tab::ALL {
            if self.segment_clicked(cx, tab_path(tab), actions) {
                self.select_tab(cx, tab);
            }
        }
        for (path, period) in [
            (ids!(od_period_day), UsagePeriod::Day),
            (ids!(od_period_week), UsagePeriod::Week),
            (ids!(od_period_month), UsagePeriod::Month),
        ] {
            if self.segment_clicked(cx, path, actions) && self.state.view.period != Some(period) {
                let action = self.state.select_period(period);
                self.emit(cx, action);
                self.changed(cx);
            }
        }
        if self.clicked(cx, ids!(od_refresh), actions) || self.clicked(cx, ids!(od_reconnect), actions) {
            let action = self.state.begin_refresh();
            self.emit(cx, action);
            self.changed(cx);
        }
        if self.clicked(cx, ids!(od_setup), actions) {
            let banner = self.state.present(self.clock()).banner;
            if let Some(action) = banner.and_then(|b| [b.primary, b.secondary].into_iter().find_map(BannerAction::action)) {
                self.emit(cx, action);
            }
        }
        if self.clicked(cx, ids!(od_hosting_setup), actions) || self.clicked(cx, ids!(od_setup_start), actions) {
            if !self.state.setup.as_ref().is_some_and(SetupView::running) {
                self.emit(cx, DashboardAction::OpenSetup);
            }
        }
        if self.clicked(cx, ids!(od_setup_cancel), actions) {
            self.emit(cx, DashboardAction::CancelSetup);
        }
        if let Some(keep) = self.view.check_box(cx, ids!(od_keep_running)).changed(actions) {
            self.emit(cx, DashboardAction::KeepRunning(keep));
        }
        if self.clicked(cx, ids!(od_use_this_service), actions) {
            self.emit(cx, DashboardAction::StartService);
        }
        if self.clicked(cx, ids!(od_uninstall), actions) && self.state.request_uninstall() {
            self.pending_focus = Some(id!(od_uninstall_cancel));
            self.pending_reveal = Some(id!(od_uninstall_sheet));
            self.changed(cx);
        }
        if self.clicked(cx, ids!(od_uninstall_cancel), actions) {
            self.state.cancel_uninstall();
            self.pending_focus = Some(id!(od_uninstall));
            self.changed(cx);
        }
        if self.clicked(cx, ids!(od_uninstall_confirm), actions) {
            if let Some(action) = self.state.confirm_uninstall() {
                self.emit(cx, action);
            }
            self.changed(cx);
        }
        for (path, kind) in [
            (ids!(od_pause), CommandKind::Pause),
            (ids!(od_resume), CommandKind::Resume),
            (ids!(od_stop), CommandKind::Stop),
            (ids!(od_stop_confirm), CommandKind::Stop),
        ] {
            if self.clicked(cx, path, actions) {
                let confirming = self.state.confirm_stop;
                if let Some(action) = self.state.request(kind, now) {
                    self.emit(cx, action);
                }
                if !confirming && self.state.confirm_stop {
                    self.pending_focus = Some(id!(od_stop_cancel));
                    self.pending_reveal = Some(id!(od_stop_sheet));
                    self.header_expanded = false;
                }
                self.changed(cx);
            }
        }
        if self.clicked(cx, ids!(od_stop_cancel), actions) {
            self.state.cancel_stop();
            self.pending_focus = Some(id!(od_stop));
            self.changed(cx);
        }
        if self.clicked(cx, ids!(od_check_command), actions) {
            if let Some(action) = self.state.check_command() {
                self.emit(cx, action);
            }
            self.changed(cx);
        }
        if self.clicked(cx, ids!(od_retry_command), actions) {
            if let Some(action) = self.state.retry_rejected(now) {
                self.emit(cx, action);
            }
            self.changed(cx);
        }
        if self.clicked(cx, ids!(od_dismiss_command), actions) {
            self.state.dismiss_command();
            self.changed(cx);
        }
        if self.clicked(cx, ids!(od_detail_back), actions) {
            self.state.select_resource(None);
            self.emit(cx, DashboardAction::Navigated(self.state.view.clone()));
            self.changed(cx);
        }
        // Resource rows in every list open the same detail.
        let mut opened = None;
        let mut lists = vec![self.view.od_list(cx, ids!(od_resources)), self.view.od_list(cx, ids!(od_unassigned))];
        for (_, account) in self.view.od_list(cx, ids!(od_accounts)).rows() {
            lists.push(account.od_list(cx, ids!(resources)));
        }
        for list in lists {
            for (key, row) in list.rows() {
                if row.button(cx, ids!(od_resource_open)).clicked(actions) {
                    opened = Some(key);
                }
            }
        }
        if let Some(id) = opened {
            self.state.select_resource(Some(id));
            self.state.select_tab(Tab::Resources);
            self.view.view(cx, ids!(od_content)).set_scroll_pos(cx, dvec2(0.0, 0.0));
            self.emit(cx, DashboardAction::Navigated(self.state.view.clone()));
            self.changed(cx);
        }
        for (key, row) in self.view.od_list(cx, ids!(od_filters)).rows() {
            if row.button(cx, ids!(chip)).clicked(actions) {
                let filter = (key != ALL_FILTER).then_some(key);
                self.state.set_account_filter(filter);
                self.refresh_filters(cx);
            }
        }
    }

    fn refresh_filters(&mut self, cx: &mut Cx) {
        self.state.view.pages.remove("usage");
        self.emit(cx, DashboardAction::Navigated(self.state.view.clone()));
        let action = self.state.begin_refresh();
        self.emit(cx, action);
        self.changed(cx);
    }

    fn page_start(&self, key: &str, count: usize) -> usize {
        self.state.view.pages.get(key).copied().unwrap_or(0).min(count.saturating_sub(1) / PAGE_ROWS) * PAGE_ROWS
    }

    fn pager(&mut self, cx: &mut Cx, key: &str, widget: WidgetRef, total: usize) -> usize {
        let start = self.page_start(key, total);
        let visible = total > PAGE_ROWS && match key { "usage" => self.state.view.tab == Tab::Usage, "activity" => self.state.view.tab == Tab::Activity, _=>true };
        widget.set_visible(cx, visible);
        if visible {
            let zh = self.state.locale == Locale::ZhHans;
            let end = (start + PAGE_ROWS).min(total);
            widget.label(cx, ids!(count)).set_text(cx, &if zh { format!("第 {}–{} 条，共 {} 条", start+1,end,total) } else { format!("Showing {}–{} of {}",start+1,end,total) });
            widget.button(cx, ids!(previous)).set_text(cx, if zh { "上一页" } else { "Previous" });
            widget.button(cx, ids!(next)).set_text(cx, if zh { "下一页" } else { "Next" });
            widget.button(cx, ids!(previous)).set_enabled(cx, start>0);
            widget.button(cx, ids!(next)).set_enabled(cx, end<total);
            self.pagers.push((key.into(), widget, total));
        }
        start
    }

    fn filter_choices(&self, dimension: u8) -> Vec<(Option<String>, String)> {
        let msg = match dimension { 0 => Msg::AllResources, 1 => Msg::AllModels, 3 => Msg::AllAccounts, _ => Msg::AllRequesters };
        let mut choices = vec![(None, tr(self.state.locale, msg).to_owned())];
        if let Some(snapshot) = &self.state.snapshot {
            if dimension == 3 {
                choices.extend(snapshot.accounts.iter().map(|row| (Some(row.id.clone()), row.label.clone())));
            } else if dimension == 0 {
                choices.extend(snapshot.resources.iter().map(|row| (Some(row.id.clone()), row.name.clone())));
            } else if let Some(details) = &snapshot.usage_breakdown {
                let keys = if dimension == 1 { &details.models } else { &details.requesters };
                choices.extend(keys.iter().map(|key| {
                    let short = if dimension == 2 { key.strip_prefix('@').and_then(|v| v.split_once(':')).map(|v| v.0).unwrap_or(key) } else { key };
                    let duplicates = keys.iter().filter(|other| other.strip_prefix('@').and_then(|v| v.split_once(':')).map(|v| v.0) == Some(short)).count();
                    (Some(key.clone()), if duplicates > 1 { key.clone() } else { short.to_owned() })
                }));
            }
        }
        choices
    }

    /// Actual content offset, independent of which banners precede the page.
    fn scroll_y(&self, cx: &Cx) -> f64 {
        let content = self.view.view(cx, ids!(od_content));
        #[cfg(feature = "native-accessibility")]
        return content.borrow().map(|view| view.get_scroll_pos().y).unwrap_or(0.0).max(0.0);
        #[cfg(not(feature = "native-accessibility"))]
        {
            // The unpatched framework has no getter. A permanent zero-height
            // first child measures the same origin; page/banner geometry cannot
            // affect it. Native builds read the ScrollBars value directly.
            let top = content.area().rect(cx).pos.y + content.borrow().map(|v| v.layout.padding.top).unwrap_or(0.0);
            (top - self.view.view(cx, ids!(od_scroll_origin)).area().rect(cx).pos.y).max(0.0)
        }
    }

    #[cfg(feature = "native-accessibility")]
    pub(crate) fn owner_controls_expanded(&self) -> bool { self.header_expanded }
    #[cfg(feature = "native-accessibility")]
    pub(crate) fn header_notice_text(&self) -> Option<String> { self.header_status(&self.state.present(self.clock())).detail }

    fn label(&self, cx: &mut Cx, path: &[LiveId], text: &str) {
        self.view.label(cx, path).set_text(cx, text);
    }

    fn show(&self, cx: &mut Cx, path: &[LiveId], visible: bool) {
        self.view.widget(cx, path).set_visible(cx, visible);
    }

    fn button(&self, cx: &mut Cx, path: &[LiveId], text: &str, visible: bool, enabled: bool) {
        let button = self.view.button(cx, path);
        button.set_text(cx, text);
        button.set_visible(cx, visible);
        button.set_enabled(cx, enabled);
    }

    fn header_status(&self, p: &Presentation) -> HeaderStatus {
        let t = |msg| tr(self.state.locale, msg);
        let inventory = matches!(self.state.view.tab, Tab::Overview | Tab::Resources)
            && self.state.snapshot.as_ref().is_some_and(|snapshot| !snapshot.inventory_available || !snapshot.inventory_complete);
        let notice_active = self.notice.as_ref().is_some_and(|notice| notice.tone == Tone::Progress);
        let notice_problem = self.notice.as_ref().is_some_and(|notice| matches!(notice.tone, Tone::Critical | Tone::Caution));
        let command_problem = self.state.command.as_ref().is_some_and(|command| !matches!(command.phase, CommandPhase::Confirmed { .. } | CommandPhase::Unchanged | CommandPhase::Superseded));
        // A completed operation must never mask a later disconnection or stale
        // inventory. The same target drives both the caption and Details.
        let target = if notice_active { HeaderTarget::Notice }
            else if !matches!(self.state.connection, ConnectionStatus::Connected) { HeaderTarget::Connection }
            else if notice_problem { HeaderTarget::Notice }
            else if command_problem { HeaderTarget::Command }
            else if inventory { HeaderTarget::Inventory }
            else if p.sync_stale { HeaderTarget::Refresh }
            else if self.state.refreshing { HeaderTarget::None }
            else if self.notice.is_some() { HeaderTarget::Notice }
            else if p.command.is_some() { HeaderTarget::Command }
            else { HeaderTarget::None };
        let compact = |status: &Status| {
            let msg = match status.tone {
                Tone::Progress => Msg::StatusWorking,
                Tone::Critical => Msg::StatusUnconfirmed,
                Tone::Caution => Msg::StatusAttention,
                Tone::Positive => Msg::StatusDone,
                Tone::Neutral | Tone::Accent => Msg::StatusUpdate,
            };
            Status::new(status.tone, status.glyph, t(msg))
        };
        let (status, _detail) = match target {
            HeaderTarget::Notice => {
                let notice = self.notice.as_ref().unwrap();
                (compact(notice), Some(notice.text.clone()))
            }
            HeaderTarget::Command => {
                let status = &p.command.as_ref().unwrap().status;
                (compact(status), Some(status.text.clone()))
            }
            HeaderTarget::Connection => {
                let (msg, tone, glyph) = match self.state.connection {
                    ConnectionStatus::Connecting => (Msg::StatusConnecting, Tone::Progress, Glyph::Clock),
                    ConnectionStatus::DaemonUnavailable => (Msg::StatusOffline, Tone::Caution, Glyph::Offline),
                    ConnectionStatus::Incompatible { .. } => (Msg::IncompatibleTitle, Tone::Caution, Glyph::Warning),
                    ConnectionStatus::AuthExpired => (Msg::AuthExpiredTitle, Tone::Caution, Glyph::Warning),
                    ConnectionStatus::OwnerMismatch => (Msg::StatusOtherOwner, Tone::Caution, Glyph::Warning),
                    ConnectionStatus::UnsupportedPlatform => (Msg::UnsupportedPlatformTitle, Tone::Neutral, Glyph::Info),
                    _ => (Msg::StatusDisconnected, Tone::Critical, Glyph::Offline),
                };
                (Status::new(tone, glyph, t(msg)), p.banner.as_ref().map(|banner| format!("{} {}", banner.title, banner.body)))
            }
            HeaderTarget::Inventory => (Status::new(Tone::Caution, Glyph::Info, t(Msg::StatusInventory)), Some(t(Msg::InventoryUnavailable).to_owned())),
            HeaderTarget::Refresh => (Status::new(Tone::Caution, Glyph::Clock, t(Msg::AccountStale)), None),
            HeaderTarget::None => (Status::new(if self.state.refreshing { Tone::Progress } else { Tone::Neutral }, Glyph::Clock, &p.sync_line), None),
        };
        HeaderStatus { status, target, #[cfg(feature = "native-accessibility")] detail: _detail }
    }

    /// Push the whole presentation into the widget tree, in place.
    fn sync(&mut self, cx: &mut Cx) {
        self.pagers.clear();
        self.dirty = false;
        self.sync_count += 1;
        let locale = self.state.locale;
        let t = |m| tr(locale, m);
        let p = self.state.present(self.clock());
        let setup = p.setup_page.is_some();
        let has_data = self.state.snapshot.is_some() && !setup;

        // Header.
        self.label(cx, ids!(od_title), t(Msg::Title));
        let enlarged = self.compact_navigation || crate::theme::effective_text_scale(cx) > 1.3;
        self.show(cx, ids!(od_title), !enlarged);
        self.show(cx, ids!(od_identity_actions), !enlarged || self.header_expanded);
        self.show(cx, ids!(od_freshness), !setup);
        let owner = self.state.snapshot.as_ref().and_then(|snapshot| snapshot.owner.as_ref())
            .map(|owner| if owner.display_name.is_empty() { owner.user_id.as_str() } else { owner.display_name.as_str() })
            .or(self.state.owner_scope.as_deref()).unwrap_or(t(Msg::Title));
        let owner = owner.strip_prefix('@').and_then(|v| v.split_once(':')).map(|v| v.0).unwrap_or(owner);
        self.button(cx, ids!(od_owner_menu), &format!("{} {}", owner, if self.header_expanded { "⌃" } else { "⌄" }), enlarged, true);
        self.show(cx, ids!(od_tabs), !enlarged);
        self.show(cx, ids!(od_destination_row), enlarged);
        let destination = self.view.drop_down(cx, ids!(od_destination));
        destination.set_labels(cx, Tab::ALL.iter().map(|tab| tab.label(self.state.locale).to_owned()).collect());
        destination.set_selected_item(cx, Tab::ALL.iter().position(|tab| *tab == self.state.view.tab).unwrap_or(0));
        self.label(cx, ids!(od_owner), &p.owner_line);
        self.show(cx, ids!(od_owner), !p.owner_line.is_empty());
        self.button(cx, ids!(od_refresh), t(Msg::Refresh), !setup, !self.state.refreshing);
        let header = self.header_status(&p);
        self.label(cx, ids!(od_sync), &header.status.text);
        let spinning = (self.state.refreshing || header.status.tone == Tone::Progress) && !self.reduced_motion;
        self.show(cx, ids!(od_sync_spinner), spinning);
        self.view.od_glyph(cx, ids!(od_sync_glyph)).set(cx, header.status.tone, header.status.glyph);
        self.show(cx, ids!(od_sync_glyph), !spinning);
        let details = !matches!(header.target, HeaderTarget::None | HeaderTarget::Refresh)
            && (header.target != HeaderTarget::Connection || p.banner.is_some());
        self.button(cx, ids!(od_status_details), t(if details { Msg::Details } else { Msg::Refresh }), details || header.target == HeaderTarget::Refresh, !self.state.refreshing || details);

        // Host notice (e.g. the outcome of starting or removing the service).
        let notice = self.view.view(cx, ids!(od_notice));
        notice.set_visible(cx, self.notice.is_some());
        if let Some(n) = &self.notice {
            notice.od_glyph(cx, ids!(glyph)).set(cx, n.tone, n.glyph);
            notice.label(cx, ids!(text)).set_text(cx, &n.text);
        }

        // Connection banner.
        self.show(cx, ids!(od_connection_banner), p.banner.is_some());
        if let Some(banner) = &p.banner {
            self.view.od_glyph(cx, ids!(od_banner_glyph)).set(cx, banner.status.tone, banner.status.glyph);
            self.label(cx, ids!(od_banner_title), &banner.title);
            self.label(cx, ids!(od_banner_body), &banner.body);
            let label = |a: BannerAction| match a {
                BannerAction::Retry => t(Msg::TryAgain),
                BannerAction::Setup => t(Msg::SetUpHosting),
                BannerAction::StartService => t(Msg::StartService),
                BannerAction::SignIn => t(Msg::SignInAgain),
                BannerAction::None => "",
            };
            let host_action = [banner.primary, banner.secondary].into_iter().find(|a| a.action().is_some());
            let retry = [banner.primary, banner.secondary].contains(&BannerAction::Retry);
            let busy = self.state.options.as_ref().is_some_and(|o| o.busy);
            self.button(cx, ids!(od_setup), host_action.map(label).unwrap_or(""), host_action.is_some(), !busy);
            self.button(cx, ids!(od_reconnect), t(Msg::TryAgain), retry, !self.state.refreshing);
        }

        // Tabs.
        for tab in Tab::ALL {
            let segment = self.view.view(cx, tab_path(tab));
            segment.view(cx, ids!(sel)).set_visible(cx, self.state.view.tab == tab);
            segment.button(cx, ids!(button)).set_text(cx, tab.label(locale));
        }
        self.show(cx, ids!(od_tabs), has_data && !enlarged);
        self.show(cx, ids!(od_destination_row), has_data && enlarged);

        // Setup replaces the dashboard until hosting serves this owner.
        self.show(cx, ids!(od_setup_page), setup);
        if let Some(page) = &p.setup_page {
            self.view.od_glyph(cx, ids!(od_setup_glyph)).set(cx, page.status.tone, page.status.glyph);
            self.label(cx, ids!(od_setup_title), &page.title);
            self.label(cx, ids!(od_setup_body), &page.body);
            self.show(cx, ids!(od_setup_progress), page.progress.is_some());
            if let Some(progress) = &page.progress {
                let spin = page.busy && !self.reduced_motion;
                self.show(cx, ids!(od_setup_spinner), spin);
                self.show(cx, ids!(od_setup_progress_glyph), !spin);
                self.view.od_glyph(cx, ids!(od_setup_progress_glyph)).set(cx, progress.tone, progress.glyph);
                self.label(cx, ids!(od_setup_progress_text), &progress.text);
            }
            self.button(cx, ids!(od_setup_start), page.start.unwrap_or(""), page.start.is_some(), true);
            self.button(cx, ids!(od_setup_cancel), t(Msg::Cancel), page.can_cancel, true);
        }

        // Full-page state while there is no snapshot.
        self.show(cx, ids!(od_empty_page), p.empty_page.is_some());
        if let Some(page) = &p.empty_page {
            self.label(cx, ids!(od_empty_title), &page.title);
            self.label(cx, ids!(od_empty_body), &page.body);
            let spin = page.busy && !self.reduced_motion;
            self.show(cx, ids!(od_empty_spinner), spin);
            self.show(cx, ids!(od_empty_glyph), !spin);
            self.view.od_glyph(cx, ids!(od_empty_glyph)).set(cx, page.status.tone, page.status.glyph);
        }
        let tab = self.state.view.tab;
        let inventory = self.state.snapshot.as_ref();
        let inventory_warning = matches!(tab, Tab::Overview | Tab::Resources)
            && inventory.is_some_and(|snapshot| !snapshot.inventory_available || !snapshot.inventory_complete);
        self.show(cx, ids!(od_inventory_notice), inventory_warning);
        if inventory_warning {
            let snapshot = inventory.unwrap();
            let notice = self.view.widget(cx, ids!(od_inventory_notice));
            notice.od_glyph(cx, ids!(glyph)).set(cx, Tone::Caution, Glyph::Info);
            notice.label(cx, ids!(title)).set_text(cx, t(Msg::InventoryUnavailable));
            notice.label(cx, ids!(detail)).set_text(cx, t(if snapshot.inventory_available {Msg::InventoryPartial}
                else if snapshot.resources.is_empty() {Msg::InventoryRetry} else {Msg::InventoryStale}));
        }
        self.show(cx, ids!(od_overview), has_data && tab == Tab::Overview);
        self.show(cx, ids!(od_resources_page), has_data && tab == Tab::Resources);
        self.show(cx, ids!(od_usage_page), has_data && tab == Tab::Usage);
        self.show(cx, ids!(od_activity_page), has_data && tab == Tab::Activity);

        self.sync_overview(cx, &p, locale);
        self.sync_resources(cx, &p, locale);
        self.sync_usage(cx, &p, locale);
        self.sync_activity(cx, &p, locale);
        self.view.redraw(cx);
    }

    fn sync_overview(&mut self, cx: &mut Cx, p: &Presentation, locale: Locale) {
        let t = |m| tr(locale, m);
        if let Some(h) = &p.hosting {
            self.view.od_glyph(cx, ids!(od_hosting_glyph)).set(cx, h.status.tone, h.status.glyph);
            self.label(cx, ids!(od_hosting_state), &h.title);
            self.label(cx, ids!(od_hosting_body), &h.body);
            self.show(cx, ids!(od_hosting_detail), h.detail.is_some());
            self.label(cx, ids!(od_hosting_detail), h.detail.as_deref().unwrap_or(""));
            self.label(cx, ids!(od_runs), &h.active_runs);
            self.label(cx, ids!(od_runs_label), t(Msg::ActiveRuns));
            self.label(cx, ids!(od_approvals), &h.pending_approvals);
            self.label(cx, ids!(od_approvals_label), t(Msg::PendingApprovals));
            let confirming = p.stop_confirm;
            self.button(cx, ids!(od_pause), t(Msg::PauseHosting), h.show_pause && !confirming, h.can_pause);
            self.button(cx, ids!(od_resume), t(Msg::ResumeHosting), h.show_resume && !confirming, h.can_resume);
            self.button(cx, ids!(od_stop), t(Msg::StopHosting), h.show_stop && !confirming, h.can_stop);
            self.button(cx, ids!(od_hosting_setup), t(Msg::SetUpHosting), h.show_setup, true);
            self.show(cx, ids!(od_hosting_summary), !confirming && p.command.is_none());
            self.show(cx, ids!(od_stop_sheet), confirming);
            self.label(cx, ids!(od_stop_title), t(Msg::StopConfirmTitle));
            self.label(cx, ids!(od_stop_body), t(Msg::StopConfirmBody));
            self.button(cx, ids!(od_stop_confirm), t(Msg::Stop), confirming, h.can_stop);
            self.button(cx, ids!(od_stop_cancel), t(Msg::Cancel), confirming, true);
        }
        self.show(cx, ids!(od_options_card), p.options.is_some());
        if let Some(options) = &p.options {
            self.label(cx, ids!(od_keep_running_title), options.keep_running_title);
            self.label(cx, ids!(od_keep_running_body), options.keep_running_body);
            let switch = self.view.check_box(cx, ids!(od_keep_running));
            if switch.active(cx) != options.keep_running {
                switch.set_active(cx, options.keep_running, Animate::No);
            }
            self.show(cx, ids!(od_foreign_service), options.foreign_service.is_some());
            self.label(cx, ids!(od_foreign_service), options.foreign_service.unwrap_or(""));
            let confirming = options.uninstall_confirm;
            self.show(cx, ids!(od_uninstall_sheet), confirming);
            self.label(cx, ids!(od_uninstall_title), t(Msg::UninstallConfirmTitle));
            self.label(cx, ids!(od_uninstall_body), t(Msg::UninstallConfirmBody));
            self.button(cx, ids!(od_uninstall_cancel), t(Msg::Cancel), confirming, true);
            self.button(cx, ids!(od_uninstall_confirm), t(Msg::Uninstall), confirming, !options.busy);
            self.show(cx, ids!(od_service_actions), !confirming);
            self.button(cx, ids!(od_use_this_service), t(Msg::UseThisService), options.foreign_service.is_some(), !options.busy);
            self.button(cx, ids!(od_uninstall), t(Msg::UninstallService), true, !options.busy);
        }
        self.show(cx, ids!(od_command_row), p.command.is_some());
        if let Some(line) = &p.command {
            self.view.od_glyph(cx, ids!(od_command_glyph)).set(cx, line.status.tone, line.status.glyph);
            self.label(cx, ids!(od_command_status), &line.status.text);
            self.button(cx, ids!(od_check_command), t(Msg::CheckStatus), line.can_check || matches!(self.state.command.as_ref().map(|c| &c.phase), Some(CommandPhase::Uncertain)), line.can_check);
            self.button(cx, ids!(od_retry_command), t(Msg::Retry), line.can_retry, true);
            self.button(cx, ids!(od_dismiss_command), t(Msg::Dismiss), line.can_dismiss, true);
        }

        // Attention.
        self.label(cx, ids!(od_attention_header), t(Msg::AttentionTitle));
        let wanted: Vec<_> = p.attention.iter().map(|item| (item.key.clone(), id!(Row))).collect();
        let rows = self.view.od_list(cx, ids!(od_attention)).sync(cx, &wanted);
        for (row, item) in rows.iter().zip(&p.attention) {
            row.od_glyph(cx, ids!(glyph)).set(cx, item.tone, item.glyph);
            row.label(cx, ids!(text)).set_text(cx, &item.text);
            let label = match item.target {
                crate::model::AttentionTarget::Approvals => t(Msg::ReviewApprovals),
                crate::model::AttentionTarget::Activity => t(Msg::TabActivity),
                crate::model::AttentionTarget::Usage => t(Msg::TabUsage),
                crate::model::AttentionTarget::Resources => t(Msg::TabResources),
                crate::model::AttentionTarget::Account(_) => t(Msg::Details),
                crate::model::AttentionTarget::Command => t(Msg::CheckStatus),
            };
            row.button(cx, ids!(od_attention_open)).set_text(cx, label);
        }
        let none = self.view.view(cx, ids!(od_attention_none));
        none.set_visible(cx, p.attention.is_empty());
        none.od_glyph(cx, ids!(glyph)).set(cx, Tone::Positive, Glyph::Check);
        none.label(cx, ids!(text)).set_text(cx, t(Msg::AttentionNone));

        // Usage summary (same component as the Usage tab).
        let summary = self.view.widget(cx, ids!(od_summary));
        fill_usage(cx, &summary, &p.overview_usage, locale);

        // Plans.
        self.label(cx, ids!(od_plans_header), t(Msg::PlansTitle));
        self.show(cx, ids!(od_plans_empty), p.accounts.is_empty());
        self.label(cx, ids!(od_plans_empty_text), t(Msg::PlansEmpty));
        let start = self.pager(cx, "accounts", self.view.widget(cx, ids!(od_accounts_pager)), p.accounts.len());
        let wanted: Vec<_> = p.accounts.iter().skip(start).take(PAGE_ROWS).map(|a| (a.id.clone(), id!(Account))).collect();
        let rows = self.view.od_list(cx, ids!(od_accounts)).sync(cx, &wanted);
        for (row, account) in rows.iter().zip(p.accounts.iter().skip(start)) {
            row.label(cx, ids!(title)).set_text(cx, &account.title);
            row.label(cx, ids!(subtitle)).set_text(cx, &account.subtitle);
            fill_badge(cx, &row.widget(cx, ids!(badge)), &account.status);
            row.label(cx, ids!(note)).set_text(cx, account.note.as_deref().unwrap_or(""));
            row.label(cx, ids!(note)).set_visible(cx, account.note.is_some());
            row.label(cx, ids!(observed)).set_text(cx, &account.observed);
            let wanted: Vec<_> = account.quotas.iter().map(|q| (q.id.clone(), id!(Quota))).collect();
            let quotas = row.od_list(cx, ids!(quotas)).sync(cx, &wanted);
            for (q_row, q) in quotas.iter().zip(&account.quotas) {
                q_row.label(cx, ids!(label)).set_text(cx, &q.label);
                q_row.label(cx, ids!(value)).set_text(cx, &q.value);
                q_row.od_meter(cx, ids!(meter)).set(cx, q.fraction, q.tone);
                q_row.label(cx, ids!(reset)).set_text(cx, &q.reset);
            }
            row.view(cx, ids!(res_section)).set_visible(cx, !account.resources.is_empty());
            row.label(cx, ids!(res_header)).set_text(cx, t(Msg::ResourcesOnAccount));
            let start = self.pager(cx, &format!("account:{}",account.id), row.widget(cx, ids!(pager)), account.resources.len());
            fill_resources(cx, &row.od_list(cx, ids!(resources)), &account.resources, start, locale, self.wide == Some(true));
        }
        self.show(cx, ids!(od_unassigned_card), !p.unassigned.is_empty());
        self.label(cx, ids!(od_unassigned_title), t(Msg::UnassignedAccount));
        self.label(cx, ids!(od_unassigned_note), t(Msg::UnassignedAccountNote));
        let start = self.pager(cx, "unassigned", self.view.widget(cx, ids!(od_unassigned_pager)), p.unassigned.len());
        fill_resources(cx, &self.view.od_list(cx, ids!(od_unassigned)), &p.unassigned, start, locale, self.wide == Some(true));
    }

    fn sync_resources(&mut self, cx: &mut Cx, p: &Presentation, locale: Locale) {
        let t = |m| tr(locale, m);
        let detail = p.detail.as_ref();
        self.show(cx, ids!(od_detail), detail.is_some());
        self.show(cx, ids!(od_resources_card), detail.is_none() && !p.resources.is_empty());
        let confirmed_inventory = self.state.snapshot.as_ref().is_some_and(|snapshot| snapshot.inventory_available && snapshot.inventory_complete);
        self.show(cx, ids!(od_resources_empty), detail.is_none() && p.resources.is_empty() && confirmed_inventory);
        self.label(cx, ids!(od_resources_empty_text), t(Msg::ResourcesEmpty));
        let start = self.pager(cx, "resources", self.view.widget(cx, ids!(od_resources_pager)), p.resources.len());
        fill_resources(cx, &self.view.od_list(cx, ids!(od_resources)), &p.resources, start, locale, self.wide == Some(true));
        if let Some(d) = detail {
            self.button(cx, ids!(od_detail_back), t(Msg::Back), true, true);
            self.label(cx, ids!(od_detail_title), &d.row.name);
            self.label(cx, ids!(od_detail_line), &d.row.line);
            let status = self.view.view(cx, ids!(od_detail_status));
            status.od_glyph(cx, ids!(glyph)).set(cx, d.row.status.tone, d.row.status.glyph);
            status.label(cx, ids!(text)).set_text(cx, &d.row.status.text);
            let wanted: Vec<_> = d.tokens.iter().map(|l| (l.label.to_string(), id!(Line))).collect();
            let lines = self.view.od_list(cx, ids!(od_detail_tokens)).sync(cx, &wanted);
            for (row, line) in lines.iter().zip(&d.tokens) {
                key_value_layout(cx, row, self.wide == Some(true));
                row.label(cx, ids!(key)).set_text(cx, line.label);
                row.label(cx, ids!(value)).set_text(cx, &line.value);
            }
            let ratio = self.view.view(cx, ids!(od_detail_ratio));
            key_value_layout(cx, &self.view.widget(cx, ids!(od_detail_ratio)), self.wide == Some(true));
            ratio.set_visible(cx, d.present);
            ratio.label(cx, ids!(key)).set_text(cx, t(Msg::CacheHitRatio));
            ratio.label(cx, ids!(value)).set_text(cx, &d.cache_ratio);
            self.label(cx, ids!(od_detail_facts_header), t(Msg::TechnicalDetails));
            let wanted: Vec<_> = d.facts.iter().map(|(k, _)| (k.to_string(), id!(Fact))).collect();
            let facts = self.view.od_list(cx, ids!(od_detail_facts)).sync(cx, &wanted);
            for (row, (k, v)) in facts.iter().zip(&d.facts) {
                key_value_layout(cx, row, false);
                row.label(cx, ids!(key)).set_text(cx, k);
                row.label(cx, ids!(value)).set_text(cx, v);
            }
        }
    }

    fn sync_usage(&mut self, cx: &mut Cx, p: &Presentation, locale: Locale) {
        let t = |m| tr(locale, m);
        let enlarged = self.compact_navigation || crate::theme::effective_text_scale(cx) > 1.3;
        self.show(cx, ids!(od_period), !enlarged);
        self.show(cx, ids!(od_period_picker_row), enlarged);
        let period_picker = self.view.drop_down(cx, ids!(od_period_picker));
        period_picker.set_labels(cx, [Msg::PeriodDay,Msg::PeriodWeek,Msg::PeriodMonth,Msg::CustomDates].iter().map(|msg| t(*msg).to_owned()).collect());
        period_picker.set_selected_item(cx, match self.state.view.period.unwrap_or(UsagePeriod::Day) { UsagePeriod::Day=>0,UsagePeriod::Week=>1,UsagePeriod::Month=>2,_=>3 });
        self.show(cx, ids!(od_usage_filters_card), self.state.view.filters_open);
        let filter_count = [&self.state.view.account_filter,&self.state.view.resource_filter,&self.state.view.model_filter,&self.state.view.requester_filter].iter().filter(|value| value.is_some()).count();
        let filter_title = t(if self.state.view.filters_open { Msg::HideFilters } else { Msg::UsageFilters });
        let filter_label = if filter_count > 0 { format!("{} · {}",filter_title,filter_count) } else { filter_title.into() };
        self.view.button(cx, ids!(od_usage_filters_toggle)).set_text(cx, &filter_label);
        for (path, msg) in [(ids!(od_custom_dates),Msg::CustomDates),(ids!(od_date_start_label),Msg::StartDate),(ids!(od_date_end_label),Msg::EndDate),(ids!(od_date_error),Msg::DateRangeError)] {
            self.label(cx,path,t(msg));
        }
        self.show(cx, ids!(od_date_error), self.date_error);
        self.view.button(cx, ids!(od_clear_filters)).set_text(cx, t(Msg::ClearFilters));
        self.view.button(cx, ids!(od_apply_dates)).set_text(cx, t(Msg::ApplyDates));
        for (path, dimension, label) in [(ids!(od_filter_account),3,Msg::CodingAccount),(ids!(od_filter_resource),0,Msg::FilterResource),(ids!(od_filter_model),1,Msg::FilterModel),(ids!(od_filter_requester),2,Msg::FilterRequester)] {
            let choices = self.filter_choices(dimension);
            let selected = match dimension { 0=>&self.state.view.resource_filter,1=>&self.state.view.model_filter,3=>&self.state.view.account_filter,_=>&self.state.view.requester_filter };
            let control = self.view.od_filter_picker(cx, path);
            control.set_locale(cx, locale);
            control.set_choices(cx, choices, selected, t(label).to_owned());
        }
        for (path, period, msg) in [
            (ids!(od_period_day), UsagePeriod::Day, Msg::PeriodDay),
            (ids!(od_period_week), UsagePeriod::Week, Msg::PeriodWeek),
            (ids!(od_period_month), UsagePeriod::Month, Msg::PeriodMonth),
        ] {
            let segment = self.view.view(cx, path);
            let selected = self.state.view.period.unwrap_or(UsagePeriod::Day) == period;
            segment.view(cx, ids!(sel)).set_visible(cx, selected);
            segment.button(cx, ids!(button)).set_text(cx, t(msg));
        }
        let wanted: Vec<_> = p.usage.filters.iter().map(|(id, _, _)| (id.clone().unwrap_or_else(|| ALL_FILTER.into()), id!(Chip))).collect();
        let chips = self.view.od_list(cx, ids!(od_filters)).sync(cx, &wanted);
        for (row, (_, label, on)) in chips.iter().zip(&p.usage.filters) {
            row.view(cx, ids!(sel)).set_visible(cx, *on);
            row.button(cx, ids!(chip)).set_text(cx, label);
        }
        self.show(cx, ids!(od_filters), false);
        fill_usage(cx, &self.view.widget(cx, ids!(od_usage)), &p.usage, locale);
        key_value_layout(cx, &self.view.widget(cx, ids!(od_cost_row)), self.wide == Some(true));
        let cost = self.view.view(cx, ids!(od_cost_row));
        cost.label(cx, ids!(key)).set_text(cx, t(Msg::EstimatedCost));
        cost.label(cx, ids!(value)).set_text(cx, &p.usage.cost);
        self.label(cx, ids!(od_cost_note), p.usage.cost_note);
        self.label(cx, ids!(od_usage_boundary), t(Msg::UsageBoundary));
        self.label(cx, ids!(od_by_resource_header), t(Msg::ByResource));
        self.show(cx, ids!(od_by_resource_card), !p.usage.by_resource.is_empty());
        let start = self.pager(cx, "usage", self.view.widget(cx, ids!(od_usage_pager)), p.usage.by_resource.len());
        let wanted: Vec<_> = p.usage.by_resource.iter().skip(start).take(PAGE_ROWS).map(|r| (r.id.clone(), id!(Row))).collect();
        let rows = self.view.od_list(cx, ids!(od_by_resource)).sync(cx, &wanted);
        for (row, r) in rows.iter().zip(p.usage.by_resource.iter().skip(start)) {
            row.label(cx, ids!(name)).set_text(cx, &r.name);
            row.label(cx, ids!(total)).set_text(cx, &r.total);
            fill_badge(cx, &row.widget(cx, ids!(badge)), &r.status);
            for (cell, label, value) in [
                (ids!(input), Msg::TokensInput, &r.input),
                (ids!(output), Msg::TokensOutput, &r.output),
                (ids!(cache_read), Msg::TokensCacheRead, &r.cache_read),
                (ids!(cache_write), Msg::TokensCacheWrite, &r.cache_write),
                (ids!(ratio), Msg::CacheHitRatio, &r.cache_ratio),
            ] {
                let cell = row.widget(cx, cell);
                cell.label(cx, ids!(label)).set_text(cx, t(label));
                cell.label(cx, ids!(value)).set_text(cx, value);
            }
        }
    }

    fn sync_activity(&mut self, cx: &mut Cx, p: &Presentation, locale: Locale) {
        self.show(cx, ids!(od_activity_empty), p.activities.is_empty());
        self.label(cx, ids!(od_activity_empty_text), tr(locale, Msg::ActivityEmpty));
        self.show(cx, ids!(od_activity_card), !p.activities.is_empty());
        let start = self.pager(cx, "activity", self.view.widget(cx, ids!(od_activity_pager)), p.activities.len());
        let wanted: Vec<_> = p.activities.iter().skip(start).take(PAGE_ROWS).map(|a| (a.id.clone(), id!(Row))).collect();
        let rows = self.view.od_list(cx, ids!(od_activity)).sync(cx, &wanted);
        for (row, a) in rows.iter().zip(p.activities.iter().skip(start)) {
            row.od_glyph(cx, ids!(glyph)).set(cx, a.status.tone, a.status.glyph);
            row.label(cx, ids!(title)).set_text(cx, &a.title);
            row.label(cx, ids!(detail)).set_text(cx, &a.detail);
            row.label(cx, ids!(detail)).set_visible(cx, !a.detail.is_empty());
            row.label(cx, ids!(meta)).set_text(cx, &format!("{} · {}", a.status.text, a.when));
        }
    }
}

fn fill_badge(cx: &mut Cx, badge: &WidgetRef, status: &Status) {
    badge.od_glyph(cx, ids!(glyph)).set(cx, status.tone, status.glyph);
    badge.label(cx, ids!(text)).set_text(cx, &status.text);
}

fn key_value_layout(cx: &mut Cx, row: &WidgetRef, wide: bool) {
    if let Some(mut view) = row.borrow_mut::<View>() {
        view.layout.flow = if wide { Flow::right() } else { Flow::Down };
        view.layout.spacing = if wide { 12.0 } else { 3.0 };
    }
    let value = row.label(cx, ids!(value));
    if let Some(mut label) = value.borrow_mut() {
        label.walk.width = if wide { Size::fit() } else { Size::fill() };
    }
}

fn fill_resources(cx: &mut Cx, list: &crate::controls::OdListRef, rows: &[ResourceRow], start: usize, locale: Locale, wide: bool) {
    let wanted: Vec<_> = rows.iter().skip(start).take(PAGE_ROWS).map(|r| (r.id.clone(), id!(Res))).collect();
    let widgets = list.sync(cx, &wanted);
    for (row, r) in widgets.iter().zip(rows.iter().skip(start)) {
        if let Some(mut view) = row.borrow_mut::<View>() {
            view.layout.flow = if wide { Flow::right() } else { Flow::Down };
            view.layout.spacing = if wide { 12.0 } else { 8.0 };
        }
        if let Some(mut actions) = row.view(cx, ids!(resource_actions)).borrow_mut() {
            actions.walk.width = if wide { Size::fit() } else { Size::fill() };
        }
        row.label(cx, ids!(name)).set_text(cx, &r.name);
        row.label(cx, ids!(line)).set_text(cx, &r.line);
        fill_badge(cx, &row.widget(cx, ids!(badge)), &r.status);
        row.label(cx, ids!(budget)).set_text(cx, &r.budget);
        let tokens = row.label(cx, ids!(tokens));
        tokens.set_text(cx, &r.tokens);
        if let Some(mut label) = tokens.borrow_mut() { label.walk.width = if wide { Size::fit() } else { Size::fill() }; }
        row.button(cx, ids!(od_resource_open)).set_text(cx, tr(locale, Msg::Details));
    }
}

fn fill_usage(cx: &mut Cx, root: &WidgetRef, usage: &UsageCard, locale: Locale) {
    let t = |m| tr(locale, m);
    root.label(cx, ids!(title)).set_text(cx, &usage.title);
    root.label(cx, ids!(total)).set_text(cx, &usage.total);
    root.label(cx, ids!(total_label)).set_text(cx, t(Msg::UsageTotal));
    let cells = [ids!(input), ids!(output), ids!(cache_read), ids!(cache_write)];
    for (path, line) in cells.into_iter().zip(&usage.lines) {
        let cell = root.widget(cx, path);
        cell.label(cx, ids!(label)).set_text(cx, line.label);
        cell.label(cx, ids!(value)).set_text(cx, &line.value);
    }
    let ratio = root.widget(cx, ids!(ratio));
    ratio.label(cx, ids!(label)).set_text(cx, t(Msg::CacheHitRatio));
    ratio.label(cx, ids!(value)).set_text(cx, &usage.cache_ratio);
    let completeness = root.widget(cx, ids!(completeness));
    completeness.od_glyph(cx, ids!(glyph)).set(cx, usage.completeness.tone, usage.completeness.glyph);
    completeness.label(cx, ids!(text)).set_text(cx, &usage.completeness.text);
    root.label(cx, ids!(observed)).set_text(cx, &usage.observed);
}

impl OwnerDashboardRef {
    fn with<R>(&self, f: impl FnOnce(&mut OwnerDashboard) -> R) -> Option<R> {
        self.borrow_mut().map(|mut inner| f(&mut inner))
    }

    /// Apply a snapshot whose usage period is not known to the host.
    pub fn set_snapshot(&self, cx: &mut Cx, snapshot: DashboardSnapshot) -> Option<SnapshotOutcome> {
        self.set_snapshot_for_period(cx, snapshot, None)
    }

    /// Apply a snapshot fetched for `period`. Older revisions and other
    /// owners are ignored (see [`SnapshotOutcome`]).
    pub fn set_snapshot_for_period(&self, cx: &mut Cx, snapshot: DashboardSnapshot, period: Option<UsagePeriod>) -> Option<SnapshotOutcome> {
        self.with(|d| {
            let outcome = d.state.apply_snapshot(snapshot, period);
            if outcome != SnapshotOutcome::Stale && outcome != SnapshotOutcome::WrongOwner {
                d.changed(cx);
            }
            outcome
        })
    }

    pub fn connection_failed(&self, cx: &mut Cx, message: &str) {
        self.with(|d| {
            d.state.connection_failed(message);
            d.changed(cx);
        });
    }

    pub fn set_connection(&self, cx: &mut Cx, status: ConnectionStatus) {
        self.with(|d| {
            d.state.set_connection(status);
            d.changed(cx);
        });
    }

    /// Record a command outcome. A definitive result triggers a refresh
    /// action so the next snapshot confirms the effect.
    pub fn command_result(&self, cx: &mut Cx, request_id: &str, result: CommandResult) {
        self.with(|d| {
            let now = d.clock().now_ms;
            if let Some(action) = d.state.command_result(request_id, result, now) {
                d.emit(cx, action);
            }
            d.changed(cx);
        });
    }

    /// Show the setup page (`Some`) or the dashboard (`None`).
    pub fn set_setup(&self, cx: &mut Cx, setup: Option<SetupView>) {
        self.with(|d| {
            if d.state.set_setup(setup) {
                d.changed(cx);
            }
        });
    }

    /// Host controls for a hosted owner, or `None` to hide them.
    pub fn set_options(&self, cx: &mut Cx, options: Option<HostOptions>) {
        self.with(|d| {
            if d.state.set_options(options) {
                d.changed(cx);
            }
        });
    }

    pub fn set_locale(&self, cx: &mut Cx, locale: Locale) {
        self.with(|d| {
            if d.state.locale != locale {
                d.state.locale = locale;
                d.changed(cx);
            }
        });
    }

    /// Switch the owner this view may show. Clears private state on change.
    pub fn set_owner_scope(&self, cx: &mut Cx, owner: Option<&str>) {
        self.with(|d| {
            if d.state.set_owner_scope(owner) {
                d.reset_owner_ui(cx);
                d.date_error = false;
                d.view.text_input(cx, ids!(od_date_start)).set_text(cx, "");
                d.view.text_input(cx, ids!(od_date_end)).set_text(cx, "");
                d.view.view(cx, ids!(od_content)).set_scroll_pos(cx, dvec2(0.0, 0.0));
                d.changed(cx);
            }
        });
    }

    /// Replace the spinner with a static glyph (system reduced motion).
    pub fn set_reduced_motion(&self, cx: &mut Cx, reduced: bool) {
        self.with(|d| {
            d.reduced_motion = reduced;
            d.changed(cx);
        });
    }

    /// Show (or clear) a host notice under the header: the outcome of a host
    /// action such as starting the service. Text must name action and reason.
    pub fn set_notice(&self, cx: &mut Cx, notice: Option<Status>) {
        self.with(|d| {
            d.notice = notice;
            d.changed(cx);
        });
    }

    pub fn locale(&self) -> Locale {
        self.borrow().map(|d| d.state.locale).unwrap_or_default()
    }

    pub fn recover_command(&self, cx: &mut Cx, command: crate::model::PendingCommand) {
        self.with(|d| { d.state.recover_command(command); d.changed(cx); });
    }

    /// Scoped UI state (navigation, filters, paging). No credentials.
    pub fn view_state(&self, cx: &Cx) -> ViewState {
        self.borrow()
            .map(|d| ViewState { scroll_y: d.scroll_y(cx), ..d.state.view.clone() })
            .unwrap_or_default()
    }

    /// Restore view state for the same owner. Returns false if refused.
    pub fn restore_view_state(&self, cx: &mut Cx, view: ViewState) -> bool {
        self.with(|d| {
            let scroll = view.scroll_y;
            let ok = d.state.restore_view(view);
            if ok {
                d.view.text_input(cx, ids!(od_date_start)).set_text(cx, &d.state.view.date_start);
                d.view.text_input(cx, ids!(od_date_end)).set_text(cx, &d.state.view.date_end);
                d.pending_scroll = Some(scroll);
                d.changed(cx);
            }
            ok
        })
        .unwrap_or(false)
    }

    /// Actions emitted by this dashboard in `actions`, in order.
    pub fn actions(&self, actions: &Actions) -> Vec<DashboardAction> {
        actions.filter_widget_actions_cast::<DashboardAction>(self.widget_uid()).collect()
    }

    /// A copy of the state (diagnostics and harness evidence).
    pub fn state(&self) -> Option<DashboardState> {
        self.borrow().map(|d| d.state.clone())
    }

    /// Number of in-place syncs so far (harness instrumentation).
    pub fn sync_count(&self) -> u64 {
        self.borrow().map(|d| d.sync_count).unwrap_or(0)
    }

    /// Use a fixed clock (fixtures and deterministic review only).
    pub fn set_clock(&self, cx: &mut Cx, clock: Option<Clock>) {
        self.with(|d| {
            d.clock_override = clock;
            d.changed(cx);
        });
    }

    /// Replace the whole state (fixtures and tests only).
    #[cfg(any(test, feature = "fixtures"))]
    pub fn load_state(&self, cx: &mut Cx, state: DashboardState) {
        self.with(|d| {
            let locale = d.state.locale;
            if d.state.owner_scope != state.owner_scope { d.reset_owner_ui(cx); }
            d.state = DashboardState { locale, ..state };
            d.view.view(cx, ids!(od_content)).set_scroll_pos(cx, dvec2(0.0, 0.0));
            d.changed(cx);
        });
    }
}

#[cfg(test)]
mod tests;
