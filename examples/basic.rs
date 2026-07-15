use middle_monitor_sdk::{init_with_config, get_global_client, LogLevel, log_global};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_with_config(
        "http://localhost:8080".to_string(),
        "example-service".to_string(),
        None,
    );

    let client = get_global_client().expect("client not initialized");

    // Report a simple error
    match risky_operation() {
        Ok(_) => println!("Success"),
        Err(e) => {
            client.report_error(&e.to_string(), None, None).await?;
        }
    }

    // Report a named error with file and line context
    client
        .report_custom_error(
            "DatabaseError",
            "Failed to connect to database",
            "/path/to/db.rs",
            123,
        )
        .await?;

    // Structured log
    log_global(&LogLevel::Info, "Service started", None)?;
    log_global(
        &LogLevel::Error,
        "Connection refused",
        Some(&{
            let mut m = std::collections::HashMap::new();
            m.insert("host".to_string(), "db.internal".to_string());
            m
        }),
    )?;

    client.flush_logs();

    Ok(())
}

fn risky_operation() -> Result<(), String> {
    Err("Something went wrong".to_string())
}
