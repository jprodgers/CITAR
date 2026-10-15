//! One HTTP/1.1 exchange with the model endpoint on a connection its caller owns
//! (design/phase4-helper.md P4.3.4, "Real cancel"). A completion's connection is driven by the
//! completion itself while it reads the response: nothing of it runs in a task of its own. So
//! dropping the [`Response`] (or the future still waiting for its head) closes the socket in that
//! very drop, and the endpoint sees its client go: LM Studio, Ollama, llama.cpp and vLLM stop a
//! streaming generation then. The order P4.2.4 asks for (the connection closed, and only then
//! `cancelled`) therefore holds by construction, not by how soon another task gets to run. (A
//! pooled client such as reqwest's closes it too, from its connection's own task, a fraction of a
//! millisecond later on this laptop: measured in 4-04, the as-built note in
//! design/phase4-helper.md.)
//!
//! **Keep-alive** ([`Pool`]): a connection whose answer was read to its end, with the endpoint
//! keeping it open, waits idle for the next completion, at most [`IDLE_KEEP`] and [`MAX_IDLE`] of
//! them, so a remote endpoint's next completion does not pay a new TCP connection and TLS
//! handshake (two round trips; 4-04's review). Only a finished answer's connection is kept: a
//! cancelled, timed-out or failed completion's is closed in its drop, never kept, reused or waited
//! on. A kept connection is checked before it is used (a close the endpoint sent while it waited
//! ends it there), and a request that fails on one within [`REUSE_RETRY`] goes once more on a new
//! connection: the endpoint closed it as the request came, so it never saw the request (a
//! completion changes nothing at the endpoint, so even if it had, a second costs only compute).
//!
//! Like the listings ([`crate::llm`]), it dials only the configured address: no system proxy, no
//! redirect followed (a 3xx is a status like any other), and a connect timeout of its own, the
//! host's addresses raced ([`crate::net::Connector`]); the completion's own `timeout_s` is the
//! caller's.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt as _;
use hyper::body::{Body as _, Bytes, Incoming};
use hyper::client::conn::http1::SendRequest;
use hyper::header;
use hyper_util::rt::TokioIo;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::time::Instant;

use crate::net::{Connector, Io};

/// How long a connection waits idle for the next completion: under the idle timeouts of NAT
/// boxes and firewalls (a minute and more), which drop a connection without a word; an endpoint
/// that closes sooner says so, and its close is seen before the connection is used.
pub const IDLE_KEEP: Duration = Duration::from_secs(30);
/// The most connections kept idle.
pub const MAX_IDLE: usize = 8;
/// How soon a request on a kept connection may fail and still go again on a new one.
pub const REUSE_RETRY: Duration = Duration::from_secs(1);
/// How long the end of an answer's body is waited for after its last event (a chunked stream's
/// closing bytes after its `[DONE]`), apart from the answer, which has gone already.
const FINISH_WAIT: Duration = Duration::from_secs(1);
/// The most bytes read after an answer's last event for its body's end.
const FINISH_BYTES: usize = 64 * 1024;
/// How long a kept connection may take to show it is ready.
const READY_WAIT: Duration = Duration::from_millis(100);

/// Where the endpoint is: its base URL (`…/v1`) taken apart once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub tls: bool,
    /// The host as written (brackets dropped from an IPv6 literal).
    pub host: String,
    pub port: u16,
    /// `host[:port]` as the `Host` header carries it.
    pub authority: String,
    /// The base's path, without a trailing `/` (`/v1`, or `` for a base at the root).
    pub path: String,
}

impl Origin {
    /// The origin of a base URL such as `http://localhost:1234/v1`.
    ///
    /// # Errors
    /// Not an `http://` or `https://` URL with a host, or one with a query or a fragment.
    pub fn parse(base: &str) -> Result<Origin, String> {
        let uri: hyper::Uri =
            base.trim().parse().map_err(|e| format!("the endpoint address {base:?}: {e}"))?;
        let tls = match uri.scheme_str() {
            Some("https") => true,
            Some("http") => false,
            _ => return Err(format!("the endpoint address {base:?} must start with http(s)://")),
        };
        let authority = uri
            .authority()
            .ok_or_else(|| format!("the endpoint address {base:?} has no host"))?
            .as_str()
            .to_owned();
        if authority.contains('@') {
            return Err(format!("the endpoint address {base:?} carries a user name"));
        }
        let host = uri.host().unwrap_or_default().trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            return Err(format!("the endpoint address {base:?} has no host"));
        }
        if uri.query().is_some() {
            return Err(format!("the endpoint address {base:?} has a query"));
        }
        Ok(Origin {
            tls,
            host: host.to_owned(),
            port: uri.port_u16().unwrap_or(if tls { 443 } else { 80 }),
            authority,
            path: uri.path().trim_end_matches('/').to_owned(),
        })
    }
}

