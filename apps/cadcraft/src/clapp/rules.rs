//! What an agent may run, by `cadcraft run` or as the command of a `cadcraft cmd` line. Held in
//! the window (the door checks it), so a program talking to the socket directly is held to it
//! too.
//!
//! Measured against the running app: `close` (and `closeall`) through the control channel throw
//! away unsaved changes without asking; `qsave` on a drawing with no file is refused; `saveas`
//! to .dxf/.dwg/.dwt makes that the drawing's file, while .svg and the like write a copy and
//! leave the drawing unsaved. Files are therefore the file verbs' alone, and they refuse the
//! cases above.
//!
//! Anything that touches the disk does so through a parameter (`path`, `file`, `output`), and
//! `run` never sends one: the verbs `open`, `save`, `export` and `import` are the only way in or
//! out, so a grant of `run` is never a grant of the person's files.

use serde_json::Value as Json;

/// Never run for an agent, by any verb. A trailing `.` is a prefix.
pub const NEVER: &[(&str, &str)] = &[
    ("open", "files are `cadcraft open`, `save`, `export` and `new`"),
    ("new", "files are `cadcraft open`, `save`, `export` and `new`"),
    ("qsave", "saving is `cadcraft save`, which never leaves a Save As dialog waiting"),
    ("saveas", "saving is `cadcraft save <path>`"),
    ("close", "closing a drawing throws its unsaved changes away without asking: that is the person's call"),
    ("closeall", "closing drawings throws their unsaved changes away without asking: that is the person's call"),
    ("recover", "recovering drawings is the person's, in the Drawing Recovery window"),
    ("drawingrecovery", "recovering drawings is the person's, in the Drawing Recovery window"),
    ("export", "copies are `cadcraft export <path>`"),
    ("exportpdf", "copies are `cadcraft export <path.pdf>`"),
    ("import", "bringing a drawing in is `cadcraft import <path>`"),
    ("plot", "printing is the person's"),
    ("publish", "publishing writes files beside the drawing: the person's call"),
    ("pdfbook", "sheet books write files beside the drawing: the person's call"),
    ("script", "a script runs commands past these rules"),
    ("appload", "a LISP application runs commands past these rules and reaches the disk"),
    ("lisp", "LISP runs commands past these rules and reaches the disk"),
    ("aliasedit", "command aliases are the person's"),
    ("reinit", "command aliases are the person's"),
    ("options", "the options are the person's preferences"),
    ("plottermanager", "plotter configurations are the person's"),
    ("-plottermanager", "plotter configurations are the person's"),
    ("stylesmanager", "plot style tables are the person's"),
    ("copyclip", "the clipboard is the person's"),
    ("cutclip", "the clipboard is the person's"),
    ("copybase", "the clipboard is the person's"),
    ("pasteclip", "the clipboard is the person's"),
    ("pasteorig", "the clipboard is the person's"),
    ("pasteblock", "the clipboard is the person's"),
    ("ui.", "the window's own controls and preferences are the person's"),
];

/// System variables that are the person's preferences rather than the drawing's.
const PERSONAL_VARS: &[&str] = &["SAVETIME", "SAVEFILEPATH", "QNEWTEMPLATE"];

/// Parameters that name a file on disk.
const DISK_PARAMS: &[&str] = &["path", "file", "output"];

/// The rule for engine command `id` (aliases resolved) with `params`.
pub fn run(id: &str, params: &Json) -> Result<(), String> {
    let id = id.trim().to_ascii_lowercase();
    if let Some(why) = never(&id) {
        return Err(format!("`{id}` is not run here: {why}"));
    }
    if let Some(key) = DISK_PARAMS.iter().find(|k| params.get(**k).is_some_and(|v| !v.is_null())) {
        return Err(format!(
            "`{key}` names a file on disk, which `run` never reaches: drawings come in with `cadcraft open` or \
             `cadcraft import`, and go out with `cadcraft save` or `cadcraft export`"
        ));
    }
    if id == "setvar" {
        let name = params.get("name").and_then(Json::as_str).unwrap_or("");
        if personal_var(name) {
            return Err(format!("{} is the person's setting, not the drawing's", name.to_ascii_uppercase()));
        }
    }
    Ok(())
}

/// Whether `run` sends `id` at all, parameters aside: for `commands`.
pub fn runnable(id: &str) -> bool {
    never(&id.to_ascii_lowercase()).is_none()
}

fn never(id: &str) -> Option<&'static str> {
    NEVER.iter().find(|(entry, _)| if entry.ends_with('.') { id.starts_with(entry) } else { id == *entry }).map(|(_, why)| *why)
}

