//! Dialling the server (design/phase4-helper.md P4.3.3): `wss://<server>/ws/worker` (`ws://` for
//! a plain-HTTP server), over a TCP connection and TLS the helper makes itself, so that:
//! - TLS is rustls with the OS's own trust store (`rustls-platform-verifier`), plus `--ca-file`'s
//!   certificates for a self-signed server, and a TLS failure is logged with the host and the
//!   verifier's reason, never relaxed;
//! - `--insecure` (protocol 1's) is honoured only for loopback and private hosts (RFC 1918, ULA,
//!   link-local, `.local`), with a warning, and ignored with a warning for any other;
//! - a development or test build (`dev-guard`, or any test feature) dials loopback addresses
//!   only, checked on the URL before any lookup and again on every address the name resolves to
//!   (P4.3.1);
//! - a name's addresses are raced as RFC 8305 ("Happy Eyeballs") says ([`Connector`]), for the
//!   server here and for the model endpoint's completions ([`crate::http1`]): an address that
//!   refuses slowly or never answers does not hold up the next. On Windows `localhost` resolves to
//!   `::1` first, and a refused connection there takes about 2 s (the system tries the SYN again),
//!   while LM Studio listens on `127.0.0.1` only: one address after another, every completion to
//!   the default `http://localhost:1234/v1` paid those 2 s (4-04's review).
//!
//! [`Dialer`] is what the session dials through; the transcripts give it an in-memory one.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::FuturesUnordered;
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

/// RFC 8305's "Connection Attempt Delay": how long one attempt runs alone before the next address
/// is tried beside it.
pub const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

/// The order a name's addresses are tried in (RFC 8305, section 4): duplicates dropped, the
/// families alternating from the family of the resolver's first choice (which put them in the
/// system's order of preference), and `first`, the address that answered last time, before all.
#[must_use]
pub fn attempt_order(addrs: &[SocketAddr], first: Option<SocketAddr>) -> Vec<SocketAddr> {
    let mut unique: Vec<SocketAddr> = Vec::with_capacity(addrs.len());
    for a in addrs {
        if !unique.contains(a) {
            unique.push(*a);
        }
    }
    let Some(lead) = unique.first().map(SocketAddr::is_ipv6) else { return unique };
    let (mut same, mut other): (Vec<SocketAddr>, Vec<SocketAddr>) =
        unique.into_iter().partition(|a| a.is_ipv6() == lead);
    let mut order = Vec::with_capacity(same.len() + other.len());
    same.reverse();
    other.reverse();
    loop {
        match (same.pop(), other.pop()) {
            (None, None) => break,
            (a, b) => order.extend(a.into_iter().chain(b)),
        }
    }
    if let Some(f) = first
        && let Some(at) = order.iter().position(|a| *a == f)
    {
        order.remove(at);
        order.insert(0, f);
    }
    order
}

/// Races connection attempts over `order` (RFC 8305, section 5): the first starts at once, and
/// each next one `delay` after the one before it or as soon as an attempt fails, whichever comes
/// first; the first to connect wins and the others are dropped (their sockets closed). The caller
/// bounds the whole race.
///
/// # Errors
/// Every attempt failed: the last failure, with its address.
pub async fn race<T, F, Fut>(
    order: &[SocketAddr],
    delay: Duration,
    mut connect: F,
) -> Result<(SocketAddr, T), String>
where
    F: FnMut(SocketAddr) -> Fut,
    Fut: Future<Output = std::io::Result<T>>,
{
    let mut attempts = FuturesUnordered::new();
    let mut next = 0;
    let mut due = tokio::time::Instant::now();
    let mut last_error: Option<String> = None;
    loop {
        if next < order.len() && (attempts.is_empty() || tokio::time::Instant::now() >= due) {
            let a = order[next];
            next += 1;
            let attempt = connect(a);
            attempts.push(async move { (a, attempt.await) });
            due = tokio::time::Instant::now() + delay;
            continue;
        }
        if attempts.is_empty() {
            return Err(last_error.unwrap_or_else(|| "no address to connect to".to_owned()));
        }
        let more = next < order.len();
        tokio::select! {
            Some((a, r)) = attempts.next() => match r {
                Ok(s) => return Ok((a, s)),
                Err(e) => {
                    last_error = Some(format!("{a}: {e}"));
                    // A failed attempt starts the next one at once.
                    due = tokio::time::Instant::now();
                }
            },
            () = tokio::time::sleep_until(due), if more => {}
        }
    }
}

