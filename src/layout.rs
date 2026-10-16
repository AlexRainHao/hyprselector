use crate::{
    geometry::Rect,
    hints::Hints,
    model::{Candidate, Output},
};
use anyhow::{Result, ensure};
use fontdue::Font;

pub(crate) const FONT_SIZE: f32 = 24.0;
pub(crate) const INFO_SIZE: f32 = 12.0;

#[derive(Debug, Clone)]
pub(crate) struct Label {
    pub index: usize,
    pub rect: Rect,
    pub anchor: (f64, f64),
    pub info: Option<String>,
}

pub(crate) fn text_width(font: &Font, text: &str, size: f32) -> f64 {
    text.chars()
        .map(|c| font.metrics(c, size).advance_width as f64)
        .sum()
}

fn readable(font: &Font, text: &str) -> String {
    let mut chars = text.chars().filter(|c| !c.is_control()).map(|c| {
        if font.lookup_glyph_index(c) == 0 {
            '?'
        } else {
            c
        }
    });
    let mut result: String = chars.by_ref().take(28).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

fn info(font: &Font, window: &Candidate, windows: &[Candidate]) -> String {
    let same_class = windows
        .iter()
        .filter(|w| w.client.class == window.client.class)
        .count()
        > 1;
    let text = if same_class {
        format!("{} {}", window.client.class, window.client.title)
    } else {
        window.client.class.clone()
    };
    let address = &window.client.address;
    let suffix = &address[address.len().saturating_sub(6)..];
    format!("{} · {suffix}", readable(font, &text))
}

pub(crate) fn place(
    windows: &[Candidate],
    outputs: &[Output],
    hints: &Hints,
    font: &Font,
) -> Result<Vec<Vec<Label>>> {
    let mut result = Vec::new();
    for (output_index, output) in outputs.iter().enumerate() {
        let bounds = Rect::new(0.0, 0.0, output.rect.w, output.rect.h)?;
        let mut labels = Vec::new();
        for (index, window) in windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.output == output_index)
        {
            let w = text_width(font, hints.label(index), FONT_SIZE) + 16.0;
            let h = FONT_SIZE as f64 + 12.0;
            ensure!(
                w <= bounds.w && h <= bounds.h,
                "output too small to display window labels"
            );
            let anchor = (
                (window.client.at[0] as f64 - output.rect.x).clamp(0.0, bounds.w - 1.0),
                (window.client.at[1] as f64 - output.rect.y).clamp(0.0, bounds.h - 1.0),
            );
            let rect = Rect::new(
                (anchor.0 + 8.0).clamp(0.0, bounds.w - w),
                (anchor.1 + 8.0).clamp(0.0, bounds.h - h),
                w,
                h,
            )?;
            labels.push(Label {
                index,
                rect,
                anchor,
                info: None,
            });
        }
        // Information changes label dimensions, so iterate collision propagation until
        // every newly colliding label has been measured with its extra information.
        loop {
            let collisions: Vec<_> = (0..labels.len())
                .filter(|&i| {
                    labels[i].info.is_none()
                        && (0..labels.len()).any(|j| {
                            i != j && labels[i].rect.intersection(labels[j].rect).is_some()
                        })
                })
                .collect();
            if collisions.is_empty() {
                break;
            }
            for i in collisions {
                let label = &mut labels[i];
                let text = info(font, &windows[label.index], windows);
                label.rect.w = label
                    .rect
                    .w
                    .max(text_width(font, &text, INFO_SIZE) + 16.0)
                    .min(bounds.w);
                label.rect.h += INFO_SIZE as f64 + 6.0;
                ensure!(
                    label.rect.h <= bounds.h,
                    "output too small for overlapping labels"
                );
                label.rect.x = label.rect.x.min(bounds.w - label.rect.w);
                label.rect.y = label.rect.y.min(bounds.h - label.rect.h);
                label.info = Some(text);
            }
        }
        let mut placed: Vec<Label> = Vec::new();
        let mut failed = false;
        for mut label in labels.iter().cloned() {
            let ideal = label.rect;
            let mut candidates = vec![(ideal.x, ideal.y)];
            for previous in &placed {
                candidates.push((ideal.x, previous.rect.y + previous.rect.h + 4.0));
                candidates.push((previous.rect.x + previous.rect.w + 4.0, ideal.y));
                candidates.push((ideal.x, (previous.rect.y - ideal.h - 4.0).max(0.0)));
            }
            candidates.sort_by(|a, b| {
                let rank = |p: (f64, f64)| {
                    if p == (ideal.x, ideal.y) {
                        0
                    } else if p.0 == ideal.x && p.1 >= ideal.y {
                        1
                    } else {
                        2
                    }
                };
                rank(*a).cmp(&rank(*b)).then(
                    ((a.0 - ideal.x).powi(2) + (a.1 - ideal.y).powi(2))
                        .total_cmp(&((b.0 - ideal.x).powi(2) + (b.1 - ideal.y).powi(2))),
                )
            });
            let position = candidates.into_iter().find(|&(x, y)| {
                x >= 0.0
                    && y >= 0.0
                    && x + ideal.w <= bounds.w
                    && y + ideal.h <= bounds.h
                    && placed
                        .iter()
                        .all(|p| Rect { x, y, ..ideal }.intersection(p.rect).is_none())
            });
            let Some((x, y)) = position else {
                failed = true;
                break;
            };
            label.rect.x = x;
            label.rect.y = y;
            placed.push(label);
        }
        if failed {
            // Deterministic compact columns are preferable to unreadable collisions.
            placed.clear();
            let (mut x, mut y, mut column_width) = (4.0, 4.0, 0.0f64);
            for mut label in labels {
                if y + label.rect.h > bounds.h {
                    x += column_width + 4.0;
                    y = 4.0;
                    column_width = 0.0;
                }
                ensure!(
                    x + label.rect.w <= bounds.w && y + label.rect.h <= bounds.h,
                    "too many overlapping labels to fit on {}; close windows and retry",
                    output.name
                );
                label.rect.x = x;
                label.rect.y = y;
                y += label.rect.h + 4.0;
                column_width = column_width.max(label.rect.w);
                placed.push(label);
            }
        }
        result.push(placed);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Snapshot;
    #[test]
    fn identical_windows_have_separate_labels_with_information() {
        let font = Font::from_bytes(
            include_bytes!("../assets/label-font.ttf") as &[u8],
            fontdue::FontSettings::default(),
        )
        .unwrap();
        let json = serde_json::json!({"address":"0x12","at":[0,0],"size":[100,100],"workspace":{"id":1},"monitor":1,"mapped":true,"hidden":false,"acceptsInput":true,"pinned":false,"class":"kitty","title":"终端\nhello"});
        let a = serde_json::from_value(json.clone()).unwrap();
        let mut j = json;
        j["address"] = "0x13".into();
        let monitor=serde_json::from_value(serde_json::json!({"id":1,"name":"DP-1","x":0,"y":0,"width":800,"height":600,"scale":1,"transform":0,"activeWorkspace":{"id":1},"specialWorkspace":{"id":0},"focused":true})).unwrap();
        let snapshot =
            Snapshot::new(vec![a, serde_json::from_value(j).unwrap()], vec![monitor]).unwrap();
        let layout = place(
            &snapshot.windows,
            &snapshot.outputs,
            &Hints::new(2).unwrap(),
            &font,
        )
        .unwrap();
        assert_eq!(layout[0].len(), 2);
        assert!(layout[0].iter().all(|l| l.info.is_some()));
        assert!(layout[0][0].rect.intersection(layout[0][1].rect).is_none());
    }
}