/// A drafting setting of the person's profile (snaps, grid, pick box...), or one of
/// [`PERSONAL_VARS`].
pub fn personal_var(name: &str) -> bool {
    let name = name.trim();
    PERSONAL_VARS.iter().any(|v| v.eq_ignore_ascii_case(name)) || cadcraft_engine::cmd::options::is_profile_var(name)
}

/// The extensions that become the drawing's own file when saved.
pub fn becomes_the_file(path: &str) -> bool {
    matches!(extension(path).as_str(), "dwg" | "dxf" | "dwt")
}

pub fn extension(path: &str) -> String {
    std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// What a `cmd` line starts at `Command:`: `Err` when it is not a command the agent may run.
/// Measured: after the first command ends, the rest of a command line is ignored, an empty input
/// is Enter (which repeats the last command, maybe the person's), and `(...)` or `!name` is LISP.
pub fn command_line(text: &str) -> Result<(), String> {
    let first = text.split_whitespace().next().unwrap_or("");
    if first.is_empty() {
        return Err("an empty line is Enter, which repeats the last command (maybe the person's): name the command".into());
    }
    if lisp(first) {
        return Err("LISP runs commands past these rules and reaches the disk; draw with commands instead".into());
    }
    let name = first.trim_start_matches(['_', '.', '\'']).to_ascii_lowercase();
    let rest = text.trim_start().get(first.len()..).unwrap_or("").trim_start();
    if rest.starts_with('{') {
        let params: Json = serde_json::from_str(rest).map_err(|e| format!("`{first} {{...}}` is not JSON: {e}"))?;
        return run(&cadcraft_engine::cmd::resolve_alias(&name), &params);
    }
    run(&cadcraft_engine::cmd::resolve_alias(&name), &Json::Null)?;
    inputs(text.trim_start().get(first.len()..).unwrap_or(""))
}

/// What a `cmd` line answers a running command's prompts with: never LISP, and never a
/// transparent `'command` the agent could not run on its own.
pub fn inputs(text: &str) -> Result<(), String> {
    for tok in text.split_whitespace() {
        if lisp(tok) {
            return Err("LISP runs commands past these rules and reaches the disk: answer the prompt with a value".into());
        }
        if let Some(name) = tok.strip_prefix('\'') {
            run(&cadcraft_engine::cmd::resolve_alias(&name.to_ascii_lowercase()), &Json::Null)?;
        }
    }
    Ok(())
}

fn lisp(tok: &str) -> bool {
    tok.starts_with('(') || tok.starts_with('!')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn what_run_reaches() {
        assert!(run("circle", &json!({"center": [0, 0], "radius": 5})).is_ok());
        assert!(run("layer.new", &json!({"name": "Walls"})).is_ok());
        assert!(run("close", &json!({})).is_err(), "close throws unsaved work away");
        assert!(run("CLOSE", &json!({})).is_err());
        assert!(run("ui.theme.dark", &json!({})).is_err());
        assert!(run("insert", &json!({"name": "Door"})).is_ok(), "a block already in the drawing");
        assert!(run("insert", &json!({"name": "Door", "file": "/x/door.dwg"})).is_err(), "a block from disk");
        assert!(run("wblock", &json!({"path": "/x/b.dwg", "name": "B"})).is_err());
        assert!(run("setvar", &json!({"name": "osmode", "value": 0})).is_err(), "the person's snaps");
        assert!(run("setvar", &json!({"name": "SAVETIME", "value": 0})).is_err());
        assert!(run("setvar", &json!({"name": "CLAYER", "value": "0"})).is_ok(), "the drawing's current layer");
    }

    #[test]
    fn a_command_line_is_checked_by_its_command() {
        assert!(command_line("circle 0,0 5").is_ok());
        assert!(command_line("c 0,0 5").is_ok(), "aliases resolve");
        assert!(command_line("_.line 0,0 10,0 ").is_ok());
        assert!(command_line("close").is_err());
        assert!(command_line("qsave").is_err());
        assert!(command_line("(command \"save\")").is_err());
        assert!(command_line("!x").is_err());
        assert!(command_line("").is_err());
        assert!(command_line("  ").is_err());
        assert!(command_line("line 0,0 'options").is_err(), "a transparent command is checked too");
        assert!(command_line("line 0,0 (list 1 2)").is_err());
        assert!(command_line("open {\"path\": \"/x.dwg\"}").is_err());
        assert!(command_line("layer.new {\"name\": \"A\"}").is_ok());
    }

    #[test]
    fn only_drawing_formats_become_the_file() {
        assert!(becomes_the_file("/x/Plan.DWG"));
        assert!(becomes_the_file("p.dxf"));
        assert!(!becomes_the_file("p.svg"));
        assert!(!becomes_the_file("p.pdf"));
    }
}