/// Why an exchange failed before its response's head came.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    /// Whether another try could succeed (the endpoint was not there, or the connection broke).
    pub retryable: bool,
}

impl Failure {
    fn new(message: impl Into<String>, retryable: bool) -> Failure {
        Failure { message: message.into(), retryable }
    }
}

/// The connection's own future, driven by whoever reads the response.
type Driver = Pin<Box<dyn Future<Output = hyper::Result<()>> + Send>>;

/// A connection waiting for its next exchange.
struct Idle {
    sender: SendRequest<String>,
    conn: Driver,
    since: Instant,
}

impl Idle {
    /// The connection, if it is still open and ready for a request: driving it reads what came
    /// while it waited, and a close from the endpoint ends it here.
    async fn ready(mut self) -> Option<Idle> {
        if self.since.elapsed() > IDLE_KEEP || self.sender.is_closed() {
            return None;
        }
        let conn = &mut self.conn;
        let sender = &mut self.sender;
        let ready = tokio::time::timeout(READY_WAIT, async {
            tokio::select! {
                biased;
                _ = conn => false,
                r = sender.ready() => r.is_ok(),
            }
        })
        .await;
        matches!(ready, Ok(true)).then_some(self)
    }
}

/// One endpoint's connections: those waiting idle for the next completion, and the address that
/// answered last.
#[derive(Default)]
pub struct Pool {
    idle: Mutex<Vec<Idle>>,
    connector: Connector,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool").field("idle", &self.idle_count()).finish_non_exhaustive()
    }
}

impl Pool {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Idle>> {
        self.idle.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The connections waiting idle now.
    #[must_use]
    pub fn idle_count(&self) -> usize {
        self.lock().len()
    }

    /// Keeps a finished exchange's connection for the next one; the oldest goes past
    /// [`MAX_IDLE`].
    fn put(&self, idle: Idle) {
        let mut v = self.lock();
        v.retain(|i| i.since.elapsed() <= IDLE_KEEP);
        v.push(idle);
        if v.len() > MAX_IDLE {
            v.remove(0);
        }
    }

    /// The most recently kept connection that is still open and ready, if any.
    async fn take(&self) -> Option<Idle> {
        loop {
            let idle = self.lock().pop()?;
            if let Some(ready) = idle.ready().await {
                return Some(ready);
            }
        }
    }
}

/// A response whose head has come; its body is read with [`Response::chunk`]. Dropping it closes
/// the connection; [`Response::recycle`] keeps it for the next exchange once the answer is whole.
pub struct Response {
    /// When the request began to go out, on a connection already made: where `gen_s` and
    /// `ttft_s` start.
    pub sent: Instant,
    pub status: u16,
    /// The `content-type`, lower case, or empty.
    pub content_type: String,
    body: Incoming,
    conn: Driver,
    conn_done: bool,
    /// The body was read to its end.
    ended: bool,
    /// The endpoint closes the connection after this answer (`connection: close`).
    closing: bool,
    /// Kept while the response is read (hyper ends a connection whose sender is gone once its
    /// exchange is over), and for the next exchange.
    sender: SendRequest<String>,
    pool: Arc<Pool>,
}

impl Response {
    /// The next piece of the body; `None` at its end.
    ///
    /// # Errors
    /// The connection failed or the body was cut short.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, String> {
        loop {
            let done = self.conn_done;
            let body = &mut self.body;
            let conn = &mut self.conn;
            tokio::select! {
                biased;
                frame = std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)) => {
                    match frame {
                        None => {
                            self.ended = true;
                            return Ok(None);
                        }
                        Some(Err(e)) => return Err(format!("the response broke off: {e}")),
                        Some(Ok(f)) => {
                            if let Ok(data) = f.into_data()
                                && !data.is_empty()
                            {
                                return Ok(Some(data));
                            }
                        }
                    }
                }
                r = conn, if !done => {
                    // The connection's work is done once the response is in; the body may still
                    // hold buffered data, which the next round reads.
                    self.conn_done = true;
                    if let Err(e) = r {
                        return Err(format!("the connection failed: {e}"));
                    }
                }
            }
        }
    }

    /// The whole body, up to `max` bytes.
    ///
    /// # Errors
    /// The body is longer than `max`, or broke off.
    pub async fn read_all(&mut self, max: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        while let Some(c) = self.chunk().await? {
            if out.len() + c.len() > max {
                return Err(format!("the response is longer than {max} bytes"));
            }
            out.extend_from_slice(&c);
        }
        Ok(out)
    }

    /// The body's first `max` bytes at most (the rest is not read), or what came of it within
    /// `within`: for an error's body, which is only quoted.
    pub async fn read_start(&mut self, max: usize, within: Duration) -> Vec<u8> {
        let mut out = Vec::new();
        let read = async {
            while out.len() < max {
                match self.chunk().await {
                    Ok(Some(c)) => out.extend_from_slice(&c[..c.len().min(max - out.len())]),
                    Ok(None) | Err(_) => break,
                }
            }
        };
        let _in_time = tokio::time::timeout(within, read).await;
        out
    }

    /// Whether the connection may carry another exchange once the body has ended.
    fn reusable(&self) -> bool {
        !self.conn_done && !self.closing && !self.sender.is_closed()
    }

    /// Hands a finished answer's connection back for the next completion: at once when its body
    /// was read to the end, or once its last bytes come (at most [`FINISH_BYTES`] within
    /// [`FINISH_WAIT`], read by a task of their own: the answer has gone already). A connection
    /// that cannot carry another exchange is closed.
    pub fn recycle(mut self) {
        // The body's end usually came with its last event: what is here already is read first.
        for _ in 0..4 {
            if self.ended {
                break;
            }
            match self.chunk().now_or_never() {
                Some(Ok(Some(_))) => {}
                Some(Ok(None)) | None => break,
                Some(Err(_)) => return,
            }
        }
        if !self.reusable() {
            return;
        }
        if self.ended {
            self.keep();
            return;
        }
        tokio::spawn(async move {
            let mut read = 0;
            let finished = tokio::time::timeout(FINISH_WAIT, async {
                loop {
                    match self.chunk().await {
                        Ok(None) => return true,
                        Ok(Some(c)) => {
                            read += c.len();
                            if read > FINISH_BYTES {
                                return false;
                            }
                        }
                        Err(_) => return false,
                    }
                }
            })
            .await;
            if finished == Ok(true) && self.reusable() {
                self.keep();
            }
        });
    }

    fn keep(self) {
        let Response { sender, conn, pool, .. } = self;
        pool.put(Idle { sender, conn, since: Instant::now() });
    }
}

