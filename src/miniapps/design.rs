//! Shared SVG handles for contained mini apps; colors come from Rinx themes.
use makepad_widgets::*;
use makepad_widgets::widget_async::ScriptAsyncResult;
use makepad_widgets::makepad_script::ScriptFnRef;
use std::collections::HashMap;

/// Stable UI scale for owner-entered amounts, not an authorization limit.
pub(crate) const DEFAULT_SLIDER_MAX: u64 = 1_000_000_000;

/// An exact integer editor with explicit steps. No artificial capacity ceiling:
/// the maximum only prevents losing integer precision at the script boundary.
#[derive(Script, Widget)]
pub struct MiniAppNumberInput {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[live] text: String,
    #[live] empty_text: String,
    #[live] is_read_only: bool,
    /// Display millions while the widget's text/actions stay exact token integers.
    #[live] millions: bool,
    /// Display ten-thousands (万), the unit Chinese readers expect for counts.
    #[live] wan: bool,
    #[live] slider_mode: bool,
    /// A nonnegative maximum is an observed capacity bound. A negative value
    /// leaves exact entry unrestricted and uses the fixed 1B slider scale.
    #[live(-1.0)] maximum: f64,
    #[live(100000.0)] step: f64,
    #[live(0.0)] minimum: f64,
    #[live] on_change: Option<ScriptFnRef>,
}
impl ScriptHook for MiniAppNumberInput {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, _: &Apply, _: &mut Scope, _: ScriptValue) {
        vm.with_cx_mut(|cx| {
            self.view.text_input(cx,ids!(input)).set_text(cx,&self.display_text());
            self.view.text_input(cx,ids!(input)).set_is_read_only(cx,self.is_read_only);
            self.view.text_input(cx,ids!(input)).set_empty_text(cx,self.empty_text.clone());
            self.view.label(cx,ids!(unit)).set_visible(cx,self.millions || self.wan);
            if self.wan {self.view.label(cx,ids!(unit)).set_text(cx,"万 token");}
            self.sync_steps(cx);
        });
    }
}
impl MiniAppNumberInput {
    const MAX: u64=9_007_199_254_740_991;
    fn display_text(&self) -> String {
        if !(self.millions || self.wan) || self.text.is_empty() { return self.text.clone(); }
        if let Some(invalid) = self.text.strip_prefix("invalid:") { return invalid.into(); }
        if self.wan {
            return self.text.parse::<u64>().map(token_wan).unwrap_or_else(|_| self.text.clone());
        }
        self.text.parse::<u64>().map(token_millions).unwrap_or_else(|_| self.text.clone())
    }
    fn number(&self)->Option<u64>{
        if self.text.is_empty(){return Some(0)}
        if !self.text.bytes().all(|v|v.is_ascii_digit()){return None}
        self.text.parse::<u64>().ok().filter(|v|*v<=Self::MAX)
    }
    fn sync_steps(&mut self,cx:&mut Cx){
        let value=self.number();
        self.view.widget(cx,ids!(decrease)).set_visible(cx,!self.slider_mode);
        self.view.widget(cx,ids!(increase)).set_visible(cx,!self.slider_mode);
        self.view.widget(cx,ids!(stepper_gap)).set_visible(cx,!self.slider_mode);
        self.view.widget(cx,ids!(slider_wrap)).set_visible(cx,self.slider_mode);
        let slider=self.view.widget(cx,ids!(slider_wrap.amount_slider));
        if let Some(mut slider)=slider.borrow_mut::<MiniAppAmountSlider>() {
            let (low,high)=self.slider_span();
            // One possible value leaves nothing to slide: the track stays, disabled.
            slider.enabled=!self.is_read_only && high>low;
            let disabled=!slider.enabled;
            slider.slider.set_disabled(cx,disabled);
            slider.value=slider_fraction(value.unwrap_or(0),low,high);
            let value=slider.value;
            slider.slider.set_value(cx,value);
            // Setting the value moves the knob only once the slider draws again.
            slider.slider.redraw(cx);
        }
        self.view.button(cx,ids!(decrease)).set_disabled(cx,self.is_read_only || value.is_none_or(|v|v<=self.minimum.max(0.0) as u64));
        self.view.button(cx,ids!(increase)).set_disabled(cx,self.is_read_only || value.is_none_or(|v|v==Self::MAX));
    }
    fn slider_max(&self)->u64 {
        if self.maximum.is_finite() && self.maximum>=0.0 {return self.maximum.min(Self::MAX as f64) as u64;}
        DEFAULT_SLIDER_MAX
    }
    /// The values the track spans: its left end is the minimum (such as what
    /// running Agents hold), its right end the maximum. Before this the track
    /// started at 0, so a knob dragged below the minimum stayed there while the
    /// box beside it showed the minimum.
    fn slider_span(&self)->(u64,u64) {
        let high=self.slider_max();
        let low=(self.minimum.max(0.0).min(Self::MAX as f64) as u64).min(high);
        (low,high)
    }
    pub(crate) fn set_maximum(&mut self,cx:&mut Cx,maximum:u64){
        self.maximum=maximum.min(Self::MAX) as f64;self.sync_steps(cx);
    }
    /// A lower bound the native form must not go below, such as what running
    /// Agents already hold. Typed values below it stay visible and invalid.
    pub(crate) fn set_minimum(&mut self,cx:&mut Cx,minimum:u64){
        self.minimum=minimum.min(Self::MAX) as f64;self.sync_steps(cx);
    }
    fn slide_to(&mut self,cx:&mut Cx,position:f64){
        let (low,high)=self.slider_span();
        if self.is_read_only || high==0 || !position.is_finite(){return}
        let value=slider_value(position,low,high);
        if self.number()!=Some(value) || self.text.is_empty() {
            self.text=value.to_string();
            self.view.text_input(cx,ids!(input)).set_text(cx,&self.display_text());
            self.notify_change(cx);
        }
        // The knob lands on the value the box shows, also when the drag left
        // that value as it was (at either end, or within one step).
        self.sync_steps(cx);
    }
    fn notify_change(&self,cx:&mut Cx){
        // Trusted confirmation screens capture their draft on Actions. Keep
        // them informed even when this instance has no Splash callback.
        cx.widget_action(self.widget_uid(),TextInputAction::Changed(self.text.clone()));
        let Some(handler)=self.on_change.clone() else{return};
        let Some(vm_id)=cx.script_ref_vm_id(&self.source) else{return};
        let value=cx.with_script_vm_id(vm_id,|vm| ScriptValue::from(vm.bx.heap.new_string_from_str(&self.text)));
        cx.widget_to_script_call(self.widget_uid(),NIL,self.source.clone(),handler,&[value]);
    }
    fn adjust(&mut self,cx:&mut Cx,up:bool){
        if self.is_read_only{return}
        let Some(value)=self.number() else{return};
        let step=if self.step.is_finite(){self.step.max(1.0).min(Self::MAX as f64) as u64}else{1};
        let minimum=self.minimum.max(0.0).min(Self::MAX as f64) as u64;
        let value=if up{value.saturating_add(step).min(Self::MAX)}else{value.saturating_sub(step).max(minimum)};
        self.text=value.to_string();
        self.view.text_input(cx,ids!(input)).set_text(cx,&self.display_text());
        self.sync_steps(cx);self.notify_change(cx);
    }
}
impl Widget for MiniAppNumberInput {
    fn handle_event(&mut self,cx:&mut Cx,event:&Event,scope:&mut Scope){
        let actions=cx.capture_actions(|cx|self.view.handle_event(cx,event,scope));
        if self.is_read_only{return}
        if self.slider_mode {
            let uid=self.view.widget(cx,ids!(slider_wrap.amount_slider)).widget_uid();
            if let SliderAction::Slide(value)=actions.find_widget_action(uid).cast(){self.slide_to(cx,value);}
        }
        if let Some(text)=self.view.text_input(cx,ids!(input)).changed(&actions){
            self.text=if self.wan && !text.is_empty() {
                tokens_from_wan(&text).map(|n| n.to_string()).unwrap_or_else(|| format!("invalid:{text}"))
            } else if self.millions && !text.is_empty() {
                tokens_from_millions(&text).map(|n| n.to_string()).unwrap_or_else(|| format!("invalid:{text}"))
            } else { text };
            self.sync_steps(cx);self.notify_change(cx);
        }
        let decrease=self.view.button(cx,ids!(decrease));let increase=self.view.button(cx,ids!(increase));
        let mut down=decrease.clicked(&actions);let mut up=increase.clicked(&actions);
        if let Event::KeyDown(key)=event {
            if !key.is_repeat && !key.modifiers.logo && !key.modifiers.control && !key.modifiers.alt
                && matches!(key.key_code,KeyCode::Space|KeyCode::ReturnKey|KeyCode::NumpadEnter){
                down|=cx.has_key_focus(decrease.area()) && !decrease.disabled(cx);
                up|=cx.has_key_focus(increase.area()) && !increase.disabled(cx);
            }
        }
        if down{self.adjust(cx,false)}else if up{self.adjust(cx,true)}
    }
    fn draw_walk(&mut self,cx:&mut Cx2d,scope:&mut Scope,walk:Walk)->DrawStep{self.view.draw_walk(cx,scope,walk)}
    fn text(&self)->String{self.text.clone()}
    fn set_text(&mut self,cx:&mut Cx,text:&str){
        // A callback may echo the unchanged token integer while the user is
        // typing "0.". Preserve that in-progress decimal and its caret.
        if self.text==text{return}
        self.text=text.into();self.view.text_input(cx,ids!(input)).set_text(cx,&self.display_text());self.sync_steps(cx)
    }
    fn script_call(&mut self,vm:&mut ScriptVm,method:LiveId,args:ScriptValue)->ScriptAsyncResult{
        if method==id!(set_text){
            if let Some(args)=args.as_object(){
                let value=vm.bx.heap.vec_value(args,0,NoTrap);
                if let Some(text)=vm.bx.heap.cast_to_owned_string(value,"number editor") {vm.with_cx_mut(|cx|self.set_text(cx,&text))}
            }
            return ScriptAsyncResult::Return(NIL)
        }
        self.view.script_call(vm,method,args)
    }
}

