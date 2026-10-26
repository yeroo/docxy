//! Talking to a `--harness` instance: send verbs, ask where a region is, take
//! its picture.

use crate::capture::{Capture, capture_pid};
use crate::expect::{BorderCheck, BorderExpect, check_border_clipped};
use crate::image::{Clip, Image, RectPx, clipped};
use crate::probe::ProbeOpts;
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

/// How many captures [`steady_shot`] takes, at most, looking for two in a row
/// that agree. A region with a blinking caret in it may never agree; it is then
/// judged on its last capture, as before the wait existed.
const STEADY_ATTEMPTS: usize = 4;

/// How long [`judge_until`] keeps re-polling a border that reads wrong.
pub const BORDER_DEADLINE: Duration = Duration::from_secs(2);

/// And how many steady shots it takes at most, whatever the clock says, so a
/// slow capture cannot keep it looping.
const BORDER_POLLS: usize = 8;

/// Two shots of a region that agree pixel for pixel, and on which edges the
/// region ran off the capture.
fn same_pixels(a: &Shot, b: &Shot) -> bool {
    a.image == b.image && a.clipped == b.clipped
}

/// A region's shot once the capture has caught up with the app.
///
/// ⚠️ [`Driver::settle`] counts the frames the app *drew*, which is not the
/// frame Windows *presents*: `PrintWindow` can return the one before, with the
/// geometry from the new one (#1783). So shots are taken until two in a row
/// agree, each settled twice over, with frames drawn in between. That reduces
/// the chance of judging a stale picture but does not prove the picture fresh:
/// two stale captures in a row would agree too.
///
/// Returns the shot, whether two agreed, and how many captures it took.
fn steady_shot<F>(mut shoot: F) -> Result<(Shot, bool, usize), String>
where
    F: FnMut() -> Result<Shot, String>,
{
    let mut prev = shoot()?;
    let mut captures = 1;
    while captures < STEADY_ATTEMPTS {
        let next = shoot()?;
        captures += 1;
        if same_pixels(&prev, &next) {
            return Ok((next, true, captures));
        }
        prev = next;
    }
    Ok((prev, false, captures))
}

/// A border check and the shot it was made on.
pub struct Judged<C> {
    pub shot: Shot,
    pub check: C,
    /// Steady shots judged, the reported one included.
    pub attempts: usize,
    /// Captures taken over all of them.
    pub captures: usize,
    /// Whether the reported shot's region agreed across two captures.
    pub steady: bool,
}

impl<C> Judged<C> {
    /// What to add after the capture's own description on a failure: empty
    /// when the first steady shot was the one judged.
    pub fn polling(&self) -> String {
        let mut s = String::new();
        if self.attempts > 1 || self.captures > 2 || !self.steady {
            s.push_str(&format!(
                "; {} attempt{}, {} captures",
                self.attempts,
                if self.attempts == 1 { "" } else { "s" },
                self.captures
            ));
        }
        if !self.steady {
            s.push_str("; region not steady");
        }
        s
    }
}

/// Judge a region on a steady shot ([`steady_shot`]), and while the verdict
/// is a failure, take another until it passes or `deadline` has passed since
/// the first (at most [`BORDER_POLLS`]). The verdict reported is the last.
///
/// A verdict that `done` calls final (it passed, or a side could not be read
/// at all, which a new picture cannot change) is returned at once, and so is
/// a shot that fails outright (a moved window, a hung app).
pub fn judge_until<C, F, J, D, N>(
    mut shoot: F,
    mut judge: J,
    done: D,
    deadline: Duration,
    mut now: N,
) -> Result<Judged<C>, String>
where
    F: FnMut() -> Result<Shot, String>,
    J: FnMut(&Shot) -> C,
    D: Fn(&C) -> bool,
    N: FnMut() -> Instant,
{
    let start = now();
    let mut captures = 0;
    let mut attempts = 0;
    loop {
        let (shot, steady, n) = steady_shot(&mut shoot)?;
        captures += n;
        attempts += 1;
        let check = judge(&shot);
        if done(&check) || attempts >= BORDER_POLLS || now().duration_since(start) >= deadline {
            return Ok(Judged {
                shot,
                check,
                attempts,
                captures,
                steady,
            });
        }
    }
}

