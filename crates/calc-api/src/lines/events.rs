//! Live: `GET {line}/events` (server-sent events) and `POST {line}/presence`. See docs/API.md, Live.
//!
//! A viewer subscribes to the line's events before the line's version is read, so nothing published in between is
//! missed; changes since `since` are replayed first (from memory when the line still holds them, else from the
//! operations log, up to 1,000; further behind, a `reset` tells the viewer to reload everything). A `ping` comes
//! every 20 seconds. When the line closes in memory (it was rebuilt or the service is stopping) the stream ends and
//! the viewer reconnects with the last version it saw.

use std::{convert::Infallible, time::Duration};

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use futures::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::broadcast::error::RecvError;

use super::{
    LineEvent,
    ctx::{LineCtx, LinePath, both},
    registry::{PRESENCE_TTL, Presence, Shared},
    store, view,
};
use crate::{
    auth::{Caller, MaybeCaller},
    db::rfc3339,
    error::{ApiError, ApiResult, JsonBody, QueryParams},
    state::AppState,
};

/// The most changes replayed to a viewer who reconnects; further behind, it reloads everything.
const REPLAY_MAX: i64 = 1_000;
const PING: Duration = Duration::from_secs(20);

pub(crate) fn router() -> Router<AppState> {
    let r = Router::new();
    let r = both(r, "/events", get(events));
    both(r, "/presence", post(presence))
}

/// `{"viewers": [{"account", "tab", "range", "client_id", "seen_at"}]}`: who is looking at the line now.
pub(crate) async fn presence_event(app: &AppState, shared: &Shared) -> Value {
    let viewers = shared.viewers();
    let accounts = app.accounts.get_map(viewers.iter().map(|v| v.account)).await;
    let list: Vec<Value> = viewers
        .iter()
        .map(|v| {
            json!({
                "account": view::account_json(accounts.get(&v.account), v.account),
                "tab": v.tab, "range": v.range, "client_id": v.client_id, "seen_at": rfc3339(v.seen_at),
            })
        })
        .collect();
    json!({"viewers": list})
}

#[derive(Deserialize)]
struct EventsQuery {
    since: Option<i64>,
    /// Read by the sign-in (EventSource can't send headers); named here so the query reads cleanly.
    #[serde(rename = "access_token")]
    _access_token: Option<String>,
}

fn sse(name: &str, data: &Value) -> Event {
    Event::default().event(name).data(data.to_string())
}

fn change(data: &Value) -> Event {
    let e = sse("change", data);
    match data.get("version").and_then(Value::as_i64) {
        Some(v) => e.id(v.to_string()),
        None => e,
    }
}

/// Saved changes after `since`, as change events (without cells: viewers reload the ranges they name).
async fn replay_from_log(state: &AppState, line_id: &str, since: i64) -> ApiResult<Vec<Value>> {
    let rows = store::changes_since(state, line_id, since, REPLAY_MAX).await?;
    let accounts = state.accounts.get_map(rows.iter().map(|r| r.author)).await;
    Ok(rows
        .into_iter()
        .map(|r| {
            let mut v = json!({
                "version": r.seq, "by": view::account_json(accounts.get(&r.author), r.author), "at": rfc3339(r.at),
                "kind": r.kind, "summary": r.summary, "changes": r.changes.unwrap_or_else(|| json!([])),
            });
            if let Some(c) = r.client_id {
                v["client_id"] = json!(c);
            }
            v
        })
        .collect())
}

