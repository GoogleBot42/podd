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

    // a partial patch (time inherited from the stored settings) still applies;
    // every save also hands the daemon the whole document first (#106)
    let patch = json!({ "primePodDaily": { "enabled": true } });
    let resp = app.clone().oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    match control.calls().as_slice() {
        [Call::SetSettings(_), Call::SetPrimeDaily(true, t)] => {
            assert_eq!(t.to_string(), "14:00:00"); // the default settings time
        }
        other => panic!("expected SetSettings + SetPrimeDaily, got {other:?}"),
    }

    // turning it off, with a new time, propagates both fields
    let patch = json!({ "primePodDaily": { "enabled": false, "time": "03:30" } });
    let resp = app.clone().oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    match control.calls().as_slice() {
        [_, _, Call::SetSettings(_), Call::SetPrimeDaily(false, t)] => {
            assert_eq!(t.to_string(), "03:30:00")
        }
        other => panic!("expected a second SetPrimeDaily, got {other:?}"),
    }
}

#[tokio::test]
async fn settings_without_bridged_fields_push_only_the_document() {
    let (app, control, _s) = build();
    let patch = json!({ "rebootDaily": false, "left": { "name": "Bedroom" } });
    let resp = app.oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // The daemon must see the new document (rebootDaily drives its reboot
    // scheduler), but none of the config.ron bridges may fire.
    match control.calls().as_slice() {
        [Call::SetSettings(s)] => {
            assert!(!s.reboot_daily);
            assert_eq!(s.left.name, "Bedroom");
        }
        other => panic!("expected exactly one SetSettings, got {other:?}"),
    }
}

