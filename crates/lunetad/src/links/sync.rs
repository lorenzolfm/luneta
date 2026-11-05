//! Brings a worktree to the head of its pull request, before an agent reviews
//! it. wt moves it, as `wt follow` does, and never one that holds work of its
//! own. wt follows the branch's upstream, so this checks it came to the head.

use std::process::Command;

use serde::Deserialize;

use super::{ASKING, GITHUB, Link, MAKING, across, failure, luneta, remote_in, said};
use crate::git;
use crate::program::{self, Ran};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewed {
    base_ref_name: String,
    head_ref_oid: String,
}

pub(super) fn to_head(dir: &str, link: &Link) -> Result<(), String> {
    let remote = remote_in(dir, link)?;
    let Viewed { base_ref_name: base, head_ref_oid: head } = viewed(link)?;
    let base = format!("+refs/heads/{base}:refs/remotes/{remote}/{base}");
    across(dir, "fetch", &["--no-tags", "--", &remote, &base])?;
    if here(dir)? == head {
        return Ok(());
    }
    follow(dir)?;
    let here = here(dir)?;
    if here == head {
        Ok(())
    } else {
        Err(format!("it followed its upstream branch to {here}, and the head of {link} is {head}"))
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

fn here(dir: &str) -> Result<String, String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", dir, "rev-parse", "--verify", "HEAD"]));
    said(&mut git).ok_or_else(|| "git did not say what HEAD is".to_owned())
}

/// A program of its own, alone, as `worktree` runs one: wt says how it goes on
/// stderr, and git may ask for a passphrase on the terminal of its session.
fn follow(dir: &str) -> Result<(), String> {
    let mut luneta = Command::new(luneta()?);
    git::local(luneta.args(["follow", dir]));
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
