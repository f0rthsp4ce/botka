use std::convert::Infallible;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use chrono::DateTime;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming as HyperIncomingBody;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request as HyperRequest, Response as HyperResponse, StatusCode};
use hyper_util::rt::TokioIo;
use itertools::Itertools as _;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::{get_wikijs_updates, PageId, VersionId, WikiJsUpdateState};

#[tokio::test]
async fn test_updates_none() {
    let mock_server = MockServer::start().await; // start теперь async

    // Initial update
    mock_server.add_query_response(
        "{\
            pages {\
                list(limit: 10, orderBy: UPDATED, orderByDirection: DESC) {\
                    id locale path title updatedAt\
                }\
            }\
        }",
        &[],
        serde_json::json!({"data": {"pages": {"list": [
            {
                "id": 1,
                "locale": "en",
                "path": "t_path_1",
                "title": "t_title_1",
                "updatedAt": "2021-01-02T00:00:00.000Z",
            },
            {
                "id": 2,
                "locale": "en",
                "path": "t_path_2",
                "title": "t_title_2",
                "updatedAt": "2021-01-01T00:00:00.000Z",
            },
        ]}}}),
    );
    let (updates, state) =
        get_wikijs_updates(&mock_server.endpoint(), "token", None)
            .await
            .unwrap();
    assert!(updates.is_none());
    assert_eq!(state, make_update_state("2021-01-02T00:00:00Z", &[]));

    // Subsequent update
    let (updates, state) = get_wikijs_updates(
        &mock_server.endpoint(),
        "token",
        Some(make_update_state("2021-01-02T00:00:00Z", &[])),
    )
    .await
    .unwrap();
    assert!(updates.is_none());
    assert_eq!(state, make_update_state("2021-01-02T00:00:00Z", &[]));

    mock_server.stop().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_updates_edited() {
    let mock_server = MockServer::start().await;

    mock_server.add_query_response(
        "{\
            pages {\
                list(limit: 10, orderBy: UPDATED, orderByDirection: DESC) {\
                    id locale path title updatedAt\
                }\
            }\
        }",
        &[],
        serde_json::json!({"data": {"pages": {"list": [
            {
                "id": 1,
                "locale": "en",
                "path": "t_path_1",
                "title": "t_title_1",
                "updatedAt": "2021-01-04T00:00:00.000Z",
            },
            {
                "id": 2,
                "locale": "en",
                "path": "t_path_2",
                "title": "t_title_2",
                "updatedAt": "2021-01-02T00:00:00.000Z",
            },
        ]}}}),
    );

    mock_server.add_query_response(
        "{\
            q1: pages {\
                single(id: 1) { authorName updatedAt content }\
                history(id: 1) { trail {versionId versionDate authorName actionType} }\
            },\n\
        }",
        &[],
        serde_json::json!({"data": {
            "q1": {
                "single": {
                    "authorName": "t_authorName",
                    "updatedAt": "2021-01-04T00:00:00.000Z",
                    "content": "t_contentNew",
                },
                "history": {"trail": [
                    {
                        "versionId": 4,
                        "versionDate": "2021-01-04T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "edit",
                    },
                    {
                        "versionId": 3,
                        "versionDate": "2021-01-03T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "edit",
                    },
                    {
                        "versionId": 2,
                        "versionDate": "2021-01-02T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "edit",
                    },
                    {
                        "versionId": 1,
                        "versionDate": "2021-01-01T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "initial",
                    },
                ]},
            },
        }}),
    );

    mock_server.add_query_response(
        "{\
            last: pages {q1: version(pageId: 1, versionId: 4) { action }}\
            prev: pages {q1: version(pageId: 1, versionId: 2) { content }}\
        }",
        &[],
        serde_json::json!({
            "data": {
                "last": {
                    "q1": {"action": "edit"},
                },
                "prev": {
                    "q1": {"content": "t_contentOld"},
                },
            }
        }),
    );

    let (updates, state) = get_wikijs_updates(
        &mock_server.endpoint(),
        "token",
        Some(make_update_state("2021-01-02T00:00:00Z", &[])),
    )
    .await
    .unwrap();

    let updates = updates.unwrap();
    assert_eq!(
        updates.to_html().replace(&mock_server.endpoint(), "%ADDR%"),
        "<a href=\"%ADDR%/en/t_path_1\">t_title_1</a> edited by t_authorName (+12, -12)",
    );
    assert_eq!(updates.paths().collect_vec(), vec!["/en/t_path_1"]);
    assert_eq!(state, make_update_state("2021-01-04T00:00:00Z", &[(1, 4)]));

    mock_server.stop().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_updates_edited_multiple() {
    let mock_server = MockServer::start().await;

    mock_server.add_query_response(
        "{\
            pages {\
                list(limit: 10, orderBy: UPDATED, orderByDirection: DESC) {\
                    id locale path title updatedAt\
                }\
            }\
        }",
        &[],
        serde_json::json!({"data": {"pages": {"list": [
            {
                "id": 1,
                "locale": "en",
                "path": "t_path_1",
                "title": "t_title_1",
                "updatedAt": "2021-01-05T00:00:00.000Z",
            },
            {
                "id": 2,
                "locale": "en",
                "path": "t_path_2",
                "title": "t_title_2",
                "updatedAt": "2021-01-05T00:00:00.000Z",
            },
        ]}}}),
    );

    mock_server.add_query_response(
        "{\
            q1: pages {\
                single(id: 1) { authorName updatedAt content }\
                history(id: 1) { trail {versionId versionDate authorName actionType} }\
            },\n\
            q2: pages {\
                single(id: 2) { authorName updatedAt content }\
                history(id: 2) { trail {versionId versionDate authorName actionType} }\
            },\n\
        }",
        &[],
        serde_json::json!({"data": {
            "q1": {
                "single": {
                    "authorName": "t_authorName",
                    "updatedAt": "2021-01-05T00:00:00.000Z",
                    "content": "t_contentNew",
                },
                "history": {"trail": [
                    {
                        "versionId": 4,
                        "versionDate": "2021-01-04T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "edit",
                    },
                    {
                        "versionId": 3,
                        "versionDate": "2021-01-03T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "edit",
                    },
                    {
                        "versionId": 2,
                        "versionDate": "2021-01-02T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "edit",
                    },
                    {
                        "versionId": 1,
                        "versionDate": "2021-01-01T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "initial",
                    },
                ]},
            },
            "q2": {
                "single": {
                    "authorName": "t_authorName",
                    "updatedAt": "2021-01-05T00:00:00.000Z",
                    "content": "t_contentNew",
                },
                "history": {"trail": [
                    {
                        "versionId": 4,
                        "versionDate": "2021-01-04T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "edit",
                    },
                    {
                        "versionId": 3,
                        "versionDate": "2021-01-03T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "edit",
                    },
                    {
                        "versionId": 2,
                        "versionDate": "2021-01-02T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "edit",
                    },
                    {
                        "versionId": 1,
                        "versionDate": "2021-01-01T00:00:00.000Z",
                        "authorName": "t_authorName",
                        "actionType": "initial",
                    },
                ]},
            },
        }}),
    );

    mock_server.add_query_response(
        "{\
            last: pages {\
                q1: version(pageId: 1, versionId: 4) { action }\n\
                q2: version(pageId: 2, versionId: 4) { action }}\
            prev: pages {\
                q1: version(pageId: 1, versionId: 1) { content }\n\
                q2: version(pageId: 2, versionId: 1) { content }}\
        }",
        &[],
        serde_json::json!({
            "data": {
                "last": {
                    "q1": {"action": "edit"},
                    "q2": {"action": "edit"},
                },
                "prev": {
                    "q1": {"content": "t_contentOld"},
                    "q2": {"content": "t_contentOld"},
                },
            }
        }),
    );

    let (updates, state) = get_wikijs_updates(
        &mock_server.endpoint(),
        "token",
        Some(make_update_state("2021-01-01T00:00:00Z", &[(1, 1), (2, 1)])),
    )
    .await
    .unwrap();

    let updates = updates.unwrap();
    assert_eq!(
        updates.to_html().replace(&mock_server.endpoint(), "%ADDR%"),
        "<a href=\"%ADDR%/en/t_path_1\">t_title_1</a> edited by t_authorName (+12, -12)\n\
         <a href=\"%ADDR%/en/t_path_2\">t_title_2</a> edited by t_authorName (+12, -12)",
    );
    assert_eq!(
        updates.paths().collect_vec(),
        vec!["/en/t_path_1", "/en/t_path_2"]
    );
    assert_eq!(
        state,
        make_update_state("2021-01-05T00:00:00Z", &[(1, 4), (2, 4)])
    );

    mock_server.stop().await;
}

#[tokio::test]
async fn test_updates_new_page_added() {
    let mock_server = MockServer::start().await;

    mock_server.add_query_response(
        "{\
            pages {\
                list(limit: 10, orderBy: UPDATED, orderByDirection: DESC) {\
                    id locale path title updatedAt\
                }\
            }\
        }",
        &[],
        serde_json::json!({"data": {"pages": {"list": [
            {
                "id": 2,
                "locale": "en",
                "path": "t_path_2",
                "title": "t_title_2",
                "updatedAt": "2021-01-02T00:00:00.000Z",
            },
            {
                "id": 1,
                "locale": "en",
                "path": "t_path_1",
                "title": "t_title_1",
                "updatedAt": "2021-01-01T00:00:00.000Z",
            },
        ]}}}),
    );

    mock_server.add_query_response(
        "{q2: pages {single(id: 2) { authorName updatedAt content }history(id: 2) { trail {versionId versionDate authorName actionType} }},\n}",
        &[],
        serde_json::json!({"data": {
            "q2": {
                "single": {
                    "authorName": "t_authorName",
                    "updatedAt": "2021-01-02T00:00:00.000Z",
                    "content": "t_contentNew",
                },
                "history": {"trail": []},
            }
        }}),
    );

    let (updates, state) = get_wikijs_updates(
        &mock_server.endpoint(),
        "token",
        Some(make_update_state("2021-01-01T00:00:00Z", &[(1, 10)])),
    )
    .await
    .unwrap();

    let updates = updates.unwrap();
    assert_eq!(
        updates.to_html().replace(&mock_server.endpoint(), "%ADDR%"),
        "<a href=\"%ADDR%/en/t_path_2\">t_title_2</a> created by t_authorName (+12)",
    );
    assert_eq!(updates.paths().collect_vec(), vec!["/en/t_path_2"]);
    assert_eq!(
        state,
        make_update_state("2021-01-02T00:00:00Z", &[(1, 10), (2, 0)]),
    );

    mock_server.stop().await;
}

fn make_update_state(
    last_update: &str,
    pages: &[(u32, u32)],
) -> WikiJsUpdateState {
    WikiJsUpdateState {
        last_update: DateTime::parse_from_rfc3339(last_update).unwrap().into(),
        pages: pages.iter().map(|(p, v)| (PageId(*p), VersionId(*v))).collect(),
    }
}

/// Mock GraphQL server. Serves predefined responses.
struct MockServer {
    stop: oneshot::Sender<()>,
    addr: SocketAddr,
    server_handle: JoinHandle<()>, // Переименовано для ясности
    data: Arc<Mutex<Vec<MockQueryResponse>>>,
}

/// Predefined response for a GraphQL query.
#[derive(Debug)]
struct MockQueryResponse {
    query: String,
    #[allow(dead_code)] // TODO: check params
    params: Vec<(String, String)>,
    response: serde_json::Value,
}

impl MockServer {
    async fn start() -> Self {
        let data = Arc::new(Mutex::new(Vec::<MockQueryResponse>::new()));

        let listener =
            TcpListener::bind(&SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .expect("Failed to bind mock server");
        let addr = listener.local_addr().expect("Failed to get local address");

        let (tx, rx) = oneshot::channel::<()>();

        let data_clone = Arc::clone(&data);
        let server_handle = tokio::spawn(async move {
            let mut rx = rx;
            loop {
                tokio::select! {
                    _ = &mut rx => {
                        log::debug!("MockServer received shutdown signal.");
                        break;
                    }
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((tcp_stream, remote_addr)) => {
                                log::debug!("MockServer accepted connection from {remote_addr}");
                                let io = TokioIo::new(tcp_stream);
                                let data_for_conn = Arc::clone(&data_clone);

                                tokio::spawn(async move {
                                    let service = service_fn(move |req: HyperRequest<HyperIncomingBody>| {
                                        let data = Arc::clone(&data_for_conn);
                                        async move {
                                            assert_eq!(*req.method(), hyper::Method::POST);
                                            assert_eq!(req.uri().path(), "/graphql");

                                            let body_bytes = match req.into_body().collect().await {
                                                Ok(collected) => collected.to_bytes(),
                                                Err(e) => {
                                                    log::error!("Failed to read mock request body: {e}");
                                                    let response = HyperResponse::builder()
                                                        .status(StatusCode::BAD_REQUEST)
                                                        .body(Full::new(Bytes::from("Bad Request")))
                                                        .unwrap();
                                                    return Ok::<_, Infallible>(response);
                                                }
                                            };

                                            let body_json: serde_json::Value = match serde_json::from_slice(&body_bytes) {
                                                Ok(val) => val,
                                                Err(e) => {
                                                    log::error!("Failed to parse mock request body JSON: {e}");
                                                    let response = HyperResponse::builder()
                                                        .status(StatusCode::BAD_REQUEST)
                                                        .body(Full::new(Bytes::from("Bad Request")))
                                                        .unwrap();
                                                    return Ok::<_, Infallible>(response);
                                                }
                                            };

                                            let Some(req_query) = body_json.get("query").and_then(|q| q.as_str()) else {
                                                      log::error!("Missing or invalid 'query' field in mock request body");
                                                      let response = HyperResponse::builder()
                                                        .status(StatusCode::BAD_REQUEST)
                                                        .body(Full::new(Bytes::from("Bad Request")))
                                                        .unwrap();
                                                    return Ok::<_, Infallible>(response);
                                            };

                                            let data_guard = data.lock().expect("Mutex poisoned");
                                            for it in &*data_guard {
                                                if req_query == it.query {
                                                    log::debug!("MockServer matched query: {req_query}");
                                                    let response = HyperResponse::builder()
                                                        .status(StatusCode::OK)
                                                        .header(hyper::header::CONTENT_TYPE, "application/json")
                                                        .body(Full::new(Bytes::from(it.response.to_string())))
                                                        .unwrap();
                                                    return Ok::<_, Infallible>(response);
                                                }
                                            }
                                            drop(data_guard);

                                            panic!("MockServer can't match query: {req_query:#?}");
                                        }
                                    });

                                    if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                                       let is_io_error = err.source().is_some_and(|source| source.is::<std::io::Error>());
                                        if !err.is_incomplete_message() && !is_io_error {
                                            log::error!("Error serving mock connection from {remote_addr}: {err}");
                                        } else {
                                            log::debug!("Mock connection closed or IO error from {remote_addr}: {err}");
                                        }
                                    }
                                });
                            }
                            Err(e) => {
                                log::error!("MockServer failed to accept connection: {e}");
                            }
                        }
                    }
                }
            }
            log::debug!("MockServer task finished.");
        });

        Self { stop: tx, addr, server_handle, data }
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn add_query_response(
        &self,
        query: &str,
        params: &[(&str, &str)],
        response: serde_json::Value,
    ) {
        self.data.lock().expect("Mutex poisoned").push(MockQueryResponse {
            query: query.to_string(),
            params: params
                .iter()
                .copied()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            response,
        });
    }

    async fn stop(self) {
        let _ = self.stop.send(());
        if let Err(e) = self.server_handle.await {
            log::error!("MockServer task panicked: {e:?}");
        } else {
            log::debug!("MockServer stopped successfully.");
        }
    }
}
