# CADCraft as a clapp

This repository is [CADCraft](https://github.com/storytold/cadcraft) (remote `upstream`), made
into a Clatch app on [clappkit](clappkit/README.md), the way
[template-clapp](https://github.com/arfium/template-clapp) builds one. Upstream's own docs and
rules (`AGENTS.md`, `docs/`) still apply; this page is only what the fork adds.

## How it fits together

One binary, two roles, one state (clappkit `docs/architecture.md`):

```text
person ─ clicks, keys, command line ─▶ CADCraft window ─┐
                                                       ├─▶ Session (the drawing) ─ one undo history
agent ── cadcraft <verb> ─▶ ~/.cadcraft/cadcraft.sock ─┘
Clatch ◀── control pipe: register, signals ── the window
```

- `cadcraft app` is the window Clatch launches. It runs only under Clatch: started any other
  way it asks Clatch to start it (`CLATCH_STANDALONE=1` runs it without, for development).
- `cadcraft <verb>` is the agent's CLI. A request arrives on clappkit's socket and becomes a
  request on the queue CADCraft's own control channel feeds, so it runs through the same
  `control::handle` and `CadApp::run` the person's command line does, and the window shows it
  as it happens.
- Under Clatch, settings, logs and autosave files live in `CLATCH_DATA_DIR`, which
  uninstalling can erase.

All of it is in [`apps/cadcraft/src/clapp/`](apps/cadcraft/src/clapp/mod.rs):

| file | is |
|---|---|
| `mod.rs` | which role a launch is; the bootstrap |
| `cli.rs` | the verbs and `cadcraft help`, the agent's only manual |
| `door.rs` | the window's side of the socket: each verb as CADCraft control methods |
| `rules.rs` | what an agent never does here |
| `signals.rs` | what the person does, told to the agent |
| `recovery.rs` | CADCraft's autosave, run seconds after a change |
| `wiring.rs` | the control pipe and the socket, on a runtime beside eframe |

## The verbs are the grants

Each is declared in `clatch.json` and is its own permission (`Bash(cadcraft <verb>:*)`).

| look | draw and edit | files | the window |
|---|---|---|---|
| `state` `read` `commands` `shot` | `cmd` `cancel` `run` | `new` `open` `save` `export` `import` | `focus` `close` |

`cmd` types one line at the command line, exactly as the person would (`cadcraft cmd circle
0,0 5`); `run` calls a command with JSON parameters and no prompts. `shot` renders the drawing
off-screen, so it never disturbs the window.

**One command line, two people.** A command the agent's `cmd` left running (LINE, waiting for
its next point) is the agent's to answer or `cancel`. Any other running command is the
person's: `cmd`, `run`, `cancel` and the file verbs refuse until it ends.

## Signals: the agent keeps up with the person

| signal | type | carries |
|---|---|---|
| `changed` | context | the commands the person completed (Circle, Move, Erase...; an undo or redo as such), once they pause for 1.2 s, one notice per burst |
| `selected` | buffered | the drawing, space, and the selection's count, entity types and handles, latest only, riding the person's next message |

Only the person's actions signal. Every change to a drawing pushes an undo step labelled with
its command, so the new steps are the list. Within a frame the agent's requests run in
`CadApp::logic` (its control queue) and the person's input in `ui`, so the app observes before
`logic` and after `ui`, and settles after `logic`.

## Unsaved work is never lost

CADCraft already has AutoCAD-style autosave and Drawing Recovery. The clapp keeps it, with two
changes Clatch needs:

- **Seconds, not minutes.** Clatch stops the window with no time for a shutdown hook, and
  SAVETIME is 10 minutes by default. Under Clatch, [`recovery.rs`](apps/cadcraft/src/clapp/recovery.rs)
  asks CADCraft's own saver to write about a second after a change (every 5 s at most during a
  burst). SAVETIME 0, autosave off, is respected.
- **At once after a relaunch.** Drawing Recovery skipped another instance's files until its
  lock was 5 minutes old. Clatch runs one instance per app, so `autosave::set_single_instance`
  makes them recoverable straight away.

The rest is upstream's: the `.sv$` files in `<data dir>/autosave/`, deleting one once its
drawing is saved or closed, and the Drawing Recovery window, which opens each drawing as an
unsaved copy with no file name.

## What was measured, and the rule it became

