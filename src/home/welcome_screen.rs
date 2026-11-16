use makepad_widgets::*;
use crate::home::{create_agent_cta::{AgentEntry, agent_entry}, navigation_tab_bar::NavigationBarAction};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.WelcomeScreen = #(WelcomeScreen::register_widget(vm)) {
        width: Fill height: Fill flow: Down
        align: Align{x: 0.0 y: 0.5}
        padding: 40
        spacing: 20
        show_bg: true
        draw_bg.color: (COLOR_PRIMARY)
        title := Label {
            width: Fill height: Fit
            text: #(crate::i18n::tr("Welcome to Hagency")) i18n_text: "Welcome to Hagency"
            draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold {font_size: (22.0 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        description := Label {
            width: Fill height: Fit flow: Flow.Right{wrap: true}
            text: #(crate::i18n::tr("Hagency runs coding Agents on your computer and lets you talk to them from chat. Invite your Agent to a room to work on it together, start a chat with a person, or explore rooms on your server."))
            i18n_text: "Hagency runs coding Agents on your computer and lets you talk to them from chat. Invite your Agent to a room to work on it together, start a chat with a person, or explore rooms on your server."
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: (14.0 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        create_agent := mod.widgets.CreateAgentCta {visible: false}
        View {width: Fill height: Fit flow: Right spacing: 12
            open_agent := Button {visible: false text: #(crate::i18n::tr("Your agents")) i18n_text: "Your agents"}
            new_chat := Button {text: #(crate::i18n::tr("New chat")) i18n_text: "New chat"}
            explore := Button {text: #(crate::i18n::tr("Explore rooms")) i18n_text: "Explore rooms"}
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct WelcomeScreen {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
}
impl WelcomeScreen {
    /// An account with no Agent gets "Create your agent", one with Agents "Your agents", and
    /// neither while the listing is still loading.
    fn show_agent_entry(&mut self, cx: &mut Cx, entry: AgentEntry) {
        self.view.widget(cx, ids!(create_agent)).set_visible(cx, entry == AgentEntry::Create);
        self.view.button(cx, ids!(open_agent)).set_visible(cx, entry == AgentEntry::Open);
    }
}
impl Widget for WelcomeScreen {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // The presence listing settles off the UI thread and signals; redraw to show its answer.
        if let Event::Signal = event { self.view.redraw(cx); }
        if let Event::Actions(actions) = event {
            if self.view.button(cx, ids!(open_agent)).clicked(actions) {
                cx.action(crate::miniapps::MiniAppsAction::OpenAgent);
            }
            if self.view.button(cx, ids!(new_chat)).clicked(actions) {
                cx.action(NavigationBarAction::NewChat);
            }
            if self.view.button(cx, ids!(explore)).clicked(actions) {
                cx.action(NavigationBarAction::GoToAddRoom);
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // Learn whether this account has an Agent: "Create your agent", "Your agents", or neither while loading.
        crate::hagency::agent_presence::refresh();
        self.show_agent_entry(cx, agent_entry(crate::hagency::agent_presence::has_agent()));
        self.view.draw_walk(cx, scope, walk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_empty_state_has_real_new_chat_and_explore_actions() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut screen = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm); crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.WelcomeScreen {}});
            let screen = WelcomeScreen::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty()); screen
        });
        assert_eq!(screen.view.label(&mut cx, ids!(title)).text(), crate::i18n::tr("Welcome to Hagency"));
        let uid = screen.view.button(&mut cx, ids!(open_agent)).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(uid, ButtonAction::Clicked(KeyModifiers::default())));
        let emitted = cx.capture_actions(|cx| screen.handle_event(cx, &Event::Actions(actions), &mut Scope::empty()));
        assert!(emitted.iter().any(|a| matches!(a.downcast_ref::<crate::miniapps::MiniAppsAction>(), Some(crate::miniapps::MiniAppsAction::OpenAgent))));
        for (button, contacts) in [(id!(new_chat), true), (id!(explore), false)] {
            let uid = screen.view.button(&mut cx, &[button]).widget_uid();
            let actions = cx.capture_actions(|cx| cx.widget_action(uid, ButtonAction::Clicked(KeyModifiers::default())));
            let emitted = cx.capture_actions(|cx| screen.handle_event(cx, &Event::Actions(actions), &mut Scope::empty()));
            assert!(emitted.iter().any(|a| match a.downcast_ref::<NavigationBarAction>() {
                Some(NavigationBarAction::NewChat) => contacts,
                Some(NavigationBarAction::GoToAddRoom) => !contacts,
                _ => false,
            }));
        }
    }

    /// Regression: a new account saw "Open Agent" and no CTA, because the CTA
    /// hid itself and a hidden child is never drawn to show itself again.
    #[test]
    fn first_run_shows_create_your_agent_instead_of_open_agent() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let (mut screen, cta) = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm); crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.WelcomeScreen {}});
            let screen = WelcomeScreen::script_from_value(vm, value);
            let value = script_eval!(vm, {mod.widgets.CreateAgentCta {}});
            let cta = WidgetRef::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty()); (screen, cta)
        });
        assert!(cta.visible(), "the CTA must not hide itself; the welcome screen decides");
        for (has_agent, cta, open) in [(Some(false), true, false), (Some(true), false, true), (None, false, false), (Some(false), true, false)] {
            screen.show_agent_entry(&mut cx, agent_entry(has_agent));
            assert_eq!(screen.view.widget(&mut cx, ids!(create_agent)).visible(), cta, "{has_agent:?}");
            assert_eq!(screen.view.button(&mut cx, ids!(open_agent)).visible(), open, "{has_agent:?}");
        }
    }
}
