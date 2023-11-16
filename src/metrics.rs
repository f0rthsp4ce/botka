use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection};

use crate::config::Config;

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

/// Refresh some metrics before dumping them.
#[allow(clippy::cast_precision_loss)] // Rounding errors are fine here.
pub fn refresh(conn: &mut SqliteConnection, config: &Config) {
    // botka_residents
    use crate::schema::residents::dsl as r;
    let resident_count = r::residents
        .filter(r::end_date.is_null())
        .count()
        .get_result::<i64>(conn)
        .unwrap_or_default() as f64;
    metrics::describe_gauge!("botka_residents", "Number of residents.");
    metrics::gauge!("botka_residents", resident_count);

    // botka_db_size_bytes
    let db = &config.db;
    let db_size = std::fs::metadata(db.strip_prefix("sqlite://").unwrap_or(db))
        .map(|m| m.len())
        .unwrap_or_default() as f64;
    metrics::describe_gauge!(
        "botka_db_size_bytes",
        "Size of the database file in bytes."
    );
    metrics::gauge!("botka_db_size_bytes", db_size);
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
