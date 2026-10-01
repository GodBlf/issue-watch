use crate::model::{IssueNotification, MonitoredRepository};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub struct PendingDelivery {
    pub subscription_id: i64,
    pub user_openid: String,
    pub issue: IssueNotification,
}

pub struct Store {
    conn: Connection,
    health: Option<crate::health::Health>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("open SQLite database")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let store = Self { conn, health: None };
        store.init()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let store = Self { conn, health: None };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS monitored_repositories (name TEXT PRIMARY KEY, baseline TEXT, cursor TEXT); CREATE TABLE IF NOT EXISTS issue_notifications (repository TEXT NOT NULL, number INTEGER NOT NULL, title TEXT NOT NULL, author TEXT NOT NULL, created_at TEXT NOT NULL, url TEXT NOT NULL, state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, next_attempt_at TEXT, last_error TEXT, PRIMARY KEY(repository, number)); CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
        // Old rows intentionally retain NULL timestamps; Issue creation is not enqueue/send time.
        let mut columns = self
            .conn
            .prepare("PRAGMA table_info(issue_notifications)")?;
        let names = columns
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for name in ["enqueued_at", "sent_at"] {
            if !names.iter().any(|column| column == name) {
                self.conn.execute(
                    &format!("ALTER TABLE issue_notifications ADD COLUMN {name} TEXT"),
                    [],
                )?;
            }
        }
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS broadcast_subscriptions (
            id INTEGER PRIMARY KEY AUTOINCREMENT, user_openid TEXT NOT NULL UNIQUE);
            CREATE TABLE IF NOT EXISTS notification_deliveries (
            subscription_id INTEGER NOT NULL, repository TEXT NOT NULL, number INTEGER NOT NULL,
            state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, next_attempt_at TEXT,
            last_error TEXT, enqueued_at TEXT, sent_at TEXT,
            PRIMARY KEY(subscription_id, repository, number));",
        )?;
        let migrated: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM settings WHERE key='broadcast_schema_version')",
            [],
            |row| row.get(0),
        )?;
        if !migrated {
            self.conn.execute(
                "INSERT OR IGNORE INTO broadcast_subscriptions(user_openid)
                SELECT value FROM settings WHERE key='qq_user_openid'",
                [],
            )?;
            self.conn.execute("INSERT INTO notification_deliveries
                (subscription_id,repository,number,state,attempts,next_attempt_at,last_error,enqueued_at,sent_at)
                SELECT s.id,n.repository,n.number,n.state,n.attempts,n.next_attempt_at,n.last_error,n.enqueued_at,n.sent_at
                FROM issue_notifications n CROSS JOIN broadcast_subscriptions s", [])?;
            self.conn
                .execute("DELETE FROM settings WHERE key='qq_user_openid'", [])?;
            self.conn.execute(
                "INSERT INTO settings(key,value) VALUES('broadcast_schema_version','1')",
                [],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
    pub fn with_health(mut self, health: crate::health::Health) -> Result<Self> {
        self.health = Some(health);
        self.observe_queue()?;
        Ok(self)
    }
    fn observe_queue(&self) -> Result<()> {
        if let Some(health) = &self.health {
            crate::qq_health::QqHealth(health.clone()).refresh_queue(self)?;
        }
        Ok(())
    }

    pub fn ensure_repository(&self, name: &str, now: DateTime<Utc>) -> Result<MonitoredRepository> {
        let existing: Option<MonitoredRepository> = self
            .conn
            .query_row(
                "SELECT name, baseline, cursor FROM monitored_repositories WHERE name = ?1 COLLATE NOCASE",
                [name],
                |row| {
                    Ok(MonitoredRepository {
                        name: row.get(0)?,
                        baseline: parse_dt(row.get(1)?),
                        cursor: parse_dt(row.get(2)?),
                    })
                },
            )
            .optional()?;
        if let Some(repo) = existing {
            return Ok(repo);
        }
        self.conn.execute(
            "INSERT INTO monitored_repositories(name, baseline) VALUES(?1, ?2)",
            params![name, now.to_rfc3339()],
        )?;
        Ok(MonitoredRepository {
            name: name.to_string(),
            baseline: Some(now),
            cursor: None,
        })
    }

    /// Commit all candidate repository baselines together or roll them all back.
    pub fn ensure_repositories(&self, names: &[String], now: DateTime<Utc>) -> Result<Vec<String>> {
        let transaction = self.conn.unchecked_transaction()?;
        let repositories = names
            .iter()
            .map(|name| self.ensure_repository(name, now).map(|repo| repo.name))
            .collect::<Result<Vec<_>>>()?;
        transaction.commit()?;
        Ok(repositories)
    }

    pub fn set_cursor(&self, repository: &str, cursor: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "UPDATE monitored_repositories SET cursor = ?2 WHERE name = ?1",
            params![repository, cursor.to_rfc3339()],
        )?;
        Ok(())
    }
    pub fn get_cursor(&self, repository: &str) -> Result<Option<DateTime<Utc>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT cursor FROM monitored_repositories WHERE name = ?1",
                [repository],
                |row| Ok(parse_dt(row.get::<_, Option<String>>(0)?)),
            )
            .optional()?
            .flatten())
    }

    pub fn insert_notification(&self, issue: &IssueNotification) -> Result<bool> {
        // Serialize the recipient snapshot with bind/unbind on the gateway connection.
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let now = Utc::now().to_rfc3339();
        let changed = self.conn.execute("INSERT OR IGNORE INTO issue_notifications(repository, number, title, author, created_at, url, state, enqueued_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'pending',?7)", params![issue.repository, issue.number, issue.title, issue.author, issue.created_at.to_rfc3339(), issue.url,now])?;
        if changed == 1 {
            self.conn.execute("INSERT INTO notification_deliveries(subscription_id,repository,number,state,enqueued_at)
                SELECT id,?1,?2,'pending',?3 FROM broadcast_subscriptions",
                params![issue.repository,issue.number,now])?;
        }
        transaction.commit()?;
        self.observe_queue()?;
        Ok(changed == 1)
    }

    /// Discovery history is independent of the number of recipients and delivery state.
    pub fn recorded_notifications(&self) -> Result<Vec<IssueNotification>> {
        let mut stmt = self.conn.prepare(
            "SELECT repository,number,title,author,created_at,url
            FROM issue_notifications ORDER BY created_at,repository,number",
        )?;
        let rows = stmt.query_map([], read_issue)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn pending_deliveries(&self, now: DateTime<Utc>) -> Result<Vec<PendingDelivery>> {
        let mut stmt = self.conn.prepare("SELECT n.repository,n.number,n.title,n.author,n.created_at,n.url,s.id,s.user_openid
            FROM notification_deliveries d JOIN issue_notifications n
            ON n.repository=d.repository AND n.number=d.number
            JOIN broadcast_subscriptions s ON s.id=d.subscription_id
            WHERE d.state IN ('pending','retry') AND (d.next_attempt_at IS NULL OR d.next_attempt_at<=?1)
            ORDER BY n.created_at,n.repository,n.number,s.id")?;
        let rows = stmt.query_map([now.to_rfc3339()], |row| {
            Ok(PendingDelivery {
                issue: read_issue(row)?,
                subscription_id: row.get(6)?,
                user_openid: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Recheck after pacing: the recipient may have unsubscribed since the batch was read.
    pub fn delivery_is_pending(&self, delivery: &PendingDelivery) -> Result<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM notification_deliveries d
            JOIN broadcast_subscriptions s ON s.id=d.subscription_id
            WHERE d.subscription_id=?1 AND d.repository=?2 AND d.number=?3 AND d.state IN ('pending','retry'))",
            params![delivery.subscription_id,delivery.issue.repository,delivery.issue.number],
            |row| row.get(0))?)
    }

    pub fn mark_sent(&self, delivery: &PendingDelivery) -> Result<()> {
        self.conn.execute("UPDATE notification_deliveries SET state='sent', next_attempt_at=NULL, last_error=NULL,sent_at=?4
            WHERE subscription_id=?1 AND repository=?2 AND number=?3 AND state IN ('pending','retry')",
            params![delivery.subscription_id,delivery.issue.repository,delivery.issue.number,Utc::now().to_rfc3339()])?;
        self.observe_queue()
    }
    pub fn mark_retry(
        &self,
        delivery: &PendingDelivery,
        next: DateTime<Utc>,
        error: &str,
    ) -> Result<()> {
        self.conn.execute("UPDATE notification_deliveries SET state='retry',attempts=attempts+1,next_attempt_at=?4,last_error=?5
            WHERE subscription_id=?1 AND repository=?2 AND number=?3 AND state IN ('pending','retry')",
            params![delivery.subscription_id,delivery.issue.repository,delivery.issue.number,next.to_rfc3339(),error])?;
        self.observe_queue()
    }
    pub fn mark_permanent_failure(&self, delivery: &PendingDelivery, error: &str) -> Result<()> {
        self.conn.execute("UPDATE notification_deliveries SET state='permanent_failure',attempts=attempts+1,last_error=?4
            WHERE subscription_id=?1 AND repository=?2 AND number=?3 AND state IN ('pending','retry')",
            params![delivery.subscription_id,delivery.issue.repository,delivery.issue.number,error])?;
        self.observe_queue()
    }
    pub fn bound_users(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT user_openid FROM broadcast_subscriptions ORDER BY id")?;
        let users = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(users)
    }
    pub fn notification_summary(&self) -> Result<serde_json::Value> {
        let count = |state: &str| -> Result<u64> {
            Ok(self.conn.query_row(
                "SELECT COUNT(*) FROM notification_deliveries WHERE state=?1",
                [state],
                |row| row.get(0),
            )?)
        };
        // Subscription IDs distinguish recipients without publishing their QQ OpenIDs.
        let mut statement=self.conn.prepare("SELECT subscription_id,repository,number,state,last_error,enqueued_at,sent_at FROM notification_deliveries WHERE state IN ('retry','permanent_failure') ORDER BY repository,number,subscription_id")?;
        let failures=statement.query_map([],|row| Ok(serde_json::json!({"subscription_id":row.get::<_,i64>(0)?,"repository":row.get::<_,String>(1)?,"number":row.get::<_,u64>(2)?,"state":row.get::<_,String>(3)?,"error":row.get::<_,Option<String>>(4)?,"enqueued_at":row.get::<_,Option<String>>(5)?,"sent_at":row.get::<_,Option<String>>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(
            serde_json::json!({"pending":count("pending")?,"retrying":count("retry")?,"permanent_failed":count("permanent_failure")?,"failures":failures}),
        )
    }
    pub fn bind_user(&self, openid: &str) -> Result<bool> {
        anyhow::ensure!(
            !openid.trim().is_empty(),
            "QQ user OpenID must not be empty"
        );
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO broadcast_subscriptions(user_openid) VALUES(?1)",
            [openid],
        )?;
        self.observe_queue()?;
        Ok(changed == 1)
    }
    pub fn unbind_user(&self, openid: &str) -> Result<bool> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        self.conn.execute(
            "UPDATE notification_deliveries SET state='cancelled',next_attempt_at=NULL
            WHERE subscription_id IN (SELECT id FROM broadcast_subscriptions WHERE user_openid=?1)
            AND state IN ('pending','retry','permanent_failure')",
            [openid],
        )?;
        let changed = self.conn.execute(
            "DELETE FROM broadcast_subscriptions WHERE user_openid=?1",
            [openid],
        )?;
        transaction.commit()?;
        self.observe_queue()?;
        Ok(changed == 1)
    }
}

fn read_issue(row: &rusqlite::Row<'_>) -> rusqlite::Result<IssueNotification> {
    let timestamp: String = row.get(4)?;
    let created_at = DateTime::parse_from_rfc3339(&timestamp)
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?
        .with_timezone(&Utc);
    Ok(IssueNotification {
        repository: row.get(0)?,
        number: row.get(1)?,
        title: row.get(2)?,
        author: row.get(3)?,
        created_at,
        url: row.get(5)?,
    })
}

fn parse_dt(value: Option<String>) -> Option<DateTime<Utc>> {
    value
        .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|v| v.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persists_baseline_and_notification() {
        let store = Store::open_in_memory().unwrap();
        let now = Utc::now();
        let repo = store.ensure_repository("owner/repo", now).unwrap();
        assert_eq!(repo.baseline.unwrap().timestamp(), now.timestamp());
        let issue = IssueNotification {
            repository: "owner/repo".into(),
            number: 1,
            title: "Bug".into(),
            author: "alice".into(),
            created_at: now,
            url: "https://github.com/owner/repo/issues/1".into(),
        };
        store.bind_user("u").unwrap();
        assert!(store.insert_notification(&issue).unwrap());
        assert!(!store.insert_notification(&issue).unwrap());
        assert_eq!(store.pending_deliveries(now).unwrap().len(), 1);
        store
            .mark_sent(&store.pending_deliveries(now).unwrap()[0])
            .unwrap();
        assert!(store.pending_deliveries(now).unwrap().is_empty());
    }

    #[test]
    fn bindings_are_independent_and_idempotent() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.bind_user("first").unwrap());
        assert!(store.bind_user("second").unwrap());
        assert!(!store.bind_user("first").unwrap());
        assert_eq!(store.bound_users().unwrap(), ["first", "second"]);
    }
}
