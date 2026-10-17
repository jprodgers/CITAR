//! The update's download (design/phase4-helper.md P4.3.8, steps 3 and 4; P4.3.10): the signed
//! manifest's asset URL and the redirects it leads to, nothing else.
//!
//! - **Where:** `https` only, at most [`MAX_REDIRECTS`] redirects, each checked as the first URL
//!   was ([`allowed`]), GitHub serving release assets from another host. A development or test
//!   build (`dev-guard`, or a test feature) also takes `http` to a loopback host (the tests' asset
//!   server) and dials loopback addresses only, on the URL before any lookup and again on every
//!   address the name resolves to ([`LoopbackOnly`]), as the connection to the server does
//!   ([`crate::net`]). No system proxy, and the TLS is the helper's own (the OS's trust store).
//! - **What:** the integrity is the signed size and SHA-256, never the host: the body is counted
//!   as it comes and refused past the size, a `Content-Length` over it refused before the body,
//!   and the whole file checked against both before it is staged ([`download`]). The file is
//!   written beside its final name (`.part`) and renamed once it holds.

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use reqwest::Url;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

use citar_jobs::update::Asset;

/// The most redirects followed.
pub const MAX_REDIRECTS: usize = 5;
/// How long connecting may take.
pub const CONNECT: Duration = Duration::from_secs(30);
/// How long the body may go without a byte before the download counts as dead.
pub const STALL: Duration = Duration::from_secs(60);
/// The longest a whole download may take.
pub const WHOLE: Duration = Duration::from_secs(1800);

/// Why a download did not give the staged file: `Refused` when what the manifest or a redirect
/// leads to breaks the rules (the size, the hash, the scheme), `Failed` when the network did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetch {
    Refused(String),
    Failed(String),
}

/// Whether a host is this machine's: `localhost` or a loopback address.
fn loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost") || h.parse::<IpAddr>().is_ok_and(crate::net::is_loopback)
}

/// Whether the helper may download from `url`: `https` with a host and no user name; `http` only
/// to a loopback host, and only in a development or test build (`loopback_only`), which also
/// refuses any host that is not loopback.
///
/// # Errors
/// Why not, for `update_status refused`.
pub fn allowed(url: &Url, loopback_only: bool) -> Result<(), String> {
    let host = url.host_str().unwrap_or_default();
    if host.is_empty() {
        return Err(format!("the update's address {} has no host", cut(url.as_str())));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("the update's address {} carries a user name", cut(url.as_str())));
    }
    let loopback = loopback_host(host);
    match url.scheme() {
        "https" => {}
        "http" if loopback_only && loopback => {}
        _ => {
            return Err(format!(
                "the update's address {} is not https: updates are downloaded over https only",
                cut(url.as_str())
            ));
        }
    }
    if loopback_only && !loopback {
        return Err(format!(
            "this is a development or test build: it downloads from loopback addresses only, not {}",
            cut(host)
        ));
    }
    Ok(())
}

fn cut(s: &str) -> String {
    crate::log::clip(s, 200)
}

/// A resolver that keeps loopback addresses only (a development or test build's downloads).
pub struct LoopbackOnly;

impl reqwest::dns::Resolve for LoopbackOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| crate::net::is_loopback(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!(
                    "{host} has no loopback address: a development or test build downloads from \
                     this machine only"
                )
                .into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// The HTTP client downloads use: no proxy, no redirect followed by itself (each is checked
/// here), the helper's TLS, and in a development or test build loopback addresses only.
///
/// # Errors
/// It could not be built.
pub fn client(tls: rustls::ClientConfig, loopback_only: bool) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT)
        .tls_backend_preconfigured(tls);
    if loopback_only {
        b = b.dns_resolver(std::sync::Arc::new(LoopbackOnly));
    }
    b.build().map_err(|e| format!("the HTTP client: {e}"))
}

/// The SHA-256 of a file, lowercase hex.
///
/// # Errors
/// It cannot be read.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads `asset` to `part`, follows its redirects as [`allowed`] lets it, checks its size and
/// SHA-256 against the signed ones, and renames it to `staged`.
///
/// # Errors
/// [`Fetch::Refused`] for a rule broken (an address, a size, a hash), [`Fetch::Failed`] for the
/// network or the disk; `part` is removed either way.
pub async fn download(
    client: &reqwest::Client,
    asset: &Asset,
    part: &Path,
    staged: &Path,
    loopback_only: bool,
) -> Result<(), Fetch> {
    let done = tokio::time::timeout(WHOLE, fetch(client, asset, part, loopback_only)).await;
    let out = match done {
        Ok(r) => r,
        Err(_) => {
            Err(Fetch::Failed(format!("the download took over {} minutes", WHOLE.as_secs() / 60)))
        }
    };
    if let Err(e) = out {
        let _gone = tokio::fs::remove_file(part).await;
        return Err(e);
    }
    let _old = tokio::fs::remove_file(staged).await;
    tokio::fs::rename(part, staged)
        .await
        .map_err(|e| Fetch::Failed(format!("staging the download: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _mode =
            tokio::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755)).await;
    }
    Ok(())
}

