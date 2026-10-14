//! The LLM proxy (design/phase4-helper.md P4.3.4; PROTOCOL.md "Completions"): each `request` the
//! server sends becomes one chat completion on the machine's endpoint, run as a task of its own,
//! and its answer is returned **as the endpoint gave it**: nothing is parsed or repaired here
//! (the server parses tool calls, P4.5.2).
//!
//! - **The request:** the server's messages and tools as they came, its sampling parameters only
//!   from the five the protocol allows ([`SERVER_PARAMS`]; any other is dropped, and the session
//!   logs each name once), and the helper's own choice of streaming. Nothing in a request changes
//!   the endpoint, its key or streaming.
//! - **Streaming** (the default, `stream: true`, with `stream_options.include_usage`): the answer
//!   is assembled from its server-sent events: content and reasoning (`reasoning_content`, or
//!   `reasoning` where an endpoint names it so) concatenated, tool calls by `index` (the first id
//!   and type, the name and arguments concatenated), the last `finish_reason` and the last usage.
//!   A streaming request with tools that the endpoint refuses for streaming is tried once more
//!   without, and tool requests then go unstreamed for the rest of the connection ([`Quirks`]). An
//!   endpoint that answers JSON to a streaming request is read as unstreamed.
//! - **Real cancel:** the completion's connection belongs to its task ([`crate::http1`]), so
//!   aborting the task closes it there and then, and [`Flights`] reports `cancelled` only once the
//!   task is gone, with the tokens it had received ([`Progress`]).
//! - **Bounds**, so that a broken or odd endpoint makes the helper hold nothing unbounded and send
//!   nothing the hub refuses: at most [`MAX_ASSEMBLED`] bytes of text (a `done` must fit the
//!   protocol's 8 MiB frame), a server-sent line or event at most as long, at most
//!   [`MAX_TOOL_CALLS`] tool calls, and the fields of a call and the finish reason cut to the
//!   protocol's lengths; an error body is read to [`ERROR_BODY_READ`] bytes and quoted to
//!   [`crate::llm::BODY_SHOWN`] characters, the helper's secrets (its API key) replaced in it
//!   first: the key stays on the machine (P4.6, threat A; P4.3.4).
//!
//! Replaces `citar/worker/agent.py` `handle_request` and `_complete` (lines 245-341), which
//! reused the server's own provider and parsed on the helper.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::Instant;

use crate::llm::{BODY_SHOWN, SharedEndpoint};
use crate::log::{Level, Log, clip};
use crate::protocol::{
    Done, DoneMessage, FunctionCall, MAX_FRAME_BYTES, Request, RequestErrorCode, ToolCall, Usage,
};

/// The longest a completion may run, whatever its `timeout_s` says.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(900);
/// The sampling parameters a request may set; every other is dropped.
pub const SERVER_PARAMS: [&str; 5] =
    ["temperature", "max_tokens", "top_p", "seed", "reasoning_effort"];
/// The most bytes of text (content, reasoning, tool calls) one completion may assemble: more could
/// not be sent in one `done`.
pub const MAX_ASSEMBLED: usize = MAX_FRAME_BYTES;
/// The longest unstreamed answer read.
pub const MAX_ANSWER: usize = 2 * MAX_FRAME_BYTES;
/// The most tool calls a `done` carries (PROTOCOL.md's `[ToolCall] ≤512`).
pub const MAX_TOOL_CALLS: usize = 512;
/// The most of an error's body read (its first [`BODY_SHOWN`] characters are quoted).
pub const ERROR_BODY_READ: usize = 16 * 1024;
/// The largest integer the protocol's `int` takes.
const MAX_INT: u64 = (1 << 53) - 1;
/// The protocol's lengths for what a `done` carries from the endpoint.
const CALL_ID_CHARS: usize = 200;
const CALL_TYPE_CHARS: usize = 32;
const CALL_NAME_CHARS: usize = 200;
const FINISH_CHARS: usize = 64;
/// The most characters of an `error`'s message (PROTOCOL.md's `text`).
pub const MESSAGE_CHARS: usize = 4000;

/// How a completion ended, before the session tells the server.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Done(Done),
    Failed { code: RequestErrorCode, message: String, retryable: bool },
}

fn failed(code: RequestErrorCode, message: impl Into<String>, retryable: bool) -> Outcome {
    Outcome::Failed { code, message: cut(&message.into(), MESSAGE_CHARS), retryable }
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// What one completion has received so far, for its `cancelled`.
#[derive(Debug, Default)]
pub struct Progress {
    tokens: AtomicU64,
}

impl Progress {
    /// The tokens received: each streamed event that carried text counts one.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        self.tokens.load(Ordering::Relaxed)
    }

    fn token(&self) {
        self.tokens.fetch_add(1, Ordering::Relaxed);
    }
}

