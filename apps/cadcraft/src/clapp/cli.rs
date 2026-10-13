//! The agent's CLI. `cadcraft help` is its only manual: Clatch ships no other document, so a verb
//! missing from it does not exist as far as an agent is concerned. Every verb is declared in
//! clatch.json's `connector.commands` and is its own grant (`Bash(cadcraft <verb>:*)`).
//!
//! Words in, lines out: data on stdout, complaints on stderr. Exit 0 done, 1 refused, 2 usage.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{Value as Json, json};

use super::CLI;

/// The declared verbs, in the order the help gives them. clatch.json must say the same.
pub const VERBS: &[&str] =
    &["state", "read", "commands", "shot", "cmd", "cancel", "run", "new", "open", "save", "export", "import", "focus", "close"];

const HELP: &str = r#"cadcraft - draft with the person, in CADCraft's own window.

Everything you change shows in the window as it happens and lands on the same undo history
as the person's work. You do not poll: Clatch tells you what the person does. `changed`
(context) lists the commands they completed once they pause; `selected` (buffered) rides
their next message with what they have selected, so "these lines" means that selection.

Look (changes nothing)
  cadcraft state                        the open drawings, the one in front (space, layers,
                                        entity counts, extents, selection) and the command line
  cadcraft read [--type <T>] [--layer <L>] [--window x1,y1,x2,y2] [--selected]
                [--limit <n>] [--offset <n>]
                                        entities with their geometry (default: the first 200)
  cadcraft commands [<words>]           CADCraft's commands, aliases and what each takes;
                                        [not run] marks one `cmd` and `run` will not send
  cadcraft shot --out <png> [--view] [--size <w>x<h>]
                                        a picture of the drawing, drawn off-screen: fitted to
                                        the drawing, or --view as the window shows it now

Draw and edit
  cadcraft cmd <command line>           type one line at the command line, as the person would:
                                        cadcraft cmd circle 0,0 5
                                        cadcraft cmd line 0,0 10,0 10,5 ""
                                        A command still waiting for input stays yours: answer
                                        it with more `cmd` lines (cadcraft cmd 20,5), or
  cadcraft cancel                       end it (Esc). The person's own commands are never
                                        typed into or cancelled.
  cadcraft run <command> ['<params-json>']
                                        any of CADCraft's commands with JSON parameters, no
                                        prompts: cadcraft run layer.new '{"name": "Walls"}'

Files (the only verbs that touch the disk)
  cadcraft new [--metric] [--sample]
  cadcraft open <path>                  .dwg .dxf .dwt .svg .svgz
  cadcraft save [<path>]                to its own file; a .dwg/.dxf/.dwt path becomes its file
  cadcraft export <path>                a copy as .pdf, .svg, .png, .dxf or .dwg
  cadcraft import <path> [--at x,y] [--scale <s>]
                                        a DWG or DXF drawn into the drawing in front

The window
  cadcraft focus                        bring the window to the front
  cadcraft close                        quit CADCraft; refused while anything is unsaved

Nothing here closes or overwrites unsaved work, changes the person's settings (snaps,
grid, options), or reaches the clipboard. If CADCraft ends with unsaved changes (stopped,
crashed), they are offered back at the next launch in its Drawing Recovery window.
Several drawings can be open; every verb acts on the one in front, and
`cadcraft run document.switch '{"index": <n>}'` switches (from 0). Points are x,y in
drawing units; handles are hex, as `read` gives them. A path is read from where you run
the command. These verbs reach the app over a local socket, which a sandbox can block: if
one fails that way, run it again with escalated permissions.
Exit: 0 done, 1 refused, 2 the command line is wrong."#;

enum Fail {
    Refused(String),
    Usage(String),
}

type Res<T> = Result<T, Fail>;

fn usage<T>(msg: impl Into<String>) -> Res<T> {
    Err(Fail::Usage(msg.into()))
}

pub async fn run(args: Vec<String>) -> ! {
    let code = match dispatch(args).await {
        Ok(()) => 0,
        Err(Fail::Refused(msg)) => {
            eprintln!("{CLI}: {msg}");
            1
        }
        Err(Fail::Usage(msg)) => {
            eprintln!("{CLI}: {msg}\nSee `{CLI} help`.");
            2
        }
    };
    std::process::exit(code)
}

/// The CLI role's `main`: a fresh runtime, one request, then exit.
pub fn main(args: Vec<String>) -> ! {
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(run(args)),
        Err(e) => {
            eprintln!("{CLI}: cannot start a runtime: {e}");
            std::process::exit(1)
        }
    }
}

