use crate::model::{IssueNotification, MonitoredRepository};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("open SQLite database")?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS monitored_repositories (name TEXT PRIMARY KEY, baseline TEXT, cursor TEXT); CREATE TABLE IF NOT EXISTS issue_notifications (repository TEXT NOT NULL, number INTEGER NOT NULL, title TEXT NOT NULL, author TEXT NOT NULL, created_at TEXT NOT NULL, url TEXT NOT NULL, state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, next_attempt_at TEXT, last_error TEXT, PRIMARY KEY(repository, number)); CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
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
        let changed = self.conn.execute("INSERT OR IGNORE INTO issue_notifications(repository, number, title, author, created_at, url, state) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'pending')", params![issue.repository, issue.number, issue.title, issue.author, issue.created_at.to_rfc3339(), issue.url])?;
        Ok(changed == 1)
    }

    pub fn pending_notifications(&self, now: DateTime<Utc>) -> Result<Vec<IssueNotification>> {
        let mut stmt = self.conn.prepare("SELECT repository, number, title, author, created_at, url FROM issue_notifications WHERE state IN ('pending','retry') AND (next_attempt_at IS NULL OR next_attempt_at <= ?1) ORDER BY created_at, repository, number")?;
        let rows = stmt.query_map([now.to_rfc3339()], |row| {
            Ok(IssueNotification {
                repository: row.get(0)?,
                number: row.get(1)?,
                title: row.get(2)?,
                author: row.get(3)?,
                created_at: parse_dt(Some(row.get::<_, String>(4)?)).unwrap(),
                url: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn mark_sent(&self, issue: &IssueNotification) -> Result<()> {
        self.conn.execute("UPDATE issue_notifications SET state='sent', next_attempt_at=NULL, last_error=NULL WHERE repository=?1 AND number=?2", params![issue.repository, issue.number])?;
        Ok(())
    }
    pub fn mark_retry(
        &self,
        issue: &IssueNotification,
        next: DateTime<Utc>,
        error: &str,
    ) -> Result<()> {
        self.conn.execute("UPDATE issue_notifications SET state='retry', attempts=attempts+1, next_attempt_at=?3, last_error=?4 WHERE repository=?1 AND number=?2", params![issue.repository, issue.number, next.to_rfc3339(), error])?;
        Ok(())
    }
    pub fn mark_permanent_failure(&self, issue: &IssueNotification, error: &str) -> Result<()> {
        self.conn.execute("UPDATE issue_notifications SET state='permanent_failure', attempts=attempts+1, last_error=?3 WHERE repository=?1 AND number=?2", params![issue.repository, issue.number, error])?;
        Ok(())
    }
    pub fn get_bound_user(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key='qq_user_openid'",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }
    pub fn bind_user(&self, openid: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO settings(key,value) VALUES('qq_user_openid', ?1)",
            [openid],
        )?;
        Ok(changed == 1)
    }
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
        assert!(store.insert_notification(&issue).unwrap());
        assert!(!store.insert_notification(&issue).unwrap());
        assert_eq!(store.pending_notifications(now).unwrap().len(), 1);
        store.mark_sent(&issue).unwrap();
        assert!(store.pending_notifications(now).unwrap().is_empty());
    }

    #[test]
    fn first_binding_wins() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.bind_user("first").unwrap());
        assert!(!store.bind_user("second").unwrap());
        assert_eq!(store.get_bound_user().unwrap().as_deref(), Some("first"));
    }
}
