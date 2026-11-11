//! Native confirmation for account and resource management.
use super::*;
pub(super) enum ProviderReview {
    Retire(crate::host::hagency::retirement::RetirementConsent),
    Enroll(crate::host::hagency::enrollment::ResourceConsent),
    Edit(Box<resource_edit_ui::ResourceEditPrompt>),
    Create(Box<resource_create_ui::ResourceCreatePrompt>),
    Personal(Box<personal_configuration_ui::PersonalPrompt>),
    Connection(Box<web_connections_ui::ConnectionPrompt>),
}
impl ProviderReview {
    pub(super) fn account(&self)->&str {match self {Self::Retire(v)=>v.account(),Self::Enroll(v)=>v.account(),Self::Create(v)=>v.account(),Self::Edit(v)=>v.account(),Self::Personal(v)=>v.account(),Self::Connection(v)=>v.account()}}
    pub(super) fn details(&self)->String {match self {Self::Retire(v)=>v.details(),Self::Enroll(v)=>v.details(),Self::Create(v)=>v.details(),Self::Edit(v)=>v.details(),Self::Personal(v)=>v.details(),Self::Connection(v)=>v.details()}}
    pub(super) fn expired(&self)->bool {match self {Self::Retire(v)=>v.expired(),Self::Enroll(v)=>v.expired(),Self::Create(v)=>v.expired(),Self::Edit(v)=>v.expired(),Self::Personal(v)=>v.expired(),Self::Connection(v)=>v.expired()}}
    pub(super) fn cancelled(&self)->&'static str {match self {Self::Retire(_)=>"account_retirement_cancelled",Self::Enroll(_)=>"resource_enrollment_cancelled",Self::Create(_)=>"resource_create_cancelled",Self::Edit(_)=>"resource_edit_cancelled",Self::Personal(_)=>"personal_configuration_cancelled",Self::Connection(_)=>"web_connection_cancelled"}}
    pub(super) fn ready(&self)->bool {match self {Self::Create(v)=>v.ready(),Self::Edit(v)=>v.ready(),Self::Personal(v)=>v.ready(),Self::Connection(v)=>v.ready(),_=>!self.expired()}}
    pub(super) fn poll(&mut self)->bool {match self {Self::Create(v)=>v.poll(),Self::Edit(v)=>v.poll(),Self::Personal(v)=>v.poll(),Self::Connection(v)=>v.poll(),_=>false}}
    pub(super) fn words(&self)->(&'static str,&'static str,&'static str,&'static str){
        match self {
            Self::Personal(v)=>v.words(),
            Self::Connection(v)=>v.words(),
            Self::Retire(_)=>("Retire this account space","Hagency will stop using this account space on this device. It must have no associated resources or sign-in in progress.","This cannot be undone here. Provider sign-out is not performed, and credentials are not deleted. Existing resources and agents are not removed.","Retire account space"),
            Self::Enroll(_)=>("Enroll this account’s resource","Hagency will verify this account and model again, then save an unpublished resource on this device. Credentials stay with Codex.","The monthly limit is your declaration, not a provider balance. Resources using the same provider account share one limit. Enrollment does not publish capacity or start an agent.","Enroll resource"),
            Self::Create(v) if v.recovering() => ("Recover saved configuration creation","This original request already created a configuration. Recover its recorded result without creating another.","The recorded result identifies the new resource. Open its details to see its current configuration.","Recover saved result"),
            Self::Create(_) => ("Add another pool","Hagency keeps the current pool as it is and creates a separate pool using the same account.","Set the new pool’s monthly maximum here. Connecting it to a server and publishing capacity are separate steps.","Create pool"),
            Self::Edit(v) if v.recovering() => ("Recover saved resource edit","This original request was already saved. Recover its recorded result without applying another edit.","The recorded result describes that request. Refresh resource details to see the latest configuration.","Recover saved result"),
            Self::Edit(v) if v.amending() || v.amendment_requested() => resource_edit_ui::AMENDMENT_WORDS,
            Self::Edit(_) => ("Change model settings","Hagency checks this account, model and pool again before saving.","This changes the pool on this computer for every profile that uses it. Clearing its monthly maximum pauses new requests.","Save settings"),
        }
    }
    /// A monthly-maximum request: shown in its own sheet of the confirmation
    /// family, which stays open while the change saves.
    pub(super) fn ceiling(&self)->Option<&resource_edit_ui::ResourceEditPrompt>{match self {Self::Edit(v) if v.amendment_requested()=>Some(v),_=>None}}
    pub(super) fn ceiling_mut(&mut self)->Option<&mut resource_edit_ui::ResourceEditPrompt>{match self {Self::Edit(v) if v.amendment_requested()=>Some(v),_=>None}}
    pub(super) async fn accept(self)->Result<Value,String>{match self {Self::Retire(v)=>v.accept().await,Self::Enroll(v)=>v.accept().await,Self::Create(v)=>v.accept().await,Self::Edit(v)=>v.accept().await,Self::Personal(v)=>v.accept().await,Self::Connection(v)=>v.accept().await}}
}
#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;
    #[test]
    fn native_hagency_enrollment_footer_survives_long_account_and_model_details() {
        for (width,height,language) in [(360.,640.,Language::English),(360.,640.,Language::Chinese),(840.,700.,Language::English),(840.,700.,Language::Chinese)] {
            let mut cx=Cx::new(Box::new(|_,_|{}));
            let mut panel=cx.with_vm(|vm|{
                makepad_widgets::script_mod(vm);crate::i18n::install(vm);makepad_code_editor::script_mod(vm);crate::shared::script_mod(vm);
                vm.bx.captured_errors=Some(Vec::new());crate::miniapps::script_mod(vm);
                let value=vm.eval(script!{use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
                let panel=MiniAppsPanel::script_from_value(vm,value);assert!(vm.take_errors().is_empty());panel
            });
            for path in [ids!(catalog),ids!(import_form),ids!(hagency_review),ids!(app_content),ids!(notice),ids!(approval)] {panel.view.widget(&cx,path).set_visible(&mut cx,false);}
            panel.view.widget(&cx,ids!(provider_confirmation)).set_visible(&mut cx,true);
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.heading.title)).set_text(&mut cx,crate::i18n::text_for(language,"Enroll this account’s resource"));
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.identity.account)).set_text(&mut cx,"@original-owner:matrix.example");
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.identity.runtime)).set_text(&mut cx,&"account_0123456789abcdef0123456789abcdef\nModel and declared capacity\n".repeat(12));
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.warning)).set_text(&mut cx,crate::i18n::text_for(language,"The monthly limit is your declaration, not a provider balance. Resources using the same provider account share one limit. Enrollment does not publish capacity or start an agent."));
            panel.view.button(&cx,ids!(provider_confirmation.buttons.connect)).set_text(&mut cx,crate::i18n::text_for(language,"Enroll resource"));
            panel.view.button(&cx,ids!(provider_confirmation.buttons.cancel)).set_text(&mut cx,crate::i18n::text_for(language,"Cancel"));
            let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(width,height));let mut list=DrawList2d::new(&mut cx);
            panel.request_confirmation_redraw(&mut cx);let event=std::mem::take(&mut cx.new_draw_event);assert!(event.will_redraw());
            let mut draw=CxDraw::new(&mut cx,&event);let mut c=Cx2d::new(&mut draw);c.begin_pass(&pass,None);
            if list.begin(&mut c,Walk::default()).is_redrawing(){c.begin_root_turtle(dvec2(width,height),Layout::flow_overlay());panel.draw_all(&mut c,&mut Scope::empty());c.end_pass_sized_turtle();list.end(&mut c);}
            c.end_pass(&pass);drop(c);drop(draw);
            for path in [ids!(provider_confirmation.buttons.cancel),ids!(provider_confirmation.buttons.connect)] {
                let button=panel.view.button(&cx,path);let rect=button.area().rect(&cx);
                assert!(button.visible() && button.area().is_valid(&cx));
                assert!(rect.size.x>0. && rect.size.y>=40. && rect.pos.y>=0. && rect.pos.y+rect.size.y<=height && rect.pos.x>=0. && rect.pos.x+rect.size.x<=width,"{rect:?}");
            }
        }
    }    #[test]
    fn native_hagency_retirement_footer_survives_long_account_details() {
        for (width,height,language) in [(360.,640.,Language::English),(360.,640.,Language::Chinese),(840.,700.,Language::English),(840.,700.,Language::Chinese)] {
            let mut cx=Cx::new(Box::new(|_,_|{}));
            let mut panel=cx.with_vm(|vm|{
                makepad_widgets::script_mod(vm);crate::i18n::install(vm);makepad_code_editor::script_mod(vm);crate::shared::script_mod(vm);
                vm.bx.captured_errors=Some(Vec::new());crate::miniapps::script_mod(vm);
                let value=vm.eval(script!{use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
                let panel=MiniAppsPanel::script_from_value(vm,value);assert!(vm.take_errors().is_empty());panel
            });
            for path in [ids!(catalog),ids!(import_form),ids!(hagency_review),ids!(app_content),ids!(notice),ids!(approval)] {panel.view.widget(&cx,path).set_visible(&mut cx,false);}
            panel.view.widget(&cx,ids!(provider_confirmation)).set_visible(&mut cx,true);
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.heading.title)).set_text(&mut cx,crate::i18n::text_for(language,"Retire this account space"));
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.identity.account)).set_text(&mut cx,"@original-owner:matrix.example");
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.identity.runtime)).set_text(&mut cx,&"account_0123456789abcdef0123456789abcdef\nModel and declared capacity\n".repeat(12));
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.warning)).set_text(&mut cx,crate::i18n::text_for(language,"This cannot be undone here. Provider sign-out is not performed, and credentials are not deleted. Existing resources and agents are not removed."));
            panel.view.button(&cx,ids!(provider_confirmation.buttons.connect)).set_text(&mut cx,crate::i18n::text_for(language,"Retire account space"));
            panel.view.button(&cx,ids!(provider_confirmation.buttons.cancel)).set_text(&mut cx,crate::i18n::text_for(language,"Cancel"));
            let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(width,height));let mut list=DrawList2d::new(&mut cx);
            panel.request_confirmation_redraw(&mut cx);let event=std::mem::take(&mut cx.new_draw_event);assert!(event.will_redraw());
            let mut draw=CxDraw::new(&mut cx,&event);let mut c=Cx2d::new(&mut draw);c.begin_pass(&pass,None);
            if list.begin(&mut c,Walk::default()).is_redrawing(){c.begin_root_turtle(dvec2(width,height),Layout::flow_overlay());panel.draw_all(&mut c,&mut Scope::empty());c.end_pass_sized_turtle();list.end(&mut c);}
            c.end_pass(&pass);drop(c);drop(draw);
            for path in [ids!(provider_confirmation.buttons.cancel),ids!(provider_confirmation.buttons.connect)] {
                let button=panel.view.button(&cx,path);let rect=button.area().rect(&cx);
                assert!(button.visible() && button.area().is_valid(&cx));
                assert!(rect.size.x>0. && rect.size.y>=40. && rect.pos.y>=0. && rect.pos.y+rect.size.y<=height && rect.pos.x>=0. && rect.pos.x+rect.size.x<=width,"{rect:?}");
            }
        }
    }
    #[test]
    fn native_hagency_resource_edit_footer_survives_full_before_and_after_details() {
        for (width,height,language) in [(360.,640.,Language::English),(360.,640.,Language::Chinese),(840.,700.,Language::English),(840.,700.,Language::Chinese)] {
            let mut cx=Cx::new(Box::new(|_,_|{}));
            let mut panel=cx.with_vm(|vm|{
                makepad_widgets::script_mod(vm);crate::i18n::install(vm);makepad_code_editor::script_mod(vm);crate::shared::script_mod(vm);
                vm.bx.captured_errors=Some(Vec::new());crate::miniapps::script_mod(vm);
                let value=vm.eval(script!{use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
                let panel=MiniAppsPanel::script_from_value(vm,value);assert!(vm.take_errors().is_empty());panel
            });
            for path in [ids!(catalog),ids!(import_form),ids!(hagency_review),ids!(app_content),ids!(notice),ids!(approval)] {panel.view.widget(&cx,path).set_visible(&mut cx,false);}
            panel.view.widget(&cx,ids!(provider_confirmation)).set_visible(&mut cx,true);
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.heading.title)).set_text(&mut cx,crate::i18n::text_for(language,"Review resource configuration"));
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.identity.account)).set_text(&mut cx,"@original-owner:matrix.example");
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.identity.runtime)).set_text(&mut cx,&"account_0123456789abcdef0123456789abcdef\nModel and declared capacity\n".repeat(12));
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.warning)).set_text(&mut cx,crate::i18n::text_for(language,"This changes the pool on this computer for every profile that uses it. Clearing its monthly maximum pauses new requests."));
            panel.view.button(&cx,ids!(provider_confirmation.buttons.connect)).set_text(&mut cx,crate::i18n::text_for(language,"Save configuration"));
            panel.view.button(&cx,ids!(provider_confirmation.buttons.cancel)).set_text(&mut cx,crate::i18n::text_for(language,"Cancel"));
            let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(width,height));let mut list=DrawList2d::new(&mut cx);
            panel.request_confirmation_redraw(&mut cx);let event=std::mem::take(&mut cx.new_draw_event);assert!(event.will_redraw());
            let mut draw=CxDraw::new(&mut cx,&event);let mut c=Cx2d::new(&mut draw);c.begin_pass(&pass,None);
            if list.begin(&mut c,Walk::default()).is_redrawing(){c.begin_root_turtle(dvec2(width,height),Layout::flow_overlay());panel.draw_all(&mut c,&mut Scope::empty());c.end_pass_sized_turtle();list.end(&mut c);}
            c.end_pass(&pass);drop(c);drop(draw);
            for path in [ids!(provider_confirmation.buttons.cancel),ids!(provider_confirmation.buttons.connect)] {
                let button=panel.view.button(&cx,path);let rect=button.area().rect(&cx);
                assert!(button.visible() && button.area().is_valid(&cx));
                assert!(rect.size.x>0. && rect.size.y>=40. && rect.pos.y>=0. && rect.pos.y+rect.size.y<=height && rect.pos.x>=0. && rect.pos.x+rect.size.x<=width,"{rect:?}");
            }
        }
    }    #[test]
    fn native_hagency_ceiling_amendment_footer_survives_long_details_in_both_languages() {
        for (width,height,language) in [(360.,640.,Language::English),(360.,640.,Language::Chinese),(840.,700.,Language::English),(840.,700.,Language::Chinese)] {
            let mut cx=Cx::new(Box::new(|_,_|{}));
            let mut panel=cx.with_vm(|vm|{
                makepad_widgets::script_mod(vm);crate::i18n::install(vm);makepad_code_editor::script_mod(vm);crate::shared::script_mod(vm);
                vm.bx.captured_errors=Some(Vec::new());crate::miniapps::script_mod(vm);
                let value=vm.eval(script!{use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
                let panel=MiniAppsPanel::script_from_value(vm,value);assert!(vm.take_errors().is_empty());panel
            });
            for path in [ids!(catalog),ids!(import_form),ids!(hagency_review),ids!(app_content),ids!(notice),ids!(approval)] {panel.view.widget(&cx,path).set_visible(&mut cx,false);}
            panel.view.widget(&cx,ids!(provider_confirmation)).set_visible(&mut cx,true);
            let (title,_,warning,confirm)=resource_edit_ui::AMENDMENT_WORDS;
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.heading.title)).set_text(&mut cx,crate::i18n::text_for(language,title));
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.identity.account)).set_text(&mut cx,"@original-owner:matrix.example");
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.identity.runtime)).set_text(&mut cx,&crate::host::hagency::resource_edit::amendment_details(language,&::hagency::native_owner::CeilingAmendmentReview{resource_id:"resource_0123456789abcdef01234567".into(),name:Some("A long owner-chosen pool name that wraps across lines".repeat(3)),revision:"a".repeat(64),sequence:7,previous_tokens:123_456_789,tokens:987_654_321,per_request_tokens:Some(12_345_678),calendar:None}).repeat(3));
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.warning)).set_text(&mut cx,crate::i18n::text_for(language,warning));
            panel.view.button(&cx,ids!(provider_confirmation.buttons.connect)).set_text(&mut cx,crate::i18n::text_for(language,confirm));
            panel.view.button(&cx,ids!(provider_confirmation.buttons.cancel)).set_text(&mut cx,crate::i18n::text_for(language,"Cancel"));
            let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(width,height));let mut list=DrawList2d::new(&mut cx);
            panel.request_confirmation_redraw(&mut cx);let event=std::mem::take(&mut cx.new_draw_event);assert!(event.will_redraw());
            let mut draw=CxDraw::new(&mut cx,&event);let mut c=Cx2d::new(&mut draw);c.begin_pass(&pass,None);
            if list.begin(&mut c,Walk::default()).is_redrawing(){c.begin_root_turtle(dvec2(width,height),Layout::flow_overlay());panel.draw_all(&mut c,&mut Scope::empty());c.end_pass_sized_turtle();list.end(&mut c);}
            c.end_pass(&pass);drop(c);drop(draw);
            for path in [ids!(provider_confirmation.buttons.cancel),ids!(provider_confirmation.buttons.connect)] {
                let button=panel.view.button(&cx,path);let rect=button.area().rect(&cx);
                assert!(button.visible() && button.area().is_valid(&cx));
                assert!(rect.size.x>0. && rect.size.y>=40. && rect.pos.y>=0. && rect.pos.y+rect.size.y<=height && rect.pos.x>=0. && rect.pos.x+rect.size.x<=width,"{rect:?}");
            }
        }
    }    #[test]
    fn native_hagency_resource_creation_footer_survives_source_and_shared_limit_details() {
        for (width,height,language) in [(360.,640.,Language::English),(360.,640.,Language::Chinese),(840.,700.,Language::English),(840.,700.,Language::Chinese)] {
            let mut cx=Cx::new(Box::new(|_,_|{}));
            let mut panel=cx.with_vm(|vm|{
                makepad_widgets::script_mod(vm);crate::i18n::install(vm);makepad_code_editor::script_mod(vm);crate::shared::script_mod(vm);
                vm.bx.captured_errors=Some(Vec::new());crate::miniapps::script_mod(vm);
                let value=vm.eval(script!{use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
                let panel=MiniAppsPanel::script_from_value(vm,value);assert!(vm.take_errors().is_empty());panel
            });
            for path in [ids!(catalog),ids!(import_form),ids!(hagency_review),ids!(app_content),ids!(notice),ids!(approval)] {panel.view.widget(&cx,path).set_visible(&mut cx,false);}
            panel.view.widget(&cx,ids!(provider_confirmation)).set_visible(&mut cx,true);
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.heading.title)).set_text(&mut cx,crate::i18n::text_for(language,"Create an additional configuration"));
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.identity.account)).set_text(&mut cx,"@original-owner:matrix.example");
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.identity.runtime)).set_text(&mut cx,&"account_0123456789abcdef0123456789abcdef\nModel and declared capacity\n".repeat(12));
            panel.view.label(&cx,ids!(provider_confirmation.sheet_scroll.warning)).set_text(&mut cx,crate::i18n::text_for(language,"The new pool is saved on this computer. Connecting it to a server and publishing capacity remain separate steps."));
            panel.view.button(&cx,ids!(provider_confirmation.buttons.connect)).set_text(&mut cx,crate::i18n::text_for(language,"Create configuration"));
            panel.view.button(&cx,ids!(provider_confirmation.buttons.cancel)).set_text(&mut cx,crate::i18n::text_for(language,"Cancel"));
            let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(width,height));let mut list=DrawList2d::new(&mut cx);
            panel.request_confirmation_redraw(&mut cx);let event=std::mem::take(&mut cx.new_draw_event);assert!(event.will_redraw());
            let mut draw=CxDraw::new(&mut cx,&event);let mut c=Cx2d::new(&mut draw);c.begin_pass(&pass,None);
            if list.begin(&mut c,Walk::default()).is_redrawing(){c.begin_root_turtle(dvec2(width,height),Layout::flow_overlay());panel.draw_all(&mut c,&mut Scope::empty());c.end_pass_sized_turtle();list.end(&mut c);}
            c.end_pass(&pass);drop(c);drop(draw);
            for path in [ids!(provider_confirmation.buttons.cancel),ids!(provider_confirmation.buttons.connect)] {
                let button=panel.view.button(&cx,path);let rect=button.area().rect(&cx);
                assert!(button.visible() && button.area().is_valid(&cx));
                assert!(rect.size.x>0. && rect.size.y>=40. && rect.pos.y>=0. && rect.pos.y+rect.size.y<=height && rect.pos.x>=0. && rect.pos.x+rect.size.x<=width,"{rect:?}");
            }
        }
    }}
