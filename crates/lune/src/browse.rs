use std::env;
use std::path::Path;
use std::process::ExitCode;

use lune_domain::projects::BrowserUrl;
use luned::api::change_projects_request::{Change, OpenBrowser};
use luned::api::ChangeProjectsRequest;
use luned::tasks;

use crate::daemon;
use crate::worktrees;

pub fn run(url: Option<&str>, folder: &Path) -> ExitCode {
    let url = match url.map(BrowserUrl::from_user).transpose() {
        Ok(url) => url.flatten(),
        Err(why) => {
            eprintln!("lune: {why}");
            return ExitCode::FAILURE;
        },
    };
    let from = match luned::mux::own::terminal_here() {
        Ok(from) => from,
        Err(why) => {
            eprintln!("lune: {why}");
            return ExitCode::FAILURE;
        },
    };
    let cwd = env::current_dir()
        .ok()
        .and_then(|dir| worktrees::real(&dir).ok())
        .and_then(|dir| dir.to_str().map(ToOwned::to_owned));
    let request = ChangeProjectsRequest {
        on: None,
        change: Some(Change::OpenBrowser(OpenBrowser {
            worktree: None,
            folder: cwd,
            url: url.map(|url| url.as_str().to_owned()),
            from: from.map(Into::into),
        })),
    };
    let ran = tasks::shared().block_on(async {
        let mut daemon = daemon::current(folder).await?.daemon;
        daemon
            .change_projects(request)
            .await
            .map_err(|status| status.message().to_owned())?;
        Ok::<(), String>(())
    });
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("lune: {why}");
            ExitCode::FAILURE
        },
    }
}
