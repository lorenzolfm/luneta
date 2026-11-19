use std::collections::BTreeMap;
use std::io;
use std::num::NonZeroU32;
use std::panic;
use std::process::Command;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::api::list_work_response::{Issue as SaidIssue, Pull as SaidPull};
use crate::api::ListWorkResponse;
use crate::projects::{Dir, PROJECTS_MAX};
use crate::{git, program};

pub const LISTED_MAX: usize = 50;

pub const LABELS_MAX: usize = 16;

pub const TITLE_BYTES_MAX: usize = 1024;

pub const NAME_BYTES_MAX: usize = 256;

pub const PATIENCE: Duration = Duration::from_secs(30);

pub const ANSWERED_MAX: Duration = PATIENCE.saturating_mul(2);

pub const FRESH: Duration = Duration::from_secs(15);

const LISTINGS_MAX: usize = PROJECTS_MAX;

const ISSUES: [&str; 4] = ["issue", "list", "--state", "open"];

const ISSUE_FIELDS: &str = "number,title,labels";

const PULLS: [&str; 4] = ["pr", "list", "--state", "open"];

const PULL_FIELDS: &str = "number,title,isDraft,headRefName,author";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub number: NonZeroU32,
    pub title: String,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pull {
    pub number: NonZeroU32,
    pub title: String,
    pub draft: bool,
    pub branch: String,
    pub author: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Work {
    issues: Vec<Issue>,
    pulls: Vec<Pull>,
}

impl Work {
    pub fn new(issues: Vec<Issue>, pulls: Vec<Pull>) -> Result<Work, String> {
        if issues.len() > LISTED_MAX {
            return Err(format!("at most {LISTED_MAX} issues"));
        }
        if pulls.len() > LISTED_MAX {
            return Err(format!("at most {LISTED_MAX} pull requests"));
        }
        for issue in &issues {
            titled(issue.number, &issue.title)?;
            if issue.labels.len() > LABELS_MAX {
                return Err(format!("#{} has more than {LABELS_MAX} labels", issue.number));
            }
            for label in &issue.labels {
                named(issue.number, "a label", label)?;
            }
        }
        for pull in &pulls {
            titled(pull.number, &pull.title)?;
            named(pull.number, "its branch", &pull.branch)?;
            if let Some(author) = &pull.author {
                named(pull.number, "its author", author)?;
            }
        }
        Ok(Work { issues, pulls })
    }

    #[must_use]
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }

    #[must_use]
    pub fn pulls(&self) -> &[Pull] {
        &self.pulls
    }

    #[must_use]
    pub fn answer(&self) -> ListWorkResponse {
        let issues = self.issues.iter().map(|issue| SaidIssue {
            number: issue.number.get(),
            title: issue.title.clone(),
            labels: issue.labels.clone(),
        });
        let pulls = self.pulls.iter().map(|pull| SaidPull {
            number: pull.number.get(),
            title: pull.title.clone(),
            draft: pull.draft,
            branch: pull.branch.clone(),
            author: pull.author.clone().unwrap_or_default(),
        });
        ListWorkResponse { issues: issues.collect(), pulls: pulls.collect() }
    }

    pub fn of(said: ListWorkResponse) -> Result<Work, String> {
        let ListWorkResponse { issues, pulls } = said;
        let number = |number| NonZeroU32::new(number).ok_or("an issue or pull request of number 0");
        let issues = issues.into_iter().map(|SaidIssue { number: at, title, labels }| {
            Ok(Issue { number: number(at)?, title, labels })
        });
        let pulls = pulls.into_iter().map(|SaidPull { number: at, title, draft, branch, author }| {
            let author = Some(author).filter(|author| !author.is_empty());
            Ok(Pull { number: number(at)?, title, draft, branch, author })
        });
        let issues = issues.collect::<Result<Vec<_>, &str>>()?;
        let pulls = pulls.collect::<Result<Vec<_>, &str>>()?;
        Work::new(issues, pulls)
    }
}

#[derive(Debug, Default)]
pub struct Listings {
    slots: Mutex<Slots>,
    done: Condvar,
}

