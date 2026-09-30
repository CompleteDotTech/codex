//! Agents overview restores local drafts while honoring resumed server settings.

use super::session_lifecycle_requests::HistoryCapabilities;
use super::session_lifecycle_requests::recorded_params;
use super::session_lifecycle_requests::start_recording_app_server_with_history;
use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

enum PendingOverviewSettings {
    Current,
    Stale,
}

#[tokio::test]
async fn read_only_overview_refreshes_server_settings_without_replaying_saved_draft() -> Result<()>
{
    let mut app = make_test_app().await;
    std::fs::write(
        app.config.codex_home.join("config.toml"),
        "[tui]\nresume_cwd = \"current\"\n",
    )?;
    for cwd in [test_path_buf("/"), app.config.cwd.to_path_buf()] {
        crate::legacy_core::config::set_project_trust_level(
            app.config.codex_home.as_path(),
            &cwd,
            codex_protocol::config_types::TrustLevel::Trusted,
        )
        .map_err(std::io::Error::other)?;
    }
    let projects = json!({
        app.config.cwd.display().to_string(): {"trust_level": "trusted"},
    });
    app.cli_kv_overrides.push((
        "projects".into(),
        TomlValue::try_from(projects).expect("trust fixture"),
    ));
    app.config.active_project.trust_level = Some(codex_protocol::config_types::TrustLevel::Trusted);
    let mut owner = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let started = owner.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    owner
        .thread_inject_items(
            thread_id,
            vec![serde_json::from_value(json!({
                "type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "writer-owned history"}]
            }))?],
        )
        .await?;

    let mut saved_plan = app.chat_widget.effective_collaboration_mode();
    saved_plan.mode = ModeKind::Plan;
    saved_plan.settings.model = "gpt-5.3".to_string();
    saved_plan.settings.reasoning_effort = Some(ReasoningEffortConfig::Low);
    app.chat_widget.set_effective_collaboration_mode(saved_plan);
    app.chat_widget
        .apply_external_edit("retained read-only draft".to_string());
    let mut input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("saved draft");
    input_state.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::Low);
    assert!(
        owner
            .thread_settings_update(codex_app_server_protocol::ThreadSettingsUpdateParams {
                thread_id: thread_id.to_string(),
                model: Some("gpt-5.3".to_string()),
                effort: Some(ReasoningEffortConfig::Low),
                ..Default::default()
            })
            .await?
    );
    input_state.pending_thread_settings =
        Some(next_thread_settings_updated(&mut owner, thread_id).await);
    assert!(
        owner
            .thread_settings_update(codex_app_server_protocol::ThreadSettingsUpdateParams {
                thread_id: thread_id.to_string(),
                model: Some("gpt-5.4".to_string()),
                effort: Some(ReasoningEffortConfig::High),
                ..Default::default()
            })
            .await?
    );
    next_thread_settings_updated(&mut owner, thread_id).await;
    app.agents_overview
        .input_states
        .insert(thread_id, input_state);
    let (mut server, requests, proxy) = start_recording_app_server_with_history(
        &app.config,
        HistoryCapabilities::Current,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
        crate::app_server_session::ThreadParamsMode::Embedded,
        LoaderOverrides::default(),
    )
    .await?;
    app.app_server_target = AppServerTarget::LocalDaemon {
        allow_embedded_fallback: true,
        endpoint: crate::resolve_remote_addr("ws://127.0.0.1:4500")?,
    };
    let mut tui = crate::tui::test_support::make_test_tui()?;
    requests.lock().expect("request recorder lock").clear();
    Box::pin(app.select_agents_overview_thread(&mut tui, &mut server, thread_id)).await?;

    assert_eq!(app.current_displayed_thread_id(), Some(thread_id));
    assert!(app.chat_widget.is_external_writer_view());
    assert_eq!(
        app.chat_widget.active_collaboration_mode_kind(),
        ModeKind::Plan
    );
    assert_eq!(app.chat_widget.current_model(), "gpt-5.4");
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        app.chat_widget
            .capture_thread_input_state()
            .expect("restored input")
            .plan_mode_reasoning_effort,
        Some(ReasoningEffortConfig::Low)
    );
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "retained read-only draft"
    );
    assert_eq!(recorded_params(&requests, "thread/resume").len(), 1);
    assert!(!recorded_params(&requests, "thread/read").is_empty());
    assert!(recorded_params(&requests, "turn/start").is_empty());
    assert!(recorded_params(&requests, "thread/settings/update").is_empty());
    insta::assert_snapshot!(
        "read_only_overview_preserves_draft_and_plan_with_server_settings",
        crate::chatwidget::tests::helpers::normalize_snapshot_paths(render_bottom_popup(
            &app.chat_widget,
            /*width*/ 80,
        ))
    );
    owner.shutdown().await?;
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}