/// What the helper has learned of its endpoint on this connection to the server.
#[derive(Debug, Default)]
pub struct Quirks {
    /// The endpoint refused a streaming request with tools: tool requests go unstreamed.
    no_stream_with_tools: AtomicBool,
}

/// The completions running on the helper, counted by their tasks: one leaves the count when its
/// task is gone, whichever way it ended. Tests read it as the helper's `in_flight`.
#[derive(Clone, Debug, Default)]
pub struct InFlight(Arc<AtomicUsize>);

impl InFlight {
    /// The completions whose tasks are alive.
    #[must_use]
    pub fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    fn enter(&self) -> Entered {
        self.0.fetch_add(1, Ordering::SeqCst);
        Entered(self.0.clone())
    }
}

/// One completion's place in [`InFlight`], given back when its task's future is dropped.
struct Entered(Arc<AtomicUsize>);

impl Drop for Entered {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The JSON body of one completion: the server's model, messages and tools as they came, the
/// allowed parameters, and the helper's own streaming.
#[must_use]
pub fn body(req: &Request, stream: bool) -> Vec<u8> {
    let mut b = Map::new();
    b.insert("model".into(), Value::from(req.model.clone()));
    b.insert(
        "messages".into(),
        Value::Array(req.messages.iter().cloned().map(Value::Object).collect()),
    );
    // A plain completion (a report's narrative) sends no tools: endpoints refuse an empty list.
    if let Some(tools) = req.tools.as_ref().filter(|t| !t.is_empty()) {
        b.insert("tools".into(), Value::Array(tools.iter().cloned().map(Value::Object).collect()));
    }
    let p = &req.params;
    if let Some(t) = p.temperature {
        b.insert("temperature".into(), Value::from(t));
    }
    if let Some(m) = p.max_tokens {
        b.insert("max_tokens".into(), Value::from(m));
    }
    if let Some(t) = p.top_p {
        b.insert("top_p".into(), Value::from(t));
    }
    if let Some(s) = p.seed {
        b.insert("seed".into(), Value::from(s));
    }
    if let Some(r) = &p.reasoning_effort {
        b.insert("reasoning_effort".into(), Value::from(r.clone()));
    }
    b.insert("stream".into(), Value::Bool(stream));
    if stream {
        b.insert("stream_options".into(), serde_json::json!({"include_usage": true}));
    }
    serde_json::to_vec(&Value::Object(b)).unwrap_or_default()
}

/// One server-sent event of a streamed completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A `data` event's text.
    Data(String),
    /// `data: [DONE]`: the stream is over.
    Done,
    /// An error the endpoint reported in the stream (`event: error`, or an `error:` field).
    Error(String),
}

/// A server-sent events reader (the WHATWG rules the endpoints follow: `data:` lines joined by
/// newlines, a blank line ending an event, `:` a comment, `\r\n` or `\n`), bounded.
#[derive(Debug, Default)]
pub struct Sse {
    /// What came after the last newline.
    pending: Vec<u8>,
    data: String,
    has_data: bool,
    error_event: bool,
}

impl Sse {
    /// The events `bytes` completes.
    ///
    /// # Errors
    /// A line or an event longer than [`MAX_ASSEMBLED`], or text that is not UTF-8.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>, String> {
        // What is pending holds no newline (each line was taken as it ended): only the new bytes
        // are searched, so a long line sent in many small pieces costs its length, not its square.
        let mut search = self.pending.len();
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(off) = self.pending[search..].iter().position(|b| *b == b'\n') {
            let end = search + off;
            let mut line = &self.pending[start..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            let text = std::str::from_utf8(line)
                .map_err(|_| "the endpoint's stream is not UTF-8".to_owned())?
                .to_owned();
            self.line(&text, &mut out)?;
            start = end + 1;
            search = start;
        }
        self.pending.drain(..start);
        if self.pending.len() > MAX_ASSEMBLED {
            return Err(format!("the endpoint sent a line longer than {MAX_ASSEMBLED} bytes"));
        }
        Ok(out)
    }

    /// At the end of the stream: an event the endpoint did not close with a blank line.
    ///
    /// # Errors
    /// What [`Sse::feed`] refuses.
    pub fn finish(&mut self) -> Result<Vec<Event>, String> {
        let mut out = Vec::new();
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            let text = std::str::from_utf8(&rest)
                .map_err(|_| "the endpoint's stream is not UTF-8".to_owned())?
                .trim_end_matches('\r')
                .to_owned();
            self.line(&text, &mut out)?;
        }
        self.line("", &mut out)?;
        Ok(out)
    }

    fn line(&mut self, line: &str, out: &mut Vec<Event>) -> Result<(), String> {
        if line.is_empty() {
            if self.has_data {
                let data = std::mem::take(&mut self.data);
                out.push(if self.error_event {
                    Event::Error(data)
                } else if data == "[DONE]" {
                    Event::Done
                } else {
                    Event::Data(data)
                });
            }
            self.has_data = false;
            self.error_event = false;
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
                if self.data.len() > MAX_ASSEMBLED {
                    return Err(format!(
                        "the endpoint sent an event longer than {MAX_ASSEMBLED} bytes"
                    ));
                }
            }
            "event" => self.error_event = value == "error",
            // llama.cpp's server reports a failure in a stream as an `error:` line.
            "error" => out.push(Event::Error(value.to_owned())),
            _ => {}
        }
        Ok(())
    }
}

