# Middle-Monitor Rust SDK

Rust SDK for capturing and reporting errors to Middle-Monitor.

**Documentation:** [middlemonitor.io/docs#sdk](https://middlemonitor.io/docs#sdk)

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
middle-monitor-sdk = { git = "https://github.com/middle-monitor/sdk-rust.git" }
```

Or from a local path:

```toml
[dependencies]
middle-monitor-sdk = { path = "../sdks/rust" }
```

## Usage

### Basic setup

```rust
use middle_monitor_sdk::{new_config, MiddleMonitorClient};

#[tokio::main]
async fn main() {
    let client = MiddleMonitorClient::new(new_config(
        "https://api.middlemonitor.io".to_string(),
        "my-service".to_string(),
        Some("your_token".to_string()),
    ));

    match risky_operation() {
        Ok(_) => println!("Success"),
        Err(e) => {
            client.report_error(&e.to_string(), None, None).await.ok();
        }
    }
}
```

### Custom error

```rust
client.report_custom_error(
    "DatabaseError",
    "Failed to connect to database",
    "/path/to/db.rs",
    123,
).await?;
```

### Axum middleware

Enable the `axum` feature:

```toml
middle-monitor-sdk = { version = "0.1", features = ["axum"] }
```

One line to enable automatic capture: one trace per request, error status on 4xx/5xx, and 5xx responses reported to the Errors view.

```rust
use axum::{middleware::from_fn, routing::get, Router};

middle_monitor_sdk::init_simple();
let app: Router = Router::new()
    .route("/", get(handler))
    .layer(from_fn(middle_monitor_sdk::axum_middleware::middleware));
```

### Environment variables

The SDK automatically reads `MIDDLE_MONITOR_API_URL` if set:

```bash
export MIDDLE_MONITOR_API_URL=https://api.middlemonitor.io
```
