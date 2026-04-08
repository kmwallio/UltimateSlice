#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AdjustmentScopeShape {
    pub(crate) scale: f64,
    pub(crate) position_x: f64,
    pub(crate) position_y: f64,
    pub(crate) crop_left_norm: f64,
    pub(crate) crop_right_norm: f64,
    pub(crate) crop_top_norm: f64,
    pub(crate) crop_bottom_norm: f64,
    pub(crate) rotation_deg: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ResolvedAdjustmentScopeShape {
    center_x: f64,
    center_y: f64,
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    rotation_deg: f64,
}

pub(crate) fn adjustment_canvas_geometry(
    width: f64,
    height: f64,
    scale: f64,
    position_x: f64,
    position_y: f64,
) -> (f64, f64, f64, f64) {
    use crate::model::transform_bounds::{
        ADJUSTMENT_POSITION_MAX, ADJUSTMENT_POSITION_MIN, SCALE_MAX, SCALE_MIN,
    };
    let scale = scale.clamp(SCALE_MIN, SCALE_MAX);
    let center_x = width / 2.0
        + position_x.clamp(ADJUSTMENT_POSITION_MIN, ADJUSTMENT_POSITION_MAX) * width / 2.0;
    let center_y = height / 2.0
        + position_y.clamp(ADJUSTMENT_POSITION_MIN, ADJUSTMENT_POSITION_MAX) * height / 2.0;
    (center_x, center_y, width * scale, height * scale)
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
        use crate::model::transform_bounds::{
            ADJUSTMENT_POSITION_MAX, ADJUSTMENT_POSITION_MIN, SCALE_MAX, SCALE_MIN,
        };
        Self {
            scale: scale.clamp(SCALE_MIN, SCALE_MAX),
            position_x: position_x.clamp(ADJUSTMENT_POSITION_MIN, ADJUSTMENT_POSITION_MAX),
            position_y: position_y.clamp(ADJUSTMENT_POSITION_MIN, ADJUSTMENT_POSITION_MAX),
            crop_left_norm: crop_left.max(0) as f64 / pw,
            crop_right_norm: crop_right.max(0) as f64 / pw,
            crop_top_norm: crop_top.max(0) as f64 / ph,
            crop_bottom_norm: crop_bottom.max(0) as f64 / ph,
            rotation_deg,
        }
    }

    pub(crate) fn is_full_frame(&self, out_w: u32, out_h: u32) -> bool {
        self.resolve(out_w as usize, out_h as usize)
            .is_full_frame(out_w, out_h)
    }

    pub(crate) fn pixel_bounds(
        &self,
        width: usize,
        height: usize,
    ) -> Option<(usize, usize, usize, usize)> {
        self.resolve(width, height).pixel_bounds(width, height)
    }

    pub(crate) fn contains_pixel(&self, x: usize, y: usize, width: usize, height: usize) -> bool {
        self.resolve(width, height).contains_pixel(x, y)
    }

    pub(crate) fn resolve(&self, width: usize, height: usize) -> ResolvedAdjustmentScopeShape {
        let pw = width.max(1) as f64;
        let ph = height.max(1) as f64;
        let (center_x, center_y, clip_width, clip_height) =
            adjustment_canvas_geometry(pw, ph, self.scale, self.position_x, self.position_y);
        let half_w = clip_width / 2.0;
        let half_h = clip_height / 2.0;
        let crop_left = self.crop_left_norm * pw;
        let crop_right = self.crop_right_norm * pw;
        let crop_top = self.crop_top_norm * ph;
        let crop_bottom = self.crop_bottom_norm * ph;
        let left = center_x - half_w + crop_left * self.scale;
        let mut right = center_x + half_w - crop_right * self.scale;
        let top = center_y - half_h + crop_top * self.scale;
        let mut bottom = center_y + half_h - crop_bottom * self.scale;
        if right < left {
            right = left;
        }
        if bottom < top {
            bottom = top;
        }
        ResolvedAdjustmentScopeShape {
            center_x,
            center_y,
            left,
            right,
            top,
            bottom,
            rotation_deg: self.rotation_deg,
        }
    }
}

impl ResolvedAdjustmentScopeShape {
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
        assert!(scope.contains_pixel(0, 0, 1920, 1080));
        assert!(scope.contains_pixel(1919, 1079, 1920, 1080));
    }

    #[test]
    fn crop_and_scale_shrink_scope() {
        let scope =
            AdjustmentScopeShape::from_transform(1000, 500, 0.5, 1.0, 0.0, 0.0, 100, 0, 0, 50);
        assert!(!scope.is_full_frame(1000, 500));
        assert_eq!(scope.pixel_bounds(1000, 500), Some((800, 125, 1000, 350)));
        assert!(scope.contains_pixel(850, 150, 1000, 500));
        assert!(!scope.contains_pixel(775, 150, 1000, 500));
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
        assert!(scope.contains_pixel(500, 500, 1000, 1000));
    }

    #[test]
    fn scope_scales_with_preview_resolution() {
        let scope =
            AdjustmentScopeShape::from_transform(1920, 1080, 0.5, 0.5, -0.25, 0.0, 120, 60, 30, 0);
        let full_bounds = scope.pixel_bounds(1920, 1080).expect("full-res bounds");
        let half_bounds = scope.pixel_bounds(960, 540).expect("half-res bounds");
        let expected_half = (
            full_bounds.0 / 2,
            full_bounds.1 / 2,
            full_bounds.2 / 2,
            full_bounds.3 / 2,
        );
        assert!((half_bounds.0 as isize - expected_half.0 as isize).abs() <= 1);
        assert!((half_bounds.1 as isize - expected_half.1 as isize).abs() <= 1);
        assert!((half_bounds.2 as isize - expected_half.2 as isize).abs() <= 1);
        assert!((half_bounds.3 as isize - expected_half.3 as isize).abs() <= 1);
        assert!(scope.contains_pixel(half_bounds.0 + 5, half_bounds.1 + 5, 960, 540));
        assert!(!scope.contains_pixel(
            half_bounds.0.saturating_sub(5),
            half_bounds.1 + 5,
            960,
            540
        ));
    }

    #[test]
    fn translated_full_frame_scope_moves_off_center() {
        let scope =
            AdjustmentScopeShape::from_transform(1000, 500, 1.0, 0.5, -0.5, 0.0, 0, 0, 0, 0);
        assert_eq!(scope.pixel_bounds(1000, 500), Some((250, 0, 1000, 375)));
        assert!(scope.contains_pixel(750, 125, 1000, 500));
        assert!(!scope.contains_pixel(200, 450, 1000, 500));
    }
}
