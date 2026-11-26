use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use lune_domain::projects::Opens;
use lune_waits::{Patience, eventually};
use luned::api::change_projects_request::{AddProject, Change};
use luned::api::show_request::{self, Event, Hello, Size, open};
use luned::api::show_response::{self, Laid, view::Found};
use luned::api::{
    self, ListResumeRequest, ListResumeResponse, ReloadResumeRequest, ReloadResumeResponse,
    ResumePersistence, ResumePlacement, ResumeRecordSource, ShowRequest, ShowResponse, TerminalKey,
};
use luned::mux::own::api as mux_api;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Streaming;

use crate::agents::file_of_pid;
use crate::host::{LUNE, PATIENT, folder_in};
use crate::own::Owned;
use crate::projects::asked;

const CLAUDE_UUID: &str = "015d3962-6f9e-45c3-92bc-c7cef5068215";

struct Shown<'a> {
    owned: &'a Owned,
    asks: mpsc::Sender<ShowRequest>,
    heard: Streaming<ShowResponse>,
    laid: Option<Laid>,
}

impl<'a> Shown<'a> {
    fn new(owned: &'a Owned, worktree: u64) -> Self {
        let folder = folder_in(owned.host.runtime.path()).unwrap();
        let (asks, asked) = mpsc::channel(16);
        let hello = Hello {
            worktree,
            size: Some(Size { columns: 80, rows: 24 }),
            runs_in: None,
            again: false,
            on: None,
        };
        asks.try_send(ShowRequest { event: Some(Event::Hello(hello)) }).unwrap();
        let heard = owned.block_on(async move {
            let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
            daemon.show(ReceiverStream::new(asked)).await.unwrap().into_inner()
        });
        let mut shown = Shown { owned, asks, heard, laid: None };
        shown.until("first layout", |_| true);
        shown
    }

    fn open_shell(&mut self) -> TerminalKey {
        let open = show_request::Open {
            beside: Some(open::Beside::Largest(())),
            opens: Some(Opens::Shell.to_api()),
        };
        self.asks.try_send(ShowRequest { event: Some(Event::Open(open)) }).unwrap();
        let laid = self.until("shell terminal ready", |l| {
            l.views.len() == 1 && view_key(&l.views[0]).is_some()
        });
        view_key(&laid.views[0]).unwrap()
    }

    fn until(&mut self, what: &str, mut done: impl FnMut(&Laid) -> bool) -> Laid {
        if let Some(laid) = &self.laid
            && done(laid)
        {
            return laid.clone();
        }
        let patience = Patience::begun();
        loop {
            let next = self.owned.block_on(patience.awaited(what, self.heard.message()));
            let word = next.unwrap().expect("layout stream went on");
            if let Some(show_response::Event::Laid(laid)) = word.event {
                let matched = done(&laid);
                self.laid = Some(laid.clone());
                if matched {
                    return laid;
                }
            }
        }
    }
}

fn view_key(view: &show_response::View) -> Option<TerminalKey> {
    match &view.found {
        Some(Found::Key(key)) => Some(*key),
        _ => None,
    }
}

fn mux_key(key: TerminalKey) -> mux_api::TerminalKey {
    mux_api::TerminalKey { id: key.id, made_at: key.made_at }
}

fn open_worktree(owned: &Owned, dir: &Path) -> u64 {
    let dir_str = fs::canonicalize(dir).unwrap().to_str().unwrap().to_owned();
    let add = Change::AddProject(AddProject { dir: dir_str, name: None });
    let project_id = asked(&owned.host, add).unwrap().expect("expected project id");
    let host = owned.host.host();
    let Some(api::projects::Held::Kept(kept)) = host.projects.and_then(|p| p.held) else {
        panic!("no projects kept");
    };
    let project = kept.projects.iter().find(|p| p.id == project_id).unwrap();
    match project.holds.as_ref().unwrap() {
        api::project::Holds::Folder(wt) => wt.id,
        api::project::Holds::Clone(_) => panic!("expected folder project"),
    }
}

fn list_resume(owned: &Owned, terminal_id: Option<u32>) -> ListResumeResponse {
    let folder = folder_in(owned.host.runtime.path()).unwrap();
    owned.block_on(async move {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        daemon
            .list_resume(ListResumeRequest { on: None, terminal_id })
            .await
            .unwrap()
            .into_inner()
    })
}

fn reload_resume(owned: &Owned) -> ReloadResumeResponse {
    let folder = folder_in(owned.host.runtime.path()).unwrap();
    owned.block_on(async move {
        let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
        daemon.reload_resume(ReloadResumeRequest { on: None }).await.unwrap().into_inner()
    })
}

