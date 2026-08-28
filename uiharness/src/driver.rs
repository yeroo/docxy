//! Talking to a `--harness` instance: send verbs, ask where a region is, take
//! its picture.

use crate::capture::{Capture, capture_pid};
use crate::image::{Clip, Image, RectPx, clipped};
use ctlcore::client::{Client, Instance, discover_live};
use ctlcore::json::Json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where a harness instance publishes its discovery file, given the sandbox
/// config root it was started with.
///
/// The same derivation the app does (`harness::control_dir`), and derived for
/// the same reason: `ctlcore::config_ctl_dir` would do its own `APPDATA`
/// lookup, which is not what `DOCXY_CONFIG_DIR` set, so the two sides could end
/// up looking in different sandboxes.
pub fn control_dir(config_root: &Path) -> PathBuf {
    config_root.join("suite").join("ctl")
}

/// How long [`Driver::connect`] waits for an instance to publish itself. A cold
/// `suite.exe` has a window up well inside this.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How long [`Driver::settle`] waits for a frame to be drawn.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// A moved window should cost one extra capture, not make a test flaky. Three
/// attempts cover an ordinary drag while still refusing a window that will not
/// stay put.
const CAPTURE_ATTEMPTS: usize = 3;

/// What the `rect` verb answered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegionRect {
    /// The region, in physical desktop pixels.
    pub rect: RectPx,
    /// The display's scale factor, for a test that wants to reason in logical
    /// units.
    pub scale: f32,
    /// Which frame the app had finished when it answered. See [`Driver::settle`].
    pub frame: u64,
}

fn same_geometry(a: RegionRect, b: RegionRect) -> bool {
    a.rect == b.rect && a.scale == b.scale
}

fn stable_shot<S, C>(region: &str, mut settle: S, mut capture: C) -> Result<Shot, String>
where
    S: FnMut() -> Result<RegionRect, String>,
    C: FnMut() -> Result<Capture, String>,
{
    let mut changed = None;
    for _ in 0..CAPTURE_ATTEMPTS {
        let before = settle()?;
        let cap = capture()?;
        // Settle again rather than merely asking for a rect. Cell and panel
        // geometry comes from layout probes, and an immediate rect after a
        // resize may still describe the pre-resize frame.
        let after = settle()?;
        if !same_geometry(before, after) {
            changed = Some((before, after));
            continue;
        }
        let want = before.rect.relative_to(cap.origin);
        let clipped = clipped(want, cap.image.w, cap.image.h);
        let image = cap.image.crop(want)?;
        return Ok(Shot {
            image,
            capture: cap,
            clipped,
        });
    }
    let (before, after) = changed.expect("capture attempts always record changed geometry");
    Err(format!(
        "region '{region}' moved or rescaled during {CAPTURE_ATTEMPTS} captures \
         (before: {:?}, frame {}; after: {:?}, frame {}); keep the window still and retry",
        before.rect, before.frame, after.rect, after.frame
    ))
}

/// A connection to one harness instance.
pub struct Driver {
    client: Client,
}

impl Driver {
    /// Attach to the instance publishing itself under `ctl_dir`. With `name`,
    /// that instance by name; without, the only live one — refusing when there
    /// is more than one, because picking arbitrarily would send a test's verbs
    /// to whichever process happened to sort first.
    pub fn connect(ctl_dir: &Path, name: Option<&str>) -> Result<Driver, String> {
        Driver::connect_within(ctl_dir, name, CONNECT_TIMEOUT)
    }