impl Listings {
    pub fn list(&self, dir: &Dir) -> Result<Work, String> {
        let slots = locked(&self.slots);
        let waited = self.done.wait_timeout_while(slots, PATIENCE, |slots| slots.running(dir));
        let (mut slots, waited) = waited.unwrap_or_else(PoisonError::into_inner);
        if waited.timed_out() {
            let (dir, waited) = (dir.as_str(), PATIENCE.as_secs());
            return Err(format!("gh has listed the work in {dir} for over {waited}s"));
        }
        let now = Instant::now();
        if let Some(listed) = slots.fresh(dir, now) {
            return listed;
        }
        slots.run(dir, now)?;
        drop(slots);
        let listed = panic::catch_unwind(|| listed_by_gh(dir));
        let kept = listed.as_ref().ok().map(|listed| (Instant::now(), listed.clone()));
        locked(&self.slots).ran(dir, kept);
        self.done.notify_all();
        listed.unwrap_or_else(|panic| panic::resume_unwind(panic))
    }
}

#[derive(Debug, Default)]
struct Slots {
    slots: BTreeMap<Dir, Slot>,
}

#[derive(Debug)]
enum Slot {
    Running,
    Listed { at: Instant, listed: Result<Work, String> },
}

impl Slots {
    fn running(&self, dir: &Dir) -> bool {
        matches!(self.slots.get(dir), Some(Slot::Running))
    }

    fn fresh(&self, dir: &Dir, now: Instant) -> Option<Result<Work, String>> {
        match self.slots.get(dir)? {
            Slot::Listed { at, listed } if fresh(*at, now) => Some(listed.clone()),
            Slot::Listed { .. } => None,
            Slot::Running => unreachable!("a listing that runs is waited for before it is read"),
        }
    }

    fn run(&mut self, dir: &Dir, now: Instant) -> Result<(), String> {
        self.slots.retain(|_, slot| match slot {
            Slot::Running => true,
            Slot::Listed { at, .. } => fresh(*at, now),
        });
        if self.slots.len() == LISTINGS_MAX {
            let listed = self.slots.iter().filter_map(|(dir, slot)| match slot {
                Slot::Listed { at, .. } => Some((*at, dir)),
                Slot::Running => None,
            });
            let Some((_, oldest)) = listed.min() else {
                let most = format!("{LISTINGS_MAX} folders at once, its most");
                return Err(format!("lunetad lists the work of {most}"));
            };
            let oldest = oldest.clone();
            self.slots.remove(&oldest);
        }
        assert!(self.slots.len() < LISTINGS_MAX, "a listing has room to run");
        let was = self.slots.insert(dir.clone(), Slot::Running);
        assert!(was.is_none(), "a folder is listed once at a time, and its stale listing went");
        Ok(())
    }

    fn ran(&mut self, dir: &Dir, kept: Option<(Instant, Result<Work, String>)>) {
        let was = self.slots.remove(dir);
        assert!(matches!(was, Some(Slot::Running)), "only a listing that runs ends");
        if let Some((at, listed)) = kept {
            self.slots.insert(dir.clone(), Slot::Listed { at, listed });
        }
    }
}

