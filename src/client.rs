use opentelemetry::global;
use opentelemetry::trace::{Span, Tracer, TracerProvider as _, Status};
use opentelemetry::logs::{Logger, LoggerProvider as _, LogRecord, Severity};
use opentelemetry::Context;
use opentelemetry::KeyValue;
use opentelemetry_sdk::{
    logs::LoggerProvider,
    runtime,
    trace::{Config as TraceConfig, Sampler, Tracer as SdkTracer, TracerProvider},
    Resource,
};
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use opentelemetry_otlp::WithExportConfig;

use crate::config::{Config, LogLevel, should_sample_trace, should_sample_log};
use crate::error::Error as SdkError;

pub fn get_message_from_exception_body(body: &[u8], status_code: u16) -> String {
    if body.is_empty() {
        return format!("HTTP {}", status_code);
    }
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            if !err.is_empty() {
                return err.to_string();
            }
        }
    }
    format!("HTTP {}", status_code)
}

pub struct OTelClient {
    config: Config,
    tracer: Option<SdkTracer>,
    provider: Option<TracerProvider>,
    logger_provider: Option<LoggerProvider>,
    initialized: bool,
}

impl std::fmt::Debug for OTelClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OTelClient")
            .field("initialized", &self.initialized)
            .finish()
    }
}

