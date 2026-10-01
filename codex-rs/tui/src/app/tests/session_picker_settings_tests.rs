//! Resume-picker restoration must not overwrite authoritative server settings.

use super::session_lifecycle_requests::HistoryCapabilities;
use super::session_lifecycle_requests::start_recording_app_server_with_history;
use super::*;
use crate::resume_picker::SessionSelection;
use crate::resume_picker::SessionTarget;
use pretty_assertions::assert_eq;
use serde_json::json;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsScenario {
    None,
    Pending,
    StalePending,
    LegacyWithoutMode,
}

async fn resume_saved_draft(scenario: SettingsScenario) -> Result<()> {
    let mut app = make_test_app().await;
    let projects = json!({
        app.config.cwd.display().to_string(): {"trust_level": "trusted"},
    });
    app.cli_kv_overrides.push((
        "projects".into(),
        TomlValue::try_from(projects).expect("trust fixture"),
    ));
    app.config.active_project.trust_level = Some(codex_protocol::config_types::TrustLevel::Trusted);
    let history_capabilities = if scenario == SettingsScenario::LegacyWithoutMode {
        HistoryCapabilities::ResumeWithoutCollaborationMode
    } else {
        HistoryCapabilities::Current
    };
    let (mut server, _requests, proxy) = start_recording_app_server_with_history(
        &app.config,
        history_capabilities,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
        crate::app_server_session::ThreadParamsMode::Embedded,
        LoaderOverrides::default(),
    )
    .await?;
    let started = server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.agents_overview
        .blank_sessions
        .insert(thread_id, started);

    if scenario == SettingsScenario::LegacyWithoutMode {
        let mut plan = app.chat_widget.effective_collaboration_mode();
        plan.mode = ModeKind::Plan;
        app.chat_widget.set_effective_collaboration_mode(plan);
    }

    app.chat_widget
        .apply_external_edit("retained resume-picker draft".to_string());
    let mut input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("saved draft");
    input_state.plan_mode_reasoning_effort = Some(ReasoningEffortConfig::Low);
    let original_model = app.chat_widget.current_model().to_string();
    let original_mode = app.chat_widget.effective_collaboration_mode();
    let (expected_model, expected_mode, expected_effort) = if matches!(
        scenario,
        SettingsScenario::Pending | SettingsScenario::StalePending
    ) {
        // A different model makes this a regression rather than a no-op restore.
        assert_ne!(
            original_model, "gpt-5.4",
            "fixture requires distinct models"
        );
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
        let expected_model = notification.thread_settings.model.clone();
        let expected_mode = notification.thread_settings.collaboration_mode.clone();
        let expected_effort = expected_mode.reasoning_effort();
        input_state.pending_thread_settings = Some(notification);
        if scenario == SettingsScenario::StalePending {
            let mut newer_mode = original_mode.clone();
            newer_mode.settings.model = original_model.clone();
            newer_mode.settings.reasoning_effort = Some(ReasoningEffortConfig::Medium);
            assert!(
                server
                    .thread_settings_update(codex_app_server_protocol::ThreadSettingsUpdateParams {
                        thread_id: thread_id.to_string(),
                        model: Some(original_model.clone()),
                        effort: Some(ReasoningEffortConfig::Medium),
                        collaboration_mode: Some(newer_mode),
                        ..Default::default()
                    })
                    .await?
            );
            let newer = next_thread_settings_updated(&mut server, thread_id).await;
            let mode = newer.thread_settings.collaboration_mode;
            (
                newer.thread_settings.model,
                mode.clone(),
                mode.reasoning_effort(),
            )
        } else {
            (expected_model, expected_mode, expected_effort)
        }
    } else {
        (
            original_model,
            original_mode.clone(),
            original_mode.reasoning_effort(),
        )
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
    // The thread is now resumable; this deliberately uses /resume, not overview.
    app.agents_overview.blank_sessions.clear();
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let control = app
        .apply_resume_picker_selection(
            &mut tui,
            &mut server,
            SessionSelection::Resume(SessionTarget {
                path: None,
                thread_id,
                cwd: None,
                history_mode: None,
            }),
        )
        .await?;

    assert_matches!(control, AppRunControl::Continue);
    assert_eq!(app.chat_widget.thread_id(), Some(thread_id));
    assert_eq!(app.chat_widget.current_model(), expected_model);
    assert_eq!(
        app.chat_widget.active_collaboration_mode_kind(),
        expected_mode.mode
    );
    assert_eq!(app.chat_widget.current_reasoning_effort(), expected_effort);
    assert_eq!(
        app.chat_widget
            .capture_thread_input_state()
            .expect("restored input")
            .plan_mode_reasoning_effort,
        (scenario == SettingsScenario::LegacyWithoutMode).then_some(ReasoningEffortConfig::Low)
    );
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "retained resume-picker draft"
    );
    if scenario == SettingsScenario::Pending {
        insta::assert_snapshot!(
            "resume_picker_preserves_server_settings_and_draft",
            crate::chatwidget::tests::helpers::normalize_snapshot_paths(render_bottom_popup(
                &app.chat_widget,
                /*width*/ 80,
            ))
        );
    } else if scenario == SettingsScenario::LegacyWithoutMode {
        insta::assert_snapshot!(
            "resume_picker_preserves_plan_mode_with_legacy_server",
            crate::chatwidget::tests::helpers::normalize_snapshot_paths(render_bottom_popup(
                &app.chat_widget,
                /*width*/ 80,
            ))
        );
    }
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}

#[tokio::test]
async fn resume_picker_preserves_server_settings_after_restoring_draft() -> Result<()> {
    resume_saved_draft(SettingsScenario::Pending).await
}

#[tokio::test]
async fn resume_picker_preserves_draft_without_pending_settings() -> Result<()> {
    resume_saved_draft(SettingsScenario::None).await
}

#[tokio::test]
async fn resume_picker_preserves_newer_server_settings_when_draft_is_stale() -> Result<()> {
    resume_saved_draft(SettingsScenario::StalePending).await
}

#[tokio::test]
async fn resume_picker_preserves_plan_selection_when_server_omits_mode() -> Result<()> {
    resume_saved_draft(SettingsScenario::LegacyWithoutMode).await
}
