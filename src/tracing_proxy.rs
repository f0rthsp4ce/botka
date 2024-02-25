use std::convert::Infallible;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use hyper::server::conn::AddrIncoming;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, Uri};
use reqwest::Client;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::utils::parse_tgapi_method;

struct Proxy {
    client: Client,
    log_file: Mutex<File>,
}

#[derive(serde::Deserialize, Debug)]
struct GetUpdatesResponse {
    #[allow(unused)]
    ok: bool,
    result: Vec<serde_json::Value>,
}

/// Start a proxy server that forwards requests to the Telegram API and logs
/// getUpdates responses, as well as all other requests and responses.
/// Returns the URL of the proxy server.
pub async fn start() -> Result<reqwest::Url> {
    // Make client from teloxide::net::default_reqwest_settings, plus 3 seconds.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5 + 3))
        .timeout(Duration::from_secs(17 + 3))
        .tcp_nodelay(true)
        .build()?;

    let log_file = Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(crate::TRACE_FILENAME)?,
    );

    let proxy = Arc::new(Proxy { client, log_file });

    let listener =
        TcpListener::bind(&SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let local_addr = listener.local_addr()?;
    let server = Server::builder(AddrIncoming::from_listener(listener)?).serve(
        make_service_fn(move |_conn| {
            let proxy = Arc::clone(&proxy);
            async {
                Ok::<_, Infallible>(service_fn(move |req| {
                    handle_request(req, Arc::clone(&proxy))
                }))
            }
        }),
    );
    tokio::spawn(async move {
        if let Err(e) = server.await {
            log::error!("Proxy server error: {e}");
        }
    });

    Ok(reqwest::Url::parse(&format!("http://{local_addr}"))?)
}

/// Naming convention used:
/// ```text
/// ┌────────┐┏━━━━━━━━━━━━━┓┌─────────┐
/// │Teloxide│┃Tracing Proxy┃│Telegram │
/// └───┬────┘┗━━━━━━┳━━━━━━┛└────┬────┘
///     │ in_request ┃            │     
///     │───────────>┃            │     
///     │            ┃out_request │     
///     │            ┃───────────>│     
///     │            ┃out_response│     
///     │            ┃<───────────│     
///     │in_response ┃            │     
///     │<───────────┃            │     
/// ```
async fn handle_request(
    in_request: Request<Body>,
    proxy: Arc<Proxy>,
) -> Result<Response<Body>> {
    let now = std::time::UNIX_EPOCH.elapsed().unwrap_or_default().as_secs();

    let (mut in_request_parts, in_request_body) = in_request.into_parts();
    in_request_parts.uri = Uri::builder()
        .scheme("https")
        .authority("api.telegram.org")
        .path_and_query(in_request_parts.uri.path_and_query().unwrap().as_str())
        .build()
        .unwrap();
    in_request_parts
        .headers
        .insert("host", "api.telegram.org".parse().expect("host header"));

    let method = parse_tgapi_method(
        in_request_parts.uri.path_and_query().unwrap().path(),
    )
    .map(|s| s.to_owned());
    let in_request_body = hyper::body::to_bytes(in_request_body).await?;
    let in_request_body_json =
        serde_json::from_slice::<serde_json::Value>(in_request_body.as_ref())
            .ok();

    let out_request: reqwest::Request =
        Request::from_parts(in_request_parts, in_request_body).try_into()?;
    let out_response = match proxy.client.execute(out_request).await {
        Ok(response) => response,
        Err(error) => {
            log::error!("{}", error.without_url());
            return Ok(Response::builder()
                .status(500)
                .body(Body::from("Internal Server Error"))
                .unwrap());
        }
    };

    // Convert the reqwest Response to hyper Response
    let out_response_status = out_response.status();
    let out_response_version = out_response.version();
    let out_response_headers = out_response.headers().clone();
    let out_response_body = out_response.bytes().await?;

    if method.as_deref() == Some("GetUpdates") {
        // Flatten updates
        if let Ok(response_body) =
            serde_json::from_slice::<GetUpdatesResponse>(&out_response_body)
        {
            crate::metrics::update_service("telegram", true);
            append_values_to_log_file(&proxy, response_body.result.iter())
                .await;
        } else {
            crate::metrics::update_service("telegram", false);
        }
    } else {
        // Log request and response
        append_values_to_log_file(
            &proxy,
            std::iter::once(serde_json::json!({
                "__f0bot": "v0",
                "date": now,
                "method": method,
                "status": out_response_status.as_u16(),
                "request": in_request_body_json,
                "response": serde_json::from_slice::<serde_json::Value>(
                    out_response_body.as_ref(),
                ).ok(),
            })),
        )
        .await;
    }

    let mut in_response = Response::new(out_response_body.into());
    *in_response.status_mut() = out_response_status;
    *in_response.headers_mut() = out_response_headers;
    *in_response.version_mut() = out_response_version;

    Ok(in_response)
}

async fn append_values_to_log_file(
    proxy: &Proxy,
    values: impl Iterator<Item = impl serde::Serialize> + Send,
) {
    let mut log_file = proxy.log_file.lock().await;
    for value in values {
        serde_json::to_writer(&mut *log_file, &value).unwrap();
        log_file.write_all(b"\n").unwrap();
    }
    log_file.flush().unwrap();
}