/// Connects to the origin.
async fn dial(
    origin: &Origin,
    tls: Option<&Arc<ClientConfig>>,
    connector: &Connector,
) -> Result<Box<dyn Io>, Failure> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((origin.host.as_str(), origin.port))
        .await
        .map_err(|e| Failure::new(format!("{}: {e}", origin.host), true))?
        .collect();
    let tcp = connector.connect(&origin.host, &addrs).await.map_err(|e| Failure::new(e, true))?;
    let _nodelay = tcp.set_nodelay(true);
    if !origin.tls {
        return Ok(Box::new(tcp));
    }
    let cfg = tls.ok_or_else(|| Failure::new("TLS is not set up for this endpoint", false))?;
    let name = ServerName::try_from(origin.host.clone()).map_err(|e| {
        Failure::new(format!("{}: not a TLS server name ({e})", origin.host), false)
    })?;
    let stream = tokio_rustls::TlsConnector::from(cfg.clone())
        .connect(name, tcp)
        .await
        // A certificate the OS does not trust stays refused, and another try will not change it.
        .map_err(|e| Failure::new(format!("TLS with {} failed: {e}", origin.host), false))?;
    Ok(Box::new(stream))
}

/// A response's head, its body and the connection that carries them.
struct Exchanged {
    sent: Instant,
    head: hyper::Response<Incoming>,
    sender: SendRequest<String>,
    conn: Driver,
    conn_done: bool,
}

/// Sends `req` on a connection and drives it until the response's head has come.
async fn exchange(
    mut sender: SendRequest<String>,
    mut conn: Driver,
    req: hyper::Request<String>,
) -> Result<Exchanged, Failure> {
    let sent = Instant::now();
    let send = sender.send_request(req);
    tokio::pin!(send);
    let mut conn_done = false;
    let head = tokio::select! {
        biased;
        r = &mut send => r,
        c = &mut conn => {
            // A small answer with `connection: close` can be read whole, its body handed over
            // and the connection finished, in the one poll that delivers its head: the response
            // is then waiting for `send`. A connection that failed has none.
            conn_done = true;
            if let Err(e) = c {
                return Err(Failure::new(format!("the connection failed: {e}"), true));
            }
            send.await
        }
    }
    .map_err(|e| Failure::new(format!("HTTP: {e}"), true))?;
    Ok(Exchanged { sent, head, sender, conn, conn_done })
}

