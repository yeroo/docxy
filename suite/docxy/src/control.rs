//! Project control policy over live tabs, followed by the normal-mode server pump.
use crate::*;
use ctlcore::json::Json;
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Effect {
    pub repaint: bool,
    pub activity: bool,
    pub focus: Option<usize>,
}

pub(crate) fn resolve_project_tab(
    tabs: &[DocTab],
    active: usize,
    arg: Option<&Json>,
) -> Result<usize, String> {
    let invalid = || "'tab' must be a tab index or a title/path substring".to_string();
    let index = match arg {
        None => {
            if !tabs.get(active).is_some_and(|t| t.kind == Kind::Project) {
                return Err("the active tab is not a Project".into());
            }
            active
        }
        Some(Json::Num(n))
            if n.is_finite()
                && n.fract() == 0.
                && *n >= 0.
                && *n < (usize::MAX as u128 + 1) as f64 =>
        {
            let i = *n as usize;
            let tab = tabs.get(i).ok_or_else(|| format!("no tab at index {i}"))?;
            if tab.kind != Kind::Project {
                return Err(format!("tab {i} is not a Project"));
            }
            i
        }
        Some(Json::Str(text)) if !text.is_empty() => {
            let needle = text.to_lowercase();
            let hits: Vec<usize> = tabs
                .iter()
                .enumerate()
                .filter(|(_, t)| {
                    t.kind == Kind::Project
                        && (t.title.to_lowercase().contains(&needle)
                            || t.path.as_ref().is_some_and(|p| {
                                p.to_string_lossy().to_lowercase().contains(&needle)
                            }))
                })
                .map(|(i, _)| i)
                .collect();
            match hits.as_slice() {
                [] => return Err(format!("no Project tab matches '{text}'")),
                [i] => *i,
                _ => {
                    return Err(format!(
                        "several Project tabs match '{text}' ({})",
                        hits.iter()
                            .map(usize::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
        _ => return Err(invalid()),
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
    Json::Obj(fields)
}

fn loaded_project(path: &Path) -> Result<DocTab, String> {
    let tab = project_tab_from_path(path);
    if matches!(tab.surface, Surface::Project(_)) {
        Ok(tab)
    } else {
        Err(tab.status.to_string())
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.into())
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
                let target = match args.get("path") {
                    Some(Json::Str(path)) if !path.is_empty() => PathBuf::from(path),
                    Some(_) => return Err("proj.save needs a non-empty 'path' string".into()),
                    None => match save_decision(tab, true, false) {
                        SaveDecision::InPlace(path) => path,
                        _ => {
                            return Err("pass \"path\" to save this project (.yppx or .xml)".into());
                        }
                    },
                };
                let old_status = tab.status.clone();
                if let Err(e) = apply_save(tab, &target) {
                    tab.status = old_status;
                    return Err(e);
                }
                if let Surface::Project(v) = &mut tab.surface {
                    v.cancel_prompt();
                }
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
                *tab = loaded;
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
                    v.cancel_prompt();
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

impl Docxy {
    pub(crate) fn dispatch_project(
        &mut self,
        verb: &str,
        args: &Json,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<Json, String>> {
        let outcome = project_verb(&mut self.tabs, self.active, verb, args)?;
        Some(outcome.map(|(result, effect)| {
            if let Some(i) = effect.focus {
                self.select_tab(i, window, cx);
            }
            if effect.repaint {
                cx.notify();
            }
            if effect.activity {
                ctlcore::signal_activity();
            }
            if matches!(verb, "proj.save" | "proj.reload") {
                self.persist();
            }
            result
        }))
    }
}

pub(crate) fn attach(
    view: &Entity<Docxy>,
    server: ctlcore::Server,
    rx: Receiver<ctlcore::Request>,
    window: &mut Window,
    cx: &mut App,
) {
    let target = view.downgrade();
    let pump = window.spawn(cx, async move |cx: &mut AsyncWindowContext| {
        loop {
            match rx.try_recv() {
                Ok(req) => {
                    let (verb, args) = (req.verb.clone(), req.args.clone());
                    match target.update_in(cx, |this, window, cx| {
                        this.dispatch_project(&verb, &args, window, cx)
                            .unwrap_or_else(|| Err(format!("unknown verb '{verb}'")))
                    }) {
                        Ok(Ok(result)) => req.reply_ok(result),
                        Ok(Err(e)) => req.reply_err(e),
                        Err(e) => {
                            req.reply_err(format!("the app is gone: {e}"));
                            break;
                        }
                    }
                }
                Err(TryRecvError::Empty) => {
                    cx.background_executor()
                        .timer(Duration::from_millis(8))
                        .await
                }
                Err(TryRecvError::Disconnected) => break,
            }
        }
    });
    view.update(cx, |this, _| {
        this.control = Some(ControlLink {
            _server: server,
            _pump: pump,
        })
    });
}

#[cfg(test)]
mod tests;