async fn restore_overview_draft_after_blank_cache_is_lost(
    pending: PendingOverviewSettings,
) -> Result<()> {
    let mut app = make_test_app().await;
    let projects = json!({
        app.config.cwd.display().to_string(): {"trust_level": "trusted"},
    });
    app.cli_kv_overrides.push((
        "projects".into(),
        TomlValue::try_from(projects).expect("trust fixture"),
    ));
    app.config.active_project.trust_level = Some(codex_protocol::config_types::TrustLevel::Trusted);
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let started = server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.retain_blank_session(&mut server, started).await;

    app.chat_widget
        .apply_external_edit("retained draft".to_string());
    let mut input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("saved draft");
    input_state.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::Low);
    let original_mode = app.chat_widget.effective_collaboration_mode();
    let mut plan_mode = original_mode.clone();
    plan_mode.mode = ModeKind::Plan;
    plan_mode.settings.model = "gpt-5.4".to_string();
    plan_mode.settings.reasoning_effort = Some(ReasoningEffortConfig::High);
    assert!(
        server
            .thread_settings_update(codex_app_server_protocol::ThreadSettingsUpdateParams {
                thread_id: thread_id.to_string(),
                model: Some("gpt-5.4".to_string()),
                effort: Some(ReasoningEffortConfig::High),
                collaboration_mode: Some(plan_mode),
                ..Default::default()
            })
            .await?
    );
    let notification = next_thread_settings_updated(&mut server, thread_id).await;
    input_state.pending_thread_settings = Some(notification.clone());
    let (expected_model, expected_mode) = match pending {
        PendingOverviewSettings::Current => (
            notification.thread_settings.model.clone(),
            notification.thread_settings.collaboration_mode.clone(),
        ),
        PendingOverviewSettings::Stale => {
            let mut newer_mode = original_mode;
            newer_mode.settings.model = "gpt-5.3".to_string();
            newer_mode.settings.reasoning_effort = Some(ReasoningEffortConfig::Medium);
            assert!(
                server
                    .thread_settings_update(codex_app_server_protocol::ThreadSettingsUpdateParams {
                        thread_id: thread_id.to_string(),
                        model: Some("gpt-5.3".to_string()),
                        effort: Some(ReasoningEffortConfig::Medium),
                        collaboration_mode: Some(newer_mode),
                        ..Default::default()
                    })
                    .await?
            );
            let newer = next_thread_settings_updated(&mut server, thread_id).await;
            (
                newer.thread_settings.model,
                newer.thread_settings.collaboration_mode,
            )
        }
    };
    app.agents_overview
        .input_states
        .insert(thread_id, input_state);

    server
        .thread_inject_items(
            thread_id,
            vec![serde_json::from_value(json!({
                "type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "saved history"}]
            }))?],
        )
        .await?;
    // Reconnecting drops the live blank-session cache but keeps saved input.
    app.agents_overview.blank_sessions.clear();
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.select_agents_overview_thread(&mut tui, &mut server, thread_id)
        .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(thread_id));
    assert_eq!(app.chat_widget.current_model(), expected_model);
    assert_eq!(
        app.chat_widget.effective_collaboration_mode(),
        expected_mode
    );
    assert_eq!(
        app.chat_widget
            .capture_thread_input_state()
            .expect("restored input")
            .plan_mode_reasoning_effort,
        None
    );
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "retained draft"
    );
    if matches!(pending, PendingOverviewSettings::Stale) {
        insta::assert_snapshot!(
            "resumed_overview_prefers_server_settings_to_stale_draft",
            crate::chatwidget::tests::helpers::normalize_snapshot_paths(render_bottom_popup(
                &app.chat_widget,
                /*width*/ 80,
            ))
        );
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn pending_settings_override_saved_draft_after_blank_cache_is_lost() -> Result<()> {
    restore_overview_draft_after_blank_cache_is_lost(PendingOverviewSettings::Current).await
}

#[tokio::test]
async fn resumed_overview_prefers_server_settings_to_stale_pending_notification() -> Result<()> {
    restore_overview_draft_after_blank_cache_is_lost(PendingOverviewSettings::Stale).await
}

#[tokio::test]
async fn overview_preserves_saved_plan_when_older_server_omits_collaboration_mode() -> Result<()> {
    let mut app = make_test_app().await;
    let projects = json!({
        app.config.cwd.display().to_string(): {"trust_level": "trusted"},
    });
    app.cli_kv_overrides.push((
        "projects".into(),
        TomlValue::try_from(projects).expect("trust fixture"),
    ));
    app.config.active_project.trust_level = Some(codex_protocol::config_types::TrustLevel::Trusted);
    let (mut server, _requests, proxy) = start_recording_app_server_with_history(
        &app.config,
        HistoryCapabilities::ResumeWithoutCollaborationMode,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
        crate::app_server_session::ThreadParamsMode::Embedded,
        LoaderOverrides::default(),
    )
    .await?;
    let started = server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.retain_blank_session(&mut server, started).await;

    let mut saved_plan = app.chat_widget.effective_collaboration_mode();
    saved_plan.mode = ModeKind::Plan;
    app.chat_widget.set_effective_collaboration_mode(saved_plan);
    app.chat_widget
        .apply_external_edit("retained legacy draft".to_string());
    let mut input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("saved draft");
    input_state.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::Low);
    assert!(
        server
            .thread_settings_update(codex_app_server_protocol::ThreadSettingsUpdateParams {
                thread_id: thread_id.to_string(),
                model: Some("gpt-5.4".to_string()),
                effort: Some(ReasoningEffortConfig::High),
                ..Default::default()
            })
            .await?
    );
    input_state.pending_thread_settings =
        Some(next_thread_settings_updated(&mut server, thread_id).await);
    app.agents_overview
        .input_states
        .insert(thread_id, input_state);
    server
        .thread_inject_items(
            thread_id,
            vec![serde_json::from_value(json!({
                "type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "saved history"}]
            }))?],
        )
        .await?;
    app.agents_overview.blank_sessions.clear();
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.select_agents_overview_thread(&mut tui, &mut server, thread_id)
        .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(thread_id));
    assert_eq!(
        app.chat_widget.active_collaboration_mode_kind(),
        ModeKind::Plan
    );
    assert_eq!(app.chat_widget.current_model(), "gpt-5.4");
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        app.chat_widget
            .capture_thread_input_state()
            .expect("restored input")
            .plan_mode_reasoning_effort,
        Some(ReasoningEffortConfig::Low)
    );
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "retained legacy draft"
    );
    server.shutdown().await?;
    proxy.abort();
    Ok(())
}