fn fresh(at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(at) < FRESH
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Deserialize)]
struct ListedIssue {
    number: NonZeroU32,
    title: String,
    #[serde(default)]
    labels: Vec<Label>,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedPull {
    number: NonZeroU32,
    title: String,
    is_draft: bool,
    head_ref_name: String,
    author: Option<Author>,
}

#[derive(Deserialize)]
struct Author {
    login: String,
}

fn listed_by_gh(dir: &Dir) -> Result<Work, String> {
    let folder = dir.as_str().to_owned();
    let pulls = thread::Builder::new()
        .name("work-pulls".to_owned())
        .spawn(move || gh::<ListedPull>(&folder, &PULLS, PULL_FIELDS))
        .map_err(|error| format!("no thread to list pull requests in: {error}"))?;
    let issues = gh::<ListedIssue>(dir.as_str(), &ISSUES, ISSUE_FIELDS);
    let pulls = pulls.join().unwrap_or_else(|panic| panic::resume_unwind(panic));
    let issues = issues?.into_iter().map(|ListedIssue { number, title, labels }| {
        let labels = labels.into_iter().map(|label| cut(label.name, NAME_BYTES_MAX));
        let labels = labels.filter(|label| !label.is_empty()).take(LABELS_MAX).collect();
        Issue { number, title: cut(title, TITLE_BYTES_MAX), labels }
    });
    let pulls = pulls?.into_iter().map(|pull| {
        if pull.head_ref_name.is_empty() {
            return Err(format!("gh lists pull request #{} with no branch", pull.number));
        }
        let author = pull.author.map(|author| cut(author.login, NAME_BYTES_MAX));
        Ok(Pull {
            number: pull.number,
            title: cut(pull.title, TITLE_BYTES_MAX),
            draft: pull.is_draft,
            branch: cut(pull.head_ref_name, NAME_BYTES_MAX),
            author: author.filter(|login| !login.is_empty()),
        })
    });
    let pulls = pulls.collect::<Result<Vec<_>, String>>()?;
    let work = Work::new(issues.collect(), pulls);
    Ok(work.expect("gh's lists are cut to their most, and their names to NAME_BYTES_MAX, none empty"))
}

fn titled(number: NonZeroU32, title: &str) -> Result<(), String> {
    if title.len() <= TITLE_BYTES_MAX {
        return Ok(());
    }
    Err(format!("#{number} has a title longer than {TITLE_BYTES_MAX} bytes"))
}

fn named(number: NonZeroU32, what: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("#{number}: {what} is empty"));
    }
    if name.len() > NAME_BYTES_MAX {
        return Err(format!("#{number}: {what} is longer than {NAME_BYTES_MAX} bytes"));
    }
    Ok(())
}

fn gh<T: DeserializeOwned>(dir: &str, asked: &[&str], fields: &str) -> Result<Vec<T>, String> {
    let said = asked.join(" ");
    let limit = LISTED_MAX.to_string();
    let mut listing = Command::new("gh");
    listing.args(asked).args(["--limit", &limit, "--json", fields]).current_dir(dir);
    git::local(&mut listing);
    let ran = program::run(&mut listing, PATIENCE).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => "gh is not on the PATH".to_owned(),
        _ => format!("gh {said} could not be run: {error}"),
    })?;
    let answer = ran.answer().map_err(|why| format!("gh {said}: {why}"))?;
    let listed: Vec<T> = serde_json::from_slice(&answer)
        .map_err(|why| format!("gh {said} said what does not read: {why}"))?;
    Ok(listed.into_iter().take(LISTED_MAX).collect())
}

