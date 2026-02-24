use std::fs;
use std::path::Path;

use anyhow::Result;
use reqwest::Client;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use ralph_api::{ApiConfig, RpcRuntime, serve_with_listener};

struct TestServer {
    base_url: String,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
    workspace: TempDir,
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
            workspace,
        }
    }

    fn workspace_path(&self) -> &Path {
        self.workspace.path()
    }

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let result = self.join.await.expect("server task should join");
        result.expect("server should shutdown cleanly");
    }
}

async fn post_rpc(client: &Client, server: &TestServer, body: &Value) -> Result<(u16, Value)> {
    let response = client
        .post(format!("{}/rpc/v1", server.base_url))
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await?;

    let status = response.status().as_u16();
    let payload = response.json::<Value>().await?;
    Ok((status, payload))
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

#[tokio::test]
async fn planning_methods_cover_lifecycle_and_artifacts() -> Result<()> {
    let server = TestServer::start(ApiConfig::default()).await;
    let client = Client::new();

    let start = rpc_request(
        "req-plan-start-1",
        "planning.start",
        json!({ "prompt": "Draft an RPC migration plan" }),
        Some("idem-plan-start-1"),
    );
    let (status, start_payload) = post_rpc(&client, &server, &start).await?;
    assert_eq!(status, 200);

    let session_id = start_payload["result"]["session"]["id"]
        .as_str()
        .expect("session id should be present")
        .to_string();

    let list = rpc_request("req-plan-list-1", "planning.list", json!({}), None);
    let (_, list_payload) = post_rpc(&client, &server, &list).await?;
    assert!(
        list_payload["result"]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|session| session["id"] == session_id)
    );

    let get = rpc_request(
        "req-plan-get-1",
        "planning.get",
        json!({ "id": session_id.clone() }),
        None,
    );
    let (_, get_payload) = post_rpc(&client, &server, &get).await?;
    assert_eq!(get_payload["result"]["session"]["id"], session_id);

    let respond = rpc_request(
        "req-plan-respond-1",
        "planning.respond",
        json!({
            "sessionId": session_id.clone(),
            "promptId": "q1",
            "response": "Use fixture conformance first"
        }),
        Some("idem-plan-respond-1"),
    );
    let (status, respond_payload) = post_rpc(&client, &server, &respond).await?;
    assert_eq!(status, 200);
    assert_eq!(respond_payload["result"]["success"], true);

    let resume = rpc_request(
        "req-plan-resume-1",
        "planning.resume",
        json!({ "id": session_id.clone() }),
        Some("idem-plan-resume-1"),
    );
    let (status, resume_payload) = post_rpc(&client, &server, &resume).await?;
    assert_eq!(status, 200);
    assert_eq!(resume_payload["result"]["success"], true);

    let artifact_path = server
        .workspace_path()
        .join(".ralph/planning-sessions")
        .join(&session_id)
        .join("artifacts")
        .join("plan.md");
    fs::create_dir_all(
        artifact_path
            .parent()
            .expect("artifact parent directory should exist"),
    )?;
    fs::write(&artifact_path, "# Plan\n- Add rpc methods")?;

    let get_artifact = rpc_request(
        "req-plan-artifact-1",
        "planning.get_artifact",
        json!({ "sessionId": session_id.clone(), "filename": "plan.md" }),
        None,
    );
    let (status, artifact_payload) = post_rpc(&client, &server, &get_artifact).await?;
    assert_eq!(status, 200);
    assert_eq!(artifact_payload["result"]["filename"], "plan.md");
    assert!(
        artifact_payload["result"]["content"]
            .as_str()
            .is_some_and(|content| content.contains("Add rpc methods"))
    );

    let delete = rpc_request(
        "req-plan-delete-1",
        "planning.delete",
        json!({ "id": session_id.clone() }),
        Some("idem-plan-delete-1"),
    );
    let (status, delete_payload) = post_rpc(&client, &server, &delete).await?;
    assert_eq!(status, 200);
    assert_eq!(delete_payload["result"]["success"], true);

    let get_deleted = rpc_request(
        "req-plan-get-deleted-1",
        "planning.get",
        json!({ "id": session_id.clone() }),
        None,
    );
    let (status, missing_payload) = post_rpc(&client, &server, &get_deleted).await?;
    assert_eq!(status, 404);
    assert_eq!(
        missing_payload["error"]["code"],
        "PLANNING_SESSION_NOT_FOUND"
    );

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn config_methods_validate_yaml_and_persist_atomically() -> Result<()> {
    let server = TestServer::start(ApiConfig::default()).await;
    let client = Client::new();

    let missing_get = rpc_request("req-config-get-missing-1", "config.get", json!({}), None);
    let (status, missing_payload) = post_rpc(&client, &server, &missing_get).await?;
    assert_eq!(status, 404);
    assert_eq!(missing_payload["error"]["code"], "NOT_FOUND");

    let invalid_update = rpc_request(
        "req-config-update-invalid-1",
        "config.update",
        json!({ "content": "invalid: [" }),
        Some("idem-config-update-invalid-1"),
    );
    let (status, invalid_payload) = post_rpc(&client, &server, &invalid_update).await?;
    assert_eq!(status, 400);
    assert_eq!(invalid_payload["error"]["code"], "CONFIG_INVALID");

    let valid_content = "backend: claude\nmax_iterations: 5\n";
    let valid_update = rpc_request(
        "req-config-update-valid-1",
        "config.update",
        json!({ "content": valid_content }),
        Some("idem-config-update-valid-1"),
    );
    let (status, update_payload) = post_rpc(&client, &server, &valid_update).await?;
    assert_eq!(status, 200);
    assert_eq!(update_payload["result"]["success"], true);
    assert_eq!(update_payload["result"]["parsed"]["backend"], "claude");

    let persisted = fs::read_to_string(server.workspace_path().join("ralph.yml"))?;
    assert_eq!(persisted, valid_content);

    let get = rpc_request("req-config-get-1", "config.get", json!({}), None);
    let (status, get_payload) = post_rpc(&client, &server, &get).await?;
    assert_eq!(status, 200);
    assert_eq!(get_payload["result"]["raw"], valid_content);
    assert_eq!(get_payload["result"]["parsed"]["max_iterations"], 5);

    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn collection_and_preset_methods_cover_crud_import_export_and_ordering() -> Result<()> {
    let server = TestServer::start(ApiConfig::default()).await;
    let client = Client::new();

    let builtin_dir = server.workspace_path().join("presets");
    let hats_dir = server.workspace_path().join(".ralph/hats");
    fs::create_dir_all(&builtin_dir)?;
    fs::create_dir_all(&hats_dir)?;
    fs::write(builtin_dir.join("b.yml"), "description: Builtin B\n")?;
    fs::write(builtin_dir.join("a.yml"), "description: Builtin A\n")?;
    fs::write(hats_dir.join("z.yml"), "description: Directory Z\n")?;

    let create = rpc_request(
        "req-collection-create-1",
        "collection.create",
        json!({
            "name": "Team Flow",
            "description": "Primary workflow",
            "graph": {
                "nodes": [
                    {
                        "id": "builder-node",
                        "type": "hatNode",
                        "position": { "x": 0, "y": 0 },
                        "data": {
                            "key": "builder",
                            "name": "Builder",
                            "description": "Builds features",
                            "triggersOn": ["task.start"],
                            "publishes": ["build.done"]
                        }
                    }
                ],
                "edges": [],
                "viewport": { "x": 0, "y": 0, "zoom": 1 }
            }
        }),
        Some("idem-collection-create-1"),
    );
    let (status, create_payload) = post_rpc(&client, &server, &create).await?;
    assert_eq!(status, 200);

    let collection_id = create_payload["result"]["collection"]["id"]
        .as_str()
        .expect("collection id should be present")
        .to_string();

    let update = rpc_request(
        "req-collection-update-1",
        "collection.update",
        json!({ "id": collection_id.clone(), "name": "Team Flow Updated" }),
        Some("idem-collection-update-1"),
    );
    let (_, update_payload) = post_rpc(&client, &server, &update).await?;
    assert_eq!(
        update_payload["result"]["collection"]["name"],
        "Team Flow Updated"
    );

    let export = rpc_request(
        "req-collection-export-1",
        "collection.export",
        json!({ "id": collection_id.clone() }),
        None,
    );
    let (status, export_payload) = post_rpc(&client, &server, &export).await?;
    assert_eq!(status, 200);
    assert!(
        export_payload["result"]["yaml"]
            .as_str()
            .is_some_and(|yaml| yaml.contains("Team Flow Updated"))
    );

    let import_yaml = r"
hats:
  scout:
    name: Scout
    description: Scout phase
    triggers: [task.start]
    publishes: [plan.start]
  builder:
    name: Builder
    description: Builder phase
    triggers: [plan.start]
    publishes: [build.done]
";

    let import = rpc_request(
        "req-collection-import-1",
        "collection.import",
        json!({
            "yaml": import_yaml,
            "name": "Imported Flow",
            "description": "Imported from yaml"
        }),
        Some("idem-collection-import-1"),
    );
    let (status, import_payload) = post_rpc(&client, &server, &import).await?;
    assert_eq!(status, 200);
    assert_eq!(
        import_payload["result"]["collection"]["name"],
        "Imported Flow"
    );

    let list = rpc_request("req-collection-list-1", "collection.list", json!({}), None);
    let (_, list_payload) = post_rpc(&client, &server, &list).await?;
    assert!(
        list_payload["result"]["collections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|collection| collection["id"] == collection_id)
    );

    let presets = rpc_request("req-preset-list-1", "preset.list", json!({}), None);
    let (status, presets_payload) = post_rpc(&client, &server, &presets).await?;
    assert_eq!(status, 200);

    let presets = presets_payload["result"]["presets"]
        .as_array()
        .expect("preset list should be an array");
    assert_eq!(presets[0]["id"], "builtin:a");
    assert_eq!(presets[1]["id"], "builtin:b");
    assert_eq!(presets[2]["id"], "directory:z");

    let collection_names: Vec<String> = presets
        .iter()
        .filter(|preset| preset["source"] == "collection")
        .filter_map(|preset| {
            preset["name"]
                .as_str()
                .map(std::string::ToString::to_string)
        })
        .collect();
    let mut sorted_collection_names = collection_names.clone();
    sorted_collection_names.sort();
    assert_eq!(collection_names, sorted_collection_names);

    let delete = rpc_request(
        "req-collection-delete-1",
        "collection.delete",
        json!({ "id": collection_id.clone() }),
        Some("idem-collection-delete-1"),
    );
    let (status, delete_payload) = post_rpc(&client, &server, &delete).await?;
    assert_eq!(status, 200);
    assert_eq!(delete_payload["result"]["success"], true);

    let get_deleted = rpc_request(
        "req-collection-get-deleted-1",
        "collection.get",
        json!({ "id": collection_id.clone() }),
        None,
    );
    let (status, missing_payload) = post_rpc(&client, &server, &get_deleted).await?;
    assert_eq!(status, 404);
    assert_eq!(missing_payload["error"]["code"], "COLLECTION_NOT_FOUND");

    server.stop().await;
    Ok(())
}