Found by driving the running app, not by reading its docs:

- **`close` and `closeall` through the control channel throw unsaved changes away without
  asking.** `run` never sends them; `cadcraft close` refuses while anything is unsaved.
- **`app.quit` skips the Save Changes question** the window's own close asks. Same rule.
- **`qsave` on a drawing with no file is refused** (no dialog). `cadcraft save` says so and
  asks for a path.
- **Only .dwg, .dxf and .dwt become the drawing's file**; `saveas` to .svg writes a copy and
  leaves the drawing unsaved. `save` takes those three; `export` writes the copies.
- **At the command line, the rest of a line is ignored once its first command ends, and an
  empty input is Enter, which repeats the last command** (maybe the person's). `cmd` checks the
  command it starts (aliases resolved) and any transparent `'command`, and refuses an empty line.
- **LISP at the command line runs any command and reaches the disk.** `cmd` refuses `(...)` and
  `!name`; `run` refuses `lisp`, `appload` and `script`.
- **Every disk access is a `path`, `file` or `output` parameter.** `run` refuses them all, so
  a grant of `run` is never a grant of the person's files.
- **Profile variables are the person's** (snaps, grid, pick box: `options::PROFILE_VARS`), as
  are SAVETIME, SAVEFILEPATH, QNEWTEMPLATE, the clipboard, aliases, plotter setups and `ui.*`.
- **eframe keeps the preferences (SAVETIME, SAVEFILEPATH, the profile) in its own folder**,
  shared with any other CADCraft on the machine: the installed clapp autosaved into another
  run's SAVEFILEPATH. Under Clatch that storage is in `CLATCH_DATA_DIR` too.
- **A control-channel `ui.pointer` click runs inside `logic`**, while `ui.click` is real input
  handled in `ui`: only the latter counts as the person's for the signals.

## Upstream patches

Everything new is `apps/cadcraft/src/clapp/`, `clatch.json`, `CLAPP.md`, `bin/`, `scripts/`,
`package.json`, `.github/`, `.gitattributes`, `assets/icon*.png` and `clappkit`. Upstream files
carry only these, each marked `clapp:`:

| file | change |
|---|---|
| `apps/cadcraft/src/main.rs` | role dispatch and the bootstrap; the signal observer and the recovery tick around `logic` and `ui`; settings, and eframe's storage, in `CLATCH_DATA_DIR`; one-instance recovery under Clatch; no `windows_subsystem` (the CLI needs a console) |
| `apps/cadcraft/Cargo.toml` | `clappkit` (core only) and `tokio` |
| `crates/engine/src/autosave.rs` | `set_single_instance`, off unless Clatch launched the window |
| `Cargo.toml` | `clappkit` excluded from the workspace (it has its own) |
| `AGENTS.md` | a pointer to this file |
| `ATTRIBUTION.md` | rows for `assets/icon.png` and `assets/icon-dock.png` |
| `.gitignore` | `pkg/` and `*.clapp` |
| `.github/workflows/` | upstream's removed (they would run on this fork's Actions minutes); template-clapp's CI (manual) and release (on a `v*` tag) in their place |

## Build, verify, ship

```sh
git clone --recurse-submodules <this repository>
npm run verify          # build -> package -> clatch validate -> CLI <-> window round-trip
npm run pack            # pkg/ -> com.arfium.cadcraft-<os>-<arch>.clapp
CLATCH_STANDALONE=1 cargo run -p cadcraft -- app    # the window, without Clatch
```

`npm run check` checks that `clatch.json`, the CLI's verbs and help, and the emitted signals
agree. `cargo xtask ci` is upstream's own gate. `pkg/` and `*.clapp` are build output, never
committed. Push a `v*` tag and the release workflow publishes one `.clapp` per platform.

Verified under a live Clatch daemon too: `clatch install`, `run`, the CLI against the
Clatch-launched window, and `clatch stop` with unsaved work (its autosave file written seconds
before, kept for Drawing Recovery). `npm run verify` launches the window standalone, outside
Clatch, so that run uses upstream's own settings folders.

Based on CADCraft's `main` at workspace version 0.6.2.

## Not done yet

- **Windows** is built by the workflows but not yet run.
- **A LISP routine the person runs** advances a slice per frame inside `logic`, so its changes
  are taken for the agent's and not signalled.
