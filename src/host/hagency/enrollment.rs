//! Native review fixes the exact account, model and declared monthly limit.
use super::*;
pub(crate) const ENROLL: &str = "hagency.accounts.enroll";
pub(crate) struct ResourceConsent {
    host: Host,
    lease: Lease,
    input: ::hagency::native_owner::NamespaceEnrollment,
    expires: Instant,
}
impl Host {
    pub fn resource_consent(&self,lease:Lease,args:Value)->Result<ResourceConsent,String>{
        let input: ::hagency::native_owner::NamespaceEnrollment=parse(args)?;
        input.validate().map_err(|_|"invalid_management_arguments".to_owned())?;
        let account=self.check(&lease)?;lease.authorize(&account,ENROLL,None)?;
        ensure_enrollment_available(&input)?;
        Ok(ResourceConsent{host:self.clone(),lease,input,expires:Instant::now()+Duration::from_secs(300)})
    }
}
impl ResourceConsent {
    pub fn account(&self)->&str {&self.lease.identity().account}
    /// The owner's own Claude Code on this computer, not a Codex account folder.
    pub fn claude(&self)->bool {self.input.account.account_id == CLAUDE_ACCOUNT}
    /// The profile name is display-only; the native enrollment must still
    /// resolve and validate the owner's connected Octos configuration.
    pub fn octos(&self)->bool {octos_profile(&self.input).is_some()}
    pub fn details(&self)->String {
        enrollment_details(&self.input)
    }
    pub fn expired(&self)->bool {Instant::now()>=self.expires}
    pub async fn accept(self)->Result<Value,String>{
        if self.expired(){return Err("provider_confirmation_expired".into());}
        let account=self.host.check(&self.lease)?;self.lease.authorize(&account,ENROLL,None)?;
        let manager=self.host.manager(&self.lease).await?;
        self.host.check(&self.lease)?;self.lease.authorize(&account,ENROLL,None)?;
        let result=manager.enroll_account_resource(self.input,self.expires).await.map_err(|e|e.code).and_then(value);
        self.host.check(&self.lease).map_err(|_|"management_outcome_unknown".to_owned())?;
        self.lease.authorize(&account,ENROLL,None)?;
        rinx_miniapp_core::bounded_reply(result?)
    }
}
const CLAUDE_ACCOUNT: &str = "account_claude_code";
const OCTOS_ACCOUNT_PREFIX: &str = "account_octos_";
fn ensure_enrollment_available(input: &::hagency::native_owner::NamespaceEnrollment) -> Result<(),String> {
    if input.account.account_id.starts_with(OCTOS_ACCOUNT_PREFIX) {
        if octos_profile(input).is_none() { return Err("invalid_management_arguments".into()); }
        if !cfg!(target_os="macos") { return Err("octos_pool_platform_unsupported".into()); }
        Ok(())
    } else {
        Ok(())
    }
}
fn octos_profile(input: &::hagency::native_owner::NamespaceEnrollment) -> Option<&str> {
    let profile=input.account.account_id.strip_prefix(OCTOS_ACCOUNT_PREFIX)?;
    (!profile.is_empty() && profile.len()<=64 && !profile.starts_with('-')
        && profile.bytes().all(|b| b.is_ascii_alphanumeric() || b==b'-' || b==b'_')).then_some(profile)
}
pub(crate) fn octos_review_text(language: crate::i18n::Language) -> &'static str {
    if language == crate::i18n::Language::Chinese {
        "Hagency 将再次检查所选的本机 Octos 配置，然后在此设备保存未发布的资源。运行时凭据仅在本机临时提供给该 Octos 进程，不会发送到 Hagency 服务器；支持本地模型和自定义端点。"
    } else {
        "Hagency will check the selected local Octos profile again, then save an unpublished resource on this device. Credentials are provided temporarily to that Octos process on this computer, not to the Hagency server. Local models and custom endpoints are supported."
    }
}
pub(crate) fn octos_limit_text(language: crate::i18n::Language) -> &'static str {
    if language == crate::i18n::Language::Chinese {
        "每个 Octos 配置有独立的每月 token 上限；该上限由你设置，并非服务商余额。登记不会发布额度或启动 Agent。"
    } else {
        "Each Octos profile has its own monthly token limit, set by you rather than read from a provider balance. Enrollment does not publish capacity or start an agent."
    }
}
/// What the review says it will do for a Claude Code resource. The Codex
/// wording ends "Credentials stay with Codex", which is not who keeps this one.
pub(crate) fn claude_review_text(language: crate::i18n::Language) -> &'static str {
    if language == crate::i18n::Language::Chinese {
        "Hagency 将再次验证本机的 Claude Code 登录与模型，然后在此设备保存未发布的资源。登录凭据由 Claude Code 保管。"
    } else {
        "Hagency will check this computer’s Claude Code sign-in and the model again, then save an unpublished resource on this device. The sign-in stays with Claude Code."
    }
}
/// Both languages are here, not in the catalog files, so the line reads the
/// same whichever source tree the app is started from.
fn claude_settings_text(language: crate::i18n::Language) -> &'static str {
    if language == crate::i18n::Language::Chinese {
        "本机的 Claude Code 登录"
    } else {
        "this computer’s own sign-in"
    }
}
fn enrollment_details(input: &::hagency::native_owner::NamespaceEnrollment) -> String {
    // The owner's own Claude Code on this computer, not a sign-in folder:
    // the pool runs on whichever sign-in that configuration has.
    let profile=octos_profile(input);
    let account = if let Some(profile)=profile {
        format!("Octos · {profile}")
    } else if input.account.account_id == CLAUDE_ACCOUNT {
        format!("Claude Code · {}", claude_settings_text(crate::i18n::language()))
    } else {
        format!("Codex · {}", input.account.account_id)
    };
    let reasoning=match (profile,input.reasoning.as_deref()) {
        (Some(_),None | Some("none"))=>crate::i18n::tr("Use profile settings"),
        (_,value)=>value.unwrap_or("—"),
    };
    format!("{account}\n{} · {}\n{}: {}", input.model,
        reasoning, crate::i18n::tr("Declared monthly token limit"),
        crate::utils::format_token_count(input.monthly_tokens))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enrollment_review_uses_millions_without_changing_the_request() {
        let input: ::hagency::native_owner::NamespaceEnrollment = parse(json!({
            "commandId":"original", "account":{"accountId":"account_selected", "expectedRevision":"a".repeat(64)},
            "model":"gpt-5.6-sol", "reasoning":"medium", "monthlyTokens":123456
        })).unwrap();
        input.validate().unwrap();
        let details = enrollment_details(&input);
        assert!(details.contains("0.123456 M"), "{details}");
        assert!(!details.contains(": 123456"));
        assert_eq!(input.monthly_tokens, 123456);
    }
    #[test]
    fn enrollment_review_names_the_claude_code_settings_not_an_account_folder() {
        let input: ::hagency::native_owner::NamespaceEnrollment = parse(json!({
            "commandId":"original", "account":{"accountId":"account_claude_code", "expectedRevision":"a".repeat(64)},
            "model":"open-model-a", "reasoning":"medium", "monthlyTokens":1000000
        })).unwrap();
        input.validate().unwrap();
        let details = enrollment_details(&input);
        assert!(details.starts_with("Claude Code · "), "{details}");
        assert!(!details.contains("account_claude_code") && !details.contains("Codex"), "{details}");
        assert!(details.contains("open-model-a · medium"), "{details}");
        // The Chinese UI gets a Chinese line, not the English one.
        let chinese = claude_settings_text(crate::i18n::Language::Chinese);
        assert!(chinese.contains("本机") && !chinese.contains("sign-in"), "{chinese}");
        // Neither language of the sheet's body says Codex keeps this sign-in.
        for language in [crate::i18n::Language::Chinese, crate::i18n::Language::English] {
            let body = claude_review_text(language);
            assert!(body.contains("Claude Code") && !body.contains("Codex"), "{body}");
        }
        assert!(claude_review_text(crate::i18n::Language::Chinese).contains("保管"));
        assert!(details.contains(claude_settings_text(crate::i18n::Language::English)), "{details}");
    }
    #[test]
    fn octos_review_names_the_profile_and_its_independent_declared_limit() {
        let input: ::hagency::native_owner::NamespaceEnrollment = parse(json!({
            "commandId":"original", "account":{"accountId":"account_octos_coding", "expectedRevision":"a".repeat(64)},
            "model":"local-model", "reasoning":"none", "monthlyTokens":1000000
        })).unwrap();
        input.validate().unwrap();
        if cfg!(target_os="macos") { assert!(ensure_enrollment_available(&input).is_ok()); }
        else { assert_eq!(ensure_enrollment_available(&input).unwrap_err(),"octos_pool_platform_unsupported"); }
        let details=enrollment_details(&input);
        assert!(details.starts_with("Octos · coding\nlocal-model · "),"{details}");
        assert!(details.contains(crate::i18n::tr("Use profile settings")),"{details}");
        assert!(!details.contains("Codex") && !details.contains("Claude"),"{details}");
        for language in [crate::i18n::Language::Chinese, crate::i18n::Language::English] {
            let body=octos_review_text(language);
            let limit=octos_limit_text(language);
            assert!(body.contains("Octos") && !body.contains("Codex") && !body.contains("Claude"),"{body}");
            assert!(limit.contains(if language==crate::i18n::Language::Chinese {"独立"} else {"own monthly"}),"{limit}");
        }
        for id in ["account_octos_", "account_octos_../escape", "account_octos_-invalid"] {
            let mut bad=json!({"commandId":"original", "account":{"accountId":id,"expectedRevision":"a".repeat(64)}, "model":"local-model", "reasoning":"none", "monthlyTokens":1000000});
            // Whether the public command accepts this ID or not, it can never
            // masquerade as a valid Octos profile in the native review.
            if let Ok(value)=parse::<::hagency::native_owner::NamespaceEnrollment>(bad.take()) {
                assert!(octos_profile(&value).is_none(),"{id}");
                assert!(ensure_enrollment_available(&value).is_err(),"{id}");
            }
        }
    }
    #[test]
    fn enrollment_input_excludes_identity_paths_and_approval_claims(){
        let valid=json!({"commandId":"original","account":{"accountId":"account_selected","expectedRevision":"a".repeat(64)},"model":"gpt-5.6-sol","reasoning":"medium","monthlyTokens":1000});
        let input: ::hagency::native_owner::NamespaceEnrollment=parse(valid.clone()).unwrap();input.validate().unwrap();
        for key in ["path","provider","quotaAccount","seatId","approved","token","url","executable","profile","owner"]{
            let mut bad=valid.clone();bad[key]=json!("forged");assert!(parse::<::hagency::native_owner::NamespaceEnrollment>(bad).is_err());
        }
        let authority=rinx_miniapp_core::SessionAuthority::default();
        let lease=authority.issue(InstanceId{app:octosense_app_contract::hagency::APP_ID.into(),account:"@owner:test".into(),room:None,generation:1},[ENROLL.to_owned()].into_iter().collect(),Default::default(),Instant::now()+Duration::from_secs(60));
        assert_eq!(permissions(&lease),vec![ManagementPermission::EnrollResources]);
        assert!(octosense_app_contract::hagency::SERVICES.contains(&ENROLL));
        assert!(octosense_app_contract::KNOWN_CAPABILITIES.contains(&ENROLL));
        assert!(octosense_app_contract::hagency::words(ENROLL).is_some());
    }
}
