//! The timeline card for a question an Agent asks during a run.
//!
//! One card sits (hidden by default) inside every `Message` widget's content
//! column, next to the approval card. `populate_message_view` shows it under
//! the message that started the run, from [`crate::hagency::pending_questions`].
//! Clicks and typed text go straight to that store; an answer ready to send
//! leaves as a [`QuestionAnswerRequested`] action, which the app sends.
//! The card holds no protocol state of its own.

use makepad_widgets::*;

use crate::hagency::pending_questions::{self, PendingQuestionsChanged, QuestionRow, QuestionView};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.AgentQuestionOptionButton = mod.widgets.AgentApprovalSecondaryButton {}

    mod.widgets.AgentQuestionBlock = #(AgentQuestionBlock::register_widget(vm)) {
        visible: false
        width: Fill, height: Fit
        flow: Down
        spacing: 6.0

        header_label := Label {
            width: Fill, height: Fit
            padding: 0
            draw_text +: {
                text_style: (mod.widgets.RBX_TEXT_META)
                color: (mod.widgets.RBX_INFO_FG)
            }
            text: ""
        }
        question_label := Label {
            width: Fill, height: Fit
            flow: Flow.Right{wrap: true}
            padding: 0
            draw_text +: {
                text_style: (mod.widgets.RBX_TEXT_BODY)
                color: (mod.widgets.RBX_FG_PRIMARY)
            }
            text: ""
        }
        options := View {
            width: Fill, height: Fit
            flow: Flow.Right{wrap: true}
            spacing: 8.0
            o0 := mod.widgets.AgentQuestionOptionButton {}
            o1 := mod.widgets.AgentQuestionOptionButton {}
            o2 := mod.widgets.AgentQuestionOptionButton {}
            o3 := mod.widgets.AgentQuestionOptionButton {}
            o4 := mod.widgets.AgentQuestionOptionButton {}
            o5 := mod.widgets.AgentQuestionOptionButton {}
            o6 := mod.widgets.AgentQuestionOptionButton {}
            o7 := mod.widgets.AgentQuestionOptionButton {}
            o8 := mod.widgets.AgentQuestionOptionButton {}
            o9 := mod.widgets.AgentQuestionOptionButton {}
            o10 := mod.widgets.AgentQuestionOptionButton {}
            o11 := mod.widgets.AgentQuestionOptionButton {}
            o12 := mod.widgets.AgentQuestionOptionButton {}
            o13 := mod.widgets.AgentQuestionOptionButton {}
            o14 := mod.widgets.AgentQuestionOptionButton {}
            o15 := mod.widgets.AgentQuestionOptionButton {}
        }
        details_label := Label {
            width: Fill, height: Fit
            flow: Flow.Right{wrap: true}
            padding: 0
            draw_text +: {
                text_style: (mod.widgets.RBX_TEXT_META)
                color: (mod.widgets.RBX_FG_SECONDARY)
            }
            text: ""
        }
        // TextInput has no `visible` of its own; this box shows or hides it.
        other_box := View {
            visible: false
            width: Fill, height: Fit
            other_input := TextInput {
                width: Fill, height: Fit
                empty_text: #(crate::i18n::tr("Other: type your own answer"))
                i18n_empty_text: "Other: type your own answer"
            }
        }
        answer_label := Label {
            visible: false
            width: Fill, height: Fit
            flow: Flow.Right{wrap: true}
            padding: 0
            draw_text +: {
                text_style: (mod.widgets.RBX_TEXT_BODY_STRONG)
                color: (mod.widgets.RBX_FG_PRIMARY)
            }
            text: ""
        }
    }

    mod.widgets.AgentQuestionCard = #(AgentQuestionCard::register_widget(vm)) {
        visible: false
        width: Fill, height: Fit
        flow: Down
        margin: Inset{top: 8.0, right: 10.0}

        // The plain View background draws no border; a rounded frame does.
        frame := RoundedView {
            width: Fill, height: Fit
            flow: Down
            spacing: 10.0
            padding: Inset{left: 16.0, right: 16.0, top: 12.0, bottom: 14.0}
            draw_bg +: {
                color: (mod.widgets.RBX_INFO_BG)
                border_radius: (mod.widgets.RBX_RADIUS_SM)
                border_size: 1.0
                border_color: (mod.widgets.RBX_INFO_FG)
            }

            header := View {
                width: Fill, height: Fit
                flow: Right
                spacing: 8.0
                align: Align{y: 0.5}

                title_label := Label {
                    width: Fill, height: Fit
                    flow: Flow.Right{wrap: true}
                    padding: 0
                    draw_text +: {
                        text_style: (mod.widgets.RBX_TEXT_CARD_TITLE)
                        color: (mod.widgets.RBX_INFO_FG)
                    }
                    text: ""
                }

                status_badge := RoundedView {
                    width: Fit, height: Fit
                    padding: Inset{left: 8.0, right: 8.0, top: 4.0, bottom: 4.0}
                    show_bg: true
                    draw_bg +: {
                        color: (mod.widgets.RBX_BG_SURFACE)
                        border_radius: (mod.widgets.RBX_RADIUS_PILL)
                    }
                    status_label := Label {
                        width: Fit, height: Fit
                        padding: 0
                        draw_text +: {
                            text_style: (mod.widgets.RBX_TEXT_BADGE)
                            color: (mod.widgets.RBX_INFO_FG)
                        }
                        text: ""
                    }
                }
            }

            questions := FlatList {
                width: Fill, height: Fit
                flow: Down
                spacing: 10.0
                scroll_bars.show_scroll_y: false
                Question := mod.widgets.AgentQuestionBlock {}
            }

            footer := View {
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                spacing: 8.0
                align: Align{y: 0.5}
                send_button := mod.widgets.AgentApprovalPrimaryButton {
                    text: #(crate::i18n::tr("Send answer"))
                    i18n_text: "Send answer"
                }
            }

            hint_label := Label {
                width: Fill, height: Fit
                flow: Flow.Right{wrap: true}
                padding: 0
                draw_text +: {
                    text_style: (mod.widgets.RBX_TEXT_META)
                    color: (mod.widgets.RBX_FG_SECONDARY)
                }
                text: ""
            }
        }
    }
}

