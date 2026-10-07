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
        Ok(Self {
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

fn verify_write(headers: &HeaderMap) -> ApiResult<()> {
    let valid = (|| {
        if headers.get("x-issue-watch-admin")?.to_str().ok()? != "1" {
            return None;
        }
        if headers
            .get("sec-fetch-site")
            .is_some_and(|v| v != "same-origin")
        {
            return None;
        }
        let origin = url::Url::parse(headers.get("origin")?.to_str().ok()?).ok()?;
        let host = headers.get("host")?.to_str().ok()?;
        let expected = url::Url::parse(&format!("http://{host}")).ok()?;
        let loopback = match expected.host()? {
            url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
        };
        if !loopback || origin.origin() != expected.origin() {
            return None;
        }
        Some(())
    })()
    .is_some();
    if valid {
        Ok(())
    } else {
        Err(ApiError(
            StatusCode::FORBIDDEN,
            "只允许来自本机管理页面的请求".into(),
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
    verify_write(&headers)?;
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
    verify_write(&headers)?;
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
async fn remove_subscription(
    State(admin): State<Admin>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    verify_write(&headers)?;
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
    verify_write(&headers)?;
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
    verify_write(&headers)?;
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
    verify_write(&headers)?;
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
    verify_write(&headers)?;
    admin
        .store
        .lock()
        .unwrap()
        .set_tracking_permissions(&user, body.can_add, body.can_cancel)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(json!({"saved":true})))
}
pub fn router(admin: Admin) -> Router {
    Router::new()
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
    axum::serve(listener, crate::health::router(health).merge(router(admin))).await
}