    /// [`Driver::connect`] with the wait spelled out — for a caller that knows
    /// the instance is already up, or a test that must not sit out the full
    /// start-up allowance.
    pub fn connect_within(
        ctl_dir: &Path,
        name: Option<&str>,
        timeout: Duration,
    ) -> Result<Driver, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let live: Vec<Instance> = discover_live(ctl_dir);
            let picked = match name {
                Some(n) => live.into_iter().find(|i| i.instance == n),
                None => match live.len() {
                    1 => live.into_iter().next(),
                    0 => None,
                    _ => {
                        let names: Vec<&str> = live.iter().map(|i| i.instance.as_str()).collect();
                        return Err(format!(
                            "{} harness instances are live in {} ({}); name one with --instance",
                            names.len(),
                            ctl_dir.display(),
                            names.join(", ")
                        ));
                    }
                },
            };
            if let Some(i) = picked {
                return Ok(Driver { client: i.client() });
            }
            if Instant::now() >= deadline {
                return Err(match name {
                    Some(n) => format!(
                        "no live harness instance called '{n}' in {}",
                        ctl_dir.display()
                    ),
                    None => format!(
                        "no live harness instance in {} — was the suite started with --harness \
                         and DOCXY_CONFIG_DIR pointing here?",
                        ctl_dir.display()
                    ),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let i = self.client.instance();
        write!(f, "Driver({} pid {} port {})", i.instance, i.pid, i.port)
    }
}

impl Driver {
    pub fn instance(&self) -> &Instance {
        self.client.instance()
    }

    /// The instance's process id — what [`Driver::capture`] finds the window
    /// from.
    pub fn pid(&self) -> u32 {
        self.client.instance().pid
    }

    /// Send a verb.
    pub fn call(&self, verb: &str, args: Json) -> Result<Json, String> {
        self.client.call(verb, args)
    }

    /// Where a region is, right now, as the app last laid it out.
    pub fn rect(&self, region: &str) -> Result<RegionRect, String> {
        let j = self.call(
            "rect",
            Json::obj(vec![("region", Json::Str(region.to_string()))]),
        )?;
        let num = |k: &str| -> Result<f64, String> {
            j.get(k)
                .and_then(Json::as_f64)
                .ok_or_else(|| format!("the rect reply has no '{k}': {j}"))
        };
        Ok(RegionRect {
            rect: RectPx {
                x: num("x")? as i32,
                y: num("y")? as i32,
                w: num("w")?.max(0.0) as u32,
                h: num("h")?.max(0.0) as u32,
            },
            scale: num("scale")? as f32,
            frame: num("frame")?.max(0.0) as u64,
        })
    }

    /// How many frames the app has drawn. Asking also requests one more, so a
    /// caller waiting for the count to move is not waiting on an idle app.
    pub fn frame(&self) -> Result<u64, String> {
        let j = self.call("frame", Json::Obj(Vec::new()))?;
        j.get("frame")
            .and_then(Json::as_f64)
            .map(|f| f.max(0.0) as u64)
            .ok_or_else(|| format!("the frame reply has no 'frame': {j}"))
    }

    /// Wait until the app has drawn a frame that is certainly newer than
    /// everything sent so far, then return the region's rectangle from it.
    ///
    /// This is not belt and braces, and it is why the count has to move
    /// **twice**. A verb only marks the view dirty, so when its reply goes out
    /// the frame that shows what it did may not have been laid out yet — and
    /// the probe geometry `rect` answers from is a frame older still, because a
    /// probe only records during the layout of the frame it belongs to and the
    /// app publishes each frame's probes as the next one starts.
    ///
    /// So: read the count as `f0`. Waiting for `f0 + 1` is not enough — that
    /// frame may have begun before the verb landed. At `f0 + 2`, the frame in
    /// between began after the read, hence after the verb, and its probes are
    /// what `rect` now answers from. Measuring earlier is how a test ends up
    /// asserting on the picture before the change, which is worse than failing.
    pub fn settle(&self, region: &str) -> Result<RegionRect, String> {
        let target = self.frame()?.saturating_add(2);
        let deadline = Instant::now() + FRAME_TIMEOUT;
        loop {
            std::thread::sleep(Duration::from_millis(8));
            let now = self.frame()?;
            if now >= target {
                return self.rect(region);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the app drew no new frame within {FRAME_TIMEOUT:?} \
                     (still frame {now}, waiting for {target}); is it hung?"
                ));
            }
        }
    }

    /// The whole window, as pixels.
    pub fn capture(&self) -> Result<Capture, String> {
        capture_pid(self.pid())
    }

