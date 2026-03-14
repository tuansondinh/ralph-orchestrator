use std::time::Duration;
use std::{fs, path::PathBuf};

use anyhow::Result;
use futures::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use ralph_api::{ApiConfig, RpcRuntime, serve_with_listener};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct TestServer {
    base_url: String,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
    _workspace: TempDir,
}

impl TestServer {
    async fn start(mut config: ApiConfig) -> Self {
        let workspace = tempfile::tempdir().expect("workspace tempdir should be created");
        config.workspace_root = workspace.path().to_path_buf();

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener should bind");
        let local_addr = listener
            .local_addr()
            .expect("listener local addr should exist");
        let runtime = RpcRuntime::new(config).expect("runtime should initialize");
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let join = tokio::spawn(async move {
            serve_with_listener(listener, runtime, async move {
                let _ = shutdown_rx.await;
            })
            .await
        });

        Self {
            base_url: format!("http://{local_addr}"),
            shutdown: Some(shutdown_tx),
            join,
            _workspace: workspace,
        }
    }

    fn ws_url(&self) -> String {
        self.base_url.replacen("http://", "ws://", 1)
    }

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let result = self.join.await.expect("server task should join");
        result.expect("server should shutdown cleanly");
    }
}

async fn post_rpc_with_token(
    client: &Client,
    server: &TestServer,
    body: &Value,
    token: Option<&str>,
) -> Result<(u16, Value)> {
    let mut request = client
        .post(format!("{}/rpc/v1", server.base_url))
        .header("content-type", "application/json")
        .json(body);

    if let Some(token) = token {
        request = request.bearer_auth(token);
    }

    let response = request.send().await?;

    let status = response.status().as_u16();
    let payload = response.json::<Value>().await?;
    Ok((status, payload))
}

async fn post_rpc(client: &Client, server: &TestServer, body: &Value) -> Result<(u16, Value)> {
    post_rpc_with_token(client, server, body, None).await
}

fn rpc_request(id: &str, method: &str, params: Value, idempotency_key: Option<&str>) -> Value {
    let mut request = json!({
        "apiVersion": "v1",
        "id": id,
        "method": method,
        "params": params,
    });

    if let Some(idempotency_key) = idempotency_key {
        request["meta"] = json!({
            "idempotencyKey": idempotency_key,
        });
    }

    request
}

async fn open_stream_with_token(
    server: &TestServer,
    subscription_id: &str,
    token: Option<&str>,
) -> Result<WsStream> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let url = format!(
        "{}/rpc/v1/stream?subscriptionId={subscription_id}",
        server.ws_url()
    );

    let mut request = url.into_client_request()?;
    if let Some(token) = token {
        let header_value = format!("Bearer {token}");
        request.headers_mut().insert(
            "Authorization",
            header_value.parse().expect("valid auth header"),
        );
    }

    let (stream, _) = connect_async(request).await?;
    Ok(stream)
}

async fn open_stream(server: &TestServer, subscription_id: &str) -> Result<WsStream> {
    open_stream_with_token(server, subscription_id, None).await
}

#[cfg(unix)]
fn create_fake_loop_ralph_command() -> Result<(TempDir, PathBuf, PathBuf)> {
    use std::os::unix::fs::PermissionsExt;

    let fake_bin = tempfile::tempdir()?;
    let command_path = fake_bin.path().join("fake-ralph");
    let call_log_path = fake_bin.path().join("calls.log");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> "{call_log_path}"
is_run=0
for arg in "$@"; do
  if [ "$arg" = "run" ]; then
    is_run=1
  fi
done
if [ "$is_run" = "1" ]; then
  prompt="[no prompt]"
  previous=""
  for arg in "$@"; do
    if [ "$previous" = "-p" ]; then
      prompt="$arg"
      previous=""
      continue
    fi
    if [ "$previous" = "-P" ]; then
      prompt="$(cat "$arg" 2>/dev/null || printf '[prompt file]')"
      previous=""
      continue
    fi
    case "$arg" in
      -p|-P) previous="$arg" ;;
    esac
  done

  mkdir -p ".ralph"
  cat > ".ralph/loop.lock" <<EOF
{{
  "pid": $$,
  "started": "2026-03-14T09:00:00Z",
  "prompt": "$prompt"
}}
EOF
  printf '%s\n' '{{"topic":"loop.started","payload":"fake loop started","ts":"2026-03-14T09:00:01Z"}}' >> ".ralph/events.jsonl"
  while [ ! -f ".ralph/stop-requested" ]; do
    sleep 0.1
  done
  printf '%s\n' '{{"topic":"loop.stopped","payload":"fake loop stopped","ts":"2026-03-14T09:00:02Z"}}' >> ".ralph/events.jsonl"
  exit 0