/// One tool call as its deltas built it.
#[derive(Clone, Debug, Default)]
struct Call {
    id: String,
    kind: String,
    name: String,
    arguments: String,
}

/// An answer being put together from the endpoint's deltas (or its one unstreamed message).
#[derive(Clone, Debug, Default)]
pub struct Assembly {
    content: Option<String>,
    reasoning: Option<String>,
    calls: BTreeMap<u64, Call>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    /// Bytes of text assembled.
    bytes: usize,
    /// Tool calls left out past [`MAX_TOOL_CALLS`].
    calls_left_out: usize,
}

/// A text value as the endpoint gave it: a string, or any other JSON written out (some
/// endpoints give a call's arguments as an object).
fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// The error an endpoint put in its body or stream, whole (its caller quotes it).
fn error_text(e: &Value) -> String {
    match e {
        Value::String(s) => s.clone(),
        Value::Object(m) => {
            m.get("message").and_then(Value::as_str).map_or_else(|| e.to_string(), str::to_owned)
        }
        other => other.to_string(),
    }
}

/// The endpoint's own words as the server may read them: the helper's secrets (its API key)
/// replaced before they are cut, so no part of one can show, and cut to [`BODY_SHOWN`] characters
/// after `prefix` (a few words of the helper's own).
fn quote(log: &Log, prefix: &str, text: &str) -> String {
    format!("{prefix}{}", clip(&log.redact(text), BODY_SHOWN))
}

/// The endpoint's token counts, when it gave both the prompt's and the completion's.
fn usage_of(u: &Value) -> Option<Usage> {
    let int = |v: Option<&Value>| v.and_then(Value::as_u64).filter(|n| *n <= MAX_INT);
    Some(Usage {
        prompt_tokens: int(u.get("prompt_tokens"))?,
        completion_tokens: int(u.get("completion_tokens"))?,
        reasoning_tokens: int(u.pointer("/completion_tokens_details/reasoning_tokens")),
        cached_tokens: int(u.pointer("/prompt_tokens_details/cached_tokens")),
    })
}

/// The choice an answer is read from: index 0 (a choice without an index is the one there is).
/// The helper asks for one; another choice an endpoint adds is not its answer.
fn first_choice(v: &Value) -> Option<&Value> {
    v.get("choices")?
        .as_array()?
        .iter()
        .find(|c| c.get("index").map_or(Some(0), Value::as_u64) == Some(0))
}

impl Assembly {
    fn add_text(slot: &mut Option<String>, bytes: &mut usize, s: &str) -> bool {
        slot.get_or_insert_with(String::new).push_str(s);
        *bytes += s.len();
        !s.is_empty()
    }

