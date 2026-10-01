use chrono::{TimeZone, Utc};
use issue_watch::{config::FileConfig, reload::ConfigReloader, Store};
use std::fs;

fn fixture() -> (tempfile::TempDir, ConfigReloader, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "repositories = ['owner/old']").unwrap();
    let active = FileConfig::load(&path).unwrap();
    let store = Store::open(dir.path().join("state.sqlite")).unwrap();
    store
        .ensure_repository(
            "owner/old",
            Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
        )
        .unwrap();
    (dir, ConfigReloader::new(path, active), store)
}

#[test]
fn atomic_save_adds_repository_changes_interval_and_preserves_existing_progress() {
    let (dir, mut reload, store) = fixture();
    let now = Utc.with_ymd_and_hms(2026, 10, 1, 8, 0, 0).unwrap();
    store.set_cursor("owner/old", now).unwrap();
    store.bind_user("private-target").unwrap();
    let replacement = dir.path().join("replacement.toml");
    fs::write(
        &replacement,
        "repositories = ['owner/old', 'owner/new']\npoll_interval_seconds = 3",
    )
    .unwrap();
    fs::rename(replacement, dir.path().join("config.toml")).unwrap();
    assert!(reload.check(&store, now).unwrap());
    assert_eq!(reload.active().poll_interval_seconds, 3);
    assert_eq!(reload.active().repositories, ["owner/old", "owner/new"]);
    assert_eq!(
        store
            .ensure_repository("owner/new", now + chrono::Duration::hours(1))
            .unwrap()
            .baseline,
        Some(now)
    );
    assert_eq!(store.get_cursor("owner/old").unwrap(), Some(now));
    assert_eq!(
        store.get_bound_user().unwrap().as_deref(),
        Some("private-target")
    );
    assert!(!reload.check(&store, now).unwrap());
}

#[test]
fn invalid_or_missing_config_keeps_last_good_settings_and_recovers() {
    let (dir, mut reload, store) = fixture();
    let path = dir.path().join("config.toml");
    for invalid in [
        "broken = [",
        "repositories = []",
        "repositories = ['owner/new']\npoll_interval_seconds = 0",
        "repositories = ['owner/new']\npoll_interval_seconds = 18446744073709551615",
        "repositories = ['owner/new']\ndatabase_path = 'other.sqlite'",
        "repositories = ['owner/new']\npoll_interval_second = 3",
        "repositories = ['owner/new', 'OWNER/NEW']",
        "repositories = ['../new']",
    ] {
        fs::write(&path, invalid).unwrap();
        assert!(reload.check(&store, Utc::now()).is_err(), "{invalid}");
        assert_eq!(reload.active().repositories, ["owner/old"]);
        assert_eq!(reload.active().poll_interval_seconds, 60);
    }
    fs::remove_file(&path).unwrap();
    assert!(reload.check(&store, Utc::now()).is_err());
    fs::write(path, "repositories = ['owner/recovered']").unwrap();
    assert!(reload.check(&store, Utc::now()).unwrap());
    assert_eq!(reload.active().repositories, ["owner/recovered"]);
}

#[test]
fn removed_repository_stops_monitoring_but_readding_resumes_progress() {
    let (dir, mut reload, store) = fixture();
    let now = Utc::now();
    store.set_cursor("owner/old", now).unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "repositories = ['owner/new']").unwrap();
    reload.check(&store, now).unwrap();
    assert!(!reload.active().repositories.contains(&"owner/old".into()));
    fs::write(path, "repositories = ['owner/old']").unwrap();
    reload.check(&store, now).unwrap();
    assert_eq!(store.get_cursor("owner/old").unwrap(), Some(now));
}

#[tokio::test]
async fn watcher_publishes_changes_without_waiting_for_github_poll() {
    let (dir, reload, store) = fixture();
    let (tx, mut rx) = tokio::sync::watch::channel(reload.active().clone());
    fs::write(
        dir.path().join("config.toml"),
        "repositories = ['owner/new']\npoll_interval_seconds = 5",
    )
    .unwrap();
    let task = tokio::spawn(issue_watch::reload::watch_config(reload, store, tx));
    tokio::time::timeout(std::time::Duration::from_secs(5), rx.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rx.borrow_and_update().poll_interval_seconds, 5);
    assert_eq!(rx.borrow().repositories, ["owner/new"]);
    task.abort();
}

#[test]
fn casing_only_edit_keeps_repository_identity_cursor_and_notification() {
    let (dir, mut reload, store) = fixture();
    let now = Utc::now();
    store.set_cursor("owner/old", now).unwrap();
    store
        .insert_notification(&issue_watch::IssueNotification {
            repository: "owner/old".into(),
            number: 7,
            title: "bug".into(),
            author: "user".into(),
            created_at: now,
            url: "https://github.com/owner/old/issues/7".into(),
        })
        .unwrap();
    fs::write(
        dir.path().join("config.toml"),
        "repositories = ['OWNER/OLD']",
    )
    .unwrap();
    assert!(!reload.check(&store, now).unwrap());
    assert_eq!(reload.active().repositories, ["owner/old"]);
    assert_eq!(store.get_cursor("owner/old").unwrap(), Some(now));
    assert_eq!(
        store.pending_notifications(now).unwrap()[0].repository,
        "owner/old"
    );
}

#[test]
fn rejected_database_write_rolls_back_all_new_baselines() {
    let (dir, mut reload, store) = fixture();
    let db = rusqlite::Connection::open(dir.path().join("state.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_b BEFORE INSERT ON monitored_repositories WHEN NEW.name = 'owner/b' BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;").unwrap();
    fs::write(
        dir.path().join("config.toml"),
        "repositories = ['owner/a', 'owner/b']",
    )
    .unwrap();
    let earlier = Utc.with_ymd_and_hms(2026, 10, 1, 8, 0, 0).unwrap();
    assert!(reload.check(&store, earlier).is_err());
    assert_eq!(reload.active().repositories, ["owner/old"]);
    db.execute_batch("DROP TRIGGER reject_b").unwrap();
    let accepted = earlier + chrono::Duration::hours(1);
    assert!(reload.check(&store, accepted).unwrap());
    assert_eq!(
        store
            .ensure_repository("owner/a", accepted)
            .unwrap()
            .baseline,
        Some(accepted)
    );
    assert_eq!(
        store
            .ensure_repository("owner/b", accepted)
            .unwrap()
            .baseline,
        Some(accepted)
    );
}

#[tokio::test]
async fn pending_configuration_wins_over_due_poll_timer() {
    let (_dir, reload, _store) = fixture();
    let (tx, mut rx) = tokio::sync::watch::channel(reload.active().clone());
    let mut updated = reload.active().clone();
    updated.repositories = vec!["owner/new".into()];
    tx.send(updated.clone()).unwrap();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
    let event = issue_watch::reload::next_monitoring_event(&mut interval, &mut rx)
        .await
        .unwrap();
    assert_eq!(event, Some(updated));
    assert_eq!(
        issue_watch::reload::next_monitoring_event(&mut interval, &mut rx)
            .await
            .unwrap(),
        None
    );
}
