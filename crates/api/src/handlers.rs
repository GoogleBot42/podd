//! Route handlers. Business logic mapping wire JSON <-> state + control commands.

use crate::control::{NotImplemented, PodControl};
use crate::error::{invalid_request_data, ApiJson};
use crate::state::StateStore;
use crate::wire::*;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_stream::wrappers::LinesStream;
use tokio_stream::StreamExt;

/// Shared handler state.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<StateStore>,
    pub control: Arc<dyn PodControl>,
}

// ---------------------------------------------------------------------------
// deep-merge helpers
// ---------------------------------------------------------------------------

/// Recursive object merge: objects merge key-by-key, everything else replaces.
fn deep_merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base_map), Value::Object(patch_map)) => {
            for (k, v) in patch_map {
                deep_merge(base_map.entry(k).or_insert(Value::Null), v);
            }
        }
        (base_slot, patch) => *base_slot = patch,
    }
}

/// Schedules merge per spec: per side/day, `power` is deep-merged while
/// `temperatures` and `alarm` are replaced wholesale.
fn merge_schedules(base: &mut Value, patch: Value) {
    let (Some(base_obj), Value::Object(patch_obj)) = (base.as_object_mut(), patch) else {
        return;
    };
    for (side, side_patch) in patch_obj {
        let Value::Object(side_patch) = side_patch else {
            continue;
        };
        let base_side = base_obj.entry(side).or_insert(json!({}));
        let Some(base_side) = base_side.as_object_mut() else {
            continue;
        };
        for (day, day_patch) in side_patch {
            let Value::Object(day_patch) = day_patch else {
                continue;
            };
            let base_day = base_side.entry(day).or_insert(json!({}));
            let Some(base_day) = base_day.as_object_mut() else {
                continue;
            };
            for (key, val) in day_patch {
                match key.as_str() {
                    // deep-merge power (individual fields may be sent alone)
                    "power" => {
                        let slot = base_day.entry("power").or_insert(json!({}));
                        deep_merge(slot, val);
                    }
                    // replace temperatures + alarm wholesale
                    _ => {
                        base_day.insert(key, val);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// deviceStatus
// ---------------------------------------------------------------------------

pub async fn get_device_status(State(app): State<AppState>) -> Json<DeviceStatus> {
    Json(app.store.device_status())
}

/// Map a [`PodControl`] error onto a response. A `send` only fails when the
/// command mpsc into podd-core is closed — the control core is dead — which
/// must not masquerade as success (#33). [`NotImplemented`] maps to 501 (#32).
fn control_error(e: anyhow::Error) -> Response {
    if e.downcast_ref::<NotImplemented>().is_some() {
        (StatusCode::NOT_IMPLEMENTED, e.to_string()).into_response()
    } else {
        log::error!("control command failed: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("control unavailable: {e}"),
        )
            .into_response()
    }
}

pub async fn post_device_status(
    State(app): State<AppState>,
    ApiJson(patch): ApiJson<DeviceStatusPatch>,
) -> Response {
    let sides = [(Side::Left, &patch.left), (Side::Right, &patch.right)];
    for (side, side_patch) in sides {
        let Some(sp) = side_patch else { continue };
        if let Some(on) = sp.is_on {
            if let Err(e) = app.control.set_power(side, on).await {
                return control_error(e);
            }
        }
        if let Some(temp) = sp.target_temperature_f {
            if let Err(e) = app.control.set_target_temp(side, temp).await {
                return control_error(e);
            }
        }
        // isAlarmVibrating can only *clear* (false), never set.
        if sp.is_alarm_vibrating == Some(false) {
            if let Err(e) = app.control.clear_alarm(side).await {
                return control_error(e);
            }
        }
    }
    if patch.is_priming == Some(true) {
        if let Err(e) = app.control.prime().await {
            return control_error(e);
        }
    }
    if let Some(settings) = patch.settings {
        if let Err(e) = app.control.apply_device_settings(settings).await {
            return control_error(e);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

pub async fn get_settings(State(app): State<AppState>) -> Json<Settings> {
    Json(app.store.settings())
}

pub async fn post_settings(
    State(app): State<AppState>,
    ApiJson(mut patch): ApiJson<Value>,
) -> Response {
    // Server drops any client-supplied `id`.
    if let Some(obj) = patch.as_object_mut() {
        obj.remove("id");
    }
    // Fields the daemon acts on have to reach the live config too —
    // settings.json alone is read by nobody (#106). Each bridged field is
    // forwarded over the command bus only when the patch touches it, so an
    // unrelated settings save never resets daemon state.
    let prime_touched = patch.get("primePodDaily").is_some();
    let away_touched = ["left", "right"]
        .iter()
        .any(|side| patch.get(side).and_then(|s| s.get("awayMode")).is_some());
    let tz_touched = patch.get("timeZone").is_some();
    let mut base = serde_json::to_value(app.store.settings()).unwrap();
    deep_merge(&mut base, patch);
    let merged: Settings = match serde_json::from_value(base) {
        Ok(s) => s,
        Err(e) => return invalid_request_data(vec![e.to_string()]),
    };
    // Validate before anything is written: a bad time must not half-apply.
    let prime_time = if prime_touched {
        match parse_hh_mm(&merged.prime_pod_daily.time) {
            Some(t) => Some(t),
            None => {
                return invalid_request_data(vec![format!(
                    "primePodDaily.time must be HH:MM, got {:?}",
                    merged.prime_pod_daily.time
                )])
            }
        }
    } else {
        None
    };
    if tz_touched && jiff::tz::TimeZone::get(&merged.time_zone).is_err() {
        return invalid_request_data(vec![format!(
            "timeZone must be an IANA zone name, got {:?}",
            merged.time_zone
        )]);
    }
    if let Err(e) = app.store.set_settings(merged.clone()) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    if let Some(time) = prime_time {
        if let Err(e) = app
            .control
            .set_prime_daily(merged.prime_pod_daily.enabled, time)
            .await
        {
            return control_error(e);
        }
    }
    if away_touched {
        if let Err(e) = app
            .control
            .set_away_mode(merged.left.away_mode, merged.right.away_mode)
            .await
        {
            return control_error(e);
        }
    }
    if tz_touched {
        if let Err(e) = app.control.set_timezone(&merged.time_zone).await {
            return control_error(e);
        }
    }
    Json(merged).into_response()
}

/// Strict `HH:MM` (the format `config.ron` stores prime times in).
fn parse_hh_mm(s: &str) -> Option<jiff::civil::Time> {
    jiff::civil::Time::strptime("%H:%M", s).ok()
}

// ---------------------------------------------------------------------------
// schedules
// ---------------------------------------------------------------------------

pub async fn get_schedules(State(app): State<AppState>) -> Json<Schedules> {
    Json(app.store.schedules())
}

pub async fn post_schedules(
    State(app): State<AppState>,
    ApiJson(patch): ApiJson<Value>,
) -> Response {
    let mut base = serde_json::to_value(app.store.schedules()).unwrap();
    merge_schedules(&mut base, patch);
    let merged: Schedules = match serde_json::from_value(base) {
        Ok(s) => s,
        Err(e) => return invalid_request_data(vec![e.to_string()]),
    };
    if let Err(e) = app.store.set_schedules(merged.clone()) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Json(merged).into_response()
}

// ---------------------------------------------------------------------------
// alarm
// ---------------------------------------------------------------------------

pub async fn post_alarm(
    State(app): State<AppState>,
    ApiJson(job): ApiJson<AlarmJob>,
) -> Response {
    if let Err(e) = app.control.fire_alarm(job).await {
        return control_error(e);
    }
    // free-sleep returns schedulesDB.data (ignored by the UI).
    Json(app.store.schedules()).into_response()
}

// ---------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------

pub async fn post_execute(
    State(app): State<AppState>,
    ApiJson(req): ApiJson<ExecuteRequest>,
) -> Response {
    match app.control.execute(&req.command, req.arg.as_deref()).await {
        Ok(message) => Json(ExecuteResponse {
            success: true,
            message,
        })
        .into_response(),
        Err(e) if e.downcast_ref::<NotImplemented>().is_some() => {
            (StatusCode::NOT_IMPLEMENTED, e.to_string()).into_response()
        }
        Err(_) => (StatusCode::BAD_REQUEST, "Invalid command").into_response(),
    }
}

// ---------------------------------------------------------------------------
// jobs
// ---------------------------------------------------------------------------

pub async fn post_jobs(
    State(app): State<AppState>,
    ApiJson(jobs): ApiJson<Vec<Job>>,
) -> Response {
    for job in jobs {
        let res = match job {
            Job::Reboot => app.control.reboot().await,
            Job::Update => app.control.update().await,
            // biometrics jobs are accepted but no-op (deferred).
            _ => Ok(()),
        };
        if let Err(e) = res {
            return control_error(e);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// services / serverStatus
// ---------------------------------------------------------------------------

pub async fn get_services() -> Json<Services> {
    Json(Services::default())
}

pub async fn post_services(ApiJson(patch): ApiJson<Value>) -> Response {
    let mut base = serde_json::to_value(Services::default()).unwrap();
    deep_merge(&mut base, patch);
    match serde_json::from_value::<Services>(base) {
        Ok(s) => Json(s).into_response(),
        Err(e) => invalid_request_data(vec![e.to_string()]),
    }
}

/// podd's real subsystem health (see `podd_core::health`) — not free-sleep's
/// twelve permanently-"OK" Node services.
pub async fn get_server_status(State(app): State<AppState>) -> Json<ServerStatus> {
    Json(ServerStatus::from_health(&app.store.health()))
}

// ---------------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------------

/// Journald sources the UI is allowed to tail. podd logs to stderr → journald
/// (there is no log file on the image), so "log names" are systemd units here.
/// `system` is the pseudo-entry for the whole journal (`journalctl` with no
/// `-u` filter).
///
/// This doubles as the validation whitelist for [`get_log_stream`]: the name
/// becomes a subprocess argument, so nothing outside this list may reach it.
pub const LOG_SOURCES: &[&str] = &["podd", "podd-wifi-setup", "NetworkManager", "system"];

/// Pseudo-entry meaning "the whole journal", i.e. no `-u` unit filter.
const LOG_SOURCE_SYSTEM: &str = "system";

/// How much backlog to show before following.
const LOG_TAIL_LINES: &str = "200";

pub async fn get_logs() -> Json<Value> {
    Json(json!({ "logs": LOG_SOURCES }))
}

/// SSE stream of live journald lines for one whitelisted source.
///
/// Spawns `journalctl -n 200 -f -o cat --no-pager` (with `-u <unit>` unless the
/// source is `system`) and forwards each line as `{"message": "..."}`. The
/// child is held by the stream with `kill_on_drop`, so closing the browser tab
/// reaps the follower instead of leaking one `journalctl -f` per page view.
pub async fn get_log_stream(Path(name): Path<String>) -> Response {
    if !LOG_SOURCES.contains(&name.as_str()) {
        return crate::error::not_found();
    }

    let mut cmd = Command::new("journalctl");
    if name != LOG_SOURCE_SYSTEM {
        cmd.arg("-u").arg(&name);
    }
    cmd.args(["-n", LOG_TAIL_LINES, "-f", "-o", "cat", "--no-pager"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let stream: BoxStream = match cmd.spawn() {
        Ok(mut child) => match child.stdout.take() {
            Some(stdout) => {
                let lines = LinesStream::new(BufReader::new(stdout).lines());
                Box::pin(lines.map_while(move |line| {
                    // Hold the child for the life of the stream: dropping it is
                    // what kills the `journalctl -f` (kill_on_drop above).
                    let _child = &child;
                    line.ok().map(|l| Ok(log_event(&l)))
                }))
            }
            None => Box::pin(once_event("log stream unavailable: journalctl stdout not captured")),
        },
        Err(e) => Box::pin(once_event(&format!(
            "log stream unavailable: could not run journalctl ({e})"
        ))),
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

type BoxStream = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

fn log_event(line: &str) -> Event {
    Event::default().data(json!({ "message": line }).to_string())
}

/// A one-shot stream carrying a single explanatory message, then EOF. Used when
/// journald isn't reachable (dev hosts, CI) so the UI shows a reason rather than
/// hanging on an empty stream.
fn once_event(message: &str) -> impl Stream<Item = Result<Event, Infallible>> + Send {
    futures::stream::once(futures::future::ready(Ok(log_event(message))))
}

// ---------------------------------------------------------------------------
// presence
// ---------------------------------------------------------------------------

pub async fn get_presence(State(app): State<AppState>) -> Json<PresenceState> {
    Json(app.store.presence())
}

pub async fn post_presence(
    State(app): State<AppState>,
    ApiJson(patch): ApiJson<PresencePatch>,
) -> Response {
    if patch.left.is_none() && patch.right.is_none() {
        return invalid_request_data(vec!["at least one side is required".to_string()]);
    }
    let now = jiff::Timestamp::now().to_string();
    let state = app.store.with_presence_mut(|p| {
        if let Some(l) = &patch.left {
            p.left.present = l.present;
            p.left.last_updated_at = now.clone();
        }
        if let Some(r) = &patch.right {
            p.right.present = r.present;
            p.right.last_updated_at = now.clone();
        }
        p.clone()
    });
    Json(state).into_response()
}

// ---------------------------------------------------------------------------
// biometrics (deferred): UI-friendly empties
// ---------------------------------------------------------------------------

pub async fn empty_array() -> Json<Value> {
    Json(json!([]))
}

pub async fn vitals_summary() -> Json<Value> {
    Json(json!({
        "avgHeartRate": 0,
        "minHeartRate": 0,
        "maxHeartRate": 0,
        "avgHRV": 0,
        "avgBreathingRate": 0,
    }))
}

pub async fn sleep_put() -> Response {
    // free-sleep returns the updated record; we have none, so 204.
    StatusCode::NO_CONTENT.into_response()
}

pub async fn sleep_delete() -> Response {
    StatusCode::NO_CONTENT.into_response()
}
