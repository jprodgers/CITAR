//! The model endpoint (design/phase4-helper.md P4.3.4), its discovery half: which models the
//! machine's endpoint serves and which are loaded, for the hello and for `catalog` whenever that
//! changes (polled every 30 s). The proxy that runs completions arrives in 4-04.
//!
//! - **lmstudio:** `/api/v0/models` (load state and context length; embedding models left out,
//!   since they cannot chat), else the OpenAI `/v1/models`;
//! - **ollama:** `/v1/models`, and `/api/ps` for what is loaded;
//! - **openai_compatible**, **openai:** `/v1/models`, with the key from `api_key_env` for
//!   `openai`; a model such a server lists is one it serves, so it counts as loaded.
//!
//! The configured base URL is the only address dialled, with no system proxy. A configured model
//! list is the owner's allowlist (protocol 1's rule, `agent.py:234-243`): only those models are
//! offered, an allowlisted model the endpoint does not list still among them (not loaded).
//! [`choose`] caps the list as PROTOCOL.md's "What the helper caps" says. Replaces
//! `citar/worker/agent.py` `discover_models`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::protocol::{Model, Provider};

/// The most models a hello or catalog carries.
pub const MAX_MODELS: usize = 500;
/// The longest model key sent; a longer one is left out.
pub const MAX_KEY: usize = 200;
/// The longest label sent; a longer one is cut.
pub const MAX_LABEL: usize = 200;
/// How long one listing may take.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// A model as the endpoint listed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub key: String,
    pub label: String,
    pub context: Option<u64>,
    pub loaded: bool,
}

/// A boxed future, so endpoints can be swapped for a fake.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A model endpoint, as the helper sees it: the real one over HTTP, or the transcripts' fake.
pub trait Endpoint: Send + Sync {
    /// The models it serves now.
    fn list(&self) -> BoxFuture<'_, Result<Vec<Listed>, String>>;
}

/// The endpoint over HTTP.
pub struct HttpEndpoint {
    client: reqwest::Client,
    provider: Provider,
    base: String,
    api_key: Option<String>,
}

impl HttpEndpoint {
    /// The endpoint at `base_url` (`…/v1`), with the TLS settings it may need for a remote API.
    ///
    /// # Errors
    /// The HTTP client could not be built.
    pub fn new(
        provider: Provider,
        base_url: &str,
        api_key: Option<String>,
        tls: rustls::ClientConfig,
    ) -> Result<HttpEndpoint, String> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(LIST_TIMEOUT)
            .connect_timeout(LIST_TIMEOUT)
            .tls_backend_preconfigured(tls)
            .build()
            .map_err(|e| format!("the HTTP client: {e}"))?;
        Ok(HttpEndpoint {
            client,
            provider,
            base: base_url.trim_end_matches('/').to_owned(),
            api_key,
        })
    }

    /// The server's root: the base without its `/v1`.
    fn root(&self) -> &str {
        self.base.strip_suffix("/v1").unwrap_or(&self.base)
    }

    async fn get(&self, url: &str) -> Result<Value, String> {
        let mut req = self.client.get(url);
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await.map_err(|e| format!("{url}: {e}"))?;
        let status = resp.status();
        let body = resp.bytes().await.map_err(|e| format!("{url}: {e}"))?;
        if !status.is_success() {
            let text: String = String::from_utf8_lossy(&body).chars().take(300).collect();
            return Err(format!("{url}: HTTP {status}: {text}"));
        }
        serde_json::from_slice(&body).map_err(|e| format!("{url}: not JSON ({e})"))
    }

    /// `/v1/models`' ids.
    async fn openai_models(&self, loaded: bool) -> Result<Vec<Listed>, String> {
        let doc = self.get(&format!("{}/models", self.base)).await?;
        Ok(data(&doc)
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str))
            .map(|id| Listed { key: id.to_owned(), label: id.to_owned(), context: None, loaded })
            .collect())
    }

    async fn lmstudio(&self) -> Result<Vec<Listed>, String> {
        match self.get(&format!("{}/api/v0/models", self.root())).await {
            Ok(doc) => Ok(data(&doc)
                .iter()
                .filter(|m| m.get("type").and_then(Value::as_str) != Some("embeddings"))
                .filter_map(|m| {
                    let id = m.get("id")?.as_str()?;
                    Some(Listed {
                        key: id.to_owned(),
                        label: id.to_owned(),
                        context: m.get("max_context_length").and_then(Value::as_u64),
                        loaded: m.get("state").and_then(Value::as_str) == Some("loaded"),
                    })
                })
                .collect()),
            // An older LM Studio, or one behind something that only speaks OpenAI's API: the
            // models are known, their load state is not.
            Err(_) => self.openai_models(false).await,
        }
    }

    async fn ollama(&self) -> Result<Vec<Listed>, String> {
        let mut models = self.openai_models(false).await?;
        if let Ok(ps) = self.get(&format!("{}/api/ps", self.root())).await {
            let running: Vec<&Value> = ps
                .get("models")
                .and_then(Value::as_array)
                .map(|a| a.iter().collect())
                .unwrap_or_default();
            for m in &mut models {
                if let Some(r) = running.iter().find(|r| {
                    r.get("name").and_then(Value::as_str) == Some(&m.key)
                        || r.get("model").and_then(Value::as_str) == Some(&m.key)
                }) {
                    m.loaded = true;
                    m.context = r.get("context_length").and_then(Value::as_u64);
                }
            }
        }
        Ok(models)
    }
}

