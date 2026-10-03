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
}
impl Admin {
    pub fn new(config_path: impl AsRef<Path>, health: Health) -> anyhow::Result<Self> {
        let config = FileConfig::load(&config_path)?;
        Ok(Self {
            store: Arc::new(Mutex::new(
                Store::open(config.database_path)?.with_health(health)?,
            )),
            config: crate::config_management::ConfigManagement::new(config_path)?,
        })
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

pub fn router(admin: Admin) -> Router {
    Router::new()
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
