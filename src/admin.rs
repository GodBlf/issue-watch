//! Management routes share the health listener and require same-origin JSON writes.
use crate::{config::FileConfig, health::Health, Store};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct Admin {
    auth: crate::admin_auth::Auth,
    live: crate::bilibili::LiveMonitoring,
    store: Arc<Mutex<Store>>,
    pub config: crate::config_management::ConfigManagement,
    tracking_source: Arc<dyn crate::tracking::TrackingSource>,
}
impl Admin {
    pub fn new(config_path: impl AsRef<Path>, health: Health) -> anyhow::Result<Self> {
        let config = FileConfig::load(&config_path)?;
        Self::with_active(config_path, &config, health)
    }
    pub fn with_active(
        config_path: impl AsRef<Path>,
        active: &FileConfig,
        health: Health,
    ) -> anyhow::Result<Self> {
        let store = Store::open(&active.database_path)?.with_health(health)?;
        store.reconcile_repositories_with_clock(&active.repositories, chrono::Utc::now)?;
        let mut auth = crate::admin_auth::Auth::open(
            &active.database_path,
            &std::env::var("ISSUE_WATCH_ADMIN_ORIGIN")
                .unwrap_or_else(|_| "https://127.0.0.1".into()),
        )?;
        if let Ok(proxy) = std::env::var("ISSUE_WATCH_ADMIN_TRUSTED_PROXY") {
            auth = auth.with_trusted_proxy(proxy.parse()?);
        }
        Ok(Self {
            live: crate::bilibili::LiveMonitoring::production(&active.database_path)?,
            auth,
            store: Arc::new(Mutex::new(store)),
            config: crate::config_management::ConfigManagement::new(config_path, active.clone())?,
            tracking_source: Arc::new(crate::tracking::GithubTrackingSource::new(
                std::env::var("GITHUB_TOKEN").ok(),
            )?),
        })
    }
    pub fn with_tracking_source(
        mut self,
        source: Arc<dyn crate::tracking::TrackingSource>,
    ) -> Self {
        self.tracking_source = source;
        self
    }
    pub fn with_live_source(mut self, source: Arc<dyn crate::bilibili::LiveSource>) -> Self {
        self.live = crate::bilibili::LiveMonitoring::new(
            &self.config.snapshot().applied.database_path,
            source,
        );
        self
    }
    pub fn with_auth(mut self, auth: crate::admin_auth::Auth) -> Self {
        self.auth = auth;
        self
    }
}

struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::warn!(%error, "Management operation failed");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "操作失败，请检查服务日志；已保存的操作可刷新确认".into(),
        )
    }
}
type ApiResult<T> = Result<T, ApiError>;

