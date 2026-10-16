use std::{
    hash::{Hash, Hasher},
    os::unix::fs::PermissionsExt,
    process::Command,
};

static PROCESS_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hyprselector"))
}

#[test]
fn help_and_version_do_not_require_a_desktop_session() {
    let _guard = PROCESS_TEST.lock().unwrap();
    for argument in ["--help", "--version"] {
        let result = binary()
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .arg(argument)
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(String::from_utf8_lossy(&result.stdout).contains("hyprselector"));
    }
    assert!(!binary().arg("--invalid").status().unwrap().success());
}

#[test]
fn empty_desktop_exits_without_connecting_to_wayland() {
    let _guard = PROCESS_TEST.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("hyprctl");
    std::fs::write(&script,"#!/bin/sh\ncase \"$1:$2\" in\n eval:*) printf ok ;;\n -j:clients) printf '[]' ;;\n -j:monitors) /usr/bin/cat \"$HYPRSELECTOR_TEST_FIXTURES/monitors.json\" ;;\n *) exit 3 ;;\nesac\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = binary()
        .env("PATH", dir.path())
        .env("XDG_RUNTIME_DIR", dir.path())
        .env("HYPRLAND_INSTANCE_SIGNATURE", "test-empty")
        .env("WAYLAND_DISPLAY", "not-a-real-socket")
        .env(
            "HYPRSELECTOR_TEST_FIXTURES",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn instance_lock_is_released_without_removing_lock_inode() {
    let _guard = PROCESS_TEST.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    "test-locked".hash(&mut hash);
    let path = dir
        .path()
        .join(format!("hyprselector-{:x}.lock", hash.finish()));
    let file = std::fs::File::create(&path).unwrap();
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    let mut cmd = binary();
    cmd.env("XDG_RUNTIME_DIR", dir.path())
        .env("HYPRLAND_INSTANCE_SIGNATURE", "test-locked")
        .env("WAYLAND_DISPLAY", "not-a-real-socket")
        .env("PATH", dir.path());
    let locked = cmd.output().unwrap();
    assert!(!locked.status.success());
    assert!(String::from_utf8_lossy(&locked.stderr).contains("already running"));
    drop(file);
    let released = cmd.output().unwrap();
    assert!(!released.status.success());
    assert!(String::from_utf8_lossy(&released.stderr).contains("cannot start hyprctl"));
    assert!(path.exists());
}