async fn events(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path(p): Path<LinePath>,
    headers: HeaderMap,
    QueryParams(q): QueryParams<EventsQuery>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let ctx = LineCtx::of(&state, caller.as_ref(), &p).await?;
    let handle = ctx.handle(&state).await?;
    let since = match q.since {
        Some(s) if s < 0 => return Err(ApiError::invalid_field("since", "since is a version number: 0 or more.")),
        Some(s) => Some(s),
        // An EventSource reconnecting on its own sends the last id it saw.
        None => headers.get("last-event-id").and_then(|h| h.to_str().ok()).and_then(|s| s.trim().parse::<i64>().ok()),
    };
    let shared = handle.shared.clone();
    // Subscribe first, then read the version: whatever is published in between comes twice and is dropped once.
    let mut rx = shared.events.subscribe();
    let mut closed = shared.closed.subscribe();
    let head = shared.version();
    let mut first: Vec<Event> = Vec::new();
    let mut last = head;
    if let Some(s) = since {
        if s > head || head - s > REPLAY_MAX {
            first.push(sse("reset", &json!({"version": head})));
        } else if s < head {
            let replay: Vec<Value> = match shared.recent_since(s, head) {
                Some(kept) => kept.iter().map(|e| (**e).clone()).collect(),
                None => replay_from_log(&state, &ctx.line.id, s).await?,
            };
            if replay.len() as i64 != head - s {
                first.push(sse("reset", &json!({"version": head})));
            } else {
                first.extend(replay.iter().map(change));
            }
        }
    }
    first.push(sse("presence", &presence_event(&state, &shared).await));
    let stream = async_stream::stream! {
        // The handle keeps the line open while someone watches.
        let _handle = handle;
        for e in first {
            if let Err(error) = crate::visitors::live_access(&state, caller.as_ref(), &p.sheet, &headers).await {
                yield Ok(sse("access", &json!({"code": error.code})));
                return;
            }
            yield Ok(e);
        }
        if *closed.borrow() {
            return;
        }
        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING, PING);
        loop {
            tokio::select! {
                ev = rx.recv() => {
                    if let Err(error) = crate::visitors::live_access(&state, caller.as_ref(), &p.sheet, &headers).await {
                        yield Ok(sse("access", &json!({"code": error.code})));
                        break;
                    }
                    match ev {
                    Ok(LineEvent::AccessChanged) => {},
                    Ok(LineEvent::Change(v)) => {
                        let version = v.get("version").and_then(Value::as_i64).unwrap_or(0);
                        if version <= last {
                            continue;
                        }
                        last = version;
                        yield Ok(change(&v));
                    }
                    Ok(LineEvent::Presence(v)) => yield Ok(sse("presence", &v)),
                    Ok(LineEvent::Comment(v)) => yield Ok(sse("comment", &v)),
                    Ok(LineEvent::Copy(v)) => yield Ok(sse("copy", &v)),
                    Ok(LineEvent::Reset) => {
                        last = shared.version();
                        yield Ok(sse("reset", &json!({"version": last})));
                    }
                    Err(RecvError::Lagged(_)) => {
                        // Too far behind to catch up event by event.
                        last = shared.version();
                        yield Ok(sse("reset", &json!({"version": last})));
                    }
                    Err(RecvError::Closed) => break,
                    }
                },
                _ = ping.tick() => {
                    if let Err(error) = crate::visitors::live_access(&state, caller.as_ref(), &p.sheet, &headers).await {
                        yield Ok(sse("access", &json!({"code": error.code})));
                        break;
                    }
                    yield Ok(sse("ping", &json!({"at": rfc3339(OffsetDateTime::now_utc())})));
                },
                changed = closed.changed() => {
                    if changed.is_err() || *closed.borrow() {
                        break;
                    }
                }
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(60))))
}

#[derive(Deserialize)]
struct PresenceBody {
    tab: Option<view::TabRef>,
    range: Option<String>,
    client_id: Option<String>,
}

async fn presence(
    State(state): State<AppState>,
    caller: Caller,
    Path(p): Path<LinePath>,
    JsonBody(body): JsonBody<PresenceBody>,
) -> ApiResult<StatusCode> {
    let ctx = LineCtx::of(&state, Some(&caller), &p).await?;
    let handle = ctx.handle(&state).await?;
    let client_id: String = body.client_id.as_deref().map(str::trim).unwrap_or("").chars().take(200).collect();
    let client_id = if client_id.is_empty() { caller.uuid.to_string() } else { client_id };
    let range = match body.range.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => {
            view::parse_range(r, "range")?;
            Some(r.to_ascii_uppercase())
        }
        None => None,
    };
    let tab = match body.tab {
        Some(t) => Some(handle.read(move |wb, _| view::find_tab(wb, &t).map(|t| t.id)).await??),
        None => None,
    };
    let key = format!("{}:{client_id}", caller.uuid);
    let changed = {
        let mut map = handle.shared.presence.lock().expect("presence lock");
        map.retain(|_, v| v.seen.elapsed() < PRESENCE_TTL);
        let changed = map.get(&key).is_none_or(|old| old.tab != tab || old.range != range);
        map.insert(
            key,
            Presence {
                account: caller.uuid,
                tab,
                range,
                client_id,
                seen_at: OffsetDateTime::now_utc(),
                seen: std::time::Instant::now(),
            },
        );
        changed
    };
    if changed {
        let ev = presence_event(&state, &handle.shared).await;
        handle.publish(LineEvent::Presence(ev));
    }
    Ok(StatusCode::NO_CONTENT)
}
