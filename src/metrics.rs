#[allow(clippy::module_name_repetitions)] // For conistency with other modules.
pub fn register_metrics() {
    // Descriptions of labeled metrics
    metrics::describe_gauge!(
        "botka_service_access_success",
        "1 if the last access to the service was successful, 0 otherwise."
    );
    metrics::describe_gauge!(
        "botka_service_last_access_timestamp_seconds",
        "UNIX timestamp of the last access to the service."
    );

    // Constant metrics

    // botka_start_time_seconds
    let start_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    metrics::describe_gauge!(
        "botka_start_time_seconds",
        "Unix timestamp of the bot start time."
    );
    metrics::gauge!("botka_start_time_seconds", start_time);

    // botka_build_info
    metrics::describe_gauge!(
        "botka_build_info",
        "A metric with a constant '1' value with the botka build information."
    );
    metrics::gauge!(
        "botka_build_info",
        1.0,
        "revision" => crate::version(),
    );
}

pub fn update_service(name: &'static str, success: bool) {
    metrics::gauge!(
        "botka_service_access_success",
        if success { 1.0 } else { 0.0 },
        "service" => name,
    );
    metrics::gauge!(
        "botka_service_last_access_timestamp_seconds",
        now_seconds_f64(),
        "service" => name,
        "status" => if success { "success" } else { "failure" },
    );
}

fn now_seconds_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
