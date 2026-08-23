//! HTTP-level tests driving the `router()` via `tower::ServiceExt::oneshot`.

use api::control::Call;
use api::{router, MockControl, PodControl, StateStore, StoreConfig};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn build() -> (axum::Router, Arc<MockControl>, Arc<StateStore>) {
    let store = Arc::new(StateStore::in_memory());
    let control = Arc::new(MockControl::new());
    let app = router(store.clone(), control.clone() as Arc<dyn PodControl>, None);
    (app, control, store)
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

fn post_json(path: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn get_device_status_ok() {
    let (app, _c, _s) = build();
    let resp = app.oneshot(get("/api/deviceStatus")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    // camelCase shape sanity
    assert!(v["left"]["targetTemperatureF"].is_number());
    assert!(v["waterLevel"].is_string());
    assert!(v["isPriming"].is_boolean());
    assert!(v["coverVersion"].is_string());
    assert!(v["freeSleep"]["version"].is_string());
    assert!(v["wifiStrength"].is_number());
}

/// Issue #109: the build chips used to report the never-bumped workspace version
/// and a hardcoded `"main"`. Both fields must now be the real build stamp.
#[tokio::test]
async fn device_status_reports_the_build_stamp() {
    let (app, _c, _s) = build();
    let resp = app.oneshot(get("/api/deviceStatus")).await.unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["freeSleep"]["version"], json!(podd_core::VERSION));
    assert_eq!(v["freeSleep"]["branch"], json!(podd_core::GIT_REV));
    assert_ne!(v["freeSleep"]["branch"], json!("main"));
}

#[tokio::test]
async fn post_device_status_power() {
    let (app, control, _s) = build();
    let resp = app
        .oneshot(post_json("/api/deviceStatus", &json!({ "left": { "isOn": true } })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let calls = control.calls();
    assert!(matches!(calls.as_slice(), [Call::SetPower(api::wire::Side::Left, true)]));
}

#[tokio::test]
async fn settings_get_then_merge_persists() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let store = Arc::new(StateStore::new(StoreConfig {
        settings_path: Some(settings_path.clone()),
        schedules_path: None,
    }));
    let control = Arc::new(MockControl::new());
    let app = router(store.clone(), control as Arc<dyn PodControl>, None);

    // GET default
    let resp = app.clone().oneshot(get("/api/settings")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["timeZone"], "UTC");
    assert_eq!(v["left"]["name"], "Left");

    // POST partial merge (+ an `id` that must be dropped)
    let patch = json!({ "id": "bogus", "timeZone": "America/New_York", "left": { "name": "Bedroom" } });
    let resp = app.clone().oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["timeZone"], "America/New_York");
    assert_eq!(v["left"]["name"], "Bedroom");
    assert_eq!(v["right"]["name"], "Right"); // untouched
    assert_eq!(v["id"], "1"); // client id dropped, server id preserved

    // Reload from disk -> change persisted
    let reloaded = StateStore::new(StoreConfig {
        settings_path: Some(settings_path),
        schedules_path: None,
    });
    let s = reloaded.settings();
    assert_eq!(s.time_zone, "America/New_York");
    assert_eq!(s.left.name, "Bedroom");
}

// ---------------------------------------------------------------------------
// settings -> live config bridge: primePodDaily
// ---------------------------------------------------------------------------

#[tokio::test]
async fn settings_prime_pod_daily_reaches_the_config() {
    let (app, control, _s) = build();

    // a partial patch (time inherited from the stored settings) still applies
    let patch = json!({ "primePodDaily": { "enabled": true } });
    let resp = app.clone().oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    match control.calls().as_slice() {
        [Call::SetPrimeDaily(true, t)] => {
            assert_eq!(t.to_string(), "14:00:00"); // the default settings time
        }
        other => panic!("expected one SetPrimeDaily, got {other:?}"),
    }

    // turning it off, with a new time, propagates both fields
    let patch = json!({ "primePodDaily": { "enabled": false, "time": "03:30" } });
    let resp = app.clone().oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    match control.calls().as_slice() {
        [_, Call::SetPrimeDaily(false, t)] => assert_eq!(t.to_string(), "03:30:00"),
        other => panic!("expected a second SetPrimeDaily, got {other:?}"),
    }
}

#[tokio::test]
async fn settings_without_bridged_fields_touch_nothing() {
    let (app, control, _s) = build();
    let patch = json!({ "rebootDaily": false, "left": { "name": "Bedroom" } });
    let resp = app.oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        control.calls().is_empty(),
        "unrelated settings must not touch the config: {:?}",
        control.calls()
    );
}

#[tokio::test]
async fn settings_away_mode_reaches_the_config() {
    let (app, control, _s) = build();

    // one side away: the command carries both sides' merged state
    let patch = json!({ "left": { "awayMode": true } });
    let resp = app.clone().oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    match control.calls().as_slice() {
        [Call::SetAwayMode(true, false)] => {}
        other => panic!("expected SetAwayMode(true, false), got {other:?}"),
    }

    // the other side follows (left inherited from the stored settings)
    let patch = json!({ "right": { "awayMode": true } });
    let resp = app.clone().oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    match control.calls().as_slice() {
        [_, Call::SetAwayMode(true, true)] => {}
        other => panic!("expected a second SetAwayMode(true, true), got {other:?}"),
    }
}

#[tokio::test]
async fn settings_timezone_reaches_the_config() {
    let (app, control, _s) = build();
    let patch = json!({ "timeZone": "America/Denver" });
    let resp = app.oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    match control.calls().as_slice() {
        [Call::SetTimezone(tz)] => assert_eq!(tz, "America/Denver"),
        other => panic!("expected one SetTimezone, got {other:?}"),
    }
}

#[tokio::test]
async fn settings_reject_a_bad_timezone_without_applying_anything() {
    let (app, control, store) = build();
    let patch = json!({ "timeZone": "Not/AZone" });
    let resp = app.oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"], "Invalid request data");
    assert_eq!(store.settings().time_zone, "UTC");
    assert!(control.calls().is_empty());
}

#[tokio::test]
async fn settings_reject_a_bad_prime_time_without_applying_anything() {
    let (app, control, store) = build();
    let patch = json!({ "primePodDaily": { "enabled": true, "time": "25:99" } });
    let resp = app.oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"], "Invalid request data");
    // nothing applied: neither the stored settings nor the config
    let s = store.settings();
    assert!(!s.prime_pod_daily.enabled);
    assert_eq!(s.prime_pod_daily.time, "14:00");
    assert!(control.calls().is_empty());
}

#[tokio::test]
async fn schedules_partial_merge() {
    let (app, _c, _s) = build();
    // Replace monday power.enabled + temperatures for left; leave rest intact.
    let patch = json!({
        "left": {
            "monday": {
                "power": { "enabled": true },
                "temperatures": { "07:00": 72, "22:00": 68 }
            }
        }
    });
    let resp = app.oneshot(post_json("/api/schedules", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["left"]["monday"]["power"]["enabled"], true);
    // power.on retained from default (deep-merge)
    assert!(v["left"]["monday"]["power"]["on"].is_string());
    assert_eq!(v["left"]["monday"]["temperatures"]["07:00"], 72);
    // other day untouched
    assert!(v["left"]["tuesday"]["power"].is_object());
}

#[tokio::test]
async fn schedules_partial_alarm_patch_merges() {
    let (app, _c, _s) = build();
    // A lone alarm field must merge into the stored alarm, not replace it
    // wholesale (which 400'd on the missing required fields, #106).
    let patch = json!({ "right": { "friday": { "alarm": { "enabled": true } } } });
    let resp = app.clone().oneshot(post_json("/api/schedules", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["right"]["friday"]["alarm"]["enabled"], true);
    // the untouched alarm fields survive from the default
    assert_eq!(v["right"]["friday"]["alarm"]["time"], "07:00");
    assert_eq!(v["right"]["friday"]["alarm"]["vibrationIntensity"], 50);
}

#[tokio::test]
async fn jobs_reboot_and_update() {
    let (app, control, _s) = build();
    let resp = app
        .clone()
        .oneshot(post_json("/api/jobs", &json!(["reboot"])))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(matches!(control.calls().as_slice(), [Call::Reboot]));

    let resp = app
        .oneshot(post_json("/api/jobs", &json!(["update"])))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(matches!(control.calls().as_slice(), [Call::Reboot, Call::Update]));
}

#[tokio::test]
async fn services_and_server_status() {
    let (app, _c, _s) = build();

    let resp = app.clone().oneshot(get("/api/services")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["biometrics"]["enabled"], false);
    assert!(v["biometrics"]["jobs"]["analyzeSleepLeft"]["status"].is_string());
    assert!(v.get("sentryLogging").is_none());

    let resp = app.oneshot(get("/api/serverStatus")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;

    // podd's real subsystems, not free-sleep's Node internals.
    for key in ["api", "clock", "coverControl", "mqtt", "sensor"] {
        assert!(v[key]["status"].is_string(), "missing key {key}");
        assert!(v[key]["name"].is_string());
        assert!(v[key]["description"].is_string());
    }
    for gone in ["express", "database", "franken", "frankenMonitor", "logger"] {
        assert!(v.get(gone).is_none(), "free-sleep fiction still present: {gone}");
    }

    // Nothing has reported (no podd-core behind this store), so every
    // core-owned subsystem is honestly "not started" — only the API, which
    // just answered, may claim to be healthy.
    assert_eq!(v["api"]["status"], "healthy");
    for key in ["clock", "coverControl", "mqtt", "sensor"] {
        assert_eq!(v[key]["status"], "not_started", "unreported {key} should be not_started");
        assert!(v[key]["timestamp"].is_null(), "unreported {key} should have no timestamp");
    }
}

#[tokio::test]
async fn server_status_reflects_the_health_registry() {
    use podd_core::health::{Health, HealthMap, Subsystem};

    let (app, _c, store) = build();

    let mut health = HealthMap::new();
    health.insert(
        podd_core::health::SENSOR.to_string(),
        Subsystem {
            health: Health::Retrying,
            message: "Sensor not responding; retrying in 10s".to_string(),
            since: jiff::Timestamp::now(),
        },
    );
    health.insert(
        podd_core::health::MQTT.to_string(),
        Subsystem {
            health: Health::Healthy,
            message: "connected to broker".to_string(),
            since: jiff::Timestamp::now(),
        },
    );
    store.set_health(health);

    let resp = app.oneshot(get("/api/serverStatus")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;

    assert_eq!(v["sensor"]["status"], "retrying");
    assert_eq!(v["sensor"]["message"], "Sensor not responding; retrying in 10s");
    assert!(v["sensor"]["timestamp"].is_string());
    assert_eq!(v["mqtt"]["status"], "healthy");
    // Subsystems the registry never mentioned stay "not started".
    assert_eq!(v["coverControl"]["status"], "not_started");
}

#[tokio::test]
async fn server_status_follows_the_health_watch() {
    use podd_core::health::{Health, HealthRegistry};

    let (registry, rx) = HealthRegistry::new();
    let store = Arc::new(StateStore::in_memory());
    store.spawn_health_updater(rx);
    let control = Arc::new(MockControl::new());
    let app = router(store.clone(), control as Arc<dyn PodControl>, None);

    let resp = app.clone().oneshot(get("/api/serverStatus")).await.unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["sensor"]["status"], "not_started");

    registry.report(podd_core::health::SENSOR, Health::Failed, "MCU wedged");
    // let the updater task run
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resp = app.oneshot(get("/api/serverStatus")).await.unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["sensor"]["status"], "failed");
    assert_eq!(v["sensor"]["message"], "MCU wedged");
}

#[tokio::test]
async fn presence_round_trip() {
    let (app, _c, _s) = build();

    let resp = app.clone().oneshot(get("/api/metrics/presence")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["left"]["present"], false);

    let resp = app
        .oneshot(post_json("/api/metrics/presence", &json!({ "left": { "present": true } })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["left"]["present"], true);
    assert!(v["left"]["lastUpdatedAt"].is_string());
    assert_eq!(v["right"]["present"], false);
}

#[tokio::test]
async fn execute_invalid_command() {
    let (app, _c, _s) = build();
    let resp = app
        .oneshot(post_json("/api/execute", &json!({ "command": "definitely-not-real" })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"Invalid command");
}

#[tokio::test]
async fn unknown_api_route_404() {
    let (app, _c, _s) = build();
    let resp = app.oneshot(get("/api/foo")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let v = body_json(resp).await;
    assert_eq!(v, json!({ "error": { "message": "Not Found" } }));
}

// ---------------------------------------------------------------------------
// state-bus wiring: live watch -> StateStore -> GET /deviceStatus
// ---------------------------------------------------------------------------

#[tokio::test]
async fn device_status_reflects_watch_snapshot() {
    use podd_core::bus::DeviceSnapshot;

    let mut snap = DeviceSnapshot::default();
    snap.left.current_temp_c = Some(30.0); // 86 °F
    snap.left.target_temp_c = Some(25.0); // 77 °F
    snap.left.is_on = true;
    snap.water_level = false;
    snap.is_priming = true;
    snap.presence_left = true;
    snap.cover_version = "Pod 4".to_string();

    let (tx, rx) = tokio::sync::watch::channel(snap);
    let store = StateStore::from_watch(rx, StoreConfig::default());
    let control = Arc::new(MockControl::new());
    let app = router(store.clone(), control as Arc<dyn PodControl>, None);

    let resp = app
        .clone()
        .oneshot(get("/api/deviceStatus"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["waterLevel"], "false");
    assert_eq!(v["isPriming"], true);
    assert_eq!(v["coverVersion"], "Pod 4");
    assert_eq!(v["left"]["isOn"], true);
    assert_eq!(v["left"]["targetTemperatureF"], 77);
    assert!((v["left"]["currentTemperatureF"].as_f64().unwrap() - 86.0).abs() < 0.5);

    // presence also tracks the snapshot
    let resp = app.oneshot(get("/api/metrics/presence")).await.unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["left"]["present"], true);
    assert_eq!(v["right"]["present"], false);

    drop(tx); // keep the sender alive until here
}

#[tokio::test]
async fn off_side_target_stays_in_the_ui_range() {
    use podd_core::bus::DeviceSnapshot;

    // An off side publishes no target (podd-core never surfaces the firmware's
    // `temp: 0` off sentinel). The wire value must stay inside the UI's
    // 55–110 contract instead of collapsing to 32 °F.
    let (tx, rx) = tokio::sync::watch::channel(DeviceSnapshot::default());
    let store = StateStore::from_watch(rx, StoreConfig::default());
    let control = Arc::new(MockControl::new());
    let app = router(store.clone(), control as Arc<dyn PodControl>, None);

    let resp = app.oneshot(get("/api/deviceStatus")).await.unwrap();
    let v = body_json(resp).await;
    for side in ["left", "right"] {
        let t = v[side]["targetTemperatureF"].as_f64().unwrap();
        assert!((55.0..=110.0).contains(&t), "{side} target out of range: {t}");
        assert_eq!(v[side]["isOn"], false);
    }

    drop(tx);
}

#[tokio::test]
async fn device_status_updates_on_watch_change() {
    use podd_core::bus::DeviceSnapshot;
    use std::time::Duration;

    let (tx, rx) = tokio::sync::watch::channel(DeviceSnapshot::default());
    let store = StateStore::from_watch(rx, StoreConfig::default());

    // Seed is reflected immediately (no priming yet).
    assert!(!store.device_status().is_priming);

    // Push an update and let the background updater task apply it.
    tx.send_modify(|s| {
        s.is_priming = true;
        s.right.is_on = true;
    });
    let mut applied = false;
    for _ in 0..50 {
        if store.device_status().is_priming {
            applied = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(applied, "watch update was not applied to the store");
    assert!(store.device_status().right.is_on);
}

// ---------------------------------------------------------------------------
// command wiring: PoddControl -> mpsc Command
// ---------------------------------------------------------------------------

#[tokio::test]
async fn poddcontrol_maps_calls_to_commands() {
    use api::wire::{AlarmJob, Side, VibrationPattern};
    use podd_core::bus::{AlarmSpec, Command};
    use pod_proto::packet::BedSide;
    use pod_proto::sensor::command::AlarmPattern;

    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let control = api::PoddControl::new(tx);

    control.set_target_temp(Side::Left, 72).await.unwrap();
    assert_eq!(
        rx.recv().await.unwrap(),
        Command::SetTargetTempF {
            side: BedSide::Left,
            f: 72
        }
    );

    control.set_power(Side::Right, true).await.unwrap();
    assert_eq!(
        rx.recv().await.unwrap(),
        Command::SetPower {
            side: BedSide::Right,
            on: true,
            duration_s: 43200
        }
    );

    control.set_power(Side::Right, false).await.unwrap();
    assert_eq!(
        rx.recv().await.unwrap(),
        Command::SetPower {
            side: BedSide::Right,
            on: false,
            duration_s: 0
        }
    );

    control.prime().await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), Command::Prime);

    control
        .set_prime_daily(false, jiff::civil::time(3, 30, 0, 0))
        .await
        .unwrap();
    assert_eq!(
        rx.recv().await.unwrap(),
        Command::SetPrimeDaily {
            enabled: false,
            time: jiff::civil::time(3, 30, 0, 0),
        }
    );

    control.clear_alarm(Side::Left).await.unwrap();
    assert_eq!(
        rx.recv().await.unwrap(),
        Command::ClearAlarm {
            side: BedSide::Left
        }
    );

    control
        .fire_alarm(AlarmJob {
            vibration_intensity: 80,
            vibration_pattern: VibrationPattern::Double,
            duration: 30,
            side: Side::Right,
            force: None,
        })
        .await
        .unwrap();
    assert_eq!(
        rx.recv().await.unwrap(),
        Command::FireAlarm(AlarmSpec {
            side: BedSide::Right,
            intensity: 80,
            duration_s: 30,
            pattern: AlarmPattern::Double,
        })
    );

    // Reboot / execute / settings are not wired to the hardware yet; they must
    // fail with NotImplemented instead of queueing into the void (#32).
    let err = control.reboot().await.unwrap_err();
    assert!(err.downcast_ref::<api::NotImplemented>().is_some());
    let err = control.execute("reboot", None).await.unwrap_err();
    assert!(err.downcast_ref::<api::NotImplemented>().is_some());
    let err = control
        .apply_device_settings(json!({"ledBrightness": 50}))
        .await
        .unwrap_err();
    assert!(err.downcast_ref::<api::NotImplemented>().is_some());
}

#[tokio::test]
async fn post_device_status_through_poddcontrol_reaches_channel() {
    use podd_core::bus::Command;
    use pod_proto::packet::BedSide;

    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let control = Arc::new(api::PoddControl::new(tx)) as Arc<dyn PodControl>;
    let store = Arc::new(StateStore::in_memory());
    let app = router(store, control, None);

    let resp = app
        .oneshot(post_json(
            "/api/deviceStatus",
            &json!({ "left": { "targetTemperatureF": 68 } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        rx.recv().await.unwrap(),
        Command::SetTargetTempF {
            side: BedSide::Left,
            f: 68
        }
    );
}

#[tokio::test]
async fn jobs_reboot_through_poddcontrol_is_501() {
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let control = Arc::new(api::PoddControl::new(tx)) as Arc<dyn PodControl>;
    let store = Arc::new(StateStore::in_memory());
    let app = router(store, control, None);

    let resp = app
        .oneshot(post_json("/api/jobs", &json!(["reboot"])))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn post_device_status_with_dead_control_core_is_500() {
    // The command mpsc closing means the control core died (#33): the handler
    // must not answer 204 as if the change was applied.
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    drop(rx);
    let control = Arc::new(api::PoddControl::new(tx)) as Arc<dyn PodControl>;
    let store = Arc::new(StateStore::in_memory());
    let app = router(store, control, None);

    let resp = app
        .oneshot(post_json(
            "/api/deviceStatus",
            &json!({ "left": { "isOn": true } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn malformed_json_400() {
    let (app, _c, _s) = build();
    let req = Request::builder()
        .method("POST")
        .uri("/api/deviceStatus")
        .header("content-type", "application/json")
        .body(Body::from("{ this is not json"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v, json!({ "error": { "message": "Invalid JSON" } }));
}

// ---------------------------------------------------------------------------
// logs (journald-backed)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_logs_lists_journald_sources() {
    let (app, _c, _s) = build();
    let resp = app.oneshot(get("/api/logs")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    // podd has no log file — these are systemd units plus the whole-journal
    // pseudo-entry. The UI feeds these names straight back to /api/logs/<name>.
    assert_eq!(
        v,
        json!({ "logs": ["podd", "podd-wifi-setup", "NetworkManager", "system"] })
    );
}

#[tokio::test]
async fn get_log_stream_rejects_unknown_source() {
    // The name becomes a journalctl argument, so anything off the whitelist —
    // including the old fake "podd.log" and flag/path-shaped names — must 404
    // instead of reaching the subprocess.
    for name in ["podd.log", "bogus", "-u", "..%2F..%2Fetc"] {
        let (app, _c, _s) = build();
        let resp = app.oneshot(get(&format!("/api/logs/{name}"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "name={name}");
        let v = body_json(resp).await;
        assert_eq!(v, json!({ "error": { "message": "Not Found" } }));
    }
}

#[tokio::test]
async fn get_log_stream_whitelisted_source_is_sse() {
    // Holds both on a systemd host (real tail) and without journalctl (single
    // fallback message): either way the response is an SSE stream. Dropping it
    // at the end of the loop is what exercises kill_on_drop.
    for name in ["podd", "system"] {
        let (app, _c, _s) = build();
        let resp = app.oneshot(get(&format!("/api/logs/{name}"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "name={name}");
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(ct.starts_with("text/event-stream"), "name={name} ct={ct}");
    }
}