fn run_in_shell(owned: &Owned, key: TerminalKey, line: &str) {
    let socket = owned.socket();
    let target = mux_key(key);
    let line = line.to_owned();
    owned.block_on(async move {
        let channel = tonic::transport::Endpoint::from_static("http://lune-mux")
            .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                let socket = socket.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(socket).await?;
                    std::io::Result::Ok(hyper_util::rt::TokioIo::new(stream))
                }
            }))
            .await
            .unwrap();
        let mut client = mux_api::mux_client::MuxClient::new(channel);
        let enter = mux_api::Key {
            press: mux_api::Press::Down.into(),
            modifiers: None,
            key: Some(mux_api::key::Key::Named(mux_api::Named::Enter.into())),
            text: String::new(),
        };
        let words = vec![
            mux_api::FromViewer {
                event: Some(mux_api::from_viewer::Event::Hello(mux_api::Greeting::default())),
            },
            mux_api::FromViewer {
                event: Some(mux_api::from_viewer::Event::Showing(mux_api::Showing {
                    terminals: vec![target],
                })),
            },
            mux_api::FromViewer {
                event: Some(mux_api::from_viewer::Event::Typed(mux_api::Typed {
                    terminal: Some(target),
                    event: Some(mux_api::typed::Event::Paste(mux_api::Paste { text: line })),
                })),
            },
            mux_api::FromViewer {
                event: Some(mux_api::from_viewer::Event::Typed(mux_api::Typed {
                    terminal: Some(target),
                    event: Some(mux_api::typed::Event::Key(enter)),
                })),
            },
        ];
        let mut stream = client.show(tokio_stream::iter(words)).await.unwrap().into_inner();
        while stream.message().await.unwrap().is_some() {}
    });
}

fn send_ctrl_c(owned: &Owned, key: TerminalKey) {
    let socket = owned.socket();
    let target = mux_key(key);
    owned.block_on(async move {
        let channel = tonic::transport::Endpoint::from_static("http://lune-mux")
            .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                let socket = socket.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(socket).await?;
                    std::io::Result::Ok(hyper_util::rt::TokioIo::new(stream))
                }
            }))
            .await
            .unwrap();
        let mut client = mux_api::mux_client::MuxClient::new(channel);
        let ctrl_c = mux_api::Key {
            press: mux_api::Press::Down.into(),
            modifiers: Some(mux_api::Modifiers { ctrl: true, ..mux_api::Modifiers::default() }),
            key: Some(mux_api::key::Key::Character('c'.into())),
            text: String::new(),
        };
        let words = vec![
            mux_api::FromViewer {
                event: Some(mux_api::from_viewer::Event::Hello(mux_api::Greeting::default())),
            },
            mux_api::FromViewer {
                event: Some(mux_api::from_viewer::Event::Showing(mux_api::Showing {
                    terminals: vec![target],
                })),
            },
            mux_api::FromViewer {
                event: Some(mux_api::from_viewer::Event::Typed(mux_api::Typed {
                    terminal: Some(target),
                    event: Some(mux_api::typed::Event::Key(ctrl_c)),
                })),
            },
        ];
        let mut stream = client.show(tokio_stream::iter(words)).await.unwrap().into_inner();
        while stream.message().await.unwrap().is_some() {}
    });
}

