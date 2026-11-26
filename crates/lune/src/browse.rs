use std::env;
use std::path::Path;
use std::process::ExitCode;

use lune_domain::projects::BrowserUrl;
use luned::api::change_projects_request::{open_browser, Change, OpenBrowser};
use luned::api::ChangeProjectsRequest;
use luned::tasks;

use crate::daemon;
use crate::worktrees;

pub fn run(url: Option<&str>, folder: &Path) -> ExitCode {
    assert_ne!(folder.as_os_str(), "", "luned folder is non-empty");
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
    let target = match from {
        Some(from) => open_browser::Target::From(open_browser::FromTerminal {
            from: Some(from.into()),
            fallback_folder: current_folder().ok(),
        }),
        None => match current_folder() {
            Ok(dir) => open_browser::Target::Folder(dir),
            Err(why) => {
                eprintln!("lune: {why}");
                return ExitCode::FAILURE;
            },
        },
    };
    let request = ChangeProjectsRequest {
        on: None,
        change: Some(Change::OpenBrowser(OpenBrowser {
            target: Some(target),
            url: url.map(|url| url.as_str().to_owned()),
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

fn current_folder() -> Result<String, String> {
    let dir = env::current_dir().map_err(|error| format!("current directory is unread: {error}"))?;
    let real = worktrees::real(&dir)?;
    let text = real.to_str().ok_or_else(|| "current directory is not UTF-8".to_owned())?;
    assert_ne!(text, "", "resolved current directory is non-empty");
    Ok(text.to_owned())
}