impl OTelClient {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            tracer: None,
            provider: None,
            logger_provider: None,
            initialized: false,
        }
    }

    pub fn init(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.initialized {
            return Ok(());
        }

        let resource = Resource::new(vec![
            KeyValue::new(SERVICE_NAME, self.config.service.clone()),
        ]);

        let mut endpoint = self.config.endpoint.clone();
        if endpoint.ends_with("/v1/traces") {
            endpoint = endpoint.replace("/v1/traces", "");
        } else if endpoint.ends_with("/v1/logs") {
            endpoint = endpoint.replace("/v1/logs", "");
        }

        let mut headers = std::collections::HashMap::new();
        if let Some(ref token) = self.config.token {
            headers.insert("Authorization".to_string(), format!("Bearer {}", token));
        }

        let mut trace_exporter_builder = opentelemetry_otlp::new_exporter()
            .http()
            .with_endpoint(format!("{}/v1/traces", endpoint));
        if !headers.is_empty() {
            trace_exporter_builder = trace_exporter_builder.with_headers(headers.clone());
        }

        // AlwaysOn: sampling decisions are made in should_sample_trace() before span creation.
        let span_exporter = trace_exporter_builder.build_span_exporter()?;
        let provider = TracerProvider::builder()
            .with_batch_exporter(span_exporter, runtime::Tokio)
            .with_config(
                TraceConfig::default()
                    .with_resource(resource.clone())
                    .with_sampler(Sampler::AlwaysOn),
            )
            .build();

        let tracer = provider.tracer("middle-monitor-sdk");
        global::set_tracer_provider(provider.clone());
        self.tracer = Some(tracer);
        self.provider = Some(provider);

        let mut log_exporter_builder = opentelemetry_otlp::new_exporter()
            .http()
            .with_endpoint(format!("{}/v1/logs", endpoint));
        if !headers.is_empty() {
            log_exporter_builder = log_exporter_builder.with_headers(headers);
        }
        let logger_provider = opentelemetry_otlp::new_pipeline()
            .logging()
            .with_exporter(log_exporter_builder)
            .install_batch(runtime::Tokio)?;
        self.logger_provider = Some(logger_provider);

        self.initialized = true;
        Ok(())
    }

    fn log_level_to_severity(level: &LogLevel) -> Severity {
        match level {
            LogLevel::Debug => Severity::Debug,
            LogLevel::Info => Severity::Info,
            LogLevel::Warn => Severity::Warn,
            LogLevel::Error => Severity::Error,
            LogLevel::Fatal | LogLevel::Panic => Severity::Fatal,
        }
    }

    pub fn log(
        &self,
        level: &LogLevel,
        message: &str,
        attrs: Option<&std::collections::HashMap<String, String>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.initialized {
            return Err(Box::new(SdkError::NotInitialized));
        }
        let provider = self.logger_provider.as_ref().ok_or(SdkError::NotInitialized)?;
        let logger = provider.logger("middle-monitor-sdk");
        let mut record = logger.create_log_record();
        record.set_severity_number(Self::log_level_to_severity(level));
        record.set_severity_text(level.as_str().into());
        record.set_body(message.to_string().into());
        record.add_attribute("service.name", self.config.service.clone());
        if let Some(attrs) = attrs {
            for (k, v) in attrs {
                record.add_attribute(k.clone(), v.clone());
            }
        }
        logger.emit(record);
        Ok(())
    }

    pub fn flush_logs(&self) {
        if let Some(ref provider) = self.logger_provider {
            let _ = provider.force_flush();
        }
    }

    pub fn report_error<E: std::error::Error + ?Sized>(
        &self,
        error: &E,
        file: Option<&str>,
        line: Option<u32>,
        http_context: Option<&HttpContext>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.initialized {
            return Err(Box::new(SdkError::NotInitialized));
        }

        let tracer = self.tracer.as_ref().ok_or(SdkError::TracerUnavailable)?;
        let route = http_context.and_then(|ctx| ctx.url.as_deref()).unwrap_or("");
        let http_status = http_context.and_then(|ctx| ctx.status_code).unwrap_or(0);
        let has_error = true;

        if !should_sample_trace(&self.config, route, has_error) {
            return Ok(());
        }

        let cx = Context::current();
        let mut span = tracer.start_with_context("error.report", &cx);

        span.set_attribute(KeyValue::new("error.message", error.to_string()));
        span.set_attribute(KeyValue::new("error.type", std::any::type_name::<E>()));
        span.set_attribute(KeyValue::new("error.file", file.unwrap_or("unknown").to_string()));
        span.set_attribute(KeyValue::new("error.line", line.unwrap_or(0) as i64));
        span.set_attribute(KeyValue::new("service.name", self.config.service.clone()));

        if let Some(ctx) = http_context {
            if let Some(ref method) = ctx.method {
                span.set_attribute(KeyValue::new("http.method", method.clone()));
            }
            if let Some(ref url) = ctx.url {
                span.set_attribute(KeyValue::new("http.url", url.clone()));
            }
            if let Some(status_code) = ctx.status_code {
                span.set_attribute(KeyValue::new("http.status_code", status_code as i64));
            }
        }

        span.set_status(Status::error(error.to_string()));

        if should_sample_log(&self.config, route, &LogLevel::Error, http_status, has_error) {
            let _ = self.log(
                &LogLevel::Error,
                &error.to_string(),
                Some(&{
                    let mut m = std::collections::HashMap::new();
                    m.insert("error.type".to_string(), std::any::type_name::<E>().to_string());
                    m.insert("error.file".to_string(), file.unwrap_or("unknown").to_string());
                    m.insert("error.line".to_string(), line.unwrap_or(0).to_string());
                    if let Some(ctx) = http_context {
                        if let Some(ref method) = ctx.method {
                            m.insert("http.method".to_string(), method.clone());
                        }
                        if let Some(ref url) = ctx.url {
                            m.insert("http.url".to_string(), url.clone());
                        }
                        if let Some(sc) = ctx.status_code {
                            m.insert("http.status_code".to_string(), sc.to_string());
                        }
                    }
                    m
                }),
            );
        }

        span.end();
        Ok(())
    }

    pub async fn submit_application_error(
        &self,
        name: &str,
        message: &str,
        file: &str,
        line: i32,
        _status_code: u16,
        method: Option<&str>,
        url: Option<&str>,
        request_body: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut base = self.config.endpoint.trim_end_matches('/').to_string();
        if base.ends_with("/v1/traces") || base.ends_with("/v1/logs") {
            base = base.rsplitn(2, '/').nth(1).unwrap_or(&base).to_string();
        }
        let api_url = format!("{}/api/v1/errors", base);
        let service = self.config.service.clone();
        let token = self.config.token.clone();
        let mut payload = serde_json::json!({
            "name": name,
            "message": message,
            "file": file,
            "line": line,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "service": service,
        });
        if let Some(m) = method {
            payload["http_method"] = serde_json::Value::String(m.to_string());
        }
        if let Some(u) = url {
            payload["http_url"] = serde_json::Value::String(u.to_string());
        }
        if let Some(b) = request_body {
            let body_str = if b.len() > 2000 {
                // Truncate on a UTF-8 char boundary to avoid panicking on multi-byte input.
                let mut end = 2000;
                while !b.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &b[..end])
            } else {
                b.to_string()
            };
            payload["http_body"] = serde_json::Value::String(body_str);
        }
        let mut req = reqwest::Client::new()
            .post(&api_url)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(5));
        if let Some(t) = &token {
            if !t.is_empty() {
                req = req.bearer_auth(t);
            }
        }
        let _ = req.send().await?;
        Ok(())
    }

    pub fn shutdown(&mut self) {
        if let Some(_provider) = self.provider.take() {
            global::shutdown_tracer_provider();
        }
        if let Some(provider) = self.logger_provider.take() {
            let _ = provider.shutdown();
        }
        self.initialized = false;
    }
}

