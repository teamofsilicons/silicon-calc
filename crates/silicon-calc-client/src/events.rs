//! Live events: `GET {line}/events?since={version}` as a stream of typed events.
//!
//! The service sends server-sent events (`event: change`, `presence`, `comment`, `copy`, `reset`, `ping`), each
//! `data:` one JSON object. [`EventStream`] parses them, remembers the last version it saw, and when the
//! connection drops it reconnects with `since` set to that version, so no change is missed (the service replays
//! what happened in between, or sends `reset` when that is too much).

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::{Stream, StreamExt};
use serde_json::Value as Json;

use crate::client::{Client, Line};
use crate::error::{Error, Result};
use crate::types::{ChangeEvent, CommentEvent, CopyEvent, PresenceEvent};

/// One server-sent event as it came over the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawEvent {
    /// The `event:` name (`message` when there was none).
    pub event: String,
    /// The `data:` lines, joined with newlines.
    pub data: String,
    /// The `id:`, if any.
    pub id: Option<String>,
    /// The `retry:` the service asked for, in milliseconds.
    pub retry: Option<u64>,
}

/// Reads a `text/event-stream` body chunk by chunk (any chunking; `\n`, `\r\n` or `\r` line ends).
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    id: Option<String>,
    retry: Option<u64>,
    /// Lines seen since the last call, including comments (`:` keep-alives).
    activity: bool,
}

impl SseParser {
    /// A parser with nothing read yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when anything (an event, a field or a keep-alive comment) arrived since the last call.
    pub fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity)
    }

    /// Feeds a chunk; returns the events it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<RawEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let Some(pos) = self.buf.iter().position(|&b| b == b'\n' || b == b'\r') else { break };
            if self.buf[pos] == b'\r' && pos + 1 == self.buf.len() {
                // A `\r` at the very end: wait to see whether `\n` follows.
                break;
            }
            let mut consumed = pos + 1;
            if self.buf[pos] == b'\r' && self.buf.get(pos + 1) == Some(&b'\n') {
                consumed += 1;
            }
            let line = String::from_utf8_lossy(&self.buf[..pos]).into_owned();
            self.buf.drain(..consumed);
            self.activity = true;
            self.line(&line, &mut out);
        }
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<RawEvent>) {
        if line.is_empty() {
            let event = self.event.take().unwrap_or_else(|| "message".into());
            if self.data.is_empty() {
                return;
            }
            let data = std::mem::take(&mut self.data).join("\n");
            out.push(RawEvent { event, data, id: self.id.clone(), retry: self.retry.take() });
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            "retry" => self.retry = value.parse().ok(),
            _ => {}
        }
    }
}

/// A typed live event.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The line changed: a new version.
    Change(ChangeEvent),
    /// Who is viewing changed.
    Presence(PresenceEvent),
    /// A thread was created, replied to, edited, resolved or reopened.
    Comment(CommentEvent),
    /// A copy of this line was made, merged or deleted.
    Copy(CopyEvent),
    /// Too much changed to describe: reload everything.
    Reset,
    /// The service is still there (every 20 seconds).
    Ping,
    /// The connection dropped; the stream reconnects after `after`, from the last version seen.
    Reconnecting {
        /// Why it dropped.
        reason: String,
        /// How many tries in a row.
        attempt: u32,
        /// How long until the next try.
        after: Duration,
    },
    /// An event this client doesn't know (or whose data it couldn't read).
    Other {
        /// The event's name.
        event: String,
        /// Its data.
        data: Json,
    },
}

impl Event {
    /// The event's name as the service sends it (`change`, `comment`…; `reconnecting` for the client's own).
    pub fn name(&self) -> &str {
        match self {
            Self::Change(_) => "change",
            Self::Presence(_) => "presence",
            Self::Comment(_) => "comment",
            Self::Copy(_) => "copy",
            Self::Reset => "reset",
            Self::Ping => "ping",
            Self::Reconnecting { .. } => "reconnecting",
            Self::Other { event, .. } => event,
        }
    }