fn write_config(owned: &Owned, toml: &str) {
    let path = owned.host.home.path().join(".config/lune/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, toml).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn logged(owned: &Owned) -> String {
    fs::read_to_string(owned.host.home.path().join(".local/state/lune/lune.log")).unwrap_or_default()
}

fn enable_esm(bin: &Path) {
    fs::write(bin.join("package.json"), r#"{"type":"module"}"#).unwrap();
}

fn kill_and_restore(owned: &Owned, shown: &mut Shown<'_>) -> Laid {
    let before: Vec<Option<TerminalKey>> =
        shown.until("current keys", |_| true).views.iter().map(view_key).collect();
    owned.kill_mux();
    shown.until("restored with fresh keys", |laid| {
        let now: Vec<Option<TerminalKey>> = laid.views.iter().map(view_key).collect();
        now.iter().all(Option::is_some) && now != before
    })
}

fn installed_node() -> PathBuf {
    let path = std::env::var_os("PATH").expect("tests run with PATH");
    std::env::split_paths(&path)
        .map(|dir| dir.join("node"))
        .find(|candidate| candidate.is_file())
        .expect("node must be on PATH")
}

fn loaded_config(response: &ListResumeResponse) -> &api::LoadedResumeConfig {
    match &response.config {
        Some(api::list_resume_response::Config::Loaded(loaded)) => loaded,
        other => panic!("expected loaded resume config, got {other:?}"),
    }
}

fn trusted_rule(record: &api::ResumeRecordInfo) -> Option<&str> {
    match &record.verdict {
        Some(api::resume_record_info::Verdict::TrustedRule(rule)) => Some(rule.as_str()),
        _ => None,
    }
}

fn is_saved(record: &api::ResumeRecordInfo) -> bool {
    matches!(record.persistence(), ResumePersistence::Saved | ResumePersistence::Tried)
}

#[test]
fn synthesized_claude_resumes_with_kept_flags_in_shell_and_returns_to_shell() {
    let owned = Owned::new();
    let work = owned.host.home.path().join("work");
    fs::create_dir_all(&work).unwrap();
    let work_canon = fs::canonicalize(&work).unwrap();

    let first_pid = owned.host.home.path().join("first_pid");
    let resumed_args = owned.host.home.path().join("resumed_args");
    let allow_exit = owned.host.home.path().join("allow_exit");
    let sessions = owned.host.home.path().join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();

    let node = installed_node();
    enable_esm(owned.host.bin());
    let fake_claude = owned.host.bin().join("claude");
    let claude_js = format!(
        "#!{node}\n\
         import fs from 'node:fs';\n\
         const args = process.argv.slice(2);\n\
         if (args[0] === '--resume') {{\n\
           fs.writeFileSync('{resumed}.tmp', args.join('\\0') + '\\0');\n\
           fs.renameSync('{resumed}.tmp', '{resumed}');\n\
           setInterval(() => {{\n\
             if (fs.existsSync('{allow_exit}')) process.exit(0);\n\
           }}, 25);\n\
         }} else {{\n\
           fs.writeFileSync('{pid}.tmp', String(process.pid));\n\
           fs.renameSync('{pid}.tmp', '{pid}');\n\
           setInterval(() => {{}}, 1000);\n\
         }}\n",
        node = node.display(),
        resumed = resumed_args.display(),
        allow_exit = allow_exit.display(),
        pid = first_pid.display(),
    );
    fs::write(&fake_claude, claude_js).unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    let wt = open_worktree(&owned, &work);
    let mut shown = Shown::new(&owned, wt);
    let key = shown.open_shell();

    run_in_shell(&owned, key, "claude --continue --model opus --verbose");
    let claude_pid: u32 = eventually("fake claude starts", || fs::read_to_string(&first_pid).ok())
        .parse()
        .unwrap();
    let mut session_file = file_of_pid(CLAUDE_UUID, claude_pid, "idle");
    session_file["cwd"] = serde_json::json!(work_canon.to_str().unwrap());
    session_file["entrypoint"] = serde_json::json!("cli");
    fs::write(sessions.join(format!("{claude_pid}.json")), session_file.to_string()).unwrap();

    let resumed_info = eventually("luned synthesizes and persists trusted claude record", || {
        let listed = list_resume(&owned, Some(key.id));
        let rec = listed.terminals.first()?.records.first()?;
        (rec.source() == ResumeRecordSource::Synthesized
            && trusted_rule(rec).is_some()
            && is_saved(rec))
            .then(|| rec.clone())
    });
    assert_eq!(trusted_rule(&resumed_info), Some("claude"));
    assert_eq!(
        resumed_info.argv,
        ["claude", "--resume", CLAUDE_UUID, "--model", "opus", "--verbose"]
    );
    assert_eq!(resumed_info.placement(), ResumePlacement::ShellHosted);

    let store_path = owned.host.home.path().join(".local/state/lune/resume/records.json");
    let store_mode = fs::metadata(&store_path).unwrap().permissions().mode() & 0o777;
    let dir_mode = fs::metadata(store_path.parent().unwrap()).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700);
    assert_eq!(store_mode, 0o600);

    let laid_after = kill_and_restore(&owned, &mut shown);
    let restored_key = view_key(&laid_after.views[0]).unwrap();

    let got_args = eventually("claude resumes with kept flags", || fs::read(&resumed_args).ok());
    let expected: Vec<u8> = ["--resume", CLAUDE_UUID, "--model", "opus", "--verbose"]
        .iter()
        .flat_map(|s| s.as_bytes().iter().copied().chain(std::iter::once(0)))
        .collect();
    assert_eq!(got_args, expected);

    let log = logged(&owned);
    assert!(log.contains("resuming saved program claude (rule claude)"), "{log}");
    assert!(!log.contains(CLAUDE_UUID), "session uuid must not leak into logs: {log}");
    assert!(!log.contains("opus"), "flag values must not leak into logs: {log}");

    fs::write(&allow_exit, "").unwrap();
    let back = owned.host.home.path().join("back_at_shell");
    run_in_shell(&owned, restored_key, &format!("echo SHELL_ALIVE > '{}'", back.display()));
    eventually("exiting resumed claude returns to interactive shell", || {
        fs::read_to_string(&back).ok().filter(|s| s.trim() == "SHELL_ALIVE")
    });
}

#[test]
fn pi_extension_emits_osc_7577_strips_token_resumes_and_clears_on_quit() {
    let owned = Owned::new();
    let pi_dir = owned.host.home.path().join(".pi/agent");
    fs::create_dir_all(&pi_dir).unwrap();
    let ext_path = pi_dir.join("extensions/lune.ts");
    eventually("luned installs pi extension", || ext_path.is_file().then_some(()));

    let work = owned.host.home.path().join("work");
    fs::create_dir_all(&work).unwrap();
    let work_canon = fs::canonicalize(&work).unwrap();
    let token_check = owned.host.home.path().join("pi_token_after_load");
    let resumed_args = owned.host.home.path().join("pi_resumed_args");
    let trigger_quit = owned.host.home.path().join("pi_trigger_quit");
    let quit_done = owned.host.home.path().join("pi_quit_done");

    let node = installed_node();
    enable_esm(owned.host.bin());
    let fake_pi = owned.host.bin().join("pi");
    let pi_js = format!(
        "#!{node}\n\
         import fs from 'node:fs';\n\
         import {{ pathToFileURL }} from 'node:url';\n\
         const args = process.argv.slice(2);\n\
         if (args[0] === '--session') {{\n\
           fs.writeFileSync('{resumed}.tmp', args.join('\\0'));\n\
           fs.renameSync('{resumed}.tmp', '{resumed}');\n\
         }}\n\
         const handlers = new Map();\n\
         const mod = await import(pathToFileURL('{ext}').href);\n\
         mod.default({{ on: (name, fn) => handlers.set(name, fn) }});\n\
         fs.writeFileSync('{tok}.tmp', String(process.env.OSC7577_TOKEN));\n\
         fs.renameSync('{tok}.tmp', '{tok}');\n\
         const ctx = {{ cwd: '{cwd}', mode: 'tui', sessionManager: {{ getSessionId: () => 'pi-sess-1' }} }};\n\
         handlers.get('session_start')?.({{ reason: 'startup' }}, ctx);\n\
         setInterval(() => {{\n\
           if (fs.existsSync('{quit}') && !fs.existsSync('{qdone}')) {{\n\
             handlers.get('session_shutdown')?.({{ reason: 'quit' }}, ctx);\n\
             fs.writeFileSync('{qdone}', 'ok');\n\
           }}\n\
         }}, 25);\n",
        node = node.display(),
        resumed = resumed_args.display(),
        ext = ext_path.display(),
        tok = token_check.display(),
        cwd = work_canon.display(),
        quit = trigger_quit.display(),
        qdone = quit_done.display(),
    );
    fs::write(&fake_pi, pi_js).unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();

    let wt = open_worktree(&owned, &work);
    let mut shown = Shown::new(&owned, wt);
    let key = shown.open_shell();

    run_in_shell(&owned, key, "pi --continue --model sonnet");

    let tok_after = eventually("pi extension loaded and checked token", || {
        fs::read_to_string(&token_check).ok()
    });
    assert_eq!(tok_after, "undefined", "pi extension must delete OSC7577_TOKEN from process.env");

    let info = eventually("luned receives signed OSC 7577 from pi extension", || {
        let listed = list_resume(&owned, Some(key.id));
        let rec = listed.terminals.first()?.records.first()?;
        (rec.source() == ResumeRecordSource::Osc7577
            && trusted_rule(rec).is_some()
            && is_saved(rec))
            .then(|| rec.clone())
    });
    assert_eq!(trusted_rule(&info), Some("pi"));
    assert_eq!(info.argv, ["pi", "--session", "pi-sess-1", "--model", "sonnet"]);

    let laid_after = kill_and_restore(&owned, &mut shown);
    let restored_key = view_key(&laid_after.views[0]).unwrap();

    let got = eventually("pi resumes with session and kept flags", || {
        fs::read_to_string(&resumed_args).ok()
    });
    assert_eq!(got, "--session\0pi-sess-1\0--model\0sonnet");

    eventually("resumed pi emits fresh OSC 7577", || {
        let listed = list_resume(&owned, Some(restored_key.id));
        let rec = listed.terminals.first()?.records.first()?;
        (rec.source() == ResumeRecordSource::Osc7577
            && trusted_rule(rec).is_some()
            && rec.persistence() != ResumePersistence::Tried)
            .then_some(())
    });

    fs::write(&trigger_quit, "").unwrap();
    eventually("pi emits clear=1 on quit and clears resume record", || {
        let listed = list_resume(&owned, Some(restored_key.id));
        listed.terminals.first().is_some_and(|t| t.records.is_empty()).then_some(())
    });
}

#[test]
fn custom_nvim_rule_resumes_across_full_restart_and_blocks_symlink_escape() {
    let mut owned = Owned::new();
    let sessions_dir = owned.host.home.path().join("sessions");
    let outside_dir = owned.host.home.path().join("outside");
    let work = owned.host.home.path().join("work");
    fs::create_dir_all(&sessions_dir).unwrap();
    fs::create_dir_all(&outside_dir).unwrap();
    fs::create_dir_all(&work).unwrap();
    let work_canon = fs::canonicalize(&work).unwrap();
    let legit_session = fs::canonicalize(&sessions_dir).unwrap().join("work.vim");
    fs::write(&legit_session, "session").unwrap();
    let outside_target = fs::canonicalize(&outside_dir).unwrap().join("secret.vim");
    fs::write(&outside_target, "secret").unwrap();
    let escaped_session = fs::canonicalize(&sessions_dir).unwrap().join("evil.vim");
    symlink(&outside_target, &escaped_session).unwrap();

    write_config(
        &owned,
        r#"
[resume.trust.nvim]
command = ["nvim", "-S", { under = "~/sessions" }]
"#,
    );
    let reloaded = reload_resume(&owned);
    assert!(matches!(
        reloaded.outcome,
        Some(api::reload_resume_response::Outcome::ReloadedRules(_))
    ));

    let resumed_out = owned.host.home.path().join("nvim_resumed");
    let node = installed_node();
    enable_esm(owned.host.bin());
    let fake_nvim = owned.host.bin().join("nvim");
    let nvim_js = format!(
        "#!{node}\n\
         import crypto from 'node:crypto';\n\
         import fs from 'node:fs';\n\
         const args = process.argv.slice(2);\n\
         const token = process.env.OSC7577_TOKEN;\n\
         delete process.env.OSC7577_TOKEN;\n\
         if (args[0] === '-S') {{\n\
           fs.writeFileSync('{out}.tmp', args.join(' '));\n\
           fs.renameSync('{out}.tmp', '{out}');\n\
         }} else if (args[0] === '--emit' && token) {{\n\
           const session = args[1];\n\
           const cwd = args[2];\n\
           const argv = ['nvim', '-S', session].map((w) => Buffer.from(w, 'utf8').toString('base64')).join(',');\n\
           const cwdB64 = Buffer.from(cwd, 'utf8').toString('base64');\n\
           const body = `pid=${{process.pid}}:time=${{Math.floor(Date.now() / 1000)}}:argv=${{argv}}:cwd=${{cwdB64}}`;\n\
           const mac = crypto.createHash('sha256').update(`${{body}}:${{token}}`).digest('hex');\n\
           fs.appendFileSync('/dev/tty', `\\x1b]7577;${{body}}:mac=${{mac}}\\x1b\\\\`);\n\
         }}\n\
         setInterval(() => {{}}, 1000);\n",
        node = node.display(),
        out = resumed_out.display(),
    );
    fs::write(&fake_nvim, nvim_js).unwrap();
    fs::set_permissions(&fake_nvim, fs::Permissions::from_mode(0o755)).unwrap();

    let wt = open_worktree(&owned, &work);
    let mut shown = Shown::new(&owned, wt);
    let key = shown.open_shell();

    run_in_shell(
        &owned,
        key,
        &format!("nvim --emit '{}' '{}'", legit_session.display(), work_canon.display()),
    );
    eventually("nvim record saved", || {
        let listed = list_resume(&owned, Some(key.id));
        let rec = listed.terminals.first()?.records.first()?;
        (is_saved(rec) && trusted_rule(rec) == Some("nvim")).then_some(())
    });

    drop(shown);
    owned.stop_daemon();
    owned.kill_mux();
    owned.start_daemon();
    let mut shown2 = Shown::new(&owned, wt);
    let laid2 = shown2.until("terminal restored after reboot", |l| {
        l.views.len() == 1 && view_key(&l.views[0]).is_some()
    });
    let got = eventually("nvim resumed after full daemon+mux restart", || {
        fs::read_to_string(&resumed_out).ok()
    });
    assert_eq!(got, format!("-S {}", legit_session.display()));

    let _ = fs::remove_file(&resumed_out);
    let key2 = view_key(&laid2.views[0]).unwrap();
    send_ctrl_c(&owned, key2);
    run_in_shell(
        &owned,
        key2,
        &format!("nvim --emit '{}' '{}'", escaped_session.display(), work_canon.display()),
    );
    eventually("symlink-escaped nvim record saved", || {
        let listed = list_resume(&owned, Some(key2.id));
        let rec = listed.terminals.first()?.records.first()?;
        (is_saved(rec) && rec.argv.last().map(String::as_str) == escaped_session.to_str())
            .then_some(())
    });

    let _laid3 = kill_and_restore(&owned, &mut shown2);
    eventually("symlink escape is blocked on restore", || {
        logged(&owned).contains("not resuming saved program nvim, starting a shell (reason=not_trusted)").then_some(())
    });
    assert!(!resumed_out.exists(), "escaped symlink session must not resume");
}

#[test]
fn missing_explicit_cwd_skips_resume_and_tried_prevents_retry_after_failure() {
    let owned = Owned::new();
    let work = owned.host.home.path().join("work");
    let ephemeral = owned.host.home.path().join("ephemeral");
    let sessions_dir = owned.host.home.path().join("sessions");
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(&ephemeral).unwrap();
    fs::create_dir_all(&sessions_dir).unwrap();
    let work_canon = fs::canonicalize(&work).unwrap();
    let eph_canon = fs::canonicalize(&ephemeral).unwrap();
    let sess_file = fs::canonicalize(&sessions_dir).unwrap().join("s.vim");
    fs::write(&sess_file, "s").unwrap();

    write_config(
        &owned,
        r#"
[resume.trust.nvim]
command = ["nvim", "-S", { under = "~/sessions" }]
"#,
    );
    assert!(matches!(
        reload_resume(&owned).outcome,
        Some(api::reload_resume_response::Outcome::ReloadedRules(_))
    ));

    let runs_file = owned.host.home.path().join("nvim_resume_count");
    let node = installed_node();
    enable_esm(owned.host.bin());
    let fake_nvim = owned.host.bin().join("nvim");
    let nvim_js = format!(
        "#!{node}\n\
         import crypto from 'node:crypto';\n\
         import fs from 'node:fs';\n\
         const args = process.argv.slice(2);\n\
         const token = process.env.OSC7577_TOKEN;\n\
         delete process.env.OSC7577_TOKEN;\n\
         if (args[0] === '-S') {{\n\
           fs.appendFileSync('{runs}', 'x\\n');\n\
           process.exit(1);\n\
         }} else if (args[0] === '--emit' && token) {{\n\
           const session = args[1];\n\
           const cwd = args[2];\n\
           const argv = ['nvim', '-S', session].map((w) => Buffer.from(w, 'utf8').toString('base64')).join(',');\n\
           const cwdB64 = Buffer.from(cwd, 'utf8').toString('base64');\n\
           const body = `pid=${{process.pid}}:time=${{Math.floor(Date.now() / 1000)}}:argv=${{argv}}:cwd=${{cwdB64}}`;\n\
           const mac = crypto.createHash('sha256').update(`${{body}}:${{token}}`).digest('hex');\n\
           fs.appendFileSync('/dev/tty', `\\x1b]7577;${{body}}:mac=${{mac}}\\x1b\\\\`);\n\
         }}\n\
         setInterval(() => {{}}, 1000);\n",
        node = node.display(),
        runs = runs_file.display(),
    );
    fs::write(&fake_nvim, nvim_js).unwrap();
    fs::set_permissions(&fake_nvim, fs::Permissions::from_mode(0o755)).unwrap();

    let wt = open_worktree(&owned, &work);
    let mut shown = Shown::new(&owned, wt);
    let key = shown.open_shell();

    run_in_shell(
        &owned,
        key,
        &format!("nvim --emit '{}' '{}'", sess_file.display(), eph_canon.display()),
    );
    eventually("ephemeral-cwd record saved", || {
        let listed = list_resume(&owned, Some(key.id));
        listed.terminals.first()?.records.first().filter(|r| is_saved(r)).map(|_| ())
    });

    fs::remove_dir_all(&ephemeral).unwrap();
    let laid2 = kill_and_restore(&owned, &mut shown);
    eventually("missing explicit cwd skipped", || {
        logged(&owned).contains("reason=dir_gone").then_some(())
    });
    assert!(!runs_file.exists(), "missing explicit cwd must not spawn program");

    let key2 = view_key(&laid2.views[0]).unwrap();
    run_in_shell(
        &owned,
        key2,
        &format!("nvim --emit '{}' '{}'", sess_file.display(), work_canon.display()),
    );
    eventually("failing-resume record saved", || {
        let listed = list_resume(&owned, Some(key2.id));
        listed
            .terminals
            .first()?
            .records
            .first()
            .filter(|r| r.persistence() == ResumePersistence::Saved)
            .map(|_| ())
    });

    let _laid3 = kill_and_restore(&owned, &mut shown);
    eventually("first resume ran once and failed", || {
        fs::read_to_string(&runs_file).ok().filter(|s| s.lines().count() == 1)
    });

    let _laid4 = kill_and_restore(&owned, &mut shown);
    eventually("second restore skips because already_tried", || {
        logged(&owned).contains("reason=already_tried").then_some(())
    });
    let count = fs::read_to_string(&runs_file).unwrap().lines().count();
    assert_eq!(count, 1, "failed resume must never retry automatically");
}

#[test]
fn reload_resume_keeps_previous_rules_on_broken_config_and_cli_reports_rules() {
    let owned = Owned::new();

    let initial = list_resume(&owned, None);
    let names: Vec<&str> =
        loaded_config(&initial).rules.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["agy", "claude", "codex", "opencode", "pi"]);

    let zero_status = {
        let folder = folder_in(owned.host.runtime.path()).unwrap();
        owned.block_on(async move {
            let mut daemon = luned::connect(&folder, PATIENT).await.unwrap();
            daemon
                .list_resume(ListResumeRequest { on: None, terminal_id: Some(0) })
                .await
                .unwrap_err()
        })
    };
    assert_eq!(zero_status.code(), tonic::Code::InvalidArgument);

    write_config(
        &owned,
        r#"
[resume.trust.codex]
enabled = false

[resume.trust.htop]
command = ["htop"]
"#,
    );
    let ok = reload_resume(&owned);
    assert_eq!(ok.outcome, Some(api::reload_resume_response::Outcome::ReloadedRules(5)));

    let after = list_resume(&owned, None);
    let after_cfg = loaded_config(&after);
    assert!(after_cfg.rules.iter().any(|r| r.name == "htop" && !r.builtin));
    assert!(after_cfg.disabled_rules.iter().any(|r| r.name == "codex"));

    let cli_rules = owned.host.vars().command(&LUNE).args(["resume", "--rules"]).output().unwrap();
    assert!(cli_rules.status.success(), "{}", String::from_utf8_lossy(&cli_rules.stderr));
    let rules_stdout = String::from_utf8_lossy(&cli_rules.stdout);
    assert!(rules_stdout.contains("htop [custom]: htop"), "{rules_stdout}");
    assert!(rules_stdout.contains("codex: disabled in config"), "{rules_stdout}");

    let cli_live = owned.host.vars().command(&LUNE).arg("resume").output().unwrap();
    assert!(cli_live.status.success(), "{}", String::from_utf8_lossy(&cli_live.stderr));

    write_config(&owned, "[resume\ndefaults = true");
    let bad = reload_resume(&owned);
    let Some(api::reload_resume_response::Outcome::Failed(failed)) = bad.outcome else {
        panic!("expected failed reload outcome, got {:?}", bad.outcome);
    };
    assert_ne!(failed.error, "");
    assert_eq!(failed.kept_previous_rules, Some(5));

    let still = list_resume(&owned, None);
    assert!(loaded_config(&still).rules.iter().any(|r| r.name == "htop"));

    let cli_bad = owned.host.vars().command(&LUNE).args(["resume", "--rules"]).output().unwrap();
    assert!(!cli_bad.status.success(), "broken config should fail --rules validation");
}

