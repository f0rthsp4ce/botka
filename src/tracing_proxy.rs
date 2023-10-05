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
/// getUpdates responses to a file.
/// Returns the URL of the proxy server.
pub async fn start(log_file: &str) -> Result<String> {
    // Make client from teloxide::net::default_reqwest_settings, plus 3 seconds.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5 + 3))
        .timeout(Duration::from_secs(17 + 3))
        .tcp_nodelay(true)
        .build()?;

    let log_file = Mutex::new(
        OpenOptions::new().create(true).append(true).open(log_file)?,
    );

    let proxy = Arc::new(Proxy { client, log_file });

    let listener =
        TcpListener::bind(&SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let local_addr = listener.local_addr()?;
    let server = Server::builder(AddrIncoming::from_listener(listener)?).serve(
        make_service_fn(move |_conn| {
            let proxy = proxy.clone();
            async {
                Ok::<_, Infallible>(service_fn(move |req| {
                    handle_request(req, proxy.clone())
                }))
            }
        }),
    );
    tokio::spawn(async move {
        if let Err(e) = server.await {
            log::error!("Proxy server error: {}", e);
        }
    });

    Ok(format!("http://{}", local_addr))
}

async fn handle_request(
    req: Request<Body>,
    proxy: Arc<Proxy>,
) -> Result<Response<Body>> {
    let (mut parts, body) = req.into_parts();
    parts.uri = Uri::builder()
        .scheme("https")
        .authority("api.telegram.org")
        .path_and_query(parts.uri.path_and_query().unwrap().as_str())
        .build()
        .unwrap();
    parts
        .headers
        .insert("host", "api.telegram.org".parse().expect("host header"));
    let path = parts.uri.path_and_query().unwrap().path();
    const PREFIX: &'static str = "/bot";
    const SUFIX: &'static str = "/GetUpdates";
    let is_get_updates = path.starts_with(PREFIX)
        && path.ends_with(SUFIX)
        && path[PREFIX.len()..path.len() - SUFIX.len()].find('/').is_none();

    let body_bytes = hyper::body::to_bytes(body).await?;
    let request = Request::from_parts(parts, body_bytes);
    let request: reqwest::Request = request.try_into()?;

    let forwarded_resp =
        proxy.client.execute(request).await.expect("request error");

    // Convert the reqwest Response to hyper Response
    let status = forwarded_resp.status();
    let version = forwarded_resp.version();
    let headers = forwarded_resp.headers().clone();
    let body = forwarded_resp.bytes().await?;

    if is_get_updates {
        if let Ok(body) =
            serde_json::from_slice::<GetUpdatesResponse>(&body[..])
        {
            let mut log_file = proxy.log_file.lock().await;
            for update in body.result {
                serde_json::to_writer(&mut *log_file, &update).unwrap();
                log_file.write_all(b"\n").unwrap();
            }
            log_file.flush().unwrap();
        }
    }

    let mut resp = Response::new(body.into());
    *resp.status_mut() = status;
    *resp.headers_mut() = headers;
    *resp.version_mut() = version;

    Ok(resp)
}
