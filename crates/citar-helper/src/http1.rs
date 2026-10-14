//! One HTTP/1.1 exchange with the model endpoint on a connection its caller owns
//! (design/phase4-helper.md P4.3.4, "Real cancel"). A completion dials its own TCP connection
//! (and TLS for an `https` endpoint), sends one request, and drives the connection itself while it
//! reads the response: nothing of it runs in a task of its own. So dropping the [`Response`] (or
//! the future still waiting for its head) drops the socket at that moment, and the endpoint sees
//! its client go: LM Studio, Ollama, llama.cpp and vLLM stop a streaming generation then. A pooled
//! client keeps each connection in a task of its own, which notices a dropped response only when
//! the endpoint next writes, so a generation that is thinking before its first token would run on.
//!
//! Like the listings ([`crate::llm`]), it dials only the configured address: no system proxy, no
//! redirect followed (a 3xx is a status like any other), and a connect timeout of its own; the
//! completion's own `timeout_s` is the caller's.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use hyper::body::{Body as _, Bytes, Incoming};
use hyper::header;
use hyper_util::rt::TokioIo;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;

use crate::net::Io;

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

/// A response whose head has come; its body is read with [`Response::chunk`]. Dropping it closes
/// the connection.
pub struct Response {
    pub status: u16,
    /// The `content-type`, lower case, or empty.
    pub content_type: String,
    body: Incoming,
    conn: Driver,
    conn_done: bool,
    /// Kept while the response is read: hyper ends a connection whose sender is gone once its
    /// exchange is over, and nothing here should depend on when it looks.
    _sender: hyper::client::conn::http1::SendRequest<String>,
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
                        None => return Ok(None),
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
}

/// Connects to the origin, within `connect`.
async fn dial(origin: &Origin, tls: Option<&Arc<ClientConfig>>) -> Result<Box<dyn Io>, Failure> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((origin.host.as_str(), origin.port))
        .await
        .map_err(|e| Failure::new(format!("{}: {e}", origin.host), true))?
        .collect();
    let mut last = format!("{}: no address", origin.host);
    let mut tcp = None;
    for a in &addrs {
        match tokio::net::TcpStream::connect(a).await {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(e) => last = format!("{}:{}: {e}", origin.host, origin.port),
        }
    }
    let tcp = tcp.ok_or_else(|| Failure::new(last, true))?;
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

/// One POST of `body` to `origin`'s `path` (after the base's own), its response's head awaited.
/// `headers` are added to `Host`, `content-type` and `content-length`.
///
/// # Errors
/// The endpoint could not be reached within `connect`, or the exchange failed before the head.
pub async fn post(
    origin: &Origin,
    tls: Option<&Arc<ClientConfig>>,
    path: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
    connect: Duration,
) -> Result<Response, Failure> {
    let io = tokio::time::timeout(connect, dial(origin, tls)).await.map_err(|_| {
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
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, String>(TokioIo::new(io))
        .await
        .map_err(|e| Failure::new(format!("HTTP: {e}"), true))?;
    let mut conn: Driver = Box::pin(conn);
    let len = body.len();
    let text =
        String::from_utf8(body).map_err(|e| Failure::new(format!("the body: {e}"), false))?;
    let mut req = hyper::Request::post(format!("{}{path}", origin.path))
        .header(header::HOST, origin.authority.as_str())
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, len);
    for (k, v) in headers {
        req = req.header(*k, v.as_str());
    }
    let req = req.body(text).map_err(|e| Failure::new(format!("the request: {e}"), false))?;
    let send = sender.send_request(req);
    tokio::pin!(send);
    let mut conn_done = false;
    let resp = tokio::select! {
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
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Ok(Response { status, content_type, body: resp.into_body(), conn, conn_done, _sender: sender })
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
}