async fn fetch(
    client: &reqwest::Client,
    asset: &Asset,
    part: &Path,
    loopback_only: bool,
) -> Result<(), Fetch> {
    let mut url = Url::parse(&asset.url)
        .map_err(|e| Fetch::Refused(format!("the update's address does not read: {e}")))?;
    let mut hops = 0;
    let mut resp = loop {
        allowed(&url, loopback_only).map_err(Fetch::Refused)?;
        let r = client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| Fetch::Failed(format!("{}: {e}", cut(url.as_str()))))?;
        if !r.status().is_redirection() {
            break r;
        }
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err(Fetch::Refused(format!(
                "more than {MAX_REDIRECTS} redirects from {}",
                cut(&asset.url)
            )));
        }
        let to =
            r.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).ok_or_else(
                || Fetch::Failed(format!("a redirect with no address from {}", cut(url.as_str()))),
            )?;
        url = url.join(to).map_err(|e| {
            Fetch::Refused(format!("a redirect to an address that does not read: {e}"))
        })?;
    };
    if !resp.status().is_success() {
        return Err(Fetch::Failed(format!("{} answered {}", cut(url.as_str()), resp.status())));
    }
    if let Some(len) = resp.content_length()
        && len > asset.size
    {
        return Err(Fetch::Refused(format!(
            "wrong size: the download is {len} bytes, the signed manifest says {}",
            asset.size
        )));
    }
    if let Some(dir) = part.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| Fetch::Failed(format!("{}: {e}", dir.display())))?;
    }
    let mut file = tokio::fs::File::create(part)
        .await
        .map_err(|e| Fetch::Failed(format!("{}: {e}", part.display())))?;
    let mut hasher = Sha256::new();
    let mut got: u64 = 0;
    loop {
        let chunk = tokio::time::timeout(STALL, resp.chunk())
            .await
            .map_err(|_| Fetch::Failed(format!("the download stalled for {} s", STALL.as_secs())))?
            .map_err(|e| Fetch::Failed(format!("the download broke off: {e}")))?;
        let Some(chunk) = chunk else { break };
        got += chunk.len() as u64;
        if got > asset.size {
            return Err(Fetch::Refused(format!(
                "wrong size: the download is over the {} bytes the signed manifest says",
                asset.size
            )));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| Fetch::Failed(format!("{}: {e}", part.display())))?;
    }
    file.flush().await.map_err(|e| Fetch::Failed(format!("{}: {e}", part.display())))?;
    file.sync_all().await.map_err(|e| Fetch::Failed(format!("{}: {e}", part.display())))?;
    drop(file);
    if got != asset.size {
        return Err(Fetch::Refused(format!(
            "wrong size: the download is {got} bytes, the signed manifest says {}",
            asset.size
        )));
    }
    let sha = hex(&hasher.finalize());
    if sha != asset.sha256 {
        return Err(Fetch::Refused(format!(
            "wrong sha256: the download's is {sha}, the signed manifest's {}",
            asset.sha256
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("a URL")
    }

    #[test]
    fn https_only_and_loopback_http_in_test_builds() {
        let ok = |s: &str, lo: bool| allowed(&url(s), lo);
        assert!(ok("https://github.com/jprodgers/CITAR/releases/download/v0.1.7/x", false).is_ok());
        assert!(ok("https://objects.githubusercontent.com/x", false).is_ok());
        let e = ok("http://github.com/x", false).expect_err("http");
        assert!(e.contains("not https"), "{e}");
        assert!(
            ok("http://127.0.0.1:8080/x", false)
                .expect_err("a release build")
                .contains("not https")
        );
        assert!(ok("ftp://example.com/x", false).is_err());
        assert!(
            ok("https://user:pw@example.com/x", false).expect_err("user").contains("user name")
        );
        // A development or test build: loopback only, http there too.
        assert!(ok("http://127.0.0.1:8080/x", true).is_ok());
        assert!(ok("http://localhost:8080/x", true).is_ok());
        assert!(ok("https://[::1]:8443/x", true).is_ok());
        let e = ok("http://10.0.0.5/x", true).expect_err("not loopback");
        assert!(e.contains("not https"), "{e}");
        let e = ok("https://github.com/x", true).expect_err("not loopback");
        assert!(e.contains("loopback addresses only"), "{e}");
    }
}
