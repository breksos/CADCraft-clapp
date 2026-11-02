//! One background update check at startup, the "Update and Restart" prompt, and Help ▸ Check for
//! Updates… (desktop only; the web build is always the latest). The flow itself is
//! `craft_update::updater::Updater`.

use craft_update::updater::{Action, Status, Updater};
use craft_update::{CheckResultDialog, CheckResultResponse};

use crate::CadApp;

/// The version this build reports to the update check.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the update check and prompt are doing between frames.
pub struct Runtime {
    updater: Updater,
    /// Whether the host asked for a startup check (see [`CadApp::enable_update_check`]).
    enabled: bool,
    /// Help ▸ Check for Updates… was chosen; the next frame's `tick` starts it with the context.
    check_requested: bool,
    /// The check-result dialog is open (from a requested check until it is closed).
    result_open: bool,
    /// The current result was already written to the command history.
    result_echoed: bool,
    /// clapp: a launcher installs this copy's updates (see [`CadApp::updates_come_from`]).
    host: Option<&'static str>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            updater: Updater::new(&crate::UPDATE_APP, VERSION),
            enabled: false,
            check_requested: false,
            result_open: false,
            result_echoed: false,
            host: None,
        }
    }
}

impl CadApp {
    /// Turn on the startup update check (the desktop host does; tests and the web build do not).
    pub fn enable_update_check(&mut self) {
        self.updates_rt.enabled = true;
    }
}

impl CadApp {
    /// clapp: `host` (a launcher such as Clatch) installs this copy and its updates. Updating in
    /// place would rewrite the launcher's copy behind its back, so CADCraft never checks for,
    /// offers or installs an update itself; Help ▸ Check for Updates… says where updates come from.
    pub fn updates_come_from(&mut self, host: &'static str) {
        self.updates_rt.host = Some(host);
        self.updates_rt.enabled = false;
    }
}

/// Help ▸ Check for Updates…: check now, and show the result when there is nothing newer.
pub fn check_now(app: &mut CadApp) {
    if let Some(host) = app.updates_rt.host {
        app.set_status(format!("CADCraft's updates come from {host}: update it there."));
        app.session.echo(format!("CADCraft's updates come from {host}: update it there."));
        return;
    }
    app.updates_rt.check_requested = true;
    app.updates_rt.result_open = true;
    app.updates_rt.result_echoed = false;
}

/// Start the checks and collect the answers.
pub fn tick(app: &mut CadApp, ctx: &egui::Context) {
    let rt = &mut app.updates_rt;
    if rt.host.is_some() {
        return;
    }
    if std::mem::take(&mut rt.check_requested) {
        rt.updater.check_now(ctx);
    } else if rt.enabled {
        rt.updater.check_at_start(ctx);
    }
    rt.updater.tick(ctx);
    let outcome = match rt.updater.status() {
        Status::UpToDate => Some(crate::tl!("CADCraft is up to date.")),
        Status::Failed(_) => Some(crate::tl!("Could not check for updates.")),
        Status::Idle | Status::Checking => None,
    };
    let Some(message) = outcome else {
        // A check that found an update leaves `Idle`; the prompt takes over from the dialog.
        if rt.result_open && matches!(rt.updater.status(), Status::Idle) {
            rt.result_open = false;
        }
        return;
    };
    if !rt.result_open {
        rt.updater.dismiss_status();
    } else if !std::mem::replace(&mut rt.result_echoed, true) {
        app.session.echo(message);
    }
}

/// Draw the prompt while there is one and act on its answer.
pub fn show(app: &mut CadApp, ctx: &egui::Context) {
    if app.updates_rt.host.is_some() {
        return;
    }
    // The restart helper is scheduled before the app closes, so no update starts over unsaved work.
    let clean = !app.session.docs.iter().any(|d| d.is_dirty());
    let rt = &mut app.updates_rt;
    if rt.result_open {
        match CheckResultDialog::new(&crate::UPDATE_APP, VERSION).show(ctx, rt.updater.status()) {
            CheckResultResponse::Close => {
                rt.result_open = false;
                rt.updater.dismiss_status();
            }
            CheckResultResponse::CheckAgain => {
                rt.result_echoed = false;
                rt.check_requested = true;
            }
            CheckResultResponse::None => {}
        }
    }
    match app.updates_rt.updater.show_if(ctx, clean) {
        Action::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
        // The guarded close path asks about unsaved changes; the restart helper relaunches once the
        // process has exited.
        Action::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        Action::None => {}
    }
}

#[cfg(test)]
mod tests {
    use cadcraft_engine::Session;

    use crate::{CadApp, Services};

    /// clapp: a launcher-installed copy never starts a check, and Check for Updates says where
    /// updates come from.
    #[test]
    fn a_host_installed_copy_never_checks_for_updates() {
        let mut app = CadApp::new(Session::empty(), Services::default());
        app.enable_update_check();
        app.updates_come_from("Clatch");
        assert!(!app.updates_rt.enabled, "no startup check");
        super::check_now(&mut app);
        assert!(!app.updates_rt.check_requested && !app.updates_rt.result_open, "no check, no dialog");
        assert!(app.session.log.iter().any(|l| l.contains("come from Clatch")), "{:?}", app.session.log);
    }
}