fi
exit 0
"#,
        call_log_path = call_log_path.display()
    );

    fs::write(&command_path, script)?;
    let mut permissions = fs::metadata(&command_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&command_path, permissions)?;

    Ok((fake_bin, command_path, call_log_path))
}

async fn recv_topic_event(stream: &mut WsStream, topic: &str) -> Value {
    loop {
        let maybe_message = timeout(Duration::from_secs(4), stream.next())
            .await
            .expect("timed out waiting for websocket message");

        let Some(message) = maybe_message else {
            panic!("websocket closed before receiving expected topic");
        };

        let message = message.expect("websocket message should be ok");
        let Message::Text(text) = message else {
            continue;
        };

        let payload: Value =
            serde_json::from_str(&text).expect("websocket event should be valid json");
        if payload["topic"] == topic {
            return payload;
        }
    }
}

async fn recv_mixed_events(stream: &mut WsStream, required: usize) -> Vec<Value> {
    let mut events = Vec::new();
    while events.len() < required {
        let maybe_message = timeout(Duration::from_secs(4), stream.next())
            .await
            .expect("timed out waiting for websocket message");

        let Some(message) = maybe_message else {
            panic!("websocket closed before receiving required events");
        };

        let message = message.expect("websocket message should be ok");
        let Message::Text(text) = message else {
            continue;
        };

        let payload: Value =
            serde_json::from_str(&text).expect("websocket event should be valid json");
        events.push(payload);
    }

    events
}

