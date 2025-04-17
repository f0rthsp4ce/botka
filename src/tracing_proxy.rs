use std::convert::Infallible;
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming as HyperIncomingBody;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{
    Request as HyperRequest, Response as HyperResponse, StatusCode, Uri,
};
use hyper_util::rt::TokioIo;
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

/// Starts a proxy server that forwards requests to the Telegram API,
/// logs getUpdates responses and all other requests/responses.
/// Returns the proxy server URL.
pub async fn start() -> Result<reqwest::Url> {
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
    let proxy_url = reqwest::Url::parse(&format!("http://{local_addr}"))?;
    log::info!("Proxy server listening on {local_addr}");

    tokio::spawn(async move {
        loop {
            let (tcp_stream, remote_addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    log::error!("Error accepting connection: {e}");
                    continue;
                }
            };
            log::debug!("Accepted connection from {remote_addr}");

            let io = TokioIo::new(tcp_stream);
            let proxy_clone = Arc::clone(&proxy);

            // Create and run a task to handle a single connection
            tokio::spawn(async move {
                let http_builder = http1::Builder::new();
                let conn_fut = http_builder.serve_connection(
                    io,
                    service_fn(move |req| {
                        service_wrapper(req, Arc::clone(&proxy_clone))
                    }),
                );

                // Execute the connection future
                if let Err(err) = conn_fut.await {
                    // Check if the error is an I/O error
                    let is_io_error = err
                        .source()
                        .is_some_and(|source| source.is::<std::io::Error>());

                    // Log only connection errors that are not I/O errors
                    // or incomplete messages (often caused by the client closing the connection)
                    if !err.is_incomplete_message() && !is_io_error {
                        log::error!(
                            "Error serving connection from {remote_addr}: {err}"
                        );
                    } else {
                        log::debug!(
                            "Connection closed or I/O error from {remote_addr}: {err}"
                        );
                    }
                }
            });
        }
    });

    Ok(proxy_url)
}

// Wrapper for handle_request that catches errors and converts them
// to a standard 500 Internal Server Error response.
// Returns Result<Response, Infallible>, as required by service_fn.
async fn service_wrapper(
    req: HyperRequest<HyperIncomingBody>,
    proxy: Arc<Proxy>,
) -> Result<HyperResponse<Full<Bytes>>, Infallible> {
    match handle_request(req, proxy).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            log::error!("Request handler error: {e:?}");
            // Create a 500 response
            let response = HyperResponse::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(
                    hyper::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )
                .body(Full::new(Bytes::from("Internal Server Error")))
                .unwrap_or_else(|_| {
                    // This error is unlikely, but just in case
                    HyperResponse::new(Full::new(Bytes::from(
                        "Internal Server Error",
                    )))
                });
            Ok(response)
        }
    }
}

