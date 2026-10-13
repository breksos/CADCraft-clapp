//! The window's side of the agent's socket: one verb per request (`{"verb": …}`), carried out
//! with CADCraft's own control methods through the [`Bridge`]. The rules live here rather than in
//! the CLI, so a program that talks to the socket directly is held to them as well.
//!
//! The person and the agent share one command line. A command the agent left running (LINE,
//! waiting for its next point) is the agent's to answer; any other running command is the
//! person's, and nothing the agent sends may type into it, cancel it, or start a new one over it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value as Json, json};

use super::CLI;
use super::rules;
use super::wiring::Bridge;

/// An answer bigger than this travels as a file: clappkit's socket takes 1 MiB at most.
const SPILL: usize = 512 << 10;
/// Entities `read` lists unless told otherwise.
const READ_LIMIT: u64 = 200;

#[derive(Clone)]
pub struct Door {
    bridge: Bridge,
    /// The command the agent's last `cmd` left running, if any.
    held: Arc<Mutex<Option<String>>>,
    spill_dir: Arc<PathBuf>,
    next: Arc<AtomicU64>,
}

impl Door {
    pub fn new(bridge: Bridge) -> Door {
        // Answers too big for the socket wait here for one request; an earlier run's are swept.
        let spill_dir = clappkit::paths::data_subdir(CLI, "exchange");
        if let Ok(entries) = std::fs::read_dir(&spill_dir) {
            for e in entries.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        Door { bridge, held: Arc::default(), spill_dir: Arc::new(spill_dir), next: Arc::new(AtomicU64::new(1)) }
    }

    pub async fn answer(&self, req: Json) -> Json {
        let verb = req.get("verb").and_then(Json::as_str).unwrap_or("");
        let s = |k: &str| req.get(k).and_then(Json::as_str).map(str::to_string);
        let result = match verb {
            "state" => self.state().await,
            "read" => self.read(&req).await,
            "commands" => self.commands(s("search")).await,
            "shot" => self.shot(&req).await,
            "cmd" => self.cmd(s("text").unwrap_or_default()).await,
            "cancel" => self.cancel().await,
            "run" => self.run(&req).await,
            "new" => self.new_drawing(&req).await,
            "open" => self.guarded(self.bridge.exec("open", pick(&req, &["path"]))).await,
            "save" => self.save(s("path")).await,
            "export" => self.export(s("path")).await,
            "import" => self.guarded(self.bridge.exec("import", pick(&req, &["path", "at", "scale"]))).await,
            "focus" => self.bridge.call("ui.focus", json!({})).await.map(|_| json!({"focused": true})),
            "close" => self.close().await,
            "" => Err("a request names its `verb`".into()),
            other => Err(format!("unknown verb `{other}`")),
        };
        match result {
            Ok(v) => self.deliver(v),
            Err(e) => json!({"ok": false, "error": e}),
        }
    }

    /// The command running now, and whether it is the person's.
    async fn running(&self) -> Result<(Option<String>, bool), String> {
        let st = self.bridge.call("cmdline.state", json!({})).await?;
        let running = st.get("running").and_then(Json::as_str).map(str::to_string);
        let held = self.held.lock().map(|h| h.clone()).unwrap_or(None);
        let persons = running.is_some() && running != held;
        Ok((running, persons))
    }

    /// Refuse while the person is in the middle of a command.
    async fn not_in_persons_command(&self) -> Result<(), String> {
        match self.running().await? {
            (Some(id), true) => {
                Err(format!("the person is in the middle of {}: wait until it ends (`{CLI} state` shows what is running)", id.to_ascii_uppercase()))
            }
            _ => Ok(()),
        }
    }

    async fn guarded(&self, call: impl std::future::Future<Output = Result<Json, String>>) -> Result<Json, String> {
        self.not_in_persons_command().await?;
        call.await
    }

    async fn hold(&self, running: Option<String>) {
        if let Ok(mut h) = self.held.lock() {
            *h = running;
        }
    }

    /// The open drawings, the one in front and what is in it, and the command line.
    async fn state(&self) -> Result<Json, String> {
        let ui = self.bridge.call("ui.inspect", json!({})).await?;
        let session = &ui["session"];
        let (running, persons) = self.running().await?;
        let mut out = json!({
            "drawings": session["documents"].as_array().map(|d| d.iter().map(|x| json!({
                "index": x["index"], "title": x["title"], "path": x["path"], "dirty": x["dirty"], "active": x["active"],
            })).collect::<Vec<_>>()).unwrap_or_default(),
            "prompt": session["prompt"],
            "running": running.as_ref().map(|r| json!({"command": r, "by": if persons { "person" } else { "agent" }})),
        });
        if session["documents"].as_array().is_some_and(|d| !d.is_empty()) {
            let d = self.bridge.call("drawing.inspect", json!({"entities": false})).await?;
            let layers: Vec<Json> = d["layers"]
                .as_array()
                .map(|l| l.iter().take(200).map(|x| x.get("name").cloned().unwrap_or_else(|| x.clone())).collect())
                .unwrap_or_default();
            out["drawing"] = json!({
                "title": d["title"], "path": d["path"], "dirty": d["dirty"],
                "space": d["space"], "layouts": d["layouts"], "currentLayer": d["currentLayer"], "layers": layers,
                "entities": d["entityCount"], "counts": d["counts"], "extents": d["extents"],
                "selection": d["selection"], "lastChanges": d["undo"].as_array().map(|u| u.iter().take(5).cloned().collect::<Vec<_>>()),
                "blockEditor": d["blockEditor"],
            });
        }
        Ok(out)
    }

    async fn read(&self, req: &Json) -> Result<Json, String> {
        let selected = req.get("selected").and_then(Json::as_bool) == Some(true);
        let mut params = pick(req, &["type", "layer", "window", "crossing", "offset"]);
        let limit = req.get("limit").and_then(Json::as_u64).unwrap_or(READ_LIMIT);
        if selected {
            let d = self.bridge.call("drawing.inspect", json!({"entities": false})).await?;
            let handles: Vec<String> =
                d["selection"].as_array().map(|h| h.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
            if handles.is_empty() {
                return Ok(json!({"count": 0, "entities": [], "note": "nothing is selected"}));
            }
            params["limit"] = json!(100_000);
            let all = self.bridge.exec("entities", params).await?;
            let picked: Vec<Json> = all["entities"]
                .as_array()
                .map(|e| {
                    e.iter()
                        .filter(|x| x["handle"].as_str().is_some_and(|h| handles.iter().any(|s| s.eq_ignore_ascii_case(h))))
                        .take(limit as usize)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            return Ok(json!({"count": handles.len(), "entities": picked}));
        }
        params["limit"] = json!(limit);
        self.bridge.exec("entities", params).await
    }

    async fn commands(&self, search: Option<String>) -> Result<Json, String> {
        let all = self.bridge.call("engine.commands", json!({})).await?;
        let words: Vec<String> = search.unwrap_or_default().split_whitespace().map(str::to_ascii_lowercase).collect();
        let rows = all.as_array().cloned().unwrap_or_default();
        Ok(Json::Array(
            rows.into_iter()
                .filter(|r| {
                    let hay = format!("{} {} {}", r["id"], r["label"], r["aliases"]).to_ascii_lowercase();
                    words.iter().all(|w| hay.contains(w))
                })
                .map(|r| {
                    let id = r["id"].as_str().unwrap_or("").to_string();
                    json!({"id": id, "label": r["label"], "aliases": r["aliases"], "params": r["params"], "enabled": r["enabled"], "run": rules::runnable(&id)})
                })
                .collect(),
        ))
    }

    /// A picture of the drawing, drawn off-screen: the window is not touched.
    async fn shot(&self, req: &Json) -> Result<Json, String> {
        let path = req.get("path").and_then(Json::as_str).ok_or("`shot` writes to a path")?;
        let mut params = pick(req, &["width", "height", "window"]);
        params["path"] = json!(path);
        params["fit"] = json!(req.get("view").and_then(Json::as_bool) != Some(true));
        let r = self.bridge.call("ui.render", params).await?;
        Ok(json!({"path": path, "width": r["width"], "height": r["height"]}))
    }

    /// Type one command line, as the person would at the command line.
    async fn cmd(&self, text: String) -> Result<Json, String> {
        match self.running().await? {
            (Some(id), true) => {
                return Err(format!(
                    "the person is in the middle of {}: wait until it ends (`{CLI} state` shows what is running)",
                    id.to_ascii_uppercase()
                ));
            }
            (Some(_), false) => rules::inputs(&text)?,
            (None, _) => rules::command_line(&text)?,
        }
        let out = self.bridge.call("cmdline.input", json!({"text": text})).await?;
        let running = out.get("running").and_then(Json::as_str).map(str::to_string);
        self.hold(running.clone()).await;
        let mut reply = json!({"output": out["output"], "prompt": out["prompt"], "running": running});
        if let Some(e) = out.get("error") {
            reply["error"] = e.clone();
        }
        Ok(reply)
    }

    /// Esc: end the command the agent left running, never the person's.
    async fn cancel(&self) -> Result<Json, String> {
        match self.running().await? {
            (None, _) => Ok(json!({"cancelled": null})),
            (Some(id), true) => Err(format!("{} is the person's command: leave it to them", id.to_ascii_uppercase())),
            (Some(id), false) => {
                self.bridge.call("cmdline.key", json!({"key": "escape"})).await?;
                self.hold(None).await;
                Ok(json!({"cancelled": id}))
            }
        }
    }

    async fn run(&self, req: &Json) -> Result<Json, String> {
        let id = req.get("command").and_then(Json::as_str).ok_or("`run` names a command")?;
        let params = match req.get("params") {
            None | Some(Json::Null) => json!({}),
            Some(p) if p.is_object() => p.clone(),
            Some(_) => return Err("a command's parameters are a JSON object".into()),
        };
        let id = cadcraft_engine::cmd::resolve_alias(id);
        rules::run(&id, &params)?;
        self.not_in_persons_command().await?;
        self.bridge.exec(&id, params).await
    }

    async fn new_drawing(&self, req: &Json) -> Result<Json, String> {
        self.not_in_persons_command().await?;
        if req.get("sample").and_then(Json::as_bool) == Some(true) {
            return self.bridge.exec("ui.sample", json!({})).await;
        }
        // "." is no template: the person's QNEWTEMPLATE is theirs to start from, not the agent's.
        let mut params = json!({"template": "."});
        if let Some(m) = req.get("metric").and_then(Json::as_bool) {
            params["metric"] = json!(m);
        }
        self.bridge.exec("new", params).await
    }

    async fn save(&self, path: Option<String>) -> Result<Json, String> {
        self.not_in_persons_command().await?;
        if let Some(p) = path {
            if !rules::becomes_the_file(&p) {
                return Err(format!(
                    "`save` takes .dwg, .dxf or .dwt, which becomes the drawing's file; a .{} copy is `{CLI} export <path>`",
                    rules::extension(&p)
                ));
            }
            return self.bridge.exec("saveas", json!({"path": p})).await;
        }
        let d = self.bridge.call("drawing.inspect", json!({"entities": false})).await?;
        if d["path"].as_str().is_none_or(str::is_empty) {
            let title = d["title"].as_str().unwrap_or("the drawing");
            return Err(format!("`{title}` has no file yet: `{CLI} save <path.dwg>`"));
        }
        self.bridge.exec("qsave", json!({})).await
    }

    async fn export(&self, path: Option<String>) -> Result<Json, String> {
        let p = path.ok_or("`export` writes to a path")?;
        match rules::extension(&p).as_str() {
            "pdf" => self.bridge.exec("exportpdf", json!({"path": p})).await,
            "svg" | "png" | "dxf" | "dwg" => self.bridge.exec("export", json!({"path": p})).await,
            other => Err(format!("`export` writes .pdf, .svg, .png, .dxf or .dwg, not .{other}")),
        }
    }

    /// Quit CADCraft through the control channel, which skips its Save Changes question: so only
    /// while nothing is unsaved and the person is not in a command.
    async fn close(&self) -> Result<Json, String> {
        self.not_in_persons_command().await?;
        let ui = self.bridge.call("ui.inspect", json!({})).await?;
        let unsaved: Vec<String> = ui["session"]["documents"]
            .as_array()
            .map(|d| d.iter().filter(|w| w["dirty"].as_bool() == Some(true)).filter_map(|w| w["title"].as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if !unsaved.is_empty() {
            return Err(format!("unsaved changes in {}: save first (`{CLI} save`) or leave closing to the person", unsaved.join(", ")));
        }
        self.bridge.call("app.quit", json!({})).await.map(|_| json!({"closed": true}))
    }

    /// The answer, or a file holding it when it is too big for the socket.
    fn deliver(&self, result: Json) -> Json {
        let body = json!({"ok": true, "result": result});
        let text = body.to_string();
        if text.len() <= SPILL {
            return body;
        }
        let file = self.spill_dir.join(format!("{}.json", self.next.fetch_add(1, Ordering::Relaxed)));
        match std::fs::write(&file, text) {
            Ok(()) => json!({"ok": true, "spill": file}),
            Err(e) => json!({"ok": false, "error": format!("an answer too big to send, and {} cannot be written: {e}", file.display())}),
        }
    }
}

/// The named fields of a request that are present, as a command's parameters.
fn pick(req: &Json, keys: &[&str]) -> Json {
    let mut out = Map::new();
    for k in keys {
        if let Some(v) = req.get(*k).filter(|v| !v.is_null()) {
            out.insert((*k).to_string(), v.clone());
        }
    }
    Json::Object(out)
}

#[cfg(test)]
mod tests {
    use super::pick;
    use serde_json::json;

    #[test]
    fn only_present_fields_become_parameters() {
        let req = json!({"verb": "read", "type": "Line", "layer": null, "limit": 5});
        assert_eq!(pick(&req, &["type", "layer", "window"]), json!({"type": "Line"}));
    }
}