#[test]
fn unlisted_flags_do_not_bypass_via_legacy_agent_and_pi_never_replays_prompt() {
    let owned = Owned::new();
    let pi_dir = owned.host.home.path().join(".pi/agent");
    fs::create_dir_all(&pi_dir).unwrap();
    let ext_path = pi_dir.join("extensions/lune.ts");
    eventually("luned installs pi extension", || ext_path.is_file().then_some(()));

    let work = owned.host.home.path().join("work");
    fs::create_dir_all(&work).unwrap();
    let work_canon = fs::canonicalize(&work).unwrap();

    let claude_pid_file = owned.host.home.path().join("claude_restricted_pid");
    let claude_resumed = owned.host.home.path().join("claude_restricted_resumed");
    let sessions = owned.host.home.path().join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let node = installed_node();
    enable_esm(owned.host.bin());
    let fake_claude = owned.host.bin().join("claude");
    let claude_js = format!(
        "#!{node}\n\
         import fs from 'node:fs';\n\
         const args = process.argv.slice(2);\n\
         if (args[0] === '--resume') {{\n\
           fs.writeFileSync('{resumed}.tmp', args.join('\\0') + '\\0');\n\
           fs.renameSync('{resumed}.tmp', '{resumed}');\n\
         }} else {{\n\
           fs.writeFileSync('{pid}.tmp', String(process.pid));\n\
           fs.renameSync('{pid}.tmp', '{pid}');\n\
         }}\n\
         setInterval(() => {{}}, 1000);\n",
        node = node.display(),
        resumed = claude_resumed.display(),
        pid = claude_pid_file.display(),
    );
    fs::write(&fake_claude, claude_js).unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    let wt = open_worktree(&owned, &work);
    let mut shown = Shown::new(&owned, wt);
    let key = shown.open_shell();

    run_in_shell(&owned, key, "claude --restricted");
    let claude_pid: u32 =
        eventually("restricted claude starts", || fs::read_to_string(&claude_pid_file).ok())
            .parse()
            .unwrap();
    let mut session_file = file_of_pid(CLAUDE_UUID, claude_pid, "idle");
    session_file["cwd"] = serde_json::json!(work_canon.to_str().unwrap());
    session_file["entrypoint"] = serde_json::json!("cli");
    fs::write(sessions.join(format!("{claude_pid}.json")), session_file.to_string()).unwrap();

    eventually("luned sees restricted claude on host without saving resume record", || {
        let seen = owned.host.host().agents.iter().any(|a| a.pid == claude_pid);
        let listed = list_resume(&owned, Some(key.id));
        let empty = listed.terminals.first().is_some_and(|t| t.records.is_empty());
        (seen && empty).then_some(())
    });

    let laid_after_claude = kill_and_restore(&owned, &mut shown);
    let key2 = view_key(&laid_after_claude.views[0]).unwrap();
    eventually("restricted claude is typed at prompt rather than executed", || {
        owned.dumped(mux_key(key2)).contains("claude --restricted").then_some(())
    });
    let projects_file = owned.host.home.path().join(".local/state/lune/projects.json");
    let projects_text = fs::read_to_string(&projects_file).unwrap_or_default();
    assert!(!projects_text.contains(CLAUDE_UUID), "untrusted agent must not be written as Runs::Agent: {projects_text}");
    assert!(!claude_resumed.exists(), "claude --restricted must not auto-resume via legacy fallback");

    let work_pi = owned.host.home.path().join("work_pi");
    fs::create_dir_all(&work_pi).unwrap();
    let work_pi_canon = fs::canonicalize(&work_pi).unwrap();
    let pi_resumed = owned.host.home.path().join("pi_prompt_resumed");
    let fake_pi = owned.host.bin().join("pi");
    let pi_js = format!(
        "#!{node}\n\
         import fs from 'node:fs';\n\
         import {{ pathToFileURL }} from 'node:url';\n\
         const args = process.argv.slice(2);\n\
         if (args[0] === '--session') {{\n\
           fs.writeFileSync('{resumed}.tmp', args.join('\\0'));\n\
           fs.renameSync('{resumed}.tmp', '{resumed}');\n\
         }}\n\
         const handlers = new Map();\n\
         const mod = await import(pathToFileURL('{ext}').href);\n\
         mod.default({{ on: (name, fn) => handlers.set(name, fn) }});\n\
         const ctx = {{ cwd: '{cwd}', mode: 'tui', sessionManager: {{ getSessionId: () => 'pi-sess-2' }} }};\n\
         handlers.get('session_start')?.({{ reason: 'startup' }}, ctx);\n\
         setInterval(() => {{}}, 1000);\n",
        node = node.display(),
        resumed = pi_resumed.display(),
        ext = ext_path.display(),
        cwd = work_pi_canon.display(),
    );
    fs::write(&fake_pi, pi_js).unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();

    let wt_pi = open_worktree(&owned, &work_pi);
    let mut shown_pi = Shown::new(&owned, wt_pi);
    let pi_key = shown_pi.open_shell();
    run_in_shell(&owned, pi_key, "pi -e ./ext.ts 'NEVER_REPLAY_PROMPT'");

    let info = eventually("pi extension emits resume without swallowing prompt", || {
        let listed = list_resume(&owned, Some(pi_key.id));
        let rec = listed.terminals.first()?.records.first()?;
        (rec.source() == ResumeRecordSource::Osc7577
            && trusted_rule(rec) == Some("pi")
            && is_saved(rec))
            .then(|| rec.clone())
    });
    assert_eq!(info.argv, ["pi", "--session", "pi-sess-2", "-e", "./ext.ts"]);

    let _laid3 = kill_and_restore(&owned, &mut shown_pi);
    let got = eventually("pi resumes without replaying initial prompt", || {
        fs::read_to_string(&pi_resumed).ok()
    });
    assert_eq!(got, "--session\0pi-sess-2\0-e\0./ext.ts");
}

