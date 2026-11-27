use axum::{
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use gateway_core::{
    admin::KeyPolicyPatch, AdminKeyCreate, AdminKeyOwnerType, AdminKeyStore, AdminServiceStore,
    GuardrailPolicy, TaskUsage, UsageQuery, UsageQueryStore, VirtualKeyMaterial,
};
use gateway_store::PostgresStore;
use serde_json::{json, Value};
use std::{
    convert::Infallible,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::Semaphore, time};
use uuid::Uuid;

const TASK_ID: &str = "core.task-1";
const SDK_SERVICE: &str = "example-worker";
const SERVICE_CREDENTIAL: &str = "fixture-sdk-credential";

struct MockSdk {
    submitted: AtomicBool,
    requests: AtomicUsize,
    finish_stream: Semaphore,
}

fn status_event(event_id: &str, status: &str) -> Value {
    let mut event = json!({
        "task_id": TASK_ID,
        "event_id": event_id,
        "status": status,
        "timestamp": "2026-11-27T00:00:00Z",
        "service": SDK_SERVICE,
        "correlation_id": "example-correlation",
        "spec_version": "1.0",
    });
    if status == "completed" {
        event["result"] = json!({"value": 42});
    }
    event
}

async fn sdk(
    State(state): State<Arc<MockSdk>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    state.requests.fetch_add(1, Ordering::SeqCst);
    assert!(
        headers.get("authorization").is_some_and(
            |value| value.as_bytes() == format!("Bearer {SERVICE_CREDENTIAL}").as_bytes()
        )
    );
    for header in [
        "x-litellm-key",
        "x-relayna-worker-token",
        "x-relayna-task-id",
        "x-relayna-run-id",
        "x-relayna-task-service",
    ] {
        assert!(!headers.contains_key(header), "forwarded {header}");
    }
    assert_ne!(headers["x-relayna-project-id"], "untrusted-project");
    let running = status_event("event-1", "running");
    let completed = status_event("event-2", "completed");
    match (method, uri.path()) {
        (Method::POST, "/tasks") => {
            let envelope: Value = serde_json::from_slice(&body).unwrap();
            if envelope["task_id"] == "unavailable" {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [("retry-after", "2")],
                    Json(json!({"detail":"Spooltail could not confirm task submission"})),
                )
                    .into_response();
            }
            assert_eq!(envelope["service"], SDK_SERVICE);
            assert_eq!(envelope["task_id"], TASK_ID);
            assert_eq!(envelope["correlation_id"], "example-correlation");
            assert_eq!(envelope["spec_version"], "1.0");
            if envelope["payload"] != json!({"value": 21}) {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"detail":"Task ID already has a different submission"})),
                )
                    .into_response();
            }
            state.submitted.store(true, Ordering::SeqCst);
            (StatusCode::ACCEPTED, Json(json!({"task_id":TASK_ID}))).into_response()
        }
        (Method::GET, "/status/core.task-1") => {
            assert!(state.submitted.load(Ordering::SeqCst));
            Json(json!({"task_id":TASK_ID,"event":completed})).into_response()
        }
        (Method::GET, "/relayna/capabilities") => {
            Json(json!({"topology_kind":"spooltail"})).into_response()
        }
        (Method::POST, "/tasks/core.task-1/cancel") => {
            assert!(body.is_empty());
            assert!(state.submitted.load(Ordering::SeqCst));
            (StatusCode::ACCEPTED, Json(json!({"task_id":TASK_ID}))).into_response()
        }
        (Method::POST, "/tasks/finished/cancel") => (
            StatusCode::CONFLICT,
            Json(json!({"detail":"Task is already terminal"})),
        )
            .into_response(),
        (Method::GET, "/history/core.task-1") => match uri.query() {
            Some("after=event-1") => {
                Json(json!({"task_id":TASK_ID,"count":1,"events":[completed]})).into_response()
            }
            Some("after=old-event") => (
                StatusCode::GONE,
                Json(json!({"detail":{"earliest_event_id":"event-1"}})),
            )
                .into_response(),
            _ => panic!("unexpected history cursor"),
        },
        (Method::GET, "/events/core.task-1") => {
            let terminal = format!("id: event-2\nevent: status\ndata: {completed}\n\n");
            let body = match headers.get("last-event-id").map(|id| id.to_str().unwrap()) {
                None => {
                    use futures_util::{stream, StreamExt};
                    Body::from_stream(
                        stream::iter([Ok::<_, Infallible>(format!(
                            "id: event-1\nevent: status\ndata: {running}\n\n"
                        ))])
                        .chain(stream::once(async move {
                            state.finish_stream.acquire().await.unwrap().forget();
                            Ok(terminal)
                        })),
                    )
                }
                Some("event-1") => Body::from(terminal),
                Some("old-event") => {
                    Body::from("event: gap\ndata: {\"earliest_event_id\":\"event-1\"}\n\n")
                }
                _ => panic!("unexpected SSE cursor"),
            };
            ([("content-type", "text/event-stream")], body).into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({"detail":"task not found"})),
        )
            .into_response(),
    }
}

