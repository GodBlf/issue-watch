use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitoredRepository {
    pub name: String,
    pub baseline: Option<DateTime<Utc>>,
    pub cursor: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueNotification {
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationState {
    Pending,
    Sent,
    Retry,
    PermanentFailure,
}

impl NotificationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Retry => "retry",
            Self::PermanentFailure => "permanent_failure",
        }
    }
}
