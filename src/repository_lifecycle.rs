//! Transactional monitoring periods shared by startup, reload and discovery.
use crate::Store;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, TransactionBehavior};

impl Store {
    pub fn reconcile_repositories(
        &self,
        names: &[String],
        now: DateTime<Utc>,
    ) -> Result<Vec<String>> {
        self.reconcile_repositories_with_clock(names, || now)
    }

    pub fn reconcile_repositories_with_clock(
        &self,
        names: &[String],
        clock: impl Fn() -> DateTime<Utc>,
    ) -> Result<Vec<String>> {
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let now = clock();
        let mut canonical = Vec::new();
        let mut new_periods = Vec::new();
        for name in names {
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM monitored_repositories WHERE name=?1 COLLATE NOCASE)",
                [name],
                |row| row.get(0),
            )?;
            let repo = self.ensure_repository(name, now)?;
            if !exists || !repo.active {
                new_periods.push(repo.name.clone());
            }
            if !repo.active {
                self.conn.execute("UPDATE monitored_repositories SET active=1,generation=generation+1,baseline=?2,cursor=NULL WHERE name=?1", params![repo.name,now.to_rfc3339()])?;
            }
            canonical.push(repo.name);
        }
        self.replace_tracking_repositories(&canonical)?;
        self.conn.execute("UPDATE monitored_repositories SET active=0 WHERE NOT EXISTS(SELECT 1 FROM tracking_repositories r WHERE r.name=monitored_repositories.name COLLATE NOCASE)", [])?;
        self.conn.execute("UPDATE notification_deliveries SET state=CASE WHEN state='sending' THEN 'cancelled_inflight' ELSE 'cancelled' END,next_attempt_at=NULL,last_error='repository removed' WHERE state IN ('pending','retry','sending') AND EXISTS(SELECT 1 FROM monitored_repositories r WHERE r.name=notification_deliveries.repository COLLATE NOCASE AND r.active=0)", [])?;
        // Choose the period boundary under the write lock, immediately before committing.
        let accepted_at = clock().to_rfc3339();
        for name in new_periods {
            self.conn.execute(
                "UPDATE monitored_repositories SET baseline=?2 WHERE name=?1",
                params![name, accepted_at],
            )?;
        }
        transaction.commit()?;
        self.observe_queue()?;
        Ok(canonical)
    }

    /// Call once before starting any workers, never when merely opening another connection.
    pub fn start_monitoring(&self, names: &[String], now: DateTime<Utc>) -> Result<Vec<String>> {
        self.start_monitoring_with_clock(names, || now)
    }

    pub fn start_monitoring_with_clock(
        &self,
        names: &[String],
        clock: impl Fn() -> DateTime<Utc>,
    ) -> Result<Vec<String>> {
        let canonical = self.reconcile_repositories_with_clock(names, clock)?;
        let transaction =
            rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        for table in ["notification_deliveries", "tracking_deliveries"] {
            self.conn.execute(&format!("UPDATE {table} SET state=CASE WHEN state='sending' THEN 'retry' ELSE 'cancelled' END,next_attempt_at=NULL WHERE state IN ('sending','cancelled_inflight')"), [])?;
        }
        transaction.commit()?;
        self.observe_queue()?;
        Ok(canonical)
    }

    pub(crate) fn repository_period_current(&self, name: &str, generation: i64) -> Result<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM monitored_repositories WHERE name=?1 COLLATE NOCASE AND active=1 AND generation=?2)", params![name,generation], |row| row.get(0))?)
    }

    pub(crate) fn set_period_cursor(
        &self,
        name: &str,
        generation: i64,
        cursor: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute("UPDATE monitored_repositories SET cursor=?3 WHERE name=?1 AND active=1 AND generation=?2", params![name,generation,cursor.to_rfc3339()])?;
        Ok(())
    }
}
