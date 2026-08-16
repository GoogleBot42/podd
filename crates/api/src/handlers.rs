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
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::IntervalStream;
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
    let mut base = serde_json::to_value(app.store.settings()).unwrap();
    deep_merge(&mut base, patch);
    let merged: Settings = match serde_json::from_value(base) {
        Ok(s) => s,
        Err(e) => return invalid_request_data(vec![e.to_string()]),
    };
    if let Err(e) = app.store.set_settings(merged.clone()) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    Json(merged).into_response()
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

pub async fn get_server_status() -> Json<ServerStatus> {
    Json(ServerStatus::default())
}

// ---------------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------------

pub async fn get_logs() -> Json<Value> {
    Json(json!({ "logs": ["podd.log"] }))
}

/// SSE stream of log lines. A simple periodic heartbeat implementation — the
/// live tail is wired to podd-core's logger later.
pub async fn get_log_stream(
    Path(filename): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = IntervalStream::new(tokio::time::interval(Duration::from_secs(1))).map(move |_| {
        let payload = json!({ "message": format!("[{filename}] tailing log...") });
        Ok(Event::default().data(payload.to_string()))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
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
