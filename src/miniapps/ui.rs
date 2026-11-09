//! One native mini-app screen shared by standalone and embedded Rinx.
use super::library::{LibraryAction, MiniAppLibraryWidgetExt};
use crate::i18n::Language;
use super::{
    ContextProvider, InstanceId, Lease, OctosProvider, ServiceEvent,
    package::{Call, Package},
};
use makepad_widgets::splash_host::{splash_host_respond, take_splash_host_requests_for};
use makepad_widgets::*;
use octoscript_ui_l0::InstanceStore;
use octosense_app_contract::AssetServer;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

#[path = "execution_ui.rs"]
mod execution_ui;
#[path = "sheet_ui.rs"]
mod sheet_ui;
#[path = "grant_ui.rs"]
mod grant_ui;
#[path = "adoption_ui.rs"]
mod adoption_ui;
#[path = "confirmation_render.rs"]
mod confirmation_render;
#[path = "reauthentication_ui.rs"]
mod reauthentication_ui;
#[path = "resource_enrollment_ui.rs"]
mod resource_enrollment_ui;
#[path = "resource_edit_ui.rs"]
mod resource_edit_ui;
#[path = "resource_create_ui.rs"]
mod resource_create_ui;
#[path = "personal_configuration_ui.rs"]
mod personal_configuration_ui;
#[path = "web_connections_ui.rs"]
mod web_connections_ui;
#[path = "personal_folder_picker.rs"]
mod personal_folder_picker;
#[path = "account_login_ui.rs"]
mod account_login_ui;
#[path = "startup.rs"]
mod startup;
#[path = "owner_approvals.rs"]
mod owner_approvals;
pub use super::sandbox::SYSTEM_APP_TIME_BUDGET;
pub use startup::hagency_root_installed;
#[path = "locale.rs"]
mod locale;
#[cfg(test)]
#[path = "agent_capabilities_tests.rs"]
mod agent_capabilities_tests;
#[cfg(test)]
#[path = "operator_tasks_tests.rs"]
mod operator_tasks_tests;

// Mirror the server's existing-room eligibility for useful choices; Palpo
// re-fetches the authoritative state under the caller's identity on submission.
fn eligible_project_room(room: &matrix_sdk::Room, account: &ruma::UserId) -> bool {
    !room.is_space()
        && !room.encryption_state().is_encrypted()
        && room.join_rule() == Some(ruma::events::room::join_rules::JoinRule::Invite)
        && room.create_content().is_some_and(|c| c.creator == account)
}

