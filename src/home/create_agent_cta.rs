//! First-run guidance: an account with no rooms and no Agent sees one action,
//! "Create your agent", instead of the generic chat empty state. Buzz shows the
//! agent first; so does Hagency now.
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.CreateAgentCta = #(CreateAgentCta::register_widget(vm)) {
        width: Fill height: Fit flow: Down spacing: 12
        headline := Label {
            width: Fill height: Fit
            text: #(crate::i18n::tr("Start with your agent")) i18n_text: "Start with your agent"
            draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_bold {font_size: (18.0 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        hint := Label {
            width: Fill height: Fit flow: Flow.Right{wrap: true}
            text: #(crate::i18n::tr("One button creates an agent that runs on this Mac's Claude Code login and opens its private chat. Rooms and invitations come after."))
            i18n_text: "One button creates an agent that runs on this Mac's Claude Code login and opens its private chat. Rooms and invitations come after."
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: (14.0 * mod.widgets.RINX_TEXT_SCALE)}}
        }
        create := Button {text: #(crate::i18n::tr("Create your agent")) i18n_text: "Create your agent"}
    }
}

/// The call-to-action shows until the account is known to have an Agent: a
/// brand-new account never opens the Agent panel, so "unknown" counts as
/// first run while the welcome screen's own listing settles it.
pub fn show_cta(has_agent: Option<bool>) -> bool {
    has_agent != Some(true)
}

#[derive(Script, ScriptHook, Widget)]
pub struct CreateAgentCta {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
}
impl Widget for CreateAgentCta {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event
            && self.view.button(cx, ids!(create)).clicked(actions)
        {
            cx.action(crate::hagency::create_agent::CreateAgentAction::Open);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // Shown or hidden by the welcome screen: a view hidden by itself is never
        // drawn again, so it could not show itself once an account is first run.
        self.view.draw_walk(cx, scope, walk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cta_until_an_agent_is_known() {
        // Never-opened panel: nothing reported yet, still first run.
        assert!(show_cta(None));
        assert!(show_cta(Some(false)));
        assert!(!show_cta(Some(true)));
    }

    /// The welcome screen (where a brand-new account lands) registers with the
    /// call-to-action inside it, and its button opens the Create agent dialog.
    #[test]
    fn welcome_screen_carries_the_cta_and_its_button_opens_create_agent() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let welcome = cx.with_vm(|vm| {
            crate::theme::tests::install(vm, Default::default());
            crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.WelcomeScreen {}});
            let welcome = WidgetRef::script_from_value(vm, value);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "Welcome registration: {errors:?}");
            welcome
        });
        let create = welcome.button(&mut cx, ids!(create_agent.create));
        assert!(!create.is_empty(), "the CTA's button is inside the welcome screen");
    }
}
