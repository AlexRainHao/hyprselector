use crate::geometry::Rect;
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Workspace {
    pub id: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Client {
    pub address: String,
    #[serde(default)]
    pub stable_id: Option<String>,
    pub at: [i32; 2],
    pub size: [i32; 2],
    pub workspace: Workspace,
    pub monitor: i64,
    pub mapped: bool,
    pub hidden: bool,
    pub accepts_input: bool,
    pub pinned: bool,
    pub class: String,
    pub title: String,
}

impl Client {
    pub fn rect(&self) -> Result<Rect> {
        Rect::new(
            self.at[0] as f64,
            self.at[1] as f64,
            self.size[0] as f64,
            self.size[1] as f64,
        )
    }
    pub fn same_identity(&self, other: &Self) -> bool {
        self.address == other.address && self.stable_id == other.stable_id
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Monitor {
    pub id: i64,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub transform: u32,
    pub active_workspace: Workspace,
    pub special_workspace: Workspace,
    pub focused: bool,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Output {
    pub id: i64,
    pub name: String,
    pub rect: Rect,
    pub scale: f64,
    pub transform: u32,
    pub workspace: i64,
    pub special: i64,
    pub focused: bool,
}

impl TryFrom<Monitor> for Output {
    type Error = anyhow::Error;
    fn try_from(m: Monitor) -> Result<Self> {
        ensure!(
            m.scale.is_finite() && m.scale > 0.0 && m.transform < 8,
            "invalid output scale or transform"
        );
        let (w, h) = if m.transform % 2 == 1 {
            (m.height, m.width)
        } else {
            (m.width, m.height)
        };
        let rect = Rect::new(
            m.x as f64,
            m.y as f64,
            (w as f64 / m.scale).round(),
            (h as f64 / m.scale).round(),
        )?;
        Ok(Self {
            id: m.id,
            name: m.name,
            rect,
            scale: m.scale,
            transform: m.transform,
            workspace: m.active_workspace.id,
            special: m.special_workspace.id,
            focused: m.focused,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub client: Client,
    pub output: usize,
}

pub(crate) struct Snapshot {
    pub outputs: Vec<Output>,
    pub windows: Vec<Candidate>,
}

pub(crate) fn valid_address(s: &str) -> bool {
    s.strip_prefix("0x").is_some_and(|digits| {
        !digits.is_empty()
            && digits.len() <= 16
            && digits.bytes().all(|b| b.is_ascii_hexdigit())
            && digits.bytes().any(|b| b != b'0')
    })
}

impl Snapshot {
    pub fn new(clients: Vec<Client>, monitors: Vec<Monitor>) -> Result<Self> {
        let mut outputs = monitors
            .into_iter()
            .filter(|m| !m.disabled)
            .map(Output::try_from)
            .collect::<Result<Vec<_>>>()?;
        outputs.sort_by(|a, b| {
            a.rect
                .y
                .total_cmp(&b.rect.y)
                .then(a.rect.x.total_cmp(&b.rect.x))
                .then(a.name.cmp(&b.name))
        });
        ensure!(!outputs.is_empty(), "no active Hyprland outputs");
        let active: HashSet<_> = outputs
            .iter()
            .flat_map(|o| [o.workspace, o.special])
            .filter(|id| *id != 0)
            .collect();
        let mut seen = HashSet::new();
        let mut windows = Vec::new();
        for client in clients {
            if !client.mapped
                || client.hidden
                || !client.accepts_input
                || (!active.contains(&client.workspace.id) && !client.pinned)
                || !valid_address(&client.address)
            {
                continue;
            }
            let Ok(rect) = client.rect() else { continue };
            let intersects = |i: usize| rect.intersection(outputs[i].rect);
            let output = outputs
                .iter()
                .enumerate()
                .find(|(i, o)| o.id == client.monitor && intersects(*i).is_some())
                .map(|(i, _)| i)
                .or_else(|| {
                    (0..outputs.len())
                        .filter_map(|i| intersects(i).map(|r| (i, r.w * r.h)))
                        .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
                        .map(|(i, _)| i)
                });
            if let Some(output) = output
                && seen.insert(client.address.clone())
            {
                windows.push(Candidate { client, output });
            }
        }
        windows.sort_by(|a, b| {
            a.output
                .cmp(&b.output)
                .then(a.client.at[1].cmp(&b.client.at[1]))
                .then(a.client.at[0].cmp(&b.client.at[0]))
                .then(a.client.address.cmp(&b.client.address))
        });
        Ok(Self { outputs, windows })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    pub fn client(address: &str, workspace: i64) -> Client {
        serde_json::from_value(json!({"address":address,"at":[10,10],"size":[100,100],
            "workspace":{"id":workspace},"monitor":1,"mapped":true,"hidden":false,
            "acceptsInput":true,"pinned":false,"class":"test","title":"test","visible":false}))
        .unwrap()
    }
    pub fn monitor() -> Monitor {
        serde_json::from_value(json!({"id":1,"name":"DP-1","x":0,"y":0,"width":1920,"height":1080,
            "scale":1.0,"transform":0,"activeWorkspace":{"id":1},"specialWorkspace":{"id":-99},"focused":true})).unwrap()
    }
    #[test]
    fn occlusion_special_and_pinned_are_included_hidden_is_excluded() {
        let mut hidden = client("0x3", 1);
        hidden.hidden = true;
        let mut pinned = client("0x5", 2);
        pinned.pinned = true;
        let s = Snapshot::new(
            vec![
                client("0x1", 1),
                client("0x2", -99),
                hidden,
                client("0x4", 2),
                pinned,
                client("0x1", 1),
            ],
            vec![monitor()],
        )
        .unwrap();
        assert_eq!(
            s.windows
                .iter()
                .map(|w| w.client.address.as_str())
                .collect::<Vec<_>>(),
            ["0x1", "0x2", "0x5"]
        );
    }
    #[test]
    fn rotated_output_uses_transformed_logical_dimensions() {
        let mut m = monitor();
        m.transform = 1;
        m.scale = 1.5;
        m.x = -720;
        let o = Output::try_from(m).unwrap();
        assert_eq!(o.rect, Rect::new(-720.0, 0.0, 720.0, 1280.0).unwrap());
        assert!(!valid_address("0x1'); os.exit()"));
        assert!(!valid_address("0x0"));
    }
    #[test]
    fn identity_detects_address_reuse() {
        let mut a = client("0x1", 1);
        a.stable_id = Some("abc".into());
        let mut b = a.clone();
        b.stable_id = Some("def".into());
        assert!(!a.same_identity(&b));
    }

    #[test]
    fn all_displayed_workspaces_participate_in_stable_spatial_order() {
        let mut left = monitor();
        left.id = 2;
        left.name = "DP-2".into();
        left.x = -1920;
        left.active_workspace.id = 2;
        let mut client_left = client("0x2", 2);
        client_left.monitor = 2;
        client_left.at = [-1900, 10];
        let mut cross = client("0x3", 1);
        cross.at = [-20, 200];
        cross.size = [200, 100];
        let s = Snapshot::new(
            vec![client("0x1", 1), cross, client_left],
            vec![monitor(), left],
        )
        .unwrap();
        assert_eq!(s.outputs[0].name, "DP-2");
        assert_eq!(
            s.windows
                .iter()
                .map(|w| w.client.address.as_str())
                .collect::<Vec<_>>(),
            ["0x2", "0x1", "0x3"]
        );
        assert_eq!(
            s.windows
                .iter()
                .filter(|w| w.client.address == "0x3")
                .count(),
            1
        );
    }
}