#[derive(Clone, Debug)]
pub enum MiniAppsAction {
    Open,
    /// The Agent sidebar entry: open or focus Hagency without notification routing.
    OpenAgent,
    /// Read-only owner review landing; never a personal marker-inbox route.
    ReviewOwnerApprovals { owner: String },
    /// The assistant opened the reviewed app: show its review; Run grants it.
    OpenReviewed,
    /// Untrusted notification routing hint; backend authorization remains mandatory.
    OpenPalpo(String),
    /// Resolved by Palpo's account worker; host navigation never emits a verdict.
    OpenSignupApproval(super::palpo::SignupApprovalTarget),
    /// Server-resolved ready agent in a project room joined by this account.
    OpenAgentChat(super::palpo::AgentChatTarget),
    OpenActionsRoom(super::palpo::ActionsRoomTarget),
    /// Locally resolved project; the existing native inbox rechecks room markers.
    OpenToolApprovals(crate::host::hagency::tool_approvals::ToolApprovalTarget),
    Close,
}
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    // Native confirmation sheets are one family: publishing a month, changing
    // it, adding people, publishing ahead and the monthly maximum all show a
    // status icon and title, a before → after hero, grouped facts, the people
    // affected, one disclosure, and two full-width buttons at the bottom.
    let SheetGroup = RoundedView {width: Fill height: Fit flow: Down show_bg: true
        draw_bg +: {color: RINX_SURFACE border_radius: 12.0 border_size: 0.0}}
    let SheetLine = SolidView {width: Fill height: 1 margin: Inset{left: 16} draw_bg.color: RINX_BORDER}
    // Label and hint padding renders about twice as wide, so 8 lines text up with 16 pt rows.
    let SheetCaption = RinxHint {width: Fill flow: Flow.Right{wrap: true} padding: Inset{left: 8 right: 8}}
    // A text button with no box: 44 pt tall, in the accent unless restyled.
    let SheetTextButton = RinxButton {width: Fit height: 44 margin: 0 padding: Inset{left: 10 right: 10 top: 8 bottom: 8} grab_key_focus: false
        draw_bg +: {color: #0000 color_focus: #0000 color_hover: RINX_HOVER color_down: RINX_PRESSED
            border_color: #0000 border_color_hover: #0000 border_color_down: #0000 border_color_focus: RINX_ACCENT
            border_color_2: #0000 border_color_2_hover: #0000 border_color_2_down: #0000 border_color_2_focus: RINX_ACCENT}
        draw_text +: {color: RINX_ACCENT color_hover: RINX_ACCENT color_down: RINX_ACCENT color_focus: RINX_ACCENT}
    }
    // A chosen preset: the selected tint and accent text, like the pool page's chosen amount.
    let SheetPresetOn = RinxKeyButton {visible: false width: Fill height: 44 margin: 0 grab_key_focus: false
        draw_bg +: {color: RINX_SELECTED color_hover: RINX_SELECTED color_down: RINX_SELECTED color_focus: RINX_SELECTED
            border_color: RINX_ACCENT border_color_hover: RINX_ACCENT border_color_down: RINX_ACCENT border_color_focus: RINX_ACCENT
            border_color_2: RINX_ACCENT border_color_2_hover: RINX_ACCENT border_color_2_down: RINX_ACCENT border_color_2_focus: RINX_ACCENT}
        draw_text +: {color: RINX_ACCENT color_hover: RINX_ACCENT color_down: RINX_ACCENT color_focus: RINX_ACCENT}
    }
    // A whole grouped row that is one tap target, like an iOS settings row.
    let SheetRowButton = SheetTextButton {width: Fill height: 48 align: Align{x: 0.0 y: 0.5} padding: Inset{left: 16 right: 16 top: 8 bottom: 8}
        draw_bg +: {border_radius: 12.0}}
    // One fact: its name, its value on the right, and an optional note below.
    let SheetRow = View {visible: false width: Fill height: Fit flow: Down
        line := SheetLine {}
        // With large text the value goes under its name instead of squeezing it.
        body := View {width: Fill height: Fit flow: (if mod.widgets.RINX_TEXT_SCALE > 1.2 {Flow.Down} else {Flow.Right}) spacing: (if mod.widgets.RINX_TEXT_SCALE > 1.2 {2} else {12}) padding: Inset{left: 16 right: 16 top: 12 bottom: 12} align: Align{y: 0.5}
            label := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
            // Under its name it takes the row's width, so a long value wraps instead of running off.
            value := RinxLabel {width: (if mod.widgets.RINX_TEXT_SCALE > 1.2 {Fill} else {Fit}) flow: Flow.Right{wrap: true} draw_text +: {color: RINX_MUTED}}
        }
        note := RinxHint {visible: false width: Fill flow: Flow.Right{wrap: true} padding: Inset{left: 8 right: 8 bottom: 5}}
    }
    // One person who can request: already listed people are locked, people
    // added in this window carry a tag and can be removed before confirming.
    let SheetPerson = View {visible: false width: Fill height: Fit flow: Down
        line := SheetLine {}
        body := View {width: Fill height: Fit flow: Right spacing: 10 padding: Inset{left: 16 right: 6 top: 2 bottom: 2} align: Align{y: 0.5}
            RoundedView {width: 30 height: 30 align: Align{x: 0.5 y: 0.5} show_bg: true draw_bg +: {color: RINX_SELECTED border_radius: 15.0}
                Icon {width: 16 height: 16 icon_walk: Walk{width: 16 height: 16} draw_icon +: {svg: mod.rinx_icons.person color: RINX_ACCENT}}
            }
            name := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
            tag := RoundedView {visible: false width: Fit height: Fit padding: Inset{left: 8 right: 8 top: 3 bottom: 3} show_bg: true draw_bg +: {color: RINX_SELECTED border_radius: 9.0}
                tag_text := RinxHint {width: Fit draw_text +: {color: RINX_ACCENT}}
            }
            locked := View {visible: false width: 44 height: 44 align: Align{x: 0.5 y: 0.5}
                Icon {width: 15 height: 15 icon_walk: Walk{width: 15 height: 15} draw_icon +: {svg: mod.rinx_icons.lock color: RINX_MUTED}}
            }
            remove := SheetTextButton {visible: false text: #(crate::i18n::tr("Remove")) i18n_text: "Remove"
                draw_text +: {color: RINX_DANGER_FG color_hover: RINX_DANGER_FG color_down: RINX_DANGER_FG color_focus: RINX_DANGER_FG}}
        }
    }
    // The sheet's status: a tinted circle with what the window does, then a
    // green check once it worked, a red warning when it was refused, an amber
    // mark when the outcome isn't known yet, or a spinner while it works.
    let SheetBadge = RoundedView {visible: false width: 60 height: 60 align: Align{x: 0.5 y: 0.5} show_bg: true
        draw_bg +: {color: RINX_SELECTED border_radius: 30.0}}
    let SheetGlyph = Icon {width: 28 height: 28 icon_walk: Walk{width: 28 height: 28} draw_icon +: {color: RINX_ACCENT}}
    let SheetFigure = View {width: Fit height: Fit flow: Down spacing: 2
        // Big numbers grow less than body text with large text, so before → after still fits a phone.
        value := RinxLabel {width: Fit draw_text +: {text_style: theme.font_bold{font_size: (25 * (0.6 + 0.4 * mod.widgets.RINX_TEXT_SCALE))}}}
        tag := RinxHint {width: Fit}
    }
    let NativeSheet = View {visible: false width: Fill height: Fill flow: Down spacing: 12 align: Align{x: 0.5}
        scroll := ScrollYView {width: Fill height: Fill flow: Down spacing: 18 align: Align{x: 0.5} padding: Inset{left: 4 right: 14 top: 6 bottom: 10}
            heading := View {width: Fill{max: 640} height: Fit flow: Down spacing: 8 align: Align{x: 0.5}
                icons := View {width: Fill height: Fit align: Align{x: 0.5}
                    glyph_publish := SheetBadge {SheetGlyph {draw_icon +: {svg: mod.widgets.ICON_SHARE}}}
                    glyph_change := SheetBadge {SheetGlyph {draw_icon +: {svg: mod.rinx_icons.adjust}}}
                    glyph_people := SheetBadge {SheetGlyph {draw_icon +: {svg: mod.widgets.ICON_ADD_USER}}}
                    glyph_renew := SheetBadge {SheetGlyph {draw_icon +: {svg: mod.rinx_icons.sync}}}
                    glyph_maximum := SheetBadge {SheetGlyph {draw_icon +: {svg: mod.rinx_icons.gauge}}}
                    glyph_working := SheetBadge {LoadingSpinner {width: 30 height: 30 draw_bg +: {color: RINX_ACCENT stroke_width: 3.0}}}
                    glyph_done := SheetBadge {draw_bg +: {color: RINX_SUCCESS_BG} SheetGlyph {draw_icon +: {svg: mod.rinx_icons.check color: RINX_SUCCESS_FG}}}
                    glyph_failed := SheetBadge {draw_bg +: {color: RINX_DANGER_BG} SheetGlyph {draw_icon +: {svg: mod.rinx_icons.alert color: RINX_DANGER_FG}}}
                    glyph_unsure := SheetBadge {draw_bg +: {color: RINX_WARNING_BG} SheetGlyph {draw_icon +: {svg: mod.rinx_icons.info color: RINX_WARNING_FG}}}
                }
                title := RinxPageTitle {width: Fill align: Align{x: 0.5} flow: Flow.Right{wrap: true}}
                pool := RinxHint {width: Fill align: Align{x: 0.5} flow: Flow.Right{wrap: true}}
            }
            problem_banner := RoundedView {visible: false width: Fill{max: 640} height: Fit flow: Right spacing: 10 padding: Inset{left: 14 right: 14 top: 12 bottom: 12} show_bg: true
                draw_bg +: {color: RINX_DANGER_BG border_radius: 12.0}
                Icon {width: 20 height: 20 icon_walk: Walk{width: 20 height: 20} draw_icon +: {svg: mod.rinx_icons.alert color: RINX_DANGER_FG}}
                problem := RinxLabel {width: Fill flow: Flow.Right{wrap: true} draw_text +: {color: RINX_DANGER_FG}}
            }
            // What changes, in big numbers: before → after, then one sentence.
            hero := SheetGroup {visible: false width: Fill{max: 640} padding: Inset{left: 18 right: 18 top: 14 bottom: 16} spacing: 8
                caption := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                // Top-aligned columns; the smaller "before" value and the arrow
                // are nudged down to sit on the big value's line.
                figures := View {width: Fill height: Fit flow: Right spacing: 14 align: Align{y: 0.0}
                    before := SheetFigure {visible: false padding: Inset{top: (6 * (0.6 + 0.4 * mod.widgets.RINX_TEXT_SCALE))} value +: {draw_text +: {color: RINX_MUTED text_style: theme.font_bold{font_size: (19 * (0.6 + 0.4 * mod.widgets.RINX_TEXT_SCALE))}}}}
                    arrow := View {visible: false width: Fit height: Fit padding: Inset{top: (19 * (0.6 + 0.4 * mod.widgets.RINX_TEXT_SCALE))}
                        Icon {width: 18 height: 18 icon_walk: Walk{width: 18 height: 18} draw_icon +: {svg: mod.rinx_icons.chevron color: RINX_MUTED}}
                    }
                    after := SheetFigure {}
                }
                lead := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
            }
            status := RinxLabel {visible: false width: Fill{max: 640} flow: Flow.Right{wrap: true} padding: Inset{left: 8 right: 8}}
            // A consequence the owner must know before confirming.
            caution := RoundedView {visible: false width: Fill{max: 640} height: Fit flow: Right spacing: 10 padding: Inset{left: 14 right: 14 top: 12 bottom: 12} show_bg: true
                draw_bg +: {color: RINX_WARNING_BG border_radius: 12.0}
                Icon {width: 18 height: 18 icon_walk: Walk{width: 18 height: 18} draw_icon +: {svg: mod.rinx_icons.alert color: RINX_WARNING_FG}}
                caution_text := RinxLabel {width: Fill flow: Flow.Right{wrap: true} draw_text +: {color: RINX_WARNING_FG}}
            }
            change := RinxHint {visible: false width: Fill{max: 640} flow: Flow.Right{wrap: true} padding: Inset{left: 8 right: 8}}
            facts := View {visible: false width: Fill{max: 640} height: Fit flow: Down spacing: 6
                facts_label := SheetCaption {}
                SheetGroup {
                    r0 := SheetRow {} r1 := SheetRow {} r2 := SheetRow {} r3 := SheetRow {} r4 := SheetRow {} r5 := SheetRow {}
                }
            }
            quota := View {visible: false width: Fill{max: 640} height: Fit flow: Down spacing: 6
                quota_label := SheetCaption {text: #(crate::i18n::tr("Per-request limit")) i18n_text: "Per-request limit"}
                SheetGroup {padding: Inset{left: 12 right: 12 top: 6 bottom: 12} spacing: 4
                    // The value box sits beside the slider; with large text it moves under it so the slider keeps its length.
                    tokens := RinxNumberInput {width: Fill flow: (if mod.widgets.RINX_TEXT_SCALE > 1.2 {Flow.Right{wrap: true}} else {Flow.Right}) millions: true slider_mode: true maximum: 0 minimum: 1 empty_text: "1" input +: {width: 80}}
                    quota_hint := RinxHint {width: Fill flow: Flow.Right{wrap: true} padding: Inset{left: 4 right: 4}}
                }
            }
            request_quota := View {visible: false width: Fill{max: 640} height: Fit flow: Down spacing: 6
                label := SheetCaption {}
                SheetGroup {padding: Inset{left: 12 right: 12 top: 6 bottom: 12} spacing: 4
                    limit := RinxNumberInput {width: Fill millions: true slider_mode: true maximum: 0 minimum: 1 empty_text: "" input +: {width: 80}}
                    hint := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                }
            }
            calendar := View {visible: false width: Fill{max: 640} height: Fit flow: Down spacing: 6
                title := SheetCaption {}
                SheetGroup {padding: 12 spacing: 10
                    cadence := View {width: Fill height: Fit flow: Right spacing: 6
                        daily := RinxKeyButton {width: Fill height: 44 margin: 0 text: "Daily"}
                        weekly := RinxKeyButton {width: Fill height: 44 margin: 0 text: "Weekly"}
                        monthly := RinxKeyButton {width: Fill height: 44 margin: 0 text: "Monthly"}
                    }
                    zone_label := RinxLabel {width: Fill}
                    zone := RinxInput {width: Fill height: 44 empty_text: "America/Los_Angeles"}
                    zones := DropDown {width: Fill height: 44 margin: 0 labels: ["Choose a time zone"]
                        draw_text +: {color: RINX_INK color_hover: RINX_INK color_focus: RINX_INK color_down: RINX_INK}
                        popup_menu: PopupMenu {width: 300 menu_item: PopupMenuItem {height: 36}}
                    }
                    note := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                }
            }
            pool_renewal := View {visible: false width: Fill{max: 640} height: Fit flow: Down spacing: 6
                title := SheetCaption {}
                SheetGroup {padding: 12 spacing: 10
                    choices := View {width: Fill height: Fit flow: Right spacing: 6
                        automatic := RinxKeyButton {width: Fill height: 44 margin: 0 text: "Automatic"}
                        manual := RinxKeyButton {width: Fill height: 44 margin: 0 text: "Manual"}
                    }
                    note := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                }
            }
            people := View {visible: false width: Fill{max: 640} height: Fit flow: Down spacing: 6
                people_label := SheetCaption {}
                list := SheetGroup {
                    // A list the window doesn't change: one row that opens in place.
                    people_toggle_row := View {visible: false width: Fill height: 48 flow: Overlay
                        people_toggle := SheetRowButton {text: ""}
                        people_toggle_body := View {width: Fill height: Fill flow: Right spacing: 8 padding: Inset{left: 16 right: 16} align: Align{y: 0.5}
                            people_toggle_label := RinxLabel {width: Fill}
                            people_toggle_value := RinxLabel {width: Fit draw_text +: {color: RINX_MUTED}}
                            people_closed := View {width: Fit height: Fit
                                Icon {width: 14 height: 14 icon_walk: Walk{width: 14 height: 14} draw_icon +: {svg: mod.rinx_icons.expand color: RINX_MUTED}}
                            }
                            people_opened := View {visible: false width: Fit height: Fit
                                Icon {width: 14 height: 14 icon_walk: Walk{width: 14 height: 14} draw_icon +: {svg: mod.rinx_icons.collapse color: RINX_MUTED}}
                            }
                        }
                    }
                    p0 := SheetPerson {} p1 := SheetPerson {} p2 := SheetPerson {} p3 := SheetPerson {}
                    p4 := SheetPerson {} p5 := SheetPerson {} p6 := SheetPerson {} p7 := SheetPerson {}
                    more_people := RinxHint {visible: false width: Fill flow: Flow.Right{wrap: true} padding: Inset{left: 8 right: 8 top: 5 bottom: 5}}
                    add := View {visible: false width: Fill height: Fit flow: Down
                        add_line := SheetLine {}
                        View {width: Fill height: Fit flow: Right spacing: 8 padding: Inset{left: 12 right: 6 top: 6 bottom: 6} align: Align{y: 0.5}
                            add_name := RinxInput {width: Fill height: 44 empty_text: ""}
                            add_button := SheetTextButton {text: #(crate::i18n::tr("Search")) i18n_text: "Search"}
                        }
                        directory := View {visible: false width: Fill height: Fit flow: Down spacing: 6 padding: Inset{left: 12 right: 12 bottom: 8}
                            View {width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                                candidates := DropDown {width: Fill height: 44 margin: 0 labels: ["Select a person to add"]
                                    draw_text +: {color: RINX_INK color_hover: RINX_INK color_focus: RINX_INK color_down: RINX_INK}
                                    popup_menu: PopupMenu {width: 300 menu_item: PopupMenuItem {height: 36}}
                                }
                                add_selected := SheetTextButton {text: #(crate::i18n::tr("Add")) i18n_text: "Add"}
                            }
                            status := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                        }
                        add_error := RinxHint {visible: false width: Fill flow: Flow.Right{wrap: true} padding: Inset{left: 8 right: 8 bottom: 5} draw_text +: {color: RINX_DANGER_FG}}
                        add_hint := RinxHint {visible: false width: Fill flow: Flow.Right{wrap: true} padding: Inset{left: 8 right: 8 bottom: 5}}
                    }
                    // A record of this month's list: add people from here, in this window.
                    open_add := View {visible: false width: Fill height: Fit flow: Down
                        SheetLine {}
                        open_add_button := SheetRowButton {text: #(crate::i18n::tr("Add People Who Can Request")) i18n_text: "Add People Who Can Request"}
                    }
                }
                people_note := SheetCaption {}
            }
            // When sharing ends: a row of its own that opens the period choice in place.
            sharing := View {visible: false width: Fill{max: 640} height: Fit flow: Down
                SheetGroup {
                    sharing_row := View {width: Fill height: 48 flow: Overlay
                        sharing_toggle := SheetRowButton {text: ""}
                        sharing_body := View {width: Fill height: Fill flow: Right spacing: 8 padding: Inset{left: 16 right: 16} align: Align{y: 0.5}
                            sharing_label := RinxLabel {width: Fill text: #(crate::i18n::tr("Shared until")) i18n_text: "Shared until"}
                            sharing_value := RinxLabel {width: Fit draw_text +: {color: RINX_MUTED}}
                            sharing_closed := View {width: Fit height: Fit
                                Icon {width: 14 height: 14 icon_walk: Walk{width: 14 height: 14} draw_icon +: {svg: mod.rinx_icons.expand color: RINX_MUTED}}
                            }
                            sharing_opened := View {visible: false width: Fit height: Fit
                                Icon {width: 14 height: 14 icon_walk: Walk{width: 14 height: 14} draw_icon +: {svg: mod.rinx_icons.collapse color: RINX_MUTED}}
                            }
                        }
                    }
                    // With large text the time wraps on its own lines under the row's name.
                    sharing_value_below := RinxLabel {visible: false width: Fill flow: Flow.Right{wrap: true} padding: Inset{left: 8 right: 8 bottom: 6} draw_text +: {color: RINX_MUTED}}
                    // The chosen period shows selected, in the tint of a selected segment.
                    period := View {visible: false width: Fill height: Fit flow: Down spacing: 8 padding: Inset{left: 16 right: 16 top: 0 bottom: 12}
                        SheetLine {margin: 0}
                        expiry_presets := View {width: Fill height: Fit flow: Right spacing: 8 padding: Inset{top: 4}
                            day := RinxKeyButton {width: Fill height: 44 margin: 0 text: #(crate::i18n::tr("1 day")) i18n_text: "1 day"}
                            day_on := SheetPresetOn {text: #(crate::i18n::tr("1 day")) i18n_text: "1 day"}
                            week := RinxKeyButton {width: Fill height: 44 margin: 0 text: #(crate::i18n::tr("7 days")) i18n_text: "7 days"}
                            week_on := SheetPresetOn {text: #(crate::i18n::tr("7 days")) i18n_text: "7 days"}
                            month := RinxKeyButton {width: Fill height: 44 margin: 0 text: #(crate::i18n::tr("Month end")) i18n_text: "Month end"}
                            month_on := SheetPresetOn {text: #(crate::i18n::tr("Month end")) i18n_text: "Month end"}
                        }
                        expiry_note := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                    }
                }
            }
            // Recovery steps for a saved record, as plain grouped rows.
            actions := SheetGroup {visible: false width: Fill{max: 640}
                recover := SheetRowButton {text: #(crate::i18n::tr("Check Again")) i18n_text: "Check Again"}
                actions_line := SheetLine {}
                cancel_intent := SheetRowButton {text: #(crate::i18n::tr("Discard This Publish…")) i18n_text: "Discard This Publish…"
                    draw_text +: {color: RINX_DANGER_FG color_hover: RINX_DANGER_FG color_down: RINX_DANGER_FG color_focus: RINX_DANGER_FG}}
            }
            // Technical details live behind one row of their own.
            more := SheetGroup {visible: false width: Fill{max: 640}
                more_row := View {width: Fill height: 48 flow: Overlay
                    more_toggle := SheetRowButton {text: ""}
                    more_body := View {width: Fill height: Fill flow: Right spacing: 8 padding: Inset{left: 16 right: 16} align: Align{y: 0.5}
                        more_label := RinxLabel {width: Fill draw_text +: {color: RINX_ACCENT}}
                        more_closed := View {width: Fit height: Fit
                            Icon {width: 14 height: 14 icon_walk: Walk{width: 14 height: 14} draw_icon +: {svg: mod.rinx_icons.expand color: RINX_MUTED}}
                        }
                        more_opened := View {visible: false width: Fit height: Fit
                            Icon {width: 14 height: 14 icon_walk: Walk{width: 14 height: 14} draw_icon +: {svg: mod.rinx_icons.collapse color: RINX_MUTED}}
                        }
                    }
                }
                details := View {visible: false width: Fill height: Fit flow: Down spacing: 6 padding: Inset{left: 16 right: 16 top: 0 bottom: 14}
                    SheetLine {margin: 0}
                    details_label := RinxHint {width: Fill padding: Inset{top: 6} text: #(crate::i18n::tr("For your server administrator")) i18n_text: "For your server administrator"}
                    copy := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                }
            }
        }
        // One secondary and one primary action side by side, full width.
        buttons := View {width: Fill{max: 640} height: Fit flow: Right spacing: 12 padding: Inset{right: 14}
            cancel := RinxButton {width: Fill height: 50 margin: 0 grab_key_focus: false text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel" draw_bg +: {border_radius: 12.0}}
            primary := RinxPrimaryButton {width: Fill height: 50 margin: 0 grab_key_focus: false text: #(crate::i18n::tr("Publish")) i18n_text: "Publish" draw_bg +: {border_radius: 12.0}}
        }
    }
    mod.widgets.MiniAppsPanel = #(MiniAppsPanel::register_widget(vm)) {
        width: Fill height: Fill flow: Down padding: 20 spacing: 16
        show_bg: true
        draw_bg +: {color: instance(mod.widgets.RINX_PAGE) pixel: fn() {return self.color}}
        header := View {width: Fill height: Fit flow: Right spacing: 12
            close := RinxButton {width: 44 text: "" icon_walk: Walk{width: 20 height: 20} draw_icon.svg: ICON_ARROW_BACK}
            title := RinxPageTitle {text: #(crate::i18n::tr("Mini apps")) i18n_text: "Mini apps"}
        }
        library := MiniAppLibrary {visible: false}
        catalog := View {width: Fill height: Fill flow: Down spacing: 12
            catalog_title := RinxLabel {text: #(crate::i18n::tr("Built-in apps")) i18n_text: "Built-in apps"}
            catalog_list := PortalList {width: Fill height: Fill
                App := RoundedView {width: Fill height: 104 padding: 16 spacing: 12 flow: Right align: Align{y: 0.5}
                    draw_bg +: {color: RINX_SURFACE border_color: RINX_BORDER border_size: 1 border_radius: theme.corner_radius}
                    article_icon := View {width: Fit height: Fit
                        Icon {width: 28 height: 28 draw_icon +: {svg: ICON_FILE color: RINX_ACCENT}}
                    }
                    operations_icon := View {visible: false width: Fit height: Fit
                        Icon {width: 28 height: 28 draw_icon +: {svg: ICON_SQUARES color: RINX_ACCENT}}
                    }
                    copy := View {width: Fill height: Fit flow: Down spacing: 6
                        name := RinxLabel {width: Fill draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
                        subtitle := RinxHint {width: Fill text: ""}
                    }
                    launch := RinxPrimaryButton {text: #(crate::i18n::tr("Open")) i18n_text: "Open"}
                }
            }
            browse_hub := RinxButton {width: Fill text: "App Hub"}
            import_app := RinxButton {width: Fill text: #(crate::i18n::tr("Import an app")) i18n_text: "Import an app"}
        }
        import_form := View {visible: false width: Fill height: Fit flow: Down spacing: 8
            bundle_path := View {width: Fill height: Fit
                path := TextInput {width: Fill empty_text: "OctoSense bundle folder"}
            }
            room_access := View {width: Fill height: Fit
                room := TextInput {width: Fill empty_text: "Room ID to allow (optional)"}
            }
            assistant_status := Label {width: Fill draw_text.color: mod.widgets.RINX_MUTED text: ""}
            core := View {width: Fill height: Fit flow: Down spacing: 6
                local_family := TextInput {width: Fill empty_text: "Assistant on this device: provider (e.g. deepseek)"}
                local_model := TextInput {width: Fill empty_text: "Model"}
                local_base_url := TextInput {width: Fill empty_text: "Base URL (optional)"}
                local_key := TextInput {width: Fill is_password: true empty_text: "API key (kept in Rinx's own runtime)"}
                local_buttons := View {width: Fill height: Fit spacing: 8
                    use_local := RinxButton {text: "Use this device"}
                    turn_off := RinxButton {text: "Turn assistant off"}
                }
                endpoint := TextInput {width: Fill empty_text: "Octos server URL"}
                profile := TextInput {width: Fill empty_text: "Octos profile"}
                token := TextInput {width: Fill is_password: true empty_text: "Octos access token"}
                connect := RinxButton {text: "Connect Octos"}
            }
            buttons := View {width: Fill height: Fit spacing: 8
                review := RinxButton {text: "Review bundle"}
                run := RinxButton {text: #(crate::i18n::tr("Run")) i18n_text: "Run"}
            }
        }
        hagency_review := View {visible: false width: Fill height: Fill flow: Down spacing: 16
            permissions_scroll := ScrollYView {width: Fill height: Fill flow: Down spacing: 16
                RinxPageTitle {width: Fill text: #(crate::i18n::tr("Hagency permissions")) i18n_text: "Hagency permissions"}
                account := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                RinxLabel {width: Fill flow: Flow.Right{wrap: true}
                    text: #(crate::i18n::tr("Hagency uses your current Matrix account.")) i18n_text: "Hagency uses your current Matrix account."
                }
                permissions := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
                RinxHint {width: Fill flow: Flow.Right{wrap: true}
                    text: #(crate::i18n::tr("Approval remains limited to your assigned server role. Provider credentials stay in the native host."))
                    i18n_text: "Approval remains limited to your assigned server role. Provider credentials stay in the native host."
                }
                RinxHint {width: Fill flow: Flow.Right{wrap: true}
                    text: #(crate::i18n::tr("Run remembers consent for this account and this exact bundled version."))
                    i18n_text: "Run remembers consent for this account and this exact bundled version."
                }
            }
            buttons := View {width: Fill height: Fit flow: Right spacing: 12
                hagency_cancel := RinxButton {text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"}
                hagency_run := RinxPrimaryButton {text: #(crate::i18n::tr("Run")) i18n_text: "Run"}
            }
        }
        notice := RinxLabel {width: Fill height: Fit flow: Flow.Right{wrap: true} text: ""}
        approval := View {visible: false width: Fill height: Fit flow: Down spacing: 8
            details := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
            buttons := View {width: Fill height: Fit spacing: 8
                allow := RinxButton {text: "Allow once"}
                deny := RinxButton {text: "Deny"}
            }
        }
        account_confirmation := View {visible: false width: Fill height: Fill flow: Down spacing: 16
            scroll := ScrollYView {width: Fill height: Fill flow: Down spacing: 16
                RinxPageTitle {width: Fill text: #(crate::i18n::tr("Sign in again")) i18n_text: "Sign in again"}
                RinxLabel {width: Fill flow: Flow.Right{wrap: true} text: #(crate::i18n::tr("Continue with this Matrix account")) i18n_text: "Continue with this Matrix account"}
                account := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
                RinxHint {width: Fill flow: Flow.Right{wrap: true}
                    text: #(crate::i18n::tr("Hagency needs a fresh sign-in to restore native access. Existing chats, resource records and saved requests remain on this device."))
                    i18n_text: "Hagency needs a fresh sign-in to restore native access. Existing chats, resource records and saved requests remain on this device."
                }
            }
            buttons := View {width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 12
                cancel := RinxButton {text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"}
                confirm := RinxPrimaryButton {text: #(crate::i18n::tr("Sign in again")) i18n_text: "Sign in again"}
            }
        }
        account_login_panel := View {visible: false width: Fill height: Fill flow: Down spacing: 16
            scroll := ScrollYView {width: Fill height: Fill flow: Down spacing: 12
                RinxPageTitle {text: #(crate::i18n::tr("Sign in to Codex")) i18n_text: "Sign in to Codex"}
                account := RinxLabel {width: Fill text: ""}
                status := RinxLabel {width: Fill text: "" draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
                explanation := RinxHint {width: Fill text: ""}
                namespace := RinxHint {width: Fill text: ""}
            }
            buttons := View {width: Fill height: Fit flow: Right spacing: 12
                cancel := RinxButton {width: Fill text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"}
                primary := RinxPrimaryButton {width: Fill text: #(crate::i18n::tr("Start sign-in")) i18n_text: "Start sign-in"}
            }
        }
        provider_confirmation := View {visible: false width: Fill height: Fill flow: Down spacing: 16
            scroll := ScrollYView {width: Fill height: Fill flow: Down spacing: 20
                heading := View {width: Fill height: Fit flow: Right spacing: 12 align: Align{y: 0.5}
                    Icon {width: 24 height: 24 draw_icon +: {svg: ICON_SHIELD color: RINX_ACCENT}}
                    title := RinxPageTitle {width: Fill flow: Flow.Right{wrap: true}}
                }
                identity := RoundedView {width: Fill height: Fit padding: 16 flow: Down spacing: 8
                    draw_bg +: {color: RINX_SURFACE border_color: RINX_BORDER border_size: 1 border_radius: theme.corner_radius}
                    runtime := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
                    account := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                }
                explanation := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
                warning := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                personal := View {visible: false width: Fill height: Fit flow: Down spacing: 12
                    rules := View {width: Fill height: Fit flow: Down spacing: 8
                        RinxLabel {width: Fill text: #(crate::i18n::tr("Allowed tools · one exact tool name per line")) i18n_text: "Allowed tools · one exact tool name per line"}
                        tools := RinxInput {width: Fill height: 140 is_multiline: true}
                        RinxLabel {width: Fill text: #(crate::i18n::tr("Allowed directories")) i18n_text: "Allowed directories"}
                        directories := DropDown {width: Fill labels: [""]}
                        remove_directory := RinxButton {text: #(crate::i18n::tr("Remove selected directory")) i18n_text: "Remove selected directory"}
                    }
                    workspace := View {width: Fill height: Fit flow: Down spacing: 8
                        restore := RinxButton {text: #(crate::i18n::tr("Restore workspace choice from the mini app")) i18n_text: "Restore workspace choice from the mini app"}
                    }
                    choose_directory := RinxButton {text: #(crate::i18n::tr("Choose a directory…")) i18n_text: "Choose a directory…"}
                }
            }
            buttons := View {width: Fill height: Fit spacing: 12
                cancel := RinxButton {text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"}
                connect := RinxPrimaryButton {}
            }
        }
        adoption_confirmation := View {visible: false width: Fill height: Fill flow: Down spacing: 16
            scroll := ScrollYView {width: Fill height: Fill flow: Down spacing: 16
                heading := View {width: Fill height: Fit flow: Right spacing: 12 align: Align{y: 0.5}
                    Icon {width: 24 height: 24 draw_icon +: {svg: ICON_SHIELD color: RINX_ACCENT}}
                    title := RinxPageTitle {width: Fill text: #(crate::i18n::tr("Review device setup")) i18n_text: "Review device setup"}
                }
                owner := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
                status := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
                details := RoundedView {width: Fill height: Fit flow: Down padding: 16
                    draw_bg +: {color: RINX_SURFACE border_color: RINX_BORDER border_size: 1 border_radius: theme.corner_radius}
                    copy := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
                }
            }
            buttons := View {width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 12
                cancel := RinxButton {text: #(crate::i18n::tr("Close review")) i18n_text: "Close review"}
                primary := RinxPrimaryButton {text: #(crate::i18n::tr("Set up this device")) i18n_text: "Set up this device"}
                recover := RinxButton {text: #(crate::i18n::tr("Recover original")) i18n_text: "Recover original"}
            }
        }
        execution_confirmation := View {visible: false width: Fill height: Fill flow: Down spacing: 16
            scroll := ScrollYView {width: Fill height: Fill flow: Down spacing: 16
                RinxPageTitle {width: Fill text: #(crate::i18n::tr("Start approved agent")) i18n_text: "Start approved agent"}
                owner := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
                status := RinxHint {width: Fill flow: Flow.Right{wrap: true}}
                details := RoundedView {width: Fill height: Fit flow: Down padding: 16
                    draw_bg +: {color: RINX_SURFACE border_color: RINX_BORDER border_size: 1 border_radius: theme.corner_radius}
                    copy := RinxLabel {width: Fill flow: Flow.Right{wrap: true}}
                }
                toggle_details := RinxButton {visible:false text: #(crate::i18n::tr("Show request details")) i18n_text: "Show request details"}
                technical := RinxHint {visible:false width: Fill flow: Flow.Right{wrap:true}}
            }
            buttons := View {width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 12
                cancel := RinxButton {text: #(crate::i18n::tr("Close review")) i18n_text: "Close review"}
                primary := RinxPrimaryButton {text: #(crate::i18n::tr("Start process")) i18n_text: "Start process"}
                recover := RinxButton {text: #(crate::i18n::tr("Resume saved start")) i18n_text: "Resume saved start"}
            }
        }
        grant_confirmation := NativeSheet {}
        // The monthly-maximum amendment uses the same sheet; its fields stay hidden.
        ceiling_confirmation := NativeSheet {}
        project_room_picker := View {visible: false width: Fill height: Fill flow: Down spacing: 12
            RinxPageTitle {text: "Choose a project room"}
            RinxHint {width: Fill flow: Flow.Right{wrap: true} text: "Choose a private, unencrypted room you created. Submitting the project will invite its Hagency representative. Rinx shares only your selected room with Palpo."}
            rooms := DropDown {width: Fill labels: ["Choose a room"] popup_menu +: {width: 280}}
            selected_room := RinxHint {width: Fill flow: Flow.Right{wrap: true} text: ""}
            use_room := RinxPrimaryButton {text: "Use this room"}
            cancel_room := RinxButton {text: "Cancel"}
        }
        app_content := View {visible: false width: Fill height: Fill
            card := Splash {width: Fill height: Fill}
        }
    }
}
type Provider = Arc<dyn OctosProvider>;
#[derive(Clone, Copy)]
enum HostNavigation {
    Palpo(super::palpo::PalpoNavigation),
    ToolApprovals,
}
impl HostNavigation {
    fn for_service(service: &str) -> Option<Self> {
        if service == crate::host::hagency::tool_approvals::OPEN {
            Some(Self::ToolApprovals)
        } else {
            super::palpo::PalpoNavigation::for_service(service).map(Self::Palpo)
        }
    }
}
struct Pending {
    reply: Option<(usize, u64)>,
    target: Option<String>,
    receiver: Receiver<ServiceEvent>,
    started: Instant,
    turn: bool,
    navigation: Option<HostNavigation>,
}
struct Approval {
    id: String,
    message: String,
}
struct ProviderConfirmation {
    consent: resource_enrollment_ui::ProviderReview,
    reply: (usize, u64),
}
#[derive(Script, Widget)]
pub struct MiniAppsPanel {
    #[deref]
    view: View,
    #[rust]
    open: bool,
    #[rust]
    showing_catalog: bool,
    /// Actual built-in indices for the rows drawn in the current layout.
    #[rust]
    catalog_apps: Vec<usize>,
    #[rust]
    direct_agent: bool,
    #[rust]
    showing_hub: bool,
    #[rust]
    return_to_hub: bool,
    #[rust]
    reviewed_room: String,
    #[rust]
    review_notice: String,
    #[rust]
    package: Option<Package>,
    #[rust]
    lease: Option<Lease>,
    #[rust]
    provider: Option<Provider>,
    #[rust]
    palpo: Option<super::palpo::PalpoHost>,
    #[rust]
    hagency: Option<crate::host::hagency::Host>,
    #[rust]
    palpo_action: Option<String>,
    #[rust]
    owner_approvals: Option<owner_approvals::Pending>,
    #[rust]
    board: Option<super::palpo::ActionsRoomTarget>,
    #[rust]
    octos_unavailable: Option<String>,
    #[rust]
    tag: String,
    #[rust]
    state: InstanceStore,
    #[rust]
    data: Value,
    #[rust]
    pending: Vec<Pending>,
    #[rust]
    approvals: VecDeque<Approval>,
    #[rust]
    provider_confirmation: Option<ProviderConfirmation>,
    #[rust]
    account_login: Option<account_login_ui::AccountLoginPrompt>,
    #[rust]
    reauthentication: Option<reauthentication_ui::ReauthenticationPrompt>,
    #[rust]
    grant_confirmation: Option<grant_ui::GrantPrompt>,
    #[rust]
    execution_confirmation: Option<execution_ui::ExecutionPrompt>,
    #[rust]
    adoption_confirmation: Option<adoption_ui::AdoptionPrompt>,
    #[rust]
    project_room_reply: Option<(usize, u64)>,
    #[rust]
    project_rooms: Vec<(String, String)>,
    #[rust]
    assets: Option<AssetServer>,
    #[rust]
    theme_revision: u64,
    #[rust]
    language: Language,
    #[rust]
    restyle_card: bool,
    #[rust]
    retained_script_layout: Option<Layout>,
    #[rust]
    notice_text: String,
    #[rust]
    reply_redraw: NextFrame,
    #[rust]
    confirmation_redraw: NextFrame,
}
impl ScriptHook for MiniAppsPanel {
    fn on_before_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        // The outer Splash prototype owns its slot size; the running script
        // owns its root layout. A host rebake otherwise applies View's default
        // horizontal flow over that live root without re-evaluating its source.
        self.retained_script_layout = if matches!(apply, Apply::Rebake | Apply::ScriptReapply)
            && self.lease.is_some()
            && self.package.as_ref().is_some_and(|package| package.script)
        {
            self.view.splash(vm.cx_mut(), ids!(card)).borrow().map(|card| card.view.layout)
        } else {
            None
        };
    }
    fn on_after_apply(&mut self, vm: &mut ScriptVm, _apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if let Some(layout) = self.retained_script_layout.take() {
            if let Some(mut card) = self.view.splash(vm.cx_mut(), ids!(card)).borrow_mut() {
                card.view.layout = layout;
            }
        }
        let revision = crate::theme::snapshot_for_vm(vm).revision;
        self.restyle_card |= self.theme_revision != revision && self.lease.is_some()
            && self.package.as_ref().is_some_and(|p| !p.script);
        self.theme_revision = revision;
        if self.open {
            let cx = vm.cx_mut();
            self.view.widget(cx, ids!(library)).set_visible(cx, self.showing_hub);
            self.view.view(cx, ids!(catalog)).set_visible(cx, self.showing_catalog);
            self.view.view(cx, ids!(header)).set_visible(cx, !self.showing_hub && self.board.is_none());
            let app = !self.showing_hub && !self.showing_catalog;
            self.view.view(cx, ids!(app_content)).set_visible(cx, app && self.project_room_reply.is_none());
            self.view.view(cx, ids!(project_room_picker)).set_visible(cx, self.project_room_reply.is_some());
            if self.project_room_reply.is_some() {
                let dropdown = self.view.drop_down(cx, ids!(project_room_picker.rooms));
                let selected = dropdown.selected_item();
                let mut labels = vec!["Choose a room".to_owned()];
                labels.extend(self.project_rooms.iter().map(|(name,_)| name.clone()));
                dropdown.set_labels(cx, labels);
                dropdown.set_selected_item(cx, selected);
                self.present_selected_room(cx, selected);
            }
            self.view.view(cx, ids!(import_form)).set_visible(cx, app && self.lease.is_none());
            self.view.label(cx, ids!(notice)).set_text(cx, &self.notice_text);
            self.view.widget(cx, ids!(notice)).set_visible(cx, (self.board.is_none() && self.hagency.is_none()) || !self.notice_text.is_empty());
            self.present_review_controls(cx);
            let imported = self.package.as_ref().and_then(Package::builtin_id).is_none();
            self.view.view(cx, ids!(bundle_path)).set_visible(cx, imported);
            self.show_approval(cx);
            self.present_provider_confirmation(cx);
            self.present_execution_confirmation(cx);
            self.present_grant_confirmation(cx);
            self.present_adoption_confirmation(cx);
            self.present_reauthentication(cx);
            self.present_account_login(cx);
            self.show_assistant_status(cx);
        }
    }
}
impl Drop for MiniAppsPanel {
    fn drop(&mut self) {
        if let Some(host) = self.hagency.take() {host.retire();}
        if let Some(lease) = self.lease.take() {
            lease.revoke();
            if let Some(provider) = self.provider.take() {
                provider.close(lease.identity());
            }
        }
    }
}
impl MiniAppsPanel {
    fn show_catalog(&mut self, cx: &mut Cx) {
        self.direct_agent = false;
        self.stop(cx);
        self.package = None;
        self.palpo_action = None;
        self.review_notice.clear();
        self.reviewed_room.clear();
        self.publish_to_assistant();
        self.showing_catalog = true;
        self.showing_hub = false;
        self.return_to_hub = false;
        self.view.mini_app_library(cx, ids!(library)).cancel_open();
        self.view.widget(cx, ids!(library)).set_visible(cx, false);
        self.view.view(cx, ids!(header)).set_visible(cx, true);
        self.view.view(cx, ids!(catalog)).set_visible(cx, true);
        self.view.view(cx, ids!(import_form)).set_visible(cx, false);
        self.view.view(cx, ids!(app_content)).set_visible(cx, false);
        self.notice(cx, "");
    }
    fn show_hub(&mut self, cx: &mut Cx) {
        self.direct_agent = false;
        self.stop(cx);
        self.package = None;
        self.palpo_action = None;
        self.review_notice.clear();
        self.reviewed_room.clear();
        self.publish_to_assistant();
        self.showing_catalog = false;
        self.showing_hub = true;
        self.return_to_hub = false;
        self.view.view(cx, ids!(catalog)).set_visible(cx, false);
        self.view.view(cx, ids!(import_form)).set_visible(cx, false);
        self.view.view(cx, ids!(app_content)).set_visible(cx, false);
        self.view.view(cx, ids!(header)).set_visible(cx, false);
        self.view.widget(cx, ids!(library)).set_visible(cx, true);
        self.view.mini_app_library(cx, ids!(library)).begin(cx);
        self.notice(cx, "");
    }
    // Back closes an outer entry; a native confirmation above it cancels first.
    fn navigate_back(&mut self, cx: &mut Cx) -> bool {
        if self.account_login.is_some(){self.close_account_login(cx);return false;}
        if self.reauthentication.is_some(){self.finish_reauthentication(cx,false);return false;}
        if self.execution_confirmation.is_some() {self.close_execution_confirmation(cx);return false;}
        if self.adoption_confirmation.is_some() {self.close_adoption_confirmation(cx);return false;}
        if self.grant_confirmation.is_some() {self.close_grant_confirmation(cx);return false;}
        if self.provider_confirmation.is_some() {
            self.finish_provider_confirmation(cx, false);
            return false;
        }
        if self.project_room_reply.is_some() {
            self.finish_room_picker(cx, false);
            return false;
        }
        if self.direct_agent {
            return true;
        }
        if self.showing_hub {
            if !self.view.mini_app_library(cx, ids!(library)).back_to_list(cx) {
                self.show_catalog(cx);
            }
        } else if self.return_to_hub {
            self.show_hub(cx);
        } else if self.showing_catalog {
            return true;
        } else {
            self.show_catalog(cx);
        }
        false
    }
    fn show_import(&mut self, cx: &mut Cx) {
        self.showing_hub = false;
        self.view.widget(cx, ids!(library)).set_visible(cx, false);
        self.view.view(cx, ids!(header)).set_visible(cx, true);
        self.showing_catalog = false;
        self.view.view(cx, ids!(catalog)).set_visible(cx, false);
        self.view.view(cx, ids!(import_form)).set_visible(cx, true);
        self.view.view(cx, ids!(app_content)).set_visible(cx, true);
        let imported = self.package.as_ref().and_then(Package::builtin_id).is_none();
        self.view.view(cx, ids!(bundle_path)).set_visible(cx, imported);
        self.show_assistant_status(cx);
        self.present_review_controls(cx);
    }
    fn open_builtin(&mut self, cx: &mut Cx, index: usize) -> Result<(), String> {
        let app = crate::system_apps::apps().get(index).ok_or("Unknown built-in app")?;
        if app.native.is_some() {
            cx.action(MiniAppsAction::Close);
            crate::system_apps::launch_native(cx, app);
        } else {
            self.stop(cx);
            self.package = None;
            let package = Package::load_builtin(&app.manifest.id, &crate::app_data_dir().join("miniapps/imports"))?;
            self.show_import(cx);
            self.view.text_input(cx, ids!(import_form.room)).set_text(cx, "");
            self.review_package(cx, package)?;
            if app.manifest.id == super::palpo::APP_ID {
                if let Some(account) = crate::sliding_sync::current_user_id()
                    && super::consent::remembered(account.as_str(), &app.manifest.integrity.bundle_blake3) {
                    if let Err(error) = self.run(cx) {
                        self.stop(cx);
                        self.present_review_controls(cx);
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    fn notice(&mut self, cx: &mut Cx, message: &str) {
        self.notice_text = message.to_owned();
        self.view.label(cx, ids!(notice)).set_text(cx, message);
        self.view.widget(cx, ids!(notice)).set_visible(cx, (self.board.is_none() && self.hagency.is_none()) || !message.is_empty());
    }
    fn stop(&mut self, cx: &mut Cx) {
        self.finish_pending_owner_approvals(cx, false);
        self.account_login=None;
        self.view.view(cx,ids!(account_login_panel)).set_visible(cx,false);
        self.view.view(cx, ids!(hagency_review)).set_visible(cx, false);
        self.reauthentication=None;
        self.view.view(cx,ids!(account_confirmation)).set_visible(cx,false);
        self.execution_confirmation=None;
        self.view.view(cx,ids!(execution_confirmation)).set_visible(cx,false);
        self.adoption_confirmation = None;
        self.view.view(cx, ids!(adoption_confirmation)).set_visible(cx, false);
        self.grant_confirmation = None;
        self.view.view(cx, ids!(grant_confirmation)).set_visible(cx, false);
        self.provider_confirmation = None;
        self.view.view(cx, ids!(provider_confirmation)).set_visible(cx, false);
        self.view.view(cx, ids!(ceiling_confirmation)).set_visible(cx, false);
        self.project_room_reply = None;
        self.project_rooms.clear();
        self.view.view(cx, ids!(project_room_picker)).set_visible(cx, false);
        if let Some(lease) = self.lease.take() {
            lease.revoke();
            if let Some(provider) = self.provider.take() {
                provider.close(lease.identity());
            }
        }
        self.palpo = None;
        if let Some(host) = self.hagency.take() {host.retire();}
        self.pending.clear();
        self.assets = None;
        self.approvals.clear();
        self.view.view(cx, ids!(approval)).set_visible(cx, false);
        self.view.splash(cx, ids!(card)).set_text(cx, "");
        self.view.view(cx, ids!(import_form)).set_visible(cx, true);
        self.publish_to_assistant();
    }
    /// What the assistant's `status` and `open_mini_app` see of this screen.
    fn publish_to_assistant(&self) {
        // A room projection does not replace the user's separately reviewed app
        // or the assistant's foreground app context.
        if self.board.is_some() { return; }
        let reviewed = self.package.as_ref().map(|package| crate::assistant::ReviewedApp {
            id: package.manifest.id.clone(),
            name: package.manifest.name.clone(),
            room: Some(self.reviewed_room.clone()).filter(|room| !room.is_empty()),
        });
        let running = self.lease.as_ref().and(self.package.as_ref()).map(|p| p.manifest.name.clone());
        crate::assistant::set_mini_apps(reviewed, running);
    }
    fn show_approval(&mut self, cx: &mut Cx) {
        if let Some(approval) = self.approvals.front() {
            self.view
                .label(cx, ids!(approval.details))
                .set_text(cx, &approval.message);
        }
        self.view
            .view(cx, ids!(approval))
            .set_visible(cx, !self.approvals.is_empty() && self.provider_confirmation.is_none() && self.account_login.is_none() && self.grant_confirmation.is_none() && self.adoption_confirmation.is_none() && self.reauthentication.is_none() && self.execution_confirmation.is_none());
    }
    fn present_provider_confirmation(&mut self, cx: &mut Cx) {
        let visible = self.provider_confirmation.is_some();
        // A monthly-maximum request has its own sheet in the confirmation family.
        let ceiling = self.provider_confirmation.as_ref().and_then(|pending| pending.consent.ceiling()).map(|prompt| prompt.sheet());
        self.view.view(cx, ids!(provider_confirmation)).set_visible(cx, visible && ceiling.is_none());
        self.view.view(cx, ids!(ceiling_confirmation)).set_visible(cx, ceiling.is_some());
        if let Some(sheet) = &ceiling {
            self.apply_sheet(cx, id!(ceiling_confirmation), sheet);
            self.apply_sheet_disclosure(cx, id!(ceiling_confirmation), sheet.more_open);
            let inputs = self.provider_confirmation.as_ref().and_then(|pending| pending.consent.ceiling()).and_then(|prompt| prompt.inputs());
            self.apply_ceiling_inputs(cx, inputs);
            self.view.view(cx, ids!(app_content)).set_visible(cx, false);
            self.view.view(cx, ids!(approval)).set_visible(cx, false);
        } else if let Some(pending) = &self.provider_confirmation {
            let details = pending.consent.details();
            let account = pending.consent.account().to_owned();
            let words = pending.consent.words();
            let ready = pending.consent.ready();
            self.apply_provider_view(cx, words, &details, &account, ready);
            self.view.view(cx, ids!(app_content)).set_visible(cx, false);
            self.view.view(cx, ids!(approval)).set_visible(cx, false);
        }
        self.present_personal_controls(cx);
    }
    /// Write one native consent's words into its window. Empty purpose or
    /// consequence lines are hidden rather than left as blank gaps.
    fn apply_provider_view(&mut self, cx: &mut Cx, (title, body, warning, action): (&str, &str, &str, &str), details: &str, account: &str, ready: bool) {
        self.view.label(cx, ids!(provider_confirmation.scroll.identity.runtime)).set_text(cx, details);
        self.view.label(cx, ids!(provider_confirmation.scroll.identity.account)).set_text(cx, account);
        self.view.label(cx,ids!(provider_confirmation.scroll.heading.title)).set_text(cx,crate::i18n::tr(title));
        for (path, english) in [(ids!(provider_confirmation.scroll.explanation), body), (ids!(provider_confirmation.scroll.warning), warning)] {
            self.view.label(cx, path).set_text(cx, crate::i18n::tr(english));
            self.view.widget(cx, path).set_visible(cx, !english.is_empty());
        }
        self.view.button(cx,ids!(provider_confirmation.buttons.connect)).set_text(cx,crate::i18n::tr(action));
        self.view.button(cx,ids!(provider_confirmation.buttons.connect)).set_enabled(cx,ready);
        // Also look unavailable while loading or refused, not merely ignore taps.
        self.view.button(cx,ids!(provider_confirmation.buttons.connect)).set_disabled(cx,!ready);
    }
    /// Offline review only: show one native confirmation built from fixture
    /// JSON through the production text and layout. No flow, lease or reply
    /// exists, so nothing shown here can be confirmed or sent anywhere.
    #[cfg(any(test, feature = "onboarding-test-support"))]
    pub fn preview_confirmation(&mut self, cx: &mut Cx, fixture: &Value) -> Result<(), String> {
        // Open, so the window scrolls like the real one. Without a lease or
        // package nothing else runs, and no prompt exists to confirm.
        self.open = true;
        for path in [
            ids!(header), ids!(library), ids!(catalog), ids!(import_form), ids!(hagency_review), ids!(notice),
            ids!(approval), ids!(account_confirmation), ids!(account_login_panel), ids!(provider_confirmation),
            ids!(adoption_confirmation), ids!(execution_confirmation), ids!(grant_confirmation),
            ids!(ceiling_confirmation), ids!(project_room_picker), ids!(app_content),
        ] {
            self.view.widget(cx, path).set_visible(cx, false);
        }
        match fixture["panel"].as_str() {
            Some("grant") => self.preview_grant(cx, fixture)?,
            Some("maximum") => {
                let preview = resource_edit_ui::CeilingPreview::from_fixture(fixture)?;
                let state = preview.state();
                let sheet = resource_edit_ui::ceiling_view(&state);
                self.apply_sheet(cx, id!(ceiling_confirmation), &sheet);
                self.apply_sheet_disclosure(cx, id!(ceiling_confirmation), sheet.more_open);
                self.apply_ceiling_inputs(cx, resource_edit_ui::ceiling_inputs(&state));
                self.view.view(cx, ids!(ceiling_confirmation)).set_visible(cx, true);
            }
            _ => return Err("unknown preview panel".into()),
        }
        self.view.redraw(cx);
        Ok(())
    }
    fn begin_provider_confirmation(&mut self, cx: &mut Cx, service: &str, args: Value, reply: (usize, u64)) -> Result<(), String> {
        if self.account_login.is_some() || self.execution_confirmation.is_some() || self.reauthentication.is_some() || self.provider_confirmation.is_some() || self.grant_confirmation.is_some() || self.adoption_confirmation.is_some() || self.project_room_reply.is_some() || self.pending.len() >= 16 {
            return Err("provider_confirmation_busy".into());
        }
        let host = self.hagency.as_ref().ok_or("management_session_required")?;
        let lease = self.lease.clone().ok_or("management_session_required")?;
        let consent = if crate::host::hagency::web_connections::is_consent(service) {
            resource_enrollment_ui::ProviderReview::Connection(Box::new(web_connections_ui::ConnectionPrompt::begin(host.web_connection_consent(lease,service,args)?)))
        } else if [crate::host::hagency::personal_configuration::MODEL,crate::host::hagency::personal_configuration::WORKSPACE,crate::host::hagency::personal_configuration::RULES].contains(&service) {
            resource_enrollment_ui::ProviderReview::Personal(Box::new(personal_configuration_ui::PersonalPrompt::begin(host.personal_configuration_consent(lease,service,args)?)))
        } else if service==crate::host::hagency::retirement::RETIRE {
            resource_enrollment_ui::ProviderReview::Retire(host.retirement_consent(lease,args)?)
        } else if service==crate::host::hagency::enrollment::ENROLL {
            resource_enrollment_ui::ProviderReview::Enroll(host.resource_consent(lease,args)?)
        } else if service==crate::host::hagency::resource_create::CREATE {
            resource_enrollment_ui::ProviderReview::Create(Box::new(resource_create_ui::ResourceCreatePrompt::begin(host.resource_create_consent(lease,args)?)))
        } else if service==crate::host::hagency::resource_edit::EDIT {
            resource_enrollment_ui::ProviderReview::Edit(Box::new(resource_edit_ui::ResourceEditPrompt::begin(host.resource_edit_consent(lease,args)?)))
        } else {
            return Err("unknown_management_service".into())
        };
        self.provider_confirmation = Some(ProviderConfirmation { consent, reply });
        self.present_provider_confirmation(cx);
        self.request_confirmation_redraw(cx);
        Ok(())
    }
    /// The monthly-maximum sheet: Save keeps it open while the change saves,
    /// then it shows the result; closing answers the mini app exactly once.
    /// Returns false when the ordinary consent path should handle the tap.
    fn finish_ceiling(&mut self, cx: &mut Cx, approved: bool) -> bool {
        self.capture_ceiling_choice(cx);
        let Some(prompt) = self.provider_confirmation.as_mut().and_then(|pending| pending.consent.ceiling_mut()) else { return false; };
        let sheet = prompt.sheet();
        // After a refusal at review: back to the slider at the amount that works.
        if approved && sheet.primary_recovers {
            prompt.rechoose();
            self.present_provider_confirmation(cx);
            self.request_confirmation_redraw(cx);
            return true;
        }
        if approved && !prompt.saving() && !sheet.primary_closes {
            if !prompt.save() { return true; }
            self.present_provider_confirmation(cx);
            self.request_confirmation_redraw(cx);
            return true;
        }
        // Closing after a refusal, while saving or once done answers through
        // the sheet, so the mini app hears the refusal or the result.
        if !(prompt.saving() || sheet.primary_closes || prompt.refused()) { return false; }
        let close = prompt.close();
        let Some(pending) = self.provider_confirmation.take() else { return true; };
        match close {
            resource_edit_ui::CeilingClose::Replied => {}
            resource_edit_ui::CeilingClose::Saving(receiver) => {
                self.pending.push(Pending {reply: Some(pending.reply), target: None, receiver, started: Instant::now(), turn: false, navigation: None});
            }
            resource_edit_ui::CeilingClose::Reply(result) => {
                let text = result.map(|value| value.to_string());
                splash_host_respond(cx, pending.reply.0, pending.reply.1, text.as_deref().map_err(String::as_str));
                self.reply_redraw = cx.new_next_frame();
            }
            resource_edit_ui::CeilingClose::Cancelled => {
                splash_host_respond(cx, pending.reply.0, pending.reply.1, Err(pending.consent.cancelled()));
                self.reply_redraw = cx.new_next_frame();
            }
        }
        self.present_provider_confirmation(cx);
        self.show_approval(cx);
        self.view.view(cx, ids!(app_content)).set_visible(cx, true);
        self.view.redraw(cx);
        true
    }
    /// The monthly-maximum slider: its bounds and the value the sheet stands for.
    fn apply_ceiling_inputs(&mut self, cx: &mut Cx, inputs: Option<(u64, u64, u64)>) {
        let Some((minimum, maximum, value)) = inputs else { return; };
        let widget = self.view.widget(cx, ids!(ceiling_confirmation.scroll.quota.tokens));
        if let Some(mut editor) = widget.borrow_mut::<crate::miniapps::design::MiniAppNumberInput>() {
            editor.set_minimum(cx, minimum);
            editor.set_maximum(cx, maximum);
        }
        widget.set_text(cx, &value.to_string());
    }
    /// Read the monthly-maximum slider back into the sheet, which then
    /// shows the new value, its checks and its Save without refilling the
    /// field the owner is using.
    fn capture_ceiling_choice(&mut self, cx: &mut Cx) {
        let choosing = self.provider_confirmation.as_ref().and_then(|pending| pending.consent.ceiling()).is_some_and(|prompt| prompt.sheet().choosing);
        if !choosing { return; }
        let text = self.view.widget(cx, ids!(ceiling_confirmation.scroll.quota.tokens)).text();
        let tokens = text.trim().parse::<u64>().ok();
        let Some(prompt) = self.provider_confirmation.as_mut().and_then(|pending| pending.consent.ceiling_mut()) else { return; };
        if !prompt.choose(tokens) { return; }
        let sheet = prompt.sheet();
        self.apply_sheet(cx, id!(ceiling_confirmation), &sheet);
        self.request_confirmation_redraw(cx);
    }
    /// Keyboard for the monthly-maximum sheet: Escape closes, Tab moves
    /// between the disclosure and the footer, Return or Space activates.
    fn ceiling_key(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let Some(sheet) = self.provider_confirmation.as_ref().and_then(|pending| pending.consent.ceiling()).map(|prompt| prompt.sheet()) else { return false; };
        let Event::KeyDown(key) = event else { return false; };
        if key.key_code == KeyCode::Escape {
            self.finish_provider_confirmation(cx, false);
            return true;
        }
        let more = self.view.button(cx, ids!(ceiling_confirmation.scroll.more.more_row.more_toggle)).area();
        let cancel = self.view.button(cx, ids!(ceiling_confirmation.buttons.cancel)).area();
        let primary = self.view.button(cx, ids!(ceiling_confirmation.buttons.primary)).area();
        let mut targets = Vec::new();
        if sheet.choosing && sheet.quota_visible {
            if let Some(slider) = self.view.widget(cx, ids!(ceiling_confirmation.scroll.quota.tokens.slider_wrap.amount_slider)).borrow::<crate::miniapps::design::MiniAppAmountSlider>() {
                targets.push(slider.focus_area());
            }
            targets.push(self.view.text_input(cx, ids!(ceiling_confirmation.scroll.quota.tokens.input)).area());
        }
        if sheet.more_visible() {targets.push(more);}
        if !sheet.cancel.is_empty() {targets.push(cancel);}
        if sheet.primary_visible && sheet.primary_enabled {targets.push(primary);}
        if targets.is_empty() {return false;}
        if key.key_code == KeyCode::Tab {
            let next = grant_ui::focus_next(targets.iter().position(|area| cx.has_key_focus(*area)), targets.len(), key.modifiers.shift);
            cx.set_key_focus(targets[next]);
            return true;
        }
        if matches!(key.key_code, KeyCode::ReturnKey | KeyCode::Space) {
            if sheet.more_visible() && cx.has_key_focus(more) {
                if let Some(prompt) = self.provider_confirmation.as_mut().and_then(|pending| pending.consent.ceiling_mut()) {prompt.toggle_more();}
                self.present_provider_confirmation(cx);
                cx.set_key_focus(more);
                return true;
            }
            if !sheet.cancel.is_empty() && cx.has_key_focus(cancel) {self.finish_provider_confirmation(cx, false); return true;}
            if sheet.primary_visible && sheet.primary_enabled && cx.has_key_focus(primary) {self.finish_provider_confirmation(cx, true); return true;}
        }
        false
    }
    /// Once a confirmed maximum is saved or refused, the mini app hears it
    /// at once, while the sheet stays to show the result.
    fn reply_ceiling(&mut self, cx: &mut Cx) {
        let Some(pending) = &mut self.provider_confirmation else { return; };
        let reply = pending.reply;
        let Some(result) = pending.consent.ceiling_mut().and_then(|prompt| prompt.take_reply()) else { return; };
        let text = result.map(|value| value.to_string());
        splash_host_respond(cx, reply.0, reply.1, text.as_deref().map_err(String::as_str));
        self.reply_redraw = cx.new_next_frame();
    }
    fn finish_provider_confirmation(&mut self, cx: &mut Cx, approved: bool) {
        if self.finish_ceiling(cx, approved) { return; }
        if approved && self.provider_confirmation.as_ref().is_some_and(|pending|!pending.consent.ready()) { return; }
        let Some(pending) = self.provider_confirmation.take() else { return; };
        self.present_provider_confirmation(cx);
        self.show_approval(cx);
        self.view.view(cx, ids!(app_content)).set_visible(cx, true);
        let failure = if pending.consent.expired() { Some("provider_confirmation_expired") }
            else if !approved { Some(pending.consent.cancelled()) }
            else if self.pending.len() >= 16 { Some("management_capacity") }
            else { None };
        if let Some(error) = failure {
            splash_host_respond(cx, pending.reply.0, pending.reply.1, Err(error));
            self.reply_redraw = cx.new_next_frame();
        } else {
            let (tx, receiver) = mpsc::sync_channel(1);
            crate::sliding_sync::spawn_async_task(async move {
                let result = pending.consent.accept().await;
                let _ = tx.try_send(ServiceEvent::Complete(result));
                SignalToUI::set_ui_signal();
            });
            self.pending.push(Pending {reply: Some(pending.reply), target: None, receiver, started: Instant::now(), turn: false, navigation: None});
        }
        self.view.redraw(cx);
    }
    fn present_review_controls(&mut self, cx: &mut Cx) {
        let palpo = self.package.as_ref().and_then(Package::builtin_id) == Some(super::palpo::APP_ID);
        let reviewing = palpo && self.lease.is_none() && !self.showing_catalog && !self.showing_hub;
        self.view.view(cx, ids!(hagency_review)).set_visible(cx, reviewing);
        if reviewing {
            // Long capability lists scroll independently of the consent actions.
            // Keep this built-in flow separate from imported apps' review forms.
            self.view.view(cx, ids!(import_form)).set_visible(cx, false);
            self.view.view(cx, ids!(app_content)).set_visible(cx, false);
            let account = crate::sliding_sync::current_user_id().map(|id| id.to_string()).unwrap_or_default();
            self.view.label(cx, ids!(hagency_review.account)).set_text(cx, &account);
            let permissions = self.package.as_ref().map(|package| package.manifest.capabilities.iter()
                .filter_map(|cap| octosense_app_contract::hagency::words(cap))
                .map(crate::i18n::tr).collect::<Vec<_>>().join("\n")).unwrap_or_default();
            self.view.label(cx, ids!(hagency_review.permissions)).set_text(cx, &permissions);
            self.view.widget(cx, ids!(notice)).set_visible(cx, !self.notice_text.is_empty() && self.notice_text != self.review_notice);
        }
        // TextInput has no visibility property. Hide its containing View so
        // unrelated room grants cannot appear in Palpo's host consent screen.
        self.view.view(cx, ids!(import_form.room_access)).set_visible(cx, !palpo);
        self.view.label(cx, ids!(import_form.assistant_status)).set_visible(cx, !palpo);
        self.view.view(cx, ids!(import_form.core)).set_visible(cx, !palpo && !crate::octos_service::is_hosted());
        self.view.button(cx, ids!(import_form.buttons.review)).set_visible(cx, !palpo);
    }
    fn show_assistant_status(&mut self, cx: &mut Cx) {
        let status = crate::octos_service::status();
        self.view
            .label(cx, ids!(import_form.assistant_status))
            .set_text(cx, &status);
    }
    /// An explicit remote server (standalone only; hosted Rinx uses
    /// OctoSense's AI settings and offers no endpoint or key form).
    #[cfg_attr(not(feature = "octos-remote"), allow(unused_variables))]
    fn connect(&mut self, cx: &mut Cx) -> Result<(), String> {
        if crate::octos_service::is_hosted() {
            return Err("This app uses OctoSense's AI settings".into());
        }
        #[cfg(feature = "octos-remote")]
        {
            let endpoint = self.view.text_input(cx, ids!(import_form.core.endpoint)).text();
            let profile = self.view.text_input(cx, ids!(import_form.core.profile)).text();
            let token = self.view.text_input(cx, ids!(import_form.core.token)).text();
            crate::octos_service::use_remote(&endpoint, &profile, &token)?;
            self.view.text_input(cx, ids!(import_form.core.token)).set_text(cx, "");
            self.show_assistant_status(cx);
            Ok(())
        }
        #[cfg(not(feature = "octos-remote"))]
        Err("This build cannot connect to an Octos server".into())
    }
    /// Rinx's own local runtime, optionally with a new provider.
    #[cfg_attr(not(feature = "octos-local"), allow(unused_variables))]
    fn use_local(&mut self, cx: &mut Cx) -> Result<(), String> {
        if crate::octos_service::is_hosted() {
            return Err("This app uses OctoSense's AI settings".into());
        }
        #[cfg(feature = "octos-local")]
        {
            let family = self.view.text_input(cx, ids!(import_form.core.local_family)).text();
            let model = self.view.text_input(cx, ids!(import_form.core.local_model)).text();
            let base = self.view.text_input(cx, ids!(import_form.core.local_base_url)).text();
            let key = self.view.text_input(cx, ids!(import_form.core.local_key)).text();
            if family.trim().is_empty() && model.trim().is_empty() {
                crate::octos_service::use_local()?;
            } else {
                crate::octos_service::configure_local_provider(&family, &model, &base, &key)?;
            }
            self.view.text_input(cx, ids!(import_form.core.local_key)).set_text(cx, "");
            self.show_assistant_status(cx);
            Ok(())
        }
        #[cfg(not(feature = "octos-local"))]
        Err("This build has no local assistant runtime".into())
    }
    fn review(&mut self, cx: &mut Cx) -> Result<(), String> {
        let builtin = self.package.as_ref().and_then(Package::builtin_id);
        self.stop(cx);
        if builtin.is_none() { self.package = None; }
        let snapshots = crate::app_data_dir().to_owned().join("miniapps/imports");
        let package = if let Some(id) = builtin {
            Package::load_builtin(id, &snapshots)?
        } else {
            let path = self.view.text_input(cx, ids!(path)).text();
            Package::load_in(&PathBuf::from(path.trim()), &snapshots)?
        };
        self.review_package(cx, package)
    }
    fn review_package(&mut self, cx: &mut Cx, package: Package) -> Result<(), String> {
        let room = self.view.text_input(cx, ids!(import_form.room)).text();
        if !room.trim().is_empty() {
            ruma::RoomId::parse(room.trim()).map_err(|_| "Invalid Matrix room ID")?;
        }
        let services = package.manifest.capabilities.join(", ");
        let imported = package.builtin_id().is_none();
        let origin = if imported { "Local unsigned bundle" } else { crate::i18n::tr("Built-in apps") };
        self.view.view(cx, ids!(bundle_path)).set_visible(cx, imported);
        self.review_notice = format!("{} {} · {}\nServices: {}\nAllowed room: {}\nRun grants these services for this session. Octos turns may use the connected core's tools.",package.manifest.name,package.manifest.version,origin,services,if room.trim().is_empty(){"None"}else{room.trim()});
        if package.builtin_id() == Some(super::palpo::APP_ID) {
            let permissions = package.manifest.capabilities.iter().filter_map(|cap| octosense_app_contract::hagency::words(cap)).map(crate::i18n::tr).collect::<Vec<_>>().join("\n");
            self.review_notice = format!("{}\n{permissions}\n{}\n{}", crate::i18n::tr("Hagency uses your current Matrix account."), crate::i18n::tr("Approval remains limited to your assigned server role. Provider credentials stay in the native host."), crate::i18n::tr("Run remembers consent for this account and this exact bundled version."));
        }
        let notice = self.review_notice.clone();
        self.notice(cx, &notice);
        self.reviewed_room = room.trim().to_string();
        self.package = Some(package);
        self.present_review_controls(cx);
        self.publish_to_assistant();
        Ok(())
    }
    fn run(&mut self, cx: &mut Cx) -> Result<(), String> {
        let owner_approvals = self.owner_approvals.take();
        self.stop(cx);
        self.owner_approvals = owner_approvals;
        let package = self.package.as_ref().ok_or("Review a bundle first")?;
        package.unchanged()?;
        let account = crate::sliding_sync::current_user_id()
            .ok_or("Log in to Matrix before running a mini app")?
            .to_string();
        let room = self.view.text_input(cx, ids!(import_form.room)).text();
        if room.trim() != self.reviewed_room {
            return Err("Room access changed. Review the bundle again".into());
        }
        let room = if room.trim().is_empty() {
            None
        } else {
            Some(
                ruma::RoomId::parse(room.trim())
                    .map_err(|_| "Invalid room ID")?
                    .to_string(),
            )
        };
        let room = self.board.as_ref().map(|target| target.room_id.to_string()).or(room);
        static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let lease = super::AUTHORITY.issue(
            InstanceId {
                app: package.manifest.id.clone(),
                account: account.clone(),
                room: room.clone(),
                generation,
            },
            package.manifest.capabilities.iter().cloned().collect(),
            room.into_iter().collect(),
            Instant::now() + Duration::from_secs(3600),
        );
        self.hagency = if package.manifest.capabilities.iter().any(|c| octosense_app_contract::hagency::SERVICES.contains(&c.as_str())) {
            if package.builtin_id() != Some(octosense_app_contract::hagency::APP_ID) {
                return Err("management_package_required".into());
            }
            Some(crate::host::hagency::Host::new(package.manifest.integrity.bundle_blake3.clone(), &lease)?)
        } else {None};
        self.palpo = if package.manifest.capabilities.iter().any(|c| octosense_app_contract::palpo::SERVICES.contains(&c.as_str())) {
            Some(super::palpo::PalpoHost::new(package.manifest.integrity.bundle_blake3.clone())?
                .with_action(self.palpo_action.take()).with_board(self.board.clone()))
        } else { None };
        if package.builtin_id() == Some(super::palpo::APP_ID) {
            super::consent::remember(&account, &package.manifest.integrity.bundle_blake3)?;
        }
        self.tag = format!("rinx-miniapp-{generation}");
        self.data = package.data.clone();
        self.state = Default::default();
        if !package.script {
            for (field, value) in octoscript_ui_l0::state_initials(&package.source) {
                self.state
                    .set_cell(octoscript_ui_l0::CARD_STATE_KEY, &field, value);
            }
        }
        let server = octosense_app_contract::AssetServer::start(&package.root)?;
        octosense_app_contract::rewrite_assets(&mut self.data, server.origin());
        let account_dir: String = account.bytes().map(|b| format!("{b:02x}")).collect();
        // The local core's default read boundary is its data root. Allocate
        // an account/app child there and then narrow the session to that child.
        // The location comes from the native host, never from bundle input.
        // The isolate's own storage stays in Rinx's data dir; the assistant's
        // workspace for this instance is its kernel request context's.
        let root = crate::app_data_dir().join("miniapps").join(account_dir);
        let mut settings = super::sandbox::IsolateSettings::for_app(&package.policy, &root);
        if package.builtin_id().is_some() {
            settings.system_app();
        }
        std::fs::create_dir_all(&settings.jail_root).map_err(|e| e.to_string())?;
        let wants_octos = package
            .manifest
            .capabilities
            .iter()
            .any(|c| octosense_app_peers::OCTOS_SERVICES.contains(&c.as_str()));
        self.provider = None;
        self.octos_unavailable = None;
        if wants_octos {
            match ContextProvider::open(&lease) {
                Ok(provider) => self.provider = Some(provider as Arc<dyn OctosProvider>),
                Err(reason) => self.octos_unavailable = Some(reason),
            }
        }
        settings.serve_bundle(server.allowlist_entry(), !package.script);
        let splash = self.view.splash(cx, ids!(card));
        super::sandbox::apply(&splash, cx, &settings);
        splash.set_host_tag(cx, Some(self.tag.clone()));
        self.assets = Some(server);
        self.lease = Some(lease);
        self.publish_to_assistant();
        self.render(cx)?;
        let calls = self.package.as_ref().unwrap().bindings.on_open.clone();
        for call in calls {
            self.binding(cx, call, "root", &Value::Null)?;
        }
        self.showing_hub = false;
        self.showing_catalog = false;
        self.view.widget(cx, ids!(library)).set_visible(cx, false);
        self.view.view(cx, ids!(header)).set_visible(cx, true);
        self.view.view(cx, ids!(app_content)).set_visible(cx, true);
        self.view.view(cx, ids!(import_form)).set_visible(cx, false);
        self.view.view(cx, ids!(hagency_review)).set_visible(cx, false);
        self.notice(cx, if self.hagency.is_some() { "" } else {
            "Running · Back closes this app and revokes its services."
        });
        if self.board.is_some() {
            self.view.view(cx, ids!(header)).set_visible(cx, false);
            self.notice(cx, "");
        }
        Ok(())
    }
    fn render(&mut self, cx: &mut Cx) -> Result<(), String> {
        let package = self.package.as_ref().ok_or("No package")?;
        if package.script {
            let source = octosense_app_contract::script_source(
                &package.root,
                self.assets
                    .as_ref()
                    .ok_or("Missing bundle assets")?
                    .origin(),
            )
            .ok_or("Missing main.splash")??;
            return self.install_script_source(cx, &source);
        }
        let report = octoscript_ui_l0::realize_with_state(
            &package.source,
            &self.data,
            &self.state,
            Default::default(),
        );
        report.complete_root()?;
        self.state.prune(&report.live_keys);
        for (field, value) in report.captured {
            self.state
                .set_cell(octoscript_ui_l0::CARD_STATE_KEY, &field, value);
        }
        let card = super::presentation::prepare(
            &package.source,
            &self.data,
            &self.state,
            &package.root.join("kit"),
            &crate::theme::snapshot(cx),
        )?;
        let ui = super::presentation::embedded_ui(card)?;
        // NAV belongs to this isolate and enters the same authenticated queue
        // as imperative host.request calls. No global Notify event is trusted.
        let body = format!(
            "let NAV = fn(t, v=\"\") {{ host.request(\"rinx.event\", {{route:t, value:v}}, fn(r){{}}) }}\nwidth:Fill height:Fill flow:Down\n{ui}"
        );
        self.view.splash(cx, ids!(card)).reapply_text(cx, &body);
        Ok(())
    }
    fn begin_room_picker(&mut self, cx: &mut Cx, args: Value, reply: (usize, u64)) -> Result<(), String> {
        if args != json!({}) || self.account_login.is_some() || self.execution_confirmation.is_some() || self.reauthentication.is_some() || self.project_room_reply.is_some() || self.provider_confirmation.is_some() || self.grant_confirmation.is_some() || self.adoption_confirmation.is_some() {
            return Err("A room selection is already open or the request is invalid".into());
        }
        let account = crate::sliding_sync::current_user_id().ok_or("Not logged in")?;
        let lease = self.lease.as_ref().ok_or("Mini app is closed")?;
        lease.authorize(account.as_str(), "palpo.projects.select_room", None)?;
        if lease.identity().app != super::palpo::APP_ID {
            return Err("Room selection belongs to the Palpo app".into());
        }
        let client = crate::sliding_sync::get_client().ok_or("Not logged in")?;
        self.project_rooms = client.joined_rooms().into_iter().filter(|r| eligible_project_room(r, account.as_ref())).map(|room| {
            let name = room.cached_display_name().map(|s| s.to_string()).or_else(|| room.name())
                .unwrap_or_else(|| room.room_id().to_string());
            (name, room.room_id().to_string())
        }).collect();
        self.project_rooms.sort_by(|a,b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then(a.1.cmp(&b.1)));
        let mut labels = vec!["Choose a room".to_owned()];
        labels.extend(self.project_rooms.iter().map(|(name,_)| name.clone()));
        let dropdown = self.view.drop_down(cx, ids!(project_room_picker.rooms));
        dropdown.set_labels(cx, labels);
        dropdown.set_selected_item(cx, 0);
        self.present_selected_room(cx, 0);
        self.project_room_reply = Some(reply);
        self.view.view(cx, ids!(app_content)).set_visible(cx, false);
        self.view.view(cx, ids!(project_room_picker)).set_visible(cx, true);
        self.view.redraw(cx);
        Ok(())
    }
    fn present_selected_room(&self, cx: &mut Cx, index: usize) {
        let text = index.checked_sub(1).and_then(|i| self.project_rooms.get(i))
            .map(|(name, id)| format!("{name}\n{id}"))
            .unwrap_or_else(|| if self.project_rooms.is_empty() { "No private, unencrypted rooms created by you are available.".into() } else { String::new() });
        self.view.label(cx, ids!(project_room_picker.selected_room)).set_text(cx, &text);
    }
    fn finish_room_picker(&mut self, cx: &mut Cx, selected: bool) {
        let Some((heap, id)) = self.project_room_reply else { return; };
        let account = crate::sliding_sync::current_user_id();
        let authorized = self.lease.as_ref().zip(account.as_ref()).is_some_and(|(lease,account)|
            lease.authorize(account.as_str(), "palpo.projects.select_room", None).is_ok());
        if !authorized { self.stop(cx); return; }
        let value = if selected {
            let index = self.view.drop_down(cx, ids!(project_room_picker.rooms)).selected_item();
            let Some((name,room)) = index.checked_sub(1).and_then(|i| self.project_rooms.get(i)) else {
                self.notice(cx, "Choose a room before continuing."); return;
            };
            let joined = crate::sliding_sync::get_client().is_some_and(|client|
                client.joined_rooms().iter().any(|r| r.room_id().as_str() == room && account.as_ref().is_some_and(|a| eligible_project_room(r, a.as_ref()))));
            if !joined { self.notice(cx, "Room membership changed. Cancel and choose again."); return; }
            json!({"cancelled":false,"name":name,"roomId":room})
        } else { json!({"cancelled":true}) };
        self.project_room_reply = None;
        self.project_rooms.clear();
        self.view.view(cx, ids!(project_room_picker)).set_visible(cx, false);
        self.view.view(cx, ids!(app_content)).set_visible(cx, true);
        splash_host_respond(cx, heap, id, Ok(&value.to_string()));
        self.view.redraw(cx);
    }

    fn dispatch(
        &mut self,
        service: &str,
        args: Value,
        reply: Option<(usize, u64)>,
        target: Option<String>,
    ) -> Result<(), String> {
        if target.is_some() && self.pending.iter().any(|p| p.target == target) {
            return Err("This action is already running".into());
        }
        if service == crate::host::hagency::tool_approvals::OPEN && !cfg!(feature = "agent_chat") {
            return Err("tool_approval_navigation_unavailable".into());
        }
        if self.pending.len() >= 16 {
            return Err("Mini app already has 16 pending requests".into());
        }
        let lease = self.lease.clone().ok_or("Mini app is closed")?;
        let account = crate::sliding_sync::current_user_id().ok_or("Not logged in")?;
        lease.authorize(account.as_str(), service, None)?;
        rinx_miniapp_core::parse_arguments(&args.to_string())?;
        let (tx, rx) = mpsc::sync_channel(128);
        if service.starts_with("matrix.") {
            let service = service.to_owned();
            crate::sliding_sync::spawn_async_task(async move {
                let result = super::matrix_request(lease, service, args).await;
                let _ = tx.try_send(ServiceEvent::Complete(result));
                SignalToUI::set_ui_signal();
            });
        } else if octosense_app_contract::hagency::SERVICES.contains(&service) {
            let host = self.hagency.clone().ok_or("Hagency session is unavailable")?;
            let service = service.to_owned();
            crate::sliding_sync::spawn_async_task(async move {
                let result = host.request(lease, &service, args).await;
                if let Err(code) = &result {
                    log!("Hagency mini app request failed: service={} code={}", service, code);
                }
                let _ = tx.try_send(ServiceEvent::Complete(result));
                SignalToUI::set_ui_signal();
            });
        } else if octosense_app_contract::palpo::SERVICES.contains(&service) {
            let host = self.palpo.clone().ok_or("Palpo session is unavailable")?;
            let service = service.to_owned();
            crate::sliding_sync::spawn_async_task(async move {
                let result = super::palpo::request(host, lease, service, args).await;
                let _ = tx.try_send(ServiceEvent::Complete(result));
                SignalToUI::set_ui_signal();
            });
        } else if octosense_app_peers::OCTOS_SERVICES.contains(&service) {
            let unavailable = self
                .octos_unavailable
                .clone()
                .unwrap_or_else(|| "The assistant is unavailable".into());
            self.provider
                .as_ref()
                .ok_or(unavailable)?
                .request(lease, service, args, tx)?;
        } else {
            return Err(format!("No adapter for {service}"));
        }
        self.pending.push(Pending {
            reply,
            target,
            receiver: rx,
            started: Instant::now(),
            turn: service == "octos.turn.start",
            navigation: HostNavigation::for_service(service),
        });
        Ok(())
    }
    fn binding(
        &mut self,
        _cx: &mut Cx,
        call: Call,
        key: &str,
        payload: &Value,
    ) -> Result<(), String> {
        let args = super::package::arguments(&call.args, &self.data, &self.state, key, payload)?;
        self.dispatch(&call.service, args, None, Some(call.target))
    }
    fn event(&mut self, cx: &mut Cx, args: Value) -> Result<(), String> {
        let route = args["route"].as_str().ok_or("Missing event route")?;
        let mut event: Value = serde_json::from_str(
            route
                .strip_prefix("l0:")
                .ok_or("Unknown navigation event")?,
        )
        .map_err(|e| e.to_string())?;
        if event["v"] == "$$" {
            event["v"] = args["value"].clone();
        }
        let key = event["k"].as_str().ok_or("Missing instance key")?;
        let name = event["e"].as_str().ok_or("Missing event name")?;
        let package = self.package.as_ref().ok_or("No package")?;
        if package.script {
            return Err("L0 events are not available to main.splash apps".into());
        }
        let changed = octoscript_ui_l0::dispatch_with_data(
            &package.source,
            &mut self.state,
            key,
            name,
            Some(&event["v"]),
            &self.data,
        );
        let call = package.bindings.events.get(name).cloned();
        if changed {
            self.render(cx)?;
        }
        if let Some(call) = call {
            self.binding(cx, call, key, &event["v"])?;
        }
        Ok(())
    }
    /// Open an existing native management surface without exposing a mutation
    /// or accepting arbitrary room/project IDs from the script.
    fn open_hagency_navigation(&mut self, cx: &mut Cx, args: Value) -> Result<(), String> {
        let account = crate::sliding_sync::current_user_id().ok_or("Not logged in")?;
        let lease = self.lease.as_ref().ok_or("Mini app is closed")?;
        lease.authorize(account.as_str(), "hagency.navigation.open", None)?;
        if self.hagency.is_none() || self.package.as_ref().and_then(|p| p.builtin_id()) != Some(octosense_app_contract::hagency::APP_ID) {
            return Err("management_navigation_unavailable".into());
        }
        let args = args.as_object().ok_or("invalid_management_navigation")?;
        if matches!(args.get("destination").and_then(Value::as_str), Some("owner_approvals_ready" | "owner_approvals_rejected")) {
            return self.finish_owner_approvals(cx, args, account.as_str());
        }
        let action = match (args.get("destination").and_then(Value::as_str), args.len()) {
            (Some("matrix_workspaces"), 1) => crate::hagency::ui::HagencyAction::OpenProjects,
            (Some("local_agents"), 1) => crate::hagency::ui::HagencyAction::OpenAgents,
            // Navigation only: the panel opens the Agent only if this owner has it.
            (Some("local_agent"), 2) => {
                let agent_id = args.get("agentId").and_then(Value::as_str)
                    .filter(|id| (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
                    .ok_or("invalid_management_navigation")?;
                crate::hagency::ui::HagencyAction::OpenAgent { agent_id: agent_id.into() }
            }
            _ => return Err("invalid_management_navigation".into()),
        };
        cx.action(action);
        Ok(())
    }
    fn pump(&mut self, cx: &mut Cx) {
        self.pump_owner_approvals(cx);
        let Some(lease) = self.lease.clone() else {
            return;
        };
        if lease
            .check(
                &crate::sliding_sync::current_user_id()
                    .map(|u| u.to_string())
                    .unwrap_or_default(),
            )
            .is_err()
        {
            self.stop(cx);
            self.notice(cx, crate::i18n::tr("Session ended. Review and run again."));
            self.present_review_controls(cx);
            return;
        }
        let heap = self.view.splash(cx, ids!(card)).isolate_heap_key(cx);
        if self.provider_confirmation.as_mut().is_some_and(|pending|pending.consent.poll()) {
            self.reply_ceiling(cx);
            self.present_provider_confirmation(cx);
            self.request_confirmation_redraw(cx);
        }
        if self.provider_confirmation.as_ref().is_some_and(|p| p.consent.expired()) {
            self.finish_provider_confirmation(cx, false);
        }
        self.poll_account_login(cx);
        self.poll_execution_confirmation(cx);
        self.poll_grant_confirmation(cx);
        self.poll_adoption_confirmation(cx);
        let owned_heaps: Vec<_> = heap.into_iter().collect();
        for req in take_splash_host_requests_for(&owned_heaps) {
            let result = rinx_miniapp_core::parse_arguments(&req.args_json).and_then(|args| {
                if req.service == "hagency.navigation.open" {
                    self.open_hagency_navigation(cx, args)
                } else if req.service == crate::host::hagency::accounts::LOGIN {
                    self.begin_account_login(cx,args,(req.heap_key,req.req_id))
                } else if reauthentication_ui::is_service(&req.service) {
                    self.begin_reauthentication(cx,args,(req.heap_key,req.req_id))
                } else if req.service == crate::host::hagency::execution::REVIEW || req.service == crate::host::hagency::execution::RESUME {
                    self.begin_execution_confirmation(cx,&req.service,args,(req.heap_key,req.req_id))
                } else if req.service == "hagency.onboarding.review" {
                    self.begin_adoption_confirmation(cx, args, (req.heap_key, req.req_id))
                } else if req.service == crate::host::hagency::grants::REVIEW_GRANT {
                    self.begin_grant_confirmation(cx, args, (req.heap_key, req.req_id))
                } else if crate::host::hagency::web_connections::is_consent(&req.service) || req.service == crate::host::hagency::enrollment::ENROLL || req.service == crate::host::hagency::retirement::RETIRE || req.service == crate::host::hagency::resource_edit::EDIT || req.service == crate::host::hagency::resource_create::CREATE || [crate::host::hagency::personal_configuration::MODEL,crate::host::hagency::personal_configuration::WORKSPACE,crate::host::hagency::personal_configuration::RULES].contains(&req.service.as_str()) {
                    self.begin_provider_confirmation(cx, &req.service, args, (req.heap_key, req.req_id))
                } else if req.service == "palpo.projects.select_room" {
                    self.begin_room_picker(cx, args, (req.heap_key, req.req_id))
                } else if req.service == "rinx.event" {
                    self.event(cx, args)
                } else {
                    self.dispatch(&req.service, args, Some((req.heap_key, req.req_id)), None)
                }
            });
            if req.service == "hagency.navigation.open" {
                log!("Hagency navigation {}: {:?}", req.args_json, result);
            }
            if req.service == "rinx.event" || req.service == "hagency.navigation.open" || result.is_err() {
                splash_host_respond(
                    cx,
                    req.heap_key,
                    req.req_id,
                    result.as_ref().map(|_| "{}").map_err(String::as_str),
                );
                if let Err(e) = result && self.hagency.is_none() {
                    self.notice(cx, &e);
                }
            }
        }
        let mut updates = Vec::new();
        self.pending.retain_mut(|pending| {
            let mut done = false;
            loop {
                match pending.receiver.try_recv() {
                    Ok(event) => {
                        let complete = matches!(event, ServiceEvent::Complete(_));
                        updates.push((pending.reply, pending.target.clone(), pending.turn, pending.navigation, event));
                        if complete {
                            done = true;
                            break;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        updates.push((
                            pending.reply,
                            pending.target.clone(),
                            pending.turn,
                            pending.navigation,
                            ServiceEvent::Complete(Err(
                                "Service connection closed before completing".into(),
                            )),
                        ));
                        done = true;
                        break;
                    }
                }
            }
            if !done && pending.started.elapsed() > Duration::from_secs(185) {
                updates.push((
                    pending.reply,
                    pending.target.clone(),
                    pending.turn,
                    pending.navigation,
                    ServiceEvent::Complete(Err("Service timed out".into())),
                ));
                done = true;
            }
            !done
        });
        let mut redraw = false;
        for (reply, target, turn, navigation, event) in updates {
            let (complete, mut result) = match event {
                ServiceEvent::Data(v) => (false, Ok(v)),
                ServiceEvent::Complete(r) => (true, r),
            };
            if complete {
                if let Some(navigation) = navigation {
                    result = result.and_then(|value| {
                        let account = &lease.identity().account;
                        match navigation {
                            HostNavigation::Palpo(super::palpo::PalpoNavigation::Signup) => {
                                let target = super::palpo::SignupApprovalTarget::from_reply(&value, account)?;
                                cx.action(MiniAppsAction::OpenSignupApproval(target));
                                Ok(json!({"opened": true}))
                            }
                            HostNavigation::Palpo(super::palpo::PalpoNavigation::AgentChat) => {
                                let target = super::palpo::AgentChatTarget::from_reply(&value, account)?;
                                cx.action(MiniAppsAction::OpenAgentChat(target));
                                Ok(json!({"requested": true}))
                            }
                            HostNavigation::Palpo(super::palpo::PalpoNavigation::ActionsRoom) => {
                                let target = super::palpo::ActionsRoomTarget::from_reply(&value, account)?;
                                cx.action(MiniAppsAction::OpenActionsRoom(target));
                                Ok(json!({"requested": true}))
                            }
                            HostNavigation::ToolApprovals => {
                                lease.authorize(account, crate::host::hagency::tool_approvals::OPEN, None)?;
                                let target = crate::host::hagency::tool_approvals::ToolApprovalTarget::from_reply(&value, account)?;
                                cx.action(MiniAppsAction::OpenToolApprovals(target));
                                Ok(json!({"requested":true}))
                            }
                        }
                    });
                }
            }
            if !complete {
                if let Ok(value) = &result {
                    let event = &value["event"];
                    if event["kind"] == "approval_requested" {
                        if let Some(id) = event["approval_id"].as_str() {
                            if !self.approvals.iter().any(|a| a.id == id) {
                                self.approvals.push_back(Approval {
                                    id: id.to_owned(),
                                    message: format!(
                                        "Octos tool approval: {}\n{}",
                                        event["title"].as_str().unwrap_or("Tool request"),
                                        event["body"].as_str().unwrap_or("")
                                    ),
                                });
                            }
                            self.show_approval(cx);
                        }
                    }
                }
            }
            if let Some(target) = target {
                let value = match &result {
                    Ok(v) => json!({"is_ok":true,"data":v}),
                    Err(e) => json!({"is_ok":false,"error":e}),
                };
                self.data[&target] = value;
                redraw = true;
            }
            if complete {
                if turn {
                    self.approvals.clear();
                    self.show_approval(cx);
                }
                if let Some((heap, req)) = reply {
                    let text = result.as_ref().map(|v| v.to_string());
                    splash_host_respond(cx, heap, req, text.as_deref().map_err(|e| e.as_str()));
                    // Host callbacks run after this event and can replace dynamic
                    // children. Invalidate this panel once after they have run.
                    self.reply_redraw = cx.new_next_frame();
                }
                if let Err(error) = result && (reply.is_none() || self.hagency.is_none()) {
                    self.notice(cx, &error);
                }
            }
        }
        if redraw {
            if let Err(e) = self.render(cx) {
                self.notice(cx, &e);
            }
        }
    }
}
impl MiniAppsPanel {
    fn notify_script_visibility(&mut self, cx: &mut Cx, visible: bool) {
        if !self.open || self.lease.is_none()
            || self.package.as_ref().and_then(Package::builtin_id)
                != Some(octosense_app_contract::hagency::APP_ID)
        { return; }
        let state = if visible { "visible" } else { "hidden" };
        if self.view.splash(cx, ids!(card)).call_script_fn_with_strings(
            cx, id!(on_app_visibility_changed), &[state],
        ) {
            self.reply_redraw = cx.new_next_frame();
        }
    }
}
impl Widget for MiniAppsPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.open {
            return;
        }
        // Desktop focus is routed by the exact mini-app window host. On mobile
        // the retained panel follows the application's foreground lifecycle.
        #[cfg(any(target_os = "android", target_os = "ios", target_env = "ohos"))]
        match event {
            Event::Background => self.notify_script_visibility(cx, false),
            Event::Foreground => self.notify_script_visibility(cx, true),
            _ => {}
        }
        self.refresh_script_language(cx, crate::i18n::language());
        if std::mem::take(&mut self.restyle_card) {
            if let Err(error) = self.render(cx) { self.notice(cx, &error); }
        }
        self.poll_execution_confirmation(cx);
        self.poll_grant_confirmation(cx);
        self.poll_adoption_confirmation(cx);
        if self.account_login_key(cx,event) || self.execution_key(cx,event) || self.reauthentication_key(cx,event) || self.adoption_key(cx,event) || self.grant_key(cx,event) || self.ceiling_key(cx,event) {return;}
        self.view.handle_event(cx, event, scope);
        if self.confirmation_redraw.is_event(event).is_some() {
            self.confirmation_redraw = NextFrame::default();
            self.redraw_confirmation_pass(cx);
        }
        if self.reply_redraw.is_event(event).is_some() {
            self.reply_redraw = NextFrame::default();
            self.view.redraw(cx);
        }
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(close)).clicked(actions) {
                if self.navigate_back(cx) { cx.action(MiniAppsAction::Close); }
                return;
            }
            if self.reauthentication.is_some() {
                if self.view.button(cx,ids!(account_confirmation.buttons.cancel)).clicked(actions){self.finish_reauthentication(cx,false);}
                else if self.view.button(cx,ids!(account_confirmation.buttons.confirm)).clicked(actions){self.finish_reauthentication(cx,true);}
                return;
            }
            if self.execution_confirmation.is_some() {
                if self.view.button(cx,ids!(execution_confirmation.buttons.cancel)).clicked(actions){self.close_execution_confirmation(cx);}
                else if self.view.button(cx,ids!(execution_confirmation.buttons.primary)).clicked(actions){self.execution_primary(cx);}
                else if self.view.button(cx,ids!(execution_confirmation.buttons.recover)).clicked(actions){self.execution_recover(cx);}
                else if self.view.button(cx,ids!(execution_confirmation.scroll.toggle_details)).clicked(actions){self.execution_details(cx);}
                return;
            }
            if self.adoption_confirmation.is_some() {
                if self.view.button(cx,ids!(adoption_confirmation.buttons.cancel)).clicked(actions) {self.close_adoption_confirmation(cx);}
                else if self.view.button(cx,ids!(adoption_confirmation.buttons.primary)).clicked(actions) {self.adoption_primary(cx);}
                else if self.view.button(cx,ids!(adoption_confirmation.buttons.recover)).clicked(actions) {self.adoption_recover(cx);}
                return;
            }
            // A headless preview of the grant window takes the same taps; it has no flow.
            #[cfg(any(test, feature = "onboarding-test-support"))]
            if self.grant_confirmation.is_none() && self.preview_grant_actions(cx, actions) {
                return;
            }
            if self.grant_confirmation.is_some() {
                self.capture_grant_choices(cx);
                let add_field = self.view.text_input(cx, ids!(grant_confirmation.scroll.people.list.add.add_name));
                let selected_person = self.view.drop_down(cx, ids!(grant_confirmation.scroll.people.list.add.directory.candidates)).selected(actions);
                if self.view.button(cx,ids!(grant_confirmation.buttons.cancel)).clicked(actions) {self.close_grant_confirmation(cx);}
                else if self.view.button(cx,ids!(grant_confirmation.buttons.primary)).clicked(actions) {self.grant_primary(cx);}
                else if self.view.button(cx,ids!(grant_confirmation.scroll.actions.recover)).clicked(actions) {self.grant_recover(cx);}
                else if self.view.button(cx,ids!(grant_confirmation.scroll.actions.cancel_intent)).clicked(actions) {self.grant_cancel_intent(cx);}
                else if self.grant_calendar_actions(cx, actions) {}
                else if self.grant_renewal_actions(cx, actions) {}
                else if let Some(days) = self.grant_preset_clicked(cx,actions) {self.grant_expiry_preset(cx,days);}
                else if self.view.button(cx,ids!(grant_confirmation.scroll.sharing.sharing_row.sharing_toggle)).clicked(actions) {self.grant_sharing_toggle(cx);}
                else if self.view.button(cx,ids!(grant_confirmation.scroll.people.list.open_add.open_add_button)).clicked(actions) {self.grant_add_people(cx);}
                else if self.view.button(cx,ids!(grant_confirmation.scroll.more.more_row.more_toggle)).clicked(actions) {self.grant_disclose(cx);}
                else if self.view.button(cx,ids!(grant_confirmation.scroll.people.list.people_toggle_row.people_toggle)).clicked(actions) {self.grant_people_toggle(cx);}
                else if self.view.button(cx,ids!(grant_confirmation.scroll.people.list.add.add_button)).clicked(actions) || add_field.returned(actions).is_some() {self.grant_add_person(cx);}
                else if let Some(index) = selected_person {self.grant_select_person(cx, index);}
                else if self.view.button(cx, ids!(grant_confirmation.scroll.people.list.add.directory.add_selected)).clicked(actions) {self.grant_add_selected_person(cx);}
                else if add_field.changed(actions).is_some() {self.grant_add_typing(cx);}
                else {
                    for (index, row) in sheet_ui::PEOPLE.iter().enumerate() {
                        let path = sheet_ui::sheet_path(id!(grant_confirmation), &[id!(scroll), id!(people), id!(list), *row, id!(body), id!(remove)]);
                        if self.view.button(cx, &path).clicked(actions) {self.grant_remove_person(cx, index); break;}
                    }
                }
                return;
            }
            self.handle_account_login_actions(cx,actions);
            if self.provider_confirmation.is_some() {
                self.handle_personal_controls(cx,actions);
                self.capture_ceiling_choice(cx);
                if self.view.button(cx, ids!(ceiling_confirmation.buttons.primary)).clicked(actions) {
                    self.finish_provider_confirmation(cx, true);
                } else if self.view.button(cx, ids!(ceiling_confirmation.buttons.cancel)).clicked(actions) {
                    self.finish_provider_confirmation(cx, false);
                } else if self.view.button(cx, ids!(ceiling_confirmation.scroll.more.more_row.more_toggle)).clicked(actions) {
                    if let Some(prompt) = self.provider_confirmation.as_mut().and_then(|pending| pending.consent.ceiling_mut()) {prompt.toggle_more();}
                    self.present_provider_confirmation(cx);
                    self.request_confirmation_redraw(cx);
                } else if self.view.button(cx, ids!(provider_confirmation.buttons.connect)).clicked(actions) {
                    self.finish_provider_confirmation(cx, true);
                } else if self.view.button(cx, ids!(provider_confirmation.buttons.cancel)).clicked(actions) {
                    self.finish_provider_confirmation(cx, false);
                }
                return;
            }
            if self.project_room_reply.is_some() {
                if let Some(index) = self.view.drop_down(cx, ids!(project_room_picker.rooms)).selected(actions) {
                    self.present_selected_room(cx, index);
                }
                if self.view.button(cx, ids!(project_room_picker.use_room)).clicked(actions) {
                    self.finish_room_picker(cx, true);
                } else if self.view.button(cx, ids!(project_room_picker.cancel_room)).clicked(actions) {
                    self.finish_room_picker(cx, false);
                }
            }
            if self.view.button(cx, ids!(browse_hub)).clicked(actions) {
                self.show_hub(cx);
                return;
            }
            if self.view.button(cx, ids!(hagency_cancel)).clicked(actions) {
                self.show_catalog(cx);
                return;
            }
            for action in actions {
                if !self.showing_hub { break; }
                if let Some(action) = action.downcast_ref::<LibraryAction>() {
                    match action {
                        LibraryAction::Back => { self.navigate_back(cx); }
                        LibraryAction::Developer => {
                            self.return_to_hub = true;
                            self.show_import(cx);
                        }
                        LibraryAction::Article => {
                            cx.action(MiniAppsAction::Close);
                            crate::system_apps::open_article(cx);
                        }
                        LibraryAction::Launch(bundle, room, account) => {
                            if crate::sliding_sync::current_user_id().is_none_or(|user| user.as_str() != account) { continue; }
                            self.return_to_hub = true;
                            self.reviewed_room = room.clone().unwrap_or_default();
                            self.view.text_input(cx, ids!(import_form.room)).set_text(cx, &self.reviewed_room);
                            let result = Package::load_verified(bundle.clone(), &crate::app_data_dir().join("miniapps/imports"))
                                .and_then(|package| { self.package = Some(package); self.run(cx) });
                            if let Err(error) = result {
                                self.show_hub(cx);
                                self.notice(cx, &error);
                            } else {
                                self.view.mini_app_library(cx, ids!(library)).record(cx, &bundle.manifest.id);
                            }
                        }
                    }
                }
            }
            if self.view.button(cx, ids!(import_app)).clicked(actions) {
                self.show_import(cx);
                self.notice(cx, "Import an OctoSense bundle to review its services.");
            }
            let list = self.view.portal_list(cx, ids!(catalog_list));
            for (index, item) in list.items_with_actions(actions) {
                if !list.was_scrolling() && item.button(cx, ids!(launch)).clicked(actions) {
                    if let Some(index) = self.catalog_apps.get(index).copied() {
                        if let Err(e) = self.open_builtin(cx, index) { self.notice(cx, &e); }
                    }
                    return;
                }
            }
            let approve = self
                .view
                .button(cx, ids!(approval.buttons.allow))
                .clicked(actions);
            let deny = self
                .view
                .button(cx, ids!(approval.buttons.deny))
                .clicked(actions);
            if approve || deny {
                if let (Some(approval), Some(provider), Some(lease)) = (
                    self.approvals.pop_front(),
                    self.provider.clone(),
                    self.lease.clone(),
                ) {
                    let (tx, rx) = mpsc::sync_channel(1);
                    match provider.decide(lease, &approval.id, approve, tx) {
                        Ok(()) => self.pending.push(Pending {
                            reply: None,
                            target: None,
                            receiver: rx,
                            started: Instant::now(),
                            turn: false,
                            navigation: None,
                        }),
                        Err(e) => self.notice(cx, &e),
                    }
                    self.show_approval(cx);
                }
            }
            let result = if self
                .view
                .button(cx, ids!(import_form.core.connect))
                .clicked(actions)
            {
                self.connect(cx)
            } else if self.view.button(cx, ids!(import_form.core.local_buttons.use_local)).clicked(actions) {
                self.use_local(cx)
            } else if self.view.button(cx, ids!(import_form.core.local_buttons.turn_off)).clicked(actions) {
                let result = crate::octos_service::turn_off();
                self.show_assistant_status(cx);
                result
            } else if self.view.button(cx, ids!(review)).clicked(actions) {
                self.review(cx)
            } else if self.view.button(cx, ids!(run)).clicked(actions)
                || self.view.button(cx, ids!(hagency_run)).clicked(actions) {
                self.run(cx)
            } else {
                Ok(())
            };
            if let Err(e) = result {
                self.stop(cx);
                self.notice(cx, &e);
                self.present_review_controls(cx);
            }
        }
        if let Event::BackPressed { handled } = event {
            handled.set(true);
            if self.navigate_back(cx) { cx.action(MiniAppsAction::Close); }
        }
        self.pump(cx);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // The direct Agent surface already supplies its navigation. The generic
        // mini-app Back row is only needed when entering through the catalog.
        if self.direct_agent {
            self.view.view(cx, ids!(header)).set_visible(cx, false);
        }
        self.view.label(cx, ids!(header.title)).set_text(cx,
            crate::i18n::tr(if self.direct_agent {"Agent"} else {"Mini apps"}));
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.borrow_mut::<PortalList>() {
                let apps = crate::system_apps::apps();
                let desktop = crate::home::home_screen::effective_is_desktop(cx);
                self.catalog_apps = apps.iter().enumerate()
                    .filter(|(_, app)| !desktop || app.manifest.id != super::palpo::APP_ID)
                    .map(|(index, _)| index).collect();
                list.set_item_range(cx, 0, self.catalog_apps.len());
                while let Some(index) = list.next_visible_item(cx) {
                    if let Some(app) = self.catalog_apps.get(index).and_then(|index| apps.get(*index)) {
                        let row = list.item(cx, index, id!(App));
                        let operations = app.manifest.id == super::palpo::APP_ID;
                        row.widget(cx, ids!(article_icon)).set_visible(cx, !operations);
                        row.widget(cx, ids!(operations_icon)).set_visible(cx, operations);
                        row.label(cx, ids!(copy.name)).set_text(cx, crate::i18n::tr(&app.manifest.name));
                        row.label(cx, ids!(copy.subtitle)).set_text(cx, if app.manifest.id == super::palpo::APP_ID {"Agents, resource pools and pending actions."} else {crate::i18n::tr("Your article, your style.")});
                        row.draw_all(cx, &mut Scope::empty());
                    }
                }
            }
        }
        DrawStep::done()
    }
}
impl MiniAppsPanelRef {
    /// Optional retained-heap hook; it cannot reopen a lease or replay startup.
    pub(super) fn script_visibility(&self, cx: &mut Cx, visible: bool) {
        let Some(mut inner) = self.borrow_mut() else { return; };
        inner.notify_script_visibility(cx, visible);
    }

    /// The app this panel is reviewing or running, as (manifest id, name), so a
    /// host with one tab per opened item can find it again. `None` while the
    /// panel is closed or shows the catalog or Hub.
    pub(super) fn current_app(&self) -> Option<(String, String)> {
        let inner = self.borrow()?;
        if !inner.open || inner.showing_catalog || inner.showing_hub {
            return None;
        }
        inner.package.as_ref().map(|package| (package.manifest.id.clone(), package.manifest.name.clone()))
    }

    /// Whether the panel is open: closing it stops its app and revokes its lease.
    pub fn is_open(&self) -> bool {
        self.borrow().is_some_and(|inner| inner.open)
    }

    /// Tells the assistant what this panel shows again, for a host whose
    /// front panel changed: the assistant follows the panel in front.
    pub(super) fn publish_to_assistant(&self) {
        if let Some(inner) = self.borrow() {
            inner.publish_to_assistant();
        }
    }

    pub fn open_board(&self, cx: &mut Cx, target: super::palpo::ActionsRoomTarget) -> Result<(), String> {
        let mut inner = self.borrow_mut().ok_or("Mini app view unavailable")?;
        inner.open = true;
        inner.board = Some(target);
        inner.palpo_action = None;
        let index = crate::system_apps::apps().iter().position(|a| a.manifest.id == super::palpo::APP_ID)
            .ok_or("Palpo app is not installed")?;
        inner.open_builtin(cx, index)
    }

    pub fn close_board(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.stop(cx);
            inner.open = false;
            inner.board = None;
        }
    }
    /// Consume Back synchronously before the underlying Rinx/shell navigation.
    pub fn back(&self, cx: &mut Cx, modal: ModalRef) {
        let mut close = false;
        if let Some(mut inner) = self.borrow_mut() {
            close = inner.navigate_back(cx);
        }
        if close { self.action(cx, modal, &MiniAppsAction::Close); }
    }

    pub fn action(&self, cx: &mut Cx, modal: ModalRef, action: &MiniAppsAction) {
        if let Some(mut inner) = self.borrow_mut() {
            match action {
                MiniAppsAction::OpenSignupApproval(_) | MiniAppsAction::OpenAgentChat(_) | MiniAppsAction::OpenActionsRoom(_) | MiniAppsAction::OpenToolApprovals(_) => {} // Handled by the account-bound app shell.
                MiniAppsAction::Open => {
                    inner.open = true;
                    inner.show_catalog(cx);
                    modal.open(cx);
                }
                MiniAppsAction::OpenAgent => {
                    let already_open = inner.open && !inner.showing_catalog && !inner.showing_hub
                        && inner.package.as_ref().and_then(Package::builtin_id) == Some(super::palpo::APP_ID);
                    inner.open = true;
                    inner.direct_agent = true;
                    if !already_open {
                        inner.palpo_action = None;
                        inner.board = None;
                        if let Some(index) = crate::system_apps::apps().iter().position(|app| app.manifest.id == super::palpo::APP_ID) {
                            if let Err(error) = inner.open_builtin(cx, index) { inner.notice(cx, &error); }
                        }
                    }
                    modal.open(cx);
                }
                MiniAppsAction::ReviewOwnerApprovals { owner } => {
                    inner.open_owner_approvals(cx, owner);
                    modal.open(cx);
                }
                MiniAppsAction::OpenPalpo(id) => {
                    inner.open = true;
                    inner.palpo_action = Some(id.clone());
                    if let Some(index) = crate::system_apps::apps().iter().position(|a| a.manifest.id == super::palpo::APP_ID) {
                        if let Err(error) = inner.open_builtin(cx, index) { inner.notice(cx, &error); }
                    }
                    modal.open(cx);
                }
                MiniAppsAction::OpenReviewed => {
                    inner.return_to_hub = false;
                    inner.view.mini_app_library(cx, ids!(library)).cancel_open();
                    inner.open = true;
                    inner.show_import(cx);
                    let notice = inner.review_notice.clone();
                    inner.notice(cx, &notice);
                    inner.show_assistant_status(cx);
                    inner.present_review_controls(cx);
                    modal.open(cx);
                }
                MiniAppsAction::Close => {
                    inner.view.mini_app_library(cx, ids!(library)).cancel_open();
                    inner.stop(cx);
                    inner.open = false;
                    modal.close(cx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempfile_path_for_room_test() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rinx-import-room-test-{}", std::process::id()))
    }

    #[test]
    fn native_panel_keeps_catalog_hub_and_import_back_navigation_separate() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::i18n::install(vm);
            makepad_code_editor::script_mod(vm);
            crate::shared::script_mod(vm);
            vm.bx.captured_errors = Some(Vec::new());
            crate::miniapps::script_mod(vm);
            let value = vm.eval(script! {use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
            let panel = MiniAppsPanel::script_from_value(vm, value);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "Mini-app panel script errors: {errors:?}");
            panel
        });
        // The hidden Hub also has a `room` DropDown. Generic ids!(room)
        // resolves it first, losing a developer-import room grant.
        let room = "!import-room:example.org";
        panel.view.text_input(&cx, ids!(import_form.room)).set_text(&mut cx, room);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/miniapps/matrix-octos-script");
        let snapshots = tempfile_path_for_room_test();
        let package = Package::load_in(&path, &snapshots).expect("example bundle");
        panel.review_package(&mut cx, package).expect("room review");
        assert_eq!(panel.reviewed_room, room);
        assert!(panel.review_notice.contains(&format!("Allowed room: {room}")));
        panel.view.text_input(&cx, ids!(import_form.room)).set_text(&mut cx, "");
        let management = Package::load_builtin(super::super::palpo::APP_ID, &snapshots).expect("versioned built-in Hagency");
        assert!(management.manifest.requires.iter().any(|v| v == octosense_app_contract::hagency::FEATURE));
        assert!(management.manifest.capabilities.iter().all(|v| !v.starts_with("palpo.")));
        let _ = std::fs::remove_dir_all(snapshots);
        panel.show_catalog(&mut cx);
        assert!(panel.navigate_back(&mut cx), "Only the outer catalog closes");
        panel.show_hub(&mut cx);
        assert!(panel.showing_hub);
        assert!(!panel.navigate_back(&mut cx));
        assert!(panel.showing_catalog);
        panel.show_hub(&mut cx);
        panel.return_to_hub = true;
        panel.show_import(&mut cx);
        assert!(!panel.navigate_back(&mut cx));
        assert!(panel.showing_hub, "Developer import returns to its Hub entry point");
        assert!(!panel.navigate_back(&mut cx));
        assert!(panel.showing_catalog);
        panel.show_import(&mut cx);
        assert!(!panel.navigate_back(&mut cx));
        assert!(panel.showing_catalog, "Catalog import returns to the catalog");
    }
}