/// Handles an incoming request to the proxy.
/// Signature changed: accepts Request<hyper::body::Incoming>
/// and returns Result<Response<Full<Bytes>>>.
async fn handle_request(
    in_request: HyperRequest<HyperIncomingBody>,
    proxy: Arc<Proxy>,
) -> Result<HyperResponse<Full<Bytes>>> // Returns a specific body type
{
    let now = std::time::UNIX_EPOCH.elapsed().unwrap_or_default().as_secs();

    let (in_request_parts, in_request_body) = in_request.into_parts();

    // --- URI and header modification logic ---
    // Clone necessary parts before modifying or passing them
    let original_method = in_request_parts.method.clone();
    let mut original_headers = in_request_parts.headers.clone();
    let original_uri = in_request_parts.uri.clone();

    let original_path_and_query =
        original_uri.path_and_query().map_or("/", |pq| pq.as_str()); // Handle case with no path/query

    // Create a new URI for the request to Telegram
    let target_uri = Uri::builder()
        .scheme("https")
        .authority("api.telegram.org")
        .path_and_query(original_path_and_query)
        .build()?; // Use ? to handle error

    // Modify headers for the request to Telegram
    original_headers.remove(hyper::header::HOST);
    original_headers.insert(
        hyper::header::HOST,
        "api.telegram.org".parse().expect("Static value is valid header"),
    );
    // --- End of URI and header modification logic ---

    let method = parse_tgapi_method(target_uri.path()).map(|s| s.to_owned());

    // --- Assemble the body of the incoming request ---
    // Use BodyExt::collect to gather body data into Bytes
    let in_request_body_bytes = match in_request_body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            log::error!("Failed to collect incoming request body: {e}");
            // Return an error that will be caught in service_wrapper
            return Err(
                anyhow::Error::new(e).context("Failed to collect request body")
            );
        }
    };
    let in_request_body_json =
        serde_json::from_slice::<serde_json::Value>(&in_request_body_bytes)
            .ok();
    // --- End of body assembly ---

    // --- Create and send the outgoing request via reqwest ---
    // Manually create reqwest::Request using reqwest::Client and its builder

    // Convert hyper::Uri to reqwest::Url
    // Use target_uri, which is already assembled for Telegram
    let out_url = reqwest::Url::parse(&target_uri.to_string())?;

    // Build the reqwest request
    let out_request = proxy
        .client
        .request(original_method, out_url) // Use method and URL
        .headers(original_headers) // Use modified headers
        .body(in_request_body_bytes.clone()) // Use collected body (clone Bytes)
        .build()?; // Build the request

    // Send the request
    let out_response = match proxy.client.execute(out_request).await {
        Ok(response) => response,
        Err(error) => {
            // Log the error before moving it into anyhow::Error
            log::error!("Reqwest error: {}", &error);
            return Err(
                anyhow::Error::new(error).context("Upstream request failed")
            );
        }
    };
    // --- End of request sending ---

    // --- Handle the response from Telegram ---
    let out_response_status = out_response.status();
    let out_response_version = out_response.version();
    let out_response_headers = out_response.headers().clone();
    let out_response_body_bytes = match out_response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            log::error!("Failed to read upstream response body: {e}");
            return Err(
                anyhow::Error::new(e).context("Failed to read response body")
            );
        }
    };
    // --- End of response handling ---

    // --- Logging ---
    if method.as_deref() == Some("GetUpdates") {
        if let Ok(response_body) = serde_json::from_slice::<GetUpdatesResponse>(
            &out_response_body_bytes,
        ) {
            crate::metrics::update_service("telegram", true);
            // Pass an iterator of references to Vec elements
            append_values_to_log_file(&proxy, response_body.result.iter())
                .await;
        } else {
            crate::metrics::update_service("telegram", false);
            log::warn!("Failed to parse GetUpdates response");
        }
    } else {
        let response_json = serde_json::from_slice::<serde_json::Value>(
            &out_response_body_bytes,
        )
        .ok();
        // Create a JSON object for logging
        let log_entry = serde_json::json!({
            "__f0bot": "v0",
            "date": now,
            "method": method,
            "status": out_response_status.as_u16(),
            "request": in_request_body_json, // Already Option<Value>
            "response": response_json, // Already Option<Value>
        });
        // Pass an iterator of a single element (reference to the created object)
        append_values_to_log_file(&proxy, std::iter::once(&log_entry)).await;
    }
    // --- End of logging ---

    // --- Create the response for the client (Teloxide) ---
    // Create hyper::Response with body Full<Bytes>
    let mut in_response =
        HyperResponse::new(Full::new(out_response_body_bytes));
    *in_response.status_mut() = out_response_status;
    *in_response.headers_mut() = out_response_headers;
    *in_response.version_mut() = out_response_version;
    // --- End of response creation ---

    Ok(in_response)
}

// Function append_values_to_log_file changed to fix E0562 error
async fn append_values_to_log_file<'a>(
    proxy: &Proxy,
    // Accept an iterator of references to serializable values
    values: impl Iterator<Item = &'a (impl serde::Serialize + Sync + 'a)> + Send,
) {
    // Lock the mutex asynchronously
    let mut log_file_guard = proxy.log_file.lock().await;
    let mut write_error = false;

    // Write logic moved back into the loop to avoid `impl Trait` in closure parameters
    for value in values {
        if let Err(e) = serde_json::to_writer(&mut *log_file_guard, value) {
            log::error!("Failed to serialize log entry: {e}");
            write_error = true;
            continue; // Skip writing \n if serialization failed
        }
        if let Err(e) = log_file_guard.write_all(b"\n") {
            log::error!("Failed to write newline to log file: {e}");
            write_error = true;
        }
    }

    // Flush the buffer only if there were no write errors
    if !write_error {
        if let Err(e) = log_file_guard.flush() {
            log::error!("Failed to flush log file: {e}");
        }
    }
    // Mutex is automatically released here when log_file_guard goes out of scope
}
