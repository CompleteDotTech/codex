use super::*;
use crate::app::test_support::make_test_app;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadListResponse;
use codex_app_server_protocol::ThreadLoadedListParams;
use codex_app_server_protocol::ThreadUnsubscribeParams;
use codex_app_server_protocol::ThreadUnsubscribeResponse;
use codex_app_server_protocol::ThreadUnsubscribeStatus;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn failed_attachment_archives_new_durable_session_without_losing_previous_draft() -> Result<()>
{
    let mut app = make_test_app().await;
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let previous = server.start_thread(&app.config).await?;
    let previous_id = previous.session.thread_id;
    app.retain_blank_session(&mut server, previous).await;
    app.chat_widget
        .apply_external_edit("previous draft".to_string());
    let failed = server.start_thread(&app.config).await?;
    let failed_id = failed.session.thread_id;
    assert!(failed.persisted_on_start);
    app.retain_blank_session(&mut server, failed.clone()).await;

    // Both attachment entry points pass their fallible terminal result through this boundary.
    // Inject an I/O error without replacing the process-wide stdout used by other tests.
    let attachment: Result<()> = Err(std::io::Error::other("attachment failed").into());
    let error = app
        .finish_blank_session_attachment(&mut server, &failed, attachment)
        .await
        .expect_err("attachment failure must still propagate");
    assert_eq!(error.to_string(), "attachment failed");
    assert_eq!(
        app.agents_overview
            .blank_sessions
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![previous_id]
    );
    assert_eq!(
        app.agents_overview
            .blank_session_order
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![previous_id]
    );
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "previous draft"
    );
    let loaded = server
        .thread_loaded_list(ThreadLoadedListParams {
            cursor: None,
            limit: None,
        })
        .await?;
    assert!(loaded.data.contains(&previous_id.to_string()));
    assert!(!loaded.data.contains(&failed_id.to_string()));
    let archived: ThreadListResponse = server
        .request_handle()
        .request_typed(ClientRequest::ThreadList {
            request_id: RequestId::String("verify-failed-attachment-archive".to_string()),
            params: ThreadListParams {
                originators: None,
                cursor: None,
                limit: Some(100),
                sort_key: None,
                sort_direction: None,
                model_providers: Some(Vec::new()),
                source_kinds: None,
                archived: Some(true),
                section_id: None,
                project_id: None,
                parent_thread_id: None,
                ancestor_thread_id: None,
                cwd: None,
                use_state_db_only: false,
                search_term: None,
            },
        })
        .await?;
    assert!(
        archived
            .data
            .iter()
            .any(|thread| thread.id == failed_id.to_string())
    );
    assert!(
        !archived
            .data
            .iter()
            .any(|thread| thread.id == previous_id.to_string())
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn older_server_rejects_persistence_then_accepts_retry() -> Result<()> {
    let app = make_test_app().await;
    let client = crate::start_embedded_app_server_with(
        codex_arg0::Arg0DispatchPaths::default(),
        app.config.clone(),
        Vec::new(),
        codex_config::LoaderOverrides::without_managed_config_for_tests(),
        /*strict_config*/ false,
        codex_config::CloudConfigBundleLoader::default(),
        codex_feedback::CodexFeedback::new(),
        /*log_db*/ None,
        /*state_db*/ None,
        Arc::clone(&app.environment_manager),
        Default::default(),
        |mut args| {
            args.experimental_api = false;
            codex_app_server_client::InProcessAppServerClient::start(args)
        },
    )
    .await?;
    let server = AppServerSession::new(
        codex_app_server_client::AppServerClient::InProcess(client),
        crate::app_server_session::ThreadParamsMode::Embedded,
    );
    let params = codex_app_server_protocol::ThreadStartParams {
        cwd: Some(app.config.cwd.to_string_lossy().into_owned()),
        persist_on_start: true,
        ..Default::default()
    };
    let (response, _, _) = crate::app_server_session::request_thread_start_with_history_fallback(
        &server.request_handle(),
        RequestId::String("persist-compatibility".to_string()),
        params,
    )
    .await?;
    assert!(!response.persisted_on_start);
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn superseded_blank_sessions_release_subscriptions() -> Result<()> {
    let mut app = make_test_app().await;
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut ids = Vec::new();
    for _ in 0..MAX_RETAINED_BLANK_SESSIONS + 2 {
        let started = server.start_thread(&app.config).await?;
        assert!(started.persisted_on_start);
        ids.push(started.session.thread_id);
        app.agents_overview
            .selected_permission_profiles
            .insert(started.session.thread_id, "retained-profile".to_string());
        app.retain_blank_session(&mut server, started).await;
    }

    assert_eq!(
        app.agents_overview
            .blank_session_order
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        ids[2..].to_vec()
    );
    assert_eq!(
        app.agents_overview.blank_sessions.len(),
        MAX_RETAINED_BLANK_SESSIONS
    );
    for thread_id in &ids[..2] {
        assert_eq!(
            app.agents_overview
                .selected_permission_profiles
                .get(thread_id)
                .map(String::as_str),
            Some("retained-profile")
        );
        let response: ThreadUnsubscribeResponse = server
            .request_handle()
            .request_typed(
                codex_app_server_protocol::ClientRequest::ThreadUnsubscribe {
                    request_id: RequestId::String(format!("verify-{thread_id}")),
                    params: ThreadUnsubscribeParams {
                        thread_id: thread_id.to_string(),
                    },
                },
            )
            .await?;
        assert_eq!(response.status, ThreadUnsubscribeStatus::NotSubscribed);
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn blank_sessions_without_persistence_ack_are_not_evicted() -> Result<()> {
    let mut app = make_test_app().await;
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut ids = Vec::new();
    for _ in 0..MAX_RETAINED_BLANK_SESSIONS + 1 {
        let mut started = server.start_thread(&app.config).await?;
        started.persisted_on_start = false;
        ids.push(started.session.thread_id);
        app.retain_blank_session(&mut server, started).await;
    }

    assert_eq!(app.agents_overview.blank_sessions.len(), ids.len());
    assert_eq!(
        app.agents_overview
            .blank_session_order
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        ids
    );
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn background_voice_owner_remains_subscribed_when_blank_sessions_overflow() -> Result<()> {
    let mut app = make_test_app().await;
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let first = server.start_thread(&app.config).await?;
    let voice_owner_id = first.session.thread_id;
    let (mut owner, _, _, _) = crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
    crate::chatwidget::activate_voice_for_thread(&mut owner, voice_owner_id);
    owner.park_voice();
    app.background_voice = Some(Box::new(owner));
    assert_eq!(app.voice_owner_thread_id(), Some(voice_owner_id));
    app.retain_blank_session(&mut server, first).await;

    let mut other_ids = Vec::new();
    for _ in 0..MAX_RETAINED_BLANK_SESSIONS {
        let started = server.start_thread(&app.config).await?;
        other_ids.push(started.session.thread_id);
        app.retain_blank_session(&mut server, started).await;
    }

    assert!(
        app.agents_overview
            .blank_sessions
            .contains_key(&voice_owner_id)
    );
    assert!(
        !app.agents_overview
            .blank_sessions
            .contains_key(&other_ids[0])
    );
    assert_eq!(
        app.agents_overview.blank_sessions.len(),
        MAX_RETAINED_BLANK_SESSIONS
    );
    server.shutdown().await?;
    Ok(())
}
