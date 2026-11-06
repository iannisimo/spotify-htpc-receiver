//! Frame-time-driven quality: steps down when frames run long, back up when there is room.

use crate::config::VisualizerConfig;

/// What the visualizer renders with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quality {
    /// Render resolution as a fraction of the window (0.1–1.0).
    pub scale: f32,
    pub mesh_w: u32,
    pub mesh_h: u32,
    pub fps_cap: u32,
}

/// Steps from `base` down to `floor`; every field moves a share of the way at each step.
pub const LEVELS: u32 = 4;
/// Consecutive frames over budget before stepping down.
const SLOW_FRAMES: u32 = 30;
/// Consecutive frames under [`FAST_SHARE`] of the budget before stepping up.
const FAST_FRAMES: u32 = 300;
const FAST_SHARE: f32 = 0.7;
/// Smallest mesh the floor may use.
const MIN_MESH: u32 = 8;

/// Quality bounds from the config: the configured mesh and FPS cap at full resolution, down to
/// `min_scale`, `min_fps` and half the mesh (at least 8, never above the base).
pub fn quality_range(cfg: &VisualizerConfig) -> (Quality, Quality) {
    let base = Quality {
        scale: 1.0,
        mesh_w: cfg.mesh_width,
        mesh_h: cfg.mesh_height,
        fps_cap: cfg.fps_cap,
    };
    let floor = Quality {
        scale: cfg.min_scale,
        mesh_w: (cfg.mesh_width / 2).max(MIN_MESH).min(cfg.mesh_width),
        mesh_h: (cfg.mesh_height / 2).max(MIN_MESH).min(cfg.mesh_height),
        fps_cap: cfg.min_fps.min(cfg.fps_cap),
    };
    (base, floor)
}

pub struct Adaptive {
    base: Quality,
    floor: Quality,
    /// 0 = base, [`LEVELS`] = floor.
    level: u32,
    current: Quality,
    slow_run: u32,
    fast_run: u32,
}

impl Adaptive {
    /// Starts at `base`. Floor fields above the base are treated as the base's.
    pub fn new(base: Quality, floor: Quality) -> Self {
        let floor = Quality {
            scale: floor.scale.min(base.scale),
            mesh_w: floor.mesh_w.min(base.mesh_w),
            mesh_h: floor.mesh_h.min(base.mesh_h),
            fps_cap: floor.fps_cap.min(base.fps_cap),
        };
        Adaptive {
            base,
            floor,
            level: 0,
            current: base,
            slow_run: 0,
            fast_run: 0,
        }
    }

    pub fn quality(&self) -> Quality {
        self.current
    }

    /// Records how long the last frame took and returns the quality for the next one. The
    /// budget is one frame at the current FPS cap.
    pub fn on_frame(&mut self, frame_ms: f32) -> Quality {
        let budget = 1000.0 / self.current.fps_cap.max(1) as f32;
        if frame_ms > budget {
            self.slow_run += 1;
            self.fast_run = 0;
        } else if frame_ms < FAST_SHARE * budget {
            self.fast_run += 1;
            self.slow_run = 0;
        } else {
            self.slow_run = 0;
            self.fast_run = 0;
        }
        if self.slow_run >= SLOW_FRAMES && self.level < LEVELS {
            self.set_level(self.level + 1);
        } else if self.fast_run >= FAST_FRAMES && self.level > 0 {
            self.set_level(self.level - 1);
        }
        self.current
    }

