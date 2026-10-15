//! Repository selection is independent of destination identity and global monitoring periods.
use crate::Store;
use anyhow::Result;
use rusqlite::{params, OptionalExtension, TransactionBehavior};

#[derive(Debug, thiserror::Error)]
pub enum SubscriptionEditError {
    #[error("通知目标已不存在，请刷新")]
    NotFound,
    #[error("仓库未在当前生效的监控仓库池中：{0}，请刷新后重试")]
    RepositoryUnavailable(String),
}

impl Store {
    pub(crate) fn init_repository_subscriptions(&self) -> Result<()> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS repository_subscriptions (
            subscription_id INTEGER NOT NULL, repository TEXT NOT NULL COLLATE NOCASE,
            PRIMARY KEY(subscription_id,repository));
            CREATE INDEX IF NOT EXISTS repository_subscribers ON repository_subscriptions(repository,subscription_id);
            CREATE TABLE IF NOT EXISTS repository_subscription_migration_targets (subscription_id INTEGER PRIMARY KEY);")?;
        // Freeze the old recipients on first schema expansion, before any new /bind can occur.
        // Assignment waits for reconciliation with the final active repository configuration.
        let staged: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM settings WHERE key='repository_subscriptions_staged')",
            [],
            |r| r.get(0),
        )?;
        if !staged {
            self.conn.execute("INSERT INTO repository_subscription_migration_targets SELECT id FROM broadcast_subscriptions", [])?;
            self.conn.execute(
                "INSERT INTO settings(key,value) VALUES('repository_subscriptions_staged','1')",
                [],
            )?;
        }
        Ok(())
    }

    /// Called inside the repository reconciliation transaction, after deactivating removed repos.
    pub(crate) fn migrate_repository_subscriptions(&self) -> Result<()> {
        let done: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM settings WHERE key='repository_subscriptions_version')",
            [],
            |r| r.get(0),
        )?;
        if !done {
            self.conn.execute(
                "INSERT OR IGNORE INTO repository_subscriptions(subscription_id,repository)
                SELECT s.id,r.name FROM broadcast_subscriptions s
                JOIN repository_subscription_migration_targets m ON m.subscription_id=s.id
                CROSS JOIN monitored_repositories r WHERE r.active=1",
                [],
            )?;
            self.conn
                .execute("DELETE FROM repository_subscription_migration_targets", [])?;
            self.conn.execute(
                "INSERT INTO settings(key,value) VALUES('repository_subscriptions_version','1')",
                [],
            )?;
        }
        Ok(())
    }

    pub fn active_repositories(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM monitored_repositories WHERE active=1 ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn subscription_repositories(&self, id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT repository FROM repository_subscriptions WHERE subscription_id=?1 ORDER BY repository COLLATE NOCASE")?;
        let rows = stmt.query_map([id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn replace_subscription_repositories(&self, id: i64, names: &[String]) -> Result<()> {
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM broadcast_subscriptions WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(SubscriptionEditError::NotFound.into());
        }
        let mut canonical = std::collections::BTreeSet::new();
        for name in names {
            let repo: Option<String> = self.conn.query_row("SELECT name FROM monitored_repositories WHERE name=?1 COLLATE NOCASE AND active=1", [name], |r| r.get(0)).optional()?;
            canonical.insert(
                repo.ok_or_else(|| SubscriptionEditError::RepositoryUnavailable(name.clone()))?,
            );
        }
        self.conn.execute(
            "DELETE FROM repository_subscriptions WHERE subscription_id=?1",
            [id],
        )?;
        for name in canonical {
            self.conn.execute(
                "INSERT INTO repository_subscriptions(subscription_id,repository) VALUES(?1,?2)",
                params![id, name],
            )?;
        }
        self.cancel_unsubscribed_deliveries()?;
        transaction.commit()?;
        self.observe_queue()
    }

    /// Cancel states, rather than just hiding rows: re-subscription must never revive old work.
    pub(crate) fn cancel_unsubscribed_deliveries(&self) -> Result<()> {
        self.conn.execute("UPDATE notification_deliveries SET state=CASE WHEN state='sending' THEN 'cancelled_inflight' ELSE 'cancelled' END,next_attempt_at=NULL WHERE state IN ('pending','retry','permanent_failure','sending') AND NOT EXISTS(SELECT 1 FROM repository_subscriptions r WHERE r.subscription_id=notification_deliveries.subscription_id AND r.repository=notification_deliveries.repository COLLATE NOCASE)", [])?;
        self.conn.execute("UPDATE tracking_deliveries SET state=CASE WHEN state='sending' THEN 'cancelled_inflight' ELSE 'cancelled' END,next_attempt_at=NULL WHERE state IN ('pending','retry','permanent_failure','sending') AND NOT EXISTS(SELECT 1 FROM tracking_batches b JOIN tracked_issues t ON t.id=b.tracking_id JOIN repository_subscriptions r ON r.repository=t.repository COLLATE NOCASE AND r.subscription_id=tracking_deliveries.subscription_id WHERE b.id=tracking_deliveries.batch_id)", [])?;
        Ok(())
    }
}
