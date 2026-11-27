use std::fs;
use std::io::{BufRead as _, BufReader};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use hyper_util::rt::TokioIo;
use lune_mux::api::{self, mux_client::MuxClient};
use lune_protocol::program_resume::{encode_base64_standard, sha256_body_token_hex};
use lune_waits::{Patience, eventually_async};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use crate::host::{LUNE_MUX, Server, Viewer, new_terminal, run, script, size, watched_until};

fn sign_osc7577(body_without_mac: &str, token: &str) -> String {
    let mac = sha256_body_token_hex(body_without_mac.as_bytes(), token.as_bytes());
    let mac = std::str::from_utf8(&mac).unwrap();
    format!("\x1b]7577;{body_without_mac}:mac={mac}\x1b\\")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn encode_argv(words: &[&str]) -> String {
    words
        .iter()
        .map(|w| encode_base64_standard(w.as_bytes()))
        .collect::<Vec<_>>()
        .join(",")
}

fn resumes_of(listing: &api::Listing, key: api::TerminalKey) -> Option<&api::TerminalResumes> {
    let terminal = listing.terminals.iter().find(|t| t.key == Some(key))?;
    match terminal.program.as_ref()? {
        api::terminal::Program::Runs(running) => running.resumes.as_ref(),
        api::terminal::Program::Ended(_) => None,
    }
}

fn chosen_of(listing: &api::Listing, key: api::TerminalKey) -> Option<&api::ResumeRecord> {
    resumes_of(listing, key)?.chosen.as_ref()
}

fn make_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

struct OwnedChild(std::process::Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

async fn start_server_with_env(
    envs: &[(&str, String)],
) -> (OwnedChild, tempfile::TempDir, MuxClient<Channel>) {
    let runtime = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(LUNE_MUX);
    cmd.arg(runtime.path().join("lune"))
        .env_remove("LUNE_MUX_TERMINAL")
        .env_remove("LUNE_MUX_TERMINAL_MADE")
        .stdin(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let child = OwnedChild(child);
    let (said, mut heard) = mpsc::unbounded_channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            eprintln!("{line}");
            let _ = said.send(line);
        }
    });
    Patience::begun()
        .awaited_first(
            "lune-mux listens",
            async || heard.recv().await,
            |line: Option<String>| {
                line.expect("lune-mux stderr ended")
                    .starts_with("lune-mux: listening on ")
                    .then_some(())
            },
        )
        .await;
    let socket = runtime.path().join("lune/mux.sock");
    let channel = Endpoint::from_static("http://lune-mux")
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move { UnixStream::connect(socket).await.map(TokioIo::new) }
        }))
        .await
        .unwrap();
    let client =
        MuxClient::new(channel).max_decoding_message_size(lune_protocol::RPC_MESSAGE_BYTES_MAX);
    (child, runtime, client)
}

#[test]
fn osc_7577_token_is_unique_per_terminal_and_strips_inherited_token() {
    run(async {
        let inherited = "inherited-outer-token-0123456789abcdef";
        let (_child, _runtime, mut mux) =
            start_server_with_env(&[("OSC7577_TOKEN", inherited.to_owned())]).await;
        let scratch = tempfile::tempdir().unwrap();
        let out_a = scratch.path().join("tok_a");
        let out_b = scratch.path().join("tok_b");

        let dump = |path: &Path| {
            script(
                &format!(
                    "printf '%s' \"$OSC7577_TOKEN\" > '{0}.tmp' && mv '{0}.tmp' '{0}'\nsleep 60",
                    path.display()
                ),
                api::Ends::Closes,
            )
        };
        let _a = new_terminal(&mut mux, dump(&out_a), b"a", size(80, 24)).await;
        let _b = new_terminal(&mut mux, dump(&out_b), b"b", size(80, 24)).await;

        let tok_a = eventually_async("terminal A writes token", async || {
            fs::read_to_string(&out_a).ok()
        })
        .await;
        let tok_b = eventually_async("terminal B writes token", async || {
            fs::read_to_string(&out_b).ok()
        })
        .await;

        assert_ne!(tok_a, inherited);
        assert_ne!(tok_b, inherited);
        assert_ne!(tok_a, tok_b);
        assert!(
            lune_protocol::program_resume::is_valid_token(&tok_a),
            "{tok_a:?}"
        );
        assert!(
            lune_protocol::program_resume::is_valid_token(&tok_b),
            "{tok_b:?}"
        );
    });
}