use crate::utils::token_millions;

/// Where `value` sits on a track from `low` to `high`, from 0 to 1. A value
/// outside the track (typed below the minimum) sits at its nearest end; a
/// track with one value keeps its knob at the right end.
fn slider_fraction(value: u64, low: u64, high: u64) -> f64 {
    if high <= low {
        return 1.0;
    }
    (value.clamp(low, high) - low) as f64 / (high - low) as f64
}
/// The value at `position` (0 to 1) on a track from `low` to `high`: steps of
/// 0.1 M once the track reaches 1 M, and exactly `low` and `high` at its ends.
fn slider_value(position: f64, low: u64, high: u64) -> u64 {
    if high <= low || position >= 1.0 {
        return high;
    }
    if position <= 0.0 {
        return low;
    }
    let step = if high >= 1_000_000 { 100_000 } else { 1 };
    let exact = low as f64 + position * (high - low) as f64;
    (((exact / step as f64).round() as u64) * step).clamp(low, high)
}

/// Whether a key button (`RinxKeyButton`, a `MiniAppButton`) was clicked.
/// `ButtonRef::clicked` downcasts to `Button`, which this wrapper isn't, so it
/// never fires for one: find the click by the button's widget uid instead, as
/// `MiniAppNumberInput` finds its slider's moves.
pub(crate) fn key_button_clicked(button: &WidgetRef, actions: &Actions) -> bool {
    matches!(actions.find_widget_action(button.widget_uid()).cast(), ButtonAction::Clicked(_))
}

