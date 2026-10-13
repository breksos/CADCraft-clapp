//! Unsaved work survives the window ending, through CADCraft's own autosave and Drawing
//! Recovery. Under Clatch the window ends whenever Clatch stops it or its pipe drops
//! (clappkit/docs/protocol.md § 9), with no time for a shutdown hook, and CADCraft autosaves every
//! SAVETIME minutes (10 by default): up to ten minutes of work went with every `clatch stop`.
//!
//! So, under Clatch, the clapp asks CADCraft's own saver to write right away
//! ([`Autosaver::save_now`]) about a second after a drawing changes, every 5 s at most during a
//! long burst. Everything else is upstream's: the files (in `<data dir>/autosave/`), the
//! background writes, deleting a file once its drawing is saved or closed, and the Drawing
//! Recovery window at the next launch, which offers each drawing as an unsaved copy. SAVETIME 0
//! (the person turned autosave off) is respected: `save_now` writes nothing then.
//!
//! [`Autosaver::save_now`]: cadcraft_engine::autosave::Autosaver::save_now

use std::collections::HashMap;
use std::time::{Duration, Instant};

use cadcraft_ui_egui::CadApp;

/// Quiet after a change before the drawing is written.
const QUIET: Duration = Duration::from_secs(1);
/// The longest an autosave file lags behind during a long burst of changes.
const AT_MOST: Duration = Duration::from_secs(5);
/// A drawing still being written when `save_now` ran is skipped by it: look again after this.
const RETRY: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct Recovery {
    /// Each open drawing's revision when `save_now` last ran.
    asked: HashMap<u64, u64>,
    /// Each open drawing's revision last frame, and when one last moved.
    seen: HashMap<u64, u64>,
    last_change: Option<Instant>,
    /// Since when a drawing has had changes `save_now` has not been asked for.
    behind_since: Option<Instant>,
    retry_at: Option<Instant>,
}

impl Recovery {
    /// Once per frame, after everything that can change a drawing has run.
    pub fn tick(&mut self, app: &mut CadApp, ctx: &egui::Context) {
        if app.autosave.is_none() {
            return;
        }
        let now = Instant::now();
        let seen: HashMap<u64, u64> = app.session.docs.iter().map(|d| (d.uid, d.revision)).collect();
        if seen != self.seen {
            self.last_change = Some(now);
            self.seen = seen;
        }
        let behind = app.session.docs.iter().any(|d| d.is_dirty() && self.asked.get(&d.uid) != Some(&d.revision));
        let retry = self.retry_at.is_some_and(|at| now >= at);
        if !behind && !retry {
            self.behind_since = None;
            if let Some(at) = self.retry_at {
                ctx.request_repaint_after(at.saturating_duration_since(now));
            }
            return;
        }
        let since = *self.behind_since.get_or_insert(now);
        let quiet = self.last_change.map_or(QUIET, |at| now.saturating_duration_since(at));
        if retry || quiet >= QUIET || now.saturating_duration_since(since) >= AT_MOST {
            if let Some(saver) = app.autosave.as_mut() {
                saver.save_now(&app.session);
            }
            self.asked = app.session.docs.iter().map(|d| (d.uid, d.revision)).collect();
            self.behind_since = None;
            self.retry_at = if retry { None } else { Some(now + RETRY) };
        } else {
            ctx.request_repaint_after(QUIET.saturating_sub(quiet));
        }
    }
}
