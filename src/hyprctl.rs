use crate::model::{Client, Monitor, Snapshot, valid_address};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::{
    ffi::OsString,
    io::{Read, Seek, SeekFrom},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const LIMIT: u64 = 16 * 1024 * 1024;

pub(crate) trait Control {
    fn snapshot(&self) -> Result<Snapshot>;
    fn active(&self) -> Result<Option<Client>>;
    fn cursor(&self) -> Result<(i32, i32)>;
    fn focus(&self, address: &str) -> Result<()>;
    fn move_cursor(&self, x: i32, y: i32) -> Result<()>;
}

pub(crate) struct Hyprctl {
    executable: OsString,
    prefix_args: Vec<OsString>,
    timeout: Duration,
    cancelled: Arc<AtomicBool>,
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Hyprctl {
    pub fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self {
            executable: "hyprctl".into(),
            prefix_args: Vec::new(),
            timeout: Duration::from_secs(2),
            cancelled,
        }
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        // File-backed capture avoids pipe deadlocks without unbounded reader threads.
        let mut stdout = tempfile::tempfile().context("cannot allocate command output capture")?;
        let stderr = tempfile::tempfile()?;
        let child = Command::new(&self.executable)
            .args(&self.prefix_args)
            .args(args)
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .spawn()
            .context("cannot start hyprctl; make sure it is installed and on PATH")?;
        let mut child = ChildGuard(child);
        let start = Instant::now();
        let status = loop {
            ensure!(
                !self.cancelled.load(Ordering::Relaxed),
                "operation interrupted"
            );
            ensure!(
                stdout.metadata()?.len() <= LIMIT && stderr.metadata()?.len() <= LIMIT,
                "hyprctl output exceeded limit"
            );
            if let Some(status) = child.0.try_wait()? {
                break status;
            }
            ensure!(start.elapsed() < self.timeout, "hyprctl timed out");
            thread::sleep(Duration::from_millis(10));
        };
        ensure!(
            status.success(),
            "hyprctl exited unsuccessfully ({status}); check the active Hyprland session"
        );
        stdout.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        stdout.take(LIMIT + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= LIMIT, "hyprctl output exceeded limit");
        Ok(bytes)
    }

    fn json<T: DeserializeOwned>(&self, command: &str) -> Result<T> {
        serde_json::from_slice(&self.run(&["-j", command])?)
            .with_context(|| format!("invalid JSON from hyprctl {command}"))
    }

    fn ok(&self, args: &[&str]) -> Result<()> {
        let bytes = self.run(args)?;
        let reply = std::str::from_utf8(&bytes)
            .context("invalid hyprctl reply encoding")?
            .trim();
        if reply != "ok" {
            // Command failures can contain arbitrary server text; bound and sanitize it.
            let reason: String = reply
                .chars()
                .filter(|c| !c.is_control())
                .take(240)
                .collect();
            bail!("hyprctl rejected the command: {reason}");
        }
        Ok(())
    }

    pub fn check_lua(&self) -> Result<()> {
        // eval is itself Lua-only. Check capabilities without changing desktop state.
        self.ok(&["eval","assert(type(hl.dsp.focus) == 'function' and type(hl.dsp.cursor.move) == 'function', 'required Lua dispatchers unavailable')"])
            .context("requires a Hyprland Lua session with focus and cursor.move dispatchers")
    }
}

impl Control for Hyprctl {
    fn snapshot(&self) -> Result<Snapshot> {
        let clients: Vec<Client> = self.json("clients")?;
        let monitors: Vec<Monitor> = self.json("monitors")?;
        Snapshot::new(clients, monitors)
    }
    fn active(&self) -> Result<Option<Client>> {
        let value: serde_json::Value = self.json("activewindow")?;
        if value.is_null() || value.as_object().is_some_and(|o| o.is_empty()) {
            return Ok(None);
        }
        Ok(Some(
            serde_json::from_value(value).context("invalid activewindow fields")?,
        ))
    }
    fn cursor(&self) -> Result<(i32, i32)> {
        #[derive(Deserialize)]
        struct Point {
            x: i32,
            y: i32,
        }
        let point: Point = self.json("cursorpos")?;
        Ok((point.x, point.y))
    }
    fn focus(&self, address: &str) -> Result<()> {
        ensure!(valid_address(address), "invalid target window address");
        self.ok(&[
            "dispatch",
            &format!("hl.dsp.focus({{ window = \"address:{address}\" }})"),
        ])
    }
    fn move_cursor(&self, x: i32, y: i32) -> Result<()> {
        self.ok(&[
            "dispatch",
            &format!("hl.dsp.cursor.move({{ x = {x}, y = {y} }})"),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fake(script: &str) -> (tempfile::TempDir, Hyprctl) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hyprctl");
        std::fs::write(&file, format!("#!/bin/sh\n{script}\n")).unwrap();
        let mut ctl = Hyprctl::new(Arc::new(AtomicBool::new(false)));
        // Interpret generated fixtures using a fixed executable. Concurrent forks
        // can briefly inherit a writable script descriptor and make direct exec
        // fail with ETXTBSY even after the creating thread closed its descriptor.
        ctl.executable = "/bin/sh".into();
        ctl.prefix_args.push(file.into_os_string());
        (dir, ctl)
    }
    #[test]
    fn zero_status_with_error_body_is_not_success() {
        let (_d, ctl) = fake("printf 'error: Lua failure'");
        assert!(ctl.focus("0x12").is_err());
    }
    #[test]
    fn hung_child_is_killed_and_reaped() {
        let mut ctl = Hyprctl::new(Arc::new(AtomicBool::new(false)));
        ctl.executable = "/usr/bin/sleep".into();
        ctl.timeout = Duration::from_millis(40);
        let start = Instant::now();
        let error = ctl.run(&["5"]).unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn dispatch_payload_is_one_argument_and_uses_lua() {
        let (_d, ctl) = fake(
            "[ \"$#\" = 2 ] && [ \"$1\" = dispatch ] && [ \"$2\" = 'hl.dsp.focus({ window = \"address:0x12\" })' ] || exit 3\nprintf ok",
        );
        ctl.focus("0x12").unwrap();
        assert!(ctl.focus("0x12\"}").is_err());
    }
    #[test]
    fn empty_active_window_is_normal() {
        let (_d, ctl) = fake("printf '{}'");
        assert!(ctl.active().unwrap().is_none());
    }
    #[test]
    fn malformed_json_and_exit_failure_are_errors() {
        let (_d, ctl) = fake("printf nope");
        assert!(ctl.snapshot().is_err());
        let (_d, ctl) = fake("exit 3");
        assert!(ctl.run(&[]).is_err());
    }
}