/// 885626 → "88.56"; whole numbers stay whole.
fn token_wan(tokens: u64) -> String {
    let hundredths = tokens / 100;
    let (whole, part) = (hundredths / 100, hundredths % 100);
    match part {
        0 => whole.to_string(),
        p if p % 10 == 0 => format!("{whole}.{}", p / 10),
        p => format!("{whole}.{p:02}"),
    }
}
/// "88.5" 万 → 885000 tokens; at most two decimals, like the display.
fn tokens_from_wan(text: &str) -> Option<u64> {
    let text = text.trim();
    let (whole, part) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() || part.len() > 2 || !whole.bytes().chain(part.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let part = format!("{part:0<2}").parse::<u64>().ok()?;
    whole.parse::<u64>().ok()?.checked_mul(10_000)?.checked_add(part * 100)
}
fn tokens_from_millions(text: &str) -> Option<u64> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && fraction.is_empty() { return None; }
    if !whole.bytes().all(|c| c.is_ascii_digit()) || !fraction.bytes().all(|c| c.is_ascii_digit())
        || fraction.len() > 6 { return None; }
    let whole = if whole.is_empty() { 0 } else { whole.parse::<u64>().ok()? };
    let fraction = if fraction.is_empty() { 0 } else { fraction.parse::<u64>().ok()? * 10u64.pow(6 - fraction.len() as u32) };
    whole.checked_mul(1_000_000)?.checked_add(fraction).filter(|v| *v <= MiniAppNumberInput::MAX)
}

#[cfg(test)]
mod token_unit_tests {
    use super::*;
    #[test]
    fn wan_display_and_entry_round_trip() {
        assert_eq!(token_wan(2_000_000), "200");
        assert_eq!(token_wan(885_626), "88.56");
        assert_eq!(token_wan(885_000), "88.5");
        assert_eq!(tokens_from_wan("200"), Some(2_000_000));
        assert_eq!(tokens_from_wan("88.5"), Some(885_000));
        assert_eq!(tokens_from_wan("88.56"), Some(885_600));
        for invalid in ["", ".5", "1.234", "1e3", "-1", "万"] {
            assert_eq!(tokens_from_wan(invalid), None, "{invalid}");
        }
    }
    #[test]
    fn the_track_spans_minimum_to_maximum_so_knob_and_box_agree() {
        // The owner's pool: the maximum can't go below 9 M, the account allows 50 M.
        let (low, high) = (9_000_000, 50_000_000);
        // The left end is the minimum: no stretch of track below it to drag into.
        assert_eq!(slider_value(0.0, low, high), low);
        assert_eq!(slider_value(-0.3, low, high), low);
        assert_eq!(slider_value(1.0, low, high), high);
        assert_eq!(slider_fraction(low, low, high), 0.0);
        assert_eq!(slider_fraction(high, low, high), 1.0);
        // A value typed in the box puts the knob where dragging to it would.
        for typed in [9_000_000, 12_000_000, 28_800_000, 50_000_000] {
            let at = slider_fraction(typed, low, high);
            assert_eq!(slider_value(at, low, high), typed, "typed {typed}");
        }
        // Below the minimum (typed, still invalid) the knob waits at the left end.
        assert_eq!(slider_fraction(5_000_000, low, high), 0.0);
        // Steps of 0.1 M, never outside the track.
        assert_eq!(slider_value(0.5, low, high), 29_500_000);
        for position in [0.001, 0.3, 0.999] {
            assert!((8_650_000..=8_700_000).contains(&slider_value(position, 8_650_000, 8_700_000)), "{position}");
        }
        // One possible value: the knob stays put at the right end.
        assert_eq!(slider_value(0.2, 9_000_000, 9_000_000), 9_000_000);
        assert_eq!(slider_fraction(5_000_000, 9_000_000, 9_000_000), 1.0);
        // Small amounts keep single-token steps.
        assert_eq!(slider_value(0.5, 1, 101), 51);
    }
    #[test]
    fn million_editor_preserves_each_token_and_safe_integer_boundary() {
        for tokens in [0, 1, 10, 100_000, 123_456, 750_000, 1_000_000, 1_000_001, MiniAppNumberInput::MAX] {
            assert_eq!(tokens_from_millions(&token_millions(tokens)), Some(tokens));
        }
        assert_eq!(token_millions(750_000), "0.75");
        assert_eq!(tokens_from_millions(".1"), Some(100_000));
        assert_eq!(tokens_from_millions("1."), Some(1_000_000));
        for invalid in ["", ".", "NaN", "1e3", "-1", "1.0000001", "9007199254.740992", "1.2.3"] {
            assert_eq!(tokens_from_millions(invalid), None, "{invalid}");
        }
    }
}