    /// One streamed chunk; true when it carried text (a token).
    ///
    /// # Errors
    /// The chunk is the endpoint's report of an error: its message, whole.
    pub fn chunk(&mut self, v: &Value) -> Result<bool, String> {
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            return Err(error_text(e));
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage = usage_of(u);
        }
        let Some(choice) = first_choice(v) else { return Ok(false) };
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(f.to_owned());
        }
        let Some(delta) = choice.get("delta").and_then(Value::as_object) else { return Ok(false) };
        let mut token = false;
        if let Some(c) = delta.get("content").and_then(Value::as_str) {
            token |= Self::add_text(&mut self.content, &mut self.bytes, c);
        }
        let reasoning = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .or_else(|| delta.get("reasoning").and_then(Value::as_str));
        if let Some(r) = reasoning {
            token |= Self::add_text(&mut self.reasoning, &mut self.bytes, r);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for c in calls {
                token |= self.call_delta(c, None);
            }
        }
        Ok(token)
    }

    /// One tool call's delta: by its `index`; one without starts a new call when it has an id
    /// (or is the first), and adds to the last call otherwise. `at` is the index an unstreamed
    /// message's position gives.
    fn call_delta(&mut self, c: &Value, at: Option<u64>) -> bool {
        let given = c.get("index").and_then(Value::as_u64).or(at);
        let last = self.calls.keys().next_back().copied();
        let index = match (given, last) {
            (Some(i), _) => i,
            (None, None) => 0,
            (None, Some(l)) => {
                let has_id = c.get("id").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
                if has_id { l.saturating_add(1) } else { l }
            }
        };
        if !self.calls.contains_key(&index) && self.calls.len() >= MAX_TOOL_CALLS {
            self.calls_left_out += 1;
            return false;
        }
        let call = self.calls.entry(index).or_default();
        if call.id.is_empty()
            && let Some(id) = c.get("id").and_then(Value::as_str)
        {
            call.id = id.to_owned();
        }
        if call.kind.is_empty()
            && let Some(k) = c.get("type").and_then(Value::as_str)
        {
            call.kind = k.to_owned();
        }
        let mut token = false;
        if let Some(f) = c.get("function") {
            if let Some(n) = f.get("name").and_then(text_of) {
                self.bytes += n.len();
                token |= !n.is_empty();
                call.name.push_str(&n);
            }
            if let Some(a) = f.get("arguments").and_then(text_of) {
                self.bytes += a.len();
                token |= !a.is_empty();
                call.arguments.push_str(&a);
            }
        }
        token
    }

    /// An unstreamed answer's body.
    ///
    /// # Errors
    /// The body reports an error (`reported an error: ` and its message, whole), or has no
    /// choice.
    pub fn message(&mut self, v: &Value) -> Result<(), String> {
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            return Err(format!("reported an error: {}", error_text(e)));
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage = usage_of(u);
        }
        let choice = first_choice(v).ok_or("has no choice")?;
        self.finish_reason = choice.get("finish_reason").and_then(Value::as_str).map(str::to_owned);
        let empty = Map::new();
        let m = choice.get("message").and_then(Value::as_object).unwrap_or(&empty);
        if let Some(c) = m.get("content").and_then(Value::as_str) {
            Self::add_text(&mut self.content, &mut self.bytes, c);
        }
        let reasoning = m
            .get("reasoning_content")
            .and_then(Value::as_str)
            .or_else(|| m.get("reasoning").and_then(Value::as_str));
        if let Some(r) = reasoning {
            Self::add_text(&mut self.reasoning, &mut self.bytes, r);
        }
        if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
            for (i, c) in (0u64..).zip(calls) {
                self.call_delta(c, Some(i));
            }
        }
        Ok(())
    }

    /// Bytes of text assembled so far.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The answer as a `done`'s message, finish reason and usage, each value cut to the
    /// protocol's length (logged once when anything is).
    #[must_use]
    pub fn into_parts(self, log: &Log) -> (DoneMessage, Option<String>, Option<Usage>) {
        let mut cuts = Vec::new();
        if self.calls_left_out > 0 {
            cuts.push(format!(
                "{} tool calls past the protocol's {MAX_TOOL_CALLS} left out",
                self.calls_left_out
            ));
        }
        let mut fit = |s: String, n: usize, what: &str| {
            if s.chars().count() > n {
                cuts.push(format!("a tool call's {what} cut to {n} characters"));
                cut(&s, n)
            } else {
                s
            }
        };
        let tool_calls: Vec<ToolCall> = self
            .calls
            .into_values()
            .map(|c| ToolCall {
                id: fit(c.id, CALL_ID_CHARS, "id"),
                // OpenAI's chat API has one kind of tool call; an endpoint that leaves it out
                // means that one.
                kind: if c.kind.is_empty() {
                    "function".to_owned()
                } else {
                    fit(c.kind, CALL_TYPE_CHARS, "type")
                },
                function: FunctionCall {
                    name: fit(c.name, CALL_NAME_CHARS, "name"),
                    arguments: c.arguments,
                },
            })
            .collect();
        let finish = self.finish_reason.map(|f| {
            if f.chars().count() > FINISH_CHARS {
                cuts.push(format!("the finish reason cut to {FINISH_CHARS} characters"));
                cut(&f, FINISH_CHARS)
            } else {
                f
            }
        });
        for c in cuts {
            log.once(&format!("cut: {c}"), Level::Warn, format!("the endpoint's answer: {c}"));
        }
        (
            DoneMessage { content: self.content, reasoning: self.reasoning, tool_calls },
            finish,
            self.usage,
        )
    }
}

fn seconds(d: Duration) -> f64 {
    // Microseconds: a frame's figures need no more.
    (d.as_secs_f64() * 1e6).round() / 1e6
}

/// Whether an HTTP status says another try could succeed.
fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429) || status >= 500
}

/// Whether an endpoint refused a streaming request because it had tools.
fn refuses_streaming_tools(status: u16, body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    matches!(status, 400 | 422 | 500 | 501)
        && lower.contains("stream")
        && (lower.contains("tool") || lower.contains("function"))
}

