#[allow(clippy::module_name_repetitions)] // For conistency with other modules.
pub fn register_metrics() {
    metrics::describe_gauge!(
        "botka_service_access_success",
        "1 if the last access to the service was successful, 0 otherwise."
    );
    metrics::describe_gauge!(
        "botka_service_last_access_timestamp_seconds",
        "UNIX timestamp of the last access to the service."
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
