//! What the person does, told to the agent (clappkit/docs/protocol.md § 7), so it keeps up
//! without polling. Only the person's actions: the agent knows its own, and hearing them back
//! would wake it in a loop.
//!
//! - `changed` (context): the commands the person completed, in order, sent once they pause, so a
//!   burst of drafting becomes one notice instead of a queue full of them. Every change to a
//!   drawing pushes an undo step labelled with its command (LINE, MOVE, Erase...), so the new
//!   steps are the list; an undo or redo is told as such.
//! - `selected` (buffered): what the person has selected (count, types, handles), the latest
//!   only, riding their next message. "Fix these" means that selection.
//!
//! Telling the two apart needs no hook in the UI. Within a frame the agent's requests run in
//! `CadApp::logic` (its control queue, `drain_control`) and the person's clicks and keys in `ui`,
//! so the app calls [`Observer::observe`] before `logic` and after `ui`, and [`Observer::settle`]
//! after `logic`: whatever changed between a settle and the next observe was the person's.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use cadcraft_ui_egui::CadApp;
use serde_json::{Value as Json, json};

/// How long the person pauses before their changes are told: one notice per burst.
const QUIET: Duration = Duration::from_millis(1200);
/// How long a selection rests before it is told: a window drag reports where it ends.
const SETTLE: Duration = Duration::from_millis(300);
/// The most commands one `changed` lists; the rest are counted.
const MAX_LISTED: usize = 50;
/// The most handles one `selected` lists; the types count them all.
const MAX_HANDLES: usize = 50;

/// One drawing's state as last settled.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Doc {
    uid: u64,
    revision: u64,
    undo: usize,
    redo: usize,
}

/// Where the person is: the drawing in front, its space and the selection.
#[derive(Clone, PartialEq, Eq, Debug)]
struct View {
    uid: u64,
    drawing: String,
    space: Json,
    selection: Vec<u64>,
}

pub struct Observer {
    control: Arc<OnceLock<clappkit::Control>>,
    doc: Option<Doc>,
    view: Option<View>,
    /// The person's changes not yet told.
    pending: Vec<String>,
    uncounted: usize,
    last_change: Option<Instant>,
    /// When the person last moved the selection, until it is told.
    moved: Option<Instant>,
    told: Option<View>,
    started: bool,
}

impl Observer {
    pub fn new(control: Arc<OnceLock<clappkit::Control>>) -> Observer {
        Observer { control, doc: None, view: None, pending: Vec::new(), uncounted: 0, last_change: None, moved: None, told: None, started: false }
    }

    /// Before the agent's requests run, and after the person's input: whatever changed since
    /// [`Self::settle`] was the person's.
    pub fn observe(&mut self, app: &CadApp) {
        if !self.started {
            // The first frame is where things start, not something the person did.
            self.started = true;
            self.settle(app);
            self.told = self.view.clone();
            return;
        }
        let now_doc = doc(app);
        if let (Some(before), Some(after)) = (&self.doc, &now_doc)
            && before.uid == after.uid
            && before.revision != after.revision
        {
            let steps = changes(app, before, after);
            for step in steps {
                if self.pending.len() < MAX_LISTED {
                    self.pending.push(step);
                } else {
                    self.uncounted += 1;
                }
            }
            self.last_change = Some(Instant::now());
        }
        self.doc = now_doc;
        let now_view = view(app);
        if now_view != self.view {
            self.view = now_view;
            self.moved = Some(Instant::now());
        }
    }

    /// After the agent's requests ran: what they changed is the agent's own, never told.
    pub fn settle(&mut self, app: &CadApp) {
        self.doc = doc(app);
        self.view = view(app);
    }

    /// Tell what is due, and wake the frame loop when the rest will be.
    pub fn tick(&mut self, app: &CadApp, ctx: &egui::Context) {
        let Some(control) = self.control.get() else { return };
        if let Some(at) = self.last_change {
            let quiet = at.elapsed();
            if quiet >= QUIET {
                let d = app.session.docs.get(app.session.active);
                let mut payload = json!({
                    "drawing": d.map(|d| d.title.clone()),
                    "changes": std::mem::take(&mut self.pending),
                    "entities": d.map(|d| d.doc.entity_count()),
                    "selection": d.map_or(0, |d| d.selection.len()),
                });
                if self.uncounted > 0 {
                    payload["more"] = json!(self.uncounted);
                }
                control.emit("changed", Vec::new(), payload);
                self.uncounted = 0;
                self.last_change = None;
            } else {
                ctx.request_repaint_after(QUIET - quiet);
            }
        }
        if let Some(at) = self.moved {
            let rested = at.elapsed();
            if rested < SETTLE {
                ctx.request_repaint_after(SETTLE - rested);
            } else {
                self.moved = None;
                if self.view != self.told
                    && let Some(v) = self.view.clone()
                {
                    control.emit("selected", Vec::new(), selected(app, &v));
                    self.told = Some(v);
                }
            }
        }
    }
}

fn doc(app: &CadApp) -> Option<Doc> {
    let d = app.session.docs.get(app.session.active)?;
    Some(Doc { uid: d.uid, revision: d.revision, undo: d.undo.len(), redo: d.redo.len() })
}

fn view(app: &CadApp) -> Option<View> {
    let d = app.session.docs.get(app.session.active)?;
    Some(View {
        uid: d.uid,
        drawing: d.title.clone(),
        space: serde_json::to_value(&d.space).unwrap_or(Json::Null),
        selection: d.selection.iter().map(|h| h.0).collect(),
    })
}

/// The steps between two states of one drawing: the undo labels added, or an undo or redo.
fn changes(app: &CadApp, before: &Doc, after: &Doc) -> Vec<String> {
    let Some(d) = app.session.docs.get(app.session.active) else { return Vec::new() };
    let grew = after.undo.saturating_sub(before.undo);
    if grew > 0 && before.redo.saturating_sub(after.redo) == grew {
        return vec![format!("Redo ({grew})")];
    }
    if grew > 0 {
        return d.undo.iter().skip(before.undo).map(|s| s.label.clone()).collect();
    }
    if after.undo < before.undo {
        return vec![format!("Undo ({})", before.undo - after.undo)];
    }
    // The undo history is capped, or the step changed in place: the newest label says what.
    vec![d.undo.last().map_or_else(|| "Change".to_string(), |s| s.label.clone())]
}

fn selected(app: &CadApp, v: &View) -> Json {
    let d = app.session.docs.iter().find(|d| d.uid == v.uid);
    let mut types: BTreeMap<&'static str, usize> = BTreeMap::new();
    if let Some(d) = d {
        for h in &d.selection {
            if let Some(e) = d.doc.entity(*h) {
                *types.entry(e.kind.type_name()).or_default() += 1;
            }
        }
    }
    json!({
        "drawing": v.drawing,
        "space": v.space,
        "count": v.selection.len(),
        "types": types,
        "handles": v.selection.iter().take(MAX_HANDLES).map(|h| format!("{h:X}")).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn handles_are_hex_as_the_cli_reads_them() {
        assert_eq!(format!("{:X}", 256u64), "100");
    }
}
