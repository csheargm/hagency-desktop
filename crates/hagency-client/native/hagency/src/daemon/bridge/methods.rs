//! Closed native-management operation table. Shared by facade and dispatch.
macro_rules! management_methods {
    ($m:ident) => { $m! {
        account_models(input: AccountModelQuery) -> Value; false;
        #[cfg(feature="resource-adoption")]
        operator_graph_page(query: OperatorGraphQuery) -> OperatorGraphPage; false;
        #[cfg(feature="resource-adoption")]
        operator_graph_detail(query: OperatorGraphDetailQuery) -> OperatorGraphDetail; false;
        #[cfg(feature="resource-adoption")]
        operator_graph_node(query: OperatorGraphNodeQuery) -> OperatorGraphNode; false;
        pool_agent_room(command_id:String,name:String) -> MatrixCreationReply; true;
        rename_resource(input: RenameResource) -> ResourceNameResult; true;
        resource_alerts(query:ResourceAlertQuery) -> ResourceAlertPage; false;
        resource_alert(query:ResourceAlertDetailQuery) -> ResourceAlertDetail; false;
        triage_resource_alert(input:TriageResourceAlert) -> ResourceAlert; true;
        saved_commands() -> Vec<PendingIntent>; false;
        acknowledge_command(command_id: String) -> (); true;
        retry_saved_command(command_id: String) -> Value; true;
        stopped_tasks(query: StoppedListQuery) -> StoppedPage; false;
        stopped_inspection(query: StoppedQuery) -> StoppedInspection; false;
        review_stopped(input: ReviewStopped) -> StoppedReview; false;
        resolve_stopped(input: ResolveStopped) -> StoppedResolution; true;
        interrupted_calls(query: InterruptedCallQuery) -> InterruptedCallPage; false;
        reconcile_interrupted_call(input: ReconcileInterruptedCall) -> InterruptedCallReconciliation; true;
        web_catalog(query:WebCatalogQuery) -> WebCatalogView; false;
        web_offers(resource_id:String) -> WebOfferPage; false;
        web_trust(query:WebCatalogQuery) -> WebTrustPage; false;
        prepare_web_catalog(input:WebCatalogPrepare) -> PreparedWebCatalog; true;
        review_web_catalog_command(id:String) -> PreparedWebCatalog; false;
        cancel_saved_web_catalog(id:String) -> PreparedWebCatalog; true;
        apply_saved_web_catalog(id:String) -> WebCatalogResult; true;
        save_personal_policy(input: SavePersonalPolicy) -> Value; true;
        operator_task_page(input: service::OperatorTaskQuery) -> service::OperatorTaskPage; false;
        operator_task_detail(input: service::OperatorTaskDetailQuery) -> service::OperatorTaskDetail; false;
        operator_board(input: service::OperatorBoardQuery) -> service::OperatorBoardPage; false;
        review_resource_configuration(input: &CreateResourceConfiguration) -> ResourceConfigurationReview; false;
        resource_workspaces(query: WorkspaceListQuery) -> WorkspacePage; false;
        resource_workspace(query: WorkspaceQuery) -> WorkspaceStatus; false;
        clear_workspace_dirty(input: ClearWorkspaceDirty) -> WorkspaceRelease; true;
        web_alerts(query: WebAlertsQuery) -> WebAlertsPage; false;
        web_capability(query: WebOverviewQuery) -> Value; false;
        web_terminal(query: WebTerminalQuery) -> Value; false;
        #[cfg(feature="resource-adoption")]
        prepare_operator_graph_create(input: OperatorGraphCreate) -> PreparedOperatorGraph; true;
        #[cfg(feature="resource-adoption")]
        prepare_operator_graph_update(input: OperatorGraphUpdate) -> PreparedOperatorGraph; true;
        #[cfg(feature="resource-adoption")]
        prepare_operator_graph_cancel(input: OperatorGraphCancel) -> PreparedOperatorGraph; true;
        #[cfg(feature="resource-adoption")]
        review_saved_operator_graph(command_id: String) -> PreparedOperatorGraph; false;
        #[cfg(feature="resource-adoption")]
        apply_saved_operator_graph(command_id: String) -> OperatorGraphResult; true;
        probe_web_connection(input: WebConnectionProbeInput) -> WebConnectionProbe; true;
        agent_execution_policies(query: ExecutionPolicyListQuery) -> AgentExecutionPolicyPage; false;
        agent_execution_policy(query: ExecutionPolicyQuery) -> AgentExecutionPolicy; false;
        edit_execution_policy(input: EditExecutionPolicy) -> AgentExecutionPolicy; true;
        resource_templates(resource_id: String, after: Option<String>) -> ResourceTemplatePage; false;
        resource_template(resource_id: String, id: String) -> ResourceTemplateDetail; false;
        prepare_template_create(input: TemplateCreateInput) -> PreparedResourceTemplate; true;
        prepare_template_update(input: TemplateUpdateInput) -> PreparedResourceTemplate; true;
        prepare_template_delete(input: TemplateDeleteInput) -> PreparedResourceTemplate; true;
        review_template_command(command_id: String) -> PreparedResourceTemplate; false;
        cancel_saved_template(command_id: String) -> PreparedResourceTemplate; true;
        apply_saved_template(command_id: String) -> ResourceTemplateResult; true;
        resource_approval_bindings(query: ResourceApprovalBindingQuery) -> ResourceApprovalBindingPage; false;
        retire_resource_approval_binding(input: RetireResourceApprovalBinding) -> RetiredResourceApprovalBinding; true;
        prepare_operator_task(change: OperatorTaskChange) -> PreparedOperatorTask; true;
        review_saved_operator_task(command_id: String) -> PreparedOperatorTask; false;
        apply_saved_operator_task(command_id: String) -> OperatorTaskResult; true;
        prepare_account(input: PrepareAccount) -> AccountPreparation; true;
        personal_agents(query: PersonalAgentQuery) -> Value; false;
        personal_agent_settings(id: String) -> Value; false;
        tool_approvals(query: ToolApprovalQuery) -> ToolApprovalPage; false;
        tool_approval(query: ToolApprovalLookup) -> ToolApprovalSnapshot; false;
        tool_grants(query: ToolGrantQuery) -> ToolGrantPage; false;
        revoke_tool_grant(input: RevokeToolGrant) -> RevokedToolGrant; true;
        resources(query: service::ResourceQuery) -> service::ResourcePage; false;
        account_inventory(query: service::AccountInventoryQuery) -> service::AccountInventory; false;
        resource_detail(query: service::ResourceDetailQuery) -> service::ResourceDetail; false;
        publish_resource(input: service::PublishResource) -> hagency_store::ResourcePublicationResult; true;
        central(command: ManagementCommand) -> Value; true;
        observe_personal_settings(input: ObservePersonalSettings) -> Value; true;
        personal_model_choices(agent_id: String) -> PersonalModelChoices; false;
        review_local_resource_edit(input: &EditLocalResource) -> LocalResourceEditReview; false;
        #[cfg(feature="resource-adoption")]
        operator_graph_recipient_observe(input: OperatorGraphRecipientObserve) -> Value; true;
        #[cfg(feature="resource-adoption")]
        prepare_operator_graph_bind_recipient(input: OperatorGraphBindRecipient) -> PreparedOperatorGraph; true;
        #[cfg(feature="resource-adoption")]
        operator_graph_recipient_get(query: OperatorGraphNodeQuery) -> Value; false;
        #[cfg(feature="resource-adoption")]
        operator_graph_recipient_options(query: OperatorGraphRecipientOptionsQuery) -> Value; false;
        agent_budget(id: String) -> Value; false;
        agent_status(id: String) -> AgentStatus; false;
        web_account_capacity(query: WebAccountCapacityQuery) -> Value; false;
        web_workforce(query: WebOverviewQuery) -> Value; false;
        web_usage(query: WebOverviewQuery) -> Value; false;
        web_connections(after: Option<String>) -> WebConnectionPage; false;
        web_connection(id: String) -> WebConnection; false;
        web_connection_result(command_id: String) -> Option<WebConnectionResult>; false;
        verify_web_connection(command_id: String, id: String, expected_revision: u64) -> WebConnectionResult; true;
        login_account(input: AccountLoginRequest) -> AccountLoginStatus; true;
        account_login_status(input: AccountLoginRequest) -> AccountLoginStatus; false;
        cancel_account_login(input: AccountLoginRequest) -> AccountLoginStatus; true;
    }};
}
pub(crate) use management_methods;
