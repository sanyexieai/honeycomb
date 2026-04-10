use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn hc_bin() -> String {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_hc") {
        return path;
    }

    let exe = if cfg!(windows) { "hc.exe" } else { "hc" };
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("debug")
        .join(exe)
        .to_string_lossy()
        .into_owned()
}

fn unique_state_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be valid")
        .as_nanos();
    std::env::temp_dir()
        .join("honeycomb-tests")
        .join(format!("claim-cli-{stamp}"))
        .join("runtime-state.json")
}

fn run_hc(state_path: &PathBuf, args: &[&str]) -> String {
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent).expect("state dir should be created");
    }

    let output = Command::new(hc_bin())
        .args(args)
        .env("HC_RUNTIME_STATE_PATH", state_path)
        .output()
        .expect("hc command should run");

    if !output.status.success() {
        panic!(
            "hc {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn claim_cli_flow_selects_highest_round_winner() {
    let state_path = unique_state_path();

    let _ = run_hc(&state_path, &["reset"]);
    let session_output = run_hc(&state_path, &["session", "create", "demo"]);
    assert!(session_output.contains("created session"));

    let alice = run_hc(&state_path, &["instance", "create", "demo", "alice"]);
    let doctor = run_hc(&state_path, &["instance", "create", "demo", "doctor"]);
    let coder = run_hc(&state_path, &["instance", "create", "demo", "coder"]);
    assert!(alice.contains("alice"));
    assert!(doctor.contains("doctor"));
    assert!(coder.contains("coder"));

    let broadcast = run_hc(
        &state_path,
        &["send", "--all", "demo", "alice", "medical", "question"],
    );
    assert!(broadcast.contains("[alice -> *] medical question"));

    let coder_claim = run_hc(
        &state_path,
        &[
            "claim",
            "submit",
            "demo",
            "coder",
            "message.0001",
            "0.86",
            "general",
            "fallback",
        ],
    );
    assert!(coder_claim.contains("claim submitted coder"));

    let doctor_claim = run_hc(
        &state_path,
        &[
            "claim",
            "submit",
            "demo",
            "doctor",
            "message.0001",
            "0.93",
            "medical",
            "specialty",
            "match",
        ],
    );
    assert!(doctor_claim.contains("claim submitted doctor"));

    let list = run_hc(&state_path, &["claim", "list", "message.0001"]);
    assert!(list.contains("coder score=0.86"));
    assert!(list.contains("doctor score=0.93"));

    let grant = run_hc(&state_path, &["claim", "resolve", "message.0001", "1"]);
    assert!(grant.contains("granted doctor for message.0001 in round 1 score=0.93"));
}
