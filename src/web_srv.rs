use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use diesel::prelude::*;
use itertools::Itertools;
use metrics_exporter_prometheus::PrometheusHandle;
use salvo::conn::tcp::TcpListener;
use salvo::server::ServerHandle;
use salvo::writing::{Json, Text};
use salvo::{Listener, Router, Server};
use salvo_oapi::{endpoint, OpenApi};
use tap::Pipe as _;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::db::DbUserId;
use crate::{models, schema};

struct AppState {
    conn: Mutex<SqliteConnection>,
    config: Arc<Config>,
    prometheus: PrometheusHandle,
}

static STATE: OnceLock<AppState> = OnceLock::new();

fn state() -> &'static AppState {
    STATE.get().expect("AppState not initialized")
}

fn server_addr() -> String {
    format!("http://{}", state().config.server_addr)
}

pub async fn run(
    conn: SqliteConnection,
    config: Arc<Config>,
    prometheus: PrometheusHandle,
    cancel: CancellationToken,
) {
    let app_state = AppState {
        conn: Mutex::new(conn),
        config: Arc::clone(&config),
        prometheus,
    };
    STATE.set(app_state).ok().expect("AppState already initialized");

    let router = Router::new()
        .get(get_index)
        .push(Router::with_path("/metrics").get(get_metrics))
        .push(Router::with_path("/residents/v0").get(get_residents_v0))
        .push(Router::with_path("/all_residents/v0").get(get_all_residents_v0))
        .push(Router::with_path("/ssh_keys/v0/all").get(get_all_ssh_keys_v0))
        .push(
            Router::with_path("/ssh_keys/v0/by_user")
                .get(get_ssh_keys_by_user_v0),
        );

    let doc = OpenApi::with_info(
        salvo_oapi::Info::new("Botka HTTP API", "0.1").description(
            "
- Source code: https://github.com/f0rthsp4ce/botka
- Wiki page: https://wiki.f0rth.space/en/residents/telegram-bot
                ",
        ),
    )
    .add_server(salvo_oapi::Server::new(server_addr()))
    .add_path(
        "/openapi.json",
        salvo_oapi::PathItem::new(
            salvo_oapi::PathItemType::Get,
            salvo_oapi::Operation::new().summary("This openapi.json file."),
        ),
    )
    .merge_router(&router);

    let router = router.unshift(doc.into_router("/openapi.json"));

    let acceptor = TcpListener::new(config.server_addr).bind().await;

    let server = Server::new(acceptor);
    let handle: ServerHandle = server.handle();

    tokio::spawn(async move {
        server.serve(router).await;
    });

    cancel.cancelled().await;

    log::info!("Graceful shutdown initiated...");
    handle.stop_graceful(Some(Duration::from_secs(60)));
    log::info!("Server stopped gracefully.");
}

#[salvo::prelude::handler]
async fn get_index() -> Text<String> {
    format!(r#"<!doctype html>
<html>
<head>
    <meta charset="utf-8">
    <title>Botka API</title>
    <script type="module" src="https://unpkg.com/rapidoc/dist/rapidoc-min.js"></script>
</head>
<body>
    <rapi-doc
      allow-authentication="false"
      allow-server-selection="false"
      render-style="view"
      spec-url="{}/openapi.json"
      theme="dark"
    />
</body>
</html>
"#, server_addr())
.pipe(Text::Html)
}

/// Prometheus metrics endpoint.
#[endpoint()]
async fn get_metrics() -> String {
    let state = state();
    let mut conn_guard =
        state.conn.lock().expect("Failed to lock DB connection mutex");
    crate::metrics::refresh(&mut conn_guard);
    drop(conn_guard);
    state.prometheus.render()
}

/// Get a list of current residents.
#[endpoint()]
async fn get_residents_v0() -> Json<Vec<models::DataResident>> {
    let state = state();
    let mut conn_guard =
        state.conn.lock().expect("Failed to lock DB connection mutex");
    let residents: Vec<(DbUserId, models::TgUser)> = schema::residents::table
        .filter(schema::residents::end_date.is_null())
        .inner_join(
            schema::tg_users::table
                .on(schema::residents::tg_id.eq(schema::tg_users::id)),
        )
        .order(schema::residents::begin_date.desc())
        .select((schema::residents::tg_id, schema::tg_users::all_columns))
        .load(&mut *conn_guard)
        .expect("Failed to load residents");
    drop(conn_guard);

    let residents_data = residents
        .into_iter()
        .map(|(id, user)| models::DataResident {
            id: id.into(),
            username: user.username,
            first_name: user.first_name,
            last_name: user.last_name,
        })
        .collect_vec();

    Json(residents_data)
}

/// Get a list of current and past residents.
/// The same resident may appear multiple times if they have left and returned.
#[endpoint()]
async fn get_all_residents_v0() -> Json<Vec<models::Resident>> {
    let state = state();
    let mut conn_guard =
        state.conn.lock().expect("Failed to lock DB connection mutex");
    schema::residents::table
        .order(schema::residents::begin_date.desc())
        .load(&mut *conn_guard)
        .map(Json)
        .expect("Failed to load all residents")
}

/// Get all SSH keys from active residents.
#[endpoint()]
async fn get_all_ssh_keys_v0() -> Json<HashMap<String, Vec<String>>> {
    let state = state();
    let mut conn_guard =
        state.conn.lock().expect("Failed to lock DB connection mutex");
    // Get all SSH keys for active residents
    let ssh_keys: Vec<String> = schema::user_ssh_keys::table
        .inner_join(
            schema::residents::table
                .on(schema::user_ssh_keys::tg_id.eq(schema::residents::tg_id)),
        )
        .filter(schema::residents::end_date.is_null())
        .select(schema::user_ssh_keys::key)
        .load(&mut *conn_guard)
        .unwrap_or_default();
    drop(conn_guard);

    // Format: {"root": ["key1", "key2", ...]}
    let mut result = HashMap::new();
    result.insert("root".to_string(), ssh_keys);

    Json(result)
}

/// Get SSH keys grouped by username from active residents.
#[endpoint()]
async fn get_ssh_keys_by_user_v0() -> Json<HashMap<String, Vec<String>>> {
    let state = state();
    let mut conn_guard =
        state.conn.lock().expect("Failed to lock DB connection mutex");
    // Get all SSH keys, usernames, and resident status
    let ssh_keys: Vec<(Option<String>, String)> = schema::user_ssh_keys::table
        .inner_join(
            schema::tg_users::table
                .on(schema::user_ssh_keys::tg_id.eq(schema::tg_users::id)),
        )
        .inner_join(
            schema::residents::table
                .on(schema::user_ssh_keys::tg_id.eq(schema::residents::tg_id)),
        )
        .filter(schema::residents::end_date.is_null())
        .select((schema::tg_users::username, schema::user_ssh_keys::key))
        .load(&mut *conn_guard)
        .unwrap_or_default();
    drop(conn_guard);

    // Group SSH keys by username
    let mut result = HashMap::new();
    for (username, key) in ssh_keys {
        result
            .entry(username.unwrap_or_else(|| "unknown".to_string()))
            .or_insert_with(Vec::new)
            .push(key);
    }

    Json(result)
}
