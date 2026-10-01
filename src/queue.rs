use crate::{
    qq::{render_notification, MessageSink, SendError},
    store::Store,
};
use anyhow::Result;
use chrono::{Duration, Utc};
use tokio::time::{sleep, Duration as TokioDuration};

pub async fn deliver_pending<S: MessageSink>(store: &Store, sink: &S) -> Result<usize> {
    let target = match store.get_bound_user()? {
        Some(value) => value,
        None => return Ok(0),
    };
    let pending = store.pending_notifications(Utc::now())?;
    let mut delivered = 0;
    for (index, issue) in pending.into_iter().enumerate() {
        if index > 0 {
            sleep(TokioDuration::from_secs(3)).await;
        }
        match sink.send(&target, &render_notification(&issue)).await {
            Ok(()) => {
                store.mark_sent(&issue)?;
                delivered += 1;
            }
            Err(SendError::Retryable(error)) => {
                store.mark_retry(&issue, Utc::now() + Duration::seconds(30), &error)?;
            }
            Err(SendError::Permanent(error)) => {
                store.mark_permanent_failure(&issue, &error)?;
            }
        }
    }
    Ok(delivered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::IssueNotification, qq::SendError};
    use async_trait::async_trait;
    use chrono::Utc;
    struct Fake {
        result: std::sync::Mutex<Vec<std::result::Result<(), SendError>>>,
    }
    #[async_trait]
    impl MessageSink for Fake {
        async fn send(&self, _: &str, _: &str) -> std::result::Result<(), SendError> {
            self.result.lock().unwrap().pop().unwrap_or(Ok(()))
        }
    }
    fn issue(n: u64) -> IssueNotification {
        IssueNotification {
            repository: "o/r".into(),
            number: n,
            title: "title".into(),
            author: "a".into(),
            created_at: Utc::now(),
            url: "https://example.com".into(),
        }
    }
    #[tokio::test]
    async fn sends_and_persists_results() {
        let store = Store::open_in_memory().unwrap();
        store.ensure_repository("o/r", Utc::now()).unwrap();
        store.bind_user("u").unwrap();
        store.insert_notification(&issue(1)).unwrap();
        let fake = Fake {
            result: std::sync::Mutex::new(vec![Ok(())]),
        };
        assert_eq!(deliver_pending(&store, &fake).await.unwrap(), 1);
        assert!(store.pending_notifications(Utc::now()).unwrap().is_empty());
    }
}