#[test]
fn osc_7577_verifies_mac_persists_across_resets_and_clears_on_reject_clear_or_exit() {
    run(async {
        let server = Server::start().await;
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let scratch = tempfile::tempdir().unwrap();
        let info = scratch.path().join("info");
        let seq_dir = scratch.path().join("seq");
        fs::create_dir(&seq_dir).unwrap();
        let child_info = scratch.path().join("child_info");
        let child_go = scratch.path().join("child_go");
        let child_exit = scratch.path().join("child_exit");

        let body = format!(
            "printf '%s:%s' \"$$\" \"$OSC7577_TOKEN\" > '{0}.tmp' && mv '{0}.tmp' '{0}'\n\
             for step in 1 2 3 4 5 6 7; do\n\
               while [ ! -f '{1}/'\"$step\" ]; do sleep 0.02; done\n\
               cat '{1}/'\"$step\"\n\
             done\n\
             while [ ! -f '{2}' ]; do sleep 0.02; done\n\
             sh -c 'printf \"%s\" \"$$\" > \"{3}.tmp\" && mv \"{3}.tmp\" \"{3}\"\n\
               while [ ! -f \"{1}/child\" ]; do sleep 0.02; done\n\
               cat \"{1}/child\"\n\
               while [ ! -f \"{4}\" ]; do sleep 0.02; done'\n\
             sleep 60\n",
            info.display(),
            seq_dir.display(),
            child_go.display(),
            child_info.display(),
            child_exit.display(),
        );
        let key = new_terminal(
            &mut mux,
            script(&body, api::Ends::Holds),
            b"r",
            size(80, 24),
        )
        .await;

        let info_text = eventually_async("script reports pid and token", async || {
            fs::read_to_string(&info).ok()
        })
        .await;
        let (pid_str, token) = info_text.split_once(':').unwrap();
        let pid: u32 = pid_str.parse().unwrap();

        let write_step = |name: &str, content: &str| {
            let tmp = seq_dir.join(format!(".{name}.tmp"));
            fs::write(&tmp, content).unwrap();
            fs::rename(&tmp, seq_dir.join(name)).unwrap();
        };

        let argv_b64 = encode_argv(&["nvim", "-S", "/tmp/a.vim"]);
        let cwd_b64 = encode_base64_standard(b"/tmp");
        let valid_body = format!(
            "pid={pid}:time={}:argv={argv_b64}:cwd={cwd_b64}",
            unix_now()
        );
        write_step("1", &sign_osc7577(&valid_body, token));

        let listing = watched_until(&mut watch, "valid osc 7577 record appears", |l| {
            chosen_of(l, key).is_some()
        })
        .await;
        let rec = chosen_of(&listing, key).unwrap();
        assert_eq!(rec.pid, pid);
        assert_eq!(rec.argv, ["nvim", "-S", "/tmp/a.vim"]);
        assert_eq!(
            rec.cwd,
            Some(api::resume_record::Cwd::ExplicitCwd("/tmp".to_owned()))
        );
        assert_eq!(rec.placement(), api::ProcessPlacement::Root);

        let resets = "\x1bc\x1b[!p\x1b[?1049h\x1b[?1049l\x1b]133;A\x07\x1b]133;C\x07";
        write_step("2", resets);

        let bad_mac = format!(
            "\x1b]7577;pid={pid}:time={}:clear=1:mac={}\x1b\\",
            unix_now(),
            "0".repeat(64)
        );
        write_step("3", &bad_mac);

        let stale_body = format!("pid={pid}:time=1000:clear=1");
        write_step("4", &sign_osc7577(&stale_body, token));

        let listing_after = mux.list(api::ListRequest {}).await.unwrap().into_inner();
        assert_eq!(
            chosen_of(&listing_after, key).unwrap().argv,
            ["nvim", "-S", "/tmp/a.vim"]
        );

        let bad_payload = format!("pid={pid}:time={}:argv=!!!", unix_now());
        write_step("5", &sign_osc7577(&bad_payload, token));
        watched_until(&mut watch, "reject clears owner record", |l| {
            resumes_of(l, key).is_none()
        })
        .await;

        let valid_body2 = format!(
            "pid={pid}:time={}:argv={argv_b64}:cwd={cwd_b64}",
            unix_now()
        );
        write_step("6", &sign_osc7577(&valid_body2, token));
        watched_until(&mut watch, "record restored after reject", |l| {
            chosen_of(l, key).is_some()
        })
        .await;

        let clear_body = format!("pid={pid}:time={}:clear=1", unix_now());
        write_step("7", &sign_osc7577(&clear_body, token));
        watched_until(&mut watch, "explicit clear removes record", |l| {
            resumes_of(l, key).is_none()
        })
        .await;

        fs::write(&child_go, "").unwrap();
        let child_pid: u32 = eventually_async("child writes pid", async || {
            fs::read_to_string(&child_info).ok()
        })
        .await
        .parse()
        .unwrap();
        let child_body = format!(
            "pid={child_pid}:time={}:argv={}",
            unix_now(),
            encode_argv(&["nvim", "-S", "/tmp/b.vim"])
        );
        write_step("child", &sign_osc7577(&child_body, token));

        let child_listing = watched_until(&mut watch, "descendant record appears", |l| {
            chosen_of(l, key).is_some_and(|r| r.pid == child_pid)
        })
        .await;
        assert_ne!(
            chosen_of(&child_listing, key).unwrap().placement(),
            api::ProcessPlacement::Root
        );

        fs::write(&child_exit, "").unwrap();
        watched_until(
            &mut watch,
            "exited descendant reaped after grace window",
            |l| resumes_of(l, key).is_none(),
        )
        .await;
    });
}