#[test]
fn custom_absolute_claude_override_without_flags_synthesizes_and_resumes() {
    let owned = Owned::new();
    let work = owned.host.home.path().join("work");
    fs::create_dir_all(&work).unwrap();
    let work_canon = fs::canonicalize(&work).unwrap();

    let first_pid = owned.host.home.path().join("custom_claude_pid");
    let resumed_args = owned.host.home.path().join("custom_claude_resumed");
    let sessions = owned.host.home.path().join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();

    let node = installed_node();
    enable_esm(owned.host.bin());
    let fake_claude = owned.host.bin().join("claude");
    let claude_js = format!(
        "#!{node}\n\
         import fs from 'node:fs';\n\
         const args = process.argv.slice(2);\n\
         if (args[0] === '--resume') {{\n\
           fs.writeFileSync('{resumed}.tmp', args.join('\\0') + '\\0');\n\
           fs.renameSync('{resumed}.tmp', '{resumed}');\n\
         }} else {{\n\
           fs.writeFileSync('{pid}.tmp', String(process.pid));\n\
           fs.renameSync('{pid}.tmp', '{pid}');\n\
         }}\n\
         setInterval(() => {{}}, 1000);\n",
        node = node.display(),
        resumed = resumed_args.display(),
        pid = first_pid.display(),
    );
    fs::write(&fake_claude, claude_js).unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();
    let claude_abs = fs::canonicalize(&fake_claude).unwrap();
    let config_toml = format!(
        "[resume.trust.claude]\ncommand = [{:?}, \"--resume\", {{ match = \"uuid\" }}]\n",
        claude_abs.to_str().unwrap(),
    );
    write_config(&owned, &config_toml);
    assert!(matches!(
        reload_resume(&owned).outcome,
        Some(api::reload_resume_response::Outcome::ReloadedRules(_))
    ));

    let wt = open_worktree(&owned, &work);
    let mut shown = Shown::new(&owned, wt);
    let key = shown.open_shell();

    run_in_shell(&owned, key, &format!("'{}' --model opus", claude_abs.display()));
    let claude_pid: u32 = eventually("custom claude starts", || fs::read_to_string(&first_pid).ok())
        .parse()
        .unwrap();
    let mut session_file = file_of_pid(CLAUDE_UUID, claude_pid, "idle");
    session_file["cwd"] = serde_json::json!(work_canon.to_str().unwrap());
    session_file["entrypoint"] = serde_json::json!("cli");
    fs::write(sessions.join(format!("{claude_pid}.json")), session_file.to_string()).unwrap();

    let info = eventually("synthesizer respects custom absolute path and omitted flags", || {
        let listed = list_resume(&owned, Some(key.id));
        let rec = listed.terminals.first()?.records.first()?;
        (rec.source() == ResumeRecordSource::Synthesized
            && trusted_rule(rec) == Some("claude")
            && is_saved(rec))
            .then(|| rec.clone())
    });
    assert_eq!(info.argv, [claude_abs.to_str().unwrap(), "--resume", CLAUDE_UUID]);

    let _laid2 = kill_and_restore(&owned, &mut shown);
    let got = eventually("custom absolute claude resumes without leaked default flags", || {
        fs::read(&resumed_args).ok()
    });
    let expected: Vec<u8> = ["--resume", CLAUDE_UUID]
        .iter()
        .flat_map(|s| s.as_bytes().iter().copied().chain(std::iter::once(0)))
        .collect();
    assert_eq!(got, expected);
}
