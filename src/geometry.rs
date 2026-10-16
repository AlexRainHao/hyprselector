use anyhow::{Result, ensure};

/// A rectangle in Hyprland's global logical coordinate space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Result<Self> {
        ensure!(
            [x, y, w, h, x + w, y + h].iter().all(|v| v.is_finite()) && w > 0.0 && h > 0.0,
            "invalid window or output geometry"
        );
        Ok(Self { x, y, w, h })
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let w = (self.x + self.w).min(other.x + other.w) - x;
        let h = (self.y + self.h).min(other.y + other.h) - y;
        (w > 0.0 && h > 0.0).then_some(Self { x, y, w, h })
    }

    pub fn center(self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn translated(self, dx: f64, dy: f64) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..self
        }
    }
}

pub(crate) fn reachable_center(window: Rect, outputs: &[Rect]) -> Result<(i32, i32)> {
    let (cx, cy) = window.center();
    // Choose integer coordinates that are actually inside the intersection, including
    // tiny windows and fractional output bounds. Avoid casting NaN or overflowing.
    let point = outputs
        .iter()
        .filter_map(|output| window.intersection(*output))
        .filter_map(|r| {
            let min_x = r.x.ceil();
            let max_x = (r.x + r.w).ceil() - 1.0;
            let min_y = r.y.ceil();
            let max_y = (r.y + r.h).ceil() - 1.0;
            if min_x > max_x || min_y > max_y {
                return None;
            }
            let x = cx.round().clamp(min_x, max_x);
            let y = cy.round().clamp(min_y, max_y);
            Some((x, y))
        })
        .min_by(|a, b| {
            ((a.0 - cx).powi(2) + (a.1 - cy).powi(2))
                .total_cmp(&((b.0 - cx).powi(2) + (b.1 - cy).powi(2)))
        });
    let (x, y) = point.ok_or_else(|| anyhow::anyhow!("window has no reachable center"))?;
    ensure!(
        window.contains(x, y) && outputs.iter().any(|r| r.contains(x, y)),
        "cursor point lies outside the visible output intersection"
    );
    ensure!(
        x >= i32::MIN as f64
            && x <= i32::MAX as f64
            && y >= i32::MIN as f64
            && y <= i32::MAX as f64,
        "cursor coordinates out of range"
    );
    Ok((x as i32, y as i32))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect::new(x, y, w, h).unwrap()
    }
    #[test]
    fn center_in_gap_uses_nearest_reachable_point() {
        let outputs = [r(-200.0, 0.0, 100.0, 100.0), r(0.0, 0.0, 100.0, 100.0)];
        let w = r(-150.0, 10.0, 200.0, 50.0);
        let p = reachable_center(w, &outputs).unwrap();
        assert!(outputs.iter().any(|r| r.contains(p.0 as f64, p.1 as f64)));
        assert_eq!(p, (0, 35));
    }
    #[test]
    fn rounds_normal_center_and_rejects_invalid_geometry() {
        let w = r(-101.0, 20.0, 51.0, 41.0);
        assert_eq!(reachable_center(w, &[w]).unwrap(), (-76, 41));
        assert!(Rect::new(0.0, 0.0, f64::NAN, 1.0).is_err());
        assert!(Rect::new(0.0, 0.0, 0.0, 1.0).is_err());
        assert!(reachable_center(w, &[]).is_err());
    }
}
