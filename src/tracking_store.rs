use crate::{
    tracking::{IssueSnapshot, TrackedIssue},
    Store,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

pub(crate) struct TrackingDelivery {
    pub batch_id: i64,
    pub subscription_id: i64,
    pub user_openid: String,
    pub text: String,
}

impl Store {
    pub(crate) fn init_tracking(&self) -> Result<()> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS tracked_issues (
          id INTEGER PRIMARY KEY AUTOINCREMENT, repository TEXT NOT NULL COLLATE NOCASE, number INTEGER NOT NULL,
          title TEXT NOT NULL,url TEXT NOT NULL,started_at TEXT NOT NULL,last_success_at TEXT,error TEXT,active INTEGER NOT NULL DEFAULT 1);
          CREATE UNIQUE INDEX IF NOT EXISTS active_issue_tracking ON tracked_issues(repository,number) WHERE active=1;
          CREATE TABLE IF NOT EXISTS tracking_repositories (name TEXT PRIMARY KEY COLLATE NOCASE);
          CREATE TABLE IF NOT EXISTS tracking_events(tracking_id INTEGER NOT NULL,event_key TEXT NOT NULL,PRIMARY KEY(tracking_id,event_key));
          CREATE TABLE IF NOT EXISTS tracking_batches(id INTEGER PRIMARY KEY AUTOINCREMENT,tracking_id INTEGER NOT NULL,text TEXT NOT NULL,created_at TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS tracking_deliveries(batch_id INTEGER NOT NULL,subscription_id INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'pending',next_attempt_at TEXT,last_error TEXT,sent_at TEXT,PRIMARY KEY(batch_id,subscription_id));
          CREATE TABLE IF NOT EXISTS tracking_permissions(user_openid TEXT PRIMARY KEY,can_add INTEGER NOT NULL DEFAULT 0,can_cancel INTEGER NOT NULL DEFAULT 0);")?;
        Ok(())
    }
    pub fn tracking_repository_allowed(&self, name: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tracking_repositories WHERE name=?1)",
            [name],
            |row| row.get(0),
        )?)
    }
    pub fn tracking_permissions(&self, user: &str) -> Result<crate::tracking::TrackingPermissions> {
        Ok(self.conn.query_row("SELECT user_openid,can_add,can_cancel FROM tracking_permissions WHERE user_openid=?1",[user],|row|Ok(crate::tracking::TrackingPermissions{user_openid:row.get(0)?,can_add:row.get(1)?,can_cancel:row.get(2)?})).optional()?.unwrap_or(crate::tracking::TrackingPermissions{user_openid:user.into(),..Default::default()}))
    }
    pub fn set_tracking_permissions(
        &self,
        user: &str,
        can_add: bool,
        can_cancel: bool,
    ) -> Result<()> {
        anyhow::ensure!(
            !user.trim().is_empty() && user.len() <= 256,
            "user_openid 无效"
        );
        self.conn.execute("INSERT INTO tracking_permissions(user_openid,can_add,can_cancel) VALUES(?1,?2,?3) ON CONFLICT(user_openid) DO UPDATE SET can_add=excluded.can_add,can_cancel=excluded.can_cancel",params![user,can_add,can_cancel])?;
        Ok(())
    }
    pub fn all_tracking_permissions(&self) -> Result<Vec<crate::tracking::TrackingPermissions>> {
        let mut stmt = self.conn.prepare(
            "SELECT user_openid,can_add,can_cancel FROM tracking_permissions ORDER BY user_openid",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(crate::tracking::TrackingPermissions {
                user_openid: row.get(0)?,
                can_add: row.get(1)?,
                can_cancel: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn set_tracking_repositories(&self, names: &[String]) -> Result<()> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        self.replace_tracking_repositories(names)?;
        transaction.commit()?;
        Ok(())
    }
    pub(crate) fn replace_tracking_repositories(&self, names: &[String]) -> Result<()> {
        self.conn.execute("DELETE FROM tracking_repositories", [])?;
        for name in names {
            self.conn
                .execute("INSERT INTO tracking_repositories(name) VALUES(?1)", [name])?;
        }
        self.conn.execute("UPDATE tracking_deliveries SET state=CASE WHEN state='sending' THEN 'cancelled_inflight' ELSE 'cancelled' END,next_attempt_at=NULL WHERE state IN ('pending','retry','permanent_failure','sending') AND batch_id IN (SELECT b.id FROM tracking_batches b JOIN tracked_issues t ON t.id=b.tracking_id WHERE NOT EXISTS(SELECT 1 FROM tracking_repositories r WHERE r.name=t.repository))",[])?;
        self.conn.execute("UPDATE tracked_issues SET active=0 WHERE active=1 AND NOT EXISTS(SELECT 1 FROM tracking_repositories r WHERE r.name=tracked_issues.repository)",[])?;
        Ok(())
    }
    pub fn tracked_issues(&self) -> Result<Vec<TrackedIssue>> {
        let mut stmt=self.conn.prepare("SELECT id,repository,number,title,url,started_at,last_success_at,error FROM tracked_issues WHERE active=1 ORDER BY repository,number")?;
        let rows = stmt.query_map([], |row| {
            let started: String = row.get(5)?;
            let last: Option<String> = row.get(6)?;
            Ok(TrackedIssue {
                id: row.get(0)?,
                repository: row.get(1)?,
                number: row.get(2)?,
                title: row.get(3)?,
                url: row.get(4)?,
                started_at: parse_time(&started)?,
                last_success_at: last.map(|s| parse_time(&s)).transpose()?,
                error: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn add_tracking(
        &self,
        repository: &str,
        number: u64,
        snapshot: &IssueSnapshot,
        now: DateTime<Utc>,
    ) -> Result<(bool, TrackedIssue)> {
        self.add_tracking_for_period(repository, number, snapshot, now, None)
    }
    pub fn tracking_period(&self, repository: &str) -> Result<i64> {
        anyhow::ensure!(
            self.tracking_repository_allowed(repository)?,
            "只能追踪已配置的监控仓库"
        );
        let repo = self.ensure_repository(repository, Utc::now())?;
        anyhow::ensure!(repo.active, "仓库已移除");
        Ok(repo.generation)
    }
    pub fn add_tracking_for_period(
        &self,
        repository: &str,
        number: u64,
        snapshot: &IssueSnapshot,
        now: DateTime<Utc>,
        generation: Option<i64>,
    ) -> Result<(bool, TrackedIssue)> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if let Some(generation) = generation {
            anyhow::ensure!(
                self.repository_period_current(repository, generation)?,
                "仓库监控周期已改变，请重新添加追踪"
            );
        }
        anyhow::ensure!(
            self.tracking_repository_allowed(repository)?,
            "只能追踪已配置的监控仓库"
        );
        anyhow::ensure!(!snapshot.is_pull_request, "该链接是 PR，请提供 Issue URL");
        anyhow::ensure!(snapshot.state == "open", "该 Issue 已关闭，不能添加追踪");
        anyhow::ensure!(
            snapshot.partial_error.is_none(),
            "{}",
            snapshot.partial_error.as_deref().unwrap_or_default()
        );
        let added=self.conn.execute("INSERT OR IGNORE INTO tracked_issues(repository,number,title,url,started_at) VALUES(?1,?2,?3,?4,?5)",params![repository,number,snapshot.title,snapshot.url,now.to_rfc3339()])?==1;
        let issue = self
            .tracked_issues()?
            .into_iter()
            .find(|i| i.repository.eq_ignore_ascii_case(repository) && i.number == number)
            .unwrap();
        if added {
            // Baseline identities exclude history even when GitHub rounds timestamps to seconds.
            for event in &snapshot.activities {
                self.conn.execute(
                    "INSERT OR IGNORE INTO tracking_events(tracking_id,event_key) VALUES(?1,?2)",
                    params![issue.id, event.key],
                )?;
            }
        }
        transaction.commit()?;
        Ok((added, issue))
    }
    pub fn cancel_tracking(&self, id: i64) -> Result<bool> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let removed = self.conn.execute(
            "UPDATE tracked_issues SET active=0 WHERE id=?1 AND active=1",
            [id],
        )? == 1;
        if removed {
            self.conn.execute("UPDATE tracking_deliveries SET state=CASE WHEN state='sending' THEN 'cancelled_inflight' ELSE 'cancelled' END,next_attempt_at=NULL WHERE batch_id IN (SELECT id FROM tracking_batches WHERE tracking_id=?1) AND state IN ('pending','retry','permanent_failure','sending')",[id])?;
        }
        transaction.commit()?;
        Ok(removed)
    }
    pub(crate) fn tracking_failed(&self, id: i64, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tracked_issues SET error=?2 WHERE id=?1 AND active=1",
            params![id, error],
        )?;
        Ok(())
    }
    pub(crate) fn apply_tracking_snapshot(
        &self,
        issue: &TrackedIssue,
        snapshot: &IssueSnapshot,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let active: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tracked_issues WHERE id=?1 AND active=1)",
            [issue.id],
            |row| row.get(0),
        )?;
        if !active {
            return Ok(0);
        }
        let mut fresh = vec![];
        for event in &snapshot.activities {
            if event.at.timestamp() >= issue.started_at.timestamp()
                && self.conn.execute(
                    "INSERT OR IGNORE INTO tracking_events(tracking_id,event_key) VALUES(?1,?2)",
                    params![issue.id, event.key],
                )? == 1
            {
                fresh.push(event.clone());
            }
        }
        let closed_at = fresh
            .iter()
            .filter(|event| event.kind == "closed")
            .map(|event| event.at)
            .min();
        if let Some(at) = closed_at {
            fresh.retain(|event| event.at <= at);
        }
        if !fresh.is_empty() {
            fresh.sort_by_key(|event| event.at);
            let text = crate::tracking::render_activity(issue, snapshot, &fresh);
            self.conn.execute(
                "INSERT INTO tracking_batches(tracking_id,text,created_at) VALUES(?1,?2,?3)",
                params![issue.id, text, now.to_rfc3339()],
            )?;
            let batch = self.conn.last_insert_rowid();
            self.conn.execute("INSERT INTO tracking_deliveries(batch_id,subscription_id) SELECT ?1,id FROM broadcast_subscriptions",[batch])?;
        }
        self.conn.execute("UPDATE tracked_issues SET title=?2,last_success_at=CASE WHEN ?4 IS NULL THEN ?3 ELSE last_success_at END,error=?4 WHERE id=?1 AND active=1",params![issue.id,snapshot.title,now.to_rfc3339(),snapshot.partial_error])?;
        if fresh.iter().any(|event| event.kind == "closed") {
            self.conn
                .execute("UPDATE tracked_issues SET active=0 WHERE id=?1", [issue.id])?;
        }
        transaction.commit()?;
        Ok(fresh.len())
    }
    pub(crate) fn pending_tracking_deliveries(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<TrackingDelivery>> {
        let mut stmt=self.conn.prepare("SELECT d.batch_id,d.subscription_id,s.user_openid,b.text FROM tracking_deliveries d JOIN tracking_batches b ON b.id=d.batch_id JOIN tracked_issues t ON t.id=b.tracking_id JOIN tracking_repositories r ON r.name=t.repository JOIN broadcast_subscriptions s ON s.id=d.subscription_id WHERE d.state IN ('pending','retry') AND (d.next_attempt_at IS NULL OR d.next_attempt_at<=?1) ORDER BY b.id,s.id")?;
        let rows = stmt.query_map([now.to_rfc3339()], |row| {
            Ok(TrackingDelivery {
                batch_id: row.get(0)?,
                subscription_id: row.get(1)?,
                user_openid: row.get(2)?,
                text: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub(crate) fn tracking_delivery_pending(&self, d: &TrackingDelivery) -> Result<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM tracking_deliveries d JOIN broadcast_subscriptions s ON s.id=d.subscription_id JOIN tracking_batches b ON b.id=d.batch_id JOIN tracked_issues t ON t.id=b.tracking_id JOIN tracking_repositories r ON r.name=t.repository WHERE d.batch_id=?1 AND d.subscription_id=?2 AND d.state IN ('pending','retry'))",params![d.batch_id,d.subscription_id],|row|row.get(0))?)
    }
    pub(crate) fn begin_tracking_delivery(&self, d: &TrackingDelivery) -> Result<bool> {
        Ok(self.conn.execute("UPDATE tracking_deliveries SET state='sending' WHERE batch_id=?1 AND subscription_id=?2 AND state IN ('pending','retry') AND EXISTS(SELECT 1 FROM broadcast_subscriptions s WHERE s.id=subscription_id) AND EXISTS(SELECT 1 FROM tracking_batches b JOIN tracked_issues t ON t.id=b.tracking_id JOIN tracking_repositories r ON r.name=t.repository WHERE b.id=batch_id)", params![d.batch_id,d.subscription_id])? == 1)
    }
    pub(crate) fn finish_tracking_delivery(
        &self,
        d: &TrackingDelivery,
        state: &str,
        next: Option<DateTime<Utc>>,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute("UPDATE tracking_deliveries SET state=CASE WHEN state='cancelled_inflight' AND ?3<>'sent' THEN 'cancelled' ELSE ?3 END,next_attempt_at=CASE WHEN state='cancelled_inflight' THEN NULL ELSE ?4 END,last_error=?5,sent_at=CASE WHEN ?3='sent' THEN ?6 ELSE sent_at END WHERE batch_id=?1 AND subscription_id=?2 AND state IN ('pending','retry','sending','cancelled_inflight')",params![d.batch_id,d.subscription_id,state,next.map(|t|t.to_rfc3339()),error,Utc::now().to_rfc3339()])?;
        Ok(())
    }
    pub(crate) fn tracking_delivery_failures(&self) -> Result<Vec<serde_json::Value>> {
        let mut stmt=self.conn.prepare("SELECT d.subscription_id,t.repository,t.number,d.state,d.last_error,b.created_at,d.sent_at,b.id FROM tracking_deliveries d JOIN tracking_batches b ON b.id=d.batch_id JOIN tracked_issues t ON t.id=b.tracking_id WHERE d.state IN ('retry','permanent_failure') ORDER BY b.id,d.subscription_id")?;
        let rows=stmt.query_map([],|row|Ok(serde_json::json!({"subscription_id":row.get::<_,i64>(0)?,"repository":row.get::<_,String>(1)?,"number":row.get::<_,u64>(2)?,"state":row.get::<_,String>(3)?,"error":row.get::<_,Option<String>>(4)?,"enqueued_at":row.get::<_,String>(5)?,"sent_at":row.get::<_,Option<String>>(6)?,"batch_id":row.get::<_,i64>(7)?,"kind":"tracking"})))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub(crate) fn tracking_queue_count(&self, state: &str) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM tracking_deliveries WHERE state=?1",
            [state],
            |r| r.get(0),
        )?)
    }
}
fn parse_time(value: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}