/// One completion on `ep`, with no time limit of its own (the caller's).
async fn complete(
    ep: SharedEndpoint,
    req: Request,
    prefer_stream: bool,
    quirks: Arc<Quirks>,
    progress: Arc<Progress>,
    log: Log,
) -> Outcome {
    let has_tools = req.tools.as_ref().is_some_and(|t| !t.is_empty());
    let mut stream =
        prefer_stream && !(has_tools && quirks.no_stream_with_tools.load(Ordering::Relaxed));
    let started = Instant::now();
    loop {
        let mut resp = match ep.complete(body(&req, stream)).await {
            Ok(r) => r,
            Err(f) => {
                return failed(
                    RequestErrorCode::Endpoint,
                    format!("the endpoint could not be reached: {}", f.message),
                    f.retryable,
                );
            }
        };
        if !(200..300).contains(&resp.status) {
            let status = resp.status;
            let start = resp.read_start(ERROR_BODY_READ, crate::llm::LIST_TIMEOUT).await;
            let text = String::from_utf8_lossy(&start).into_owned();
            drop(resp);
            let text = log.redact(&text);
            if stream && has_tools && refuses_streaming_tools(status, &text) {
                quirks.no_stream_with_tools.store(true, Ordering::Relaxed);
                log.warn(format!(
                    "the endpoint refuses to stream a request with tools (HTTP {status}: {}): \
                     such requests go unstreamed from now on, and a cancel of one may not stop \
                     the model",
                    clip(&text, BODY_SHOWN)
                ));
                stream = false;
                continue;
            }
            return failed(
                RequestErrorCode::Endpoint,
                format!("the endpoint answered HTTP {status}: {}", clip(&text, BODY_SHOWN)),
                retryable_status(status),
            );
        }
        let streamed = resp.content_type.starts_with("text/event-stream");
        let mut asm = Assembly::default();
        let mut ttft = None;
        if streamed {
            if let Err((message, retryable)) =
                read_stream(&mut resp, &mut asm, &mut ttft, started, &progress, &log).await
            {
                return failed(RequestErrorCode::Endpoint, message, retryable);
            }
        } else {
            let raw = match resp.read_all(MAX_ANSWER).await {
                Ok(b) => b,
                Err(e) => {
                    return failed(
                        RequestErrorCode::Endpoint,
                        format!("the endpoint's answer could not be read: {e}"),
                        !e.contains("longer than"),
                    );
                }
            };
            let v: Value = match serde_json::from_slice(&raw) {
                Ok(v) => v,
                Err(e) => {
                    let said = String::from_utf8_lossy(&raw);
                    return failed(
                        RequestErrorCode::Endpoint,
                        quote(&log, &format!("the endpoint's answer is not JSON ({e}): "), &said),
                        false,
                    );
                }
            };
            if let Err(e) = asm.message(&v) {
                let message = quote(&log, "the endpoint's answer ", &e);
                return failed(RequestErrorCode::Endpoint, message, false);
            }
            if asm.bytes() > MAX_ASSEMBLED {
                return failed(RequestErrorCode::Endpoint, too_large(), false);
            }
        }
        let gen_s = seconds(started.elapsed());
        drop(resp);
        let (message, finish_reason, usage) = asm.into_parts(&log);
        return Outcome::Done(Done {
            id: req.id,
            message,
            finish_reason,
            usage,
            gen_s,
            ttft_s: ttft.map(seconds),
            streamed,
        });
    }
}

fn too_large() -> String {
    format!("the answer grew past {MAX_ASSEMBLED} bytes, more than one frame can carry")
}

/// Reads a streamed answer into `asm` to its `[DONE]` or its end; a failure is the endpoint's,
/// with whether another try could succeed.
async fn read_stream(
    resp: &mut crate::http1::Response,
    asm: &mut Assembly,
    ttft: &mut Option<Duration>,
    started: Instant,
    progress: &Progress,
    log: &Log,
) -> Result<(), (String, bool)> {
    let mut sse = Sse::default();
    loop {
        let (events, end) = match resp.chunk().await {
            Ok(Some(c)) => (sse.feed(&c), false),
            Ok(None) => (sse.finish(), true),
            Err(e) => return Err((format!("the endpoint's stream broke off: {e}"), true)),
        };
        for ev in events.map_err(|e| (e, false))? {
            match ev {
                Event::Done => return Ok(()),
                Event::Error(e) => {
                    return Err((quote(log, "the endpoint reported an error: ", &e), false));
                }
                Event::Data(text) => {
                    let v: Value = serde_json::from_str(&text).map_err(|e| {
                        let prefix = format!(
                            "the endpoint's stream holds an event that is not JSON ({e}): "
                        );
                        (quote(log, &prefix, &text), false)
                    })?;
                    let token = asm
                        .chunk(&v)
                        .map_err(|e| (quote(log, "the endpoint reported an error: ", &e), false))?;
                    if token {
                        progress.token();
                        ttft.get_or_insert_with(|| started.elapsed());
                    }
                    if asm.bytes() > MAX_ASSEMBLED {
                        return Err((too_large(), false));
                    }
                }
            }
        }
        if end {
            return Ok(());
        }
    }
}