    /// A capture cropped to `region`: settle first, then photograph, then cut.
    ///
    /// The rect is read before and after the capture. If its desktop position,
    /// size or scale changed in between, the capture is retried: combining
    /// geometry from one window position with pixels from another can yield a
    /// valid but shifted crop that clipping alone cannot detect. The frame may
    /// advance — asking for the second rect itself causes a normal redraw.
    pub fn shot(&self, region: &str) -> Result<Shot, String> {
        stable_shot(region, || self.settle(region), || self.capture())
    }
}

/// One region's pixels: the crop, the capture it came out of, and which of the
/// crop's edges are the capture's own because the region ran off it.
pub struct Shot {
    pub image: Image,
    pub capture: Capture,
    pub clipped: Clip,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_control_directory_is_derived_from_the_sandbox_root() {
        let root = Path::new("target").join("harness-cfg");
        assert_eq!(
            control_dir(&root),
            root.join("suite").join("ctl"),
            "must match the app's harness::control_dir, or the two look in \
             different places"
        );
    }

    #[test]
    fn a_capture_snapshot_requires_stable_geometry_but_allows_a_new_frame() {
        let a = RegionRect {
            rect: RectPx::new(10, 20, 30, 40),
            scale: 1.0,
            frame: 7,
        };
        let mut b = a;
        assert!(same_geometry(a, b));
        b.rect.x += 1;
        assert!(!same_geometry(a, b), "a move must invalidate the capture");
        b = a;
        b.frame += 1;
        assert!(
            same_geometry(a, b),
            "a normal redraw does not move the crop"
        );
        b = a;
        b.scale = 1.25;
        assert!(
            !same_geometry(a, b),
            "a DPI change must invalidate the capture"
        );
    }

    fn capture() -> Capture {
        Capture {
            image: Image::new(100, 100),
            origin: (0, 0),
            how: crate::capture::How::PrintWindow,
        }
    }

    #[test]
    fn shot_retries_changed_geometry_and_uses_the_stable_capture() {
        let a = RegionRect {
            rect: RectPx::new(10, 10, 20, 20),
            scale: 1.0,
            frame: 1,
        };
        let b = RegionRect {
            rect: RectPx::new(20, 10, 20, 20),
            scale: 1.0,
            frame: 2,
        };
        let mut rects = [a, b, b, b].into_iter();
        let mut captures = 0;
        let shot = stable_shot(
            "grid",
            || Ok(rects.next().expect("two attempts need four settled rects")),
            || {
                captures += 1;
                Ok(capture())
            },
        )
        .expect("the second stable attempt succeeds");
        assert_eq!(captures, 2);
        assert_eq!(shot.image.w, 20);
        assert_eq!(shot.image.h, 20);
    }

    #[test]
    fn shot_refuses_three_changed_attempts() {
        let mut frame = 0;
        let mut captures = 0;
        let err = stable_shot(
            "grid",
            || {
                frame += 1;
                Ok(RegionRect {
                    rect: RectPx::new(frame, 10, 20, 20),
                    scale: 1.0,
                    frame: frame as u64,
                })
            },
            || {
                captures += 1;
                Ok(capture())
            },
        )
        .err()
        .expect("geometry that never stabilizes must be refused");
        assert_eq!(captures, CAPTURE_ATTEMPTS);
        assert!(err.contains("moved or rescaled"), "{err}");
    }

    /// A connect against a directory with nothing in it must fail with a
    /// message that says what to do, not hang for the full timeout.
    #[test]
    fn connecting_to_a_directory_with_no_instances_says_so() {
        let dir = std::env::temp_dir().join(format!("uiharness-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A named instance is looked for once per poll; the message is what is
        // under test, so use a short-circuiting path: a name that cannot exist.
        let e =
            Driver::connect_within(&dir, Some("suite-nonexistent"), Duration::ZERO).unwrap_err();
        assert!(e.contains("suite-nonexistent"), "{e}");
        // And the unnamed case names the flag that would have started one.
        let e = Driver::connect_within(&dir, None, Duration::ZERO).unwrap_err();
        assert!(e.contains("--harness"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