    /// Reads a raw event.
    pub fn from_raw(raw: RawEvent) -> Self {
        let data: Json = serde_json::from_str(&raw.data).unwrap_or(Json::String(raw.data.clone()));
        fn typed<T: serde::de::DeserializeOwned>(data: &Json) -> Option<T> {
            serde_json::from_value(data.clone()).ok()
        }
        let typed_event = match raw.event.as_str() {
            "change" => typed(&data).map(Self::Change),
            "presence" => typed(&data).map(Self::Presence),
            "comment" => typed(&data).map(Self::Comment),
            "copy" => typed(&data).map(Self::Copy),
            "reset" => Some(Self::Reset),
            "ping" => Some(Self::Ping),
            _ => None,
        };
        typed_event.unwrap_or(Self::Other { event: raw.event, data })
    }

    /// The event as one JSON object: `{"event": name, …data}`.
    pub fn to_json(&self) -> Json {
        let mut v = match self {
            Self::Change(c) => serde_json::to_value(c).unwrap_or_default(),
            Self::Presence(p) => serde_json::to_value(p).unwrap_or_default(),
            Self::Comment(c) => serde_json::to_value(c).unwrap_or_default(),
            Self::Copy(c) => serde_json::to_value(c).unwrap_or_default(),
            Self::Reset | Self::Ping => Json::Object(Default::default()),
            Self::Reconnecting { reason, attempt, after } => serde_json::json!({
                "reason": reason, "attempt": attempt, "after_ms": after.as_millis() as u64,
            }),
            Self::Other { data, .. } => match data {
                Json::Object(_) => data.clone(),
                other => serde_json::json!({ "data": other }),
            },
        };
        if let Json::Object(m) = &mut v {
            m.insert("event".into(), Json::String(self.name().to_owned()));
        }
        v
    }
}

/// How [`Client::events`] behaves.
#[derive(Debug, Clone)]
pub struct EventOptions {
    /// Replay what happened after this version first.
    pub since: Option<i64>,
    /// Reconnect when the connection drops (default on).
    pub reconnect: bool,
    /// Treat the connection as dead after this long with nothing from the service (default 60 s; the service
    /// pings every 20).
    pub idle_timeout: Duration,
    /// The longest wait between reconnection tries (default 30 s).
    pub max_backoff: Duration,
}

impl Default for EventOptions {
    fn default() -> Self {
        Self { since: None, reconnect: true, idle_timeout: Duration::from_secs(60), max_backoff: Duration::from_secs(30) }
    }
}

impl EventOptions {
    /// Starting after `version`.
    pub fn since(version: Option<i64>) -> Self {
        Self { since: version, ..Self::default() }
    }
}

fn backoff(attempt: u32, max: Duration) -> Duration {
    let secs = 1u64 << attempt.saturating_sub(1).min(5);
    Duration::from_secs(secs).min(max)
}

/// The live events of a line. Ends only on an error that retrying can't fix (signed out, no access, no such
/// line), or when the connection drops with reconnecting turned off.
pub struct EventStream {
    inner: Pin<Box<dyn Stream<Item = Result<Event>> + Send>>,
    last: Arc<AtomicI64>,
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream").field("last_version", &self.last_version()).finish()
    }
}