/// The command line: words, `--name value` pairs and the bare switches a verb takes.
struct Args {
    words: Vec<String>,
    flags: HashMap<String, String>,
}

impl Args {
    fn parse(raw: Vec<String>, switches: &[&str]) -> Res<Args> {
        let (mut words, mut flags) = (Vec::new(), HashMap::new());
        let mut it = raw.into_iter();
        while let Some(a) = it.next() {
            if a == "--" {
                words.extend(it.by_ref());
            } else if let Some(name) = a.strip_prefix("--").filter(|n| !n.is_empty()) {
                if switches.contains(&name) {
                    flags.insert(name.to_string(), String::new());
                } else {
                    let Some(value) = it.next() else { return usage(format!("--{name} needs a value")) };
                    flags.insert(name.to_string(), value);
                }
            } else {
                words.push(a);
            }
        }
        Ok(Args { words, flags })
    }
    fn word(&self, i: usize) -> Option<&str> {
        self.words.get(i).map(String::as_str)
    }
    fn on(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }
    fn flag(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(String::as_str)
    }
    fn at_most(&self, n: usize) -> Res<()> {
        match self.words.get(n) {
            Some(extra) => usage(format!("unexpected `{extra}`")),
            None => Ok(()),
        }
    }
}

async fn dispatch(raw: Vec<String>) -> Res<()> {
    let verb = raw.first().cloned().unwrap_or_else(|| "help".into());
    if matches!(verb.as_str(), "help" | "-h" | "--help") {
        println!("{HELP}");
        return Ok(());
    }
    // `cmd` passes its line through untouched: `--` or `""` mean something at the command line.
    if verb == "cmd" {
        // An empty argument is Enter; so is a literal `""`, which a shell passes when it was quoted.
        let enter = |w: &String| w.is_empty() || w == "\"\"";
        let text = raw[1..].iter().map(|w| if enter(w) { String::new() } else { w.clone() }).collect::<Vec<_>>().join(" ");
        if text.trim().is_empty() && !raw[1..].iter().any(enter) {
            return usage("cmd <command line>, such as: cmd circle 0,0 5");
        }
        let result = ask(json!({"verb": "cmd", "text": text})).await?;
        show_cmd(&result);
        return Ok(());
    }
    let a = Args::parse(raw, &["selected", "view", "metric", "sample", "json", "help"])?;
    if a.on("help") {
        println!("{HELP}");
        return Ok(());
    }
    let req = match verb.as_str() {
        "state" | "cancel" | "focus" | "close" => {
            a.at_most(1)?;
            json!({"verb": verb})
        }
        "read" => read_request(&a)?,
        "commands" => json!({"verb": "commands", "search": (a.words.len() > 1).then(|| a.words[1..].join(" "))}),
        "shot" => shot_request(&a)?,
        "run" => run_request(&a)?,
        "new" => {
            a.at_most(1)?;
            json!({"verb": "new", "metric": a.on("metric").then_some(true), "sample": a.on("sample")})
        }
        "open" | "export" | "import" => path_request(&verb, &a)?,
        "save" => {
            a.at_most(2)?;
            json!({"verb": "save", "path": a.word(1).map(full).transpose()?})
        }
        other => return usage(format!("no verb `{other}`; the verbs are {}", VERBS.join(", "))),
    };
    let result = ask(req).await?;
    show(&verb, &a, &result);
    Ok(())
}

fn number(name: &str, text: &str) -> Res<f64> {
    text.trim().parse::<f64>().ok().filter(|n| n.is_finite()).ok_or_else(|| Fail::Usage(format!("--{name} takes a number, not `{text}`")))
}

/// `x,y` as a JSON point.
fn point(name: &str, text: &str) -> Res<Json> {
    let Some((x, y)) = text.split_once(',') else { return usage(format!("--{name} takes x,y, not `{text}`")) };
    Ok(json!([number(name, x)?, number(name, y)?]))
}

fn read_request(a: &Args) -> Res<Json> {
    a.at_most(1)?;
    let window = match a.flag("window") {
        None => None,
        Some(w) => {
            let n: Vec<&str> = w.split(',').collect();
            if n.len() != 4 {
                return usage("--window takes x1,y1,x2,y2");
            }
            Some(json!([[number("window", n[0])?, number("window", n[1])?], [number("window", n[2])?, number("window", n[3])?]]))
        }
    };
    let whole = |name: &str| -> Res<Option<u64>> {
        a.flag(name).map(|v| v.parse::<u64>().map_err(|_| Fail::Usage(format!("--{name} takes a whole number, not `{v}`")))).transpose()
    };
    Ok(json!({
        "verb": "read",
        "type": a.flag("type"),
        "layer": a.flag("layer"),
        "window": window,
        "selected": a.on("selected"),
        "limit": whole("limit")?,
        "offset": whole("offset")?,
    }))
}

