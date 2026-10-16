use crate::{
    geometry::reachable_center,
    hyprctl::{Control, Hyprctl},
    model::{Client, Snapshot},
    overlay,
    session::Session,
};
use anyhow::{Context, Result, ensure};
use std::{sync::atomic::Ordering, thread, time::Duration};

pub(crate) fn run() -> Result<()> {
    let session = Session::acquire()?;
    let ctl = Hyprctl::new(session.cancelled.clone());
    ctl.check_lua()?;
    let snapshot = ctl.snapshot()?;
    if snapshot.windows.is_empty() {
        return Ok(());
    }
    let original = ctl.active()?;
    let selected = overlay::select(&snapshot, session.cancelled.clone())?;
    if session.cancelled.load(Ordering::Relaxed) {
        return Ok(());
    }
    if let Some(index) = selected {
        commit(&ctl, &snapshot, index)
    } else {
        restore_keyboard(&ctl, original.as_ref())
    }
}

fn restore_keyboard(ctl: &impl Control, original: Option<&Client>) -> Result<()> {
    // Do not overwrite an intentional focus change to another regular window.
    if ctl.active()?.is_none()
        && let Some(original) = original
    {
        let snapshot = ctl.snapshot()?;
        if snapshot
            .windows
            .iter()
            .any(|w| original.same_identity(&w.client))
        {
            let pointer = ctl.cursor()?;
            ctl.focus(&original.address)
                .context("could not restore the original keyboard focus")?;
            ensure!(
                ctl.cursor()? == pointer,
                "compositor moved the pointer while restoring keyboard focus"
            );
        }
    }
    Ok(())
}