const OPTIONS: [LiveId; ::hagency::agent_questions::MAX_CARD_OPTIONS] = [
    id!(o0), id!(o1), id!(o2), id!(o3), id!(o4), id!(o5), id!(o6), id!(o7),
    id!(o8), id!(o9), id!(o10), id!(o11), id!(o12), id!(o13), id!(o14), id!(o15),
];

/// Blocks are instantiated only for a visible card, not under every chat
/// message. A recycled block keeps its text input and keyboard focus.
#[derive(Script, ScriptHook, Widget)]
pub struct AgentQuestionBlock {
    #[deref] view: View,
    #[rust] data: Option<(String, QuestionRow, bool)>,
}
#[derive(Clone, Debug)]
struct QuestionDraftChanged(String);

impl Widget for AgentQuestionBlock {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return; };
        let Some((key,row,_)) = &self.data else { return; };
        let mut changed=false;
        for (index,(label,_)) in row.options.iter().enumerate() {
            if self.view.button(cx,&[id!(options),OPTIONS[index]]).clicked(actions) {
                if let Some(request)=pending_questions::choose(key,&row.id,label) {cx.action(request);}
                changed=true;
            }
        }
        let input=self.view.text_input(cx,ids!(other_box.other_input));
        if let Some(text)=input.changed(actions) {
            pending_questions::type_text(key,&row.id,&text);
            cx.action(QuestionDraftChanged(key.clone()));
        }
        if input.returned(actions).is_some() && let Some(request)=pending_questions::submit(key) {
            cx.action(request); changed=true;
        }
        if changed {cx.action(PendingQuestionsChanged);}
    }
    fn draw_walk(&mut self,cx:&mut Cx2d,scope:&mut Scope,walk:Walk)->DrawStep {
        self.view.draw_walk(cx,scope,walk)
    }
}
impl AgentQuestionBlock {
    fn set_row(&mut self,cx:&mut Cx,key:&str,row:&QuestionRow,open:bool) {
        if self.data.as_ref().is_some_and(|(old_key,old_row,old_open)|old_key==key && old_row==row && *old_open==open) {return;}
        let rebinding=self.data.as_ref().is_none_or(|(old,_,_)|old!=key);
        self.view.set_visible(cx,true);
        let header=self.view.label(cx,ids!(header_label));
        header.set_visible(cx,!row.header.is_empty()); header.set_text(cx,&row.header);
        self.view.label(cx,ids!(question_label)).set_text(cx,&row.question);
        for (index,id) in OPTIONS.iter().enumerate() {
            let button=self.view.button(cx,&[id!(options),*id]);
            let Some((label,chosen))=row.options.get(index) else {button.set_visible(cx,false);continue;};
            button.set_visible(cx,row.answer.is_none());
            button.set_text(cx,&if *chosen {format!("✓ {label}")}else{label.clone()});
            show_enabled(cx,&button,open);
            if open {button.reset_hover(cx);}
        }
        self.view.view(cx,ids!(options)).set_visible(cx,!row.options.is_empty() && row.answer.is_none());
        let details=self.view.label(cx,ids!(details_label));
        details.set_visible(cx,open && !row.details.is_empty());details.set_text(cx,&row.details);
        self.view.view(cx,ids!(other_box)).set_visible(cx,row.text.is_some());
        let input=self.view.text_input(cx,ids!(other_box.other_input));
        if let Some(draft)=&row.text && (rebinding || input.text()!=*draft) {input.set_text(cx,draft);}
        let answer=self.view.label(cx,ids!(answer_label));
        answer.set_visible(cx,row.answer.is_some());
        if let Some(text)=&row.answer {answer.set_text(cx,&format!("→ {text}"));}
        self.data=Some((key.into(),row.clone(),open));
    }
}

