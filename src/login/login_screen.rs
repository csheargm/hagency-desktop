use makepad_widgets::*;

use crate::sliding_sync::{submit_async_request, LoginByPassword, LoginRequest, MatrixRequest};

use super::homeserver::{password_identifier, LoginMethods};
use super::login_status_modal::{LoginStatusModalAction, LoginStatusModalWidgetExt};

// TODO(R4): this file is over budget; split the DSL and the action handling
// into child modules along the server / method / registration steps.
#[path = "login_screen_account.rs"]
mod account;
use account::AccountUi;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.IMG_APP_LOGO = crate_resource("self://resources/icon_256.png")
    mod.widgets.ICON_EYE_OPEN   = crate_resource("self://resources/icons/eye_open.svg")
    mod.widgets.ICON_EYE_CLOSED = crate_resource("self://resources/icons/eye_closed.svg")

    mod.widgets.LoginScreen = set_type_default() do #(LoginScreen::register_widget(vm)) {
        ..mod.widgets.SolidView

        width: Fill, height: Fill,
        align: Align{x: 0.5, y: 0.5}
        show_bg: true,
        draw_bg +: {
            color: COLOR_SECONDARY
        }

        ScrollYView {
            width: Fill, height: Fill,
            flow: Down, // Required for vertical scrolling to work.
            align: Align{x: 0.5, y: 0.5}
            show_bg: true,
            draw_bg.color: (COLOR_SECONDARY)

            // allow the view to be scrollable but hide the actual scroll bar
            scroll_bars: {
                show_scroll_x: false, show_scroll_y: true,
                scroll_bar_y: {
                    bar_size: 0.0
                    min_handle_size: 0.0
                    drag_scrolling: true
                }
            }

            RoundedView {
                margin: Inset{left: 16, right: 16, top: 50, bottom: 50}
                width: Fill
                height: Fit
                align: Align{x: 0.5, y: 0.5}
                flow: Overlay,

                show_bg: true,
                draw_bg +: {
                    color: (COLOR_SECONDARY)
                    border_radius: 6.0
                }

                View {
                    width: Fill
                    height: Fit
                    flow: Down
                    align: Align{x: 0.5, y: 0.5}
                    spacing: 15.0

                    logo_image := Image {
                        fit: ImageFit.Smallest,
                        width: 80
                        src: (mod.widgets.IMG_APP_LOGO),
                    }

                    title := Label {
                        width: Fit, height: Fit
                        margin: Inset{ bottom: 5 }
                        padding: 0,
                        draw_text +: {
                            color: (COLOR_TEXT)
                            text_style: TITLE_TEXT {font_size: (16.0 * mod.widgets.RINX_TEXT_SCALE)}
                        }
                        text: #(crate::i18n::tr("Sign in to Hagency")) i18n_text: "Sign in to Hagency"
                    }

                    View {
                        width: Fit height: Fit flow: Right spacing: 12
                        login_language_en := ButtonFlat {text: "English"}
                        login_language_zh := ButtonFlat {text: "简体中文"}
                    }

                    saved_accounts := mod.widgets.AccountList {
                        width: Fill{max: 320}, show_add: false
                    }

                    server_step := View {
                        width: Fill{max: 320}, height: Fit, flow: Down, spacing: 14

                        RoundedView {
                            width: Fill, height: Fit
                            flow: Down, spacing: 9
                            padding: Inset{left: 14, right: 14, top: 14, bottom: 14}
                            show_bg: true
                            draw_bg +: {
                                color: mod.widgets.RINX_SURFACE
                                border_size: 1
                                border_color: mod.widgets.RINX_BORDER
                                border_radius: 8
                            }
                            Label {
                                width: Fill, height: Fit
                                draw_text +: {color: COLOR_TEXT, text_style: REGULAR_TEXT {font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
                                text: #(crate::i18n::tr("Choose your Hagency server")) i18n_text: "Choose your Hagency server"
                            }
                            recent_servers_row := View {
                                visible: false, width: Fill, height: Fit
                                recent_servers := RobrixSettingsDropDown {
                                    width: Fill, height: 38, margin: 0
                                    popup_menu_position: PopupMenuPosition.BelowInput
                                    popup_menu: RobrixSettingsPopupMenu {
                                        width: 292
                                        draw_bg +: {color: mod.widgets.RINX_SURFACE}
                                        menu_item: RobrixSettingsPopupMenuItem {
                                            draw_bg +: {color: mod.widgets.RINX_SURFACE}
                                        }
                                    }
                                    labels: ["Other server…"]
                                }
                            }
                            manual_server_row := View {
                                width: Fill, height: Fit
                                homeserver_input := RobrixTextInput {
                                    width: Fill, height: Fit
                                    padding: 10
                                    empty_text: #(crate::i18n::tr("Hagency server address"))
                                    autocapitalize: None
                                    autocorrect: Disabled
                                    content_type: Url
                                    input_mode: Url
                                }
                            }
                            Label {
                                width: Fill, height: Fit
                                flow: Flow.Right{wrap: true}
                                draw_text +: {color: mod.widgets.RINX_MUTED, text_style: REGULAR_TEXT {font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
                                text: #(crate::i18n::tr("Enter your Hagency server address. Other Matrix servers are not supported.")) i18n_text: "Enter your Hagency server address. Other Matrix servers are not supported."
                            }
                        }
                        continue_server_button := RobrixIconButton {
                            width: Fill, height: 42, padding: 10
                            align: Align{x: 0.5, y: 0.5}
                            text: #(crate::i18n::tr("Continue")) i18n_text: "Continue"
                        }
                        server_status := Label {
                            width: Fill, height: Fit
                            flow: Flow.Right{wrap: true}
                            draw_text +: {color: COLOR_TEXT, text_style: REGULAR_TEXT {font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
                            text: ""
                        }
                    }

                    method_step := View {
                        visible: false
                        width: Fill{max: 320}, height: Fit, flow: Down, spacing: 14

                        RoundedView {
                            width: Fill, height: Fit
                            flow: Down, spacing: 4
                            padding: Inset{left: 14, right: 14, top: 10, bottom: 12}
                            show_bg: true
                            draw_bg +: {
                                color: mod.widgets.RINX_SURFACE
                                border_size: 1
                                border_color: mod.widgets.RINX_BORDER
                                border_radius: 8
                            }
                            View {
                                width: Fill, height: Fit, flow: Right
                                align: Align{y: 0.5}
                                Label {
                                    width: Fill, height: Fit
                                    draw_text +: {color: mod.widgets.RINX_MUTED, text_style: REGULAR_TEXT {font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
                                    text: #(crate::i18n::tr("Selected server")) i18n_text: "Selected server"
                                }
                                edit_server_button := RobrixNeutralIconButton {
                                    width: 52, height: 26
                                    padding: 3, spacing: 0
                                    icon_walk: Walk{width: 0, height: 0}
                                    align: Align{x: 0.5, y: 0.5}
                                    draw_bg +: {
                                        color: mod.widgets.RINX_FIELD
                                        color_hover: mod.widgets.RINX_HOVER
                                        color_down: mod.widgets.RINX_PRESSED
                                        border_size: 1
                                        border_color: mod.widgets.RINX_BORDER
                                        border_radius: 5
                                    }
                                    text: #(crate::i18n::tr("Edit")) i18n_text: "Edit"
                                }
                            }
                            selected_server := Label {
                                width: Fill, height: Fit
                                flow: Flow.Right{wrap: false}
                                max_lines: 1, text_overflow: Ellipsis
                                draw_text +: {color: COLOR_TEXT, text_style: REGULAR_TEXT {font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
                                text: ""
                            }
                        }
                        method_status := Label {
                            width: Fill, height: Fit
                            flow: Flow.Right{wrap: true}
                            draw_text +: {color: COLOR_TEXT, text_style: REGULAR_TEXT {font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
                            text: ""
                        }
                        provider_list_container := View {
                            visible: false
                            width: Fill, height: Fit
                            provider_list := PortalList {
                                width: Fill, height: 48
                                Provider := View {
                                    width: Fill, height: 48
                                    provider_button := RobrixIconButton {
                                        width: Fill, height: 42, padding: 10
                                        align: Align{x: 0.5, y: 0.5}
                                        text: ""
                                    }
                                }
                            }
                        }
                        browser_login_button := RobrixIconButton {
                            visible: false
                            width: Fill, height: 42, padding: 10
                            align: Align{x: 0.5, y: 0.5}
                            text: #(crate::i18n::tr("Continue with single sign-on")) i18n_text: "Continue with single sign-on"
                        }
                        github_login_button := RobrixIconButton {
                            visible: false
                            width: Fill, height: 42, padding: 10
                            align: Align{x: 0.5, y: 0.5}
                            text: #(crate::i18n::tr("Continue with GitHub")) i18n_text: "Continue with GitHub"
                        }
                        password_option_button := ButtonFlat {
                            visible: false
                            width: Fit, height: Fit
                            text: #(crate::i18n::tr("Sign in with a password instead")) i18n_text: "Sign in with a password instead"
                        }
                        password_form := View {
                            visible: false
                            width: Fill, height: Fit, flow: Down, spacing: 12

                            user_id_input := RobrixTextInput {
                                width: Fill, height: Fit
                                padding: 10
                                empty_text: #(crate::i18n::tr("Matrix ID, username or email")) i18n_empty_text: "Matrix ID, username or email"
                                autocapitalize: None
                                autocorrect: Disabled
                                content_type: Username
                            }
                            View {
                                width: Fill, height: Fit
                                flow: Overlay
                                align: Align{x: 1.0, y: 0.5}
                                password_input := RobrixTextInput {
                                    width: Fill, height: Fit
                                    padding: Inset{top: 10, bottom: 10, left: 10, right: 38}
                                    empty_text: #(crate::i18n::tr("Password")) i18n_empty_text: "Password"
                                    is_password: true
                                    autocapitalize: None
                                    autocorrect: Disabled
                                    content_type: Password
                                }
                                View {
                                    width: 38, height: Fill
                                    align: Align{x: 0.5, y: 0.5}
                                    show_password_button := RobrixNeutralIconButton {
                                        width: Fit, height: Fit, padding: 5, spacing: 0, margin: 0
                                        draw_icon +: {svg: (mod.widgets.ICON_EYE_CLOSED), color: mod.widgets.RINX_MUTED}
                                        icon_walk: Walk{width: 18, height: 18, margin: 0}
                                        text: ""
                                    }
                                    hide_password_button := RobrixNeutralIconButton {
                                        visible: false
                                        width: Fit, height: Fit, padding: 5, spacing: 0, margin: 0
                                        draw_icon +: {svg: (mod.widgets.ICON_EYE_OPEN), color: mod.widgets.RINX_MUTED}
                                        icon_walk: Walk{width: 18, height: 18, margin: 0}
                                        text: ""
                                    }
                                }
                            }
                            login_button := RobrixIconButton {
                                width: Fill, height: 42, padding: 10
                                align: Align{x: 0.5, y: 0.5}
                                text: #(crate::i18n::tr("Sign in with password")) i18n_text: "Sign in with password"
                            }
                            forgot_password_button := ButtonFlat {
                                width: Fit, height: Fit
                                text: #(crate::i18n::tr("Forgot password?")) i18n_text: "Forgot password?"
                            }
                        }
                        recovery_form := View {
                            visible: false
                            width: Fill, height: Fit, flow: Down, spacing: 12
                            recovery_email := RobrixTextInput {
                                width: Fill, height: Fit, padding: 10
                                empty_text: #(crate::i18n::tr("Email")) i18n_empty_text: "Email"
                                autocapitalize: None, autocorrect: Disabled
                            }
                            recovery_status := Label {
                                width: Fill, height: Fit
                                flow: Flow.Right{wrap: true}
                                draw_text +: {color: COLOR_TEXT, text_style: REGULAR_TEXT {font_size: (10 * mod.widgets.RINX_TEXT_SCALE)}}
                                text: ""
                            }
                            send_recovery_button := RobrixIconButton {
                                width: Fill, height: 42, padding: 10
                                align: Align{x: 0.5, y: 0.5}
                                text: #(crate::i18n::tr("Send reset link")) i18n_text: "Send reset link"
                            }
                            close_recovery_button := ButtonFlat {
                                width: Fit, height: Fit
                                text: #(crate::i18n::tr("Back to sign in")) i18n_text: "Back to sign in"
                            }
                        }
                        register_option_button := ButtonFlat {
                            width: Fit, height: Fit
                            text: #(crate::i18n::tr("Create an account")) i18n_text: "Create an account"
                        }
                        registration_form := View {
                            visible: false
                            width: Fill, height: Fit, flow: Down, spacing: 12
                            registration_username := RobrixTextInput {
                                width: Fill, height: Fit, padding: 10
                                empty_text: #(crate::i18n::tr("Choose a username")) i18n_empty_text: "Choose a username"
                                autocapitalize: None, autocorrect: Disabled, content_type: Username
                            }
                            registration_password := RobrixTextInput {
                                width: Fill, height: Fit, padding: 10
                                empty_text: #(crate::i18n::tr("Choose a password")) i18n_empty_text: "Choose a password"
                                is_password: true, autocapitalize: None, autocorrect: Disabled, content_type: Password
                            }
                            registration_token := RobrixTextInput {
                                width: Fill, height: Fit, padding: 10
                                empty_text: #(crate::i18n::tr("Registration token, if required")) i18n_empty_text: "Registration token, if required"
                                autocapitalize: None, autocorrect: Disabled
                            }
                            registration_email := RobrixTextInput {
                                width: Fill, height: Fit, padding: 10
                                empty_text: #(crate::i18n::tr("Email (optional, for password recovery)")) i18n_empty_text: "Email (optional, for password recovery)"
                                autocapitalize: None, autocorrect: Disabled
                            }
                            registration_code_row := View {
                                visible: false
                                width: Fill, height: Fit
                                registration_code := RobrixTextInput {
                                    width: Fill, height: Fit, padding: 10
                                    empty_text: #(crate::i18n::tr("Code from the email")) i18n_empty_text: "Code from the email"
                                    autocapitalize: None, autocorrect: Disabled
                                }
                            }
                            submit_registration_button := RobrixIconButton {
                                width: Fill, height: 42, padding: 10
                                align: Align{x: 0.5, y: 0.5}
                                text: #(crate::i18n::tr("Create account")) i18n_text: "Create account"
                            }
                            back_to_login_button := ButtonFlat {
                                width: Fit, height: Fit
                                text: #(crate::i18n::tr("Back to sign in")) i18n_text: "Back to sign in"
                            }
                        }
                    }
                }

                // The modal that pops up to display login status messages,
                // such as when the user is logging in or when there is an error.
                login_status_modal := Modal {
                    can_dismiss: false,
                    content := mod.widgets.LoginStatusModal {}
                }
            }
        }
    }
}

#[derive(Script, Widget)]
pub struct LoginScreen {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    /// Whether the password field is currently showing plaintext.
    #[rust] password_visible: bool,
    #[rust] sso_pending: bool,
    #[rust] login_pending: bool,
    #[rust] discovery_generation: u64,
    #[rust] discovery_pending: bool,
    #[rust] methods: Option<LoginMethods>,
    #[rust] password_form_open: bool,
    #[rust] registration_form_open: bool,
    #[rust] recent_servers: Vec<String>,
    #[rust] recent_servers_dirty: bool,
    #[rust] recent_server_selected: String,
    #[rust] recent_server_prompt: String,
    /// GitHub sign-in, password recovery and email registration (see `account`).
    #[rust] account: AccountUi,

}


impl ScriptHook for LoginScreen {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, _apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        // Rebaking the theme can reset DropDown labels without changing saved accounts.
        self.recent_servers_dirty = true;
    }
}

impl LoginScreen {
    fn sync_recent_servers(&mut self, cx: &mut Cx, servers: Vec<String>) {
        self.sync_recent_servers_with_prompt(cx, servers, crate::i18n::tr("Other server…").to_owned());
    }
    fn sync_recent_servers_with_prompt(&mut self, cx: &mut Cx, servers: Vec<String>, prompt: String) {
        if self.recent_servers_dirty || self.recent_servers != servers || self.recent_server_prompt != prompt {
            self.recent_servers = servers;
            self.recent_server_prompt = prompt.clone();
            let dropdown = self.view.drop_down(cx, ids!(recent_servers));
            let mut labels = vec![prompt];
            labels.extend(self.recent_servers.clone());
            let selected = self.recent_servers.iter().position(|s| s == &self.recent_server_selected).map_or(0, |i| i + 1);
            dropdown.set_labels(cx, labels);
            dropdown.set_selected_item(cx, selected);
            self.recent_servers_dirty = false;
        }
        self.view.view(cx, ids!(recent_servers_row)).set_visible(cx, !self.recent_servers.is_empty());
        let using_recent = self.recent_servers.contains(&self.recent_server_selected);
        self.view.view(cx, ids!(manual_server_row)).set_visible(cx, !using_recent);
    }
}

impl Widget for LoginScreen {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        self.match_event(cx, event);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let mut servers = Vec::new();
        for account in crate::accounts::saved() {
            let server = account.homeserver.trim_end_matches('/').to_owned();
            if !servers.contains(&server) {
                servers.push(server);
            }
        }
        self.sync_recent_servers(cx, servers);
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.borrow_mut::<PortalList>() {
                let providers = self.methods.as_ref().map(|m| m.providers.as_slice()).unwrap_or_default();
                list.set_item_range(cx, 0, providers.len());
                while let Some(index) = list.next_visible_item(cx) {
                    if let Some(provider) = providers.get(index) {
                        let row = list.item(cx, index, id!(Provider));
                        row.button(cx, ids!(provider_button)).set_text(cx, &crate::i18n::format(
                            "Sign in with {provider}",
                            &[("provider", provider.name.clone())],
                        ));
                        row.draw_all(cx, scope);
                    }
                }
            }
        }
        DrawStep::done()
    }
}

impl LoginScreen {
    fn show_status(&mut self, cx: &mut Cx, title: &str, status: &str, button: &str, enabled: bool) {
        let content = self.view.login_status_modal(cx, ids!(login_status_modal.content));
        content.set_title(cx, title);
        content.set_status(cx, status);
        content.button_ref(cx).set_text(cx, button);
        content.button_ref(cx).set_enabled(cx, enabled);
        self.view.modal(cx, ids!(login_status_modal)).open(cx);
        self.redraw(cx);
    }

    fn reset_server(&mut self, cx: &mut Cx) {
        self.discovery_generation += 1;
        self.discovery_pending = false;
        self.methods = None;
        self.password_form_open = false;
        self.registration_form_open = false;
        self.view.view(cx, ids!(server_step)).set_visible(cx, true);
        self.view.view(cx, ids!(method_step)).set_visible(cx, false);
        self.view.label(cx, ids!(server_status)).set_text(cx, "");
        self.view.text_input(cx, ids!(password_input)).set_text(cx, "");
        self.view.text_input(cx, ids!(registration_username)).set_text(cx, "");
        self.view.text_input(cx, ids!(registration_password)).set_text(cx, "");
        self.view.text_input(cx, ids!(registration_token)).set_text(cx, "");
        self.view.view(cx, ids!(registration_form)).set_visible(cx, false);
        self.reset_account_options(cx);
        if self.password_visible {
            self.password_visible = false;
            self.view.text_input(cx, ids!(password_input)).toggle_is_password(cx);
            self.view.button(cx, ids!(show_password_button)).set_visible(cx, true);
            self.view.button(cx, ids!(hide_password_button)).set_visible(cx, false);
        }
    }

    fn check_server(&mut self, cx: &mut Cx, server: String) {
        self.discovery_generation += 1;
        let generation = self.discovery_generation;
        self.discovery_pending = true;
        self.view.label(cx, ids!(server_status)).set_text(cx, crate::i18n::tr("Checking homeserver and sign-in methods…"));
        crate::sliding_sync::spawn_async_task(async move {
            let result = super::homeserver::discover("", &server).await.map_err(|e| e.to_string());
            Cx::post_action(LoginAction::ServerDiscovered { generation, result });
        });
    }

    fn show_methods(&mut self, cx: &mut Cx, methods: LoginMethods) {
        let has_sso = methods.sso || methods.oauth;
        let has_password = methods.password;
        let has_providers = !methods.providers.is_empty();
        let display_server = methods.homeserver.strip_prefix("https://").unwrap_or(&methods.homeserver);
        let display_server = display_server.strip_suffix('/').unwrap_or(display_server);
        self.view.label(cx, ids!(selected_server)).set_text(cx, display_server);
        self.view.view(cx, ids!(provider_list_container)).set_visible(cx, has_sso && has_providers);
        let height = (methods.providers.len().min(4).max(1) * 48) as f64;
        let mut list = self.view.portal_list(cx, ids!(provider_list));
        script_apply_eval!(cx, list, {height: #(height)});
        self.view.button(cx, ids!(browser_login_button)).set_visible(cx, has_sso && !has_providers);
        self.view.button(cx, ids!(password_option_button)).set_visible(cx, has_sso && has_password);
        self.view.button(cx, ids!(register_option_button)).set_visible(cx, methods.browser_registration || (has_password && !methods.oauth_aware_preferred));
        self.password_form_open = has_password && !has_sso;
        self.view.view(cx, ids!(password_form)).set_visible(cx, self.password_form_open);
        self.view.view(cx, ids!(registration_form)).set_visible(cx, false);
        self.view.view(cx, ids!(server_step)).set_visible(cx, false);
        self.view.view(cx, ids!(method_step)).set_visible(cx, true);
        self.methods = Some(methods);
        self.refresh_account_options(cx);
        self.refresh_method_copy(cx);
        self.redraw(cx);
    }

    fn refresh_method_copy(&mut self, cx: &mut Cx) {
        let Some(methods) = self.methods.as_ref() else { return };
        let description = if self.registration_form_open {
            crate::i18n::tr("Create an account on this server.")
        } else if self.password_form_open {
            crate::i18n::tr("This server supports password sign-in.")
        } else if methods.oauth {
            crate::i18n::tr("Sign in once through Pasion to access your chats, Projects and Agents.")
        } else if methods.sso && !methods.providers.is_empty() {
            crate::i18n::tr("Choose a sign-in provider. Your server handles authentication in the browser.")
        } else if methods.sso {
            crate::i18n::tr("Your server handles sign-in in the browser.")
        } else {
            crate::i18n::tr("This server did not advertise SSO or password sign-in.")
        };
        self.view.label(cx, ids!(method_status)).set_text(cx, description);
        self.view.button(cx, ids!(password_option_button)).set_text(cx, if self.password_form_open {
            crate::i18n::tr("Hide password sign-in")
        } else {
            crate::i18n::tr("Sign in with a password instead")
        });
    }

    fn start_sso(&mut self, cx: &mut Cx, provider_id: Option<String>, register: bool) {
        let Some(methods) = self.methods.as_ref().filter(|m| browser_login_allowed(m.sso, m.oauth, m.browser_registration, register)) else { return };
        self.login_pending = true;
        self.sso_pending = true;
        let destination = methods.homeserver.clone();
        self.show_status(cx, crate::i18n::tr("Connecting to your server"), &crate::i18n::format("Checking browser sign-in for {destination}…", &[("destination", destination.clone())]), crate::i18n::tr("Cancel"), true);
        submit_async_request(MatrixRequest::SpawnSSOServer { homeserver_url: destination, provider_id, register });
    }
}

impl MatchEvent for LoginScreen {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let mut language_changed = false;
        for (path, language) in [(ids!(login_language_en), crate::i18n::Language::English), (ids!(login_language_zh), crate::i18n::Language::Chinese)] {
            if self.view.button(cx, path).clicked(actions) {
                if let Err(error) = crate::i18n::set_language(cx, language) {
                    crate::shared::popup_list::enqueue_popup_notification(crate::i18n::format("Could not save language: {error}", &[("error", error.to_string())]), crate::shared::popup_list::PopupKind::Error, Some(5.0));
                } else {
                    language_changed = true;
                }
            }
        }
        if language_changed {
            self.recent_servers_dirty = true;
            self.refresh_method_copy(cx);
        }
        let user_input = self.view.text_input(cx, ids!(user_id_input));
        let password_input = self.view.text_input(cx, ids!(password_input));
        let server_input = self.view.text_input(cx, ids!(homeserver_input));
        if let Some(index) = self.view.drop_down(cx, ids!(recent_servers)).changed(actions) {
            if let Some(server) = index.checked_sub(1).and_then(|i| self.recent_servers.get(i)).cloned() {
                self.recent_server_selected = server.clone();
                self.reset_server(cx);
                server_input.set_text(cx, &server);
                self.view.view(cx, ids!(manual_server_row)).set_visible(cx, false);
            } else {
                self.recent_server_selected.clear();
                self.reset_server(cx);
                self.view.view(cx, ids!(manual_server_row)).set_visible(cx, true);
                server_input.set_text(cx, "");
                server_input.set_key_focus(cx);
            }
        }
        let modal = self.view.modal(cx, ids!(login_status_modal));
        let server = if self.recent_servers.contains(&self.recent_server_selected) {
            self.recent_server_selected.clone()
        } else {
            server_input.text().trim().to_owned()
        };

        let show = self.view.button(cx, ids!(show_password_button));
        let hide = self.view.button(cx, ids!(hide_password_button));
        if show.clicked(actions) || hide.clicked(actions) {
            self.password_visible = !self.password_visible;
            password_input.toggle_is_password(cx);
            show.set_visible(cx, !self.password_visible);
            hide.set_visible(cx, self.password_visible);
            password_input.set_key_focus(cx);
        }
        if server_input.changed(actions).is_some() {
            self.reset_server(cx);
        }
        if !self.login_pending && self.view.button(cx, ids!(edit_server_button)).clicked(actions) {
            self.reset_server(cx);
            server_input.set_key_focus(cx);
        }
        if !self.login_pending && !self.discovery_pending && (
            self.view.button(cx, ids!(continue_server_button)).clicked(actions)
            || server_input.returned(actions).is_some()
        ) {
            self.check_server(cx, server.clone());
        }
        if !self.login_pending && self.password_form_open && self.methods.as_ref().is_some_and(|m| m.password) && (
            self.view.button(cx, ids!(login_button)).clicked(actions)
            || user_input.returned(actions).is_some()
            || password_input.returned(actions).is_some()
        ) {
            let user = user_input.text().trim().to_owned();
            let destination = self.methods.as_ref().map(|m| m.homeserver.clone()).ok_or_else(|| anyhow::anyhow!(crate::i18n::tr("Choose a server first."))).and_then(|server| {
                password_identifier(&user)?;
                if password_input.text().is_empty() { anyhow::bail!(crate::i18n::tr("Enter your password.")); }
                Ok(server)
            });
            match destination {
                Ok(destination) => {
                    self.login_pending = true;
                    self.show_status(cx, crate::i18n::tr("Signing in"), &crate::i18n::format("Connecting to {destination}…", &[("destination", (destination).to_string())]), crate::i18n::tr("Please wait…"), false);
                    if self.methods.as_ref().is_some_and(|methods| methods.oauth) {
                        // A plain Matrix password token cannot manage Agents; go through Pasion OAuth.
                        let username = user.trim_start_matches('@').split(':').next().unwrap_or_default().to_owned();
                        submit_async_request(MatrixRequest::PasswordSignIn { homeserver_url: destination, username, password: password_input.text(), token: String::new() });
                    } else {
                        submit_async_request(MatrixRequest::Login(LoginRequest::LoginByPassword(LoginByPassword {
                            user_id: user.clone(), password: password_input.text(), homeserver: Some(destination),
                        })));
                    }
                }
                Err(error) => self.show_status(cx, crate::i18n::tr("Check sign-in details"), &error.to_string(), crate::i18n::tr("Okay"), true),
            }
        }
        if !self.registration_form_open && self.view.button(cx, ids!(password_option_button)).clicked(actions) {
            self.password_form_open = !self.password_form_open;
            self.view.view(cx, ids!(password_form)).set_visible(cx, self.password_form_open);
            self.refresh_method_copy(cx);
        }
        if !self.login_pending && self.view.button(cx, ids!(register_option_button)).clicked(actions) {
            // Pasion servers: one in-app screen (username + password), no browser.
            if self.methods.as_ref().is_some_and(|methods| methods.password || methods.oauth) {
                self.registration_form_open = true;
                self.view.view(cx, ids!(provider_list_container)).set_visible(cx, false);
                self.view.button(cx, ids!(browser_login_button)).set_visible(cx, false);
                self.view.button(cx, ids!(password_option_button)).set_visible(cx, false);
                self.view.view(cx, ids!(password_form)).set_visible(cx, false);
                self.view.button(cx, ids!(register_option_button)).set_visible(cx, false);
                self.view.view(cx, ids!(registration_form)).set_visible(cx, true);
                self.refresh_method_copy(cx);
            }
        }
        if !self.login_pending && self.view.button(cx, ids!(back_to_login_button)).clicked(actions) {
            if let Some(methods) = self.methods.clone() {
                self.registration_form_open = false;
                self.show_methods(cx, methods);
            }
        }
        if !self.login_pending && self.registration_form_open && self.view.button(cx, ids!(submit_registration_button)).clicked(actions) {
            let username = self.view.text_input(cx, ids!(registration_username)).text().trim().to_owned();
            let password = self.view.text_input(cx, ids!(registration_password)).text();
            let token = self.view.text_input(cx, ids!(registration_token)).text().trim().to_owned();
            if self.submit_email_registration(cx, &username, &password) {
                // Handled by the email registration flow.
            } else if username.is_empty() || password.is_empty() {
                self.show_status(cx, crate::i18n::tr("Check registration details"), crate::i18n::tr("Enter a username and password."), crate::i18n::tr("Okay"), true);
            } else if let Some(homeserver_url) = self.methods.as_ref().map(|methods| methods.homeserver.clone()) {
                self.login_pending = true;
                self.show_status(cx, crate::i18n::tr("Creating account"), crate::i18n::tr("Contacting your server…"), crate::i18n::tr("Please wait…"), false);
                submit_async_request(MatrixRequest::RegisterAccount { homeserver_url, username, password, token });
            }
        }
        if !self.login_pending && self.view.button(cx, ids!(browser_login_button)).clicked(actions) {
            self.start_sso(cx, None, false);
        }
        if !self.login_pending {
            let selected_provider = self.view.portal_list(cx, ids!(provider_list))
                .items_with_actions(actions)
                .into_iter()
                .find_map(|(index, row)| {
                    if row.button(cx, ids!(provider_button)).clicked(actions) {
                        self.methods.as_ref().and_then(|m| m.providers.get(index)).map(|p| p.id.clone())
                    } else { None }
                });
            if let Some(provider_id) = selected_provider {
                self.start_sso(cx, Some(provider_id), false);
            }
        }

        self.handle_account_options(cx, actions);
        for action in actions {
            if let LoginStatusModalAction::Close = action.as_widget_action().cast() {
                if self.sso_pending {
                    submit_async_request(MatrixRequest::CancelSsoLogin);
                    self.show_status(cx, crate::i18n::tr("Cancelling sign-in"), crate::i18n::tr("Closing the sign-in connection…"), crate::i18n::tr("Please wait…"), false);
                } else if !self.login_pending {
                    modal.close(cx);
                }
            }
            match action.downcast_ref() {
                Some(LoginAction::ServerDiscovered { generation, result }) if *generation == self.discovery_generation => {
                    self.discovery_pending = false;
                    match result {
                        Ok(methods) => self.show_methods(cx, methods.clone()),
                        Err(error) => self.view.label(cx, ids!(server_status)).set_text(cx, &crate::i18n::format("Could not check this server: {error}", &[("error", error.clone())])),
                    }
                }
                Some(LoginAction::CliAutoLogin { user_id, homeserver }) => {
                    self.login_pending = true;
                    user_input.set_text(cx, user_id);
                    password_input.set_text(cx, "");
                    server_input.set_text(cx, homeserver.as_deref().unwrap_or_default());
                    self.recent_server_selected.clear();
                    self.recent_servers_dirty = true;
                    self.reset_server(cx);
                    self.show_status(cx, crate::i18n::tr("Signing in"), crate::i18n::tr("Connecting to your account…"), crate::i18n::tr("Please wait…"), false);
                }
                Some(LoginAction::Status { title, status }) => {
                    self.login_pending = true;
                    self.show_status(cx, title, status, if self.sso_pending { crate::i18n::tr("Cancel") } else { crate::i18n::tr("Please wait…") }, self.sso_pending);
                }
                Some(LoginAction::AccountPickerReady) => {
                    self.login_pending = false;
                    self.sso_pending = false;
                    self.reset_server(cx);
                    self.view.text_input(cx, ids!(user_id_input)).set_text(cx, "");
                    modal.close(cx);
                    if let Some(origin)=crate::sliding_sync::reauthentication_origin() {
                        self.recent_server_selected.clear();
                        self.recent_servers_dirty = true;
                        self.view.text_input(cx,ids!(homeserver_input)).set_text(cx,&origin);
                        self.check_server(cx,origin);
                    }
                }
                Some(LoginAction::LoginSuccess) => {
                    self.login_pending = false;
                    self.sso_pending = false;
                    user_input.set_text(cx, "");
                    password_input.set_text(cx, "");
                    server_input.set_text(cx, "");
                    self.reset_server(cx);
                    modal.close(cx);
                }
                Some(LoginAction::LoginFailure(error)) => {
                    self.login_pending = false;
                    self.sso_pending = false;
                    self.show_status(cx, crate::i18n::tr(crate::login::failure_text::login_failure_title(self.registration_form_open)), error, crate::i18n::tr("Okay"), true);
                }
                Some(LoginAction::SsoPending(pending)) => self.sso_pending = *pending,
                Some(LoginAction::Cancelled) => {
                    self.login_pending = false;
                    self.sso_pending = false;
                    modal.close(cx);
                }
                _ => {}
            }
        }
        self.view.button(cx, ids!(login_button)).set_enabled(cx, !self.login_pending && !crate::sliding_sync::account_changing());
        self.view.button(cx, ids!(browser_login_button)).set_enabled(cx, !self.login_pending);
        self.view.button(cx, ids!(edit_server_button)).set_enabled(cx, !self.login_pending);
        self.view.button(cx, ids!(continue_server_button)).set_enabled(cx, !self.login_pending && !self.discovery_pending);
        self.view.button(cx, ids!(register_option_button)).set_enabled(cx, !self.login_pending);
        self.view.button(cx, ids!(submit_registration_button)).set_enabled(cx, !self.login_pending);
        self.redraw(cx);
    }
}

/// Actions sent to or from the login screen. Never include a password or token.
#[derive(Clone, Default, Debug)]
pub enum LoginAction {
    LoginSuccess,
    AccountPickerReady,
    LoginFailure(String),
    Status { title: String, status: String },
    CliAutoLogin { user_id: String, homeserver: Option<String> },
    SsoPending(bool),
    Cancelled,
    ServerDiscovered { generation: u64, result: Result<LoginMethods, String> },
    #[default]
    None,
}

/// The visible browser sign-in option and its activation use the same protocols.
fn browser_login_allowed(sso: bool, oauth: bool, browser_registration: bool, register: bool) -> bool {
    (sso || oauth) && (!register || browser_registration)
}

#[cfg(test)]
mod browser_login_tests {
    use super::browser_login_allowed;

    #[test]
    fn oauth_registration_button_follows_server_capability() {
        use super::*;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut screen = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.LoginScreen {}});
            let screen = LoginScreen::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty());
            screen
        });
        for browser_registration in [false, true, false] {
            screen.show_methods(&mut cx, LoginMethods {
                homeserver: "https://hagency.meldry.com/".to_owned(),
                password: false, sso: false, oauth: true,
                oauth_aware_preferred: true, browser_registration,
                providers: Vec::new(),
            });
            assert_eq!(screen.view.button(&mut cx, ids!(register_option_button)).visible(), browser_registration);
            assert!(screen.view.button(&mut cx, ids!(browser_login_button)).visible());
            assert!(!screen.view.view(&mut cx, ids!(registration_form)).visible());
            assert!(!screen.view.view(&mut cx, ids!(password_form)).visible());
        }
    }

    #[test]
    fn recent_servers_survive_reapply_and_reopen_without_changing_address() {
        use super::*;
        let mut cx=Cx::new(Box::new(|_,_| {}));
        let (mut screen,value)=cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);
            vm.bx.captured_errors=Some(Vec::new());
            let value=script_eval!(vm,{mod.widgets.LoginScreen {}});
            let screen=LoginScreen::script_from_value(vm,value);
            assert!(vm.take_errors().is_empty());(screen,value)
        });
        let servers=vec!["https://hagency.local".to_owned()];
        screen.sync_recent_servers(&mut cx,servers.clone());
        screen.recent_server_selected=servers[0].clone();
        screen.view.text_input(&mut cx,ids!(homeserver_input)).set_text(&mut cx,"https://hagency.local");
        for apply in [Apply::Rebake,Apply::ScriptReapply,Apply::Rebake] {
            cx.with_vm(|vm|screen.script_apply(vm,&apply,&mut Scope::empty(),value));
            assert!(screen.recent_servers_dirty);
            let prompt=crate::i18n::text_for(crate::i18n::Language::Chinese,"Recent servers").to_owned();
            screen.sync_recent_servers_with_prompt(&mut cx,servers.clone(),prompt.clone());
            let dropdown=screen.view.drop_down(&mut cx,ids!(recent_servers));
            assert_eq!(dropdown.selected_label(),"https://hagency.local");
            assert!(!screen.recent_servers_dirty);
            assert!(!screen.view.view(&mut cx, ids!(manual_server_row)).visible());
            dropdown.set_selected_item(&mut cx,0);assert_eq!(dropdown.selected_label(),prompt);
            dropdown.set_selected_item(&mut cx,1);
            screen.view.set_visible(&mut cx,false);screen.view.set_visible(&mut cx,true);
            screen.sync_recent_servers(&mut cx,servers.clone());
            assert_eq!(screen.view.text_input(&mut cx,ids!(homeserver_input)).text(),"https://hagency.local");
            {let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(980.0,760.0));let mut list=DrawList2d::new(&mut cx);let overlay=Overlay{draw_list:DrawList::new(&mut cx)};let e=DrawEvent::default();let mut draw=CxDraw::new(&mut cx,&e);let mut cx2d=Cx2d::new(&mut draw);cx2d.begin_pass(&pass,None);list.begin_always(&mut cx2d);overlay.begin(&mut cx2d);cx2d.begin_root_turtle(dvec2(980.0,760.0),Layout::flow_overlay());screen.view.draw_walk_all(&mut cx2d,&mut Scope::empty(),Walk::fixed(980.0,760.0));overlay.end(&mut cx2d);cx2d.end_pass_sized_turtle();list.end(&mut cx2d);cx2d.end_pass(&pass);}
            let dropdown_rect = screen.view.drop_down(&mut cx,ids!(recent_servers)).area().rect(&cx);
            assert_eq!(dropdown_rect.size, dvec2(292.0, 38.0));
        }
        let dropdown = screen.view.drop_down(&mut cx, ids!(recent_servers));
        let actions = cx.capture_actions(|cx| {
            cx.widget_action(dropdown.widget_uid(), DropDownAction::Select(0));
        });
        screen.handle_actions(&mut cx, &actions);
        screen.sync_recent_servers(&mut cx, servers.clone());
        assert!(screen.view.view(&mut cx, ids!(manual_server_row)).visible());
        assert!(screen.recent_server_selected.is_empty());
        assert!(screen.view.text_input(&mut cx, ids!(homeserver_input)).text().is_empty());
        let actions = cx.capture_actions(|cx| {
            cx.widget_action(dropdown.widget_uid(), DropDownAction::Select(1));
        });
        screen.handle_actions(&mut cx, &actions);
        screen.sync_recent_servers(&mut cx, servers);
        assert!(!screen.view.view(&mut cx, ids!(manual_server_row)).visible());
        assert_eq!(screen.recent_server_selected, "https://hagency.local");
    }

    #[test]
    fn oauth_only_browser_sign_in_can_start() {
        assert!(browser_login_allowed(false, true, false, false));
        assert!(browser_login_allowed(true, false, false, false));
        assert!(!browser_login_allowed(false, false, false, false));
    }

    #[test]
    fn browser_registration_requires_both_a_protocol_and_registration_capability() {
        for (sso, oauth) in [(true, false), (false, true), (true, true)] {
            assert!(!browser_login_allowed(sso, oauth, false, true));
            assert!(browser_login_allowed(sso, oauth, true, true));
        }
        assert!(!browser_login_allowed(false, false, true, true));
    }
}