pub(crate) fn commit(ctl: &impl Control, initial: &Snapshot, index: usize) -> Result<()> {
    let original = &initial
        .windows
        .get(index)
        .context("invalid selected label")?
        .client;
    let fresh = ctl
        .snapshot()
        .context("could not revalidate selected window")?;
    ensure!(
        same_outputs(initial, &fresh),
        "output or workspace changed during selection; retry"
    );
    let target = fresh
        .windows
        .iter()
        .find(|w| original.same_identity(&w.client))
        .context("selected window closed or left the current workspace; retry")?;
    ctl.focus(&target.client.address)
        .context("failed to focus the selected window")?;
    let mut active = None;
    for _ in 0..5 {
        active = ctl.active()?;
        if active.as_ref().is_some_and(|c| original.same_identity(c)) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    active
        .filter(|c| original.same_identity(c))
        .context("compositor did not focus the selected window; pointer was not moved")?;
    let current = ctl.snapshot()?;
    ensure!(
        same_geometry(initial, &current),
        "output geometry changed after focus; pointer was not moved"
    );
    let current_target = current
        .windows
        .iter()
        .find(|w| original.same_identity(&w.client))
        .context("selected window disappeared after focus")?;
    let outputs: Vec<_> = current.outputs.iter().map(|o| o.rect).collect();
    let (x, y) = reachable_center(current_target.client.rect()?, &outputs)?;
    ctl.move_cursor(x, y)
        .context("window focused, but pointer movement failed")?;
    ensure!(
        ctl.active()?
            .as_ref()
            .is_some_and(|c| original.same_identity(c)),
        "pointer moved but final window focus does not match the selection"
    );
    let actual = ctl.cursor()?;
    ensure!(
        (actual.0 as i64 - x as i64).abs() <= 1 && (actual.1 as i64 - y as i64).abs() <= 1,
        "window focused, but compositor did not place pointer at the requested center"
    );
    Ok(())
}

fn same_outputs(a: &Snapshot, b: &Snapshot) -> bool {
    same_geometry(a, b)
        && a.outputs
            .iter()
            .zip(&b.outputs)
            .all(|(a, b)| a.workspace == b.workspace && a.special == b.special)
}

fn same_geometry(a: &Snapshot, b: &Snapshot) -> bool {
    a.outputs.len() == b.outputs.len()
        && a.outputs.iter().zip(&b.outputs).all(|(a, b)| {
            a.id == b.id
                && a.name == b.name
                && a.rect == b.rect
                && a.scale == b.scale
                && a.transform == b.transform
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    struct Fake {
        calls: RefCell<Vec<&'static str>>,
        focus_error: bool,
        warp_error: bool,
    }
    fn snapshot() -> Snapshot {
        let client=serde_json::from_value(serde_json::json!({"address":"0x1","stableId":"abc","at":[0,0],"size":[100,100],"workspace":{"id":1},"monitor":1,"mapped":true,"hidden":false,"acceptsInput":true,"pinned":false,"class":"test","title":"test"})).unwrap();
        let monitor=serde_json::from_value(serde_json::json!({"id":1,"name":"DP-1","x":0,"y":0,"width":800,"height":600,"scale":1,"transform":0,"activeWorkspace":{"id":1},"specialWorkspace":{"id":0},"focused":true})).unwrap();
        Snapshot::new(vec![client], vec![monitor]).unwrap()
    }
    impl Control for Fake {
        fn snapshot(&self) -> Result<Snapshot> {
            self.calls.borrow_mut().push("snapshot");
            Ok(snapshot())
        }
        fn active(&self) -> Result<Option<Client>> {
            self.calls.borrow_mut().push("active");
            Ok(Some(snapshot().windows.remove(0).client))
        }
        fn cursor(&self) -> Result<(i32, i32)> {
            self.calls.borrow_mut().push("cursor");
            Ok((50, 50))
        }
        fn focus(&self, _: &str) -> Result<()> {
            self.calls.borrow_mut().push("focus");
            ensure!(!self.focus_error, "focus failed");
            Ok(())
        }
        fn move_cursor(&self, x: i32, y: i32) -> Result<()> {
            assert_eq!((x, y), (50, 50));
            self.calls.borrow_mut().push("warp");
            ensure!(!self.warp_error, "warp failed");
            Ok(())
        }
    }
    #[test]
    fn focus_failure_never_warps_pointer() {
        let f = Fake {
            calls: RefCell::new(vec![]),
            focus_error: true,
            warp_error: false,
        };
        assert!(commit(&f, &snapshot(), 0).is_err());
        assert!(!f.calls.borrow().contains(&"warp"));
    }
    #[test]
    fn pointer_failure_is_reported_as_partial_failure() {
        let f = Fake {
            calls: RefCell::new(vec![]),
            focus_error: false,
            warp_error: true,
        };
        assert!(
            commit(&f, &snapshot(), 0)
                .unwrap_err()
                .to_string()
                .contains("window focused")
        );
    }
    #[test]
    fn successful_commit_verifies_focus_and_center() {
        let f = Fake {
            calls: RefCell::new(vec![]),
            focus_error: false,
            warp_error: false,
        };
        commit(&f, &snapshot(), 0).unwrap();
        assert_eq!(
            *f.calls.borrow(),
            [
                "snapshot", "focus", "active", "snapshot", "warp", "active", "cursor"
            ]
        );
    }
    #[test]
    fn reused_address_does_not_select_new_window() {
        let f = Fake {
            calls: RefCell::new(vec![]),
            focus_error: false,
            warp_error: false,
        };
        let mut old = snapshot();
        old.windows[0].client.stable_id = Some("old".into());
        assert!(commit(&f, &old, 0).is_err());
        assert!(!f.calls.borrow().contains(&"focus"));
    }

    #[test]
    fn closed_target_or_changed_workspace_never_focuses_another_window() {
        for changed_workspace in [false, true] {
            let f = Fake {
                calls: RefCell::new(vec![]),
                focus_error: false,
                warp_error: false,
            };
            let mut old = snapshot();
            if changed_workspace {
                old.outputs[0].workspace = 2;
            } else {
                old.windows[0].client.address = "0x123".into();
            }
            assert!(commit(&f, &old, 0).is_err());
            assert!(!f.calls.borrow().contains(&"focus"));
        }
    }

    #[test]
    fn center_uses_revalidated_geometry_not_the_initial_snapshot() {
        let f = Fake {
            calls: RefCell::new(vec![]),
            focus_error: false,
            warp_error: false,
        };
        let mut old = snapshot();
        old.windows[0].client.at = [300, 200];
        commit(&f, &old, 0).unwrap();
        // Fake::move_cursor asserts (50,50), the fresh center, not (350,250).
    }

    #[test]
    fn compositor_hiding_special_workspace_does_not_change_output_geometry() {
        let mut a = snapshot();
        a.outputs[0].special = -99;
        let b = snapshot();
        assert!(same_geometry(&a, &b));
        assert!(!same_outputs(&a, &b));
    }
}