/// A card for one native question request and its final Send action.
#[derive(Script, ScriptHook, Widget)]
pub struct AgentQuestionCard {
    #[deref] view: View,
    #[rust] card_id: Option<String>,
    #[rust] content: Option<QuestionView>,
}

impl Widget for AgentQuestionCard {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else {
            return;
        };
        let Some(card_id) = self.card_id.clone() else {
            return;
        };
        let mut changed = false;
        for action in actions {
            if let Some(QuestionDraftChanged(key))=action.downcast_ref() && key==&card_id {
                self.refresh_send(cx,&card_id);
            }
        }
        if self
            .view
            .button(cx, ids!(footer.send_button))
            .clicked(actions)
        {
            if let Some(request) = pending_questions::submit(&card_id) {
                cx.action(request);
            }
            changed = true;
        }
        if changed {
            cx.action(PendingQuestionsChanged);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(item)=self.view.draw_walk(cx,scope,walk).step() {
            let list=item.as_flat_list();
            let Some(mut list)=list.borrow_mut() else {continue;};
            if let Some(content)=&self.content {
                for (index,row) in content.questions.iter().enumerate() {
                    if let Some(item)=list.item(cx,LiveId::from_str(&format!("q{index}")),id!(Question)) {
                        if let Some(mut block)=item.borrow_mut::<AgentQuestionBlock>() {
                            block.set_row(cx,&content.key,row,content.open);
                        }
                        item.draw_all(cx,scope);
                    }
                }
            }
            list.items.retain_visible();
        }
        DrawStep::done()
    }

}

impl AgentQuestionCard {
    /// Shows the card for `view`, or hides it entirely when `view` is `None`.
    pub fn set_view(&mut self, cx: &mut Cx, view: Option<&QuestionView>) {
        let Some(v) = view else {
            self.card_id = None;
            self.content = None;
            self.view.set_visible(cx, false);
            return;
        };
        // A recycled timeline item may have shown another card's draft.
        self.card_id = Some(v.key.clone());
        self.view.set_visible(cx, true);
        self.view
            .label(cx, ids!(header.title_label))
            .set_text(cx, &v.title);
        self.view
            .label(cx, ids!(header.status_badge.status_label))
            .set_text(cx, &v.status);
        let send = self.view.button(cx, ids!(footer.send_button));
        send.set_visible(cx, v.send_visible);
        show_enabled(cx, &send, v.can_send);
        self.view
            .view(cx, ids!(footer))
            .set_visible(cx, v.send_visible);
        let hint = self.view.label(cx, ids!(hint_label));
        hint.set_visible(cx, !v.hint.is_empty());
        hint.set_text(cx, &v.hint);
        self.content = Some(v.clone());
        self.view.redraw(cx);
    }

    fn refresh_send(&mut self, cx: &mut Cx, card_id: &str) {
        let ready = pending_questions::get(card_id).is_some_and(|c| c.ready());
        show_enabled(cx, &self.view.button(cx, ids!(footer.send_button)), ready);
    }
}

/// `set_enabled` alone only stops clicks; the face and label also dim so a
/// button that cannot act yet looks it.
fn show_enabled(cx: &mut Cx, button: &ButtonRef, enabled: bool) {
    button.set_enabled(cx, enabled);
    let dim = if enabled { 0.0 } else { 1.0 };
    let mut button = button.clone();
    script_apply_eval!(cx, button, {
        draw_bg +: {disabled: #(dim)}
        draw_text +: {disabled: #(dim)}
    });
}

impl AgentQuestionCardRef {
    /// See [`AgentQuestionCard::set_view`].
    pub fn set_view(&self, cx: &mut Cx, view: Option<&QuestionView>) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_view(cx, view);
        }
    }
}
