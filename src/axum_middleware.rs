//! Axum integration (feature `axum`), mirroring the Go SDK's HTTP middlewares:
//! one SERVER span per request (W3C trace context extracted from headers), error
//! status on 4xx/5xx, an error span for 5xx on never-sampled routes, and 5xx
//! responses submitted to the Errors view.
//!
//! Usage:
//! ```ignore
//! use axum::{middleware::from_fn, Router};
//!
//! middle_monitor_sdk::init_simple();
//! let app: Router = Router::new()
//!     .route("/", axum::routing::get(handler))
//!     .layer(from_fn(middle_monitor_sdk::axum_middleware::middleware));
//! ```

use axum::{
    body::{to_bytes, Body},
    extract::{MatchedPath, Request},
    http::header::CONTENT_LENGTH,
    middleware::Next,
    response::Response,
};
use opentelemetry::{
    global,
    propagation::Extractor,
    trace::{Span, SpanKind, Status, Tracer, TracerProvider as _},
    KeyValue,
};

use crate::client::get_message_from_exception_body;
use crate::config::should_sample_trace;
use crate::get_global_client;

const BODY_CAPTURE_LIMIT: usize = 4096;
// 5xx bodies larger than this are not buffered for message extraction (streaming safety).
const MAX_BUFFERED_RESPONSE: u64 = 64 * 1024;

struct HeaderExtractor<'a>(&'a axum::http::HeaderMap);

impl<'a> Extractor for HeaderExtractor<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

fn span_attributes(method: &str, route: &str, url: &str) -> Vec<KeyValue> {
    vec![
        KeyValue::new("http.method", method.to_string()),
        KeyValue::new("http.route", route.to_string()),
        KeyValue::new("http.url", url.to_string()),
    ]
}

pub async fn middleware(req: Request, next: Next) -> Response {
    let client = match get_global_client() {
        Some(c) => c,
        None => return next.run(req).await,
    };
    let cfg = client.config.clone();

    // MatchedPath gives the route template when the middleware is layered on a Router.
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let method = req.method().to_string();
    let url = req.uri().to_string();

    let parent_cx =
        global::get_text_map_propagator(|prop| prop.extract(&HeaderExtractor(req.headers())));

    let sampled = should_sample_trace(&cfg, &route, false);
    let tracer = global::tracer_provider().tracer("middle-monitor-sdk");
    let span_name = format!("{} {}", method, route);

    let mut span = if sampled {
        Some(
            tracer
                .span_builder(span_name.clone())
                .with_kind(SpanKind::Server)
                .with_attributes(span_attributes(&method, &route, &url))
                .start_with_context(&tracer, &parent_cx),
        )
    } else {
        None
    };

    let response = next.run(req).await;

    let status = response.status().as_u16();
    let has_error = status >= 400;
    let is_server_error = status >= 500;

    if let Some(ref mut span) = span {
        span.set_attribute(KeyValue::new("http.status_code", status as i64));
        span.set_attribute(KeyValue::new("error", has_error));
        if has_error {
            span.set_status(Status::error(format!("HTTP {}", status)));
        } else {
            span.set_status(Status::Ok);
        }
        span.end();
    } else if is_server_error && should_sample_trace(&cfg, &route, true) {
        // Never-sampled route (e.g. /health) that failed: still export an error span
        let mut attrs = span_attributes(&method, &route, &url);
        attrs.push(KeyValue::new("http.status_code", status as i64));
        attrs.push(KeyValue::new("error", true));
        let mut error_span = tracer
            .span_builder(span_name)
            .with_kind(SpanKind::Server)
            .with_attributes(attrs)
            .start_with_context(&tracer, &parent_cx);
        error_span.set_status(Status::error(format!("HTTP {}", status)));
        error_span.end();
    }

    if !is_server_error || cfg.disable_http_error_reporting {
        return response;
    }

    // Buffer small 5xx bodies to extract the "error" JSON field for the Errors view.
    // Large or unsized (streaming) bodies pass through untouched with a generic message.
    let content_length = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    let (message, response) = match content_length {
        Some(len) if len <= MAX_BUFFERED_RESPONSE => {
            let (parts, body) = response.into_parts();
            match to_bytes(body, MAX_BUFFERED_RESPONSE as usize).await {
                Ok(bytes) => {
                    let capped = &bytes[..bytes.len().min(BODY_CAPTURE_LIMIT)];
                    let message = get_message_from_exception_body(capped, status);
                    (message, Response::from_parts(parts, Body::from(bytes)))
                }
                Err(_) => (
                    format!("HTTP {}", status),
                    Response::from_parts(parts, Body::empty()),
                ),
            }
        }
        _ => (format!("HTTP {}", status), response),
    };

    // Fire and forget: never delay the response on the Errors API.
    tokio::spawn(async move {
        let _ = client
            .submit_application_error(
                "http",
                &message,
                "handler",
                0,
                status,
                Some(&method),
                Some(&url),
                None,
            )
            .await;
    });

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{middleware::from_fn, routing::get, Router};
    use tower::ServiceExt;

    const FAIL_BODY: &str = r#"{"error":"db down"}"#;

    fn make_app() -> Router {
        Router::new()
            .route("/ok", get(|| async { "ok" }))
            .route(
                "/fail",
                get(|| async {
                    axum::http::Response::builder()
                        .status(500)
                        .header("content-type", "application/json")
                        .header(CONTENT_LENGTH, FAIL_BODY.len())
                        .body(Body::from(FAIL_BODY))
                        .unwrap()
                }),
            )
            .layer(from_fn(middleware))
    }

    async fn body_string(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn passes_through_2xx_untouched() {
        crate::init_simple();
        let response = make_app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/ok")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(body_string(response).await, "ok");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn preserves_5xx_body_after_buffering() {
        crate::init_simple();
        let response = make_app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/fail")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 500);
        // The middleware buffers the body to extract the message; the client must
        // still receive it intact.
        assert_eq!(body_string(response).await, FAIL_BODY);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn propagates_w3c_traceparent_without_error() {
        crate::init_simple();
        let response = make_app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/ok")
                    .header(
                        "traceparent",
                        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
}