/// One completion within its `timeout_s` (at most [`MAX_TIMEOUT`]); past it, the completion is
/// dropped (its connection with it) and answered `timeout`.
pub async fn run(
    ep: SharedEndpoint,
    req: Request,
    prefer_stream: bool,
    quirks: Arc<Quirks>,
    progress: Arc<Progress>,
    log: Log,
) -> Outcome {
    let limit = Duration::from_secs_f64(req.timeout_s.clamp(1.0, MAX_TIMEOUT.as_secs_f64()));
    let work = complete(ep, req, prefer_stream, quirks, progress, log);
    match tokio::time::timeout(limit, work).await {
        Ok(o) => o,
        Err(_) => failed(
            RequestErrorCode::Timeout,
            format!("no answer within its {:.0} s", limit.as_secs_f64()),
            false,
        ),
    }
}

/// How a completion in [`Flights`] finished.
#[derive(Debug)]
pub enum Finished {
    /// It ran to its end: a `done` or an `error`, even when a cancel came too late to stop it.
    Outcome(Outcome),
    /// The server cancelled it and its task is gone, its connection closed.
    Cancelled { gen_s: f64, tokens_out: u64 },
    /// Its task failed (a panic): reported as the endpoint's failure.
    Broken(String),
}

struct Flight {
    abort: AbortHandle,
    started: Instant,
    progress: Arc<Progress>,
    cancelled: bool,
}

/// The completions in flight on one connection to the server, each a task: started, cancelled,
/// and collected as they finish. Dropping it aborts every one (a lost connection, P4.2.4).
pub struct Flights {
    set: JoinSet<Outcome>,
    by_id: BTreeMap<String, Flight>,
    /// Never iterated (the workspace denies iterating a hash map: its order is not fixed).
    by_task: HashMap<tokio::task::Id, String>,
    gauge: InFlight,
    quirks: Arc<Quirks>,
}

impl Flights {
    /// An empty table counting its tasks in `gauge`.
    #[must_use]
    pub fn new(gauge: InFlight) -> Flights {
        Flights {
            set: JoinSet::new(),
            by_id: BTreeMap::new(),
            by_task: HashMap::new(),
            gauge,
            quirks: Arc::new(Quirks::default()),
        }
    }

    /// The completions in flight.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether none is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Whether `id` is in flight.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// Starts `req` on `ep`.
    pub fn start(&mut self, ep: SharedEndpoint, req: Request, prefer_stream: bool, log: Log) {
        let id = req.id.clone();
        let progress = Arc::new(Progress::default());
        let entered = self.gauge.enter();
        let work = run(ep, req, prefer_stream, self.quirks.clone(), progress.clone(), log);
        let abort = self.set.spawn(async move {
            let _entered = entered;
            work.await
        });
        self.by_task.insert(abort.id(), id.clone());
        self.by_id
            .insert(id, Flight { abort, started: Instant::now(), progress, cancelled: false });
    }

    /// Cancels `id`: its task is aborted, and [`Flights::next`] reports it once it is gone.
    /// False when it is not in flight (it finished, or never was).
    pub fn cancel(&mut self, id: &str) -> bool {
        match self.by_id.get_mut(id) {
            Some(f) => {
                if !f.cancelled {
                    f.cancelled = true;
                    f.abort.abort();
                }
                true
            }
            None => false,
        }
    }

    /// The next completion to finish; pending while none is in flight.
    pub async fn next(&mut self) -> (String, Finished) {
        loop {
            let Some(joined) = self.set.join_next_with_id().await else {
                return std::future::pending().await;
            };
            let (task, result) = match joined {
                Ok((task, outcome)) => (task, Ok(outcome)),
                Err(e) => (e.id(), Err(e)),
            };
            let Some(id) = self.by_task.remove(&task) else { continue };
            let Some(flight) = self.by_id.remove(&id) else { continue };
            let finished = match result {
                Ok(outcome) => Finished::Outcome(outcome),
                Err(e) if e.is_cancelled() => Finished::Cancelled {
                    gen_s: seconds(flight.started.elapsed()),
                    tokens_out: flight.progress.tokens(),
                },
                Err(e) => Finished::Broken(e.to_string()),
            };
            return (id, finished);
        }
    }