#[tokio::test]
async fn settings_away_mode_reaches_the_config() {
    let (app, control, _s) = build();

    // one side away: the command carries both sides' merged state
    let patch = json!({ "left": { "awayMode": true } });
    let resp = app.clone().oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    match control.calls().as_slice() {
        [Call::SetSettings(_), Call::SetAwayMode(true, false)] => {}
        other => panic!("expected SetAwayMode(true, false), got {other:?}"),
    }

    // the other side follows (left inherited from the stored settings)
    let patch = json!({ "right": { "awayMode": true } });
    let resp = app.clone().oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    match control.calls().as_slice() {
        [_, _, Call::SetSettings(_), Call::SetAwayMode(true, true)] => {}
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
        [Call::SetSettings(_), Call::SetTimezone(tz)] => assert_eq!(tz, "America/Denver"),
        other => panic!("expected SetSettings + SetTimezone, got {other:?}"),
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
async fn settings_accept_a_schedule_override_and_push_it_to_the_daemon() {
    let (app, control, store) = build();
    let patch = json!({
        "left": { "scheduleOverrides": { "alarm": {
            "disabled": false,
            "timeOverride": "06:30",
            "expiresAt": "2026-08-18T06:32:00-06:00"
        } } }
    });
    let resp = app.oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        store.settings().left.schedule_overrides.alarm.time_override,
        "06:30"
    );
    match control.calls().as_slice() {
        [Call::SetSettings(s)] => {
            assert_eq!(s.left.schedule_overrides.alarm.time_override, "06:30");
        }
        other => panic!("expected exactly one SetSettings, got {other:?}"),
    }
}

#[tokio::test]
async fn settings_reject_bad_override_fields_without_applying_anything() {
    let (app, control, store) = build();
    let patch = json!({
        "left": { "scheduleOverrides": {
            "alarm": { "timeOverride": "25:99", "expiresAt": "not-a-time" },
            "temperatureSchedules": { "disabled": true, "expiresAt": "later" }
        } }
    });
    let resp = app.oneshot(post_json("/api/settings", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"], "Invalid request data");
    // three distinct offenses, each named
    assert_eq!(v["details"].as_array().unwrap().len(), 3);
    assert_eq!(
        store.settings().left.schedule_overrides.alarm.time_override,
        ""
    );
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

// ---------------------------------------------------------------------------
// schedules -> daemon bridge + validation (#6, #106)
// ---------------------------------------------------------------------------

/// A save has to reach the control core, not just the file: schedules.json is
/// read by podd-core only at startup.
#[tokio::test]
async fn schedules_save_reaches_the_daemon_and_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let schedules_path = dir.path().join("schedules.json");
    let store = Arc::new(StateStore::new(StoreConfig {
        settings_path: None,
        schedules_path: Some(schedules_path.clone()),
    }));
    let control = Arc::new(MockControl::new());
    let app = router(store.clone(), control.clone() as Arc<dyn PodControl>, None);

    let patch = json!({
        "left": { "monday": { "power": { "enabled": true, "onTemperature": 77 } } }
    });
    let resp = app.oneshot(post_json("/api/schedules", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // the daemon gets the whole merged document, not the patch
    match control.calls().as_slice() {
        [Call::SetSchedules(s)] => {
            assert!(s.left.monday.power.enabled);
            assert_eq!(s.left.monday.power.on_temperature, 77);
            // untouched days come along, still disabled
            assert!(!s.left.tuesday.power.enabled);
            assert!(!s.right.monday.power.enabled);
        }
        other => panic!("expected one SetSchedules, got {other:?}"),
    }

    // and it survives a reload from disk
    let reloaded = StateStore::new(StoreConfig {
        settings_path: None,
        schedules_path: Some(schedules_path),
    });
    assert!(reloaded.schedules().left.monday.power.enabled);
}

/// Every rejection must leave the stored document *and* the daemon untouched.
async fn assert_schedules_rejected(patch: Value, expect_detail: &str) {
    let (app, control, store) = build();
    let resp = app.oneshot(post_json("/api/schedules", &patch)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "patch: {patch}");
    let v = body_json(resp).await;
    assert_eq!(v["error"], "Invalid request data");
    let details = v["details"].as_array().expect("details list").clone();
    assert!(
        details.iter().any(|d| d.as_str().unwrap().contains(expect_detail)),
        "expected a detail mentioning {expect_detail:?}, got {details:?}"
    );
    assert_eq!(store.schedules(), api::wire::Schedules::default());
    assert!(control.calls().is_empty(), "{:?}", control.calls());
}

#[tokio::test]
async fn schedules_reject_a_bad_temperature_key() {
    assert_schedules_rejected(
        json!({ "left": { "monday": { "temperatures": { "99:99": 72 } } } }),
        "temperatures key \"99:99\" is not HH:mm",
    )
    .await;
}

#[tokio::test]
async fn schedules_reject_an_out_of_range_temperature() {
    assert_schedules_rejected(
        json!({ "left": { "monday": { "temperatures": { "07:00": 140 } } } }),
        "must be 55-110 °F, got 140",
    )
    .await;
    assert_schedules_rejected(
        json!({ "right": { "friday": { "power": { "onTemperature": 40 } } } }),
        "right.friday.power.onTemperature",
    )
    .await;
}

#[tokio::test]
async fn schedules_reject_a_zero_length_window() {
    // on == off is not "24 hours", it's an unresolvable window.
    assert_schedules_rejected(
        json!({ "left": { "monday": { "power": { "on": "21:00", "off": "21:00" } } } }),
        "power.on and power.off must differ",
    )
    .await;
    assert_schedules_rejected(
        json!({ "left": { "monday": { "power": { "on": "9pm" } } } }),
        "power.on is not HH:mm",
    )
    .await;
}

#[tokio::test]
async fn schedules_reject_bad_alarm_fields_even_though_they_are_inert() {
    assert_schedules_rejected(
        json!({ "left": { "monday": { "alarm": { "vibrationIntensity": 0 } } } }),
        "alarm.vibrationIntensity must be 1-100",
    )
    .await;
    assert_schedules_rejected(
        json!({ "left": { "monday": { "alarm": { "duration": 6000 } } } }),
        "alarm.duration must be 0-600",
    )
    .await;
    assert_schedules_rejected(
        json!({ "left": { "monday": { "alarm": { "time": "25:00" } } } }),
        "alarm.time is not HH:mm",
    )
    .await;
}

/// These three used to be silently dropped by the merge and answer 200 having
/// changed nothing (#106).
#[tokio::test]
async fn schedules_reject_an_unknown_day_key() {
    assert_schedules_rejected(
        json!({ "left": { "mondey": { "power": { "enabled": true } } } }),
        "unknown day key \"mondey\"",
    )
    .await;
}

#[tokio::test]
async fn schedules_reject_an_unknown_side_key() {
    assert_schedules_rejected(
        json!({ "middle": { "monday": { "power": { "enabled": true } } } }),
        "unknown side key \"middle\"",
    )
    .await;
}

#[tokio::test]
async fn schedules_reject_a_non_object_day_value() {
    assert_schedules_rejected(
        json!({ "left": { "monday": true } }),
        "left.monday must be an object",
    )
    .await;
    assert_schedules_rejected(json!({ "left": [] }), "left must be an object").await;
    assert_schedules_rejected(json!("nope"), "body must be an object").await;
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
async fn biometrics_jobs_are_501() {
    // #107: these used to hit a catch-all Ok(()) and answer 204.
    for job in [
        "analyzeSleepLeft",
        "analyzeSleepRight",
        "biometricsCalibrationLeft",
        "biometricsCalibrationRight",
    ] {
        let (app, control, _s) = build();
        let resp = app
            .oneshot(post_json("/api/jobs", &json!([job])))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED, "job {job}");
        assert!(control.calls().is_empty(), "job {job} reached control");
    }
}

#[tokio::test]
async fn mixed_job_batch_is_rejected_before_anything_runs() {
    let (app, control, _s) = build();
    let resp = app
        .oneshot(post_json("/api/jobs", &json!(["reboot", "analyzeSleepLeft"])))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    // the supported job in the batch must not have half-applied
    assert!(control.calls().is_empty());
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

    // #107: the biometrics stack does not exist, so no job may claim health.
    // The wire shape stays intact (the SPA's zod schema is `.strict()`).
    for job in [
        "analyzeSleepLeft",
        "analyzeSleepRight",
        "installation",
        "stream",
        "calibrateLeft",
        "calibrateRight",
    ] {
        let info = &v["biometrics"]["jobs"][job];
        assert_eq!(info["status"], "not_started", "job {job}");
        assert_eq!(info["message"], "not implemented in podd", "job {job}");
        assert!(info["name"].is_string(), "job {job}");
        assert!(info["description"].is_string(), "job {job}");
    }

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
async fn post_services_is_501_and_changes_nothing() {
    // #107: this used to merge into a fresh default and echo the patch back
    // while persisting nothing, so the next GET silently reverted.
    let (app, _c, _s) = build();
    let resp = app
        .clone()
        .oneshot(post_json(
            "/api/services",
            &json!({ "biometrics": { "enabled": true } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);

    let resp = app.oneshot(get("/api/services")).await.unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["biometrics"]["enabled"], false);
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

// ---------------------------------------------------------------------------
// metrics query params (#108)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_endpoints_accept_the_ui_query_params() {
    let (app, _c, _s) = build();
    // Exactly the shape ui/src/api/{sleep,vitals,movement}.ts builds.
    let query =
        "startTime=2026-08-15T00%3A00%3A00.000Z&endTime=2026-08-22T00%3A00%3A00.000Z&side=left";
    for path in [
        "/api/metrics/sleep",
        "/api/metrics/vitals",
        "/api/metrics/movement",
    ] {
        let resp = app
            .clone()
            .oneshot(get(&format!("{path}?{query}")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        // Biometrics pipeline is deferred (#12): filtered result of no records.
        assert_eq!(body_json(resp).await, json!([]), "{path}");
    }

    let resp = app
        .clone()
        .oneshot(get(&format!("/api/metrics/vitals/summary?{query}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["avgHeartRate"], 0);
}

#[tokio::test]
async fn metrics_endpoints_reject_bad_query_params() {
    let (app, _c, _s) = build();

    let resp = app
        .clone()
        .oneshot(get("/api/metrics/vitals?side=middle"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"], "Invalid request data");
    assert!(v["details"][0].as_str().unwrap().contains("side"));

    let resp = app
        .oneshot(get("/api/metrics/sleep?startTime=yesterday"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert!(v["details"][0].as_str().unwrap().contains("startTime"));
}

#[tokio::test]
async fn metrics_endpoints_work_without_query_params() {
    let (app, _c, _s) = build();
    let resp = app.oneshot(get("/api/metrics/movement")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await, json!([]));
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

    control.reboot().await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), Command::Reboot);

    // Execute is not wired to the hardware yet; it must fail with
    // NotImplemented instead of queueing into the void (#32).
    let err = control.execute("reboot", None).await.unwrap_err();
    assert!(err.downcast_ref::<api::NotImplemented>().is_some());

    // Device settings: `ledBrightness` is appliable (#10) — clamped to
    // 0–100 — while a block without it still fails honestly (#32).
    control
        .apply_device_settings(json!({"ledBrightness": 50, "gainLeft": 400}))
        .await
        .unwrap();
    assert_eq!(rx.recv().await.unwrap(), Command::SetLedBrightness(50));
    control
        .apply_device_settings(json!({"ledBrightness": 900}))
        .await
        .unwrap();
    assert_eq!(rx.recv().await.unwrap(), Command::SetLedBrightness(100));
    let err = control
        .apply_device_settings(json!({"gainLeft": 400}))
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
async fn jobs_reboot_through_poddcontrol_queues_a_reboot() {
    use podd_core::bus::Command;

    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let control = Arc::new(api::PoddControl::new(tx)) as Arc<dyn PodControl>;
    let store = Arc::new(StateStore::in_memory());
    let app = router(store, control, None);

    let resp = app
        .oneshot(post_json("/api/jobs", &json!(["reboot"])))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(rx.recv().await.unwrap(), Command::Reboot);
}

#[tokio::test]
async fn jobs_update_through_poddcontrol_is_501_and_queues_nothing() {
    // #107: the update job used to queue a Command::Update that podd-core's
    // dispatcher only logs, and answer 204.
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let control = Arc::new(api::PoddControl::new(tx)) as Arc<dyn PodControl>;
    let store = Arc::new(StateStore::in_memory());
    let app = router(store, control, None);

    let resp = app
        .oneshot(post_json("/api/jobs", &json!(["update"])))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    assert!(rx.try_recv().is_err(), "update must not queue a command");
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

#[tokio::test]
async fn vitals_endpoints_serve_the_store() {
    use podd_core::biometrics::{VitalsRecord as StoreRecord, VitalsStore};

    let dir = std::env::temp_dir().join(format!("podd-vitals-http-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let vitals = Arc::new(VitalsStore::open(dir.join("vitals.jsonl")).unwrap());
    for (side, ts, hr) in [
        (pod_proto::packet::BedSide::Left, 100i64, 60i64),
        (pod_proto::packet::BedSide::Right, 200, 70),
        (pod_proto::packet::BedSide::Left, 300, 64),
    ] {
        vitals
            .append(&StoreRecord {
                side,
                timestamp: ts,
                heart_rate: hr,
                hrv: 40,
                breathing_rate: 14,
            })
            .unwrap();
    }

    let store = Arc::new(StateStore::in_memory());
    let control = Arc::new(MockControl::new());
    let app = api::router_with_biometrics(
        store,
        control as Arc<dyn PodControl>,
        None,
        podd_core::biometrics::Stores {
            vitals: Some(vitals),
            ..Default::default()
        },
    );

    // side filter + window filter both apply (timestamps are epoch seconds;
    // the query params are ISO-8601)
    let resp = app
        .clone()
        .oneshot(get("/api/metrics/vitals?side=left"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let records = v.as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["side"], "left");
    assert_eq!(records[0]["heart_rate"], 60);

    let resp = app
        .clone()
        .oneshot(get(
            "/api/metrics/vitals?startTime=1970-01-01T00:02:30Z&endTime=1970-01-01T00:06:00Z",
        ))
        .await
        .unwrap();
    let v = body_json(resp).await;
    // window is 150s..360s -> ts=200 and ts=300, not ts=100
    assert_eq!(v.as_array().unwrap().len(), 2);

    let resp = app
        .oneshot(get("/api/metrics/vitals/summary?side=left"))
        .await
        .unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["avgHeartRate"], 62);
    assert_eq!(v["minHeartRate"], 60);
    assert_eq!(v["maxHeartRate"], 64);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sleep_and_movement_endpoints_serve_the_stores() {
    use pod_proto::packet::BedSide;
    use podd_core::biometrics::{MovementRecord, MovementStore, SleepRecord, SleepStore};

    let dir = std::env::temp_dir().join(format!("podd-sleep-http-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let sleep = Arc::new(SleepStore::open(dir.join("sleep.jsonl")).unwrap());
    let movement = Arc::new(MovementStore::open(dir.join("movement.jsonl")).unwrap());

    // 2026-08-20T02:00:00Z -> 2026-08-20T10:00:00Z, one 10-minute exit
    let entered = 1_787_191_200i64;
    let left = entered + 8 * 3600;
    let record = SleepRecord {
        id: SleepRecord::make_id(entered, BedSide::Left),
        side: BedSide::Left,
        entered_bed_at: entered,
        left_bed_at: left,
        sleep_period_seconds: 8 * 3600 - 600,
        times_exited_bed: 1,
        present_intervals: vec![(entered, entered + 3600), (entered + 4200, left)],
        not_present_intervals: vec![(entered + 3600, entered + 4200)],
    };
    let id = record.id;
    sleep.append(&record).unwrap();
    // a second night on the other side, a week later
    sleep
        .append(&SleepRecord {
            id: SleepRecord::make_id(entered + 7 * 86400, BedSide::Right),
            side: BedSide::Right,
            entered_bed_at: entered + 7 * 86400,
            left_bed_at: left + 7 * 86400,
            sleep_period_seconds: 8 * 3600,
            times_exited_bed: 0,
            present_intervals: vec![],
            not_present_intervals: vec![],
        })
        .unwrap();
    for (ts, mv) in [(entered, 40i64), (entered + 120, 900)] {
        movement
            .append(&MovementRecord {
                side: BedSide::Left,
                timestamp: ts,
                total_movement: mv,
            })
            .unwrap();
    }

    let store = Arc::new(StateStore::in_memory());
    let control = Arc::new(MockControl::new());
    let app = api::router_with_biometrics(
        store,
        control as Arc<dyn PodControl>,
        None,
        podd_core::biometrics::Stores {
            sleep: Some(sleep),
            movement: Some(movement),
            ..Default::default()
        },
    );

    // GET: epoch seconds become the ISO-8601 strings the UI's zod schema wants
    let resp = app
        .clone()
        .oneshot(get("/api/metrics/sleep?side=left"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let records = v.as_array().unwrap();
    assert_eq!(records.len(), 1, "the right-side night must be filtered out");
    assert_eq!(records[0]["side"], "left");
    assert_eq!(records[0]["entered_bed_at"], "2026-08-20T02:00:00Z");
    assert_eq!(records[0]["left_bed_at"], "2026-08-20T10:00:00Z");
    assert_eq!(records[0]["times_exited_bed"], 1);
    assert_eq!(records[0]["present_intervals"].as_array().unwrap().len(), 2);
    assert_eq!(records[0]["id"], id);

    // the week window filters on entered_bed_at
    let resp = app
        .clone()
        .oneshot(get(
            "/api/metrics/sleep?startTime=2026-08-19T00:00:00Z&endTime=2026-08-21T00:00:00Z",
        ))
        .await
        .unwrap();
    assert_eq!(body_json(resp).await.as_array().unwrap().len(), 1);

    // movement records keep epoch seconds and gain a UI-facing id
    let resp = app
        .clone()
        .oneshot(get("/api/metrics/movement?side=left"))
        .await
        .unwrap();
    let v = body_json(resp).await;
    let records = v.as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["timestamp"], entered);
    assert_eq!(records[1]["total_movement"], 900);
    assert_ne!(records[0]["id"], records[1]["id"]);

    // PUT: correcting the bed times reclips the intervals
    let body = json!({ "entered_bed_at": "2026-08-20T04:00:00Z" });
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/metrics/sleep/{id}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["entered_bed_at"], "2026-08-20T04:00:00Z");
    // only the second presence interval survives, clipped to the new start,
    // and the exit that fell outside the window is no longer counted
    assert_eq!(v["present_intervals"].as_array().unwrap().len(), 1);
    assert_eq!(v["sleep_period_seconds"], left - (entered + 2 * 3600));
    assert_eq!(v["times_exited_bed"], 0);

    // DELETE: gone, and a second delete 404s
    let del = |id: i64| {
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/metrics/sleep/{id}"))
            .body(Body::empty())
            .unwrap()
    };
    let resp = app.clone().oneshot(del(id)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = app.clone().oneshot(del(id)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = app
        .oneshot(get("/api/metrics/sleep?side=left"))
        .await
        .unwrap();
    assert!(body_json(resp).await.as_array().unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Without stores (the `router()` construction the other tests use) the
/// biometrics endpoints must still answer with well-formed empties.
#[tokio::test]
async fn metrics_endpoints_are_empty_without_stores() {
    for path in [
        "/api/metrics/sleep",
        "/api/metrics/vitals",
        "/api/metrics/movement",
    ] {
        let (app, _c, _s) = build();
        let resp = app.oneshot(get(path)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "path={path}");
        assert!(body_json(resp).await.as_array().unwrap().is_empty());
    }
}
// ---------------------------------------------------------------------------
// updates (REPLACEMENT_PLAN §9 observability; issue #1)
// ---------------------------------------------------------------------------

fn build_with_updates(updates: Arc<api::MockUpdates>) -> axum::Router {
    let store = Arc::new(StateStore::in_memory());
    let control = Arc::new(MockControl::new());
    api::router_full(
        store,
        control as Arc<dyn PodControl>,
        None,
        Default::default(),
        Some(updates as Arc<dyn api::UpdateOps>),
    )
}

/// Without an agent the surface still reports the daemon's build stamp, and
/// says "no updater" instead of implying everything is up to date.
#[tokio::test]
async fn updates_without_an_agent() {
    let (app, _c, _s) = build();
    let resp = app.clone().oneshot(get("/api/updates")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["daemon"]["version"], json!(podd_core::VERSION));
    assert_eq!(v["daemon"]["rev"], json!(podd_core::GIT_REV));
    assert!(v["updater"].is_null());

    for path in ["/api/updates/check", "/api/updates/rollback"] {
        let resp = app
            .clone()
            .oneshot(post_json(path, &json!({})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }
}

#[tokio::test]
async fn updates_report_the_agent_status_in_camel_case() {
    let mut status = api::UpdateStatus::new(true, "stable".into(), "manual".into());
    status.last_check_unix = Some(1_700_000_000);
    status.last_check_ok = true;
    status.last_applied = Some("app -> 0.2.0 (committed)".into());
    let app = build_with_updates(Arc::new(api::MockUpdates::new(status)));

    let v = body_json(app.oneshot(get("/api/updates")).await.unwrap()).await;
    assert_eq!(v["updater"]["enabled"], json!(true));
    assert_eq!(v["updater"]["channel"], json!("stable"));
    assert_eq!(v["updater"]["mode"], json!("manual"));
    assert_eq!(v["updater"]["lastCheckUnix"], json!(1_700_000_000_i64));
    assert_eq!(v["updater"]["lastCheckOk"], json!(true));
    assert!(v["updater"]["currentVersions"].is_array());
    assert!(v["updater"]["available"].is_array());
    assert!(v["updater"]["lastError"].is_null());
    assert!(v["updater"]["lastApplied"].is_string());
}

#[tokio::test]
async fn check_now_and_rollback_reach_the_agent() {
    let updates = Arc::new(api::MockUpdates::default());
    let app = build_with_updates(updates.clone());

    let resp = app
        .clone()
        .oneshot(post_json("/api/updates/check", &json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // the refreshed status comes back, so the UI needs no second round-trip
    assert_eq!(body_json(resp).await["lastCheckOk"], json!(true));

    let resp = app
        .oneshot(post_json("/api/updates/rollback", &json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["restored"], json!("0.0.1"));

    assert_eq!(updates.calls(), vec!["check", "rollback"]);
}

/// A failed check must not read as success (an unreachable channel is not
/// "up to date"), and a rollback with nothing to roll back to must say so.
#[tokio::test]
async fn update_action_failures_are_surfaced() {
    let updates = Arc::new(api::MockUpdates::default().failing("no sources configured"));
    let app = build_with_updates(updates);

    let resp = app
        .clone()
        .oneshot(post_json("/api/updates/check", &json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);

    let resp = app
        .oneshot(post_json("/api/updates/rollback", &json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// mqtt (issue #18)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mqtt_get_reports_the_mirrored_settings_without_the_password() {
    let (app, _c, store) = build();

    // API-only default: nothing configured, and no password field at all.
    let resp = app.clone().oneshot(get("/api/mqtt")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["enabled"], json!(false));
    assert_eq!(v["server"], json!(""));
    assert_eq!(v["port"], json!(1883));
    assert_eq!(v["passwordSet"], json!(false));
    assert!(
        v.get("password").is_none(),
        "the API must never echo the password"
    );

    // What podd-core's mirror hands us shows up verbatim (minus the secret).
    store.set_mqtt(api::wire::MqttSettings {
        enabled: true,
        server: "broker.lan".to_string(),
        port: 8883,
        user: "podd".to_string(),
        password_set: true,
    });
    let resp = app.oneshot(get("/api/mqtt")).await.unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["server"], json!("broker.lan"));
    assert_eq!(v["port"], json!(8883));
    assert_eq!(v["user"], json!("podd"));
    assert_eq!(v["passwordSet"], json!(true));
    assert!(v.get("password").is_none());
}

#[tokio::test]
async fn mqtt_post_merges_and_reaches_the_config() {
    let (app, control, store) = build();
    store.set_mqtt(api::wire::MqttSettings {
        enabled: true,
        server: "old.lan".to_string(),
        port: 1883,
        user: "podd".to_string(),
        password_set: true,
    });

    // A partial patch: everything untouched is inherited, and an absent
    // password means "keep the stored one" (None on the command).
    let resp = app
        .clone()
        .oneshot(post_json("/api/mqtt", &json!({ "port": 8883 })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["server"], json!("old.lan"));
    assert_eq!(v["port"], json!(8883));
    assert_eq!(v["passwordSet"], json!(true));
    match control.calls().as_slice() {
        [Call::SetMqtt(u)] => {
            assert_eq!(u.server, "old.lan");
            assert_eq!(u.port, 8883);
            assert_eq!(u.user, "podd");
            assert!(u.enabled);
            assert_eq!(u.password, None, "an absent password must not clear it");
        }
        other => panic!("expected one SetMqtt, got {other:?}"),
    }
    // GET reflects the edit straight away (no race with the daemon mirror).
    let v = body_json(app.clone().oneshot(get("/api/mqtt")).await.unwrap()).await;
    assert_eq!(v["port"], json!(8883));

    // A new password is forwarded, and only ever reported as `passwordSet`.
    let patch = json!({ "password": "hunter2", "user": "ha" });
    let resp = app
        .clone()
        .oneshot(post_json("/api/mqtt", &patch))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body =
        String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(!body.contains("hunter2"), "{body}");
    match control.calls().as_slice() {
        [_, Call::SetMqtt(u)] => {
            assert_eq!(u.password.as_deref(), Some("hunter2"));
            assert_eq!(u.user, "ha");
        }
        other => panic!("expected a second SetMqtt, got {other:?}"),
    }
    // ... and the redacting Debug keeps it out of any log line.
    assert!(!format!("{:?}", control.calls()).contains("hunter2"));

    // An explicit empty password clears it.
    let resp = app
        .clone()
        .oneshot(post_json("/api/mqtt", &json!({ "password": "" })))
        .await
        .unwrap();
    let v = body_json(resp).await;
    assert_eq!(v["passwordSet"], json!(false));
}

#[tokio::test]
async fn mqtt_post_rejects_a_useless_broker_without_applying_anything() {
    for patch in [
        json!({ "enabled": true, "server": "" }),
        json!({ "server": "mqtt://broker.lan" }),
        json!({ "server": "broker lan" }),
        json!({ "server": "broker.lan", "port": 0 }),
    ] {
        let (app, control, store) = build();
        store.set_mqtt(api::wire::MqttSettings {
            enabled: true,
            server: "good.lan".to_string(),
            port: 1883,
            user: "podd".to_string(),
            password_set: true,
        });
        let resp = app.oneshot(post_json("/api/mqtt", &patch)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{patch}");
        let v = body_json(resp).await;
        assert_eq!(v["error"], "Invalid request data");
        assert!(
            control.calls().is_empty(),
            "{patch} must not reach the config"
        );
        assert_eq!(store.mqtt().server, "good.lan");
    }
}

#[tokio::test]
async fn mqtt_post_rejects_unknown_fields() {
    let (app, control, _s) = build();
    let resp = app
        .oneshot(post_json("/api/mqtt", &json!({ "brokerHost": "broker.lan" })))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(control.calls().is_empty());
}

/// The document a client GETs must POST back cleanly (`passwordSet` is
/// accepted and ignored, not a schema error).
#[tokio::test]
async fn mqtt_post_accepts_the_document_it_returned() {
    let (app, control, _s) = build();
    let v = body_json(app.clone().oneshot(get("/api/mqtt")).await.unwrap()).await;
    let mut doc = v.clone();
    doc["enabled"] = json!(true);
    doc["server"] = json!("broker.lan");
    let resp = app.oneshot(post_json("/api/mqtt", &doc)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    match control.calls().as_slice() {
        [Call::SetMqtt(u)] => {
            assert!(u.enabled);
            assert_eq!(u.server, "broker.lan");
            assert_eq!(u.password, None);
        }
        other => panic!("expected one SetMqtt, got {other:?}"),
    }
}
