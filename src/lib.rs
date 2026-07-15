pub mod config;
pub mod client;
pub mod error;

use std::sync::{Mutex, Once};

use config::Config;
use client::{OTelClient, HttpContext};

pub use client::get_message_from_exception_body;
pub use config::{
    LogLevel, new_config, config_from_env,
    default_sampling_config, should_sample_trace, should_sample_log,
    SamplingConfig, TracesSamplingConfig, LogsSamplingConfig,
};

static GLOBAL_CLIENT: Mutex<Option<MiddleMonitorClient>> = Mutex::new(None);
static INIT_ONCE: Once = Once::new();

#[derive(Debug, Clone)]
pub struct MiddleMonitorClient {
    pub config: Config,
    otel_client: std::sync::Arc<Mutex<OTelClient>>,
}

impl MiddleMonitorClient {
    pub fn new(cfg: Config) -> Self {
        let mut otel_client = OTelClient::new(cfg.clone());
        if let Err(e) = otel_client.init() {
            eprintln!("[Middle-Monitor] Warning: failed to initialize OpenTelemetry client: {}", e);
        }
        Self {
            config: cfg,
            otel_client: std::sync::Arc::new(Mutex::new(otel_client)),
        }
    }

    fn is_application_error(&self, file: &str) -> bool {
        if file.is_empty() {
            return false;
        }
        let framework_paths = [
            "/.cargo/", "/rustc/", "/target/", "/vendor/",
            "/node_modules/", "libstd", "libcore", "liballoc",
        ];
        !framework_paths.iter().any(|p| file.contains(p))
            && !file.contains("_test.rs")
            && !file.contains(".gen.rs")
    }

    pub async fn report_error(
        &self,
        error: &str,
        file: Option<String>,
        line: Option<i32>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let file_str = file.as_deref().unwrap_or("unknown");
        if !self.is_application_error(file_str) {
            return Ok(());
        }

        #[derive(Debug)]
        struct SimpleError { message: String }
        impl std::fmt::Display for SimpleError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.message) }
        }
        impl std::error::Error for SimpleError {}

        let err = SimpleError { message: error.to_string() };
        let otel = self.otel_client.lock().unwrap();
        otel.report_error(&err, file.as_deref(), line.map(|l| l as u32), None)?;
        Ok(())
    }

    pub async fn report_custom_error(
        &self,
        name: &str,
        message: &str,
        file: &str,
        line: i32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.is_application_error(file) {
            return Ok(());
        }

        #[derive(Debug)]
        struct CustomError { name: String, message: String }
        impl std::fmt::Display for CustomError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}: {}", self.name, self.message)
            }
        }
        impl std::error::Error for CustomError {}

        let err = CustomError { name: name.to_string(), message: message.to_string() };
        let otel = self.otel_client.lock().unwrap();
        otel.report_error(&err, Some(file), Some(line as u32), None)?;
        Ok(())
    }

    pub async fn report_custom_error_with_http(
        &self,
        name: &str,
        message: &str,
        file: &str,
        line: i32,
        http_method: Option<&str>,
        http_url: Option<&str>,
        http_headers: Option<&str>,
        http_body: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.is_application_error(file) {
            return Ok(());
        }

        #[derive(Debug)]
        struct CustomError { name: String, message: String }
        impl std::fmt::Display for CustomError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}: {}", self.name, self.message)
            }
        }
        impl std::error::Error for CustomError {}

        let err = CustomError { name: name.to_string(), message: message.to_string() };
        let http_context = HttpContext {
            method: http_method.map(|s| s.to_string()),
            url: http_url.map(|s| s.to_string()),
            status_code: None,
            headers: http_headers.map(|s| s.to_string()),
            body: http_body.map(|s| s.to_string()),
        };
        let otel = self.otel_client.lock().unwrap();
        otel.report_error(&err, Some(file), Some(line as u32), Some(&http_context))?;
        Ok(())
    }

    pub fn capture_panic(&self, panic_info: &std::panic::PanicHookInfo) {
        let location = panic_info.location();
        let file = location.map(|l| l.file()).unwrap_or("unknown");
        let line = location.map(|l| l.line()).unwrap_or(0);
        let message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Panic occurred".to_string()
        };

        #[derive(Debug)]
        struct PanicError { message: String }
        impl std::fmt::Display for PanicError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "Panic: {}", self.message)
            }
        }
        impl std::error::Error for PanicError {}

        let err = PanicError { message };
        if let Ok(otel) = self.otel_client.lock() {
            let _ = otel.report_error(&err, Some(file), Some(line), None);
        }
    }

    pub fn log(
        &self,
        level: &LogLevel,
        message: &str,
        attrs: Option<&std::collections::HashMap<String, String>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let otel = self.otel_client.lock().unwrap();
        otel.log(level, message, attrs)
    }

    pub fn flush_logs(&self) {
        if let Ok(otel) = self.otel_client.lock() {
            otel.flush_logs();
        }
    }

    pub async fn submit_application_error(
        &self,
        name: &str,
        message: &str,
        file: &str,
        line: i32,
        status_code: u16,
        method: Option<&str>,
        url: Option<&str>,
        request_body: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Ok(otel) = self.otel_client.lock() {
            otel.submit_application_error(name, message, file, line, status_code, method, url, request_body).await
        } else {
            Ok(())
        }
    }

    pub fn shutdown(&self) {
        if let Ok(mut otel) = self.otel_client.lock() {
            otel.shutdown();
        }
    }
}

