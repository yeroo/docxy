//! Talking to a `--harness` instance: send verbs, ask where a region is, take
//! its picture.

use crate::capture::{Capture, capture_pid};
use crate::image::{Image, RectPx};
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
    /// The rect is read BEFORE the capture and both come after the same
    /// settle, so a window that moved between the two would be caught by the
    /// crop landing outside the image rather than by silently cropping the
    /// wrong place — `Image::crop` reports that with both rectangles in the
    /// message.
    pub fn shot(&self, region: &str) -> Result<(Image, Capture), String> {
        let r = self.settle(region)?;
        let cap = self.capture()?;
        let img = cap.image.crop(r.rect.relative_to(cap.origin))?;
        Ok((img, cap))
    }
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
