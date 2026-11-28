use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub use lune_domain::agent::Harness;
use serde_json::{Value, json};

use crate::{folder, mux, program};

pub const AGY_STRICT: &str = r#"{"toolPermission":"strict"}"#;

pub const TOOLLESS: &str = "You have no tools here: read no file and run nothing, and answer from \
    what you are told.";

const BARE_FOLDER: &str = "bare";

pub const CODEX_READS_ONLY: [&str; 5] = [
    "--sandbox",
    "read-only",
    "--skip-git-repo-check",
    "--color",
    "never",
];

pub const CODEX_TOLD_ON_STDIN: &str = "-";

const CODEX_ERROR: &str = "ERROR:";

pub const PI_BARE: [&str; 6] = [
    "--no-extensions",
    "--no-mcp",
    "--no-skills",
    "--no-context-files",
    "--no-prompt-templates",
    "--no-approve",
];

const _: () = assert!(!AGY_STRICT.is_empty(), "strict settings json is not empty");
const _: () = assert!(!TOOLLESS.is_empty(), "toolless instruction is not empty");
const _: () = assert!(!BARE_FOLDER.is_empty(), "bare folder name is not empty");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NonEmpty<'a>(&'a str);

impl<'a> NonEmpty<'a> {
    #[must_use]
    pub const fn try_new(text: &'a str) -> Option<Self> {
        if text.is_empty() {
            None
        } else {
            Some(Self(text))
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'a str {
        self.0
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerWith<'a> {
    ClaudeCode { model: NonEmpty<'a> },
    Pi,
    Antigravity,
    Codex,
}

impl<'a> AnswerWith<'a> {
    #[must_use]
    pub const fn of(harness: Harness, claude_model: NonEmpty<'a>) -> Self {
        match harness {
            Harness::ClaudeCode => AnswerWith::ClaudeCode {
                model: claude_model,
            },
            Harness::Pi => AnswerWith::Pi,
            Harness::Antigravity => AnswerWith::Antigravity,
            Harness::Codex => AnswerWith::Codex,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OneAnswer<'a> {
    pub target: AnswerWith<'a>,
    pub system: NonEmpty<'a>,
    pub prompt: &'a str,
}

#[must_use]
pub const fn writer(harness: Harness, model: NonEmpty<'static>) -> &'static str {
    match harness {
        Harness::ClaudeCode => model.as_str(),
        Harness::Pi | Harness::Antigravity | Harness::Codex => harness.program(),
    }
}

#[derive(Debug)]
pub enum AgyStreamError {
    NoResult,
    InvalidJson(serde_json::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgyAnswer {
    Said(String),
    StoppedAtTool,
    Failed(String),
}

pub fn bare_folder() -> io::Result<PathBuf> {
    runtime_subfolder(BARE_FOLDER)
}

pub(crate) fn runtime_subfolder(name: &str) -> io::Result<PathBuf> {
    assert!(!name.is_empty(), "runtime subfolder name is not empty");
    let runtime = folder::find().map_err(|unfound| io::Error::other(unfound.to_string()))?;
    let dir = folder::make(&runtime)
        .and_then(|_| folder::make(&runtime.join(name)))
        .map_err(|refusal| io::Error::other(refusal.to_string()))?;
    Ok(dir.path().to_owned())
}

#[must_use]
pub fn agy_message(prompt: &str) -> String {
    let mut line = json!({
        "event": "user",
        "message": { "content": prompt },
    })
    .to_string();
    line.push('\n');
    assert!(line.ends_with('\n'));
    line
}

#[must_use]
pub fn agy_toolless_message(system: &str, prompt: &str) -> String {
    agy_message(&format!("{system} {TOOLLESS}\n\n{prompt}"))
}

#[must_use]
pub fn agy_strict() -> Command {
    let mut agy = Command::new(Harness::Antigravity.program());
    agy.args([
        "--settings",
        AGY_STRICT,
        "--disable-slash-commands",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
    ]);
    agy
}

#[must_use]
pub fn codex_toolless_message(system: &str, prompt: &str) -> String {
    format!("{system} {TOOLLESS}\n\n{prompt}")
}

#[must_use]
pub fn codex_once() -> Command {
    let mut codex = Command::new(Harness::Codex.program());
    codex
        .args(["exec", "--ephemeral"])
        .args(CODEX_READS_ONLY)
        .arg(CODEX_TOLD_ON_STDIN);
    codex
}

#[must_use]
pub fn codex_failure(stderr: &[u8]) -> Option<String> {
    let said = String::from_utf8_lossy(stderr);
    let errors = said
        .lines()
        .rev()
        .filter_map(|line| line.trim().strip_prefix(CODEX_ERROR));
    let last = errors.map(str::trim).find(|why| !why.is_empty());
    last.map(str::to_owned)
}

pub fn agy_stream_result(stdout: &[u8]) -> Result<Value, AgyStreamError> {
    let said = String::from_utf8_lossy(stdout);
    let mut result = None;
    let mut first_error = None;
    for line in said.lines().map(str::trim).filter(|line| !line.is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(mut record) => {
                if record["event"] == "result"
                    && let Some(found) = record.get_mut("result").filter(|value| value.is_object())
                {
                    result = Some(found.take());
                }
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    match (result, first_error) {
        (Some(result), _) => Ok(result),
        (None, Some(error)) => Err(AgyStreamError::InvalidJson(error)),
        (None, None) => Err(AgyStreamError::NoResult),
    }
}

pub fn agy_stream_answer(stdout: &[u8]) -> Result<AgyAnswer, AgyStreamError> {
    let result = agy_stream_result(stdout)?;
    if result["status"] == "SUCCESS" {
        let said = result["response"].as_str().unwrap_or_default().trim_end();
        if said.trim().is_empty() {
            let denied = result["denied_actions"]
                .as_array()
                .is_some_and(|denied| !denied.is_empty());
            return Ok(if denied {
                AgyAnswer::StoppedAtTool
            } else {
                AgyAnswer::Said(String::new())
            });
        }
        Ok(AgyAnswer::Said(said.to_owned()))
    } else {
        let why = result["error"]
            .as_str()
            .and_then(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
            .or_else(|| {
                result["status"]
                    .as_str()
                    .map(str::trim)
                    .filter(|status| !status.is_empty())
            })
            .unwrap_or("the run failed");
        Ok(AgyAnswer::Failed(why.to_owned()))
    }
}

pub fn one_answer(ask: OneAnswer<'_>) -> io::Result<(Command, Stdio)> {
    let system = ask.system.as_str();
    let (mut agent, stdin) = match ask.target {
        AnswerWith::ClaudeCode { model } => {
            let mut claude = Command::new(Harness::ClaudeCode.program());
            claude.args([
                "-p",
                "--model",
                model.as_str(),
                "--tools",
                "",
                "--safe-mode",
            ]);
            claude.args(["--no-session-persistence", "--system-prompt", system]);
            (claude, program::saying(ask.prompt)?)
        }
        AnswerWith::Pi => {
            let mut pi = Command::new(Harness::Pi.program());
            pi.args(["-p", "--no-session", "--no-tools"]).args(PI_BARE);
            pi.args(["--system-prompt", system]);
            (pi, program::saying(ask.prompt)?)
        }
        AnswerWith::Antigravity => {
            let agy = agy_strict();
            let message = agy_toolless_message(system, ask.prompt);
            (agy, program::saying(&message)?)
        }
        AnswerWith::Codex => {
            let message = codex_toolless_message(system, ask.prompt);
            (codex_once(), program::saying(&message)?)
        }
    };
    agent.current_dir(bare_folder()?);
    mux::own::in_no_terminal(&mut agent);
    Ok((agent, stdin))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    const HAIKU: NonEmpty<'static> = NonEmpty::try_new("haiku").expect("model name is non-empty");
    const SONNET: NonEmpty<'static> = NonEmpty::try_new("sonnet").expect("model name is non-empty");
    const BRIEF: NonEmpty<'static> =
        NonEmpty::try_new("be brief").expect("system prompt is non-empty");

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn sample(harness: Harness) -> OneAnswer<'static> {
        OneAnswer {
            target: AnswerWith::of(harness, HAIKU),
            system: BRIEF,
            prompt: "the prompt",
        }
    }

    #[test]
    fn one_answer_runs_in_no_terminal_with_no_tools() {
        let bare = bare_folder().unwrap();
        let (claude, stdin) = one_answer(sample(Harness::ClaudeCode)).unwrap();
        assert_eq!(claude.get_program(), "claude");
        assert_eq!(claude.get_current_dir(), Some(bare.as_path()));
        let said = [
            "-p",
            "--model",
            "haiku",
            "--tools",
            "",
            "--safe-mode",
            "--no-session-persistence",
            "--system-prompt",
            "be brief",
        ];
        assert_eq!(args(&claude), said);
        let mut unset: Vec<&OsStr> = claude
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name)
            .collect();
        unset.sort_unstable();
        assert_eq!(
            unset,
            [
                "LUNE_MUX_TERMINAL",
                "LUNE_MUX_TERMINAL_MADE",
                "OSC7577_TOKEN"
            ]
            .map(OsStr::new)
        );
        let heard = Command::new("cat").stdin(stdin).output().unwrap();
        assert_eq!(heard.stdout, b"the prompt");
    }

    #[test]
    fn one_answer_of_another_harness_has_no_tools_either() {
        let bare = bare_folder().unwrap();
        let (pi, stdin) = one_answer(sample(Harness::Pi)).unwrap();
        assert_eq!(pi.get_program(), "pi");
        assert_eq!(pi.get_current_dir(), Some(bare.as_path()));
        assert!(PI_BARE.contains(&"--no-extensions"));
        let said = [
            &["-p", "--no-session", "--no-tools"][..],
            &PI_BARE,
            &["--system-prompt", "be brief"],
        ];
        assert_eq!(args(&pi), said.concat());
        let heard = Command::new("cat").stdin(stdin).output().unwrap();
        assert_eq!(heard.stdout, b"the prompt");

        let (agy, stdin) = one_answer(sample(Harness::Antigravity)).unwrap();
        assert_eq!(agy.get_program(), "agy");
        assert_eq!(agy.get_current_dir(), Some(bare.as_path()));
        assert_eq!(
            args(&agy),
            [
                "--settings",
                AGY_STRICT,
                "--disable-slash-commands",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
            ]
        );
        let heard = Command::new("cat").stdin(stdin).output().unwrap();
        let line = std::str::from_utf8(&heard.stdout).unwrap();
        assert!(line.ends_with('\n'));
        let payload: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(
            payload,
            json!({
                "event": "user",
                "message": { "content": format!("be brief {TOOLLESS}\n\nthe prompt") },
            })
        );
        let (codex, stdin) = one_answer(sample(Harness::Codex)).unwrap();
        assert_eq!(codex.get_program(), "codex");
        assert_eq!(codex.get_current_dir(), Some(bare.as_path()));
        assert_eq!(
            args(&codex),
            [
                "exec",
                "--ephemeral",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--color",
                "never",
                "-",
            ]
        );
        let heard = Command::new("cat").stdin(stdin).output().unwrap();
        assert_eq!(
            String::from_utf8(heard.stdout).unwrap(),
            format!("be brief {TOOLLESS}\n\nthe prompt"),
            "Codex takes no system prompt, so the words it is told open with it"
        );
        for agent in [&pi, &agy, &codex] {
            let unset = |name: &str| {
                agent
                    .get_envs()
                    .any(|(key, value)| key.to_str() == Some(name) && value.is_none())
            };
            assert!(unset("LUNE_MUX_TERMINAL"));
        }
    }

    #[test]
    fn agy_takes_large_prompts_on_stdin_and_reads_stream_results() {
        assert_eq!(NonEmpty::try_new(""), None);
        let large = format!("head\0{}", "x".repeat(128 * 1024));
        let (agy, stdin) = one_answer(OneAnswer {
            target: AnswerWith::Antigravity,
            system: BRIEF,
            prompt: &large,
        })
        .unwrap();
        assert!(!args(&agy).iter().any(|arg| arg.contains("head")));
        let heard = Command::new("cat").stdin(stdin).output().unwrap();
        let payload: Value = serde_json::from_slice(&heard.stdout).unwrap();
        assert_eq!(
            payload["message"]["content"].as_str(),
            Some(format!("be brief {TOOLLESS}\n\n{large}").as_str())
        );

        let stream = [
            json!({"event": "init", "conversation_id": "c1"}).to_string(),
            json!({"event": "step_update", "step_update": {"step_type": "agent_response"}})
                .to_string(),
            json!({"event": "result", "result": {"status": "SUCCESS", "response": "Done.\n"}})
                .to_string(),
        ]
        .join("\n");
        assert_eq!(
            agy_stream_answer(stream.as_bytes()).unwrap(),
            AgyAnswer::Said("Done.".to_owned())
        );

        let denied = json!({
            "event": "result",
            "result": {
                "status": "SUCCESS",
                "response": "",
                "denied_actions": [{"action": "read_file"}],
            }
        })
        .to_string();
        assert_eq!(
            agy_stream_answer(denied.as_bytes()).unwrap(),
            AgyAnswer::StoppedAtTool
        );

        let failed = json!({
            "event": "result",
            "result": {"status": "ERROR", "error": "\nnot signed in\nmore details"}
        })
        .to_string();
        assert_eq!(
            agy_stream_answer(failed.as_bytes()).unwrap(),
            AgyAnswer::Failed("not signed in".to_owned())
        );

        let init_only = json!({"event": "init", "conversation_id": "c1"}).to_string();
        assert!(matches!(
            agy_stream_answer(init_only.as_bytes()),
            Err(AgyStreamError::NoResult)
        ));
        assert!(matches!(
            agy_stream_answer(b"not json"),
            Err(AgyStreamError::InvalidJson(_))
        ));
    }

    #[test]
    fn a_codex_run_that_failed_says_why_on_its_last_error_line() {
        let stderr = "OpenAI Codex v0.172.0\n--------\nworkdir: /w\nmodel: x\n--------\nuser\nSay hi\n\
            warning: Model metadata for `x` not found.\n\
            ERROR: stream disconnected\n\
            hook: Stop\n\
            ERROR: The 'x' model is not supported when using Codex with a ChatGPT account.\n\
            ERROR:   \n";
        assert_eq!(
            codex_failure(stderr.as_bytes()).as_deref(),
            Some("The 'x' model is not supported when using Codex with a ChatGPT account.")
        );
        assert_eq!(codex_failure(b"OpenAI Codex v0.172.0\n--------\n"), None);
        assert_eq!(codex_failure(b""), None);
    }

    #[test]
    fn what_writes_is_named_by_its_model_or_its_program() {
        assert_eq!(writer(Harness::ClaudeCode, SONNET), "sonnet");
        assert_eq!(writer(Harness::Pi, SONNET), "pi");
        assert_eq!(writer(Harness::Antigravity, SONNET), "agy");
        assert_eq!(writer(Harness::Codex, SONNET), "codex");
    }
}
