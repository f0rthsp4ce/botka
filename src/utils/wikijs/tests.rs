use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use chrono::DateTime;
use hyper::{Body, Response, Server};
use itertools::Itertools as _;
use tap::Pipe as _;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::{get_wikijs_updates, PageId, VersionId, WikiJsUpdateState};

#[tokio::test]
async fn test_updates_none() {
    let mock_server = MockServer::start();

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
    let mock_server = MockServer::start();

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
    let mock_server = MockServer::start();

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
    let mock_server = MockServer::start();

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
    server: JoinHandle<()>,
    data: Arc<Mutex<Vec<MockQueryResponse>>>,
}

/// Predefined response for a GraphQL query.
struct MockQueryResponse {
    query: String,
    #[allow(dead_code)] // TODO: check params
    params: Vec<(String, String)>,
    response: serde_json::Value,
}

impl MockServer {
    fn start() -> Self {
        let data = Arc::new(Mutex::new(Vec::<MockQueryResponse>::new()));

        let data_clone = Arc::clone(&data);
        let make_svc = hyper::service::make_service_fn(move |_| {
            let data = Arc::clone(&data_clone);
            async move {
                Ok::<_, hyper::Error>(hyper::service::service_fn(move |req| {
                    let data = Arc::clone(&data);
                    async move {
                        assert_eq!(*req.method(), hyper::Method::POST);
                        assert_eq!(req.uri().path(), "/graphql");

                        let body = hyper::body::to_bytes(req.into_body())
                            .await
                            .expect("Failed to read body")
                            .pipe_ref(|x| {
                                serde_json::from_slice::<serde_json::Value>(x)
                            })
                            .expect("Failed to parse body");

                        let req_query = body
                            .get("query")
                            .expect("Missing query")
                            .as_str()
                            .expect("Invalid query");

                        for it in &*data.lock().unwrap() {
                            if req_query != it.query {
                                continue;
                            }
                            return Ok::<_, hyper::Error>(Response::new(
                                Body::from(it.response.to_string()),
                            ));
                        }

                        panic!("Can't match query: {req_query:#?}");
                    }
                }))
            }
        });
        let server = Server::bind(&([127, 0, 0, 1], 0).into()).serve(make_svc);
        let addr = server.local_addr();
        let (tx, rx) = oneshot::channel::<()>();
        let graceful = server.with_graceful_shutdown(async {
            rx.await.ok();
        });
        Self {
            stop: tx,
            addr,
            server: tokio::spawn(async move {
                graceful.await.unwrap();
            }),
            data,
        }
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
        self.data.lock().unwrap().push(MockQueryResponse {
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
        self.stop.send(()).unwrap();
        self.server.await.unwrap();
    }
}
