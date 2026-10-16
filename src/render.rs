use crate::{
    hints::Hints,
    layout::{FONT_SIZE, INFO_SIZE, Label},
    model::{Candidate, Output},
};
use anyhow::{Context, Result, ensure};
use fontdue::{Font, FontSettings};
use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Rect as SkRect, Stroke, Transform};

const GREEN: [u8; 3] = [166, 227, 161];
const GRAY: [u8; 3] = [108, 112, 134];
const WHITE: [u8; 3] = [255, 255, 255];

pub(crate) struct Renderer {
    pub font: Font,
}
impl Renderer {
    pub fn new() -> Result<Self> {
        let font = Font::from_bytes(
            include_bytes!("../assets/label-font.ttf") as &[u8],
            FontSettings::default(),
        )
        .map_err(|e| anyhow::anyhow!("invalid bundled font: {e}"))?;
        Ok(Self { font })
    }

    pub fn draw(
        &self,
        output: &Output,
        size: (u32, u32),
        scale: f64,
        windows: &[Candidate],
        labels: &[Label],
        hints: &Hints,
    ) -> Result<Vec<u8>> {
        let (width, height) = size;
        ensure!(
            width > 0 && height > 0 && (width as u64) * (height as u64) <= 64 * 1024 * 1024,
            "output buffer too large"
        );
        let mut pixmap = Pixmap::new(width, height).context("cannot allocate output buffer")?;
        let transform = Transform::from_scale(scale as f32, scale as f32);
        for (_, window) in windows
            .iter()
            .enumerate()
            .filter(|(i, _)| hints.matches(*i))
        {
            let rect = window
                .client
                .rect()?
                .translated(-output.rect.x, -output.rect.y);
            if let Some(rect) = SkRect::from_xywh(
                (rect.x + 1.5) as f32,
                (rect.y + 1.5) as f32,
                (rect.w - 3.0).max(0.5) as f32,
                (rect.h - 3.0).max(0.5) as f32,
            ) {
                let path = PathBuilder::from_rect(rect);
                pixmap.stroke_path(
                    &path,
                    &paint(GREEN),
                    &Stroke {
                        width: 3.0,
                        ..Stroke::default()
                    },
                    transform,
                    None,
                );
            }
        }
        for label in labels {
            let color = if hints.matches(label.index) {
                GREEN
            } else {
                GRAY
            };
            let r = label.rect;
            if (r.x - label.anchor.0 - 8.0).abs() > 1.0 || (r.y - label.anchor.1 - 8.0).abs() > 1.0
            {
                let mut path = PathBuilder::new();
                path.move_to(label.anchor.0 as f32, label.anchor.1 as f32);
                path.line_to(r.x as f32, (r.y + r.h / 2.0) as f32);
                if let Some(path) = path.finish() {
                    pixmap.stroke_path(
                        &path,
                        &paint(color),
                        &Stroke {
                            width: 1.0,
                            ..Stroke::default()
                        },
                        transform,
                        None,
                    );
                }
            }
        }
        // Draw all badges after borders and connecting lines so no line obscures text.
        for label in labels {
            let r = label.rect;
            if let Some(rect) = SkRect::from_xywh(r.x as f32, r.y as f32, r.w as f32, r.h as f32) {
                pixmap.fill_rect(rect, &paint([30, 30, 46]), transform, None);
            }
            let text = hints.label(label.index);
            let color = if hints.matches(label.index) {
                GREEN
            } else {
                GRAY
            };
            self.text(
                &mut pixmap,
                text,
                r.x + 8.0,
                r.y + 4.0,
                FONT_SIZE,
                scale,
                color,
                if hints.matches(label.index) {
                    hints.prefix().len()
                } else {
                    0
                },
                r.x + r.w - 4.0,
            );
            if let Some(info) = &label.info {
                self.text(
                    &mut pixmap,
                    info,
                    r.x + 8.0,
                    r.y + FONT_SIZE as f64 + 10.0,
                    INFO_SIZE,
                    scale,
                    color,
                    0,
                    r.x + r.w - 4.0,
                );
            }
        }
        // wl_shm ARGB8888 is a native-endian 0xAARRGGBB word; tiny-skia is
        // premultiplied RGBA bytes. Keep premultiplication and convert explicitly.
        let mut pixels = pixmap.take();
        for pixel in pixels.chunks_exact_mut(4) {
            let [r, g, b, a] = [pixel[0], pixel[1], pixel[2], pixel[3]];
            pixel.copy_from_slice(&u32::from_be_bytes([a, r, g, b]).to_ne_bytes());
        }
        Ok(pixels)
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        pixmap: &mut Pixmap,
        text: &str,
        x: f64,
        y: f64,
        size: f32,
        scale: f64,
        color: [u8; 3],
        prefix: usize,
        right: f64,
    ) {
        let px = size * scale as f32;
        let ascent = self
            .font
            .horizontal_line_metrics(px)
            .map_or(px, |m| m.ascent);
        let mut pen = x * scale;
        let baseline = y * scale + ascent as f64;
        let width = pixmap.width() as usize;
        let height = pixmap.height() as usize;
        for (index, ch) in text.chars().enumerate() {
            let (metrics, bitmap) = self.font.rasterize(ch, px);
            let gx = pen.round() as i64 + metrics.xmin as i64;
            let gy = baseline.round() as i64 - metrics.ymin as i64 - metrics.height as i64;
            let rgb = if index < prefix { WHITE } else { color };
            for by in 0..metrics.height {
                for bx in 0..metrics.width {
                    let xx = gx + bx as i64;
                    let yy = gy + by as i64;
                    if xx < 0
                        || yy < 0
                        || xx >= width as i64
                        || yy >= height as i64
                        || xx as f64 >= right * scale
                    {
                        continue;
                    }
                    let a = bitmap[by * metrics.width + bx] as u32;
                    let offset = (yy as usize * width + xx as usize) * 4;
                    let dst = &mut pixmap.data_mut()[offset..offset + 4];
                    for c in 0..3 {
                        dst[c] =
                            ((rgb[c] as u32 * a + dst[c] as u32 * (255 - a) + 127) / 255) as u8;
                    }
                    dst[3] = (a + (dst[3] as u32 * (255 - a) + 127) / 255) as u8;
                }
            }
            pen += metrics.advance_width as f64;
        }
    }
}