async fn task_key(
    store: &PostgresStore,
    project_id: Uuid,
    service_names: Vec<String>,
    allow_streaming: bool,
) -> VirtualKeyMaterial {
    create_key(
        store,
        project_id,
        KeyPolicyPatch {
            allowed_routes: Some(vec!["/services/*".into()]),
            allowed_providers: Some(vec!["internal-service".into()]),
            allowed_services: Some(service_names),
            allow_streaming: Some(allow_streaming),
            ..KeyPolicyPatch::default()
        },
    )
    .await
}

async fn create_key(
    store: &PostgresStore,
    project_id: Uuid,
    policy: KeyPolicyPatch,
) -> VirtualKeyMaterial {
    let material = VirtualKeyMaterial::generate().unwrap();
    store
        .create_admin_key(
            AdminKeyCreate {
                name: None,
                owner_type: AdminKeyOwnerType::Project,
                project_id: Some(project_id),
                service_names: vec![],
                preset: None,
                expires_at: None,
                rotation_due_at: None,
                policy,
                guardrail_policy: GuardrailPolicy::default(),
            },
            &material,
        )
        .await
        .expect("create task key");
    material
}

pub(super) async fn check_contract(
    client: &reqwest::Client,
    proxy_url: &str,
    store: &PostgresStore,
    project_id: Uuid,
    other_service_key: &str,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(MockSdk {
        submitted: AtomicBool::new(false),
        requests: AtomicUsize::new(0),
        finish_stream: Semaphore::new(0),
    });
    let sdk_app = Router::new().fallback(sdk).with_state(state.clone());
    let sdk_server = tokio::spawn(async move { axum::serve(listener, sdk_app).await.unwrap() });
    let service = format!("task-sdk-{}", Uuid::new_v4().simple());
    let other_task_service = format!("task-sdk-{}", Uuid::new_v4().simple());
    for registration in [&service, &other_task_service] {
        store
            .create_service(
                serde_json::from_value(json!({
                    "name": registration,
                    "route_pattern": format!("/services/{registration}/*"),
                    "upstream_base_url": upstream,
                    "access": {"relayna_tasks": true},
                    "credential": SERVICE_CREDENTIAL,
                    "allowed_methods": ["GET", "POST"],
                    "cost_mode": "none",
                    "endpoint_pricing_rules": [
                        {"method":"POST","path_template":"/tasks","cost_mode":"fixed",
                         "estimated_cost_usd":0.01}
                    ],
                }))
                .unwrap(),
            )
            .await
            .expect("register SDK service");
    }
    let edited = store
        .patch_service(
            &service,
            serde_json::from_value(json!({"access":{}})).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(edited.access.is_task_service());

    let key = task_key(store, project_id, vec![service.clone()], true).await;
    let no_stream = task_key(store, project_id, vec![service.clone()], false).await;
    let no_service = task_key(store, project_id, Vec::new(), true).await;
    let base = format!("{proxy_url}/services/{service}");
    let send = |method: Method, path: &str, key: Option<&str>| {
        let mut request = client
            .request(method, format!("{base}{path}"))
            .timeout(Duration::from_secs(10));
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        request
    };
    for (method, path) in [
        (Method::POST, "/tasks"),
        (Method::GET, "/status/core.task-1"),
        (Method::GET, "/history/core.task-1?after=event-1"),
        (Method::GET, "/events/core.task-1"),
        (Method::GET, "/relayna/capabilities"),
        (Method::POST, "/tasks/core.task-1/cancel"),
        (Method::GET, "/tasks/core.task-1/usage"),
    ] {
        for (credential, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(other_service_key), StatusCode::FORBIDDEN),
            (Some(no_service.raw_key.as_str()), StatusCode::FORBIDDEN),
        ] {
            let response = send(method.clone(), path, credential).send().await.unwrap();
            assert_eq!(response.status(), expected, "{path}");
        }
    }
    for (credential, path, expected) in [
        (&no_stream, "/events/core.task-1", StatusCode::FORBIDDEN),
        (&key, "/docs", StatusCode::NOT_FOUND),
        (&key, "/status/core.task-1%2Fevents", StatusCode::NOT_FOUND),
    ] {
        let response = send(Method::GET, path, Some(&credential.raw_key))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{path}");
    }
    assert_eq!(state.requests.load(Ordering::SeqCst), 0);

    let mut envelope = json!({"task_id":TASK_ID,"payload":{"value":21},
        "service":SDK_SERVICE,"correlation_id":"example-correlation","spec_version":"1.0"});
    let mut submission_requests = Vec::new();
    for _ in 0..2 {
        let response = send(Method::POST, "/tasks", Some(&key.raw_key))
            .header("x-litellm-key", "unused-client-credential")
            .header("x-relayna-task-id", "untrusted-task")
            .header("x-relayna-project-id", "untrusted-project")
            .json(&envelope)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        submission_requests.push(
            response.headers()["x-request-id"]
                .to_str()
                .unwrap()
                .to_owned(),
        );
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"task_id":TASK_ID})
        );
    }
    envelope["payload"] = json!({"value":22});
    let response = send(Method::POST, "/tasks", Some(&key.raw_key))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"detail":"Task ID already has a different submission"})
    );
    envelope["task_id"] = json!("unavailable");
    let response = send(Method::POST, "/tasks", Some(&key.raw_key))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "2");
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"detail":"Spooltail could not confirm task submission"})
    );

    for (task_id, expected_status, expected_body) in [
        (TASK_ID, StatusCode::ACCEPTED, json!({"task_id":TASK_ID})),
        (TASK_ID, StatusCode::ACCEPTED, json!({"task_id":TASK_ID})),
        (
            "finished",
            StatusCode::CONFLICT,
            json!({"detail":"Task is already terminal"}),
        ),
        (
            "unknown",
            StatusCode::NOT_FOUND,
            json!({"detail":"task not found"}),
        ),
    ] {
        let response = send(
            Method::POST,
            &format!("/tasks/{task_id}/cancel"),
            Some(&key.raw_key),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), expected_status);
        assert_eq!(response.json::<Value>().await.unwrap(), expected_body);
    }

    let mut read_requests = Vec::new();
    for (path, expected_status, expected_body) in [
        (
            "/relayna/capabilities",
            StatusCode::OK,
            json!({"topology_kind":"spooltail"}),
        ),
        (
            "/status/core.task-1",
            StatusCode::OK,
            json!({"task_id":TASK_ID,"event":status_event("event-2", "completed")}),
        ),
        (
            "/history/core.task-1?after=event-1",
            StatusCode::OK,
            json!({"task_id":TASK_ID,"count":1,"events":[status_event("event-2", "completed")]}),
        ),
        (
            "/history/core.task-1?after=old-event",
            StatusCode::GONE,
            json!({"detail":{"earliest_event_id":"event-1"}}),
        ),
        (
            "/status/unknown",
            StatusCode::NOT_FOUND,
            json!({"detail":"task not found"}),
        ),
    ] {
        let response = send(Method::GET, path, Some(&no_stream.raw_key))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected_status);
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        read_requests.push((request_id, path.split('?').next().unwrap(), expected_status));
        assert_eq!(response.json::<Value>().await.unwrap(), expected_body);
    }

    let open_events = || send(Method::GET, "/events/core.task-1", Some(&key.raw_key));
    let mut stream = open_events().send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert_eq!(stream.headers()["content-type"], "text/event-stream");
    let first = time::timeout(Duration::from_secs(3), async {
        let mut frame = Vec::new();
        while !frame.ends_with(b"\n\n") {
            frame.extend_from_slice(&stream.chunk().await.unwrap().expect("first SSE frame"));
        }
        String::from_utf8(frame).unwrap()
    })
    .await
    .expect("first event must arrive while upstream is still open");
    assert_eq!(
        first,
        format!(
            "id: event-1\nevent: status\ndata: {}\n\n",
            status_event("event-1", "running")
        )
    );
    drop(stream);
    state.finish_stream.add_permits(1);

    for (cursor, expected) in [
        (
            "event-1",
            format!(
                "id: event-2\nevent: status\ndata: {}\n\n",
                status_event("event-2", "completed")
            ),
        ),
        (
            "old-event",
            "event: gap\ndata: {\"earliest_event_id\":\"event-1\"}\n\n".to_owned(),
        ),
    ] {
        let response = open_events()
            .header("last-event-id", cursor)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        read_requests.push((request_id, "/events/core.task-1", StatusCode::OK));
        assert_eq!(response.text().await.unwrap(), expected);
    }

    for (request_id, method, path, status, price) in submission_requests
        .into_iter()
        .map(|id| (id, "POST", "/tasks", StatusCode::ACCEPTED, Some(0.01)))
        .chain(
            read_requests
                .into_iter()
                .map(|(id, path, status)| (id, "GET", path, status, None)),
        )
    {
        let usage = time::timeout(Duration::from_secs(5), async {
            loop {
                let recorded = sqlx::query_as::<
                    _,
                    (
                        String,
                        String,
                        String,
                        i32,
                        Option<f64>,
                        Option<i64>,
                        Option<String>,
                    ),
                >(
                    "SELECT service_name, http_method, endpoint_path, status_code,
                            estimated_cost::double precision, total_tokens, task_id
                     FROM usage_events WHERE request_id = $1",
                )
                .bind(&request_id)
                .fetch_optional(store.pool())
                .await
                .unwrap();
                if let Some(usage) = recorded {
                    break usage;
                }
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("task request usage");
        assert_eq!(
            usage,
            (
                service.clone(),
                method.into(),
                path.into(),
                i32::from(status.as_u16()),
                price,
                None,
                (method == "POST").then(|| TASK_ID.to_owned())
            )
        );
    }
    check_scoped_usage(
        client,
        proxy_url,
        store,
        project_id,
        &service,
        &other_task_service,
        &key.raw_key,
    )
    .await;
    sdk_server.abort();
    let disabled = store
        .patch_service(
            &service,
            serde_json::from_value(json!({"enabled":false,"access":{"relayna_tasks":false}}))
                .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(serde_json::to_value(disabled.access)
        .unwrap()
        .get("relayna_tasks")
        .is_none());
}

async fn check_scoped_usage(
    client: &reqwest::Client,
    proxy_url: &str,
    store: &PostgresStore,
    project_id: Uuid,
    service: &str,
    other_service: &str,
    task_key_raw: &str,
) {
    let inference_key = create_key(
        store,
        project_id,
        KeyPolicyPatch {
            allowed_routes: Some(vec!["/v1/chat/completions".into()]),
            allowed_providers: Some(vec!["litellm".into()]),
            allowed_models: Some(vec!["coverage-model".into()]),
            allowed_services: Some(vec![service.into()]),
            ..KeyPolicyPatch::default()
        },
    )
    .await;
    for (worker_token, requested_service) in [
        ("worker-secret", service),
        ("worker-secret", other_service),
        ("public-header", service),
    ] {
        let response = client
            .post(format!("{proxy_url}/v1/chat/completions"))
            .bearer_auth(&inference_key.raw_key)
            .header("x-relayna-worker-token", worker_token)
            .header("x-relayna-task-service", requested_service)
            .header("x-relayna-task-id", TASK_ID)
            .header("x-relayna-run-id", "attempt-1")
            .header(
                "traceparent",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            )
            .json(&json!({"model":"coverage-model","messages":[]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.bytes().await.unwrap();
    }
    let usage_url = |service| format!("{proxy_url}/services/{service}/tasks/{TASK_ID}/usage");
    let usage = time::timeout(Duration::from_secs(5), async {
        loop {
            let response = client
                .get(usage_url(service))
                .bearer_auth(task_key_raw)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["cache-control"], "private, no-store");
            let usage = response.json::<TaskUsage>().await.unwrap();
            if usage.request_count >= 5 {
                break usage;
            }
            time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("scoped task observations should be recorded");
    assert_eq!(usage.service, service);
    assert_eq!(usage.task_id, TASK_ID);
    assert_eq!(usage.request_count, 5); // Two submissions, two cancellations, one scoped inference.
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (2, 3, 5)
    );
    assert!(!usage.truncated);
    assert_eq!(usage.observations.len(), 5);
    let inference = usage
        .observations
        .iter()
        .find(|event| event.total_tokens == Some(5))
        .unwrap();
    assert_eq!(inference.run_id.as_deref(), Some("attempt-1"));
    assert_eq!(
        inference.trace_id.as_deref(),
        Some("4bf92f3577b34da6a3ce929d0e0e4736")
    );

    let admin_events = store
        .usage_events(UsageQuery {
            service: Some(service.into()),
            task_id: Some(TASK_ID.into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(admin_events.rows.len(), 5);
    assert!(admin_events
        .rows
        .iter()
        .any(|event| event.request_id == inference.request_id));

    let other_key = task_key(store, project_id, vec![other_service.into()], false).await;
    let response = client
        .get(usage_url(other_service))
        .bearer_auth(&other_key.raw_key)
        .query(&[("service", service)])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let empty = response.json::<TaskUsage>().await.unwrap();
    assert_eq!(empty.service, other_service);
    assert_eq!(empty.request_count, 0);
    assert!(empty.observations.is_empty());
}