fn data(doc: &Value) -> Vec<Value> {
    doc.get("data").and_then(Value::as_array).cloned().unwrap_or_default()
}

impl Endpoint for HttpEndpoint {
    fn list(&self) -> BoxFuture<'_, Result<Vec<Listed>, String>> {
        Box::pin(async move {
            match self.provider {
                Provider::LmStudio => self.lmstudio().await,
                Provider::Ollama => self.ollama().await,
                Provider::OpenAiCompatible | Provider::OpenAi => self.openai_models(true).await,
            }
        })
    }
}

/// A shared endpoint.
pub type SharedEndpoint = Arc<dyn Endpoint>;

/// A label cut to [`MAX_LABEL`] characters.
fn cut(s: &str) -> String {
    s.chars().take(MAX_LABEL).collect()
}

/// The models to offer: with an allowlist, its models in its order (the endpoint's facts where
/// it lists them, else not loaded); without one, everything listed. At most [`MAX_MODELS`],
/// chosen allowlist first, then the loaded ones, then by key; a key over [`MAX_KEY`] characters
/// is left out and a label cut. The result is sorted by key, so an unchanged set compares equal
/// whatever order the endpoint lists it in.
#[must_use]
pub fn choose(listed: &[Listed], allowlist: Option<&[String]>) -> Vec<Model> {
    let fits = |k: &str| !k.is_empty() && k.chars().count() <= MAX_KEY;
    let mut chosen: Vec<Listed> = Vec::new();
    match allowlist {
        Some(allow) => {
            for key in allow.iter().filter(|k| fits(k)) {
                if chosen.iter().any(|c| &c.key == key) {
                    continue;
                }
                let found = listed.iter().find(|l| &l.key == key).cloned();
                chosen.push(found.unwrap_or_else(|| Listed {
                    key: key.clone(),
                    label: key.clone(),
                    context: None,
                    loaded: false,
                }));
            }
        }
        None => {
            for l in listed.iter().filter(|l| fits(&l.key)) {
                if !chosen.iter().any(|c| c.key == l.key) {
                    chosen.push(l.clone());
                }
            }
            chosen.sort_by(|a, b| b.loaded.cmp(&a.loaded).then_with(|| a.key.cmp(&b.key)));
        }
    }
    chosen.truncate(MAX_MODELS);
    chosen.sort_by(|a, b| a.key.cmp(&b.key));
    chosen
        .into_iter()
        .map(|l| Model { key: l.key, label: cut(&l.label), context: l.context, loaded: l.loaded })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(key: &str, loaded: bool) -> Listed {
        Listed { key: key.to_owned(), label: key.to_owned(), context: None, loaded }
    }

    #[test]
    fn six_hundred_models_become_the_first_five_hundred_by_the_rule() {
        let mut listed: Vec<Listed> = (0..600).map(|i| l(&format!("m{i:03}"), false)).collect();
        listed[599].loaded = true;
        listed.push(l(&"k".repeat(201), true));
        listed.push(Listed { label: "L".repeat(300), ..l("long-label", false) });
        let chosen = choose(&listed, None);
        assert_eq!(chosen.len(), 500);
        assert!(chosen.iter().any(|m| m.key == "m599" && m.loaded), "loaded ones first");
        assert!(!chosen.iter().any(|m| m.key.len() > 200));
        assert!(chosen.windows(2).all(|w| w[0].key < w[1].key), "sorted by key");
        assert!(chosen.iter().all(|m| m.label.chars().count() <= 200));
        let allow: Vec<String> = vec!["m550".into(), "absent".into(), "m550".into()];
        let a = choose(&listed, Some(&allow));
        assert_eq!(a.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(), ["absent", "m550"]);
        assert!(!a[0].loaded);
    }
}