fn paint(rgb: [u8; 3]) -> Paint<'static> {
    Paint {
        shader: tiny_skia::Shader::SolidColor(Color::from_rgba8(rgb[0], rgb[1], rgb[2], 255)),
        anti_alias: true,
        ..Paint::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_overlay_is_fully_transparent() {
        let renderer = Renderer::new().unwrap();
        let output = Output {
            id: 1,
            name: "test".into(),
            rect: crate::geometry::Rect::new(0.0, 0.0, 100.0, 100.0).unwrap(),
            scale: 1.0,
            transform: 0,
            workspace: 1,
            special: 0,
            focused: true,
        };
        let pixels = renderer
            .draw(&output, (100, 100), 1.0, &[], &[], &Hints::new(0).unwrap())
            .unwrap();
        assert!(pixels.iter().all(|b| *b == 0));
    }

    #[test]
    fn highlighter_uses_native_argb_and_disappears_for_nonmatches() {
        let renderer = Renderer::new().unwrap();
        let client=serde_json::from_value(serde_json::json!({"address":"0x12","at":[0,0],"size":[100,100],"workspace":{"id":1},"monitor":1,"mapped":true,"hidden":false,"acceptsInput":true,"pinned":false,"class":"test","title":"test"})).unwrap();
        let monitor=serde_json::from_value(serde_json::json!({"id":1,"name":"DP-1","x":0,"y":0,"width":100,"height":100,"scale":1,"transform":0,"activeWorkspace":{"id":1},"specialWorkspace":{"id":0},"focused":true})).unwrap();
        let snapshot = crate::model::Snapshot::new(vec![client], vec![monitor]).unwrap();
        let mut hints = Hints::new(1).unwrap();
        let pixels = renderer
            .draw(
                &snapshot.outputs[0],
                (100, 100),
                1.0,
                &snapshot.windows,
                &[],
                &hints,
            )
            .unwrap();
        let offset = (100 + 50) * 4;
        assert_eq!(&pixels[offset..offset + 4], &0xffa6e3a1u32.to_ne_bytes());
        hints.input(crate::hints::Input::Letter('z'));
        let pixels = renderer
            .draw(
                &snapshot.outputs[0],
                (100, 100),
                1.0,
                &snapshot.windows,
                &[],
                &hints,
            )
            .unwrap();
        assert!(pixels.iter().all(|b| *b == 0));
    }
}
