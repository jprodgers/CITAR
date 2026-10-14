//! Dialling the server (design/phase4-helper.md P4.3.3): `wss://<server>/ws/worker` (`ws://` for
//! a plain-HTTP server), over a TCP connection and TLS the helper makes itself, so that:
//! - TLS is rustls with the OS's own trust store (`rustls-platform-verifier`), plus `--ca-file`'s
//!   certificates for a self-signed server, and a TLS failure is logged with the host and the
//!   verifier's reason, never relaxed;
//! - `--insecure` (protocol 1's) is honoured only for loopback and private hosts (RFC 1918, ULA,
//!   link-local, `.local`), with a warning, and ignored with a warning for any other;
//! - a development or test build (`dev-guard`, or any test feature) dials loopback addresses
//!   only, checked on the URL before any lookup and again on every address the name resolves to
//!   (P4.3.1).
//!
//! [`Dialer`] is what the session dials through; the transcripts give it an in-memory one.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::log::Log;
use crate::protocol::MAX_FRAME_BYTES;

/// A byte stream the WebSocket runs over: TCP, TLS over TCP, or an in-memory pipe in tests.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// The connection to the server.
pub type Ws = WebSocketStream<Box<dyn Io>>;

/// A boxed future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the session dials through.
pub trait Dialer: Send + Sync {
    /// Opens a new connection to the server, its WebSocket handshake done.
    fn dial(&self) -> BoxFuture<'_, Result<Ws, String>>;
}

/// How long connecting, TLS and the WebSocket handshake may take together.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// The WebSocket's limits: a message may exceed the protocol's cap, so that the parser refuses it
/// as `too_large` (logged and dropped) rather than the socket failing; twice the cap bounds it.
#[must_use]
pub fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(2 * MAX_FRAME_BYTES))
        .max_frame_size(Some(2 * MAX_FRAME_BYTES))
}

/// Where the helper dials, read from the server's address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// `ws[s]://host[:port][/prefix]/ws/worker`.
    pub url: String,
    /// The host as written (brackets dropped from an IPv6 literal).
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

/// The WebSocket URL of a server address (protocol 1's `ws_url`): `https` becomes `wss`, `http`
/// `ws`, and `/ws/worker` is appended to the address, which may have a path of its own.
///
/// # Errors
/// An address that is not `http(s)://` or `ws(s)://` with a host.
pub fn target(server_url: &str) -> Result<Target, String> {
    let url = server_url.trim().trim_end_matches('/');
    let (tls, rest) = if let Some(r) =
        url.strip_prefix("https://").or_else(|| url.strip_prefix("wss://"))
    {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://").or_else(|| url.strip_prefix("ws://")) {
        (false, r)
    } else {
        return Err(format!(
            "the server address {server_url:?} must start with https:// (or http:// for a server \
             on this machine or its network)"
        ));
    };
    let scheme = if tls { "wss" } else { "ws" };
    let full = format!("{scheme}://{rest}/ws/worker");
    let req = full.as_str().into_client_request().map_err(|e| format!("{server_url:?}: {e}"))?;
    let uri = req.uri();
    let host = uri.host().ok_or_else(|| format!("{server_url:?} has no host"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']').to_owned();
    if host.is_empty() {
        return Err(format!("{server_url:?} has no host"));
    }
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    Ok(Target { url: full, host, port, tls })
}

/// Whether an address is this machine's.
#[must_use]
pub fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// Whether a host name or address is loopback or on a private network: where `--insecure` is
/// honoured.
#[must_use]
pub fn is_private_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        Ok(IpAddr::V6(v6)) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || (first & 0xfe00) == 0xfc00 // unique local, fc00::/7
                || (first & 0xffc0) == 0xfe80 // link-local, fe80::/10
                || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback() || v4.is_private())
        }
        Err(_) => false,
    }
}