fn shot_request(a: &Args) -> Res<Json> {
    a.at_most(1)?;
    let Some(out) = a.flag("out") else { return usage("`shot` writes to --out <png>") };
    let (width, height) = match a.flag("size") {
        None => (None, None),
        Some(s) => {
            let Some((w, h)) = s.split_once(['x', 'X']) else { return usage("--size takes <w>x<h>, such as 1600x1000") };
            let px = |v: &str| {
                v.trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (16..=8192).contains(n))
                    .ok_or_else(|| Fail::Usage(format!("--size takes 16 to 8192 pixels a side, not `{v}`")))
            };
            (Some(px(w)?), Some(px(h)?))
        }
    };
    Ok(json!({"verb": "shot", "path": full(out)?, "view": a.on("view"), "width": width, "height": height}))
}

fn run_request(a: &Args) -> Res<Json> {
    a.at_most(3)?;
    let Some(command) = a.word(1) else { return usage("run <command> ['<params-json>']") };
    let params = match a.word(2) {
        None => json!({}),
        Some(text) => match serde_json::from_str::<Json>(text) {
            Ok(v) if v.is_object() => v,
            _ => return usage(format!("parameters are a JSON object, such as '{{\"name\": \"Walls\"}}', not {text}")),
        },
    };
    Ok(json!({"verb": "run", "command": command, "params": params}))
}

fn path_request(verb: &str, a: &Args) -> Res<Json> {
    a.at_most(2)?;
    let Some(path) = a.word(1) else { return usage(format!("{verb} <path>")) };
    let path = full(path)?;
    if verb != "export" && !std::path::Path::new(&path).is_file() {
        return Err(Fail::Refused(format!("no file at {path}")));
    }
    let at = a.flag("at").map(|p| point("at", p)).transpose()?;
    let scale = a.flag("scale").map(|s| number("scale", s)).transpose()?;
    Ok(json!({"verb": verb, "path": path, "at": at, "scale": scale}))
}

/// A path as the person running the command means it: from their folder, `~` their home.
fn full(path: &str) -> Res<String> {
    let p = match path.strip_prefix("~/") {
        Some(rest) => clappkit::paths::home().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    };
    std::path::absolute(&p).map(|p| p.to_string_lossy().into_owned()).map_err(|e| Fail::Usage(format!("{path}: {e}")))
}

// --- talking to the window --------------------------------------------------------------------

async fn ask(mut req: Json) -> Res<Json> {
    // Who is calling: Clatch puts the agent's id in its shell.
    if let Some(agent) = std::env::var("CLATCH_AGENT_ID").ok().filter(|s| !s.is_empty()) {
        req["agent"] = json!(agent);
    }
    let reply = clappkit::ipc::request(CLI, &req).await.map_err(|e| Fail::Refused(strip(&format!("{e:#}"))))?;
    if reply.get("ok").and_then(Json::as_bool) != Some(true) {
        return Err(Fail::Refused(reply.get("error").and_then(Json::as_str).unwrap_or("refused").to_string()));
    }
    if let Some(spill) = reply.get("spill").and_then(Json::as_str) {
        let text = std::fs::read_to_string(spill).map_err(|e| Fail::Refused(format!("cannot read the answer at {spill}: {e}")))?;
        let _ = std::fs::remove_file(spill);
        let body: Json = serde_json::from_str(&text).map_err(|e| Fail::Refused(format!("an unreadable answer: {e}")))?;
        return Ok(body.get("result").cloned().unwrap_or(Json::Null));
    }
    Ok(reply.get("result").cloned().unwrap_or(Json::Null))
}

/// clappkit's messages already start with `<cli>: `; `run` adds it once.
fn strip(msg: &str) -> String {
    msg.strip_prefix(&format!("{CLI}: ")).unwrap_or(msg).to_string()
}

// --- answers as lines -------------------------------------------------------------------------

