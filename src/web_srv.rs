use std::sync::{Arc, Mutex, OnceLock};

use diesel::prelude::*;
use itertools::Itertools;
use metrics_exporter_prometheus::PrometheusHandle;
use salvo::prelude::{
    handler, Json, Listener, Response, Router, Server, TcpListener,
};
use tokio_util::sync::CancellationToken;

use crate::db::DbUserId;
use crate::{models, schema};

struct AppState {
    conn: Mutex<SqliteConnection>,
    config: Arc<models::Config>,
    prometheus: PrometheusHandle,
}

static STATE: OnceLock<AppState> = OnceLock::new();

fn state() -> &'static AppState {
    STATE.get().expect("AppState not initialized")
}

pub async fn run(
    conn: SqliteConnection,
    config: Arc<models::Config>,
    prometheus: PrometheusHandle,
    cancel: CancellationToken,
) {
    let app_state =
        AppState { conn: Mutex::new(conn), config: config.clone(), prometheus };
    STATE.set(app_state).ok().expect("AppState already initialized");

    let router = Router::new()
        .push(Router::with_path("/metrics").get(get_metrics))
        .push(Router::with_path("/residents/v0").get(get_residents_v0))
        .push(Router::with_path("/all_residents/v0").get(get_all_residents_v0));

    let listener = TcpListener::new(config.server_addr).bind().await;
    Server::new(listener)
        .serve_with_graceful_shutdown(
            router,
            async move { cancel.cancelled().await },
            None,
        )
        .await;
}

#[handler]
async fn get_metrics() -> String {
    let resident_count = schema::residents::table
        .filter(schema::residents::end_date.is_null())
        .count()
        .get_result::<i64>(&mut *state().conn.lock().unwrap())
        .unwrap_or_default() as f64;
    metrics::describe_gauge!("botka_residents", "Number of residents.");
    metrics::gauge!("botka_residents", resident_count);

    let db = &state().config.db;
    let db_size = std::fs::metadata(db.strip_prefix("sqlite://").unwrap_or(db))
        .map(|m| m.len())
        .unwrap_or_default() as f64;
    metrics::describe_gauge!(
        "botka_db_size_bytes",
        "Size of the database file in bytes."
    );
    metrics::gauge!("botka_db_size_bytes", db_size);

    state().prometheus.render()
}

#[handler]
async fn get_residents_v0(res: &mut Response) {
    let residents: Vec<(DbUserId, models::TgUser)> = schema::residents::table
        .filter(schema::residents::end_date.is_null())
        .inner_join(
            schema::tg_users::table
                .on(schema::residents::tg_id.eq(schema::tg_users::id)),
        )
        .order(schema::residents::tg_id.asc())
        .select((schema::residents::tg_id, schema::tg_users::all_columns))
        .load(&mut *state().conn.lock().unwrap())
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

    res.render(Json(residents));
}

#[handler]
async fn get_all_residents_v0(res: &mut Response) {
    let residents: Vec<models::Resident> = schema::residents::table
        .load(&mut *state().conn.lock().unwrap())
        .unwrap();
    res.render(Json(residents));
}
