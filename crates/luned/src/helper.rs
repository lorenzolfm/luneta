use std::process::ExitCode;

use clap::Subcommand;

use crate::links::{self, Of};

#[derive(Subcommand)]
pub enum Helper {
    #[command(
        hide = true,
        about = "Make a worktree of a branch, or of a pull request by its link, in a repository \
                 `wt clone` made, as `wt add` does, and print where it is, or where one of the \
                 branch is already. luned makes the worktree of a link pasted in lune's sidebar \
                 so, with wt's code, and no `wt` on the `PATH`, in a process of its own that it \
                 can end. Why it made none comes last on stderr, after `luned: `"
    )]
    Worktree {
        #[arg(long, value_name = "DIR", help = "A directory of the repository")]
        repo: String,
        #[arg(
            long,
            help = "When no worktree has the branch, and a worktree of the repository on another \
                    branch, or on none, is where wt would make one, make none and print that \
                    worktree after `moved off: `"
        )]
        moved_off: bool,
        #[arg(
            value_parser = Of::parse,
            value_name = "BRANCH",
            help = "The branch, or the link of a pull request on GitHub, with or without its \
                    `https://`"
        )]
        of: Of,
    },
    #[command(
        hide = true,
        about = "Bring a worktree to its upstream branch, as `wt follow` does. Why it did not \
                 comes last on stderr, after `luned: `"
    )]
    Follow {
        #[arg(value_name = "DIR")]
        dir: String,
    },
    #[command(
        hide = true,
        about = "Fetch the upstream branch of a branch of the repository a directory is in, as \
                 `wt follow` fetches it, and move no branch. luned has it fetched so, for the \
                 count of how far behind it a branch is. What wt found of it is one word on \
                 stdout; why it did not fetch it comes last on stderr, after `luned: `"
    )]
    FetchUpstream {
        #[arg(value_name = "DIR")]
        dir: String,
        #[arg(value_name = "BRANCH")]
        branch: String,
    },
}

impl Helper {
    pub fn run(self) -> ExitCode {
        let said = match self {
            Helper::Worktree { repo, moved_off, of } => {
                links::worktree_made(&repo, &of, moved_off).map(Some)
            },
            Helper::Follow { dir } => links::followed(&dir).map(|()| None),
            Helper::FetchUpstream { dir, branch } => links::fetched_upstream(&dir, &branch)
                .map(|found| Some(links::upstream_word(found).to_owned())),
        };
        match said {
            Ok(said) => {
                if let Some(said) = said {
                    println!("{said}");
                }
                ExitCode::SUCCESS
            },
            Err(why) => {
                eprintln!("{}{why}", links::WHY);
                ExitCode::FAILURE
            },
        }
    }
}