/// [`judge_until`] for a border expectation: the check both the runner's
/// `assert border` and the CLI's `assert` make.
pub fn judge_border<F>(
    exp: &BorderExpect,
    opts: ProbeOpts,
    shoot: F,
) -> Result<Judged<BorderCheck>, String>
where
    F: FnMut() -> Result<Shot, String>,
{
    judge_until(
        shoot,
        |s| check_border_clipped(exp, &s.image, opts, s.clipped),
        |c: &BorderCheck| c.passed() || !c.errors.is_empty(),
        BORDER_DEADLINE,
        Instant::now,
    )
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
    ///
    /// Windows photographs the window from outside with `PrintWindow`. macOS
    /// cannot: a harness window is never on screen, and reading another
    /// process's pixels needs Screen Recording permission. So there the app
    /// renders its last drawn frame offscreen and hands the pixels back through
    /// its sandbox — see [`crate::capture::offscreen_capture`]. The app has to
    /// be built with the suite's `harness-capture` feature for that verb to
    /// answer; a default build refuses it and says so.
    pub fn capture(&self) -> Result<Capture, String> {
        if cfg!(target_os = "macos") {
            let reply = self.call("capture", ctlcore::json::Json::Obj(Vec::new()))?;
            crate::capture::offscreen_capture(&reply, |p: &std::path::Path| std::fs::read(p))
        } else {
            capture_pid(self.pid())
        }
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
    use crate::expect::parse_border;

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

    // ---- judging on a steady shot (#1783) ---------------------------------

    const TEAL: [u8; 4] = [0x2a, 0xa7, 0x9b, 0xff];
    const WHITE: [u8; 4] = [0xff, 0xff, 0xff, 0xff];

    /// A 30x20 white crop, with a 2 px box of `edge` round it when given.
    fn crop(edge: Option<[u8; 4]>) -> Image {
        let (w, h) = (30, 20);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let on_edge = x < 2 || y < 2 || x >= w - 2 || y >= h - 2;
                let c = match edge {
                    Some(c) if on_edge => c,
                    _ => WHITE,
                };
                img.set_pixel(x, y, c);
            }
        }
        img
    }

    fn shot_of(image: Image) -> Shot {
        Shot {
            image,
            capture: capture(),
            clipped: Clip::default(),
        }
    }

    /// Shots handed out in order, counted.
    fn shots(images: Vec<Image>) -> impl FnMut() -> Result<Shot, String> {
        let mut it = images.into_iter();
        move || {
            Ok(shot_of(
                it.next().expect("asked for more shots than the test has"),
            ))
        }
    }

    /// A clock that moves `step` every time it is read.
    fn clock(step: Duration) -> impl FnMut() -> Instant {
        let start = Instant::now();
        let mut reads = 0u32;
        move || {
            let t = start + step * reads;
            reads += 1;
            t
        }
    }

    fn final_verdict(c: &BorderCheck) -> bool {
        c.passed() || !c.errors.is_empty()
    }

    fn judge(
        exp: &BorderExpect,
        shoot: impl FnMut() -> Result<Shot, String>,
        now: impl FnMut() -> Instant,
    ) -> Judged<BorderCheck> {
        let mut judged = 0;
        let j = judge_until(
            shoot,
            |s| {
                judged += 1;
                check_border_clipped(exp, &s.image, ProbeOpts::default(), s.clipped)
            },
            final_verdict,
            BORDER_DEADLINE,
            now,
        )
        .expect("the fake shots never fail");
        assert_eq!(judged, j.attempts, "each attempt is judged once");
        j
    }

    #[test]
    fn a_border_is_judged_only_once_two_captures_agree() {
        // The stale frame of #1783: no bar yet. Judged alone it would fail.
        let exp = parse_border("border bar:1 solid teal").unwrap();
        let j = judge(
            &exp,
            shots(vec![crop(None), crop(Some(TEAL)), crop(Some(TEAL))]),
            clock(Duration::ZERO),
        );
        assert!(j.check.passed(), "{}", j.check.report(None));
        assert_eq!((j.attempts, j.captures, j.steady), (1, 3, true));
        assert_eq!(
            j.shot.image,
            crop(Some(TEAL)),
            "the evidence is the judged shot"
        );
    }

    #[test]
    fn a_steady_passing_border_costs_one_extra_capture() {
        let exp = parse_border("border bar:1 solid teal").unwrap();
        let j = judge(
            &exp,
            shots(vec![crop(Some(TEAL)), crop(Some(TEAL))]),
            clock(Duration::ZERO),
        );
        assert!(j.check.passed());
        assert_eq!((j.attempts, j.captures), (1, 2));
        assert_eq!(j.polling(), "", "nothing to say about a first steady shot");
    }

    #[test]
    fn a_region_that_never_steadies_is_judged_on_its_last_capture() {
        // A blinking caret, say: no two captures in a row agree.
        let blue = [0, 0, 0xff, 0xff];
        let images = vec![
            crop(Some(TEAL)),
            crop(None),
            crop(Some(blue)),
            crop(Some(TEAL)),
        ];
        assert_eq!(images.len(), STEADY_ATTEMPTS);
        let exp = parse_border("border bar:1 solid teal").unwrap();
        let j = judge(&exp, shots(images), clock(Duration::ZERO));
        assert!(j.check.passed(), "judged on the last capture, the teal one");
        assert_eq!(
            (j.attempts, j.captures, j.steady),
            (1, STEADY_ATTEMPTS, false)
        );
        assert!(j.polling().contains("region not steady"), "{}", j.polling());
    }

    #[test]
    fn a_failing_border_is_polled_until_the_deadline_and_reports_the_last_attempt() {
        let exp = parse_border("border bar:1 solid teal").unwrap();
        // Steady and wrong every time: grey, then white. One second passes
        // per read of the clock, so the 2 s deadline ends it after the second.
        let grey = [0x80, 0x80, 0x80, 0xff];
        let j = judge(
            &exp,
            shots(vec![
                crop(Some(grey)),
                crop(Some(grey)),
                crop(None),
                crop(None),
            ]),
            clock(Duration::from_secs(1)),
        );
        assert!(!j.check.passed());
        assert!(j.check.errors.is_empty(), "a reading, not a geometry error");
        assert_eq!((j.attempts, j.captures, j.steady), (2, 4, true));
        assert_eq!(
            j.shot.image,
            crop(None),
            "the last attempt is the one reported"
        );
        assert_eq!(j.polling(), "; 2 attempts, 4 captures");
    }

    #[test]
    fn a_failing_border_stops_polling_at_the_cap_even_with_time_left() {
        let exp = parse_border("border bar:1 solid teal").unwrap();
        let j = judge(
            &exp,
            shots(vec![crop(None); 2 * BORDER_POLLS]),
            clock(Duration::ZERO),
        );
        assert!(!j.check.passed());
        assert_eq!((j.attempts, j.captures), (BORDER_POLLS, 2 * BORDER_POLLS));
    }

    #[test]
    fn a_stale_frame_without_the_border_cannot_pass_a_no_border_assertion() {
        // The stale capture lacks the border, so judged alone it would pass a
        // `no border`; the fresh frames that follow disagree with it, so it is
        // never judged, and the steady fresh frame fails as it should.
        let exp = parse_border("no border bar:1").unwrap();
        let j = judge(
            &exp,
            shots(vec![crop(None), crop(Some(TEAL)), crop(Some(TEAL))]),
            clock(Duration::from_secs(3)),
        );
        assert!(!j.check.passed(), "{}", j.check.report(None));
        assert_eq!(j.shot.image, crop(Some(TEAL)));
        assert_eq!(j.attempts, 1, "the deadline had passed after one attempt");
    }

    #[test]
    fn a_side_that_cannot_be_read_is_not_polled() {
        // A clipped side is geometry: another picture cannot change it.
        let exp = parse_border("border bar:1 solid teal").unwrap();
        let mut taken = 0;
        let j = judge_border(&exp, ProbeOpts::default(), || {
            taken += 1;
            let mut s = shot_of(crop(Some(TEAL)));
            s.clipped.right = true;
            Ok(s)
        })
        .unwrap();
        assert!(!j.check.errors.is_empty());
        assert_eq!((j.attempts, taken), (1, 2));
    }

    #[test]
    fn a_shot_that_fails_ends_the_judging_with_its_error() {
        let exp = parse_border("border bar:1 solid teal").unwrap();
        let mut n = 0;
        let e = judge_border(&exp, ProbeOpts::default(), || {
            n += 1;
            if n == 1 {
                Ok(shot_of(crop(None)))
            } else {
                Err("region 'bar:1' moved".to_string())
            }
        })
        .err()
        .expect("the shot's error is the step's");
        assert!(e.contains("moved"), "{e}");
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