/// The development guard's rule for the server: `localhost` or a loopback address, nothing else.
///
/// # Errors
/// What it refuses.
pub fn guard_loopback(t: &Target) -> Result<(), String> {
    let ok =
        t.host.eq_ignore_ascii_case("localhost") || t.host.parse::<IpAddr>().is_ok_and(is_loopback);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "this is a development or test build: it dials loopback addresses only, not {}",
            t.host
        ))
    }
}

/// The TLS provider: ring.
#[must_use]
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// TLS that trusts the OS's store, and the certificates in `ca_file` too.
///
/// # Errors
/// The CA file cannot be read, or the platform verifier cannot start.
pub fn verified_tls(ca_file: Option<&Path>) -> Result<ClientConfig, String> {
    let provider = provider();
    let verifier = match ca_file {
        None => rustls_platform_verifier::Verifier::new(provider.clone()),
        Some(path) => {
            let certs: Vec<CertificateDer<'static>> =
                rustls_pki_types::pem::PemObject::pem_file_iter(path)
                    .map_err(|e| format!("--ca-file {}: {e}", path.display()))?
                    .collect::<Result<_, _>>()
                    .map_err(|e| format!("--ca-file {}: {e}", path.display()))?;
            if certs.is_empty() {
                return Err(format!("--ca-file {}: no certificate in it", path.display()));
            }
            platform_with_extra(certs, provider.clone())
        }
    }
    .map_err(|e| format!("the system's certificate verifier: {e}"))?;
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

#[cfg(not(target_os = "android"))]
fn platform_with_extra(
    certs: Vec<CertificateDer<'static>>,
    provider: Arc<CryptoProvider>,
) -> Result<rustls_platform_verifier::Verifier, rustls::Error> {
    rustls_platform_verifier::Verifier::new_with_extra_roots(certs, provider)
}

/// Accepts any certificate, while still checking the handshake's signatures: `--insecure`, for a
/// loopback or private host only.
#[derive(Debug)]
struct Unverified(Arc<CryptoProvider>);

impl ServerCertVerifier for Unverified {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn unverified_tls() -> Result<ClientConfig, String> {
    let provider = provider();
    Ok(ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Unverified(provider)))
        .with_no_client_auth())
}

/// The real dialler: TCP, TLS for `wss`, then the WebSocket handshake.
pub struct TcpDialer {
    target: Target,
    tls: Option<Arc<ClientConfig>>,
    loopback_only: bool,
}

impl TcpDialer {
    /// A dialler for `target`. `insecure` is honoured for a private host only (logged either
    /// way); `loopback_only` is the development guard.
    ///
    /// # Errors
    /// The guard refuses the host, or TLS cannot be set up.
    pub fn new(
        target: Target,
        ca_file: Option<&Path>,
        insecure: bool,
        loopback_only: bool,
        log: &Log,
    ) -> Result<TcpDialer, String> {
        if loopback_only {
            guard_loopback(&target)?;
        }
        let tls = if !target.tls {
            None
        } else if insecure && is_private_host(&target.host) {
            log.warn(format!(
                "TLS verification is OFF for {} (--insecure): use this only for a test server",
                target.host
            ));
            Some(Arc::new(unverified_tls()?))
        } else {
            if insecure {
                log.warn(format!(
                    "--insecure is ignored for {}, which is not on this machine or a private \
                     network: its certificate is verified (use --ca-file for a self-signed one)",
                    target.host
                ));
            }
            Some(Arc::new(verified_tls(ca_file)?))
        };
        Ok(TcpDialer { target, tls, loopback_only })
    }