fn verify_write(admin: &Admin, headers: &HeaderMap) -> ApiResult<()> {
    if admin.auth.verify_write(headers) {
        Ok(())
    } else {
        Err(ApiError(
            StatusCode::FORBIDDEN,
            "请求来源不受信任，请从管理页面操作".into(),
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddSubscription {
    user_openid: String,
    qq_number_note: Option<String>,
}

async fn subscriptions(
    State(admin): State<Admin>,
) -> ApiResult<Json<Vec<crate::store::Subscription>>> {
    Ok(Json(admin.store.lock().unwrap().subscriptions()?))
}
async fn add_subscription(
    State(admin): State<Admin>,
    headers: HeaderMap,
    Json(body): Json<AddSubscription>,
) -> ApiResult<impl IntoResponse> {
    verify_write(&admin, &headers)?;
    let openid = body.user_openid.trim();
    if openid.is_empty() || openid.len() > 256 || openid.chars().any(char::is_whitespace) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "请填写有效的 user_openid".into(),
        ));
    }
    if body
        .qq_number_note
        .as_ref()
        .is_some_and(|note| note.len() > 256)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "QQ 号备注过长".into()));
    }
    let added = admin
        .store
        .lock()
        .unwrap()
        .add_subscription(openid, body.qq_number_note.as_deref().map(str::trim))?;
    Ok((
        if added {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(json!({"added":added})),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Note {
    qq_number_note: Option<String>,
}
async fn update_note(
    State(admin): State<Admin>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    headers: HeaderMap,
    Json(body): Json<Note>,
) -> ApiResult<Json<serde_json::Value>> {
    verify_write(&admin, &headers)?;
    if body
        .qq_number_note
        .as_ref()
        .is_some_and(|note| note.len() > 256)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "QQ 号备注过长".into()));
    }
    if !admin
        .store
        .lock()
        .unwrap()
        .update_subscription_note(id, body.qq_number_note.as_deref().map(str::trim))?
    {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "订阅已不存在，请刷新".into(),
        ));
    }
    Ok(Json(json!({"updated":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositorySelection {
    repositories: Vec<String>,
}

async fn repository_choices(State(admin): State<Admin>) -> ApiResult<Json<Vec<String>>> {
    Ok(Json(admin.store.lock().unwrap().active_repositories()?))
}

async fn select_repositories(
    State(admin): State<Admin>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    headers: HeaderMap,
    Json(body): Json<RepositorySelection>,
) -> ApiResult<Json<serde_json::Value>> {
    verify_write(&admin, &headers)?;
    let store = admin.store.lock().unwrap();
    store
        .replace_subscription_repositories(id, &body.repositories)
        .map_err(|error| {
            match error.downcast_ref::<crate::repository_subscriptions::SubscriptionEditError>() {
                Some(crate::repository_subscriptions::SubscriptionEditError::NotFound) => {
                    ApiError(StatusCode::NOT_FOUND, error.to_string())
                }
                Some(_) => ApiError(StatusCode::BAD_REQUEST, error.to_string()),
                None => error.into(),
            }
        })?;
    Ok(Json(
        json!({"repositories":store.subscription_repositories(id)?}),
    ))
}

async fn remove_subscription(
    State(admin): State<Admin>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    verify_write(&admin, &headers)?;
    if !admin.store.lock().unwrap().remove_subscription_by_id(id)? {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "订阅已不存在，请刷新".into(),
        ));
    }
    Ok(Json(json!({"removed":true})))
}

async fn configuration(
    State(admin): State<Admin>,
) -> Json<crate::config_management::ConfigSnapshot> {
    Json(admin.config.snapshot())
}
async fn save_configuration(
    State(admin): State<Admin>,
    headers: HeaderMap,
    Json(body): Json<crate::config_management::ConfigEdit>,
) -> ApiResult<Json<crate::config_management::ConfigSnapshot>> {
    verify_write(&admin, &headers)?;
    use crate::config_management::SaveError;
    match admin.config.save(body) {
        Ok(snapshot) => Ok(Json(snapshot)),
        Err(SaveError::Conflict) => Err(ApiError(
            StatusCode::CONFLICT,
            "配置已被其他操作修改，请刷新后重试".into(),
        )),
        Err(SaveError::Invalid(message)) => Err(ApiError(StatusCode::BAD_REQUEST, message)),
        Err(SaveError::Io(error)) => Err(error.into()),
    }
}

async fn tracking_list(
    State(admin): State<Admin>,
) -> ApiResult<Json<Vec<crate::tracking::TrackedIssue>>> {
    Ok(Json(admin.store.lock().unwrap().tracked_issues()?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackingInput {
    url: String,
}
async fn tracking_add(
    State(admin): State<Admin>,
    headers: HeaderMap,
    Json(body): Json<TrackingInput>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    verify_write(&admin, &headers)?;
    let (repository, number) = crate::tracking::issue_url(&body.url)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    if !admin
        .store
        .lock()
        .unwrap()
        .tracking_repository_allowed(&repository)?
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "只能追踪已配置的监控仓库".into(),
        ));
    }
    let now = chrono::Utc::now();
    let period = admin.store.lock().unwrap().tracking_period(&repository)?;
    let snapshot = admin
        .tracking_source
        .snapshot(&repository, number)
        .await
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let (added, tracking) = admin
        .store
        .lock()
        .unwrap()
        .add_tracking_for_period(&repository, number, &snapshot, now, Some(period))
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok((
        if added {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(
            json!({"added":added,"tracking":tracking,"message":if added {"已添加追踪"}else{"该 Issue 已在追踪中"}}),
        ),
    ))
}
async fn tracking_remove(
    State(admin): State<Admin>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    verify_write(&admin, &headers)?;
    let removed = admin.store.lock().unwrap().cancel_tracking(id)?;
    Ok(Json(
        json!({"removed":removed,"message":if removed {"已取消共享追踪"} else {"该 Issue 未在追踪中"}}),
    ))
}

async fn permissions_list(
    State(admin): State<Admin>,
) -> ApiResult<Json<Vec<crate::tracking::TrackingPermissions>>> {
    Ok(Json(
        admin.store.lock().unwrap().all_tracking_permissions()?,
    ))
}
async fn permissions_save(
    State(admin): State<Admin>,
    axum::extract::Path(user): axum::extract::Path<String>,
    headers: HeaderMap,
    Json(body): Json<crate::tracking::TrackingPermissions>,
) -> ApiResult<Json<serde_json::Value>> {
    verify_write(&admin, &headers)?;
    admin
        .store
        .lock()
        .unwrap()
        .set_tracking_permissions(&user, body.can_add, body.can_cancel)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(json!({"saved":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveInput {
    room: String,
}
async fn live_diagnostics(State(admin): State<Admin>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(admin.live.diagnostics().await?))
}
async fn live_subscriptions(
    State(admin): State<Admin>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> ApiResult<Json<Vec<crate::bilibili::LiveSnapshot>>> {
    Ok(Json(admin.live.subscriptions(id).await?))
}
async fn live_add(
    State(admin): State<Admin>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    headers: HeaderMap,
    Json(body): Json<LiveInput>,
) -> ApiResult<Json<serde_json::Value>> {
    verify_write(&admin, &headers)?;
    let snapshot = admin
        .live
        .subscribe_target(id, &body.room)
        .await
        .map_err(|error| {
            tracing::warn!(%error,"Live subscription failed");
            ApiError(
                StatusCode::BAD_REQUEST,
                "直播订阅失败，请检查房间输入或稍后重试".into(),
            )
        })?;
    Ok(Json(
        json!({"room":snapshot,"message":format!("直播订阅已生效\n{}",snapshot.feedback())}),
    ))
}
async fn live_remove(
    State(admin): State<Admin>,
    axum::extract::Path((id, room)): axum::extract::Path<(i64, u64)>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    verify_write(&admin, &headers)?;
    Ok(Json(json!({"removed":admin.live.remove(id,room).await?})))
}
fn router(admin: Admin) -> Router {
    Router::new()
        .route("/api/admin/bilibili", get(live_diagnostics))
        .route(
            "/api/admin/subscriptions/{id}/bilibili",
            get(live_subscriptions).post(live_add),
        )
        .route(
            "/api/admin/subscriptions/{id}/bilibili/{room}",
            axum::routing::delete(live_remove),
        )
        .route(
            "/admin-session.js",
            get(|| async {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/javascript; charset=utf-8",
                    )],
                    include_str!("admin-session.js"),
                )
            }),
        )
        .route(
            "/admin-tracking.js",
            get(|| async {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/javascript; charset=utf-8",
                    )],
                    include_str!("admin-tracking.js"),
                )
            }),
        )
        .route("/api/admin/tracking", get(tracking_list).post(tracking_add))
        .route(
            "/api/admin/tracking/{id}",
            axum::routing::delete(tracking_remove),
        )
        .route("/api/admin/permissions", get(permissions_list))
        .route(
            "/api/admin/permissions/{user}",
            axum::routing::put(permissions_save),
        )
        .route(
            "/admin-navigation.js",
            get(|| async {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/javascript; charset=utf-8",
                    )],
                    include_str!("admin-navigation.js"),
                )
            }),
        )
        .route(
            "/admin.js",
            get(|| async {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/javascript; charset=utf-8",
                    )],
                    include_str!("admin.js"),
                )
            }),
        )
        .route(
            "/api/admin/subscriptions",
            get(subscriptions).post(add_subscription),
        )
        .route(
            "/api/admin/subscriptions/{id}",
            axum::routing::patch(update_note).delete(remove_subscription),
        )
        .route("/api/admin/repositories", get(repository_choices))
        .route(
            "/api/admin/subscriptions/{id}/repositories",
            axum::routing::put(select_repositories),
        )
        .route(
            "/api/admin/config",
            get(configuration).patch(save_configuration),
        )
        .layer(axum::middleware::map_response(
            |mut response: Response| async move {
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    "no-store".parse().unwrap(),
                );
                response
            },
        ))
        .with_state(admin)
}
pub async fn serve(
    listener: tokio::net::TcpListener,
    health: Health,
    admin: Admin,
) -> std::io::Result<()> {
    let auth = admin.auth.clone();
    let internal_health = health.clone();
    // This listener is published only on the host loopback. The public proxy must
    // reject /internal/ before forwarding; no Host or forwarded header enables it.
    let router =
        crate::admin_auth::protect(crate::health::router(health).merge(router(admin)), auth).route(
            "/internal/status",
            get(move || async move {
                (
                    [(axum::http::header::CACHE_CONTROL, "no-store")],
                    Json(internal_health.snapshot()),
                )
            }),
        );
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
}
