//! Who may call the manager, from where: a loopback-only token guard.
//!
//! - every route under a protected prefix needs `Authorization: Bearer <token>`,
//!   except its `health`. No cookies, so no CSRF;
//! - a `Host` not in the allowlist gets 421 (defeats DNS rebinding);
//! - a request with a foreign `Origin` gets 403;
//! - errors are `{"error": "…"}` with a status that says whose fault it was.

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Access {
    pub token: String,
    /// `Host` header values accepted, e.g. `127.0.0.1:8470`.
    pub hosts: Vec<String>,
    /// Path prefixes the token guards.
    pub protected: Vec<String>,
    /// Paths inside those prefixes that are open anyway.
    pub open: Vec<String>,
}

impl Access {
    /// Loopback on `port`, guarding `protected` prefixes (their `health` stays open).
    pub fn loopback(token: impl Into<String>, port: u16, protected: &[&str]) -> Self {
        Self {
            token: token.into(),
            hosts: vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")],
            protected: protected.iter().map(|p| p.to_string()).collect(),
            open: protected.iter().map(|p| format!("{p}health")).collect(),
        }
    }

    fn host_ok(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| self.hosts.iter().any(|a| a == h))
    }

    fn origin_ok(&self, headers: &HeaderMap) -> bool {
        match headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
            None => true,
            Some(o) => self.hosts.iter().any(|h| {
                o.strip_prefix("http://")
                    .or_else(|| o.strip_prefix("https://"))
                    == Some(h.as_str())
            }),
        }
    }

    fn token_ok(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|t| eq_ct(t, &self.token))
    }
}

/// Constant-time comparison, so the token cannot be guessed byte by byte.
pub fn eq_ct(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

pub fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"error": msg.into()}))).into_response()
}

async fn guard(State(a): State<Arc<Access>>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    if !a.host_ok(headers) {
        return err(StatusCode::MISDIRECTED_REQUEST, "unknown Host");
    }
    if !a.origin_ok(headers) {
        return err(StatusCode::FORBIDDEN, "Origin not allowed");
    }
    let path = req.uri().path();
    let guarded = a.protected.iter().any(|p| path.starts_with(p.as_str()))
        && !a.open.iter().any(|o| o == path);
    if guarded && !a.token_ok(headers) {
        return err(StatusCode::UNAUTHORIZED, "missing or wrong token");
    }
    next.run(req).await
}

/// Wraps a router in the guard.
pub fn guarded(router: Router, access: Arc<Access>) -> Router {
    router.layer(middleware::from_fn_with_state(access, guard))
}

/// `GET <prefix>health`: open, says which service and API version answer.
pub fn health(service: &'static str, api: u32) -> Router {
    Router::new().route(
        "/health",
        get(move || async move { Json(json!({"ok": true, "service": service, "api": api})) }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn app() -> Router {
        let access = Arc::new(Access::loopback("0123456789abcdef", 9000, &["/x/v1/"]));
        let x = Router::new()
            .route("/secret", get(|| async { "ok" }))
            .merge(health("x", 1));
        guarded(
            Router::new()
                .nest("/x/v1", x)
                .route("/ui", get(|| async { "page" })),
            access,
        )
    }

    async fn status(path: &str, host: &str, token: Option<&str>, origin: Option<&str>) -> u16 {
        let mut b = axum::http::Request::builder()
            .uri(path)
            .header(header::HOST, host);
        if let Some(t) = token {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        app()
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    #[tokio::test]
    async fn the_token_host_and_origin_rules() {
        let h = "127.0.0.1:9000";
        let t = Some("0123456789abcdef");
        assert_eq!(status("/x/v1/health", h, None, None).await, 200);
        assert_eq!(status("/x/v1/secret", h, None, None).await, 401);
        assert_eq!(
            status("/x/v1/secret", h, Some("wrong-wrong-wron"), None).await,
            401
        );
        assert_eq!(status("/x/v1/secret", h, t, None).await, 200);
        assert_eq!(
            status("/ui", h, None, None).await,
            200,
            "outside the prefix"
        );
        assert_eq!(
            status("/x/v1/secret", "evil.example:9000", t, None).await,
            421
        );
        assert_eq!(
            status("/x/v1/health", h, None, Some("https://evil.example")).await,
            403
        );
        assert_eq!(
            status("/x/v1/secret", h, t, Some("http://127.0.0.1:9000")).await,
            200
        );
    }

    #[test]
    fn tokens_compare_exactly() {
        assert!(eq_ct("abc", "abc"));
        assert!(!eq_ct("abc", "abd") && !eq_ct("abc", "ab"));
    }
}
