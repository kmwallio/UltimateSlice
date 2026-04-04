#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AdjustmentScopeShape {
    pub(crate) center_x: f64,
    pub(crate) center_y: f64,
    pub(crate) left: f64,
    pub(crate) right: f64,
    pub(crate) top: f64,
    pub(crate) bottom: f64,
    pub(crate) rotation_deg: f64,
}

impl AdjustmentScopeShape {
    pub(crate) fn from_transform(
        out_w: u32,
        out_h: u32,
        scale: f64,
        position_x: f64,
        position_y: f64,
        rotation_deg: f64,
        crop_left: i32,
        crop_right: i32,
        crop_top: i32,
        crop_bottom: i32,
    ) -> Self {
        let pw = out_w.max(1) as f64;
        let ph = out_h.max(1) as f64;
        let scale = scale.clamp(0.1, 4.0);
        let position_x = position_x.clamp(-1.0, 1.0);
        let position_y = position_y.clamp(-1.0, 1.0);
        let crop_left = crop_left.max(0) as f64;
        let crop_right = crop_right.max(0) as f64;
        let crop_top = crop_top.max(0) as f64;
        let crop_bottom = crop_bottom.max(0) as f64;

        let center_x = pw / 2.0 + position_x * pw * (1.0 - scale) / 2.0;
        let center_y = ph / 2.0 + position_y * ph * (1.0 - scale) / 2.0;
        let half_w = pw * scale / 2.0;
        let half_h = ph * scale / 2.0;

        let left = center_x - half_w + crop_left * scale;
        let mut right = center_x + half_w - crop_right * scale;
        let top = center_y - half_h + crop_top * scale;
        let mut bottom = center_y + half_h - crop_bottom * scale;
        if right < left {
            right = left;
        }
        if bottom < top {
            bottom = top;
        }

        Self {
            center_x,
            center_y,
            left,
            right,
            top,
            bottom,
            rotation_deg,
        }
    }

    pub(crate) fn is_full_frame(&self, out_w: u32, out_h: u32) -> bool {
        const EPS: f64 = 0.5;
        self.rotation_deg.abs() < f64::EPSILON
            && self.left <= EPS
            && self.top <= EPS
            && self.right >= out_w as f64 - EPS
            && self.bottom >= out_h as f64 - EPS
    }

    pub(crate) fn pixel_bounds(
        &self,
        width: usize,
        height: usize,
    ) -> Option<(usize, usize, usize, usize)> {
        let (min_x, min_y, max_x, max_y) = self.axis_aligned_bounds();
        let x0 = min_x.floor().max(0.0).min(width as f64) as usize;
        let y0 = min_y.floor().max(0.0).min(height as f64) as usize;
        let x1 = max_x.ceil().max(0.0).min(width as f64) as usize;
        let y1 = max_y.ceil().max(0.0).min(height as f64) as usize;
        if x0 >= x1 || y0 >= y1 {
            None
        } else {
            Some((x0, y0, x1, y1))
        }
    }

    pub(crate) fn contains_pixel(&self, x: usize, y: usize) -> bool {
        let (ux, uy) = self.unrotate_point(x as f64, y as f64);
        ux >= self.left && ux < self.right && uy >= self.top && uy < self.bottom
    }

    fn axis_aligned_bounds(&self) -> (f64, f64, f64, f64) {
        let rot_rad = (-self.rotation_deg).to_radians();
        let corners = [
            rotate_point_about(self.left, self.top, self.center_x, self.center_y, rot_rad),
            rotate_point_about(self.right, self.top, self.center_x, self.center_y, rot_rad),
            rotate_point_about(
                self.right,
                self.bottom,
                self.center_x,
                self.center_y,
                rot_rad,
            ),
            rotate_point_about(
                self.left,
                self.bottom,
                self.center_x,
                self.center_y,
                rot_rad,
            ),
        ];
        let min_x = corners
            .iter()
            .map(|(x, _)| *x)
            .fold(f64::INFINITY, f64::min);
        let min_y = corners
            .iter()
            .map(|(_, y)| *y)
            .fold(f64::INFINITY, f64::min);
        let max_x = corners
            .iter()
            .map(|(x, _)| *x)
            .fold(f64::NEG_INFINITY, f64::max);
        let max_y = corners
            .iter()
            .map(|(_, y)| *y)
            .fold(f64::NEG_INFINITY, f64::max);
        (min_x, min_y, max_x, max_y)
    }

    fn unrotate_point(&self, x: f64, y: f64) -> (f64, f64) {
        rotate_point_about(
            x,
            y,
            self.center_x,
            self.center_y,
            self.rotation_deg.to_radians(),
        )
    }
}

fn rotate_point_about(x: f64, y: f64, cx: f64, cy: f64, rad: f64) -> (f64, f64) {
    let dx = x - cx;
    let dy = y - cy;
    let xr = dx * rad.cos() - dy * rad.sin();
    let yr = dx * rad.sin() + dy * rad.cos();
    (cx + xr, cy + yr)
}

#[cfg(test)]
mod tests {
    use super::AdjustmentScopeShape;

    #[test]
    fn full_frame_scope_matches_canvas() {
        let scope =
            AdjustmentScopeShape::from_transform(1920, 1080, 1.0, 0.0, 0.0, 0.0, 0, 0, 0, 0);
        assert!(scope.is_full_frame(1920, 1080));
        assert!(scope.contains_pixel(0, 0));
        assert!(scope.contains_pixel(1919, 1079));
    }

    #[test]
    fn crop_and_scale_shrink_scope() {
        let scope =
            AdjustmentScopeShape::from_transform(1000, 500, 0.5, 1.0, 0.0, 0.0, 100, 0, 0, 50);
        assert!(!scope.is_full_frame(1000, 500));
        assert_eq!(scope.pixel_bounds(1000, 500), Some((550, 125, 1000, 350)));
        assert!(scope.contains_pixel(600, 150));
        assert!(!scope.contains_pixel(525, 150));
    }

    #[test]
    fn rotation_expands_axis_aligned_bounds() {
        let scope =
            AdjustmentScopeShape::from_transform(1000, 1000, 0.5, 0.0, 0.0, 45.0, 0, 0, 0, 0);
        let bounds = scope
            .pixel_bounds(1000, 1000)
            .expect("rotated scope bounds");
        assert!(bounds.0 < 250);
        assert!(bounds.2 > 750);
        assert!(scope.contains_pixel(500, 500));
    }
}