/// One POST of `body` to `origin`'s `path` (after the base's own), its response's head awaited,
/// on a connection `pool` kept from a finished exchange or a new one (dialled within `connect`).
/// `headers` are added to `Host`, `content-type` and `content-length`.
///
/// # Errors
/// The endpoint could not be reached within `connect`, or the exchange failed before the head.
pub async fn post(
    origin: &Origin,
    tls: Option<&Arc<ClientConfig>>,
    pool: &Arc<Pool>,
    path: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
    connect: Duration,
) -> Result<Response, Failure> {
    let text =
        String::from_utf8(body).map_err(|e| Failure::new(format!("the body: {e}"), false))?;
    let request = |text: String| {
        let mut req = hyper::Request::post(format!("{}{path}", origin.path))
            .header(header::HOST, origin.authority.as_str())
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, text.len());
        for (k, v) in headers {
            req = req.header(*k, v.as_str());
        }
        req.body(text).map_err(|e| Failure::new(format!("the request: {e}"), false))
    };
    let mut answered = None;
    if let Some(idle) = pool.take().await {
        let sent = Instant::now();
        match exchange(idle.sender, idle.conn, request(text.clone())?).await {
            Ok(x) => answered = Some(x),
            // The endpoint closed the kept connection as the request came: a new one.
            Err(_) if sent.elapsed() < REUSE_RETRY => {}
            Err(f) => return Err(f),
        }
    }
    let x = match answered {
        Some(x) => x,
        None => {
            let io = tokio::time::timeout(connect, dial(origin, tls, &pool.connector))
                .await
                .map_err(|_| {
                    Failure::new(
                        format!(
                            "{}:{} did not answer within {} s",
                            origin.host,
                            origin.port,
                            connect.as_secs()
                        ),
                        true,
                    )
                })??;
            let (sender, conn) =
                hyper::client::conn::http1::handshake::<_, String>(TokioIo::new(io))
                    .await
                    .map_err(|e| Failure::new(format!("HTTP: {e}"), true))?;
            exchange(sender, Box::pin(conn), request(text)?).await?
        }
    };
    let Exchanged { sent, head, sender, conn, conn_done } = x;
    let status = head.status().as_u16();
    let content_type = head
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let closing = head.headers().get_all(header::CONNECTION).iter().any(|v| {
        v.to_str().is_ok_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("close")))
    }) || head.version() < hyper::Version::HTTP_11;
    Ok(Response {
        sent,
        status,
        content_type,
        body: head.into_body(),
        conn,
        conn_done,
        ended: false,
        closing,
        sender,
        pool: pool.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_read_as_the_listings_dial_them() {
        let o = Origin::parse("http://localhost:1234/v1").expect("reads");
        assert_eq!(
            (o.tls, o.host.as_str(), o.port, o.authority.as_str(), o.path.as_str()),
            (false, "localhost", 1234, "localhost:1234", "/v1")
        );
        let o = Origin::parse("https://api.openai.com/v1/").expect("reads");
        assert_eq!((o.tls, o.port, o.path.as_str()), (true, 443, "/v1"));
        let o = Origin::parse("http://[::1]:11434/v1").expect("reads");
        assert_eq!((o.host.as_str(), o.authority.as_str()), ("::1", "[::1]:11434"));
        let o = Origin::parse("http://10.0.0.5").expect("reads");
        assert_eq!((o.port, o.path.as_str()), (80, ""));
        for bad in
            ["localhost:1234/v1", "ftp://x/v1", "http:///v1", "http://u:p@h/v1", "http://h/v1?x=1"]
        {
            assert!(Origin::parse(bad).is_err(), "{bad}");
        }
    }

    /// A connection with an endpoint that has said nothing yet: enough for the pool's rules.
    async fn idle_pair() -> (Idle, tokio::io::DuplexStream) {
        let (ours, theirs) = tokio::io::duplex(1024);
        let (sender, conn) = hyper::client::conn::http1::handshake::<_, String>(TokioIo::new(ours))
            .await
            .expect("a handshake");
        (Idle { sender, conn: Box::pin(conn), since: Instant::now() }, theirs)
    }

    #[tokio::test(start_paused = true)]
    async fn the_pool_keeps_a_few_recent_open_connections() {
        let pool = Pool::default();
        let (idle, _peer) = idle_pair().await;
        pool.put(idle);
        assert!(pool.take().await.is_some(), "an open connection, just kept");
        assert_eq!(pool.idle_count(), 0);
        // Past IDLE_KEEP it is not used: a box between may have forgotten it.
        let (idle, _peer) = idle_pair().await;
        pool.put(idle);
        tokio::time::advance(IDLE_KEEP + Duration::from_secs(1)).await;
        assert!(pool.take().await.is_none());
        // One the endpoint closed while it waited is not either.
        let (idle, peer) = idle_pair().await;
        pool.put(idle);
        drop(peer);
        assert!(pool.take().await.is_none());
        assert_eq!(pool.idle_count(), 0);
        // At most MAX_IDLE wait.
        let mut peers = Vec::new();
        for _ in 0..MAX_IDLE + 3 {
            let (idle, peer) = idle_pair().await;
            pool.put(idle);
            peers.push(peer);
        }
        assert_eq!(pool.idle_count(), MAX_IDLE);
    }
}
