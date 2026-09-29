//! Retains blank task subscriptions until their empty threads are durable and resumable.

use super::*;

const MAX_RETAINED_BLANK_SESSIONS: usize = 8;

impl App {
    pub(in crate::app) async fn finish_blank_session_attachment<T>(
        &mut self,
        app_server: &mut AppServerSession,
        started: &crate::app_server_session::AppServerStartedThread,
        result: Result<T>,
    ) -> Result<T> {
        if result.is_err() {
            let thread_id = started.session.thread_id;
            self.agents_overview.blank_sessions.remove(&thread_id);
            self.agents_overview
                .blank_session_order
                .retain(|id| *id != thread_id);
            let _ = app_server.thread_unsubscribe(thread_id).await;
            if started.persisted_on_start {
                let _ = app_server.thread_archive(thread_id).await;
            }
        }
        result
    }

    pub(in crate::app) async fn retain_blank_session(
        &mut self,
        app_server: &mut AppServerSession,
        started: crate::app_server_session::AppServerStartedThread,
    ) {
        let thread_id = started.session.thread_id;
        self.agents_overview
            .blank_session_order
            .retain(|id| self.agents_overview.blank_sessions.contains_key(id));
        if self
            .agents_overview
            .blank_sessions
            .insert(thread_id, started)
            .is_none()
        {
            self.agents_overview
                .blank_session_order
                .push_back(thread_id);
        }
        let current = self.current_displayed_thread_id();
        let voice_owner = self.voice_owner_thread_id();
        while self.agents_overview.blank_sessions.len() > MAX_RETAINED_BLANK_SESSIONS {
            let Some(index) = self
                .agents_overview
                .blank_session_order
                .iter()
                .position(|id| {
                    *id != thread_id
                        && Some(*id) != current
                        && Some(*id) != voice_owner
                        && self
                            .agents_overview
                            .blank_sessions
                            .get(id)
                            .is_some_and(|blank| blank.persisted_on_start)
                })
            else {
                break;
            };
            let Some(evicted) = self.agents_overview.blank_session_order.remove(index) else {
                break;
            };
            self.agents_overview.blank_sessions.remove(&evicted);
            if let Err(error) = app_server.thread_unsubscribe(evicted).await {
                tracing::warn!(%evicted, %error, "failed to unsubscribe superseded blank session");
            }
            self.abort_thread_event_listener(evicted);
            self.thread_event_channels.remove(&evicted);
            self.pending_server_profiles.remove(&evicted);
        }
    }
}

#[cfg(test)]
#[path = "agents_overview_retention_tests.rs"]
mod tests;