#[cfg(unix)]
#[tokio::test]
async fn loop_start_publishes_runtime_events_and_resume_replays_history() -> Result<()> {
    let (fake_bin, fake_ralph, call_log_path) = create_fake_loop_ralph_command()?;

    let mut config = ApiConfig::default();
    config.ralph_command = fake_ralph.to_string_lossy().to_string();

    let server = TestServer::start(config).await;
    let client = Client::new();

    let subscribe = rpc_request(
        "req-loop-stream-live-1",
        "stream.subscribe",
        json!({
            "topics": ["loop.status.changed", "loop.event", "loop.log.line"],
            "filters": { "resourceIds": ["(primary)"] }
        }),
        None,
    );
    let (_, subscribe_payload) = post_rpc(&client, &server, &subscribe).await?;
    let subscription_id = subscribe_payload["result"]["subscriptionId"]
        .as_str()
        .expect("subscription id should be present")
        .to_string();

    let mut live_stream = open_stream(&server, &subscription_id).await?;

    let start = rpc_request(
        "req-loop-start-live-1",
        "loop.start",
        json!({
            "config": "presets/spec-driven.yml",
            "prompt": "Ship phase-1 loop start",
            "backend": "codex",
            "exclusive": true
        }),
        Some("idem-loop-start-live-1"),
    );
    let (status, start_payload) = post_rpc(&client, &server, &start).await?;
    assert_eq!(status, 200, "loop.start must succeed: {start_payload}");
    assert_eq!(start_payload["result"]["loop"]["id"], "(primary)");
    assert_eq!(start_payload["result"]["loop"]["status"], "running");

    let calls = fs::read_to_string(&call_log_path)?;
    assert!(
        calls
            .lines()
            .any(|line| line.contains("-c presets/spec-driven.yml run --no-tui -p Ship phase-1 loop start -b codex --exclusive")),
        "expected loop.start invocation, got: {calls}"
    );

    let running_event = recv_topic_event(&mut live_stream, "loop.status.changed").await;
    assert_eq!(running_event["payload"]["from"], "none");
    assert_eq!(running_event["payload"]["to"], "running");
    let resume_cursor = running_event["cursor"]
        .as_str()
        .expect("cursor should be present")
        .to_string();

    let loop_event = recv_topic_event(&mut live_stream, "loop.event").await;
    assert_eq!(loop_event["payload"]["event"], "loop.started");
    assert_eq!(loop_event["payload"]["message"], "fake loop started");

    let log_event = recv_topic_event(&mut live_stream, "loop.log.line").await;
    assert_eq!(log_event["payload"]["line"], "fake loop started");
    assert_eq!(log_event["payload"]["source"], "event");

    live_stream.close(None).await?;

    let stop = rpc_request(
        "req-loop-stop-live-1",
        "loop.stop",
        json!({ "id": "(primary)", "force": false }),
        Some("idem-loop-stop-live-1"),
    );
    let (status, stop_payload) = post_rpc(&client, &server, &stop).await?;
    assert_eq!(status, 200, "loop.stop must succeed: {stop_payload}");

    tokio::time::sleep(Duration::from_millis(300)).await;

    let resume = rpc_request(
        "req-loop-stream-resume-1",
        "stream.subscribe",
        json!({
            "topics": ["loop.status.changed", "loop.event", "loop.log.line"],
            "cursor": resume_cursor,
            "filters": { "resourceIds": ["(primary)"] }
        }),
        None,
    );
    let (_, resume_payload) = post_rpc(&client, &server, &resume).await?;
    let resume_subscription_id = resume_payload["result"]["subscriptionId"]
        .as_str()
        .expect("resume subscription id should be present")
        .to_string();

    let mut replay_stream = open_stream(&server, &resume_subscription_id).await?;
    let replay_events = recv_mixed_events(&mut replay_stream, 5).await;

    assert!(replay_events.iter().any(|event| {
        event["topic"] == "loop.event"
            && event["payload"]["event"] == "loop.stopped"
    }));
    assert!(replay_events.iter().any(|event| {
        event["topic"] == "loop.status.changed" && event["replay"]["mode"] == "resume"
    }));

    replay_stream.close(None).await?;
    drop(fake_bin);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn cold_subscribe_delivers_live_filtered_events() -> Result<()> {
    let server = TestServer::start(ApiConfig::default()).await;
    let client = Client::new();

    let task_id = "task-stream-cold-1";
    let subscribe = rpc_request(
        "req-stream-subscribe-cold-1",
        "stream.subscribe",
        json!({
            "topics": ["task.status.changed"],
            "filters": { "resourceIds": [task_id] }
        }),
        None,
    );
    let (status, subscribe_payload) = post_rpc(&client, &server, &subscribe).await?;
    assert_eq!(status, 200);

    let subscription_id = subscribe_payload["result"]["subscriptionId"]
        .as_str()
        .expect("subscription id should be present")
        .to_string();

    let mut stream = open_stream(&server, &subscription_id).await?;

    let create = rpc_request(
        "req-stream-task-create-1",
        "task.create",
        json!({
            "id": task_id,
            "title": "Streaming task",
            "autoExecute": false
        }),
        Some("idem-stream-task-create-1"),
    );
    let (status, _) = post_rpc(&client, &server, &create).await?;
    assert_eq!(status, 200);

    let event = recv_topic_event(&mut stream, "task.status.changed").await;
    assert_eq!(event["resource"]["id"], task_id);
    assert_eq!(event["resource"]["type"], "task");
    assert_eq!(event["payload"]["to"], "open");
    assert_eq!(event["replay"]["mode"], "live");

    stream.close(None).await?;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn resume_with_cursor_replays_ordered_without_duplicates() -> Result<()> {
    let server = TestServer::start(ApiConfig::default()).await;
    let client = Client::new();

    let task_id = "task-stream-resume-1";
    let subscribe_live = rpc_request(
        "req-stream-subscribe-live-1",
        "stream.subscribe",
        json!({
            "topics": ["task.status.changed"],
            "filters": { "resourceIds": [task_id] }
        }),
        None,
    );
    let (_, live_subscribe_payload) = post_rpc(&client, &server, &subscribe_live).await?;
    let live_subscription_id = live_subscribe_payload["result"]["subscriptionId"]
        .as_str()
        .expect("subscription id should be present")
        .to_string();

    let mut live_stream = open_stream(&server, &live_subscription_id).await?;

    let create = rpc_request(
        "req-stream-resume-create-1",
        "task.create",
        json!({
            "id": task_id,
            "title": "Resume task",
            "autoExecute": false
        }),
        Some("idem-stream-resume-create-1"),
    );
    let (status, _) = post_rpc(&client, &server, &create).await?;
    assert_eq!(status, 200);

    let first_event = recv_topic_event(&mut live_stream, "task.status.changed").await;
    let first_cursor = first_event["cursor"]
        .as_str()
        .expect("cursor should be present")
        .to_string();

    live_stream.close(None).await?;

    let update = rpc_request(
        "req-stream-resume-update-1",
        "task.update",
        json!({ "id": task_id, "status": "running" }),
        Some("idem-stream-resume-update-1"),
    );
    let (status, _) = post_rpc(&client, &server, &update).await?;
    assert_eq!(status, 200);

    let close = rpc_request(
        "req-stream-resume-close-1",
        "task.close",
        json!({ "id": task_id }),
        Some("idem-stream-resume-close-1"),
    );
    let (status, _) = post_rpc(&client, &server, &close).await?;
    assert_eq!(status, 200);

    let subscribe_resume = rpc_request(
        "req-stream-subscribe-resume-1",
        "stream.subscribe",
        json!({
            "topics": ["task.status.changed"],
            "cursor": first_cursor,
            "replayLimit": 10,
            "filters": { "resourceIds": [task_id] }
        }),
        None,
    );
    let (_, resume_subscribe_payload) = post_rpc(&client, &server, &subscribe_resume).await?;
    let resume_subscription_id = resume_subscribe_payload["result"]["subscriptionId"]
        .as_str()
        .expect("subscription id should be present")
        .to_string();

    let mut resume_stream = open_stream(&server, &resume_subscription_id).await?;
    let replay_events = [
        recv_topic_event(&mut resume_stream, "task.status.changed").await,
        recv_topic_event(&mut resume_stream, "task.status.changed").await,
    ];

    let first_sequence = replay_events[0]["sequence"]
        .as_u64()
        .expect("sequence should be present");
    let second_sequence = replay_events[1]["sequence"]
        .as_u64()
        .expect("sequence should be present");

    assert!(second_sequence > first_sequence);
    assert!(
        replay_events
            .iter()
            .all(|event| event["replay"]["mode"] == "resume")
    );
    assert!(
        replay_events
            .iter()
            .all(|event| event["cursor"] != first_event["cursor"])
    );

    resume_stream.close(None).await?;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn replay_overflow_emits_backpressure_error_and_bounds_batch() -> Result<()> {
    let server = TestServer::start(ApiConfig::default()).await;
    let client = Client::new();

    let task_id = "task-stream-overflow-1";

    let subscribe_live = rpc_request(
        "req-stream-overflow-live-1",
        "stream.subscribe",
        json!({
            "topics": ["task.status.changed"],
            "filters": { "resourceIds": [task_id] }
        }),
        None,
    );
    let (_, live_subscribe_payload) = post_rpc(&client, &server, &subscribe_live).await?;
    let live_subscription_id = live_subscribe_payload["result"]["subscriptionId"]
        .as_str()
        .expect("subscription id should be present")
        .to_string();

    let mut live_stream = open_stream(&server, &live_subscription_id).await?;

    let create = rpc_request(
        "req-stream-overflow-create-1",
        "task.create",
        json!({
            "id": task_id,
            "title": "Overflow task",
            "autoExecute": false
        }),
        Some("idem-stream-overflow-create-1"),
    );
    let (status, _) = post_rpc(&client, &server, &create).await?;
    assert_eq!(status, 200);

    let first_event = recv_topic_event(&mut live_stream, "task.status.changed").await;
    let first_cursor = first_event["cursor"]
        .as_str()
        .expect("cursor should be present")
        .to_string();
    live_stream.close(None).await?;

    for index in 0..8 {
        let request_id = format!("req-stream-overflow-update-{index}");
        let idempotency_key = format!("idem-stream-overflow-update-{index}");
        let update = rpc_request(
            &request_id,
            "task.update",
            json!({ "id": task_id, "status": format!("state-{index}") }),
            Some(&idempotency_key),
        );
        let (status, _) = post_rpc(&client, &server, &update).await?;
        assert_eq!(status, 200);
    }

    let subscribe_resume = rpc_request(
        "req-stream-overflow-resume-1",
        "stream.subscribe",
        json!({
            "topics": ["task.status.changed"],
            "cursor": first_cursor,
            "replayLimit": 3,
            "filters": { "resourceIds": [task_id] }
        }),
        None,
    );
    let (_, resume_subscribe_payload) = post_rpc(&client, &server, &subscribe_resume).await?;
    let resume_subscription_id = resume_subscribe_payload["result"]["subscriptionId"]
        .as_str()
        .expect("subscription id should be present")
        .to_string();

    let mut replay_stream = open_stream(&server, &resume_subscription_id).await?;
    let events = recv_mixed_events(&mut replay_stream, 4).await;

    let error_event = events
        .iter()
        .find(|event| event["topic"] == "error.raised")
        .expect("expected overflow error event");
    assert_eq!(error_event["payload"]["code"], "BACKPRESSURE_DROPPED");

    let replay_count = events
        .iter()
        .filter(|event| event["topic"] == "task.status.changed")
        .count();
    assert_eq!(replay_count, 3);

    replay_stream.close(None).await?;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn reconnect_with_ack_cursor_replays_only_new_events() -> Result<()> {
    let server = TestServer::start(ApiConfig::default()).await;
    let client = Client::new();

    let task_id = "task-stream-reconnect-1";
    let subscribe = rpc_request(
        "req-stream-reconnect-subscribe-1",
        "stream.subscribe",
        json!({
            "topics": ["task.status.changed"],
            "filters": { "resourceIds": [task_id] }
        }),
        None,
    );
    let (_, subscribe_payload) = post_rpc(&client, &server, &subscribe).await?;
    let subscription_id = subscribe_payload["result"]["subscriptionId"]
        .as_str()
        .expect("subscription id should be present")
        .to_string();

    let mut first_stream = open_stream(&server, &subscription_id).await?;

    let create = rpc_request(
        "req-stream-reconnect-create-1",
        "task.create",
        json!({
            "id": task_id,
            "title": "Reconnect task",
            "autoExecute": false
        }),
        Some("idem-stream-reconnect-create-1"),
    );
    let (status, _) = post_rpc(&client, &server, &create).await?;
    assert_eq!(status, 200);

    let first_event = recv_topic_event(&mut first_stream, "task.status.changed").await;
    let first_cursor = first_event["cursor"]
        .as_str()
        .expect("cursor should be present")
        .to_string();

    let ack = rpc_request(
        "req-stream-reconnect-ack-1",
        "stream.ack",
        json!({
            "subscriptionId": subscription_id,
            "cursor": first_cursor
        }),
        None,
    );
    let (status, ack_payload) = post_rpc(&client, &server, &ack).await?;
    assert_eq!(status, 200);
    assert_eq!(ack_payload["result"]["success"], true);

    first_stream.close(None).await?;

    let update = rpc_request(
        "req-stream-reconnect-update-1",
        "task.update",
        json!({ "id": task_id, "status": "running" }),
        Some("idem-stream-reconnect-update-1"),
    );
    let (status, _) = post_rpc(&client, &server, &update).await?;
    assert_eq!(status, 200);

    let close = rpc_request(
        "req-stream-reconnect-close-1",
        "task.close",
        json!({ "id": task_id }),
        Some("idem-stream-reconnect-close-1"),
    );
    let (status, _) = post_rpc(&client, &server, &close).await?;
    assert_eq!(status, 200);

    let mut reconnect_stream = open_stream(&server, &subscription_id).await?;
    let replay_events = [
        recv_topic_event(&mut reconnect_stream, "task.status.changed").await,
        recv_topic_event(&mut reconnect_stream, "task.status.changed").await,
    ];

    let first_sequence = first_event["sequence"]
        .as_u64()
        .expect("sequence should exist");
    assert!(replay_events.iter().all(|event| {
        event["sequence"]
            .as_u64()
            .is_some_and(|sequence| sequence > first_sequence)
    }));
    assert!(
        replay_events
            .iter()
            .all(|event| event["cursor"] != first_event["cursor"])
    );

    reconnect_stream.close(None).await?;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn token_mode_stream_requires_matching_ws_principal() -> Result<()> {
    let mut config = ApiConfig::default();
    config.auth_mode = ralph_api::AuthMode::Token;
    config.token = Some("super-secret-token".to_string());

    let server = TestServer::start(config).await;
    let client = Client::new();

    let subscribe = rpc_request(
        "req-stream-token-subscribe-1",
        "stream.subscribe",
        json!({
            "topics": ["task.status.changed"],
            "filters": { "resourceIds": ["task-token-1"] }
        }),
        None,
    );

    let (status, subscribe_payload) =
        post_rpc_with_token(&client, &server, &subscribe, Some("super-secret-token")).await?;
    assert_eq!(status, 200);

    let subscription_id = subscribe_payload["result"]["subscriptionId"]
        .as_str()
        .expect("subscription id should be present")
        .to_string();

    let no_token_result = open_stream_with_token(&server, &subscription_id, None).await;
    assert!(no_token_result.is_err());

    let wrong_token_result =
        open_stream_with_token(&server, &subscription_id, Some("wrong-token")).await;
    assert!(wrong_token_result.is_err());

    let mut stream =
        open_stream_with_token(&server, &subscription_id, Some("super-secret-token")).await?;
    stream.close(None).await?;

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn unsubscribe_removes_subscription() -> Result<()> {
    let server = TestServer::start(ApiConfig::default()).await;
    let client = Client::new();

    let subscribe = rpc_request(
        "req-stream-unsubscribe-sub",
        "stream.subscribe",
        json!({
            "topics": ["task.status.changed"],
            "filters": { "resourceIds": ["task-unsub-1"] }
        }),
        None,
    );
    let (status, subscribe_payload) = post_rpc(&client, &server, &subscribe).await?;
    assert_eq!(status, 200);

    let subscription_id = subscribe_payload["result"]["subscriptionId"]
        .as_str()
        .expect("subscription id should be present")
        .to_string();

    let mut stream = open_stream(&server, &subscription_id).await?;

    // Create task
    let create = rpc_request(
        "req-stream-unsubscribe-create",
        "task.create",
        json!({ "id": "task-unsub-1", "title": "Unsub Task", "status": "open", "priority": 1, "autoExecute": false }),
        Some("idem-unsub-create"),
    );
    let (status, _) = post_rpc(&client, &server, &create).await?;
    assert_eq!(status, 200);

    // Verify event received
    let event = recv_topic_event(&mut stream, "task.status.changed").await;
    assert_eq!(event["resource"]["id"], "task-unsub-1");

    // Unsubscribe
    let unsubscribe = rpc_request(
        "req-stream-unsubscribe-call",
        "stream.unsubscribe",
        json!({ "subscriptionId": subscription_id }),
        Some("idem-unsub-call"),
    );
    let (status, payload) = post_rpc(&client, &server, &unsubscribe).await?;
    assert!(status == 200, "unsubscribe failed: {:?}", payload);
    assert_eq!(status, 200);
    assert_eq!(payload["result"]["success"], true);

    // Update task
    let update = rpc_request(
        "req-stream-unsubscribe-update",
        "task.update",
        json!({ "id": "task-unsub-1", "status": "running" }),
        Some("idem-unsub-update"),
    );
    let (status, _) = post_rpc(&client, &server, &update).await?;
    assert_eq!(status, 200);

    // Verify no further event is received
    let next_msg = timeout(Duration::from_millis(500), stream.next()).await;
    match next_msg {
        Err(_) => {}   // Timeout, meaning no event
        Ok(None) => {} // Closed
        Ok(Some(Ok(msg))) => {
            if !msg.is_close() {
                if let Message::Text(text) = &msg {
                    let payload: Value = serde_json::from_str(text).unwrap();
                    if payload["topic"] == "stream.keepalive" {
                        // Keepalives are expected, but we shouldn't get the task.status.changed
                    } else {
                        panic!("Received unexpected message after unsubscribe: {:?}", msg);
                    }
                } else {
                    panic!("Received unexpected message after unsubscribe: {:?}", msg);
                }
            }
        }
        Ok(Some(Err(_))) => {} // Error, probably closed
    }

    server.stop().await;
    Ok(())
}
