//! Script DSL for the agent panel. Registers `mod.widgets.HagencyAgentPanel`.
use makepad_widgets::*;
use super::AgentPanel;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Hint = Text {draw_text +: {color: mod.widgets.RINX_MUTED text_style.font_size: (10.5 * mod.widgets.RINX_TEXT_SCALE)}}
    let Title = Text {draw_text.text_style: theme.font_bold{font_size: (14 * mod.widgets.RINX_TEXT_SCALE)}}
    let Field = View {width: Fill height: Fit flow: Down spacing: 4}
    let Input = RobrixTextInput {
        width: Fill height: 40 margin: 0 padding: Inset{left: 12 right: 12 top: 8 bottom: 8}
        draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
    }
    let Select = RobrixSettingsDropDown {
        width: Fill height: 40 margin: 0
        popup_menu_position: PopupMenuPosition.BelowInput
        draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
        popup_menu: RobrixSettingsPopupMenu {width: 420 menu_item: RobrixSettingsPopupMenuItem {height: 36 draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    }
    let Action = RobrixNeutralIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}
    let Primary = RobrixIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}
    let Danger = RobrixNegativeIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}
    let Card = RoundedView {width: Fill height: Fit flow: Down padding: 16 spacing: 10
        draw_bg +: {color: mod.widgets.RINX_SURFACE border_color: mod.widgets.RINX_BORDER border_size: 1 border_radius: 8}
    }
    let Page = View {visible: false width: Fill height: Fit flow: Down spacing: 10}

    mod.widgets.HagencyAgentPanel = #(AgentPanel::register_widget(vm)) {
        width: Fill height: Fit flow: Down spacing: 12
        View {width: Fill height: Fit flow: Right spacing: 16 align: Align{y: 0.5}
            avatar := Avatar {width: 56 height: 56}
            View {width: Fill height: Fit flow: Down spacing: 4
                name := Text {draw_text.text_style: theme.font_bold{font_size: (18 * mod.widgets.RINX_TEXT_SCALE)}}
                runs_on := Hint {}
            }
            message := Primary {text: #(crate::i18n::tr("Message")) i18n_text: "Message"}
        }
        View {width: Fill height: 40 spacing: 8
            identity_tab := Action {text: #(crate::i18n::tr("Identity")) i18n_text: "Identity"}
            context_tab := Action {text: #(crate::i18n::tr("Context")) i18n_text: "Context"}
            activity_tab := Action {text: #(crate::i18n::tr("Activity")) i18n_text: "Activity"}
            settings_tab := Action {text: #(crate::i18n::tr("Settings")) i18n_text: "Settings"}
        }
        status := Hint {}
        identity_page := Page {visible: true
            Card {
                Title {text: #(crate::i18n::tr("Name")) i18n_text: "Name"}
                View {width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                    rename_input := Input {empty_text: #(crate::i18n::tr("Agent name")) i18n_empty_text: "Agent name"}
                    rename_save := Primary {text: #(crate::i18n::tr("Rename")) i18n_text: "Rename"}
                }
                Hint {text: #(crate::i18n::tr("People see the new name in every room.")) i18n_text: "People see the new name in every room."}
            }
            Card {
                Title {text: #(crate::i18n::tr("About")) i18n_text: "About"}
                Text {text: #(crate::i18n::tr("Matrix ID")) i18n_text: "Matrix ID"}
                View {width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                    matrix_id := Text {}
                    copy_matrix_id := Action {text: #(crate::i18n::tr("Copy")) i18n_text: "Copy"}
                }
                about := Text {}
                about_rooms := Action {}
            }
            Card {
                Field {Text {text: #(crate::i18n::tr("Model")) i18n_text: "Model"} panel_model := Select {labels: [#(crate::i18n::tr("Checking model choices…"))]}}
                Field {Text {text: #(crate::i18n::tr("Thinking")) i18n_text: "Thinking"} thinking := Select {labels: [#(crate::i18n::tr("Default"))]}}
                model_hint := Hint {}
                Field {Text {text: #(crate::i18n::tr("Backup model")) i18n_text: "Backup model"} backup := Select {labels: [#(crate::i18n::tr("None"))]}}
                backup_hint := Hint {}
            }
            Card {
                Title {text: #(crate::i18n::tr("Who can ask this agent")) i18n_text: "Who can ask this agent"}
                who := Select {labels: [#(crate::i18n::tr("Only me")) #(crate::i18n::tr("People in rooms I add it to")) #(crate::i18n::tr("Anyone who can reach it (hosted Agents)"))]}
                who_hint := Hint {}
                Hint {text: #(crate::i18n::tr("High-risk tools always ask you first, whatever is chosen here.")) i18n_text: "High-risk tools always ask you first, whatever is chosen here."}
            }
            Card {
                Title {text: #(crate::i18n::tr("Credit")) i18n_text: "Credit"}
                credit_text := Text {draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
                credit_bar := ProgressBar {height: 8}
                credit_detail := Hint {}
            }
        }
        context_page := Page {
            Card {
                Title {text: #(crate::i18n::tr("Rooms")) i18n_text: "Rooms"}
                rooms := Text {}
                // Ticked: the Agent is in that room. Unticking makes it leave.
                room_ticks := View {width: Fill height: Fit flow: Down spacing: 4
                    room_tick_0 := CheckBox {visible: false} room_tick_1 := CheckBox {visible: false}
                    room_tick_2 := CheckBox {visible: false} room_tick_3 := CheckBox {visible: false}
                    room_tick_4 := CheckBox {visible: false} room_tick_5 := CheckBox {visible: false}
                    room_tick_6 := CheckBox {visible: false} room_tick_7 := CheckBox {visible: false}
                }
                add_room := Select {labels: [#(crate::i18n::tr("Add to a room…"))]}
                leave_open_room := Danger {visible: false text: #(crate::i18n::tr("Remove my agent from this room")) i18n_text: "Remove my agent from this room"}
                Hint {text: #(crate::i18n::tr("Saved conversation contexts per room, each with Forget, come next.")) i18n_text: "Saved conversation contexts per room, each with Forget, come next."}
            }
        }
        activity_page := Page {
            Card {
                Title {text: #(crate::i18n::tr("Recent requests")) i18n_text: "Recent requests"}
                activity := Text {}
            }
        }
        settings_page := Page {
            Card {
                Title {text: #(crate::i18n::tr("Approvals")) i18n_text: "Approvals"}
                approval_mode := Select {labels: [#(crate::i18n::tr("Ask every time")) #(crate::i18n::tr("Ask, remember approvals")) #(crate::i18n::tr("Ask only for outside changes"))]}
                approval_hint := Hint {}
                Text {text: #(crate::i18n::tr("Remembered approvals")) i18n_text: "Remembered approvals"}
                View {width: Fill height: Fit flow: Right spacing: 8
                    rules := Select {labels: [#(crate::i18n::tr("No remembered approvals"))]}
                    revoke := Action {text: #(crate::i18n::tr("Revoke")) i18n_text: "Revoke"}
                }
                Text {text: #(crate::i18n::tr("Pull request merges")) i18n_text: "Pull request merges"}
                auto_merge := Select {labels: [#(crate::i18n::tr("People merge")) #(crate::i18n::tr("Agents merge low-risk PRs"))]}
                Hint {text: #(crate::i18n::tr(super::mapping::AUTO_MERGE_HINT))}
            }
            // What the Agent does for a group room's members (ADR 0024).
            Card {
                Title {text: #(crate::i18n::tr("In group rooms")) i18n_text: "In group rooms"}
                share_room := Select {labels: [#(crate::i18n::tr("Not in a group room yet"))]}
                share_answer := CheckBox {text: #(crate::i18n::tr("Answer questions")) i18n_text: "Answer questions"}
                share_review := CheckBox {text: #(crate::i18n::tr("Review PRs")) i18n_text: "Review PRs"}
                share_fix := CheckBox {text: #(crate::i18n::tr("Fix issues without asking")) i18n_text: "Fix issues without asking"}
                share_hint := Hint {}
                Hint {text: #(crate::i18n::tr(super::mapping::SHARE_HINT))}
            }
            // The int build's own blocks: issue watching (incl. Trusted mode and
            // Room choice) and the per-Agent GitHub access token with Check.
            Card {issue_watch := mod.widgets.IssueWatchSection {}}
            Card {work := mod.widgets.WorkSection {}}
            Card {github_access := mod.widgets.GithubAccessSection {}}
            Card {
                Title {text: #(crate::i18n::tr("Budget")) i18n_text: "Budget"}
                panel_limit_mode := Select {labels: [#(crate::i18n::tr("No limit")) #(crate::i18n::tr("Token limit"))]}
                token_holder := Field {Text {text: #(crate::i18n::tr("Tokens")) i18n_text: "Tokens"} panel_token_limit := Input {empty_text: "2000000"}}
                panel_period := Select {labels: [#(crate::i18n::tr("In total")) #(crate::i18n::tr("Per day (UTC)")) #(crate::i18n::tr("Per month (UTC)"))]}
                save_budget := Primary {text: #(crate::i18n::tr("Save budget")) i18n_text: "Save budget"}
            }
            Card {
                Title {text: #(crate::i18n::tr("Rooms")) i18n_text: "Rooms"}
                join_rooms := CheckBox {text: #(crate::i18n::tr("Join rooms I join")) i18n_text: "Join rooms I join"}
                Hint {text: #(crate::i18n::tr("When off, you are asked each time you join a room.")) i18n_text: "When off, you are asked each time you join a room."}
            }
            Card {
                Title {text: #(crate::i18n::tr("Workspace folder")) i18n_text: "Workspace folder"}
                panel_workspace := Text {}
            }
            Card {
                Title {text: #(crate::i18n::tr("Service")) i18n_text: "Service"}
                service_state := Text {}
                panel_service_toggle := Action {text: #(crate::i18n::tr("Pause")) i18n_text: "Pause"}
            }
            Card {
                Title {text: #(crate::i18n::tr("Retire")) i18n_text: "Retire"}
                panel_retire_confirm := CheckBox {text: #(crate::i18n::tr("I confirm retirement stops every Room; this identity remains mine")) i18n_text: "I confirm retirement stops every Room; this identity remains mine"}
                retire := Danger {text: #(crate::i18n::tr("Retire Agent permanently")) i18n_text: "Retire Agent permanently"}
            }
        }
        advanced := Action {text: #(crate::i18n::tr("Advanced settings")) i18n_text: "Advanced settings"}
    }
}