// ---------------------------------------------------------------------------
// Global init API (mirrors Go: init, init_with_config, init_simple)
// ---------------------------------------------------------------------------

pub fn init(cfg: Option<Config>) {
    INIT_ONCE.call_once(|| {
        let resolved = cfg.unwrap_or_else(|| {
            config_from_env().unwrap_or_else(|e| {
                eprintln!("[Middle-Monitor] failed to load config from env: {}", e);
                new_config("http://localhost:8080".to_string(), "unknown".to_string(), None)
            })
        });

        if resolved.token.is_none() {
            eprintln!(
                "[Middle-Monitor] initialized without token: service={}",
                resolved.service,
            );
        }

        let client = MiddleMonitorClient::new(resolved);
        *GLOBAL_CLIENT.lock().unwrap() = Some(client);
    });
}

pub fn init_simple() {
    init(None);
}

pub fn init_with_config(
    api_url: String,
    service: String,
    token: Option<String>,
) {
    init(Some(new_config(api_url, service, token)));
}

pub fn get_global_client() -> Option<MiddleMonitorClient> {
    if GLOBAL_CLIENT.lock().unwrap().is_none() {
        init(None);
    }
    GLOBAL_CLIENT.lock().unwrap().clone()
}

pub fn get_global_config() -> Option<Config> {
    get_global_client().map(|c| c.config)
}

// ---------------------------------------------------------------------------
// Global convenience functions
// ---------------------------------------------------------------------------

pub async fn report_error(error: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(client) = get_global_client() {
        client.report_error(error, None, None).await
    } else {
        Ok(())
    }
}

pub async fn report_error_with_details(
    error: &str,
    file: &str,
    line: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(client) = get_global_client() {
        client.report_error(error, Some(file.to_string()), Some(line)).await
    } else {
        Ok(())
    }
}

pub fn capture_panic_global(panic_info: &std::panic::PanicHookInfo) {
    if let Some(client) = get_global_client() {
        client.capture_panic(panic_info);
    }
}

pub fn log_global(
    level: &LogLevel,
    message: &str,
    attrs: Option<&std::collections::HashMap<String, String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(client) = get_global_client() {
        client.log(level, message, attrs)
    } else {
        Ok(())
    }
}

