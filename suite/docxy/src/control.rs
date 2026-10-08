//! Project control policy over live tabs and the shared control server pump.
use crate::*;
use ctlcore::json::Json;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Effect {
    pub repaint: bool,
    pub activity: bool,
    pub focus: Option<usize>,
}

impl Effect {
    /// Whether this effect flashes the agwinterm pane's activity dot. Never
    /// for a harness instance (#697): it only inherited the pane of whatever
    /// terminal launched it, and must not spawn `agwintermctl` against it.
    pub(crate) fn signals_activity(&self, harness: bool) -> bool {
        self.activity && !harness
    }
}

/// Match a `tab` argument (an index, or a case-insensitive title/path
/// substring) against the open tabs, Projects only when `project_only`. The
/// one rule `proj.*` and the harness's `tab-select` share.
pub(crate) fn match_tab(tabs: &[DocTab], arg: &Json, project_only: bool) -> Result<usize, String> {
    let noun = if project_only { "Project tab" } else { "tab" };
    let wanted = |t: &DocTab| !project_only || t.kind == Kind::Project;
    match arg {
        Json::Num(n)
            if n.is_finite()
                && n.fract() == 0.
                && *n >= 0.
                && *n < (usize::MAX as u128 + 1) as f64 =>
        {
            let i = *n as usize;
            let tab = tabs.get(i).ok_or_else(|| format!("no tab at index {i}"))?;
            if !wanted(tab) {
                return Err(format!("tab {i} is not a Project"));
            }
            Ok(i)
        }
        Json::Str(text) if !text.is_empty() => {
            let needle = text.to_lowercase();
            let hits: Vec<usize> = tabs
                .iter()
                .enumerate()
                .filter(|(_, t)| {
                    wanted(t)
                        && (t.title.to_lowercase().contains(&needle)
                            || t.path.as_ref().is_some_and(|p| {
                                p.to_string_lossy().to_lowercase().contains(&needle)
                            }))
                })
                .map(|(i, _)| i)
                .collect();
            match hits.as_slice() {
                [] => Err(format!("no {noun} matches '{text}'")),
                [i] => Ok(*i),
                _ => Err(format!(
                    "several {noun}s match '{text}' ({})",
                    hits.iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
        _ => Err("'tab' must be a tab index or a title/path substring".into()),
    }
}

pub(crate) fn resolve_project_tab(
    tabs: &[DocTab],
    active: usize,
    arg: Option<&Json>,
) -> Result<usize, String> {
    let index = match arg {
        None => {
            if !tabs.get(active).is_some_and(|t| t.kind == Kind::Project) {
                return Err("the active tab is not a Project".into());
            }
            active
        }
        Some(arg) => match_tab(tabs, arg, true)?,
    };
    if !matches!(tabs[index].surface, Surface::Project(_)) {
        return Err("that tab could not be loaded".into());
    }
    Ok(index)
}

fn path_info(tab: &DocTab, index: usize) -> Json {
    let Surface::Project(v) = &tab.surface else {
        unreachable!("resolved loaded Project")
    };
    let path = tab.path.as_ref().map(|p| p.to_string_lossy());
    let Json::Obj(mut fields) = projctl::path_info(path.as_deref(), &v.ed) else {
        unreachable!()
    };
    fields.push(("tab".into(), Json::Num(index as f64)));
    fields.push(("imported".into(), Json::Bool(is_imported(tab))));
    fields.extend(project_cell_state(v));
    Json::Obj(fields)
}

/// `proj.new`'s arguments: none. The blank plan is the app's (Backstage ›
/// New › Project), so the verb takes nothing that would make it the verb's.
pub(crate) fn check_new_project_args(args: &Json) -> Result<(), String> {
    if args.get("tab").is_some() {
        return Err("proj.new does not take 'tab'".into());
    }
    if args.get("name").is_some() {
        return Err(
            "proj.new does not take 'name'; name the plan with proj.save {\"path\"}".into(),
        );
    }
    Ok(())
}

/// `proj.new`'s reply: `proj.path` for the tab it made.
pub(crate) fn new_project_reply(tabs: &[DocTab], index: usize) -> Json {
    path_info(&tabs[index], index)
}

fn loaded_project(path: &Path) -> Result<DocTab, String> {
    let tab = project_tab_from_path(path);
    if matches!(tab.surface, Surface::Project(_)) {
        Ok(tab)
    } else {
        Err(tab.status.to_string())
    }
}

fn open_project(tabs: &mut Vec<DocTab>, args: &Json) -> Result<(Json, Effect), String> {
    if args.get("tab").is_some() {
        return Err("proj.open does not take 'tab'".into());
    }
    let path = args
        .get_str("path")
        .ok_or("proj.open needs a 'path' string")?;
    let path = canonical(Path::new(path));
    // Validate before changing even the focus or replacing a failed-load placeholder.
    let loaded = loaded_project(&path)?;
    let index = if let Some(i) = tabs.iter().position(|t| {
        t.kind == Kind::Project && t.path.as_ref().is_some_and(|p| canonical(p) == path)
    }) {
        if !matches!(tabs[i].surface, Surface::Project(_)) {
            tabs[i] = loaded;
        }
        i
    } else {
        tabs.push(loaded);
        tabs.len() - 1
    };
    Ok((
        path_info(&tabs[index], index),
        Effect {
            repaint: true,
            focus: Some(index),
            ..Effect::default()
        },
    ))
}

/// GPUI-free policy. Focus is returned for the host to apply through select_tab.
pub(crate) fn project_verb(
    tabs: &mut Vec<DocTab>,
    active: usize,
    verb: &str,
    args: &Json,
) -> Option<Result<(Json, Effect), String>> {
    if verb == "proj.open" {
        return Some(open_project(tabs, args));
    }
    if !projctl::is_editor_verb(verb) && !matches!(verb, "proj.path" | "proj.save" | "proj.reload")
    {
        return None;
    }
    Some((|| {
        let index = resolve_project_tab(tabs, active, args.get("tab"))?;
        let tab = &mut tabs[index];
        match verb {
            "proj.path" => Ok((path_info(tab, index), Effect::default())),
            "proj.save" => {
                let old_status = tab.status.clone();
                let save = (|| {
                    if !commit_project_cell(tab) {
                        return Err(tab.status.to_string());
                    }
                    let target = match args.get("path") {
                        Some(Json::Str(path)) if !path.is_empty() => PathBuf::from(path),
                        Some(_) => return Err("proj.save needs a non-empty 'path' string".into()),
                        None => match save_decision(tab, true, false) {
                            SaveDecision::InPlace(path) => path,
                            _ => {
                                return Err(
                                    "pass \"path\" to save this project (.yppx or .xml)".into()
                                );
                            }
                        },
                    };
                    apply_save(tab, &target)
                })();
                if let Err(e) = save {
                    tab.status = old_status;
                    return Err(e);
                }
                if let Surface::Project(v) = &mut tab.surface {
                    v.cancel_prompt();
                }
                tab.dialogs.clear();
                Ok((
                    path_info(tab, index),
                    Effect {
                        repaint: true,
                        ..Effect::default()
                    },
                ))
            }
            "proj.reload" => {
                let path = tab
                    .path
                    .as_ref()
                    .ok_or("project has no file path to reload")?;
                let loaded = loaded_project(path)?;
                let Surface::Project(fresh) = loaded.surface else {
                    unreachable!()
                };
                let Surface::Project(v) = &mut tab.surface else {
                    unreachable!()
                };
                // The cursor keeps its row kind: the entry row is derived
                // from the task count, so it stays valid over any reload,
                // and a reload to an empty plan latches it.
                v.ed.replace_project(fresh.ed.project().clone());
                v.latch_entry_row();
                v.cancel_prompt();
                v.cell = None;
                v.refresh_schedule_layout();
                // A dialog over the old plan would apply to the new one.
                tab.dialogs.clear();
                tab.dirty = false;
                tab.status = loaded.status;
                Ok((
                    path_info(tab, index),
                    Effect {
                        repaint: true,
                        ..Effect::default()
                    },
                ))
            }
            _ => {
                let Surface::Project(v) = &mut tab.surface else {
                    unreachable!()
                };
                let result = projctl::dispatch_editor(&mut v.ed, verb, args)
                    .expect("recognized editor verb")?;
                let mut effect = Effect::default();
                if projctl::MUTATING.contains(&verb) {
                    tab.dirty = v.ed.dirty();
                    crate::bump_edit_generation();
                    v.cancel_prompt();
                    tab.dialogs.clear();
                    v.cell = None;
                    v.latch_entry_row();
                    effect.repaint = true;
                    effect.activity = true;
                }
                Ok((result, effect))
            }
        }
    })())
}

/// Lifetime of either server; which app field holds it determines the mode.
pub(crate) struct ControlLink {
    pub(crate) _server: ctlcore::Server,
    pub(crate) _pump: Task<()>,
}

/// A reply, with optional harness-only requests to draw a frame and to quit
/// after it is sent.
pub(crate) struct Done {
    pub result: Json,
    pub quit: bool,
    /// Draw one frame before replying. **macOS only** — see [`Done::ok_drawn`].
    pub draw: bool,
    /// Input events (mouse #545, key-downs #1027) the verb asked the pump to
    /// dispatch through gpui's own hit testing and focus after the entity
    /// borrow above it has ended. A dispatch inside `update_in` would
    /// double-borrow the app.
    pub input: Vec<gpui::PlatformInput>,
    /// Whether to follow the queued key-downs `on_key` typed with the
    /// WM_CHAR Windows would send (#1139), as a Windows build always does:
    /// this input never passes through the window procedure. Set by
    /// `real-type`'s `wm_char`, so the dedupe runs on every platform.
    pub wm_char: WmChar,
}

impl Done {
    pub(crate) fn ok(result: Json) -> Result<Self, String> {
        Ok(Self {
            result,
            quit: false,
            draw: false,
            input: Vec::new(),
            wm_char: WmChar::Off,
        })
    }

    /// The reply to a verb that must leave a freshly drawn frame behind it —
    /// **on macOS, and only there**.
    ///
    /// ⚠️ This is what makes geometry verbs work on macOS. gpui draws a dirty
    /// window when its platform frame source ticks, and there that source is a
    /// display link which starts only while the window's occlusion state says
    /// it is visible. A harness instance is unfocused and unshown, so the link
    /// never runs: `cx.notify()` leaves the view dirty forever, the app answers
    /// every verb correctly, and the frame counter never moves — so `rect`
    /// times out against an app that is not hung at all. Drawing here drives
    /// the render pass directly instead of waiting for a tick that is never
    /// coming.
    ///
    /// ⚠️ And it is gated to macOS deliberately, not for tidiness. Elsewhere
    /// frames already flow for a shown window, so this would buy nothing —
    /// while costing something real: a draw advances the frame counter WITHOUT
    /// presenting, so `Driver::settle` could be satisfied by a frame that was
    /// never put on screen, and `PrintWindow` would then photograph the one
    /// before it. A capture that quietly reads stale pixels is the exact
    /// failure a pixel assertion cannot notice by itself.
    ///
    /// Nothing is presented here either. A draw is all the probes and the
    /// frame counter need, and presentation is the part that would require the
    /// window to be on screen — which is what this is avoiding.
    pub(crate) fn ok_drawn(result: Json) -> Result<Self, String> {
        Ok(Self {
            result,
            quit: false,
            draw: cfg!(target_os = "macos"),
            input: Vec::new(),
            wm_char: WmChar::Off,
        })
    }
}

/// When the harness's queued key-downs get their WM_CHAR twins (#1139).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WmChar {
    /// None (macOS and Linux, unless a verb asks).
    #[default]
    Off,
    /// Each right after its key-down, as a free message queue delivers them.
    Each,
    /// All after the last key-down, as a queue drained of input first does
    /// (gpui's under load): every twin arrives after later keys.
    Late,
}

/// Block off the UI thread; the foreground receiver wakes only for arrivals or closure.
fn async_requests<T: Send + 'static>(rx: Receiver<T>) -> flume::Receiver<T> {
    let (tx, pending) = flume::bounded(1);
    std::thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            if tx.send(request).is_err() {
                break;
            }
        }
    });
    pending
}

