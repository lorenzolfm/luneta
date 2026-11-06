//! Brings a worktree to the head of its pull request, before an agent reviews
//! it. wt moves it, as `wt follow` does, and never one that holds work of its
//! own. wt follows the branch's upstream, so this checks it came to the head.

use std::env;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use super::{
    ASKING, GITHUB, Link, MAKING, Remotes, failure, luneta, remote, remotes_in, said, ssh_host,
};
use crate::git;
use crate::program::{self, Ran};

const NOTHING_OF_THE_TREE: [(&str, &str); 2] =
    [("core.hooksPath", "/dev/null"), ("core.fsmonitor", "false")];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewed {
    base_ref_name: String,
    head_ref_oid: String,
}

pub(super) fn to_head(dir: &str, link: &Link) -> Result<(), String> {
    let remote = remote_of(dir, link)?;
    let Viewed { base_ref_name: base, head_ref_oid: head } = viewed(link)?;
    if let Some(remote) = remote {
        fetch(dir, &remote, &base)?;
    }
    if here(dir)? == head {
        return Ok(());
    }
    let top = top(dir)?;
    follow(&top)?;
    let here = here(&top)?;
    if here != head {
        return Err(format!(
            "it followed its upstream branch to {here}, and the head of {link} is {head}"
        ));
    }
    if !Path::new(dir).is_dir() {
        return Err("the worktree moved, and where it went has no such directory".to_owned());
    }
    Ok(())
}

fn remote_of(dir: &str, link: &Link) -> Result<Option<String>, String> {
    match remotes_in(dir) {
        Remotes::Listed(listed) => Ok(remote(&listed, link, ssh_host)),
        Remotes::Unlisted => Ok(None),
        Remotes::Unsaid(why) => Err(format!("git did not say the remotes of {dir}: {why}")),
    }
}

/// Asks GitHub, as git cannot tell a pull request's base.
fn viewed(link: &Link) -> Result<Viewed, String> {
    let number = link.number.to_string();
    let repository = format!("{GITHUB}/{}", link.repository());
    let mut gh = Command::new("gh");
    gh.args(["pr", "view", &number, "--repo", &repository, "--json", "baseRefName,headRefOid"]);
    let ran = program::run(&mut gh, ASKING)
        .map_err(|error| format!("gh pr view could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh pr view: {why}"))?;
    serde_json::from_slice(&said).map_err(|error| format!("gh pr view does not read: {error}"))
}

fn fetch(dir: &str, remote: &str, base: &str) -> Result<(), String> {
    let base = format!("+refs/heads/{base}:refs/remotes/{remote}/{base}");
    let args = ["-C", dir, "fetch", "--no-tags", "--", remote, &base];
    let mut git = Command::new("git");
    git::local(trusting_no_tree(&mut git).args(args));
    let ran = program::run_alone(&mut git, MAKING)
        .map_err(|error| format!("git fetch could not be run: {error}"))?;
    ran.answer().map(drop).map_err(|why| format!("git fetch: {why}"))
}

fn trusting_no_tree(command: &mut Command) -> &mut Command {
    let count = env::var("GIT_CONFIG_COUNT").ok();
    let mut next = count.and_then(|count| count.parse().ok()).unwrap_or(0_u32);
    for (key, value) in NOTHING_OF_THE_TREE {
        command.env(format!("GIT_CONFIG_KEY_{next}"), key);
        command.env(format!("GIT_CONFIG_VALUE_{next}"), value);
        next = next.saturating_add(1);
    }
    command.env("GIT_CONFIG_COUNT", next.to_string())
}

fn here(dir: &str) -> Result<String, String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "rev-parse", "--verify", "HEAD"]));
    said(&mut git).ok_or_else(|| "git did not say what HEAD is".to_owned())
}

fn top(dir: &str) -> Result<String, String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "rev-parse", "--show-toplevel"]));
    said(&mut git).ok_or_else(|| "git did not say where the worktree's top is".to_owned())
}

/// A program of its own, alone, as `worktree` runs one: wt says how it goes on
/// stderr, and git may ask for a passphrase on the terminal of its session.
fn follow(dir: &str) -> Result<(), String> {
    let mut luneta = Command::new(luneta()?);
    git::local(trusting_no_tree(&mut luneta).args(["follow", dir]));
    let ran = program::run_alone(&mut luneta, MAKING)
        .map_err(|error| format!("luneta follow could not be run: {error}"))?;
    match ran {
        Ran::Ended { status, .. } if status.success() => Ok(()),
        Ran::Ended { status, stderr, .. } => {
            let said = String::from_utf8_lossy(&stderr);
            Err(failure(&said).unwrap_or_else(|| format!("luneta follow: {status}")))
        },
        Ran::Stuck => Err("wt follow did not end in time".to_owned()),
    }
}