pub fn flush_logs_global() {
    if let Some(client) = get_global_client() {
        client.flush_logs();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_cfg() -> Config {
        new_config(
            "http://localhost:29997".to_string(),
            "test-service".to_string(),
            Some("test-token".to_string()),
        )
    }

    // --- MiddleMonitorClient ---

    #[tokio::test(flavor = "multi_thread")]
    async fn client_creation() {
        let client = MiddleMonitorClient::new(make_cfg());
        assert_eq!(client.config.service, "test-service");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn client_creation_init_warning() {
        // init() may fail for localhost:29997 — MiddleMonitorClient::new swallows the error
        let cfg = new_config("http://localhost:29997".to_string(), "svc".to_string(), None);
        let client = MiddleMonitorClient::new(cfg);
        assert_eq!(client.config.service, "svc");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn client_set_token_not_exposed_directly() {
        // config is pub, so we verify token is set in the underlying config
        let mut cfg = make_cfg();
        cfg.token = Some("newtoken".to_string());
        let client = MiddleMonitorClient::new(cfg);
        assert_eq!(client.config.token, Some("newtoken".to_string()));
    }

    // --- is_application_error (tested via report_error) ---

    #[tokio::test(flavor = "multi_thread")]
    async fn report_error_returns_ok_for_framework_path() {
        let client = MiddleMonitorClient::new(make_cfg());
        let result = client.report_error("test error", Some("/.cargo/registry/src/lib.rs".to_string()), Some(10)).await;
        assert!(result.is_ok()); // returns early because is_application_error → false
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn report_error_returns_ok_for_empty_file() {
        let client = MiddleMonitorClient::new(make_cfg());
        let result = client.report_error("test error", Some("".to_string()), Some(10)).await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn report_error_returns_ok_for_test_file() {
        let client = MiddleMonitorClient::new(make_cfg());
        let result = client.report_error("err", Some("src/handler_test.rs".to_string()), Some(5)).await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn report_error_returns_ok_for_gen_file() {
        let client = MiddleMonitorClient::new(make_cfg());
        let result = client.report_error("err", Some("src/proto.gen.rs".to_string()), Some(5)).await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn report_error_user_file() {
        let client = MiddleMonitorClient::new(make_cfg());
        let result = client.report_error("user error", Some("src/handler.rs".to_string()), Some(42)).await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn report_error_no_file() {
        let client = MiddleMonitorClient::new(make_cfg());
        let result = client.report_error("err", None, None).await;
        assert!(result.is_ok());
    }

    // --- report_custom_error ---

    #[tokio::test(flavor = "multi_thread")]
    async fn report_custom_error_framework_path() {
        let client = MiddleMonitorClient::new(make_cfg());
        let result = client.report_custom_error("DBError", "conn failed", "/.cargo/lib.rs", 10).await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn report_custom_error_user_path() {
        let client = MiddleMonitorClient::new(make_cfg());
        let result = client.report_custom_error("APIError", "timeout", "src/api.rs", 99).await;
        assert!(result.is_ok());
    }

    // --- report_custom_error_with_http ---

    #[tokio::test(flavor = "multi_thread")]
    async fn report_custom_error_with_http_framework_path() {
        let client = MiddleMonitorClient::new(make_cfg());
        let result = client.report_custom_error_with_http(
            "E", "msg", "/.cargo/lib.rs", 0, None, None, None, None,
        ).await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn report_custom_error_with_http_user_path() {
        let client = MiddleMonitorClient::new(make_cfg());
        let result = client.report_custom_error_with_http(
            "APIError", "timeout", "src/api.rs", 10,
            Some("POST"), Some("/api/data"), Some("Content-Type: json"), Some("{}"),
        ).await;
        assert!(result.is_ok());
    }

    // --- capture_panic ---

    #[tokio::test(flavor = "multi_thread")]
    async fn capture_panic_with_str_payload() {
        let client = MiddleMonitorClient::new(make_cfg());
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("test panic message");
        })).map_err(|payload| {
            // Create a minimal PanicInfo is not possible directly; use the old panic hook
            let _ = &payload; // payload is Box<dyn Any>
        }).ok();
        // Just call capture_panic directly with a custom PanicInfo — can't construct one directly
        // Instead, test via panic hook
        let client_arc = std::sync::Arc::new(client);
        let client_clone = client_arc.clone();
        let old_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            client_clone.capture_panic(info);
        }));
        let _ = std::panic::catch_unwind(|| panic!("str payload"));
        std::panic::set_hook(old_hook);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn capture_panic_with_string_payload() {
        let client = MiddleMonitorClient::new(make_cfg());
        let client_arc = std::sync::Arc::new(client);
        let client_clone = client_arc.clone();
        let old_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            client_clone.capture_panic(info);
        }));
        let _ = std::panic::catch_unwind(|| panic!("{}", "owned string payload"));
        std::panic::set_hook(old_hook);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn capture_panic_with_unknown_payload_type() {
        let client = MiddleMonitorClient::new(make_cfg());
        let client_arc = std::sync::Arc::new(client);
        let client_clone = client_arc.clone();
        let old_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            client_clone.capture_panic(info);
        }));
        // panic_any with an integer: neither &str nor String → else branch
        let _ = std::panic::catch_unwind(|| std::panic::panic_any(42u32));
        std::panic::set_hook(old_hook);
    }

    // --- log ---

    #[tokio::test(flavor = "multi_thread")]
    async fn log_all_levels() {
        let client = MiddleMonitorClient::new(make_cfg());
        for level in &[LogLevel::Debug, LogLevel::Info, LogLevel::Warn, LogLevel::Error, LogLevel::Fatal, LogLevel::Panic] {
            let _ = client.log(level, "msg", None);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn log_with_attrs() {
        let client = MiddleMonitorClient::new(make_cfg());
        let mut attrs = HashMap::new();
        attrs.insert("k".to_string(), "v".to_string());
        let _ = client.log(&LogLevel::Info, "msg", Some(&attrs));
    }

    // --- flush_logs ---

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_logs_does_not_panic() {
        let client = MiddleMonitorClient::new(make_cfg());
        client.flush_logs();
    }

    // --- submit_application_error ---

    #[tokio::test(flavor = "multi_thread")]
    async fn submit_application_error_all_params() {
        let client = MiddleMonitorClient::new(make_cfg());
        let _ = client.submit_application_error("err", "msg", "f.rs", 10, 500, Some("GET"), Some("/api"), Some("body")).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn submit_application_error_minimal() {
        let client = MiddleMonitorClient::new(make_cfg());
        let _ = client.submit_application_error("err", "msg", "f.rs", 0, 500, None, None, None).await;
    }

    // --- shutdown ---

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_does_not_panic() {
        let client = MiddleMonitorClient::new(make_cfg());
        client.shutdown();
    }

    // --- Global API ---
    // Note: INIT_ONCE is a static Once — once called, subsequent init() calls are no-ops.
    // All global tests below work correctly regardless of call order.

    #[tokio::test(flavor = "multi_thread")]
    async fn get_global_client_returns_some() {
        // get_global_client() triggers init(None) if not already initialized
        let client = get_global_client();
        assert!(client.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_global_config_returns_some() {
        let cfg = get_global_config();
        assert!(cfg.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn global_report_error_ok() {
        let result = report_error("some global error").await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn global_report_error_with_details_ok() {
        let result = report_error_with_details("err", "src/main.rs", 10).await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn global_log_ok() {
        let result = log_global(&LogLevel::Info, "hello", None);
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn global_flush_logs_does_not_panic() {
        flush_logs_global();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn init_with_config_is_no_op_after_first_init() {
        init_with_config(
            "http://localhost:29997".to_string(),
            "override-svc".to_string(),
            None,
        );
        // The INIT_ONCE means this is a no-op — global client stays unchanged
        let client = get_global_client();
        assert!(client.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn init_simple_is_no_op_after_first_init() {
        init_simple();
        let client = get_global_client();
        assert!(client.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn capture_panic_global_does_not_panic() {
        let old_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|info| {
            capture_panic_global(info);
        }));
        let _ = std::panic::catch_unwind(|| panic!("global panic test"));
        std::panic::set_hook(old_hook);
    }
}
