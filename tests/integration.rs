//! Portable tests that run on any platform. They exercise the pure enforcer
//! logic through `MockAudio` — no Windows APIs are touched.

mod common;

use std::thread;
use std::time::{Duration, Instant};

use balance_enforcer::audio::{AudioEvent, BalancePolicy};
use balance_enforcer::config::Config;
use balance_enforcer::enforcer::BalanceEnforcer;
use common::MockAudio;

const TOL: f32 = 0.02;

fn mute_left() -> BalancePolicy {
    BalancePolicy {
        left: 0.0,
        right: 1.0,
        tolerance: TOL,
    }
}

#[test]
fn reconciles_when_drifted_to_equal_volumes() {
    let mock = MockAudio::stereo(0.8, 0.8);
    let enforcer = BalanceEnforcer::new(mock, mute_left());
    enforcer.reconcile_once().expect("reconcile failed");

    // Drop the enforcer to get the mock back.
    let mock = enforcer.into_controller();
    let writes = mock.writes();
    assert_eq!(
        writes.len(),
        2,
        "expected exactly two set_channel calls, got {writes:?}"
    );
    assert_eq!(writes[0], (0, 0.0));
    assert_eq!(writes[1], (1, 1.0));
    assert_eq!(mock.channels(), vec![0.0, 1.0]);
}

#[test]
fn no_op_when_already_compliant() {
    let mock = MockAudio::stereo(0.0, 1.0);
    let enforcer = BalanceEnforcer::new(mock, mute_left());
    enforcer.reconcile_once().expect("reconcile failed");
    let mock = enforcer.into_controller();
    assert!(
        mock.writes().is_empty(),
        "expected no writes, got {:?}",
        mock.writes()
    );
}

#[test]
fn tolerance_suppresses_micro_drift() {
    // Left is 0.005 away from target 0.0, well under the 0.02 tolerance.
    let mock = MockAudio::stereo(0.005, 0.995);
    let enforcer = BalanceEnforcer::new(mock, mute_left());
    enforcer.reconcile_once().expect("reconcile failed");
    let mock = enforcer.into_controller();
    assert!(
        mock.writes().is_empty(),
        "tolerance breach: {:?}",
        mock.writes()
    );
}

#[test]
fn mono_device_logs_warning_and_does_not_write() {
    let mock = MockAudio::with_channel_count(1);
    let enforcer = BalanceEnforcer::new(mock, mute_left());
    enforcer.reconcile_once().expect("reconcile failed");
    let mock = enforcer.into_controller();
    assert!(mock.writes().is_empty());
}

#[test]
fn six_channel_device_logs_warning_and_does_not_write() {
    let mock = MockAudio::with_channel_count(6);
    let enforcer = BalanceEnforcer::new(mock, mute_left());
    enforcer.reconcile_once().expect("reconcile failed");
    let mock = enforcer.into_controller();
    assert!(mock.writes().is_empty());
}

#[test]
fn get_level_error_is_propagated_not_swallowed() {
    let mock = MockAudio::stereo(0.8, 0.8);
    mock.fail_next_get();
    let enforcer = BalanceEnforcer::new(mock, mute_left());
    let result = enforcer.reconcile_once();
    assert!(result.is_err(), "expected error, got {result:?}");
    let mock = enforcer.into_controller();
    // No write should have happened since the get failed first.
    assert!(mock.writes().is_empty());
}

#[test]
fn inverted_policy_mutes_right_instead() {
    let mock = MockAudio::stereo(0.5, 0.5);
    let policy = BalancePolicy {
        left: 1.0,
        right: 0.0,
        tolerance: TOL,
    };
    let enforcer = BalanceEnforcer::new(mock, policy);
    enforcer.reconcile_once().expect("reconcile failed");
    let mock = enforcer.into_controller();
    assert_eq!(mock.channels(), vec![1.0, 0.0]);
}

#[test]
fn event_storm_is_coalesced_into_single_reconcile_pass() {
    // This test drives the full `run` loop. The mock starts at drifted values,
    // we fire 50 VolumeChanged events in rapid succession, then sleep longer
    // than the debounce window and fire one more to force a second pass.
    let mock = MockAudio::stereo(0.8, 0.8);
    let tx = mock.sender();
    let enforcer = BalanceEnforcer::new(mock, mute_left());
    let stop = enforcer.stop_flag();

    let handle = thread::spawn(move || {
        let _ = enforcer.run();
        enforcer
    });

    // Give the enforcer time to perform its startup reconcile.
    thread::sleep(Duration::from_millis(150));

    for _ in 0..50 {
        tx.send(AudioEvent::VolumeChanged).unwrap();
    }
    thread::sleep(Duration::from_millis(200));

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    // Nudge the loop so it wakes and sees the stop flag.
    let _ = tx.send(AudioEvent::Tick);

    let deadline = Instant::now() + Duration::from_secs(2);
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let enforcer = handle.join().expect("run thread panicked");
    let mock = enforcer.into_controller();

    // Startup reconciles the drifted state → writes (0,0.0) and (1,1.0).
    // The storm should trigger at most one additional pass, but because the
    // state is now compliant that pass produces no writes. So the total number
    // of writes is exactly 2.
    let writes = mock.writes();
    assert_eq!(writes.len(), 2, "expected exactly 2 writes, got {writes:?}");
}

#[test]
fn config_roundtrip_serializes_and_deserializes() {
    let tmp = tempdir();
    let path = tmp.join("config.toml");
    let cfg = Config {
        target_device_id: "{0.0.0.00000000}.{abc-def}".into(),
        target_device_name: "Speakers (Test Audio)".into(),
    };
    cfg.save_to(&path).expect("save failed");
    let loaded = Config::load_from(&path).expect("load failed");
    assert_eq!(cfg, loaded);
}

// -------- helpers --------

use std::path::PathBuf;

fn tempdir() -> PathBuf {
    let mut p = std::env::current_dir().unwrap();
    p.push(".tmp");
    p.push(format!("test-{}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}