fn show(verb: &str, a: &Args, v: &Json) {
    if a.on("json") {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()));
        return;
    }
    match verb {
        "commands" => {
            let rows = v.as_array().cloned().unwrap_or_default();
            for r in &rows {
                let id = r["id"].as_str().unwrap_or("");
                let aliases = r["aliases"]
                    .as_array()
                    .filter(|x| !x.is_empty())
                    .map(|x| format!(" ({})", x.iter().filter_map(Json::as_str).collect::<Vec<_>>().join(", ")))
                    .unwrap_or_default();
                let off = if r["enabled"].as_bool() == Some(false) { "  [off]" } else { "" };
                let not_run = if r["run"].as_bool() == Some(false) { "  [not run]" } else { "" };
                println!("{id:<28} {}{aliases}{off}{not_run}", r["label"].as_str().unwrap_or(""));
                if let Some(p) = r["params"].as_str().filter(|p| !p.is_empty() && rows.len() <= 10) {
                    println!("{:<28}   {p}", "");
                }
            }
            eprintln!("{} commands", rows.len());
        }
        "shot" => println!("{}", v["path"].as_str().unwrap_or("")),
        _ => {
            for line in lines(v, 0) {
                println!("{line}");
            }
        }
    }
}

/// A command line's answer as the command line shows it: its history lines, then the prompt.
fn show_cmd(v: &Json) {
    for line in v["output"].as_array().into_iter().flatten().filter_map(Json::as_str) {
        println!("{line}");
    }
    if let Some(running) = v["running"].as_str() {
        println!(
            "{}  [{} waiting: answer with `{CLI} cmd ...`, or `{CLI} cancel`]",
            v["prompt"].as_str().unwrap_or(""),
            running.to_ascii_uppercase()
        );
    }
    if let Some(e) = v["error"].as_str() {
        eprintln!("{CLI}: {e}");
    }
}

/// Any answer as `key: value` lines, nested keys indented, list items under `-`.
fn lines(v: &Json, indent: usize) -> Vec<String> {
    let pad = " ".repeat(indent);
    let mut out = Vec::new();
    match v {
        Json::Null => out.push(format!("{pad}done")),
        Json::Object(map) => {
            for (k, val) in map {
                match inline(val) {
                    Some(t) => out.push(format!("{pad}{k}: {t}")),
                    None => {
                        out.push(format!("{pad}{k}:"));
                        out.extend(lines(val, indent + 2));
                    }
                }
            }
        }
        Json::Array(items) => {
            for item in items {
                match inline(item) {
                    Some(t) => out.push(format!("{pad}- {t}")),
                    None => {
                        out.push(format!("{pad}-"));
                        out.extend(lines(item, indent + 2));
                    }
                }
            }
        }
        other => out.push(format!("{pad}{}", scalar(other))),
    }
    out
}

fn inline(v: &Json) -> Option<String> {
    match v {
        Json::Object(m) if m.is_empty() => Some("{}".into()),
        Json::Array(a) if a.is_empty() => Some("[]".into()),
        Json::Array(a) if a.iter().all(|x| !x.is_object() && !x.is_array()) => {
            Some(format!("[{}]", a.iter().map(scalar).collect::<Vec<_>>().join(", ")))
        }
        Json::Object(_) | Json::Array(_) => None,
        other => Some(scalar(other)),
    }
}

fn scalar(v: &Json) -> String {
    match v {
        Json::String(s) if s.contains(['\n', '\r', '\t']) || s.trim() != s || s.is_empty() => serde_json::to_string(s).unwrap_or_default(),
        Json::String(s) => s.clone(),
        Json::Null => "-".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest declares exactly the verbs the help documents, in the same order.
    #[test]
    fn the_manifest_declares_exactly_the_verbs_the_help_gives() {
        let manifest: Json = serde_json::from_str(include_str!("../../../../clatch.json")).unwrap();
        let declared: Vec<&str> = manifest["connector"]["commands"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(declared, VERBS);
        for v in VERBS {
            assert!(HELP.lines().any(|l| l.starts_with(&format!("  {CLI} {v}"))), "the help never shows `{v}`");
        }
    }

    /// What the binary prints of its own is ASCII: a legacy Windows console turns anything else
    /// into mojibake, and this output is an agent's input.
    #[test]
    fn the_help_is_ascii() {
        assert!(HELP.is_ascii());
    }

    #[test]
    fn points_and_windows_parse() {
        assert_eq!(point("at", "1.5,-2").ok(), Some(json!([1.5, -2.0])));
        assert!(point("at", "1;2").is_err());
        let a = Args::parse(vec!["read".into(), "--window".into(), "0,0,10,5".into(), "--selected".into()], &["selected"]).ok().unwrap();
        let r = read_request(&a).ok().unwrap();
        assert_eq!(r["window"], json!([[0.0, 0.0], [10.0, 5.0]]));
        assert_eq!(r["selected"], json!(true));
    }
}