#[derive(Debug, Clone)]
pub struct HttpContext {
    pub method: Option<String>,
    pub url: Option<String>,
    pub status_code: Option<u16>,
    pub headers: Option<String>,
    pub body: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{new_config, LogLevel};

    // --- get_message_from_exception_body ---

    #[test]
    fn get_message_empty_body() {
        assert_eq!(get_message_from_exception_body(b"", 500), "HTTP 500");
    }

    #[test]
    fn get_message_json_with_error_field() {
        let body = br#"{"error":"db connection failed"}"#;
        assert_eq!(get_message_from_exception_body(body, 500), "db connection failed");
    }

    #[test]
    fn get_message_json_with_empty_error_field() {
        let body = br#"{"error":""}"#;
        assert_eq!(get_message_from_exception_body(body, 502), "HTTP 502");
    }

    #[test]
    fn get_message_json_without_error_field() {
        let body = br#"{"message":"ok"}"#;
        assert_eq!(get_message_from_exception_body(body, 503), "HTTP 503");
    }

    #[test]
    fn get_message_invalid_json() {
        assert_eq!(get_message_from_exception_body(b"not json", 501), "HTTP 501");
    }

    // --- OTelClient (uninitialized paths) ---

    #[test]
    fn otel_client_new_not_initialized() {
        let cfg = new_config("http://localhost:8080".to_string(), "svc".to_string(), None);
        let client = OTelClient::new(cfg);
        assert!(!client.initialized);
        assert!(client.tracer.is_none());
    }

    #[test]
    fn otel_client_log_not_initialized_returns_error() {
        let cfg = new_config("http://localhost:8080".to_string(), "svc".to_string(), None);
        let client = OTelClient::new(cfg);
        let result = client.log(&LogLevel::Info, "msg", None);
        assert!(result.is_err());
    }

