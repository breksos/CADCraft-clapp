//! The window's two channels (clappkit/docs/architecture.md § 3): the Clatch control pipe, and
//! the app's own socket the agent's CLI talks to. Both are async and the window is eframe, which
//! owns the main thread, so they live on a small runtime of their own. A CLI request becomes a
//! [`ControlRequest`] on the queue CADCraft's own control channel feeds into the frame loop
//! (`CadApp::drain_control`, inside `logic`): the agent's verbs run through the same
//! `cadcraft_ui_egui::control::handle` and `CadApp::run` the person's command line does.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use cadcraft_ui_egui::control::ControlRequest;
use serde_json::{Value as Json, json};

use super::{CLI, door::Door};

/// How long a request waits for the window. A request the window never started is then dropped
/// (CADCraft's `PendingReply` says so); this is for a window that is not drawing at all.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

/// What the window takes from the wiring once eframe has made its context.
pub struct Wiring {
    /// The queue the frame loop drains (`CadApp::with_control`).
    pub rx: Receiver<ControlRequest>,
    /// The same queue's sending end, for upstream's own `--control` port to feed as well.
    pub tx: Sender<ControlRequest>,
    /// Set once the window exists, so a request can wake the frame loop.
    pub ctx: Arc<OnceLock<egui::Context>>,
    /// The control pipe, once registered: where [`super::signals`] sends what the person does.
    pub control: Arc<OnceLock<clappkit::Control>>,
}

/// Open both channels on a background runtime and return at once: the socket opens before the
/// window does, so an agent that calls the moment Clatch has launched us waits for the window
/// instead of being told nothing is running.
pub fn start() -> Wiring {
    let (tx, rx) = channel();
    let ctx: Arc<OnceLock<egui::Context>> = Arc::default();
    let control: Arc<OnceLock<clappkit::Control>> = Arc::default();
    let bridge = Bridge { tx: tx.clone(), ctx: ctx.clone() };
    let held = control.clone();
    let spawned = std::thread::Builder::new().name("clapp".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
            Ok(r) => r,
            Err(e) => {
                log::error!("clapp: no runtime for the control pipe and the agent's socket: {e}");
                return;
            }
        };
        runtime.block_on(serve(bridge, held));
    });
    if let Err(e) = spawned {
        log::error!("clapp: cannot start the control pipe and the agent's socket: {e}");
    }
    Wiring { rx, tx, ctx, control }
}

async fn serve(bridge: Bridge, held: Arc<OnceLock<clappkit::Control>>) {
    // Clatch's stop arrives here. clappkit answers it, runs this, and ends the process; the
    // drawings' autosave files are already on disk ([`super::recovery`]).
    let on_shutdown: clappkit::OnShutdown = Arc::new(|cause| log::info!("clapp: {cause}"));
    match clappkit::Control::connect_with(clappkit::declared_signals(), on_shutdown).await {
        Ok(control) => {
            let _ = held.set(control);
        }
        Err(e) => {
            // Wired but not registered: Clatch kills an app that never registers, so say why.
            log::error!("clapp: the Clatch control pipe: {e:#}");
            let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("{CLI}: {e:#}\n"));
            std::process::exit(1);
        }
    }
    let door = Door::new(bridge);
    let addr = clappkit::ipc::address(CLI);
    let served = clappkit::ipc::serve(&addr, move |request: Json| {
        let door = door.clone();
        async move { door.answer(request).await }
    })
    .await;
    if let Err(e) = served {
        log::error!("clapp: the agent's CLI cannot reach this window: {e:#}");
    }
}

/// One way into the frame loop: send a control method, wait for the window's answer.
#[derive(Clone)]
pub struct Bridge {
    tx: Sender<ControlRequest>,
    ctx: Arc<OnceLock<egui::Context>>,
}

impl Bridge {
    /// Call one of CADCraft's control methods (`cadcraft_ui_egui::control`). `Ok` is its
    /// `result`, `Err` the window's refusal or why it was not reached.
    pub async fn call(&self, method: &str, params: Json) -> Result<Json, String> {
        let (req, pending) = ControlRequest::new(method, params);
        self.tx.send(req).map_err(|_| "the CADCraft window has closed".to_string())?;
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
        let reply =
            tokio::task::spawn_blocking(move || pending.wait(ANSWER_TIMEOUT)).await.map_err(|e| format!("the window's answer was lost: {e}"))?;
        if reply.get("ok").and_then(Json::as_bool) == Some(true) {
            return Ok(reply.get("result").cloned().unwrap_or(Json::Null));
        }
        let error = reply.get("error").and_then(Json::as_str).unwrap_or("the window refused without saying why");
        Err(match reply.get("state").and_then(Json::as_str) {
            Some("not-run") => format!("the window did not answer within {}s; nothing was run", ANSWER_TIMEOUT.as_secs()),
            Some(_) => format!("the window did not answer within {}s; it may have run, so look before sending it again", ANSWER_TIMEOUT.as_secs()),
            None => error.to_string(),
        })
    }

    /// [`Self::call`] for one engine command.
    pub async fn exec(&self, command: &str, params: Json) -> Result<Json, String> {
        self.call("engine.execute", json!({"command": command, "params": params})).await
    }
}