type Dispatch =
    fn(&mut Docxy, &str, &Json, &mut Window, &mut Context<Docxy>) -> Result<Done, String>;

pub(crate) fn attach_with_dispatch(
    server: ctlcore::Server,
    rx: Receiver<ctlcore::Request>,
    window: &mut Window,
    cx: &mut App,
    dispatch: Dispatch,
    hit_tests: fn(&str) -> bool,
) -> ControlLink {
    let pending = async_requests(rx);
    // The pump lives on the app, not a window: closing a window — even the
    // first — must not take the control server down (#587). Each request
    // resolves the registry's SELECTED window, so window-new/window-select
    // retarget every verb.
    //
    // ⚠️ The resolution borrows fallibly, like the AutoRecover loop's
    // `update_in`: on Windows a native rfd dialog pumps a modal loop while a
    // gpui listener holds the App borrowed, and an infallible borrow there
    // panics (see main's startup comment). `update_window` goes through
    // `try_borrow_mut`, so a busy app gets "busy, try again" — never a
    // panic. The candidates come from the registry's shared handle list, so
    // a window opened or closed by the UI between verbs is always tried
    // (#587 r2 M1) — no success-dependent cache.
    let bootstrap = window.window_handle();
    let handles = windows::handle_list(cx);
    let pump = cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(req) = pending.recv_async().await {
            let mut resolved = None;
            let mut gone = false;
            let candidates: Vec<gpui::AnyWindowHandle> = {
                let mut v: Vec<gpui::AnyWindowHandle> = handles.borrow().clone();
                if !v.contains(&bootstrap) {
                    v.push(bootstrap);
                }
                v
            };
            for candidate in candidates {
                match cx.update_window(candidate, |_, _, cx| windows::selected_target(cx)) {
                    Ok(Some(target)) => {
                        resolved = Some(target);
                        break;
                    }
                    // The registry is empty: the app is gone.
                    Ok(None) => {
                        gone = true;
                        break;
                    }
                    // The window is gone or the app is busy: try the next
                    // candidate.
                    Err(_) => continue,
                }
            }
            let Some(target) = resolved else {
                req.reply_err(if gone || handles.borrow().is_empty() {
                    "the app is gone".to_string()
                } else {
                    "busy, try again".to_string()
                });
                continue;
            };
            // A verb whose pointer input hit-tests the rendered frame gets a
            // frame drawn from the current state first (#1121), as gpui gives
            // a key-down: mouse dispatch hit-tests `rendered_frame` without
            // drawing, so an earlier verb's change was otherwise visible only
            // if the platform's tick happened to fire in between. The verb
            // aims at the fresh probes and the events land on its hitboxes.
            // No await separates this draw from the dispatch below, so no tick
            // lands between them, and a whole gesture (press, moves, release)
            // hit-tests this one frame. `draw` marks the frame for presenting,
            // so the next tick puts it on screen.
            if hit_tests(&req.verb) {
                let _ = cx.update_window(target.handle, |_, window, cx| window.draw(cx).clear(cx));
            }
            match cx.update_window(target.handle, |_, window, cx| {
                target.view.update(cx, |this, cx| {
                    dispatch(this, &req.verb, &req.args, window, cx)
                })
            }) {
                // update_window's Result around the weak entity's upgrade
                // Result around the verb's own Result.
                Ok(Ok(Ok(mut done))) => {
                    // Pointer and key verbs queue real input: dispatch it
                    // through gpui's own hit testing and focus now, outside
                    // the entity borrow, so the listeners it triggers may
                    // update the app.
                    if !done.input.is_empty() {
                        let events = std::mem::take(&mut done.input);
                        let wm_char = match done.wm_char {
                            WmChar::Off if text_input::WINDOWS => WmChar::Each,
                            mode => mode,
                        };
                        let _ = cx.update_window(target.handle, |_, window, cx| {
                            let mut late = Vec::new();
                            for event in events {
                                let deferred = text_input::deferred_text(&event);
                                let char_message =
                                    text_input::char_message_text(&event, wm_char != WmChar::Off);
                                // A key the root left to the input context
                                // gets the `insertText:` AppKit would send
                                // (#1072), and a key `on_key` typed on
                                // Windows the WM_CHAR TranslateMessage would
                                // post (#1139): this input never passes
                                // through AppKit or the window procedure.
                                if !window.dispatch_event(event, cx).propagate {
                                    continue;
                                }
                                if let Some(text) = deferred {
                                    let _ = target.view.update(cx, |this, cx| {
                                        this.macos_commit(&text, window, cx)
                                    });
                                } else if let Some(text) = char_message {
                                    if wm_char == WmChar::Late {
                                        late.push(text);
                                        continue;
                                    }
                                    let _ = target.view.update(cx, |this, cx| {
                                        this.windows_char(&text, window, cx)
                                    });
                                }
                            }
                            for text in late {
                                let _ = target
                                    .view
                                    .update(cx, |this, cx| this.windows_char(&text, window, cx));
                            }
                        });
                    }
                    // Before the reply, so a driver that reads the frame
                    // counter and then polls it sees it actually move.
                    if done.draw {
                        let _ = cx.update_window(target.handle, |_, window, cx| {
                            window.draw(cx).clear(cx)
                        });
                    }
                    req.reply_ok(done.result);
                    if done.quit {
                        // ctlcore's connection thread needs time to put the reply on the wire.
                        const QUIT_GRACE: Duration = Duration::from_millis(120);
                        cx.background_executor().timer(QUIT_GRACE).await;
                        cx.update(crate::macos_menu::end_process);
                        break;
                    }
                }
                Ok(Ok(Err(e))) => req.reply_err(e),
                Ok(Err(e)) => {
                    // The entity was released between resolution and update:
                    // this request answers with the error; the pump keeps
                    // serving the next one.
                    req.reply_err(format!("the app is gone: {e}"));
                }
                Err(e) => {
                    // The window is gone or mid-update: this request answers
                    // with the error and the pump keeps serving the next one
                    // (#587). An empty registry above is the "app is gone"
                    // case; here the selection was lost mid-flight.
                    req.reply_err(format!("the app is gone: {e}"));
                }
            }
        }
    });
    ControlLink {
        _server: server,
        _pump: pump,
    }
}