impl EventStream {
    pub(crate) fn new(client: Client, line: Line, options: EventOptions) -> Self {
        let last = Arc::new(AtomicI64::new(options.since.unwrap_or(-1)));
        let seen = last.clone();
        let inner = async_stream::stream! {
            let mut attempt: u32 = 0;
            loop {
                let since = { let v = seen.load(Ordering::SeqCst); (v >= 0).then_some(v) };
                let reason: String = match client.open_events(&line, since).await {
                    Ok(response) => {
                        let mut parser = SseParser::new();
                        let mut body = Box::pin(response.bytes_stream());
                        loop {
                            match tokio::time::timeout(options.idle_timeout, body.next()).await {
                                Err(_) => break format!(
                                    "nothing arrived for {} seconds", options.idle_timeout.as_secs()
                                ),
                                Ok(None) => break "the service ended the stream".to_owned(),
                                Ok(Some(Err(e))) => break crate::client::network_error(e).message().to_owned(),
                                Ok(Some(Ok(chunk))) => {
                                    let events = parser.push(&chunk);
                                    if parser.take_activity() {
                                        attempt = 0;
                                    }
                                    for raw in events {
                                        let event = Event::from_raw(raw);
                                        match &event {
                                            Event::Change(c) => seen.store(c.version, Ordering::SeqCst),
                                            Event::Reset => {
                                                // Everything gets reloaded: carry on from the version as it is now.
                                                if let Ok(t) = client.tabs(&line).await {
                                                    seen.store(t.version, Ordering::SeqCst);
                                                }
                                            }
                                            _ => {}
                                        }
                                        yield Ok(event);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) if options.reconnect && e.is_transient() => e.message().to_owned(),
                    Err(e) => {
                        yield Err(e);
                        break;
                    }
                };
                if !options.reconnect {
                    break;
                }
                attempt += 1;
                let after = backoff(attempt, options.max_backoff);
                yield Ok(Event::Reconnecting { reason, attempt, after });
                tokio::time::sleep(after).await;
            }
        };
        Self { inner: Box::pin(inner), last }
    }

    /// The last version a `change` event carried (or the `since` it started from).
    pub fn last_version(&self) -> Option<i64> {
        let v = self.last.load(Ordering::SeqCst);
        (v >= 0).then_some(v)
    }
}

impl Stream for EventStream {
    type Item = Result<Event, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_any_chunking() {
        let text = "event: change\ndata: {\"version\": 4, \"by\": {\"id\": \"c:ada\"}, \"changes\": [{\"tab\": 1, \"range\": \"B2\"}]}\n\n: keep-alive\n\nevent: ping\ndata: {}\n\n";
        for size in [1, 2, 3, 7, 1000] {
            let mut p = SseParser::new();
            let mut events = Vec::new();
            for chunk in text.as_bytes().chunks(size) {
                events.extend(p.push(chunk));
            }
            assert_eq!(events.len(), 2, "chunk size {size}");
            assert_eq!(events[0].event, "change");
            assert_eq!(events[1].event, "ping");
            match Event::from_raw(events[0].clone()) {
                Event::Change(c) => {
                    assert_eq!(c.version, 4);
                    assert_eq!(c.by.id, "c:ada");
                    assert_eq!(c.changes[0].range.as_deref(), Some("B2"));
                }
                other => panic!("not a change: {other:?}"),
            }
            assert_eq!(Event::from_raw(events[1].clone()), Event::Ping);
        }
    }

    #[test]
    fn handles_crlf_multiline_data_and_ids() {
        let mut p = SseParser::new();
        let events = p.push(b"id: 7\r\nevent: comment\r\ndata: {\"thread\":\r\ndata: {\"id\": \"t1\"}}\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("7"));
        assert_eq!(events[0].data, "{\"thread\":\n{\"id\": \"t1\"}}");
        match Event::from_raw(events[0].clone()) {
            Event::Comment(c) => assert_eq!(c.thread.id, "t1"),
            other => panic!("not a comment: {other:?}"),
        }
        // A lone \r at the end of a chunk waits for the next one.
        let mut p = SseParser::new();
        assert!(p.push(b"event: reset\r").is_empty());
        let events = p.push(b"\ndata: {}\r\n\r\n");
        assert_eq!(Event::from_raw(events[0].clone()), Event::Reset);
    }

    #[test]
    fn unknown_events_are_kept() {
        let e = Event::from_raw(RawEvent { event: "future".into(), data: "{\"x\": 1}".into(), ..Default::default() });
        assert_eq!(e.name(), "future");
        assert_eq!(e.to_json()["x"], 1);
        assert_eq!(e.to_json()["event"], "future");
        let e = Event::from_raw(RawEvent { event: "change".into(), data: "not json".into(), ..Default::default() });
        assert!(matches!(e, Event::Other { .. }));
    }

    #[test]
    fn backs_off() {
        let max = Duration::from_secs(30);
        assert_eq!(backoff(1, max), Duration::from_secs(1));
        assert_eq!(backoff(2, max), Duration::from_secs(2));
        assert_eq!(backoff(4, max), Duration::from_secs(8));
        assert_eq!(backoff(9, max), Duration::from_secs(30));
    }
}