    async fn connect(&self) -> Result<Ws, String> {
        let t = &self.target;
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((t.host.as_str(), t.port))
            .await
            .map_err(|e| format!("{}: {e}", t.host))?
            .collect();
        if addrs.is_empty() {
            return Err(format!("{}: no address", t.host));
        }
        if self.loopback_only
            && let Some(a) = addrs.iter().find(|a| !is_loopback(a.ip()))
        {
            return Err(format!(
                "this is a development or test build: {} resolves to {}, which is not a \
                 loopback address",
                t.host,
                a.ip()
            ));
        }
        let mut last = String::new();
        let mut tcp = None;
        for a in &addrs {
            match tokio::net::TcpStream::connect(a).await {
                Ok(s) => {
                    tcp = Some(s);
                    break;
                }
                Err(e) => last = e.to_string(),
            }
        }
        let tcp = tcp.ok_or_else(|| format!("{}:{}: {last}", t.host, t.port))?;
        let _nodelay = tcp.set_nodelay(true);
        let io: Box<dyn Io> = match &self.tls {
            None => Box::new(tcp),
            Some(cfg) => {
                let name = ServerName::try_from(t.host.clone())
                    .map_err(|e| format!("{}: not a TLS server name ({e})", t.host))?;
                let stream = tokio_rustls::TlsConnector::from(cfg.clone())
                    .connect(name, tcp)
                    .await
                    .map_err(|e| format!("TLS with {} failed: {e}", t.host))?;
                Box::new(stream)
            }
        };
        let (ws, _resp) =
            tokio_tungstenite::client_async_with_config(t.url.as_str(), io, Some(ws_config()))
                .await
                .map_err(|e| format!("the WebSocket handshake with {} failed: {e}", t.url))?;
        Ok(ws)
    }
}

impl Dialer for TcpDialer {
    fn dial(&self) -> BoxFuture<'_, Result<Ws, String>> {
        Box::pin(async move {
            tokio::time::timeout(DIAL_TIMEOUT, self.connect())
                .await
                .unwrap_or_else(|_| Err(format!("{} did not answer within 30 s", self.target.url)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses_become_websocket_urls_as_protocol_one_made_them() {
        let t = target("https://citar.example.com/").expect("reads");
        assert_eq!(
            (t.url.as_str(), t.host.as_str(), t.port, t.tls),
            ("wss://citar.example.com/ws/worker", "citar.example.com", 443, true)
        );
        let t = target("http://127.0.0.1:8000").expect("reads");
        assert_eq!((t.url.as_str(), t.port, t.tls), ("ws://127.0.0.1:8000/ws/worker", 8000, false));
        let t = target("https://example.com/citar").expect("reads");
        assert_eq!(t.url, "wss://example.com/citar/ws/worker");
        let t = target("http://[::1]:9/").expect("reads");
        assert_eq!((t.host.as_str(), t.port), ("::1", 9));
        assert!(target("citar.example.com").is_err());
        assert!(target("ftp://x").is_err());
    }

    #[test]
    fn the_guard_takes_loopback_only_and_insecure_private_hosts_only() {
        for ok in ["http://localhost:1", "http://127.0.0.1:1", "http://127.9.9.9", "http://[::1]:5"]
        {
            assert!(guard_loopback(&target(ok).expect("reads")).is_ok(), "{ok}");
        }
        for bad in ["https://example.com", "http://10.0.0.1", "http://localhost.example.com"] {
            assert!(guard_loopback(&target(bad).expect("reads")).is_err(), "{bad}");
        }
        for private in [
            "localhost",
            "10.1.2.3",
            "192.168.0.9",
            "172.20.0.1",
            "fd00::1",
            "nas.local",
            "169.254.1.1",
        ] {
            assert!(is_private_host(private), "{private}");
        }
        for public in ["example.com", "8.8.8.8", "2001:db8::1", "172.32.0.1"] {
            assert!(!is_private_host(public), "{public}");
        }
    }

    #[test]
    fn tls_builds_with_the_platform_verifier_and_refuses_a_bad_ca_file() {
        assert!(verified_tls(None).is_ok());
        let dir = std::env::temp_dir().join(format!("citar-helper-net-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let empty = dir.join("empty.pem");
        std::fs::write(&empty, "nothing here\n").expect("written");
        assert!(verified_tls(Some(&empty)).expect_err("no certificate").contains("no certificate"));
        assert!(verified_tls(Some(&dir.join("absent.pem"))).is_err());
        std::fs::remove_dir_all(&dir).expect("cleaned");
    }
}
