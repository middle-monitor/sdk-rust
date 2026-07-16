use std::env;

#[derive(Debug, Clone, PartialEq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    Panic,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Fatal => "FATAL",
            LogLevel::Panic => "PANIC",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "DEBUG" => Some(LogLevel::Debug),
            "INFO" => Some(LogLevel::Info),
            "WARN" | "WARNING" => Some(LogLevel::Warn),
            "ERROR" => Some(LogLevel::Error),
            "FATAL" => Some(LogLevel::Fatal),
            "PANIC" => Some(LogLevel::Panic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TracesSamplingConfig {
    // -1 = auto (uses the default sampling rate); 0.0–1.0 for an explicit rate
    pub percentage: f64,
    pub always_sample_errors: bool,
    pub always_sample_routes: Vec<String>,
    pub never_sample_routes: Vec<String>,
}

impl Default for TracesSamplingConfig {
    fn default() -> Self {
        Self {
            percentage: -1.0,
            always_sample_errors: true,
            always_sample_routes: vec![],
            never_sample_routes: vec![
                "/health".to_string(),
                "/metrics".to_string(),
                "/ready".to_string(),
                "/healthz".to_string(),
                "/readyz".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogsSamplingConfig {
    pub levels: Vec<LogLevel>,
    pub min_http_status: u16,
    pub capture_on_trace_error: bool,
    pub always_capture_routes: Vec<String>,
    pub never_capture_routes: Vec<String>,
}

impl Default for LogsSamplingConfig {
    fn default() -> Self {
        Self {
            levels: vec![LogLevel::Error, LogLevel::Fatal, LogLevel::Panic],
            min_http_status: 500,
            capture_on_trace_error: true,
            always_capture_routes: vec![],
            never_capture_routes: vec![
                "/health".to_string(),
                "/metrics".to_string(),
                "/ready".to_string(),
                "/healthz".to_string(),
                "/readyz".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone)]
pub struct SamplingConfig {
    pub traces: TracesSamplingConfig,
    pub logs: LogsSamplingConfig,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub endpoint: String,
    // Derived from http:// scheme; disables TLS.
    pub insecure: bool,
    pub service: String,
    pub token: Option<String>,
    pub protocol: String,
    pub sampling: SamplingConfig,
    pub timeout_seconds: u64,
}

pub fn default_sampling_config() -> SamplingConfig {
    SamplingConfig {
        traces: TracesSamplingConfig {
            percentage: 0.10,
            ..TracesSamplingConfig::default()
        },
        logs: LogsSamplingConfig::default(),
    }
}

pub fn new_config(
    endpoint: String,
    service: String,
    token: Option<String>,
) -> Config {
    let ep = endpoint.trim_end_matches('/').to_string();
    let insecure = ep.starts_with("http://");
    Config {
        insecure,
        endpoint: ep,
        service,
        sampling: default_sampling_config(),
        token,
        protocol: "http".to_string(),
        timeout_seconds: 5,
    }
}

pub fn config_from_env() -> Result<Config, String> {
    let endpoint = env::var("MIDDLE_MONITOR_API_URL")
        .or_else(|_| env::var("OTEL_EXPORTER_OTLP_ENDPOINT"))
        .unwrap_or_else(|_| "https://api.middlemonitor.io".to_string());

    let service = env::var("MIDDLE_MONITOR_SERVICE")
        .or_else(|_| env::var("OTEL_SERVICE_NAME"))
        .unwrap_or_else(|_| "unknown".to_string());

    let mut token = env::var("MIDDLE_MONITOR_TOKEN").ok();
    if token.is_none() {
        if let Ok(headers_str) = env::var("OTEL_EXPORTER_OTLP_HEADERS") {
            if headers_str.contains('=') {
                for part in headers_str.split(',') {
                    let kv: Vec<&str> = part.trim().splitn(2, '=').collect();
                    if kv.len() == 2 && kv[0].to_lowercase() == "authorization" {
                        let v = kv[1].trim_start_matches("Bearer ").to_string();
                        token = Some(v);
                        break;
                    }
                }
            }
        }
    }

    let protocol = env::var("MIDDLE_MONITOR_PROTOCOL")
        .or_else(|_| env::var("OTEL_EXPORTER_OTLP_PROTOCOL"))
        .unwrap_or_else(|_| "http".to_string());

    let mut cfg = new_config(endpoint, service, token);
    cfg.protocol = protocol;

    if let Ok(pct_str) = env::var("MIDDLE_MONITOR_TRACES_SAMPLING") {
        let pct: f64 = pct_str
            .parse()
            .map_err(|_| format!("invalid MIDDLE_MONITOR_TRACES_SAMPLING: {}", pct_str))?;
        if pct < -1.0 || pct > 1.0 {
            return Err(format!("MIDDLE_MONITOR_TRACES_SAMPLING must be between -1 and 1, got {}", pct));
        }
        cfg.sampling.traces.percentage = pct;
    }

    if let Ok(levels_str) = env::var("MIDDLE_MONITOR_LOGS_LEVELS") {
        let mut levels = Vec::new();
        for s in levels_str.split(',') {
            let s = s.trim();
            match LogLevel::from_str(s) {
                Some(l) => levels.push(l),
                None => return Err(format!("invalid log level in MIDDLE_MONITOR_LOGS_LEVELS: {}", s)),
            }
        }
        if !levels.is_empty() {
            cfg.sampling.logs.levels = levels;
        }
    }

    if let Ok(min_str) = env::var("MIDDLE_MONITOR_LOGS_MIN_HTTP_STATUS") {
        cfg.sampling.logs.min_http_status = min_str
            .parse()
            .map_err(|_| format!("invalid MIDDLE_MONITOR_LOGS_MIN_HTTP_STATUS: {}", min_str))?;
    }

    Ok(cfg)
}

pub fn should_sample_trace(cfg: &Config, route: &str, has_error: bool) -> bool {
    let traces = &cfg.sampling.traces;

    for pattern in &traces.never_sample_routes {
        if matches_route(route, pattern) {
            if traces.always_sample_errors && has_error {
                return true;
            }
            return false;
        }
    }

    for pattern in &traces.always_sample_routes {
        if matches_route(route, pattern) {
            return true;
        }
    }

    if traces.always_sample_errors && has_error {
        return true;
    }

    let mut pct = traces.percentage;
    if pct < 0.0 {
        pct = default_sampling_config().traces.percentage;
    }

    if pct >= 1.0 {
        return true;
    }
    if pct <= 0.0 {
        return false;
    }
    rand::random::<f64>() < pct
}

pub fn should_sample_log(
    cfg: &Config,
    route: &str,
    level: &LogLevel,
    http_status: u16,
    trace_has_error: bool,
) -> bool {
    let logs = &cfg.sampling.logs;

    for pattern in &logs.never_capture_routes {
        if matches_route(route, pattern) {
            if logs.min_http_status > 0 && http_status >= logs.min_http_status {
                return true;
            }
            return false;
        }
    }

    for pattern in &logs.always_capture_routes {
        if matches_route(route, pattern) {
            return true;
        }
    }

    if logs.min_http_status > 0 && http_status >= logs.min_http_status {
        return true;
    }

    if logs.levels.contains(level) {
        return true;
    }

    if logs.capture_on_trace_error && trace_has_error {
        return true;
    }

    false
}

pub(crate) fn matches_route(route: &str, pattern: &str) -> bool {
    if route == pattern {
        return true;
    }
    if !pattern.contains('*') {
        return false;
    }
    // Simple glob: each segment around '*' must match in order.
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut remaining = route;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !remaining.starts_with(part) {
                return false;
            }
            remaining = &remaining[part.len()..];
        } else if i == parts.len() - 1 {
            return remaining.ends_with(part);
        } else if let Some(pos) = remaining.find(part) {
            remaining = &remaining[pos + part.len()..];
        } else {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        for k in &[
            "MIDDLE_MONITOR_API_URL", "OTEL_EXPORTER_OTLP_ENDPOINT",
            "MIDDLE_MONITOR_SERVICE", "OTEL_SERVICE_NAME",
            "MIDDLE_MONITOR_TOKEN", "OTEL_EXPORTER_OTLP_HEADERS",
            "MIDDLE_MONITOR_PROTOCOL", "OTEL_EXPORTER_OTLP_PROTOCOL",
            "MIDDLE_MONITOR_TRACES_SAMPLING", "MIDDLE_MONITOR_LOGS_LEVELS",
            "MIDDLE_MONITOR_LOGS_MIN_HTTP_STATUS",
        ] {
            std::env::remove_var(k);
        }
    }

    // --- LogLevel ---

    #[test]
    fn log_level_as_str_all_variants() {
        assert_eq!(LogLevel::Debug.as_str(), "DEBUG");
        assert_eq!(LogLevel::Info.as_str(), "INFO");
        assert_eq!(LogLevel::Warn.as_str(), "WARN");
        assert_eq!(LogLevel::Error.as_str(), "ERROR");
        assert_eq!(LogLevel::Fatal.as_str(), "FATAL");
        assert_eq!(LogLevel::Panic.as_str(), "PANIC");
    }

    #[test]
    fn log_level_from_str_valid_variants() {
        assert_eq!(LogLevel::from_str("DEBUG"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::from_str("debug"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::from_str("INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::from_str("WARN"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::from_str("WARNING"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::from_str("ERROR"), Some(LogLevel::Error));
        assert_eq!(LogLevel::from_str("FATAL"), Some(LogLevel::Fatal));
        assert_eq!(LogLevel::from_str("PANIC"), Some(LogLevel::Panic));
    }

    #[test]
    fn log_level_from_str_invalid_returns_none() {
        assert_eq!(LogLevel::from_str("UNKNOWN"), None);
        assert_eq!(LogLevel::from_str(""), None);
        assert_eq!(LogLevel::from_str("trace"), None);
    }

    // --- default_sampling_config ---

    #[test]
    fn default_sampling_percentage() {
        assert!((default_sampling_config().traces.percentage - 0.10).abs() < 1e-10);
    }

    #[test]
    fn default_sampling_includes_never_sample_routes() {
        let cfg = default_sampling_config();
        assert!(cfg.traces.never_sample_routes.contains(&"/health".to_string()));
        assert!(cfg.logs.never_capture_routes.contains(&"/metrics".to_string()));
    }

    // --- new_config ---

    #[test]
    fn new_config_basic_http() {
        let cfg = new_config("http://host:8080".to_string(), "svc".to_string(), None);
        assert_eq!(cfg.endpoint, "http://host:8080");
        assert!(cfg.insecure);
        assert_eq!(cfg.service, "svc");
        assert_eq!(cfg.token, None);
        assert_eq!(cfg.timeout_seconds, 5);
    }

    #[test]
    fn new_config_strips_trailing_slash() {
        let cfg = new_config("http://host:8080/".to_string(), "svc".to_string(), None);
        assert_eq!(cfg.endpoint, "http://host:8080");
    }

    #[test]
    fn new_config_https_not_insecure() {
        let cfg = new_config("https://host:4318".to_string(), "svc".to_string(), Some("tok".to_string()));
        assert!(!cfg.insecure);
        assert_eq!(cfg.token, Some("tok".to_string()));
    }

    // --- config_from_env ---

    #[test]
    fn config_from_env_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        let cfg = config_from_env().unwrap();
        assert_eq!(cfg.endpoint, "https://api.middlemonitor.io");
        assert_eq!(cfg.service, "unknown");
        assert_eq!(cfg.token, None);
    }

    #[test]
    fn config_from_env_middle_monitor_api_url() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("MIDDLE_MONITOR_API_URL", "http://custom:9090");
        let cfg = config_from_env().unwrap();
        assert_eq!(cfg.endpoint, "http://custom:9090");
        std::env::remove_var("MIDDLE_MONITOR_API_URL");
    }

    #[test]
    fn config_from_env_otel_endpoint_fallback() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otel:4318");
        let cfg = config_from_env().unwrap();
        assert_eq!(cfg.endpoint, "http://otel:4318");
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
    }

    #[test]
    fn config_from_env_service_from_otel() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("OTEL_SERVICE_NAME", "my-svc");
        let cfg = config_from_env().unwrap();
        assert_eq!(cfg.service, "my-svc");
        std::env::remove_var("OTEL_SERVICE_NAME");
    }

    #[test]
    fn config_from_env_token_from_middle_monitor() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("MIDDLE_MONITOR_TOKEN", "mytoken");
        let cfg = config_from_env().unwrap();
        assert_eq!(cfg.token, Some("mytoken".to_string()));
        std::env::remove_var("MIDDLE_MONITOR_TOKEN");
    }

    #[test]
    fn config_from_env_token_from_otlp_headers_authorization() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer secret123,x-other=val");
        let cfg = config_from_env().unwrap();
        assert_eq!(cfg.token, Some("secret123".to_string()));
        std::env::remove_var("OTEL_EXPORTER_OTLP_HEADERS");
    }

    #[test]
    fn config_from_env_otlp_headers_without_equals_sign() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("OTEL_EXPORTER_OTLP_HEADERS", "noequals");
        let cfg = config_from_env().unwrap();
        assert_eq!(cfg.token, None);
        std::env::remove_var("OTEL_EXPORTER_OTLP_HEADERS");
    }

    #[test]
    fn config_from_env_otlp_headers_without_authorization() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("OTEL_EXPORTER_OTLP_HEADERS", "x-other=val");
        let cfg = config_from_env().unwrap();
        assert_eq!(cfg.token, None);
        std::env::remove_var("OTEL_EXPORTER_OTLP_HEADERS");
    }

    #[test]
    fn config_from_env_protocol_from_otel() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc");
        let cfg = config_from_env().unwrap();
        assert_eq!(cfg.protocol, "grpc");
        std::env::remove_var("OTEL_EXPORTER_OTLP_PROTOCOL");
    }

    #[test]
    fn config_from_env_traces_sampling_valid() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("MIDDLE_MONITOR_TRACES_SAMPLING", "0.5");
        let cfg = config_from_env().unwrap();
        assert!((cfg.sampling.traces.percentage - 0.5).abs() < 1e-10);
        std::env::remove_var("MIDDLE_MONITOR_TRACES_SAMPLING");
    }

    #[test]
    fn config_from_env_traces_sampling_out_of_range() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("MIDDLE_MONITOR_TRACES_SAMPLING", "2.0");
        let result = config_from_env();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("MIDDLE_MONITOR_TRACES_SAMPLING"));
        std::env::remove_var("MIDDLE_MONITOR_TRACES_SAMPLING");
    }

    #[test]
    fn config_from_env_traces_sampling_not_a_number() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("MIDDLE_MONITOR_TRACES_SAMPLING", "notanumber");
        let result = config_from_env();
        assert!(result.is_err());
        std::env::remove_var("MIDDLE_MONITOR_TRACES_SAMPLING");
    }

    #[test]
    fn config_from_env_logs_levels_valid() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("MIDDLE_MONITOR_LOGS_LEVELS", "DEBUG,WARN");
        let cfg = config_from_env().unwrap();
        assert!(cfg.sampling.logs.levels.contains(&LogLevel::Debug));
        assert!(cfg.sampling.logs.levels.contains(&LogLevel::Warn));
        std::env::remove_var("MIDDLE_MONITOR_LOGS_LEVELS");
    }

    #[test]
    fn config_from_env_logs_levels_invalid() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("MIDDLE_MONITOR_LOGS_LEVELS", "INVALID");
        let result = config_from_env();
        assert!(result.is_err());
        std::env::remove_var("MIDDLE_MONITOR_LOGS_LEVELS");
    }

    #[test]
    fn config_from_env_min_http_status() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("MIDDLE_MONITOR_LOGS_MIN_HTTP_STATUS", "400");
        let cfg = config_from_env().unwrap();
        assert_eq!(cfg.sampling.logs.min_http_status, 400);
        std::env::remove_var("MIDDLE_MONITOR_LOGS_MIN_HTTP_STATUS");
    }

    #[test]
    fn config_from_env_min_http_status_invalid() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("MIDDLE_MONITOR_LOGS_MIN_HTTP_STATUS", "notanumber");
        let result = config_from_env();
        assert!(result.is_err());
        std::env::remove_var("MIDDLE_MONITOR_LOGS_MIN_HTTP_STATUS");
    }

    // --- should_sample_trace ---

    fn make_cfg_traces(percentage: f64, always_errors: bool, always: Vec<&str>, never: Vec<&str>) -> Config {
        let mut cfg = new_config("http://h".to_string(), "s".to_string(), None);
        cfg.sampling.traces.percentage = percentage;
        cfg.sampling.traces.always_sample_errors = always_errors;
        cfg.sampling.traces.always_sample_routes = always.into_iter().map(|s| s.to_string()).collect();
        cfg.sampling.traces.never_sample_routes = never.into_iter().map(|s| s.to_string()).collect();
        cfg
    }

    #[test]
    fn should_sample_trace_never_route_no_error() {
        let cfg = make_cfg_traces(1.0, false, vec![], vec!["/health"]);
        assert!(!should_sample_trace(&cfg, "/health", false));
    }

    #[test]
    fn should_sample_trace_never_route_with_error_and_always_sample_errors() {
        let cfg = make_cfg_traces(1.0, true, vec![], vec!["/health"]);
        assert!(should_sample_trace(&cfg, "/health", true));
    }

    #[test]
    fn should_sample_trace_never_route_with_error_but_no_always_sample() {
        let cfg = make_cfg_traces(1.0, false, vec![], vec!["/health"]);
        assert!(!should_sample_trace(&cfg, "/health", true));
    }

    #[test]
    fn should_sample_trace_always_route_match() {
        let cfg = make_cfg_traces(0.0, false, vec!["/admin"], vec![]);
        assert!(should_sample_trace(&cfg, "/admin", false));
    }

    #[test]
    fn should_sample_trace_always_route_no_match() {
        let cfg = make_cfg_traces(0.0, false, vec!["/admin"], vec![]);
        assert!(!should_sample_trace(&cfg, "/other", false));
    }

    #[test]
    fn should_sample_trace_always_sample_errors_with_error() {
        let cfg = make_cfg_traces(0.0, true, vec![], vec![]);
        assert!(should_sample_trace(&cfg, "/api", true));
    }

    #[test]
    fn should_sample_trace_percentage_100() {
        let cfg = make_cfg_traces(1.0, false, vec![], vec![]);
        assert!(should_sample_trace(&cfg, "/api", false));
    }

    #[test]
    fn should_sample_trace_percentage_0() {
        let cfg = make_cfg_traces(0.0, false, vec![], vec![]);
        assert!(!should_sample_trace(&cfg, "/api", false));
    }

    #[test]
    fn should_sample_trace_auto_resolves_to_default() {
        // -1 (auto) resolves to the fixed default rate, independent of any environment
        assert!((default_sampling_config().traces.percentage - 0.10).abs() < 1e-10);
    }

    #[test]
    fn should_sample_trace_wildcard_route() {
        let cfg = make_cfg_traces(1.0, false, vec![], vec!["/api/users/*"]);
        assert!(!should_sample_trace(&cfg, "/api/users/123", false));
    }

    // --- should_sample_log ---

    fn make_cfg_logs(
        levels: Vec<LogLevel>,
        min: u16,
        capture_on_trace: bool,
        always: Vec<&str>,
        never: Vec<&str>,
    ) -> Config {
        let mut cfg = new_config("http://h".to_string(), "s".to_string(), None);
        cfg.sampling.logs.levels = levels;
        cfg.sampling.logs.min_http_status = min;
        cfg.sampling.logs.capture_on_trace_error = capture_on_trace;
        cfg.sampling.logs.always_capture_routes = always.into_iter().map(|s| s.to_string()).collect();
        cfg.sampling.logs.never_capture_routes = never.into_iter().map(|s| s.to_string()).collect();
        cfg
    }

    #[test]
    fn should_sample_log_never_route_below_status() {
        let cfg = make_cfg_logs(vec![], 500, false, vec![], vec!["/health"]);
        assert!(!should_sample_log(&cfg, "/health", &LogLevel::Info, 200, false));
    }

    #[test]
    fn should_sample_log_never_route_above_status() {
        let cfg = make_cfg_logs(vec![], 500, false, vec![], vec!["/health"]);
        assert!(should_sample_log(&cfg, "/health", &LogLevel::Info, 500, false));
    }

    #[test]
    fn should_sample_log_never_route_min_http_status_0() {
        let cfg = make_cfg_logs(vec![], 0, false, vec![], vec!["/health"]);
        assert!(!should_sample_log(&cfg, "/health", &LogLevel::Error, 500, false));
    }

    #[test]
    fn should_sample_log_always_route_match() {
        let cfg = make_cfg_logs(vec![], 500, false, vec!["/api"], vec![]);
        assert!(should_sample_log(&cfg, "/api", &LogLevel::Debug, 200, false));
    }

    #[test]
    fn should_sample_log_always_route_no_match() {
        let cfg = make_cfg_logs(vec![], 500, false, vec!["/api"], vec![]);
        assert!(!should_sample_log(&cfg, "/other", &LogLevel::Debug, 200, false));
    }

    #[test]
    fn should_sample_log_min_http_status_hit() {
        let cfg = make_cfg_logs(vec![], 500, false, vec![], vec![]);
        assert!(should_sample_log(&cfg, "/api", &LogLevel::Debug, 500, false));
    }

    #[test]
    fn should_sample_log_level_match() {
        let cfg = make_cfg_logs(vec![LogLevel::Error], 500, false, vec![], vec![]);
        assert!(should_sample_log(&cfg, "/api", &LogLevel::Error, 200, false));
    }

    #[test]
    fn should_sample_log_capture_on_trace_error() {
        let cfg = make_cfg_logs(vec![], 500, true, vec![], vec![]);
        assert!(should_sample_log(&cfg, "/api", &LogLevel::Debug, 200, true));
    }

    #[test]
    fn should_sample_log_nothing_matches() {
        let cfg = make_cfg_logs(vec![LogLevel::Error], 500, false, vec![], vec![]);
        assert!(!should_sample_log(&cfg, "/api", &LogLevel::Info, 200, false));
    }

    // --- matches_route ---

    #[test]
    fn matches_route_exact() {
        assert!(matches_route("/health", "/health"));
        assert!(!matches_route("/health", "/healthy"));
    }

    #[test]
    fn matches_route_no_wildcard_no_match() {
        assert!(!matches_route("/api/users/123", "/api/users"));
    }

    #[test]
    fn matches_route_wildcard_suffix() {
        assert!(matches_route("/api/users/123", "/api/users/*"));
        assert!(!matches_route("/api/items/123", "/api/users/*"));
    }

    #[test]
    fn matches_route_wildcard_prefix_not_matches() {
        assert!(!matches_route("/api/users/123", "*/admin"));
    }

    #[test]
    fn matches_route_wildcard_middle() {
        assert!(matches_route("/api/users/123/profile", "/api/*/profile"));
    }
}