    /// Aborts every completion and waits (at most `within`) until their tasks are gone; returns
    /// their ids.
    pub async fn stop(&mut self, within: Duration) -> Vec<String> {
        let ids: Vec<String> = self.by_id.keys().cloned().collect();
        self.set.abort_all();
        let _drained =
            tokio::time::timeout(within, async { while self.set.join_next().await.is_some() {} })
                .await;
        self.by_id.clear();
        self.by_task.clear();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::FrameSet as _;
    use serde_json::json;

    fn request(params: Value, tools: Value) -> Request {
        serde_json::from_value(json!({
            "id": "r1", "model": "model-a",
            "messages": [{"role": "system", "content": "s"}, {"role": "user", "content": "u"}],
            "tools": tools, "params": params, "timeout_s": 60
        }))
        .expect("a request")
    }

    #[test]
    fn the_body_carries_the_five_parameters_and_the_helpers_own_streaming() {
        let r = request(
            json!({"temperature": 0.2, "seed": 3, "max_tokens": 64, "top_p": 0.9,
                   "reasoning_effort": "low", "logit_bias": {"1": 5},
                   "base_url": "http://elsewhere.invalid", "stream": false}),
            json!([{"type": "function", "function": {"name": "end_turn"}}]),
        );
        let b: Value = serde_json::from_slice(&body(&r, true)).expect("json");
        assert_eq!(
            b,
            json!({"model": "model-a", "messages": [{"role": "system", "content": "s"},
                                                     {"role": "user", "content": "u"}],
                   "tools": [{"type": "function", "function": {"name": "end_turn"}}],
                   "temperature": 0.2, "max_tokens": 64, "top_p": 0.9, "seed": 3,
                   "reasoning_effort": "low", "stream": true,
                   "stream_options": {"include_usage": true}})
        );
        let plain = request(json!({}), json!([]));
        let b: Value = serde_json::from_slice(&body(&plain, false)).expect("json");
        assert_eq!(b.get("tools"), None, "an empty tool list is not sent");
        assert_eq!((b["stream"].as_bool(), b.get("stream_options")), (Some(false), None));
    }

    #[test]
    fn server_sent_events_split_anywhere_read_the_same() {
        let text = ": keep-alive\r\ndata: {\"a\":1}\r\n\r\nevent: message\ndata: x\ndata: y\n\n\
                    event: error\ndata: boom\n\ndata: [DONE]\n\n";
        let whole = Sse::default().feed(text.as_bytes()).expect("reads");
        assert_eq!(
            whole,
            [
                Event::Data("{\"a\":1}".into()),
                Event::Data("x\ny".into()),
                Event::Error("boom".into()),
                Event::Done
            ]
        );
        for size in [1, 2, 3, 7] {
            let mut sse = Sse::default();
            let mut got = Vec::new();
            for piece in text.as_bytes().chunks(size) {
                got.extend(sse.feed(piece).expect("reads"));
            }
            got.extend(sse.finish().expect("ends"));
            assert_eq!(got, whole, "{size}-byte pieces");
        }
        let mut sse = Sse::default();
        assert!(sse.feed(b"data: last").expect("no event yet").is_empty());
        assert_eq!(sse.finish().expect("ends"), [Event::Data("last".into())]);
        assert!(Sse::default().feed(&[b'd', 0xff, b'\n']).is_err(), "not UTF-8");
        let mut sse = Sse::default();
        assert!(sse.feed(&vec![b'x'; MAX_ASSEMBLED + 1]).is_err(), "a line without end");
        // A long line in small pieces reads once, not once per piece.
        let mut sse = Sse::default();
        let long = format!("data: {}\n\n", "y".repeat(4 * 1024 * 1024));
        let t0 = std::time::Instant::now();
        let mut got = Vec::new();
        for piece in long.as_bytes().chunks(512) {
            got.extend(sse.feed(piece).expect("reads"));
        }
        assert_eq!(got.len(), 1);
        assert!(t0.elapsed() < std::time::Duration::from_secs(2), "{:?}", t0.elapsed());
    }

    #[test]
    fn deltas_assemble_by_index_and_nothing_is_repaired() {
        let mut a = Assembly::default();
        let chunks = [
            json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}}]}),
            json!({"choices": [{"index": 0, "delta": {"reasoning": "think"}}]}),
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 1, "id": "b", "type": "function", "function": {"name": "two", "arguments": "{"}},
                {"index": 0, "id": "a", "type": "function", "function": {"name": "on", "arguments": ""}}]}}]}),
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "a", "function": {"name": "e", "arguments": "{\"x\": "}},
                {"index": 1, "function": {"arguments": "broken"}}]}}]}),
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": {"y": 1}}}]}}]}),
            json!({"choices": [{"index": 1, "delta": {"content": "another choice"}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}], "usage": null}),
            json!({"choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 7}}),
        ];
        let tokens: Vec<bool> = chunks.iter().map(|c| a.chunk(c).expect("a chunk")).collect();
        assert_eq!(tokens, [false, true, true, true, true, false, false, false]);
        let (m, finish, usage) = a.into_parts(&Log::memory());
        assert_eq!(m.content.as_deref(), Some(""), "the endpoint's empty content, as it came");
        assert_eq!(m.reasoning.as_deref(), Some("think"));
        let calls: Vec<(&str, &str, &str)> = m
            .tool_calls
            .iter()
            .map(|c| (c.id.as_str(), c.function.name.as_str(), c.function.arguments.as_str()))
            .collect();
        assert_eq!(calls, [("a", "one", "{\"x\": {\"y\":1}"), ("b", "two", "{broken")]);
        assert_eq!(finish.as_deref(), Some("tool_calls"));
        assert_eq!(
            usage,
            Some(Usage {
                prompt_tokens: 5,
                completion_tokens: 7,
                reasoning_tokens: None,
                cached_tokens: None
            })
        );

        // Calls without an index: one with an id starts a call, one without adds to the last.
        let mut a = Assembly::default();
        for c in [
            json!({"choices": [{"delta": {"tool_calls": [{"id": "x", "function": {"name": "f", "arguments": "{"}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"function": {"arguments": "}"}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"id": "y", "function": {"name": "g", "arguments": "{}"}}]}}]}),
        ] {
            a.chunk(&c).expect("a chunk");
        }
        let (m, _, _) = a.into_parts(&Log::memory());
        let calls: Vec<(&str, &str)> =
            m.tool_calls.iter().map(|c| (c.id.as_str(), c.function.arguments.as_str())).collect();
        assert_eq!(calls, [("x", "{}"), ("y", "{}")]);
        assert_eq!(m.tool_calls[0].kind, "function");
        assert!(
            Assembly::default().chunk(&json!({"error": {"message": "model crashed"}})).is_err()
        );
    }

    #[test]
    fn an_unstreamed_answer_reads_whole() {
        let mut a = Assembly::default();
        a.message(&json!({
            "choices": [{"index": 0, "finish_reason": "stop", "message": {
                "role": "assistant", "content": null, "reasoning_content": "r",
                "tool_calls": [{"id": "", "type": "function", "function": {"name": "a", "arguments": "{}"}},
                               {"id": "", "type": "function", "function": {"name": "b", "arguments": "{}"}}]}}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 2,
                      "completion_tokens_details": {"reasoning_tokens": 1},
                      "prompt_tokens_details": {"cached_tokens": 4}}
        }))
        .expect("an answer");
        let (m, finish, usage) = a.into_parts(&Log::memory());
        assert_eq!(m.content, None);
        assert_eq!(m.tool_calls.len(), 2, "two calls without ids stay two");
        assert_eq!(finish.as_deref(), Some("stop"));
        assert_eq!(usage.map(|u| (u.reasoning_tokens, u.cached_tokens)), Some((Some(1), Some(4))));
        assert!(Assembly::default().message(&json!({"choices": []})).is_err());
        assert!(Assembly::default().message(&json!({"error": "no such model"})).is_err());
    }

    #[test]
    fn what_an_endpoint_sends_is_cut_to_the_protocols_bounds() {
        let mut a = Assembly::default();
        for i in 0..(MAX_TOOL_CALLS as u64 + 10) {
            let c = json!({"choices": [{"delta": {"tool_calls": [{"index": i * 1_000_000,
                "id": "i".repeat(300), "type": "t".repeat(40),
                "function": {"name": "n".repeat(300), "arguments": "{}"}}]}}]});
            a.chunk(&c).expect("a chunk");
        }
        a.chunk(&json!({"choices": [{"delta": {}, "finish_reason": "f".repeat(100)}]}))
            .expect("a chunk");
        let log = Log::memory();
        let (m, finish, _) = a.into_parts(&log);
        assert_eq!(m.tool_calls.len(), MAX_TOOL_CALLS);
        let c = &m.tool_calls[0];
        assert_eq!((c.id.len(), c.kind.len(), c.function.name.len()), (200, 32, 200));
        assert_eq!(finish.map(|f| f.len()), Some(64));
        // The frame these make passes the protocol's own checks.
        let done = Done {
            id: "r".into(),
            message: m,
            finish_reason: Some("x".into()),
            usage: None,
            gen_s: 1.0,
            ttft_s: None,
            streamed: true,
        };
        let text = crate::protocol::HelperFrame::Done(done).encode(0.0);
        assert!(crate::protocol::parse_helper_frame(&text).is_ok());
        assert!(log.contains("10 tool calls past"), "{:?}", log.lines());
    }

    #[test]
    fn usage_is_kept_only_when_it_reads() {
        assert_eq!(usage_of(&json!({"prompt_tokens": 1})), None);
        assert_eq!(usage_of(&json!({"prompt_tokens": -1, "completion_tokens": 1})), None);
        assert_eq!(usage_of(&json!({"prompt_tokens": 1u64 << 60, "completion_tokens": 1})), None);
        assert!(usage_of(&json!({"prompt_tokens": 1, "completion_tokens": 2})).is_some());
    }

    #[test]
    fn statuses_and_refusals_read_as_the_endpoint_meant_them() {
        assert!(retryable_status(503) && retryable_status(429) && !retryable_status(400));
        assert!(refuses_streaming_tools(400, "Streaming of tool calls is not supported"));
        assert!(refuses_streaming_tools(500, "Cannot use tools with stream"));
        assert!(!refuses_streaming_tools(400, "model not found"));
        assert!(!refuses_streaming_tools(503, "stream of tools"));
    }
}
