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
    assert_eq!(v["sentryLogging"]["enabled"], false);

    let resp = app.oneshot(get("/api/serverStatus")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    for key in [
        "alarmSchedule", "database", "express", "franken", "frankenMonitor",
        "jobs", "logger", "powerSchedule", "primeSchedule", "rebootSchedule",
        "systemDate", "temperatureSchedule",
    ] {
        assert_eq!(v[key]["status"], "healthy", "missing/unhealthy key {key}");
    }
    // optional biometrics keys omitted
    assert!(v.get("analyzeSleepLeft").is_none());
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

    control.reboot().await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), Command::Reboot);
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