impl Docxy {
    pub(crate) fn dispatch_project(
        &mut self,
        verb: &str,
        args: &Json,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<Json, String>> {
        // The build this binary is (#1023): the same JSON in a harness and in
        // normal Project control, whatever tab is open.
        if verb == "app-info" {
            return Some(Json::parse(&crate::about::info().json()));
        }
        if verb == "proj.new" {
            return Some((|| {
                check_new_project_args(args)?;
                self.refuse_under_dialog()?;
                // The Backstage › New › Project card's handler.
                self.add_tab(Kind::Project, window, cx);
                Ok(new_project_reply(&self.tabs, self.active))
            })());
        }
        // A control client reads and saves the plan: never half-leveled.
        self.flush_project_passes(cx);
        if let Err(e) = crate::close::close_prompt_refusal(&self.tabs, verb) {
            return Some(Err(e));
        }
        let outcome = project_verb(&mut self.tabs, self.active, verb, args)?;
        Some(outcome.map(|(result, effect)| {
            // What drops the tab's dialogs (an edit, a reload, a save) or
            // moves the focus drops an open menu too (#397).
            if effect.repaint || effect.focus.is_some() {
                self.close_menu();
            }
            if let Some(i) = effect.focus {
                self.select_tab(i, window, cx);
            }
            if effect.repaint {
                cx.notify();
            }
            if effect.signals_activity(self.harness) {
                ctlcore::signal_activity();
            }
            if matches!(verb, "proj.save" | "proj.reload") {
                self.persist(cx);
            }
            result
        }))
    }
}

pub(crate) fn attach(
    server: ctlcore::Server,
    rx: Receiver<ctlcore::Request>,
    window: &mut Window,
    cx: &mut App,
) {
    let link = attach_with_dispatch(
        server,
        rx,
        window,
        cx,
        |this, verb, args, window, cx| {
            this.dispatch_project(verb, args, window, cx)
                .unwrap_or_else(|| Err(format!("unknown verb '{verb}'")))
                .and_then(Done::ok)
        },
        // No automation verb queues pointer input.
        |_| false,
    );
    // The registry owns the link for the run: a control surface must not
    // die with the window that attached it (#587 r1 M4).
    windows::keep_link(cx, link);
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod done_tests {
    use super::Done;
    use ctlcore::json::Json;

    /// The ordinary reply must not drive the render pass. Drawing on every verb
    /// would mean a normal Project control client silently paying for frames it
    /// never asked for, and would make the frame counter useless as a signal
    /// that a *requested* frame happened.
    #[test]
    fn an_ordinary_reply_does_not_ask_for_a_frame() {
        assert!(!Done::ok(Json::Null).unwrap().draw);
    }

    /// On macOS the `frame` verb's reply must leave a drawn frame behind it:
    /// it is what a driver polls while it waits for the view to settle, and
    /// nothing else will draw a window that is unfocused or unshown.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_drawing_reply_asks_for_a_frame_on_macos() {
        assert!(Done::ok_drawn(Json::Null).unwrap().draw);
    }

    /// ⚠️ Nowhere else. On Windows frames already flow for a shown window, so
    /// a forced draw would buy nothing and would actively cost something: it
    /// advances the frame counter WITHOUT presenting, so `settle` could be
    /// satisfied by a frame that was never put on screen and `PrintWindow`
    /// would then photograph the one before it. A capture that reads stale
    /// pixels is the one failure a pixel assertion cannot notice by itself.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_drawing_reply_does_not_force_a_frame_off_macos() {
        assert!(!Done::ok_drawn(Json::Null).unwrap().draw);
    }
}
