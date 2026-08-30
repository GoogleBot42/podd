//! Route handlers. Business logic mapping wire JSON <-> state + control commands.

use crate::control::{NotImplemented, PodControl};
use crate::error::{invalid_request_data, ApiJson};
use crate::metrics::{
    filter_records, MetricsFilter, MetricsQuery, MovementRecord, SleepRecord, VitalsRecord,
};
use crate::state::StateStore;
use crate::updates::{DaemonBuild, UpdateOps, UpdatesReport};
use crate::wire::*;
use axum::extract::{Path, Query, State};
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
    /// Biometrics history stores; a missing store serves empty results.
    pub biometrics: podd_core::biometrics::Stores,
    /// The on-device update agent; `None` = API-only mode (update routes
    /// report "no agent" rather than pretending).
    pub updates: Option<Arc<dyn UpdateOps>>,
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

/// Schedules merge: per side/day, the `power` and `alarm` structs are
/// deep-merged (individual fields may be sent alone — a partial alarm patch
/// used to 400 on the missing required fields, #106), while `temperatures`
/// is replaced wholesale: it's a time→temp map, so merging would make
/// removing an entry impossible.
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
                    "power" | "alarm" => {
                        let slot = base_day.entry(key).or_insert(json!({}));
                        deep_merge(slot, val);
                    }
                    // replace temperatures (a map) wholesale
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
    // The whole merged document, like the schedules endpoint: what lands in
    // settings.json now drives the alarm/temperature override resolvers, so
    // it has to stay parseable on every future boot (#106).
    let errs = validate_schedule_overrides(&merged);
    if !errs.is_empty() {
        return invalid_request_data(errs);
    }
    if let Err(e) = app.store.set_settings(merged.clone()) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    // Hand the daemon the whole document (daily reboot + schedule overrides
    // live only here); the config.ron-backed fields follow field-by-field.
    if let Err(e) = app.control.set_settings(merged.clone()).await {
        return control_error(e);
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

/// Validate both sides' `scheduleOverrides`: times must be `HH:MM` (or empty =
/// none), expiries RFC 3339 (or empty). The daemon-side resolvers treat
/// malformed fields as "no override", so garbage here would silently disarm
/// the override a user thinks they set — reject it instead.
fn validate_schedule_overrides(s: &Settings) -> Vec<String> {
    fn bad_expiry(at: &str) -> bool {
        !at.is_empty() && at.parse::<jiff::Timestamp>().is_err()
    }
    let mut errs = Vec::new();
    for (key, side) in [("left", &s.left), ("right", &s.right)] {
        let alarm = &side.schedule_overrides.alarm;
        if !alarm.time_override.is_empty() && parse_hh_mm(&alarm.time_override).is_none() {
            errs.push(format!(
                "{key}.scheduleOverrides.alarm.timeOverride must be HH:MM or empty, got {:?}",
                alarm.time_override
            ));
        }
        if bad_expiry(&alarm.expires_at) {
            errs.push(format!(
                "{key}.scheduleOverrides.alarm.expiresAt must be RFC 3339 or empty, got {:?}",
                alarm.expires_at
            ));
        }
        let temps = &side.schedule_overrides.temperature_schedules;
        if bad_expiry(&temps.expires_at) {
            errs.push(format!(
                "{key}.scheduleOverrides.temperatureSchedules.expiresAt must be RFC 3339 or empty, got {:?}",
                temps.expires_at
            ));
        }
    }
    errs
}

// ---------------------------------------------------------------------------
// schedules
// ---------------------------------------------------------------------------

pub async fn get_schedules(State(app): State<AppState>) -> Json<Schedules> {
    Json(app.store.schedules())
}

/// Bounds a bed temperature must stay inside, °F. Same range the wire contract
/// uses everywhere else; the control core turns these into setpoints.
const TEMP_RANGE_F: std::ops::RangeInclusive<TempF> = 55..=110;

/// Structural check on the PATCH body *before* merging: the merge walks the
/// body key-by-key and used to silently drop anything it didn't recognise, so
/// a typo'd day ("mondey") answered 200 and changed nothing (#106).
fn validate_schedules_patch(patch: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    let Some(obj) = patch.as_object() else {
        return vec!["body must be an object of side patches".to_string()];
    };
    for (side, side_patch) in obj {
        if !SIDE_KEYS.contains(&side.as_str()) {
            errs.push(format!("unknown side key {side:?} (expected left or right)"));
            continue;
        }
        let Some(side_patch) = side_patch.as_object() else {
            errs.push(format!("{side} must be an object of day patches"));
            continue;
        };
        for (day, day_patch) in side_patch {
            if !DAY_KEYS.contains(&day.as_str()) {
                errs.push(format!("{side}: unknown day key {day:?}"));
            } else if !day_patch.is_object() {
                errs.push(format!("{side}.{day} must be an object"));
            }
        }
    }
    errs
}

/// Validate the whole merged document — not just the patched fields. What
/// lands in `schedules.json` is what drives the bed's heating windows, so it
/// has to be resolvable by `podd_core::schedule` on every future boot, not
/// merely well-typed today.
fn validate_schedules(s: &Schedules) -> Vec<String> {
    let mut errs = Vec::new();
    for (side, side_sched) in s.sides() {
        for (day, d) in side_sched.days() {
            let at = format!("{side}.{day}");

            for (k, v) in &d.temperatures {
                if parse_hh_mm(k).is_none() {
                    errs.push(format!("{at}.temperatures key {k:?} is not HH:mm"));
                }
                if !TEMP_RANGE_F.contains(v) {
                    errs.push(format!(
                        "{at}.temperatures[{k:?}] must be {}-{} °F, got {v}",
                        TEMP_RANGE_F.start(),
                        TEMP_RANGE_F.end()
                    ));
                }
            }

            match (parse_hh_mm(&d.power.on), parse_hh_mm(&d.power.off)) {
                (Some(on), Some(off)) if on == off => errs.push(format!(
                    "{at}.power.on and power.off must differ (both {:?})",
                    d.power.on
                )),
                (on, off) => {
                    if on.is_none() {
                        errs.push(format!("{at}.power.on is not HH:mm: {:?}", d.power.on));
                    }
                    if off.is_none() {
                        errs.push(format!("{at}.power.off is not HH:mm: {:?}", d.power.off));
                    }
                }
            }
            if !TEMP_RANGE_F.contains(&d.power.on_temperature) {
                errs.push(format!(
                    "{at}.power.onTemperature must be {}-{} °F, got {}",
                    TEMP_RANGE_F.start(),
                    TEMP_RANGE_F.end(),
                    d.power.on_temperature
                ));
            }

            // Alarm fields drive the sensor manager's vibration alarms on
            // owned sides (podd_core::alarm), so they must stay resolvable.
            if parse_hh_mm(&d.alarm.time).is_none() {
                errs.push(format!("{at}.alarm.time is not HH:mm: {:?}", d.alarm.time));
            }
            if !TEMP_RANGE_F.contains(&d.alarm.alarm_temperature) {
                errs.push(format!(
                    "{at}.alarm.alarmTemperature must be {}-{} °F, got {}",
                    TEMP_RANGE_F.start(),
                    TEMP_RANGE_F.end(),
                    d.alarm.alarm_temperature
                ));
            }
            if !(1..=100).contains(&d.alarm.vibration_intensity) {
                errs.push(format!(
                    "{at}.alarm.vibrationIntensity must be 1-100, got {}",
                    d.alarm.vibration_intensity
                ));
            }
            if !(0..=600).contains(&d.alarm.duration) {
                errs.push(format!(
                    "{at}.alarm.duration must be 0-600 s, got {}",
                    d.alarm.duration
                ));
            }
        }
    }
    errs
}

pub async fn post_schedules(
    State(app): State<AppState>,
    ApiJson(patch): ApiJson<Value>,
) -> Response {
    let errs = validate_schedules_patch(&patch);
    if !errs.is_empty() {
        return invalid_request_data(errs);
    }
    let mut base = serde_json::to_value(app.store.schedules()).unwrap();
    merge_schedules(&mut base, patch);
    let merged: Schedules = match serde_json::from_value(base) {
        Ok(s) => s,
        Err(e) => return invalid_request_data(vec![e.to_string()]),
    };
    // Validate before persisting: a rejected save must leave both the file and
    // the running daemon exactly as they were.
    let errs = validate_schedules(&merged);
    if !errs.is_empty() {
        return invalid_request_data(errs);
    }
    if let Err(e) = app.store.set_schedules(merged.clone()) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    // Persist first, then tell the daemon: schedules.json is the source of
    // truth podd-core re-reads on its next start.
    if let Err(e) = app.control.set_schedules(merged.clone()).await {
        return control_error(e);
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

/// Wire name of a job podd has no implementation for at all, or `None` for the
/// jobs that go through [`PodControl`].
///
/// Biometrics (sleep analysis, sensor calibration) is a whole subsystem podd
/// doesn't have — the UI's Features toggle for it has been disabled since #103
/// — so requesting one is `501`, not the old catch-all `Ok(())` that reported a
/// no-op as done (#107).
fn unimplemented_job(job: Job) -> Option<&'static str> {
    match job {
        Job::AnalyzeSleepLeft => Some("analyzeSleepLeft"),
        Job::AnalyzeSleepRight => Some("analyzeSleepRight"),
        Job::BiometricsCalibrationLeft => Some("biometricsCalibrationLeft"),
        Job::BiometricsCalibrationRight => Some("biometricsCalibrationRight"),
        // reboot/update are hardware-seam commands: whether they are
        // implemented is `PodControl`'s answer, not this table's.
        Job::Reboot | Job::Update => None,
    }
}

pub async fn post_jobs(
    State(app): State<AppState>,
    ApiJson(jobs): ApiJson<Vec<Job>>,
) -> Response {
    // Checked up front so a mixed batch never half-applies before the 501.
    let unsupported: Vec<&str> = jobs.iter().copied().filter_map(unimplemented_job).collect();
    if !unsupported.is_empty() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            format!("not implemented yet: {}", unsupported.join(", ")),
        )
            .into_response();
    }
    for job in jobs {
        let res = match job {
            Job::Reboot => app.control.reboot().await,
            Job::Update => app.control.update().await,
            // Unreachable: `unimplemented_job` above rejected the whole
            // request for anything else. Skip rather than report success.
            _ => continue,
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

/// The services document (see [`Services::default`]). `biometrics.enabled`
/// reflects whether the biometrics pipeline is actually wired (#12, #141); its
/// per-job entries stay "not implemented" — free-sleep's batch sleep-analysis
/// and calibration jobs don't exist in podd, whose detection runs continuously
/// off the live stream instead.
pub async fn get_services(State(app): State<AppState>) -> Json<Services> {
    let mut services = Services::default();
    services.biometrics.enabled = app.biometrics.vitals.is_some();
    Json(services)
}

/// `501`. There is nothing to configure: the only service in the document is
/// biometrics, which podd does not implement, and this handler used to merge
/// the patch into a fresh default and echo it back — persisting nothing, so the
/// next `GET` silently reverted (#107; the UI toggle has been disabled since
/// #103). Reinstate a real handler when a service exists to configure.
pub async fn post_services(ApiJson(_patch): ApiJson<Value>) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        "configuring services is not implemented yet",
    )
        .into_response()
}

/// podd's real subsystem health (see `podd_core::health`) — not free-sleep's
/// twelve permanently-"OK" Node services.
pub async fn get_server_status(State(app): State<AppState>) -> Json<ServerStatus> {
    Json(ServerStatus::from_health(&app.store.health()))
}

// ---------------------------------------------------------------------------
// updates (REPLACEMENT_PLAN §9 observability; issue #1)
// ---------------------------------------------------------------------------

/// `GET /api/updates` — what the device runs plus the update agent's state.
/// Always 200: `updater: null` says "no agent wired", which is different from
/// "no updates available".
pub async fn get_updates(State(app): State<AppState>) -> Json<UpdatesReport> {
    Json(UpdatesReport {
        daemon: DaemonBuild::default(),
        updater: app.updates.as_ref().map(|u| u.status()),
    })
}

/// 503 for the action routes when no update agent is wired.
fn no_updater() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "no update agent is running",
    )
        .into_response()
}