fn cut(mut text: String, most: usize) -> String {
    let end = text.floor_char_boundary(most);
    text.truncate(end);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(number: u32, title: &str) -> Issue {
        Issue { number: NonZeroU32::new(number).unwrap(), title: title.to_owned(), labels: vec![] }
    }

    #[test]
    fn work_goes_over_the_wire_and_back_as_it_was() {
        let pull = Pull {
            number: NonZeroU32::new(524).unwrap(),
            title: "Say view, not pane".to_owned(),
            draft: true,
            branch: "issue-511".to_owned(),
            author: None,
        };
        let labelled = Issue { labels: vec!["enhancement".to_owned()], ..issue(507, "Home") };
        let work = Work::new(vec![labelled, issue(498, "Menu")], vec![pull]).unwrap();
        assert_eq!(Work::of(work.answer()), Ok(work));
    }

    #[test]
    fn work_holds_no_number_0_and_no_more_than_it_lists() {
        let mut said = Work::new(vec![issue(1, "one")], vec![]).unwrap().answer();
        said.issues[0].number = 0;
        assert_eq!(Work::of(said), Err("an issue or pull request of number 0".to_owned()));
        let many = (1..=51).map(|at| issue(at, "many")).collect();
        assert_eq!(Work::new(many, vec![]), Err("at most 50 issues".to_owned()));
        let long = issue(7, &"é".repeat(TITLE_BYTES_MAX));
        let refused = Err("#7 has a title longer than 1024 bytes".to_owned());
        assert_eq!(Work::new(vec![long], vec![]), refused);
    }

    #[test]
    fn work_holds_no_name_that_is_empty_or_past_its_most() {
        let pull = |branch: &str, author: Option<&str>| Pull {
            number: NonZeroU32::new(9).unwrap(),
            title: "t".to_owned(),
            draft: false,
            branch: branch.to_owned(),
            author: author.map(str::to_owned),
        };
        let long = "b".repeat(NAME_BYTES_MAX + 1);
        let refused = [
            (pull("", None), "#9: its branch is empty"),
            (pull(&long, None), "#9: its branch is longer than 256 bytes"),
            (pull("b", Some("")), "#9: its author is empty"),
        ];
        for (pull, why) in refused {
            assert_eq!(Work::new(vec![], vec![pull]), Err(why.to_owned()));
        }
        let labelled = |label: &str| Issue { labels: vec![label.to_owned()], ..issue(4, "t") };
        assert_eq!(Work::new(vec![labelled("")], vec![]), Err("#4: a label is empty".to_owned()));
        let refused = Err("#4: a label is longer than 256 bytes".to_owned());
        assert_eq!(Work::new(vec![labelled(&long)], vec![]), refused);
        let most = "b".repeat(NAME_BYTES_MAX);
        assert!(Work::new(vec![labelled(&most)], vec![pull(&most, Some(&most))]).is_ok());
    }

    fn dir(path: &str) -> Dir {
        Dir::parse(path).unwrap()
    }

    #[test]
    fn a_listing_is_kept_a_while_and_none_is_kept_of_one_that_panicked() {
        let (mut slots, now, luneta) = (Slots::default(), Instant::now(), dir("/w/luneta"));
        assert_eq!(slots.fresh(&luneta, now), None);
        slots.run(&luneta, now).unwrap();
        assert!(slots.running(&luneta));
        let work = Ok(Work::new(vec![issue(1, "one")], vec![]).unwrap());
        slots.ran(&luneta, Some((now, work.clone())));
        assert!(!slots.running(&luneta));
        assert_eq!(slots.fresh(&luneta, now + FRESH.saturating_sub(Duration::from_millis(1))), Some(work));
        assert_eq!(slots.fresh(&luneta, now + FRESH), None);
        let refused = Err("gh is not on the PATH".to_owned());
        slots.run(&luneta, now + FRESH).unwrap();
        slots.ran(&luneta, Some((now + FRESH, refused.clone())));
        assert_eq!(slots.fresh(&luneta, now + FRESH), Some(refused), "a refusal is kept as long");
        slots.run(&luneta, now + FRESH * 2).unwrap();
        slots.ran(&luneta, None);
        assert!(!slots.running(&luneta));
        assert_eq!(slots.fresh(&luneta, now + FRESH * 2), None);
    }

    #[test]
    fn listings_make_room_by_what_is_stale_then_the_oldest_but_never_a_running_one() {
        let (mut slots, now) = (Slots::default(), Instant::now());
        let dirs: Vec<Dir> = (0..=LISTINGS_MAX).map(|at| dir(&format!("/w/{at}"))).collect();
        let after = |millis: usize| now + Duration::from_millis(u64::try_from(millis).unwrap());
        for (at, dir) in dirs[..LISTINGS_MAX].iter().enumerate() {
            slots.run(dir, after(at)).unwrap();
            slots.ran(dir, Some((after(at), Ok(Work::default()))));
        }
        slots.run(&dirs[LISTINGS_MAX], after(LISTINGS_MAX)).unwrap();
        assert_eq!(slots.fresh(&dirs[0], after(LISTINGS_MAX)), None, "the oldest made room");
        assert_eq!(slots.fresh(&dirs[1], after(LISTINGS_MAX)), Some(Ok(Work::default())));
        assert_eq!(slots.slots.len(), LISTINGS_MAX);
        slots.run(&dirs[0], now + FRESH * 2).unwrap();
        assert_eq!(slots.slots.len(), 2, "the stale went, and the two running stay");
        let mut running = Slots::default();
        for dir in &dirs[..LISTINGS_MAX] {
            running.run(dir, now).unwrap();
        }
        let refused = Err("lunetad lists the work of 64 folders at once, its most".to_owned());
        assert_eq!(running.run(&dirs[LISTINGS_MAX], now), refused);
    }

    #[test]
    fn a_long_title_is_cut_where_a_character_ends() {
        let title = format!("a{}", "é".repeat(TITLE_BYTES_MAX));
        let cut = cut(title, TITLE_BYTES_MAX);
        assert_eq!(cut.len(), TITLE_BYTES_MAX - 1);
        assert!(cut.ends_with('é'));
        assert_eq!(super::cut("short".to_owned(), TITLE_BYTES_MAX), "short");
    }
}
