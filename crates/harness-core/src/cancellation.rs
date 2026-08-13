use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct TurnCancellation {
    sender: watch::Sender<bool>,
    receiver: watch::Receiver<bool>,
}

impl Default for TurnCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnCancellation {
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel(false);
        Self { sender, receiver }
    }

    pub fn cancel(&self) {
        let _ = self.sender.send(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    pub fn receiver(&self) -> watch::Receiver<bool> {
        self.receiver.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;

    use crate::agent::{AgentLoop, TurnOutcome};
    use crate::events::EventKind;
    use crate::llm::{LlmAdapter, LlmRequest, StreamFrame};
    use crate::session::SessionLog;
    use crate::tools::ToolRegistry;
    use uuid::Uuid;

    #[derive(Default)]
    struct NeverAdapter;

    #[async_trait]
    impl LlmAdapter for NeverAdapter {
        async fn stream(
            &self,
            _request: LlmRequest,
        ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamFrame>> {
            let (_sender, receiver) = tokio::sync::mpsc::channel(1);
            std::mem::forget(_sender);
            Ok(receiver)
        }
    }

    #[tokio::test]
    async fn precancelled_turns_close_without_model_request() {
        let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));
        let cancellation = super::TurnCancellation::new();
        cancellation.cancel();
        let agent = AgentLoop::new(Arc::new(NeverAdapter), Arc::new(ToolRegistry::new()));

        let outcome = agent
            .run_turn_with_cancellation(log.clone(), "stop".into(), cancellation)
            .await
            .unwrap();

        assert_eq!(outcome, TurnOutcome::Cancelled);
        assert!(log.events().iter().any(|event| matches!(
            &event.kind,
            EventKind::TurnCompleted { reason }
                if matches!(reason, crate::events::TurnCompletionReason::Cancelled)
        )));
        assert!(!log
            .events()
            .iter()
            .any(|event| matches!(&event.kind, EventKind::StepStarted { .. })));
    }

    #[tokio::test]
    async fn running_turn_cancels_while_waiting_for_model_stream() {
        let log = Arc::new(SessionLog::in_memory(Uuid::new_v4()));
        let cancellation = super::TurnCancellation::new();
        let agent = AgentLoop::new(Arc::new(NeverAdapter), Arc::new(ToolRegistry::new()));
        let task = {
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                agent
                    .run_turn_with_cancellation(log, "wait".into(), cancellation)
                    .await
                    .unwrap()
            })
        };

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!task.is_finished());
        cancellation.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome, TurnOutcome::Cancelled);
    }
}
