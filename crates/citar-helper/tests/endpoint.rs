//! Model discovery against each kind of endpoint (design/phase4-helper.md P4.3.4), over real HTTP
//! on loopback: LM Studio's `/api/v0/models` (load state, context, embedding models left out) and
//! its fallback to `/v1/models`; Ollama's `/v1/models` with `/api/ps` for what is loaded; an
//! OpenAI-compatible server's `/v1/models`, everything it lists counted as loaded; `openai` with
//! the key sent to the configured address only; and an endpoint listing 600 models, of which the
//! helper offers 500 by PROTOCOL.md's rule.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use citar_helper::llm::{Endpoint as _, HttpEndpoint, Listed, choose};
use citar_helper::net::verified_tls;
use citar_helper::protocol::Provider;
use common::loopback_routes;
use serde_json::json;

fn route(path: &str, body: serde_json::Value) -> (String, (u16, String)) {
    (format!("GET {path}"), (200, body.to_string()))
}

async fn endpoint(
    provider: Provider,
    routes: BTreeMap<String, (u16, String)>,
    key: Option<&str>,
) -> (Result<Vec<Listed>, String>, Vec<String>) {
    let heads = Arc::new(Mutex::new(Vec::new()));
    let port = loopback_routes(routes, heads.clone()).await;
    let ep = HttpEndpoint::new(
        provider,
        &format!("http://127.0.0.1:{port}/v1/"),
        key.map(str::to_owned),
        verified_tls(None).expect("tls"),
    )
    .expect("a client");
    let listed = ep.list().await;
    let seen = heads.lock().expect("heads").clone();
    (listed, seen)
}

fn l(key: &str, context: Option<u64>, loaded: bool) -> Listed {
    Listed { key: key.to_owned(), label: key.to_owned(), context, loaded }
}

#[tokio::test]
async fn lm_studio_reports_load_state_and_context_and_leaves_out_embeddings() {
    let routes = BTreeMap::from([route(
        "/api/v0/models",
        json!({"data": [
            {"id": "google/gemma-4-e2b", "type": "llm", "state": "loaded", "max_context_length": 131072},
            {"id": "qwen", "type": "vlm", "state": "not-loaded", "max_context_length": 32768},
            {"id": "nomic-embed", "type": "embeddings", "state": "loaded"}
        ]}),
    )]);
    let (got, heads) = endpoint(Provider::LmStudio, routes, None).await;
    assert_eq!(
        got.expect("listed"),
        [l("google/gemma-4-e2b", Some(131_072), true), l("qwen", Some(32_768), false)]
    );
    assert!(heads.iter().all(|h| !h.to_ascii_lowercase().contains("authorization")), "{heads:?}");
}

#[tokio::test]
async fn lm_studio_without_its_own_api_falls_back_to_the_openai_list() {
    let routes =
        BTreeMap::from([route("/v1/models", json!({"data": [{"id": "m1"}, {"id": "m2"}]}))]);
    let (got, _) = endpoint(Provider::LmStudio, routes, None).await;
    assert_eq!(got.expect("listed"), [l("m1", None, false), l("m2", None, false)]);
}

#[tokio::test]
async fn ollama_reads_what_is_loaded_from_api_ps() {
    let routes = BTreeMap::from([
        route("/v1/models", json!({"data": [{"id": "llama3:latest"}, {"id": "phi4:latest"}]})),
        route(
            "/api/ps",
            json!({"models": [{"name": "phi4:latest", "model": "phi4:latest", "context_length": 8192}]}),
        ),
    ]);
    let (got, _) = endpoint(Provider::Ollama, routes, None).await;
    assert_eq!(
        got.expect("listed"),
        [l("llama3:latest", None, false), l("phi4:latest", Some(8192), true)]
    );
}

#[tokio::test]
async fn an_openai_compatible_server_serves_what_it_lists_and_openai_gets_its_key() {
    let routes = BTreeMap::from([route("/v1/models", json!({"data": [{"id": "gpt-x"}]}))]);
    let (got, heads) = endpoint(Provider::OpenAiCompatible, routes.clone(), None).await;
    assert_eq!(got.expect("listed"), [l("gpt-x", None, true)]);
    assert!(heads.iter().all(|h| !h.to_ascii_lowercase().contains("authorization")));
    let (got, heads) = endpoint(Provider::OpenAi, routes, Some("sk-test-key")).await;
    assert_eq!(got.expect("listed"), [l("gpt-x", None, true)]);
    assert!(heads.iter().any(|h| h.contains("Bearer sk-test-key")), "{heads:?}");
}

#[tokio::test]
async fn an_endpoint_that_fails_says_so() {
    let (got, _) = endpoint(Provider::OpenAiCompatible, BTreeMap::new(), None).await;
    let e = got.expect_err("404");
    assert!(e.contains("404"), "{e}");
}

#[tokio::test]
async fn six_hundred_models_are_offered_as_the_first_five_hundred() {
    let data: Vec<_> = (0..600)
        .map(|i| {
            json!({"id": format!("model-{i:03}"), "type": "llm",
                        "state": if i % 100 == 99 { "loaded" } else { "not-loaded" }})
        })
        .chain([json!({"id": "k".repeat(201), "type": "llm", "state": "loaded"})])
        .collect();
    let routes = BTreeMap::from([route("/api/v0/models", json!({"data": data}))]);
    let (got, _) = endpoint(Provider::LmStudio, routes, None).await;
    let listed = got.expect("listed");
    assert_eq!(listed.len(), 601);
    let offered = choose(&listed, None);
    assert_eq!(offered.len(), 500);
    for loaded in ["model-099", "model-199", "model-299", "model-399", "model-499", "model-599"] {
        assert!(offered.iter().any(|m| m.key == loaded && m.loaded), "{loaded} is loaded: kept");
    }
    assert!(offered.iter().all(|m| m.key.len() <= 200));
    assert!(!offered.iter().any(|m| m.key == "model-499" && !m.loaded));
    // The 494 not loaded that fit, by key: model-000 to model-497 less the four loaded among them.
    assert!(offered.iter().any(|m| m.key == "model-497"));
    assert!(!offered.iter().any(|m| m.key == "model-498"));
}
