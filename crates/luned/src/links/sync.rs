use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use super::{ASKING, GITHUB, Link, MAKING, failure, lune, remote, remotes, said, ssh_host};
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

pub(super) fn to_head(dir: &str, link: &Link, remote: Option<&str>) -> Result<(), String> {
    let remote = match remote {
        Some(remote) => Some(remote.to_owned()),
        None => remote_of(dir, link)?,
    };
    let Viewed { base_ref_name: base, head_ref_oid: head } = viewed(link)?;
    if let Some(remote) = remote {
        fetch(dir, &remote, &base, MAKING)?;
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
    Ok(remotes(dir)?.and_then(|listed| remote(&listed, link, ssh_host)))
}

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

pub(crate) fn fetch(dir: &str, remote: &str, base: &str, limit: Duration) -> Result<(), String> {
    let base = format!("+refs/heads/{base}:refs/remotes/{remote}/{base}");
    let args = ["-C", dir, "fetch", "--no-tags", "--", remote, &base];
    let mut git = Command::new("git");
    git::local(trusting_no_tree(&mut git).args(args));
    let ran = program::run_alone(&mut git, limit)
        .map_err(|error| format!("git fetch could not be run: {error}"))?;
    ran.answer().map(drop).map_err(|why| format!("git fetch: {why}"))
}

fn trusting_no_tree(command: &mut Command) -> &mut Command {
    git::configured(command, &NOTHING_OF_THE_TREE)
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

fn follow(dir: &str) -> Result<(), String> {
    let mut lune = Command::new(lune()?);
    git::local(trusting_no_tree(&mut lune).args(["follow", dir]));
    let ran = program::run_alone(&mut lune, MAKING)
        .map_err(|error| format!("lune follow could not be run: {error}"))?;
    match ran {
        Ran::Ended { status, .. } if status.success() => Ok(()),
        Ran::Ended { status, stderr, .. } => {
            let said = String::from_utf8_lossy(&stderr);
            Err(failure(&said).unwrap_or_else(|| format!("lune follow: {status}")))
        },
        Ran::Stuck => Err("wt follow did not end in time".to_owned()),
    }
}