    fn set_level(&mut self, level: u32) {
        self.level = level;
        self.slow_run = 0;
        self.fast_run = 0;
        let t = level as f32 / LEVELS as f32;
        let lerp = |b: f32, f: f32| b + (f - b) * t;
        let lerp_u = |b: u32, f: u32| lerp(b as f32, f as f32).round() as u32;
        self.current = Quality {
            scale: lerp(self.base.scale, self.floor.scale),
            mesh_w: lerp_u(self.base.mesh_w, self.floor.mesh_w),
            mesh_h: lerp_u(self.base.mesh_h, self.floor.mesh_h),
            fps_cap: lerp_u(self.base.fps_cap, self.floor.fps_cap),
        };
        if level == LEVELS {
            self.current = self.floor; // exact, without rounding drift
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Quality {
        Quality {
            scale: 1.0,
            mesh_w: 48,
            mesh_h: 32,
            fps_cap: 60,
        }
    }

    fn floor() -> Quality {
        Quality {
            scale: 0.5,
            mesh_w: 24,
            mesh_h: 16,
            fps_cap: 30,
        }
    }

    /// Feeds `n` frames of `ms` each and returns the last quality.
    fn frames(a: &mut Adaptive, n: u32, ms: f32) -> Quality {
        (0..n).map(|_| a.on_frame(ms)).last().unwrap()
    }

    fn budget(q: Quality) -> f32 {
        1000.0 / q.fps_cap as f32
    }

    #[test]
    fn starts_at_base() {
        assert_eq!(Adaptive::new(base(), floor()).quality(), base());
    }

    #[test]
    fn degrades_after_sustained_slow_frames() {
        let mut a = Adaptive::new(base(), floor());
        assert_eq!(frames(&mut a, 29, 20.0), base());
        let q = a.on_frame(20.0);
        assert!(q.scale < 1.0 && q.fps_cap < 60 && q.mesh_w < 48, "{q:?}");
        assert!(q.scale > floor().scale, "one step only: {q:?}");
    }

    #[test]
    fn single_slow_frame_does_not_degrade() {
        let mut a = Adaptive::new(base(), floor());
        for _ in 0..10 {
            frames(&mut a, 29, 20.0);
            a.on_frame(10.0); // breaks the run
        }
        assert_eq!(a.quality(), base());
    }

    #[test]
    fn recovers_after_sustained_fast_frames() {
        let mut a = Adaptive::new(base(), floor());
        let degraded = frames(&mut a, 30, 100.0);
        let fast = 0.69 * budget(degraded);
        assert_eq!(frames(&mut a, 299, fast), degraded);
        assert_eq!(a.on_frame(fast), base());
    }

    #[test]
    fn frame_between_70_percent_and_budget_holds_level() {
        let mut a = Adaptive::new(base(), floor());
        let degraded = frames(&mut a, 30, 100.0);
        let fast = 0.5 * budget(degraded);
        frames(&mut a, 299, fast);
        a.on_frame(0.8 * budget(degraded)); // neither slow nor fast: restarts the count
        assert_eq!(frames(&mut a, 299, fast), degraded);
        assert_eq!(a.on_frame(fast), base());
    }

    #[test]
    fn never_below_floor() {
        let mut a = Adaptive::new(base(), floor());
        assert_eq!(frames(&mut a, 30 * (LEVELS + 5), 1000.0), floor());
    }

    #[test]
    fn never_above_base() {
        let mut a = Adaptive::new(base(), floor());
        assert_eq!(frames(&mut a, 3000, 0.1), base());
    }

    #[test]
    fn steps_are_monotonic_down_to_floor() {
        let mut a = Adaptive::new(base(), floor());
        let mut prev = base();
        for _ in 0..LEVELS {
            let q = frames(&mut a, 30, 1000.0);
            assert!(
                q.scale < prev.scale && q.fps_cap < prev.fps_cap,
                "{prev:?} -> {q:?}"
            );
            assert!(q.mesh_w <= prev.mesh_w && q.mesh_h <= prev.mesh_h);
            prev = q;
        }
        assert_eq!(prev, floor());
    }

    #[test]
    fn floor_above_base_is_clamped_to_base() {
        let high_floor = Quality {
            scale: 2.0,
            mesh_w: 100,
            mesh_h: 100,
            fps_cap: 120,
        };
        let mut a = Adaptive::new(base(), high_floor);
        assert_eq!(frames(&mut a, 300, 1000.0), base());
    }

    #[test]
    fn range_from_config() {
        let mut cfg = crate::config::Config::default().visualizer;
        cfg.mesh_width = 48;
        cfg.mesh_height = 32;
        cfg.fps_cap = 60;
        cfg.min_scale = 0.5;
        cfg.min_fps = 30;
        let (b, f) = quality_range(&cfg);
        assert_eq!(b, base());
        assert_eq!(f, floor());
    }

    #[test]
    fn range_floor_mesh_has_a_minimum() {
        let mut cfg = crate::config::Config::default().visualizer;
        cfg.mesh_width = 10;
        cfg.mesh_height = 6;
        let (b, f) = quality_range(&cfg);
        assert_eq!((f.mesh_w, f.mesh_h), (8, 6));
        assert_eq!((b.mesh_w, b.mesh_h), (10, 6));
    }
}
