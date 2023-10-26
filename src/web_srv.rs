use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use diesel::prelude::*;
use itertools::Itertools;
use metrics_exporter_prometheus::PrometheusHandle;
use tokio_util::sync::CancellationToken;

use crate::db::DbUserId;
use crate::{models, schema};

struct AppState {
    conn: Mutex<SqliteConnection>,
    config: Arc<models::Config>,
    prometheus: PrometheusHandle,
}

pub async fn run(
    conn: SqliteConnection,
    config: Arc<models::Config>,
    prometheus: PrometheusHandle,
    cancel: CancellationToken,
) {
    let app_state = Arc::new(AppState {
        conn: Mutex::new(conn),
        config: config.clone(),
        prometheus,
    });

    let app = Router::new()
        .route("/metrics", get(metrics))
        .route("/residents/v0", get(residents_v0))
        .route("/all_residents/v0", get(get_all_residents_v0))
        .with_state(app_state);

    axum::Server::bind(&config.server_addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(cancel.cancelled())
        .await
        .unwrap();
}

#[allow(clippy::cast_precision_loss)]
async fn metrics(State(state): State<Arc<AppState>>) -> String {
    let resident_count = schema::residents::table
        .filter(schema::residents::end_date.is_null())
        .count()
        .get_result::<i64>(&mut *state.conn.lock().unwrap())
        .unwrap_or_default() as f64;
    metrics::describe_gauge!("botka_residents", "Number of residents.");
    metrics::gauge!("botka_residents", resident_count);

    let db_size = std::fs::metadata(
        state.config.db.strip_prefix("sqlite://").unwrap_or(&state.config.db),
    )
    .map(|m| m.len())
    .unwrap_or_default() as f64;
    metrics::describe_gauge!(
        "botka_db_size_bytes",
        "Size of the database file in bytes."
    );
    metrics::gauge!("botka_db_size_bytes", db_size);

    state.prometheus.render()
}

async fn residents_v0(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Vec<models::DataResident>>) {
    let residents: Vec<(DbUserId, models::TgUser)> = schema::residents::table
        .filter(schema::residents::end_date.is_null())
        .inner_join(
            schema::tg_users::table
                .on(schema::residents::tg_id.eq(schema::tg_users::id)),
        )
        .order(schema::residents::tg_id.asc())
        .select((schema::residents::tg_id, schema::tg_users::all_columns))
        .load(&mut *state.conn.lock().unwrap())
        .unwrap();

    let residents = residents
        .into_iter()
        .map(|(id, user)| models::DataResident {
            id: id.into(),
            username: user.username,
            first_name: user.first_name,
            last_name: user.last_name,
        })
        .collect_vec();

    (StatusCode::OK, Json(residents))
}

async fn get_all_residents_v0(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Vec<models::Resident>>) {
    let residents: Vec<models::Resident> = schema::residents::table
        .load(&mut *state.conn.lock().unwrap())
        .unwrap();
    (StatusCode::OK, Json(residents))
}
