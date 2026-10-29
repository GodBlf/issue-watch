use super::LiveSnapshot;
use crate::Store;
use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction};

pub(super) struct Delivery {
    pub id: i64,
    pub claim: i64,
    pub user: String,
    pub text: String,
}
impl Store {
    pub(crate) fn init_live(&self) -> Result<()> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS live_rooms (
            room_id INTEGER PRIMARY KEY, anchor TEXT NOT NULL, title TEXT NOT NULL,
            live_status INTEGER NOT NULL, live_time INTEGER,
            checked_at INTEGER, last_success_at INTEGER, error TEXT);
            CREATE TABLE IF NOT EXISTS live_subscriptions (
            id INTEGER PRIMARY KEY AUTOINCREMENT, target_id INTEGER NOT NULL, room_id INTEGER NOT NULL,
            skipped_session INTEGER, UNIQUE(target_id,room_id));
            CREATE TABLE IF NOT EXISTS live_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT, room_id INTEGER NOT NULL, live_time INTEGER NOT NULL,
            discovered_at INTEGER NOT NULL, message TEXT NOT NULL, UNIQUE(room_id,live_time));
            CREATE TABLE IF NOT EXISTS live_deliveries (
            id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, subscription_id INTEGER NOT NULL,
            target_id INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'pending', next_attempt_at INTEGER,
            error TEXT, claims INTEGER NOT NULL DEFAULT 0, lease_until INTEGER, UNIQUE(session_id,subscription_id));
            CREATE TABLE IF NOT EXISTS live_room_aliases(alias INTEGER PRIMARY KEY,room_id INTEGER NOT NULL);")?;
        let columns = self
            .conn
            .prepare("PRAGMA table_info(live_deliveries)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (name, definition) in [
            ("claims", "INTEGER NOT NULL DEFAULT 0"),
            ("lease_until", "INTEGER"),
        ] {
            if !columns.iter().any(|column| column == name) {
                self.conn.execute(
                    &format!("ALTER TABLE live_deliveries ADD COLUMN {name} {definition}"),
                    [],
                )?;
            }
        }
        Ok(())
    }
    pub(crate) fn add_live_subscription(
        &self,
        target: i64,
        snapshot: &LiveSnapshot,
        requested: u64,
        now: i64,
        observed_at: i64,
    ) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM broadcast_subscriptions WHERE id=?1)",
            [target],
            |r| r.get(0),
        )?;
        anyhow::ensure!(exists, "通知目标已不存在，请刷新");
        add_subscription(&tx, target, snapshot, requested, now, observed_at)?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn add_live_user_subscription(
        &self,
        user: &str,
        expected: Option<i64>,
        snapshot: &LiveSnapshot,
        requested: u64,
        now: i64,
        observed_at: i64,
    ) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        if let Some(id) = expected {
            anyhow::ensure!(
                self.live_target(user)? == Some(id),
                "通知目标已变化，请重新订阅"
            );
        }
        tx.execute(
            "INSERT OR IGNORE INTO broadcast_subscriptions(user_openid) VALUES(?1)",
            [user],
        )?;
        let target = tx.query_row(
            "SELECT id FROM broadcast_subscriptions WHERE user_openid=?1",
            [user],
            |r| r.get(0),
        )?;
        add_subscription(&tx, target, snapshot, requested, now, observed_at)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn live_subscriptions(&self, target: i64) -> Result<Vec<LiveSnapshot>> {
        let mut stmt=self.conn.prepare("SELECT r.room_id,r.anchor,r.title,r.live_status,r.live_time FROM live_rooms r JOIN live_subscriptions s ON s.room_id=r.room_id WHERE s.target_id=?1 ORDER BY r.room_id")?;
        let rows = stmt
            .query_map([target], read_snapshot)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub(crate) fn remove_live_subscription(&self, target: i64, room: u64) -> Result<bool> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        tx.execute("UPDATE live_deliveries SET state='cancelled',next_attempt_at=NULL WHERE subscription_id IN (SELECT id FROM live_subscriptions WHERE target_id=?1 AND room_id=?2) AND state IN ('pending','retry','sending')",params![target,room])?;
        let removed = tx.execute(
            "DELETE FROM live_subscriptions WHERE target_id=?1 AND room_id=?2",
            params![target, room],
        )? > 0;
        tx.commit()?;
        Ok(removed)
    }
    pub(super) fn known_live_room(&self, target: i64, input: u64) -> Result<Option<u64>> {
        Ok(self.conn.query_row("SELECT room_id FROM live_subscriptions WHERE target_id=?1 AND (room_id=?2 OR room_id=(SELECT room_id FROM live_room_aliases WHERE alias=?2))",params![target,input],|r|r.get(0)).optional()?)
    }
    pub(crate) fn live_target(&self, user: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM broadcast_subscriptions WHERE user_openid=?1",
                [user],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub(super) fn active_live_rooms(&self) -> Result<Vec<u64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT room_id FROM live_subscriptions ORDER BY room_id")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    pub(super) fn observe_live(
        &self,
        snapshot: &LiveSnapshot,
        now: i64,
        observed_at: i64,
    ) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM live_subscriptions WHERE room_id=?1)",
            [snapshot.room_id],
            |r| r.get(0),
        )?;
        if active {
            observe(&tx, snapshot, now, observed_at)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub(super) fn live_check_error(&self, room: u64, observed_at: i64) -> Result<()> {
        self.conn.execute("UPDATE live_rooms SET checked_at=?2,error='直播状态查询失败，请稍后重试或检查服务日志' WHERE room_id=?1 AND (checked_at IS NULL OR checked_at<=?2)",params![room,observed_at])?;
        Ok(())
    }
    pub(super) fn claim_live_delivery(&self, now: i64) -> Result<Option<Delivery>> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        tx.execute("UPDATE live_deliveries SET state='cancelled',next_attempt_at=NULL WHERE state IN ('pending','retry') AND (session_id IN (SELECT id FROM live_sessions WHERE discovered_at<?1) OR NOT EXISTS (SELECT 1 FROM live_subscriptions s JOIN broadcast_subscriptions t ON t.id=s.target_id WHERE s.id=live_deliveries.subscription_id))",[now-600])?;
        // A failed verification/result write must not strand a claim until restart.
        tx.execute("UPDATE live_deliveries SET state='retry',next_attempt_at=NULL WHERE state='sending' AND lease_until<=?1",[now])?;
        let delivery=tx.query_row("SELECT d.id,t.user_openid,n.message,d.claims+1 FROM live_deliveries d JOIN live_sessions n ON n.id=d.session_id JOIN broadcast_subscriptions t ON t.id=d.target_id WHERE d.state IN ('pending','retry') AND (d.next_attempt_at IS NULL OR d.next_attempt_at<=?1) ORDER BY d.id LIMIT 1",[now],|r|Ok(Delivery{id:r.get(0)?,user:r.get(1)?,text:r.get(2)?,claim:r.get(3)?})).optional()?;
        if let Some(d) = &delivery {
            tx.execute(
                "UPDATE live_deliveries SET state='sending',claims=claims+1,lease_until=?2 WHERE id=?1",
                params![d.id,now+120],
            )?;
        }
        tx.commit()?;
        Ok(delivery)
    }
    pub(super) fn live_diagnostics(&self) -> Result<serde_json::Value> {
        let mut stmt=self.conn.prepare("SELECT r.room_id,r.anchor,r.title,r.live_status,r.live_time,COUNT(s.id),r.checked_at/1000000,r.last_success_at/1000000,r.error FROM live_rooms r JOIN live_subscriptions s ON s.room_id=r.room_id GROUP BY r.room_id ORDER BY r.room_id")?;
        let rooms=stmt.query_map([],|r|Ok(serde_json::json!({"room_id":r.get::<_,u64>(0)?,"anchor":r.get::<_,String>(1)?,"title":r.get::<_,String>(2)?,"live_status":r.get::<_,i64>(3)?,"live_time":r.get::<_,Option<i64>>(4)?,"subscriber_count":r.get::<_,u64>(5)?,"checked_at":r.get::<_,Option<i64>>(6)?,"last_success_at":r.get::<_,Option<i64>>(7)?,"error":r.get::<_,Option<String>>(8)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut stmt=self.conn.prepare("SELECT d.target_id,n.room_id,n.live_time,d.error FROM live_deliveries d JOIN live_sessions n ON n.id=d.session_id WHERE d.state='permanent_failure' ORDER BY d.id DESC LIMIT 100")?;
        let failures=stmt.query_map([],|r|Ok(serde_json::json!({"target_id":r.get::<_,i64>(0)?,"room_id":r.get::<_,u64>(1)?,"live_time":r.get::<_,i64>(2)?,"error":r.get::<_,Option<String>>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(serde_json::json!({"rooms":rooms,"failures":failures}))
    }
    pub(super) fn recover_live_deliveries(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE live_deliveries SET state='retry',next_attempt_at=NULL WHERE state='sending'",
            [],
        )?;
        Ok(())
    }
    pub(super) fn live_delivery_valid(&self, id: i64, claim: i64, now: i64) -> Result<bool> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        tx.execute("UPDATE live_deliveries SET state='cancelled',next_attempt_at=NULL WHERE id=?1 AND state='sending' AND (session_id IN (SELECT id FROM live_sessions WHERE discovered_at<?2) OR NOT EXISTS (SELECT 1 FROM live_subscriptions s JOIN broadcast_subscriptions t ON t.id=s.target_id WHERE s.id=live_deliveries.subscription_id))",params![id,now-600])?;
        let valid = tx.query_row(
            "SELECT state='sending' AND claims=?2 FROM live_deliveries WHERE id=?1",
            params![id, claim],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(valid)
    }
    pub(super) fn finish_live_delivery(
        &self,
        id: i64,
        claim: i64,
        state: &str,
        next: Option<i64>,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute("UPDATE live_deliveries SET state=?2,next_attempt_at=?3,error=?4 WHERE id=?1 AND state='sending' AND claims=?5",params![id,state,next,error,claim])?;
        Ok(())
    }
}
fn add_subscription(
    tx: &Transaction<'_>,
    target: i64,
    snapshot: &LiveSnapshot,
    requested: u64,
    now: i64,
    observed_at: i64,
) -> Result<()> {
    ensure_room(tx, snapshot)?;
    tx.execute("INSERT INTO live_room_aliases(alias,room_id) VALUES(?1,?2) ON CONFLICT(alias) DO UPDATE SET room_id=excluded.room_id",params![requested,snapshot.room_id])?;
    // Freeze existing subscribers before adding the newcomer; the latter only
    // receives current status, even when this validation discovers a new session.
    observe(tx, snapshot, now, observed_at)?;
    tx.execute("INSERT OR IGNORE INTO live_subscriptions(target_id,room_id,skipped_session) VALUES(?1,?2,?3)",params![target,snapshot.room_id,snapshot.session()])?;
    Ok(())
}
fn ensure_room(tx: &Transaction<'_>, snapshot: &LiveSnapshot) -> Result<()> {
    tx.execute("INSERT OR IGNORE INTO live_rooms(room_id,anchor,title,live_status,live_time) VALUES(?1,?2,?3,?4,?5)",params![snapshot.room_id,snapshot.anchor,snapshot.title,snapshot.live_status,snapshot.live_time])?;
    Ok(())
}
fn observe(
    tx: &Transaction<'_>,
    snapshot: &LiveSnapshot,
    now: i64,
    observed_at: i64,
) -> Result<()> {
    let last: Option<i64> = tx.query_row(
        "SELECT checked_at FROM live_rooms WHERE room_id=?1",
        [snapshot.room_id],
        |r| r.get(0),
    )?;
    if last.is_some_and(|last| last > observed_at) {
        return Ok(());
    }
    if !matches!(snapshot.live_status, 0..=2) {
        tx.execute("UPDATE live_rooms SET checked_at=?2,error='未知直播状态，保留上次成功结果' WHERE room_id=?1",params![snapshot.room_id,observed_at])?;
        return Ok(());
    }
    if let Some(session) = snapshot.session() {
        let changed=tx.execute("INSERT OR IGNORE INTO live_sessions(room_id,live_time,discovered_at,message) VALUES(?1,?2,?3,?4)",params![snapshot.room_id,session,now,format!("[开播通知] {}\n标题：{}\n房间号：{}\nhttps://live.bilibili.com/{}",snapshot.anchor,snapshot.title,snapshot.room_id,snapshot.room_id)])?;
        if changed > 0 {
            let id = tx.last_insert_rowid();
            tx.execute("INSERT INTO live_deliveries(session_id,subscription_id,target_id) SELECT ?1,id,target_id FROM live_subscriptions WHERE room_id=?2 AND (skipped_session IS NULL OR skipped_session!=?3)",params![id,snapshot.room_id,session])?;
        }
        tx.execute("UPDATE live_deliveries SET state='cancelled',next_attempt_at=NULL WHERE state IN ('pending','retry','sending') AND session_id IN (SELECT id FROM live_sessions WHERE room_id=?1 AND live_time!=?2)",params![snapshot.room_id,session])?;
    } else {
        tx.execute("UPDATE live_deliveries SET state='cancelled',next_attempt_at=NULL WHERE state IN ('pending','retry','sending') AND session_id IN (SELECT id FROM live_sessions WHERE room_id=?1)",[snapshot.room_id])?;
    }
    tx.execute("UPDATE live_rooms SET anchor=?2,title=?3,live_status=?4,live_time=?5,checked_at=?6,last_success_at=?6,error=NULL WHERE room_id=?1",params![snapshot.room_id,snapshot.anchor,snapshot.title,snapshot.live_status,snapshot.live_time,observed_at])?;
    Ok(())
}
fn read_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<LiveSnapshot> {
    Ok(LiveSnapshot {
        room_id: row.get(0)?,
        anchor: row.get(1)?,
        title: row.get(2)?,
        live_status: row.get(3)?,
        live_time: row.get(4)?,
    })
}