    #[test]
    fn otel_client_report_error_not_initialized_returns_error() {
        let cfg = new_config("http://localhost:8080".to_string(), "svc".to_string(), None);
        let client = OTelClient::new(cfg);

        #[derive(Debug)]
        struct E;
        impl std::fmt::Display for E {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "err") }
        }
        impl std::error::Error for E {}

        let result = client.report_error(&E, None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn otel_client_flush_logs_when_not_initialized() {
        let cfg = new_config("http://localhost:8080".to_string(), "svc".to_string(), None);
        let client = OTelClient::new(cfg);
        client.flush_logs(); // Should not panic
    }

    // --- OTelClient (initialized paths, needs Tokio runtime) ---

    fn make_test_cfg() -> Config {
        new_config(
            "http://localhost:29999".to_string(),
            "test-svc".to_string(),
            Some("test-token".to_string()),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_init_succeeds() {
        let cfg = make_test_cfg();
        let mut client = OTelClient::new(cfg);
        let result = client.init();
        assert!(result.is_ok());
        assert!(client.initialized);
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_init_idempotent() {
        let cfg = make_test_cfg();
        let mut client = OTelClient::new(cfg);
        client.init().unwrap();
        let result = client.init(); // second call is no-op
        assert!(result.is_ok());
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_init_strips_v1_traces() {
        let cfg = new_config(
            "http://localhost:29999/v1/traces".to_string(),
            "svc".to_string(), None,
        );
        let mut client = OTelClient::new(cfg);
        assert!(client.init().is_ok());
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_init_strips_v1_logs() {
        let cfg = new_config(
            "http://localhost:29999/v1/logs".to_string(),
            "svc".to_string(), None,
        );
        let mut client = OTelClient::new(cfg);
        assert!(client.init().is_ok());
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_init_without_token() {
        let cfg = new_config("http://localhost:29999".to_string(), "svc".to_string(), None);
        let mut client = OTelClient::new(cfg);
        assert!(client.init().is_ok());
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_log_all_levels() {
        let cfg = make_test_cfg();
        let mut client = OTelClient::new(cfg);
        client.init().unwrap();
        for level in &[LogLevel::Debug, LogLevel::Info, LogLevel::Warn, LogLevel::Error, LogLevel::Fatal, LogLevel::Panic] {
            assert!(client.log(level, "test message", None).is_ok());
        }
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_log_with_attrs() {
        let cfg = make_test_cfg();
        let mut client = OTelClient::new(cfg);
        client.init().unwrap();
        let mut attrs = std::collections::HashMap::new();
        attrs.insert("key".to_string(), "value".to_string());
        assert!(client.log(&LogLevel::Info, "msg", Some(&attrs)).is_ok());
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_log_without_attrs() {
        let cfg = make_test_cfg();
        let mut client = OTelClient::new(cfg);
        client.init().unwrap();
        assert!(client.log(&LogLevel::Info, "msg", None).is_ok());
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_flush_logs_when_initialized() {
        let cfg = make_test_cfg();
        let mut client = OTelClient::new(cfg);
        client.init().unwrap();
        client.flush_logs();
        // No assertion — just verify no panic
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_report_error_basic() {
        let cfg = make_test_cfg();
        let mut client = OTelClient::new(cfg);
        client.init().unwrap();

        #[derive(Debug)]
        struct E { msg: String }
        impl std::fmt::Display for E {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.msg) }
        }
        impl std::error::Error for E {}

        let err = E { msg: "test error".to_string() };
        let result = client.report_error(&err, Some("src/handler.rs"), Some(42), None);
        assert!(result.is_ok());
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_report_error_not_sampled() {
        let mut cfg = make_test_cfg();
        cfg.sampling.traces.percentage = 0.0;
        cfg.sampling.traces.always_sample_errors = false;
        let mut client = OTelClient::new(cfg);
        client.init().unwrap();

        #[derive(Debug)]
        struct E;
        impl std::fmt::Display for E {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "err") }
        }
        impl std::error::Error for E {}

        let result = client.report_error(&E, None, None, None);
        assert!(result.is_ok()); // returns Ok but skips reporting
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_report_error_with_http_context() {
        let cfg = make_test_cfg();
        let mut client = OTelClient::new(cfg);
        client.init().unwrap();

        #[derive(Debug)]
        struct E;
        impl std::fmt::Display for E {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "err") }
        }
        impl std::error::Error for E {}

        let ctx = HttpContext {
            method: Some("POST".to_string()),
            url: Some("/api/data".to_string()),
            status_code: Some(500),
            headers: Some("Content-Type: json".to_string()),
            body: Some("{}".to_string()),
        };
        let result = client.report_error(&E, Some("handler.rs"), Some(10), Some(&ctx));
        assert!(result.is_ok());
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_report_error_http_context_no_method_no_url_no_status() {
        let cfg = make_test_cfg();
        let mut client = OTelClient::new(cfg);
        client.init().unwrap();

        #[derive(Debug)]
        struct E;
        impl std::fmt::Display for E {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "err") }
        }
        impl std::error::Error for E {}

        let ctx = HttpContext {
            method: None,
            url: None,
            status_code: None,
            headers: None,
            body: None,
        };
        let result = client.report_error(&E, None, None, Some(&ctx));
        assert!(result.is_ok());
        client.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_submit_application_error_branches() {
        // Tests argument branches (method, url, body) — connection will be refused
        let cfg = new_config("http://127.0.0.1:29998".to_string(), "svc".to_string(), Some("tok".to_string()));
        let client = OTelClient::new(cfg);
        // with all optional fields
        let _ = client.submit_application_error("err", "msg", "file.rs", 10, 500, Some("GET"), Some("/api"), Some("body")).await;
        // with long body (> 2000)
        let long_body = "x".repeat(2001);
        let _ = client.submit_application_error("err", "msg", "file.rs", 10, 500, None, None, Some(&long_body)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_submit_strips_v1_traces() {
        let cfg = new_config("http://127.0.0.1:29998/v1/traces".to_string(), "svc".to_string(), None);
        let client = OTelClient::new(cfg);
        let _ = client.submit_application_error("err", "msg", "f", 0, 500, None, None, None).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_submit_strips_v1_logs() {
        let cfg = new_config("http://127.0.0.1:29998/v1/logs".to_string(), "svc".to_string(), None);
        let client = OTelClient::new(cfg);
        let _ = client.submit_application_error("err", "msg", "f", 0, 500, None, None, None).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_submit_with_empty_token() {
        let cfg = new_config("http://127.0.0.1:29998".to_string(), "svc".to_string(), Some("".to_string()));
        let client = OTelClient::new(cfg);
        let _ = client.submit_application_error("err", "msg", "f", 0, 500, None, None, None).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn otel_client_shutdown_not_initialized() {
        let cfg = make_test_cfg();
        let mut client = OTelClient::new(cfg);
        client.shutdown(); // Should not panic
    }
}
