use anyhow::{Context, Result, ensure};
use rustix::fs::{FlockOperation, flock};
use signal_hook::{
    SigId,
    consts::{SIGINT, SIGTERM},
    flag,
};
use std::{
    fs::{File, OpenOptions},
    hash::{Hash, Hasher},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

pub(crate) struct Session {
    _lock: File,
    pub cancelled: Arc<AtomicBool>,
    signals: Vec<SigId>,
}

impl Session {
    pub fn acquire() -> Result<Self> {
        ensure!(
            std::env::var_os("WAYLAND_DISPLAY").is_some(),
            "WAYLAND_DISPLAY is missing; run inside your Hyprland session"
        );
        let instance = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .context("HYPRLAND_INSTANCE_SIGNATURE is missing")?;
        ensure!(!instance.is_empty(), "empty Hyprland instance signature");
        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is missing")?,
        );
        ensure!(
            runtime.is_absolute() && runtime.is_dir(),
            "invalid XDG_RUNTIME_DIR"
        );
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        instance.hash(&mut hash);
        let path = runtime.join(format!("hyprselector-{:x}.lock", hash.finish()));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)
            .context("cannot open instance lock")?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive)
            .context("hyprselector is already running in this session")?;
        let mut session = Self {
            _lock: lock,
            cancelled: Arc::new(AtomicBool::new(false)),
            signals: Vec::new(),
        };
        for signal in [SIGINT, SIGTERM] {
            session
                .signals
                .push(flag::register(signal, session.cancelled.clone())?);
        }
        Ok(session)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for id in self.signals.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}
