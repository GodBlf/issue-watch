use axum::{
    routing::{get, post},
    Json, Router,
};
use issue_watch::tracking::{GithubTrackingSource, TrackingSource};
use serde_json::{json, Value};
#[tokio::test]
async fn github_timeline_pages_expose_comments_closure_and_explicit_cross_repository_links() {
    let app=Router::new().route("/repos/owner/repo/issues/12",get(||async{Json(json!({"title":"bug","state":"open"}))}))
        .route("/graphql",post(|Json(body):Json<Value>|async move{
            if body["query"].as_str().unwrap().contains("pullRequest(number") {return Json(json!({"data":{"repository":{"pullRequest":{"timelineItems":{"nodes":[],"pageInfo":{"hasNextPage":false}}}}}}));}
            let page=body["variables"]["after"].as_str();
            Json(if page.is_none(){json!({"data":{"repository":{"issue":{"timelineItems":{"nodes":[
                {"__typename":"IssueComment","id":"c1","createdAt":"2026-10-04T01:00:00Z","author":{"login":"alice"},"body":"fixed","url":"https://github.com/owner/repo/issues/12#issuecomment-1"},
                {"__typename":"CrossReferencedEvent","id":"r1","createdAt":"2026-10-04T01:01:00Z","actor":{"login":"alice"},"willCloseTarget":true,"source":{"__typename":"PullRequest","title":"fix","url":"https://github.com/other/repo/pull/8"}}
                ],"pageInfo":{"hasNextPage":true,"endCursor":"next"}}}}}})}else{json!({"data":{"repository":{"issue":{"timelineItems":{"nodes":[{"__typename":"ClosedEvent","id":"e1","createdAt":"2026-10-04T01:02:00Z","actor":{"login":"bob"}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}})})
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let source = GithubTrackingSource::with_api_base(Some("test-token".into()), &base).unwrap();
    let snapshot = source.snapshot("owner/repo", 12).await.unwrap();
    assert_eq!(
        snapshot
            .activities
            .iter()
            .map(|a| a.kind.as_str())
            .collect::<Vec<_>>(),
        ["comment", "linked", "closed"]
    );
    assert_eq!(
        snapshot.linked_prs,
        ["https://github.com/other/repo/pull/8"]
    );
    task.abort();
}
#[tokio::test]
async fn linked_pr_failure_does_not_hide_an_issue_close() {
    let app = Router::new()
        .route("/repos/owner/repo/issues/12", get(|| async { Json(json!({"title":"bug","state":"closed"})) }))
        .route("/graphql", post(|Json(body): Json<Value>| async move {
            if body["query"].as_str().unwrap().contains("pullRequest(number") {
                return Json(json!({"errors":[{"message":"Resource inaccessible"}]}));
            }
            Json(json!({"data":{"repository":{"issue":{"timelineItems":{"nodes":[
                {"__typename":"ConnectedEvent","id":"link","createdAt":"2026-10-04T01:01:00Z","source":{"__typename":"PullRequest","url":"https://github.com/other/repo/pull/8"}},
                {"__typename":"ClosedEvent","id":"close","createdAt":"2026-10-04T01:02:00Z"}
            ],"pageInfo":{"hasNextPage":false}}}}}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let source = GithubTrackingSource::with_api_base(Some("test-token".into()), &base).unwrap();
    let snapshot = source.snapshot("owner/repo", 12).await.unwrap();
    assert!(snapshot
        .activities
        .iter()
        .any(|event| event.kind == "closed"));
    task.abort();
}
#[tokio::test]
async fn only_explicitly_linked_prs_supply_merge_close_and_reopen_events() {
    let app=Router::new().route("/repos/owner/repo/issues/12",get(||async{Json(json!({"title":"bug","state":"open"}))}))
    .route("/graphql",post(|Json(body):Json<Value>|async move{
        if body["query"].as_str().unwrap().contains("pullRequest(number") {
            assert_eq!(body["variables"]["number"],8);
            Json(json!({"data":{"repository":{"pullRequest":{"timelineItems":{"nodes":[
            {"__typename":"MergedEvent","id":"m1","createdAt":"2026-10-04T01:02:00Z","actor":{"login":"alice"}},
            {"__typename":"ClosedEvent","id":"c1","createdAt":"2026-10-04T01:02:00Z","actor":{"login":"alice"}},
            {"__typename":"ReopenedEvent","id":"r1","createdAt":"2026-10-04T01:03:00Z","actor":{"login":"alice"}}
            ,{"__typename":"ClosedEvent","id":"late","createdAt":"2026-10-04T01:05:00Z","actor":{"login":"alice"}}
            ],"pageInfo":{"hasNextPage":false}}}}}}))
        } else {Json(json!({"data":{"repository":{"issue":{"timelineItems":{"nodes":[
        {"__typename":"CrossReferencedEvent","id":"ref","createdAt":"2026-10-04T01:00:00Z","willCloseTarget":false,"source":{"__typename":"PullRequest","title":"mention","url":"https://github.com/other/repo/pull/9"}},
        {"__typename":"ConnectedEvent","id":"link","createdAt":"2026-10-04T01:01:00Z","source":{"__typename":"PullRequest","title":"fix","url":"https://github.com/other/repo/pull/8"},"subject":{"__typename":"Issue"}}
        ,{"__typename":"DisconnectedEvent","id":"unlink","createdAt":"2026-10-04T01:04:00Z","source":{"__typename":"PullRequest","url":"https://github.com/other/repo/pull/8"}}
        ],"pageInfo":{"hasNextPage":false}}}}}}))}
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let source = GithubTrackingSource::with_api_base(Some("test-token".into()), &base).unwrap();
    let snapshot = source.snapshot("owner/repo", 12).await.unwrap();
    assert_eq!(
        snapshot
            .activities
            .iter()
            .map(|a| a.kind.as_str())
            .collect::<Vec<_>>(),
        ["referenced", "linked", "merged", "pr_reopened"]
    );
    task.abort();
}