#[test]
fn shell_hosted_resume_runs_hostile_argv_verbatim_bypasses_builtins_and_returns_to_shell() {
    run(async {
        let scratch = tempfile::tempdir().unwrap();
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let pwned = scratch.path().join("pwned");

        let eval_out = scratch.path().join("eval_out");
        let eval_bin = bin.join("eval");
        make_executable(
            &eval_bin,
            &format!(
                "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{0}.tmp' && mv '{0}.tmp' '{0}'\n",
                eval_out.display()
            ),
        );

        let path_env = {
            let existing = std::env::var("PATH").unwrap_or_default();
            format!("{}:{existing}", bin.display())
        };
        fs::write(
            scratch.path().join(".bashrc"),
            format!("PS1='{}'\n", lune_waits::shell::PROMPT),
        )
        .unwrap();
        let (_child, runtime, mut mux) = start_server_with_env(&[
            ("PATH", path_env),
            ("HOME", scratch.path().to_str().unwrap().to_owned()),
            ("PS1", lune_waits::shell::PROMPT.to_owned()),
        ])
        .await;
        let _keeper = new_terminal(
            &mut mux,
            script("sleep 60", api::Ends::Closes),
            b"keep",
            size(80, 24),
        )
        .await;

        let mut shells_to_test: Vec<String> = Vec::new();
        for candidate in ["sh", "bash", "fish"] {
            if let Some(found) = find_on_path(candidate) {
                let p = found.to_str().unwrap().to_owned();
                if !shells_to_test.contains(&p) {
                    shells_to_test.push(p);
                }
            }
        }
        if shells_to_test.is_empty() && Path::new("/bin/sh").is_file() {
            shells_to_test.push("/bin/sh".to_owned());
        }
        assert!(
            !shells_to_test.is_empty(),
            "at least one supported shell must exist"
        );

        for (index, shell_path) in shells_to_test.iter().enumerate() {
            let record_out = scratch.path().join(format!("args_{index}"));
            let fg_release = scratch.path().join(format!("release_{index}"));
            let recorder = bin.join(format!("rec_{index}"));
            make_executable(
                &recorder,
                &format!(
                    "#!/bin/sh\n\
                     printf '%s\\0' \"$@\" > '{0}.tmp' && mv '{0}.tmp' '{0}'\n\
                     printf 'resumed\\n'\n\
                     while [ ! -f '{1}' ]; do sleep 0.02; done\n",
                    record_out.display(),
                    fg_release.display(),
                ),
            );

            let hostile_args = vec![
                recorder.to_str().unwrap().to_owned(),
                "arg with spaces".to_owned(),
                format!("$(touch {})", pwned.display()),
                "a; b | c & d".to_owned(),
                "it's \"quoted\"".to_owned(),
                String::new(),
                "--flag=val".to_owned(),
            ];
            let opens = api::Opens {
                cwd: scratch.path().to_str().unwrap().to_owned(),
                program: None,
                ends: api::Ends::Unspecified.into(),
                resume_in_shell: Some(api::opens::ResumeInShell {
                    shell: shell_path.clone(),
                    argv: Some(api::Program {
                        argv: hostile_args.clone(),
                    }),
                }),
            };
            let key = new_terminal(&mut mux, opens, b"h", size(80, 24)).await;

            let raw = eventually_async(&format!("{shell_path} runs resumed program"), async || {
                fs::read(&record_out).ok()
            })
            .await;
            let expected: Vec<u8> = hostile_args[1..]
                .iter()
                .flat_map(|a| a.as_bytes().iter().copied().chain(std::iter::once(0)))
                .collect();
            assert_eq!(raw, expected, "shell {shell_path} mangled argv");
            assert!(
                !pwned.exists(),
                "shell {shell_path} expanded command substitution"
            );

            eventually_async(
                &format!("{shell_path} gives resumed child its own foreground process group"),
                async || {
                    let listing = mux.list(api::ListRequest {}).await.ok()?.into_inner();
                    let term = listing.terminals.iter().find(|t| t.key == Some(key))?;
                    let api::terminal::Program::Runs(runs) = term.program.as_ref()? else {
                        return None;
                    };
                    let fg = runs.foreground.as_ref()?;
                    (fg.pid != runs.pid).then_some(())
                },
            )
            .await;
            fs::write(&fg_release, "").unwrap();

            let mut viewer = Viewer::new(&mut mux).await;
            viewer.show(&[key]).await;
            let marker = format!("BACK_IN_SHELL_{index}");
            if Path::new(shell_path).file_name().and_then(|n| n.to_str()) == Some("fish") {
                viewer
                    .until("fish prompt shown", |v| {
                        v.rows_of(key.id)
                            .iter()
                            .any(|row| !row.trim().is_empty() && row.trim() != "resumed")
                    })
                    .await;
                let ping = scratch.path().join(format!("shell_ok_{index}"));
                viewer
                    .send(api::from_viewer::Event::Typed(api::Typed {
                        terminal: Some(key),
                        event: Some(api::typed::Event::Paste(api::Paste {
                            text: format!("echo {marker} > '{}'", ping.display()),
                        })),
                    }))
                    .await;
                viewer.press_enter(key).await;
                eventually_async("fish returned to interactive prompt", async || {
                    fs::read_to_string(&ping)
                        .ok()
                        .filter(|s| s.trim() == marker)
                })
                .await;
            } else {
                let prompted = viewer.until_prompted(key.id).await;
                let typed = viewer
                    .type_at(key, &prompted, &format!("echo {marker}"))
                    .await;
                let said = viewer.until_said(key.id, &typed).await;
                assert_eq!(
                    said,
                    [marker],
                    "{shell_path} did not return to interactive prompt"
                );
            }
            let _ = mux
                .close_terminal(api::CloseTerminalRequest {
                    terminal: Some(key),
                })
                .await;
            drop(viewer);
        }

        let eval_opens = api::Opens {
            cwd: scratch.path().to_str().unwrap().to_owned(),
            program: None,
            ends: api::Ends::Unspecified.into(),
            resume_in_shell: Some(api::opens::ResumeInShell {
                shell: shells_to_test[0].clone(),
                argv: Some(api::Program {
                    argv: vec!["eval".to_owned(), format!("touch {}", pwned.display())],
                }),
            }),
        };
        let eval_key = new_terminal(&mut mux, eval_opens, b"e", size(80, 24)).await;
        let eval_raw = eventually_async("eval binary ran instead of shell builtin", async || {
            fs::read(&eval_out).ok()
        })
        .await;
        assert_eq!(eval_raw, format!("touch {}\0", pwned.display()).as_bytes());
        assert!(!pwned.exists(), "shell builtin eval must not execute");
        let _ = mux
            .close_terminal(api::CloseTerminalRequest {
                terminal: Some(eval_key),
            })
            .await;

        let launch_root = runtime.path().join("lune/resume");
        assert!(launch_root.exists(), "resume staging directory must exist");
        let leftovers: Vec<_> = fs::read_dir(&launch_root).unwrap().collect();
        assert!(leftovers.is_empty(), "leftover resume dirs: {leftovers:?}");
    });
}