/// The pinned Slider has no script change callback. Keep the adapter local and
/// queue callbacks in the owning Splash isolate, just like TextInput does.
#[derive(Script, Widget)]
pub struct MiniAppAmountSlider {
    #[source] source: ScriptObjectRef,
    #[deref] slider: Slider,
    #[live] value: f64,
    #[live(true)] enabled: bool,
    #[live] on_change: Option<ScriptFnRef>,
    #[rust] focus_area: Area,
}
impl ScriptHook for MiniAppAmountSlider {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, _: &Apply, _: &mut Scope, _: ScriptValue) {
        self.value = if self.value.is_finite() {self.value.clamp(0.0, 1.0)} else {0.0};
        if self.slider.dragging.is_none() {
            self.slider.set_value(vm.cx_mut(), self.value);
        }
        self.slider.set_disabled(vm.cx_mut(), !self.enabled);
    }
}
impl MiniAppAmountSlider {
    pub(crate) fn focus_area(&self)->Area {self.focus_area}
    fn changed(&mut self, cx: &mut Cx, value: f64) {
        if !value.is_finite() {return;}
        self.value = value.clamp(0.0, 1.0);
        self.slider.set_value(cx, self.value);
        self.slider.redraw(cx);
        cx.widget_action(self.widget_uid(),SliderAction::Slide(self.value));
        if let Some(handler) = self.on_change.clone() {
            cx.widget_to_script_call(self.widget_uid(), NIL, self.source.clone(), handler,
                &[ScriptValue::from_f64(self.value)]);
        }
    }
}
impl Widget for MiniAppAmountSlider {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        for action in cx.capture_actions(|cx| self.slider.handle_event(cx, event, scope)) {
            if !self.enabled {continue;}
            match action.as_widget_action().cast() {
                SliderAction::StartSlide | SliderAction::EndSlide(_) => cx.set_key_focus(self.focus_area),
                SliderAction::Slide(v) | SliderAction::TextSlide(v) => self.changed(cx, v),
                _ => (),
            }
        }
        if let Event::KeyDown(key) = event {
            if self.enabled && cx.has_key_focus(self.focus_area) {
                let value = match key.key_code {
                    KeyCode::ArrowLeft | KeyCode::ArrowDown => Some(self.value - 0.01),
                    KeyCode::ArrowRight | KeyCode::ArrowUp => Some(self.value + 0.01),
                    KeyCode::Home => Some(0.0), KeyCode::End => Some(1.0), _ => None,
                };
                if let Some(value) = value {self.changed(cx, value);}
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let step = self.slider.draw_walk(cx, scope, walk);
        if step.is_done() {
            let rect = self.slider.area().rect(cx);
            cx.add_aligned_rect_area(&mut self.focus_area, rect);
            if self.enabled {cx.add_nav_stop(self.focus_area, NavRole::TextInput, Inset::default());}
        }
        step
    }
    fn script_call(&mut self, vm: &mut ScriptVm, method: LiveId, args: ScriptValue) -> ScriptAsyncResult {
        if method == id!(set_value) {
            if let Some(args) = args.as_object() {
                let trap = vm.bx.threads.cur().trap.pass();
                if let Some(value) = vm.bx.heap.vec_value(args, 0, trap).as_f64() {
                    self.value = if value.is_finite() {value.clamp(0.0, 1.0)} else {0.0};
                    self.slider.set_value(vm.cx_mut(), self.value);
                }
            }
            return ScriptAsyncResult::Return(NIL);
        }
        self.slider.script_call(vm, method, args)
    }
}

/// The pinned Button supports Tab focus but not keyboard activation. Keep the
/// adapter local to mini apps until the framework supplies the same contract.
#[derive(Script, Widget)]
pub struct MiniAppButton {
    #[deref]
    button: Button,
}
impl ScriptHook for MiniAppButton {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, _: &Apply, _: &mut Scope, _: ScriptValue) {
        self.button.set_disabled(vm.cx_mut(), !self.button.enabled());
    }
}
impl Widget for MiniAppButton {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.button.handle_event(cx, event, scope);
        if let Event::KeyDown(key) = event {
            if self.button.enabled() && !self.button.disabled(cx) && self.button.visible()
                && cx.has_key_focus(self.button.area()) && !key.is_repeat
                && !key.modifiers.control && !key.modifiers.logo && !key.modifiers.alt
                && matches!(key.key_code, KeyCode::ReturnKey | KeyCode::NumpadEnter | KeyCode::Space)
            {
                cx.with_vm(|vm| { self.button.script_call(vm, id!(on_click), NIL); });
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.button.draw_walk(cx, scope, walk)
    }
    fn script_call(&mut self, vm: &mut ScriptVm, method: LiveId, args: ScriptValue) -> ScriptAsyncResult {
        // A task tab can rebuild its content. Restore focus after that frame so
        // subsequent Tab/Enter input continues from the selected native button.
        if method == id!(focus) {
            if self.button.enabled() && self.button.visible() {
                vm.with_cx_mut(|cx| cx.set_key_focus(self.button.area()));
            }
            return ScriptAsyncResult::Return(NIL);
        }
        // Update one button without rebuilding its parent and stealing input focus.
        if method == id!(set_enabled) {
            if let Some(args) = args.as_object() {
                let trap = vm.bx.threads.cur().trap.pass();
                if let Some(enabled) = vm.bx.heap.vec_value(args, 0, trap).as_bool() {
                    let patch = vm.bx.heap.new_object();
                    vm.bx.heap.set_value(patch, id!(enabled).into(), enabled.into(), NoTrap);
                    self.button.script_apply(vm, &Apply::Animate, &mut Scope::empty(), patch.into());
                    self.button.set_disabled(vm.cx_mut(), !enabled);
                    self.button.redraw(vm.cx_mut());
                }
            }
            return ScriptAsyncResult::Return(NIL);
        }
        self.button.script_call(vm, method, args)
    }
    fn text(&self) -> String { self.button.text() }
    fn set_text(&mut self, cx: &mut Cx, text: &str) { self.button.set_text(cx,text); }
    fn disabled(&self, cx: &Cx) -> bool { !self.button.enabled() || self.button.disabled(cx) }
    fn set_disabled(&mut self, cx: &mut Cx, disabled: bool) { self.button.set_disabled(cx,disabled); }
}

/// Use the macOS UI's installed CJK sans inside mini apps: PingFang SC, as in
/// the host UI, so bold roles keep their weight; Hiragino Sans GB, one face
/// for every role, where PingFang is unavailable. Keep this in the script font
/// family so lazy emoji loading cannot discard the font. Only the framework's
/// default WenKai fallback is replaced; customer fonts and the Latin face,
/// metrics and lazy-loading policy are retained.
pub(super) fn install_system_cjk_fallback(vm: &mut ScriptVm) {
    #[cfg(target_os = "macos")]
    {
        let (regular, bold) = match article_makepad::apple_fonts::pingfang(vm) {
            Ok(faces) => faces,
            Err(_) => {
                const FONT: &str = "/System/Library/Fonts/Hiragino Sans GB.ttc";
                if !std::path::Path::new(FONT).is_file() { return; }
                vm.bx.code.crate_manifests.borrow_mut().insert(
                    "rinx_system_fonts".into(), "/System/Library/Fonts".into());
                let resource = makepad_widgets::makepad_platform::script::res::register_crate_resource_path(
                    vm, "rinx_system_fonts/Hiragino Sans GB.ttc");
                (resource, resource)
            }
        };
        let theme = vm.module(id!(theme));
        for role in [id!(font_regular), id!(font_label), id!(font_bold), id!(font_italic), id!(font_bold_italic)] {
            let resource = if role == id!(font_bold) || role == id!(font_bold_italic) { bold } else { regular };
            let Some(style) = vm.bx.heap.value(theme, role.into(), NoTrap).as_object() else { continue; };
            let Some(family) = vm.bx.heap.value(style, id!(font_family).into(), NoTrap).as_object() else { continue; };
            for i in 0..vm.bx.heap.vec_len(family) {
                let kv = vm.bx.heap.vec_key_value(family, i, NoTrap);
                let Some(member) = kv.value.as_object() else { continue; };
                let Some(handle) = vm.bx.heap.value(member, id!(res).into(), NoTrap).as_handle() else { continue; };
                let heap_key = vm.bx.heap.heap_key();
                let path = vm.cx().script_data.resources.path_for_handle(heap_key, handle);
                if path.as_deref().is_some_and(|p| p.ends_with("/LXGWWenKaiRegular.ttf") || p.ends_with("/LXGWWenKaiBold.ttf")) {
                    vm.bx.heap.set_value(member, id!(res).into(), resource, NoTrap);
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = vm;
}

/// A persistent scroll container for multi-page mini apps. Navigating to a new
/// route can reset its offset without rebuilding the scroll view or scheduling
/// a second render before newly created widgets have entered the widget tree.
#[derive(Script, ScriptHook, Widget)]
pub struct MiniAppPageScroll {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    scroll_positions: HashMap<String, Vec2d>,
    #[rust]
    last_scroll: Vec2d,
    #[rust]
    restore_position: Option<Vec2d>,
    #[rust]
    restore_frame: NextFrame,
    /// Scroll offset past which `on_scroll_mark` reports true, as when a
    /// page's own title scrolls under its navigation bar.
    #[live]
    scroll_mark: f64,
    #[live]
    on_scroll_mark: Option<ScriptFnRef>,
    #[rust]
    past_mark: bool,
    #[rust]
    mark_frame: NextFrame,
    /// A named section to bring to the top once it has been laid out, for
    /// rows that deep-link into a long page. Cleared by `reset_scroll`.
    #[rust]
    anchor: Option<LiveId>,
}
impl Widget for MiniAppPageScroll {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.restore_frame.is_event(event).is_some() {
            self.view.redraw(cx);
        }
        if self.mark_frame.is_event(event).is_some() {
            if let Some(handler) = self.on_scroll_mark.clone() {
                cx.widget_to_script_call(self.widget_uid(), NIL, self.source.clone(), handler,
                    &[ScriptValue::from_bool(self.past_mark)]);
            }
        }
        self.view.handle_event(cx, event, scope);
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let step = self.view.draw_walk(cx, scope, walk);
        if step.is_done() {
            // Area data is valid immediately after drawing. Script callbacks
            // may run after render() has invalidated those areas, so retain
            // the measured displacement here rather than reading stale areas.
            let viewport = self.view.view(cx, ids!(body)).area().rect(cx);
            let page = self.view.view(cx, ids!(body.page)).area().rect(cx);
            self.last_scroll = dvec2(0.0, (viewport.pos.y - page.pos.y).max(0.0));
            // Report crossings on the next frame: script must not run mid-draw.
            let past = self.on_scroll_mark.is_some() && self.last_scroll.y > self.scroll_mark;
            if past != self.past_mark {
                self.past_mark = past;
                self.mark_frame = cx.new_next_frame();
            }
            if let Some(pos) = self.restore_position.take() {
                // The new page's scroll bounds are now known; applying before
                // this draw can clamp a saved offset to a loading placeholder.
                self.view.view(cx, ids!(body)).set_scroll_pos(cx, pos);
                self.restore_frame = cx.new_next_frame();
            }
            if let Some(anchor) = self.anchor {
                // Only a section that was drawn has a position; until then the
                // anchor waits for the render that adds it.
                let target = self.view.widget(cx, &[anchor]);
                let rect = target.area().rect(cx);
                if !target.is_empty() && rect.size.y > 0.0 {
                    self.anchor = None;
                    let offset = (self.last_scroll.y + rect.pos.y - viewport.pos.y - 8.0).max(0.0);
                    self.view.view(cx, ids!(body)).set_scroll_pos(cx, dvec2(0.0, offset));
                    self.restore_frame = cx.new_next_frame();
                }
            }
        }
        step
    }
    fn script_call(&mut self, vm: &mut ScriptVm, method: LiveId, args: ScriptValue) -> ScriptAsyncResult {
        if method == id!(remember_scroll) || method == id!(restore_scroll) {
            let key = args.as_object().and_then(|args| {
                let trap = vm.bx.threads.cur().trap.pass();
                let value = vm.bx.heap.vec_value(args, 0, trap);
                vm.bx.heap.cast_to_owned_string(value, "mini app scroll route")
            });
            if let Some(key) = key {
                vm.with_cx_mut(|cx| {
                    if method == id!(remember_scroll) {
                        self.scroll_positions.insert(key, self.last_scroll);
                    } else {
                        self.restore_position = self.scroll_positions.get(&key).copied();
                        self.view.redraw(cx);
                    }
                });
            }
            return ScriptAsyncResult::Return(NIL);
        }
        if method == id!(scroll_to) {
            let name = args.as_object().and_then(|args| {
                let trap = vm.bx.threads.cur().trap.pass();
                let value = vm.bx.heap.vec_value(args, 0, trap);
                vm.bx.heap.cast_to_owned_string(value, "mini app scroll anchor")
            });
            if let Some(name) = name.filter(|name| !name.is_empty() && name.len() <= 64) {
                vm.with_cx_mut(|cx| {
                    self.anchor = Some(LiveId::from_str(&name));
                    self.view.redraw(cx);
                });
            }
            return ScriptAsyncResult::Return(NIL);
        }
        if method == id!(reset_scroll) {
            self.anchor = None;
            self.restore_position = None;
            self.last_scroll = dvec2(0.0, 0.0);
            vm.with_cx_mut(|cx| {
                self.view.view(cx, ids!(body)).set_scroll_pos(cx, dvec2(0.0, 0.0));
                self.view.redraw(cx);
            });
            return ScriptAsyncResult::Return(NIL);
        }
        ScriptAsyncResult::MethodNotFound
    }
}
script_mod! {
        use mod.prelude.widgets.*
        mod.widgets.RinxAmountSlider = set_type_default() do #(MiniAppAmountSlider::register_widget(vm)) {
            // The whole chain, nearest first; see RinxKeyButton below.
            ..mod.widgets.Slider,
            ..mod.widgets.SliderFlat,
            ..mod.widgets.SliderMinimal,
            width: Fill height: 44 margin: 0 padding: 0
            min: 0 max: 1 step: 0 default: 0
            label_walk: Walk{width: 0 height: 0}
            text_input +: {width: 0 height: 0 is_read_only: true}
            draw_bg +: {
                label_size: 0
                track_color: uniform(mod.widgets.RINX_BORDER)
                fill_color: uniform(mod.widgets.RINX_ACCENT)
                knob_color: uniform(mod.widgets.RINX_FIELD)
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    let x = 12.0 + (self.rect_size.x - 24.0) * self.slide_pos
                    let y = self.rect_size.y * 0.5
                    sdf.box(12.0, y - 3.0, self.rect_size.x - 24.0, 6.0, 3.0)
                    sdf.fill(self.track_color)
                    sdf.box(12.0, y - 3.0, max(0.0, x - 12.0), 6.0, 3.0)
                    sdf.fill(self.fill_color.mix(self.track_color, self.disabled))
                    sdf.circle(x, y, 9.0)
                    sdf.fill_keep(self.knob_color)
                    sdf.stroke(self.fill_color.mix(self.track_color, self.disabled), 2.0 + self.focus)
                    return sdf.result
                }
            }
        }
        mod.widgets.RinxNumberInput = set_type_default() do #(MiniAppNumberInput::register_widget(vm)) {
            width: Fill height: Fit flow: Right spacing: 8 align: Align{y:0.5}
            slider_wrap := View{visible:false width:Fill height:44
                amount_slider := mod.widgets.RinxAmountSlider{width:Fill}
            }
            input := mod.widgets.RinxInput{width:110 height:max(44,mod.widgets.RINX_CONTROL_HEIGHT)}
            unit := mod.widgets.RinxHint{visible:false width:Fit text:"M"}
            // The value reads with its unit; the stepper sits at the trailing edge.
            stepper_gap := View{width:Fill height:1}
            decrease := mod.widgets.RinxButton{width:max(44,mod.widgets.RINX_CONTROL_HEIGHT) height:max(44,mod.widgets.RINX_CONTROL_HEIGHT) text:"−" grab_key_focus:true}
            increase := mod.widgets.RinxButton{width:max(44,mod.widgets.RINX_CONTROL_HEIGHT) height:max(44,mod.widgets.RINX_CONTROL_HEIGHT) text:"+" grab_key_focus:true}
        }
        // A spread copies only what its source sets itself, and when a page
        // renders again the engine resets every property this type's default
        // does not name. Name the whole chain, nearest first (a spread never
        // overwrites), down to the engine Button's own default: without it the
        // width, alignment and label box of every button fell back to
        // full-width, top-left after the first render.
        mod.widgets.RinxKeyButton = set_type_default() do #(MiniAppButton::register_widget(vm)) {
            ..mod.widgets.RinxButton,
            ..mod.widgets.Button,
            ..mod.widgets.ButtonFlat,
        }
        mod.widgets.RinxKeyPrimaryButton = mod.widgets.RinxKeyButton {
            draw_bg +: {color: mod.widgets.RINX_ACCENT color_focus: mod.widgets.RINX_ACCENT color_hover: mod.widgets.RINX_ACCENT_HOVER color_down: mod.widgets.RINX_ACCENT_DOWN
                border_color: mod.widgets.RINX_ACCENT border_color_hover: mod.widgets.RINX_ACCENT_HOVER border_color_down: mod.widgets.RINX_ACCENT_DOWN border_color_focus: mod.widgets.RINX_ON_ACCENT}
            draw_text +: {color: mod.widgets.RINX_ON_ACCENT color_hover: mod.widgets.RINX_ON_ACCENT color_down: mod.widgets.RINX_ON_ACCENT color_focus: mod.widgets.RINX_ON_ACCENT}
            draw_icon +: {color: mod.widgets.RINX_ON_ACCENT}
        }
mod.widgets.RinxToggle = mod.widgets.RinxKeyButton{height:max(44,mod.widgets.RINX_CONTROL_HEIGHT) padding:Inset{left:60 right:12 top:10 bottom:10}
    draw_bg +: {selected:instance(0.0) accent:uniform(mod.widgets.RINX_ACCENT) muted:uniform(mod.widgets.RINX_MUTED) knob:uniform(mod.widgets.RINX_ON_ACCENT)
        pixel:fn(){
            let sdf=Sdf2d.viewport(self.pos * self.rect_size)
            let y=self.rect_size.y * 0.5
            sdf.box(1.0,1.0,self.rect_size.x-2.0,self.rect_size.y-2.0,6.0)
            sdf.fill_keep(self.color.mix(self.color_hover,self.hover))
            sdf.stroke(self.accent, self.focus)
            sdf.box(12.0,y-10.0,34.0,20.0,10.0) sdf.fill(self.muted.mix(self.accent,self.selected).mix(self.muted,self.disabled))
            sdf.circle(22.0+14.0*self.selected,y,7.0) sdf.fill(self.knob)
            return sdf.result
        }
    }
}
// The track is iOS's 51 x 31; the tap target is the full 44 pt tall around it.
mod.widgets.RinxSwitch = mod.widgets.RinxKeyButton{width:51 height:44 text:"" padding:0
    draw_bg +: {selected:instance(0.0) on_color:uniform(#x1f9d55) off_color:uniform(#x7f8b9c) knob:uniform(#fff)
        pixel:fn(){
            let sdf=Sdf2d.viewport(self.pos * self.rect_size)
            let h=min(31.0,self.rect_size.y)
            let y0=(self.rect_size.y-h)*0.5
            sdf.box(0.0,y0,self.rect_size.x,h,h*0.5)
            sdf.fill(self.off_color.mix(self.on_color,self.selected))
            let r=h*0.5-2.5
            sdf.circle(2.5+r+(self.rect_size.x-5.0-2.0*r)*self.selected,y0+h*0.5,r)
            sdf.fill(self.knob)
            // Disabled switches fade as a whole, in either position.
            return sdf.result*(1.0-0.5*self.disabled)
        }
    }
}
mod.widgets.RinxChevron = View{width:8 height:14 show_bg:true
    draw_bg +: {color:mod.widgets.RINX_MUTED
        pixel:fn(){
            let sdf=Sdf2d.viewport(self.pos * self.rect_size)
            sdf.move_to(1.0,1.0)
            sdf.line_to(self.rect_size.x-1.5,self.rect_size.y*0.5)
            sdf.line_to(1.0,self.rect_size.y-1.0)
            sdf.stroke(self.color,1.6)
            return sdf.result
        }
    }
}
mod.widgets.RinxProgress = mod.widgets.RinxAmountSlider{height:8 enabled:false
    draw_bg +: {pixel:fn(){
        let sdf=Sdf2d.viewport(self.pos * self.rect_size)
        sdf.box(0.0,0.0,self.rect_size.x,self.rect_size.y,4.0) sdf.fill(self.track_color)
        sdf.box(0.0,0.0,self.rect_size.x * self.slide_pos,self.rect_size.y,4.0) sdf.fill(self.fill_color)
        return sdf.result
    }}
}
        mod.widgets.RinxPageScroll = set_type_default() do #(MiniAppPageScroll::register_widget(vm)) {
            width: Fill height: Fill
            body := ScrollYView {width: Fill height: Fill flow: Down
                page := View {width: Fill height: Fit flow: Down spacing: 16}
            }
        }
        mod.rinx_icons = {
            hagency: crate_resource("self://resources/icons/hagency-network.svg")
            projects: crate_resource("self://resources/icons/hagency-project.svg")
            tasks: crate_resource("self://resources/icons/hagency-tasks.svg")
            activity: crate_resource("self://resources/icons/hagency-activity.svg")
            home: crate_resource("self://resources/icons/home.svg")
            resources: crate_resource("self://resources/icons/squares_filled.svg")
            connection: crate_resource("self://resources/icons/link.svg")
            inbox: crate_resource("self://resources/icons/shield.svg")
            refresh: crate_resource("self://resources/icons/article_refresh.svg")
            search: crate_resource("self://resources/icons/search.svg")
            add: crate_resource("self://resources/icons/add.svg")
            check: crate_resource("self://resources/icons/checkmark.svg")
            close: crate_resource("self://resources/icons/close.svg")
            back: crate_resource("self://resources/icons/arrow_back.svg")
            person: crate_resource("self://resources/icons/person.svg")
            device: crate_resource("self://resources/icons/device.svg")
            expand: crate_resource("self://resources/icons/chevron_down.svg")
            collapse: crate_resource("self://resources/icons/chevron_up.svg")
            chevron: crate_resource("self://resources/icons/chevron_right.svg")
            chevron_back: crate_resource("self://resources/icons/chevron_left.svg")
            lock: crate_resource("self://resources/icons/lock.svg")
            chat: crate_resource("self://resources/icons/message_circle.svg")
            power: crate_resource("self://resources/icons/power.svg")
            robot: crate_resource("self://resources/icons/robot.svg")
            cpu: crate_resource("self://resources/icons/cpu.svg")
            bulb: crate_resource("self://resources/icons/bulb.svg")
            usage: crate_resource("self://resources/icons/chart_bar.svg")
            gauge: crate_resource("self://resources/icons/gauge.svg")
            people: crate_resource("self://resources/icons/people.svg")
            globe: crate_resource("self://resources/icons/globe.svg")
            shield: crate_resource("self://resources/icons/shield.svg")
            adjust: crate_resource("self://resources/icons/adjustments.svg")
            sync: crate_resource("self://resources/icons/refresh.svg")
            folder: crate_resource("self://resources/icons/folder.svg")
            alert: crate_resource("self://resources/icons/alert_triangle.svg")
            reply: crate_resource("self://resources/icons/reply.svg")
            trash: crate_resource("self://resources/icons/trash.svg")
            info: crate_resource("self://resources/icons/info.svg")
            edit: crate_resource("self://resources/icons/edit.svg")
            file: crate_resource("self://resources/icons/file.svg")
            eye: crate_resource("self://resources/icons/eye_open.svg")
            pause: crate_resource("self://resources/icons/service_pause.svg")
            wallet: crate_resource("self://resources/icons/add_wallet.svg")
        }
}
