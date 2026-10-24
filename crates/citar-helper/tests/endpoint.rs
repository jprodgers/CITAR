//! Model discovery against each kind of endpoint (design/phase4-helper.md P4.3.4), over real HTTP
//! on loopback: LM Studio's `/api/v0/models` (load state, context, embedding models left out) and
//! its fallback to `/v1/models`; Ollama's `/v1/models` with `/api/ps` for what is loaded; an
//! OpenAI-compatible server's `/v1/models`, everything it lists counted as loaded; `openai` with
//! the key sent to the configured address only; an endpoint listing 600 models, of which the
//! helper offers 500 by PROTOCOL.md's rule; a redirect, which is not followed; and LM Studio's own
//! API failing once after it answered, which keeps what it said was loaded.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use citar_helper::llm::{Endpoint as _, HttpEndpoint, Listed, choose};
use citar_helper::net::verified_tls;
use citar_helper::protocol::Provider;
use common::{loopback_routes, loopback_routes_shared};
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
async fn a_redirect_is_not_followed_and_its_target_never_dialled() {
    // The other address, which must see no request.
    let elsewhere = Arc::new(Mutex::new(Vec::new()));
    let other = loopback_routes(
        BTreeMap::from([route("/v1/models", json!({"data": [{"id": "stolen"}]}))]),
        elsewhere.clone(),
    )
    .await;
    let to = format!("http://127.0.0.1:{other}/v1/models");
    let routes = BTreeMap::from([("GET /v1/models".to_owned(), (302, to.clone()))]);
    let (got, heads) = endpoint(Provider::OpenAi, routes, Some("sk-test-key")).await;
    let e = got.expect_err("a redirect is an error");
    assert!(e.contains("302") && e.contains(&to), "{e}");
    assert_eq!(heads.len(), 1, "{heads:?}");
    // LM Studio: its own API redirects, the fallback is still the configured address's.
    let routes = BTreeMap::from([
        ("GET /api/v0/models".to_owned(), (307, format!("http://127.0.0.1:{other}/api/v0/models"))),
        route("/v1/models", json!({"data": [{"id": "m1"}]})),
    ]);
    let (got, _) = endpoint(Provider::LmStudio, routes, None).await;
    assert_eq!(got.expect("the fallback"), [l("m1", None, false)]);
    assert!(elsewhere.lock().expect("heads").is_empty(), "the redirect's target was dialled");
}

#[tokio::test]
async fn lm_studios_own_api_failing_once_keeps_what_it_said_was_loaded() {
    let v0 = json!({"data": [
        {"id": "gemma", "type": "llm", "state": "loaded", "max_context_length": 8192},
        {"id": "qwen", "type": "llm", "state": "not-loaded", "max_context_length": 4096},
        {"id": "embed", "type": "embeddings", "state": "loaded"}
    ]});
    let v1 = json!({"data": [{"id": "gemma"}, {"id": "qwen"}, {"id": "embed"}, {"id": "new"}]});
    let routes = Arc::new(Mutex::new(BTreeMap::from([
        route("/api/v0/models", v0),
        route("/v1/models", v1),
    ])));
    let port = loopback_routes_shared(routes.clone(), Arc::new(Mutex::new(Vec::new()))).await;
    let ep = HttpEndpoint::new(
        Provider::LmStudio,
        &format!("http://127.0.0.1:{port}/v1"),
        None,
        verified_tls(None).expect("tls"),
    )
    .expect("a client");
    let want = [l("gemma", Some(8192), true), l("qwen", Some(4096), false)];
    assert_eq!(ep.list().await.expect("listed"), want);
    // Its own API fails (a 500): the OpenAI list, read with the last answer's states; the
    // embedding model still left out, a model it never described not loaded.
    routes
        .lock()
        .expect("routes")
        .insert("GET /api/v0/models".to_owned(), (500, "{\"error\": \"busy\"}".to_owned()));
    let got = ep.list().await.expect("listed");
    assert_eq!(got, [want[0].clone(), want[1].clone(), l("new", None, false)]);
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