/// `POST /api/updates/check` — poll the configured channel now. Returns the
/// refreshed status on success; `502` (with the agent's message) when the
/// channel could not be reached or verified.
pub async fn post_updates_check(State(app): State<AppState>) -> Response {
    let Some(updates) = app.updates.clone() else {
        return no_updater();
    };
    match updates.check_now().await {
        Ok(()) => Json(updates.status()).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("update check failed: {e}")).into_response(),
    }
}

/// `POST /api/updates/rollback` — flip the Tier-2 app symlink back to the
/// previous release. On-device this restarts `podd`, so the response may never
/// reach the client; the UI treats a dropped connection as "restarting".
pub async fn post_updates_rollback(State(app): State<AppState>) -> Response {
    let Some(updates) = app.updates.clone() else {
        return no_updater();
    };
    match updates.rollback() {
        Ok(restored) => {
            log::warn!("rolling app back to {restored} on API request");
            Json(json!({ "restored": restored })).into_response()
        }
        Err(e) => (
            StatusCode::CONFLICT,
            format!("rollback not possible: {e}"),
        )
            .into_response(),
    }
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
// biometrics
//
// All three histories are real now: vitals (#12) and sleep/movement (#141) are
// served from the stores handed to `router_with_biometrics` (empty without
// them). The `?startTime=&endTime=&side=` filtering runs on every request
// (#108).
// ---------------------------------------------------------------------------

/// Validate the shared metrics query, or answer 400 with the failure details.
fn metrics_filter(query: &MetricsQuery) -> Result<MetricsFilter, Response> {
    query.parse().map_err(invalid_request_data)
}

fn to_wire_side(side: pod_proto::packet::BedSide) -> Side {
    match side {
        pod_proto::packet::BedSide::Left => Side::Left,
        pod_proto::packet::BedSide::Right => Side::Right,
    }
}

fn to_store_side(side: Side) -> pod_proto::packet::BedSide {
    match side {
        Side::Left => pod_proto::packet::BedSide::Left,
        Side::Right => pod_proto::packet::BedSide::Right,
    }
}

/// Epoch seconds as the ISO-8601 instant the UI's zod schemas expect.
fn to_iso(seconds: i64) -> String {
    jiff::Timestamp::from_second(seconds)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

/// Read one biometrics store for a validated filter. The store pre-filters by
/// window/side; `filter_records` still runs on the converted records so every
/// endpoint honours exactly the same contract.
fn from_store<T: podd_core::biometrics::StoredRecord>(
    store: Option<&Arc<podd_core::biometrics::JsonlStore<T>>>,
    filter: &MetricsFilter,
) -> Vec<T> {
    let Some(store) = store else {
        return Vec::new();
    };
    match store.query(
        filter.start.map(|t| t.as_second()),
        filter.end.map(|t| t.as_second()),
        filter.side.map(to_store_side),
    ) {
        Ok(records) => records,
        Err(e) => {
            log::error!("{} store read failed: {e}", T::LABEL);
            Vec::new()
        }
    }
}

/// Read + convert the sleep history (epoch seconds in the store, ISO-8601 on
/// the wire).
fn sleep_from_store(app: &AppState, filter: &MetricsFilter) -> Vec<SleepRecord> {
    from_store(app.biometrics.sleep.as_ref(), filter)
        .into_iter()
        .map(|r| SleepRecord {
            id: r.id,
            side: match to_wire_side(r.side) {
                Side::Left => "left".to_string(),
                Side::Right => "right".to_string(),
            },
            entered_bed_at: to_iso(r.entered_bed_at),
            left_bed_at: to_iso(r.left_bed_at),
            sleep_period_seconds: r.sleep_period_seconds,
            times_exited_bed: r.times_exited_bed,
            present_intervals: r
                .present_intervals
                .iter()
                .map(|(s, e)| (to_iso(*s), to_iso(*e)))
                .collect(),
            not_present_intervals: r
                .not_present_intervals
                .iter()
                .map(|(s, e)| (to_iso(*s), to_iso(*e)))
                .collect(),
        })
        .collect()
}

fn vitals_from_store(app: &AppState, filter: &MetricsFilter) -> Vec<VitalsRecord> {
    from_store(app.biometrics.vitals.as_ref(), filter)
        .into_iter()
        .map(|r| VitalsRecord {
            side: to_wire_side(r.side),
            timestamp: r.timestamp,
            heart_rate: r.heart_rate,
            hrv: r.hrv,
            breathing_rate: r.breathing_rate,
        })
        .collect()
}

fn movement_from_store(app: &AppState, filter: &MetricsFilter) -> Vec<MovementRecord> {
    from_store(app.biometrics.movement.as_ref(), filter)
        .into_iter()
        .map(|r| MovementRecord {
            // The UI keys rows by id; bucket start + side is unique.
            id: r.timestamp * 2 + matches!(r.side, pod_proto::packet::BedSide::Right) as i64,
            side: to_wire_side(r.side),
            timestamp: r.timestamp,
            total_movement: r.total_movement,
        })
        .collect()
}

pub async fn get_sleep_records(
    State(app): State<AppState>,
    Query(query): Query<MetricsQuery>,
) -> Response {
    let filter = match metrics_filter(&query) {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    let records = sleep_from_store(&app, &filter);
    Json(filter_records(records, &filter)).into_response()
}

pub async fn get_vitals_records(
    State(app): State<AppState>,
    Query(query): Query<MetricsQuery>,
) -> Response {
    let filter = match metrics_filter(&query) {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    let records = vitals_from_store(&app, &filter);
    Json(filter_records(records, &filter)).into_response()
}

pub async fn get_movement_records(
    State(app): State<AppState>,
    Query(query): Query<MetricsQuery>,
) -> Response {
    let filter = match metrics_filter(&query) {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    let records = movement_from_store(&app, &filter);
    Json(filter_records(records, &filter)).into_response()
}

pub async fn vitals_summary(
    State(app): State<AppState>,
    Query(query): Query<MetricsQuery>,
) -> Response {
    let filter = match metrics_filter(&query) {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    let records = filter_records(vitals_from_store(&app, &filter), &filter);
    let n = records.len() as i64;
    let avg = |f: fn(&VitalsRecord) -> i64| {
        if n == 0 {
            0
        } else {
            records.iter().map(f).sum::<i64>() / n
        }
    };
    Json(json!({
        "avgHeartRate": avg(|r| r.heart_rate),
        "minHeartRate": records.iter().map(|r| r.heart_rate).min().unwrap_or(0),
        "maxHeartRate": records.iter().map(|r| r.heart_rate).max().unwrap_or(0),
        "avgHRV": avg(|r| r.hrv),
        "avgBreathingRate": avg(|r| r.breathing_rate),
    }))
    .into_response()
}

/// Body of `PUT /metrics/sleep/{id}`: the UI's edit dialog sends the two
/// timestamps as ISO-8601 (either may be omitted).
#[derive(serde::Deserialize, Default)]
pub struct SleepPatch {
    pub entered_bed_at: Option<String>,
    pub left_bed_at: Option<String>,
}

/// Correct a detected session's bed times (the UI's edit dialog). The derived
/// fields are recomputed from the stored intervals clipped to the new window,
/// so an edit can't leave a record self-inconsistent.
pub async fn sleep_put(
    State(app): State<AppState>,
    Path(id): Path<i64>,
    ApiJson(patch): ApiJson<SleepPatch>,
) -> Response {
    let mut errors = Vec::new();
    let entered = crate::metrics::parse_param(
        "entered_bed_at",
        patch.entered_bed_at.as_deref(),
        &mut errors,
    );
    let left = crate::metrics::parse_param("left_bed_at", patch.left_bed_at.as_deref(), &mut errors);
    if !errors.is_empty() {
        return invalid_request_data(errors);
    }
    let Some(store) = app.biometrics.sleep.as_ref() else {
        return crate::error::not_found();
    };

    let mut updated = None;
    let res = store.rewrite(|mut rec| {
        if rec.id == id {
            if let Some(t) = entered {
                rec.entered_bed_at = t.as_second();
            }
            if let Some(t) = left {
                rec.left_bed_at = t.as_second();
            }
            rec.reclip();
            updated = Some(rec.clone());
        }
        Some(rec)
    });
    if let Err(e) = res {
        log::error!("sleep store update failed: {e}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match updated {
        // Re-read through the normal conversion so the response is exactly
        // what a subsequent GET would return.
        Some(rec) => {
            let filter = MetricsFilter {
                start: jiff::Timestamp::from_second(rec.entered_bed_at).ok(),
                end: jiff::Timestamp::from_second(rec.entered_bed_at).ok(),
                side: None,
            };
            match sleep_from_store(&app, &filter)
                .into_iter()
                .find(|r| r.id == rec.id)
            {
                Some(wire) => Json(wire).into_response(),
                None => StatusCode::NO_CONTENT.into_response(),
            }
        }
        None => crate::error::not_found(),
    }
}

pub async fn sleep_delete(State(app): State<AppState>, Path(id): Path<i64>) -> Response {
    let Some(store) = app.biometrics.sleep.as_ref() else {
        return crate::error::not_found();
    };
    match store.rewrite(|rec| (rec.id != id).then_some(rec)) {
        Ok(0) => crate::error::not_found(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            log::error!("sleep store delete failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