/// TCP connections to one host, its addresses raced ([`race`]), remembering the address that
/// answered: the next connection tries it first, so a host whose first address never answers
/// costs [`ATTEMPT_DELAY`] once, not on every connection.
#[derive(Debug, Default)]
pub struct Connector {
    last: Mutex<Option<SocketAddr>>,
}

impl Connector {
    fn last(&self) -> std::sync::MutexGuard<'_, Option<SocketAddr>> {
        self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A connection to one of `addrs`, `host`'s addresses in the resolver's order.
    ///
    /// # Errors
    /// None of them could be reached: the last failure, with its address (after the host's
    /// name, when it has one).
    pub async fn connect(
        &self,
        host: &str,
        addrs: &[SocketAddr],
    ) -> Result<tokio::net::TcpStream, String> {
        let first = *self.last();
        let order = attempt_order(addrs, first);
        let named =
            |e: String| if host.parse::<IpAddr>().is_ok() { e } else { format!("{host}: {e}") };
        let (a, tcp) =
            race(&order, ATTEMPT_DELAY, tokio::net::TcpStream::connect).await.map_err(named)?;
        *self.last() = Some(a);
        Ok(tcp)
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
    connector: Connector,
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
        Ok(TcpDialer { target, tls, loopback_only, connector: Connector::default() })
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
        let tcp = self.connector.connect(&t.host, &addrs).await?;
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

    fn sa(s: &str) -> SocketAddr {
        s.parse().expect("an address")
    }

    #[test]
    fn addresses_are_tried_families_alternating_and_the_last_good_one_first() {
        let (v6a, v6b, v4a, v4b) =
            (sa("[::1]:9"), sa("[fe80::1]:9"), sa("127.0.0.1:9"), sa("10.0.0.1:9"));
        assert_eq!(attempt_order(&[v6a, v6b, v4a, v4b], None), [v6a, v4a, v6b, v4b]);
        assert_eq!(attempt_order(&[v4a, v6a, v4b], None), [v4a, v6a, v4b]);
        assert_eq!(attempt_order(&[v6a, v4a, v6a, v4a], None), [v6a, v4a], "duplicates once");
        assert_eq!(attempt_order(&[v6a, v6b, v4a, v4b], Some(v4b)), [v4b, v6a, v4a, v6b]);
        assert_eq!(attempt_order(&[v6a, v4a], Some(v4b)), [v6a, v4a], "a remembered address gone");
        assert!(attempt_order(&[], Some(v4a)).is_empty());
    }

    /// How a fake attempt ends: after this long, connected or refused; or never.
    #[derive(Clone, Copy)]
    enum Fake {
        Connects(u64),
        Refused(u64),
        Hangs,
    }

    /// Races `plan` with fake attempts on the paused clock: the winner, when the race ended (ms),
    /// and when each attempt started (ms).
    async fn fake_race(
        plan: &[(SocketAddr, Fake)],
    ) -> (Result<SocketAddr, String>, u64, Vec<(SocketAddr, u64)>) {
        let t0 = tokio::time::Instant::now();
        let started = Arc::new(Mutex::new(Vec::new()));
        let order: Vec<SocketAddr> = plan.iter().map(|(a, _)| *a).collect();
        let log = started.clone();
        let out = race(&order, ATTEMPT_DELAY, |a| {
            let how = plan.iter().find(|(p, _)| *p == a).map(|(_, f)| *f).expect("planned");
            let ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
            log.lock().expect("log").push((a, ms));
            async move {
                match how {
                    Fake::Connects(d) => {
                        tokio::time::sleep(Duration::from_millis(d)).await;
                        Ok(a)
                    }
                    Fake::Refused(d) => {
                        tokio::time::sleep(Duration::from_millis(d)).await;
                        Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
                    }
                    Fake::Hangs => std::future::pending().await,
                }
            }
        })
        .await
        .map(|(a, _)| a);
        let ended = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
        let started = started.lock().expect("log").clone();
        (out, ended, started)
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_or_silent_first_address_does_not_hold_up_the_next() {
        let (v6, v4, v4b) = (sa("[::1]:1234"), sa("127.0.0.1:1234"), sa("127.0.0.2:1234"));
        // Windows' `localhost`: ::1 refused after about 2 s, 127.0.0.1 listening.
        let (won, at, started) =
            fake_race(&[(v6, Fake::Refused(2040)), (v4, Fake::Connects(0))]).await;
        assert_eq!((won, at), (Ok(v4), 250), "{started:?}");
        // An address that never answers (a black-holed AAAA record) costs the same 250 ms.
        let (won, at, _) = fake_race(&[(v6, Fake::Hangs), (v4, Fake::Connects(3))]).await;
        assert_eq!((won, at), (Ok(v4), 253));
        // A quick refusal starts the next attempt at once.
        let (won, at, started) =
            fake_race(&[(v6, Fake::Refused(1)), (v4, Fake::Connects(0))]).await;
        assert_eq!((won, at), (Ok(v4), 1));
        assert_eq!(started, [(v6, 0), (v4, 1)]);
        // A first address that answers within the delay is the only one tried.
        let (won, at, started) =
            fake_race(&[(v6, Fake::Connects(100)), (v4, Fake::Connects(0))]).await;
        assert_eq!((won, at, started.len()), (Ok(v6), 100, 1));
        // Two silent addresses: the third starts at 500 ms.
        let (won, at, started) =
            fake_race(&[(v6, Fake::Hangs), (v4b, Fake::Hangs), (v4, Fake::Connects(0))]).await;
        assert_eq!((won, at), (Ok(v4), 500));
        assert_eq!(started, [(v6, 0), (v4b, 250), (v4, 500)]);
        // Every one refused: the last refusal, named by its address, once all have ended.
        let (won, at, _) = fake_race(&[(v6, Fake::Refused(2000)), (v4, Fake::Refused(10))]).await;
        let e = won.expect_err("nothing answered");
        assert!(e.starts_with("[::1]:1234: "), "{e}");
        assert_eq!(at, 2000);
        assert!(race(&[], ATTEMPT_DELAY, |a| async move { Ok(a) }).await.is_err());
    }

    /// The real thing: `localhost` with only its IPv4 address listening (LM Studio's way), on
    /// whatever order this machine's resolver gives.
    #[tokio::test]
    async fn localhost_connects_at_once_when_only_ipv4_listens() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let port = listener.local_addr().expect("its address").port();
        let accepting = tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                drop(s);
            }
        });
        let addrs: Vec<SocketAddr> =
            tokio::net::lookup_host(("localhost", port)).await.expect("localhost").collect();
        let c = Connector::default();
        let t0 = std::time::Instant::now();
        c.connect("localhost", &addrs).await.expect("connected");
        let first = t0.elapsed();
        // Within the delay and some slack, not the 2 s a refused ::1 takes on Windows.
        assert!(first < Duration::from_millis(1000), "{first:?} for {addrs:?}");
        assert_eq!(*c.last(), Some(SocketAddr::from(([127, 0, 0, 1], port))));
        // The address that answered goes first from now on.
        let t0 = std::time::Instant::now();
        c.connect("localhost", &addrs).await.expect("connected");
        assert!(t0.elapsed() < Duration::from_millis(200), "{:?}", t0.elapsed());
        // A refusal names the host and the address that refused.
        let closed = SocketAddr::from(([127, 0, 0, 1], 9));
        let e = c.connect("localhost", &[closed]).await.expect_err("nothing listens on 9");
        assert!(e.starts_with("localhost: 127.0.0.1:9: "), "{e}");
        let e = c.connect("127.0.0.1", &[closed]).await.expect_err("nothing listens on 9");
        assert!(e.starts_with("127.0.0.1:9: "), "{e}");
        accepting.abort();
    }
}
