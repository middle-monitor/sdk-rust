# Middle-Monitor Rust SDK

Rust SDK for capturing and reporting errors to Middle-Monitor.

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
use middle_monitor::MiddleMonitorClient;

#[tokio::main]
async fn main() {
    let client = MiddleMonitorClient::new(
        Some("http://localhost:8080".to_string()),
        "my-service".to_string(),
        "production".to_string(),
    );

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

### Environment variables

The SDK automatically reads `MIDDLE_MONITOR_API_URL` if set:

```bash
export MIDDLE_MONITOR_API_URL=http://monitor.example.com
```
