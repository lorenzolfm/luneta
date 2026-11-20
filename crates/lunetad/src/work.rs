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

use crate::api::list_work_response::{
    Issue as SaidIssue, Listed as SaidListed, Pull as SaidPull, Work as SaidWork,
};
use crate::api::ListWorkResponse;
use crate::directory::Directory;
use crate::projects::{Dir, PROJECTS_MAX};
use crate::{git, issues, program};

pub const LISTED_MAX: usize = 50;

pub const LABELS_MAX: usize = 16;

pub const TITLE_BYTES_MAX: usize = 1024;

pub const NAME_BYTES_MAX: usize = 256;

pub const PATIENCE: Duration = Duration::from_secs(30);

pub const ANSWERED_MAX: Duration = PATIENCE.saturating_mul(2);

pub const FRESH: Duration = Duration::from_secs(15);

const LISTINGS_MAX: usize = PROJECTS_MAX;

const WAITERS_MAX: usize = LISTINGS_MAX;

const ISSUES: [&str; 4] = ["issue", "list", "--state", "open"];

const ISSUE_FIELDS: &str = "number,title,labels";

const PULLS: [&str; 4] = ["pr", "list", "--state", "open"];

const PULL_FIELDS: &str = "number,title,isDraft,headRefName,author";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Item {
    Issue(NonZeroU32),
    Pull(NonZeroU32),
}

impl Item {
    #[must_use]
    pub fn number(self) -> NonZeroU32 {
        match self {
            Item::Issue(number) | Item::Pull(number) => number,
        }
    }
}

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct More {
    pub issues: bool,
    pub pulls: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Work {
    issues: Vec<Issue>,
    pulls: Vec<Pull>,
    more: More,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listed {
    Work(Work),
    Unhosted,
}

impl Listed {
    #[must_use]
    pub fn answer(&self) -> ListWorkResponse {
        let listed = match self {
            Listed::Work(work) => SaidListed::Work(work.answer()),
            Listed::Unhosted => SaidListed::Unhosted(()),
        };
        ListWorkResponse { listed: Some(listed) }
    }

    pub fn of(said: ListWorkResponse) -> Result<Listed, String> {
        match said.listed.ok_or("listed: neither work nor unhosted")? {
            SaidListed::Work(work) => Work::of(work).map(Listed::Work),
            SaidListed::Unhosted(()) => Ok(Listed::Unhosted),
        }
    }
}

impl Work {
    pub fn new(issues: Vec<Issue>, pulls: Vec<Pull>, more: More) -> Result<Work, String> {
        full("issues", issues.len(), more.issues)?;
        full("pull requests", pulls.len(), more.pulls)?;
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
        Ok(Work { issues, pulls, more })
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
    pub fn more(&self) -> More {
        self.more
    }

    fn answer(&self) -> SaidWork {
        let Work { issues, pulls, more } = self;
        let issues = issues.iter().map(|issue| SaidIssue {
            number: issue.number.get(),
            title: issue.title.clone(),
            labels: issue.labels.clone(),
        });
        let pulls = pulls.iter().map(|pull| SaidPull {
            number: pull.number.get(),
            title: pull.title.clone(),
            draft: pull.draft,
            branch: pull.branch.clone(),
            author: pull.author.clone(),
        });
        SaidWork {
            issues: issues.collect(),
            pulls: pulls.collect(),
            more_issues: more.issues,
            more_pulls: more.pulls,
        }
    }

    fn of(said: SaidWork) -> Result<Work, String> {
        let SaidWork { issues, pulls, more_issues, more_pulls } = said;
        let number = |number| NonZeroU32::new(number).ok_or("an issue or pull request of number 0");
        let issues = issues.into_iter().map(|SaidIssue { number: at, title, labels }| {
            Ok(Issue { number: number(at)?, title, labels })
        });
        let pulls = pulls.into_iter().map(|SaidPull { number: at, title, draft, branch, author }| {
            Ok(Pull { number: number(at)?, title, draft, branch, author })
        });
        let issues = issues.collect::<Result<Vec<_>, &str>>()?;
        let pulls = pulls.collect::<Result<Vec<_>, &str>>()?;
        Work::new(issues, pulls, More { issues: more_issues, pulls: more_pulls })
    }
}

fn full(kind: &str, listed: usize, more: bool) -> Result<(), String> {
    if listed > LISTED_MAX {
        return Err(format!("at most {LISTED_MAX} {kind}"));
    }
    if more && listed < LISTED_MAX {
        return Err(format!("{listed} {kind} are listed of more, where {LISTED_MAX} would be"));
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct Listings {
    slots: Mutex<Slots>,
    done: Condvar,
}

impl Listings {
    pub fn list(&self, dir: &Dir) -> Result<Listed, String> {
        let mut slots = locked(&self.slots);
        if slots.seen(dir, Instant::now()) == Seen::Running {
            slots = self.list_waited(slots, dir)?;
        }
        let now = Instant::now();
        match slots.seen(dir, now) {
            Seen::Running => {
                let (dir, waited) = (dir.as_str(), PATIENCE.as_secs());
                return Err(format!("gh has listed the work in {dir} for over {waited}s"));
            },
            Seen::Fresh(listed) => return listed.clone(),
            Seen::Unlisted => slots.run(dir, now)?,
        }
        drop(slots);
        let listed = panic::catch_unwind(|| listed_by_gh(dir));
        let kept = listed.as_ref().ok().map(|listed| (Instant::now(), listed.clone()));
        locked(&self.slots).ran(dir, kept);
        self.done.notify_all();
        listed.unwrap_or_else(|panic| panic::resume_unwind(panic))
    }

    fn list_waited<'a>(
        &self,
        mut slots: MutexGuard<'a, Slots>,
        dir: &Dir,
    ) -> Result<MutexGuard<'a, Slots>, String> {
        assert!(slots.waiters <= WAITERS_MAX, "so many wait at most");
        if slots.waiters == WAITERS_MAX {
            return Err(format!("lunetad has {WAITERS_MAX} asks waiting on gh already, its most"));
        }
        slots.waiters += 1;
        let running = |slots: &mut Slots| matches!(slots.seen(dir, Instant::now()), Seen::Running);
        let waited = self.done.wait_timeout_while(slots, PATIENCE, running);
        let (mut slots, _) = waited.unwrap_or_else(PoisonError::into_inner);
        assert!(slots.waiters > 0, "a waiter is counted until it wakes");
        slots.waiters -= 1;
        Ok(slots)
    }
}

#[derive(Debug, Default)]
struct Slots {
    slots: BTreeMap<Dir, Slot>,
    waiters: usize,
}

#[derive(Debug)]
enum Slot {
    Running,
    Listed { at: Instant, listed: Result<Listed, String> },
}

#[derive(Debug, PartialEq, Eq)]
enum Seen<'a> {
    Running,
    Fresh(&'a Result<Listed, String>),
    Unlisted,
}

impl Slots {
    fn seen(&self, dir: &Dir, now: Instant) -> Seen<'_> {
        match self.slots.get(dir) {
            Some(Slot::Running) => Seen::Running,
            Some(Slot::Listed { at, listed }) if fresh(*at, now) => Seen::Fresh(listed),
            Some(Slot::Listed { .. }) | None => Seen::Unlisted,
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

    fn ran(&mut self, dir: &Dir, kept: Option<(Instant, Result<Listed, String>)>) {
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

fn listed_by_gh(dir: &Dir) -> Result<Listed, String> {
    if issues::remotes(&Directory::from(dir))?.is_empty() {
        return Ok(Listed::Unhosted);
    }
    let folder = dir.as_str().to_owned();
    let pulls = thread::Builder::new()
        .name("work-pulls".to_owned())
        .spawn(move || gh::<ListedPull>(&folder, &PULLS, PULL_FIELDS))
        .map_err(|error| format!("no thread to list pull requests in: {error}"))?;
    let issues = gh::<ListedIssue>(dir.as_str(), &ISSUES, ISSUE_FIELDS);
    let pulls = pulls.join().unwrap_or_else(|panic| panic::resume_unwind(panic));
    let (issues, more_issues) = issues?;
    let (pulls, more_pulls) = pulls?;
    let issues = issues.into_iter().map(|ListedIssue { number, title, labels }| {
        let labels = labels.into_iter().map(|label| cut(label.name, NAME_BYTES_MAX));
        let labels = labels.filter(|label| !label.is_empty()).take(LABELS_MAX).collect();
        Issue { number, title: cut(title, TITLE_BYTES_MAX), labels }
    });
    let pulls = pulls.into_iter().map(|pull| {
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
    let more = More { issues: more_issues, pulls: more_pulls };
    let work = Work::new(issues.collect(), pulls, more);
    let work = work.expect("gh's lists are cut to their most, and their names to NAME_BYTES_MAX, none empty");
    Ok(Listed::Work(work))
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

fn gh<T: DeserializeOwned>(dir: &str, asked: &[&str], fields: &str) -> Result<(Vec<T>, bool), String> {
    let said = asked.join(" ");
    let limit = (LISTED_MAX + 1).to_string();
    let mut listing = Command::new("gh");
    listing.args(asked).args(["--limit", &limit, "--json", fields]).current_dir(dir);
    git::local(&mut listing);
    let ran = program::run(&mut listing, PATIENCE).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => "gh is not on the PATH".to_owned(),
        _ => format!("gh {said} could not be run: {error}"),
    })?;
    let answer = ran.answer().map_err(|why| format!("gh {said}: {why}"))?;
    let mut listed: Vec<T> = serde_json::from_slice(&answer)
        .map_err(|why| format!("gh {said} said what does not read: {why}"))?;
    let more = listed.len() > LISTED_MAX;
    listed.truncate(LISTED_MAX);
    Ok((listed, more))
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
        let pull = |number: u32, author: Option<&str>| Pull {
            number: NonZeroU32::new(number).unwrap(),
            title: "Say view, not pane".to_owned(),
            draft: true,
            branch: "issue-511".to_owned(),
            author: author.map(str::to_owned),
        };
        let labelled = Issue { labels: vec!["enhancement".to_owned()], ..issue(507, "Home") };
        let work = Work::new(vec![labelled, issue(498, "Menu")], vec![pull(524, None), pull(526, Some("o"))], More::default());
        let work = Listed::Work(work.unwrap());
        assert_eq!(Listed::of(work.answer()), Ok(work));
        let many: Vec<Issue> = (1..=50).map(|at| issue(at, "many")).collect();
        let more = More { issues: true, pulls: false };
        let full = Listed::Work(Work::new(many, vec![], more).unwrap());
        assert_eq!(Listed::of(full.answer()), Ok(full));
        assert_eq!(Listed::of(Listed::Unhosted.answer()), Ok(Listed::Unhosted));
        let unsaid = Listed::of(ListWorkResponse { listed: None });
        assert_eq!(unsaid, Err("listed: neither work nor unhosted".to_owned()));
    }

    #[test]
    fn work_holds_no_number_0_and_no_more_than_it_lists() {
        let mut said = Work::new(vec![issue(1, "one")], vec![], More::default()).unwrap().answer();
        said.issues[0].number = 0;
        assert_eq!(Work::of(said), Err("an issue or pull request of number 0".to_owned()));
        let many = |most: u32| (1..=most).map(|at| issue(at, "many")).collect::<Vec<_>>();
        assert_eq!(Work::new(many(51), vec![], More::default()), Err("at most 50 issues".to_owned()));
        let long = issue(7, &"é".repeat(TITLE_BYTES_MAX));
        let refused = Err("#7 has a title longer than 1024 bytes".to_owned());
        assert_eq!(Work::new(vec![long], vec![], More::default()), refused);
        let more = More { issues: false, pulls: true };
        let refused = Err("0 pull requests are listed of more, where 50 would be".to_owned());
        assert_eq!(Work::new(many(50), vec![], more), refused, "more is said only past a full list");
        let more = More { issues: true, pulls: false };
        let refused = Err("49 issues are listed of more, where 50 would be".to_owned());
        assert_eq!(Work::new(many(49), vec![], more), refused);
        assert_eq!(Work::new(many(50), vec![], more).map(|work| work.more()), Ok(more));
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
        let work = |issues, pulls| Work::new(issues, pulls, More::default());
        for (pull, why) in refused {
            assert_eq!(work(vec![], vec![pull]), Err(why.to_owned()));
        }
        let labelled = |label: &str| Issue { labels: vec![label.to_owned()], ..issue(4, "t") };
        assert_eq!(work(vec![labelled("")], vec![]), Err("#4: a label is empty".to_owned()));
        let refused = Err("#4: a label is longer than 256 bytes".to_owned());
        assert_eq!(work(vec![labelled(&long)], vec![]), refused);
        let most = "b".repeat(NAME_BYTES_MAX);
        assert!(work(vec![labelled(&most)], vec![pull(&most, Some(&most))]).is_ok());
    }

    fn dir(path: &str) -> Dir {
        Dir::parse(path).unwrap()
    }

    #[test]
    fn a_listing_is_kept_a_while_and_none_is_kept_of_one_that_panicked() {
        let (mut slots, now, luneta) = (Slots::default(), Instant::now(), dir("/w/luneta"));
        assert_eq!(slots.seen(&luneta, now), Seen::Unlisted);
        slots.run(&luneta, now).unwrap();
        assert_eq!(slots.seen(&luneta, now), Seen::Running);
        let work = Ok(Listed::Work(Work::new(vec![issue(1, "one")], vec![], More::default()).unwrap()));
        slots.ran(&luneta, Some((now, work.clone())));
        assert_eq!(slots.seen(&luneta, now + FRESH.saturating_sub(Duration::from_millis(1))), Seen::Fresh(&work));
        assert_eq!(slots.seen(&luneta, now + FRESH), Seen::Unlisted);
        let refused = Err("gh is not on the PATH".to_owned());
        slots.run(&luneta, now + FRESH).unwrap();
        slots.ran(&luneta, Some((now + FRESH, refused.clone())));
        assert_eq!(slots.seen(&luneta, now + FRESH), Seen::Fresh(&refused), "a refusal is kept as long");
        slots.run(&luneta, now + FRESH * 2).unwrap();
        slots.ran(&luneta, None);
        assert_eq!(slots.seen(&luneta, now + FRESH * 2), Seen::Unlisted);
    }

    #[test]
    fn listings_make_room_by_what_is_stale_then_the_oldest_but_never_a_running_one() {
        let (mut slots, now) = (Slots::default(), Instant::now());
        let dirs: Vec<Dir> = (0..=LISTINGS_MAX).map(|at| dir(&format!("/w/{at}"))).collect();
        let after = |millis: usize| now + Duration::from_millis(u64::try_from(millis).unwrap());
        for (at, dir) in dirs[..LISTINGS_MAX].iter().enumerate() {
            slots.run(dir, after(at)).unwrap();
            slots.ran(dir, Some((after(at), Ok(Listed::Unhosted))));
        }
        slots.run(&dirs[LISTINGS_MAX], after(LISTINGS_MAX)).unwrap();
        assert_eq!(slots.seen(&dirs[0], after(LISTINGS_MAX)), Seen::Unlisted, "the oldest made room");
        assert_eq!(slots.seen(&dirs[1], after(LISTINGS_MAX)), Seen::Fresh(&Ok(Listed::Unhosted)));
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
    fn an_ask_past_the_most_that_wait_is_refused_at_once_and_a_waiter_counts_until_it_wakes() {
        let (listings, luneta) = (Listings::default(), dir("/w/luneta"));
        locked(&listings.slots).run(&luneta, Instant::now()).unwrap();
        locked(&listings.slots).waiters = WAITERS_MAX;
        let refused = Err(format!("lunetad has {WAITERS_MAX} asks waiting on gh already, its most"));
        let started = Instant::now();
        assert_eq!(listings.list(&luneta), refused);
        assert!(started.elapsed() < PATIENCE, "the refusal waits for nothing");
        assert_eq!(locked(&listings.slots).waiters, WAITERS_MAX, "a refused ask counts no waiter");

        locked(&listings.slots).waiters = WAITERS_MAX - 1;
        let came = thread::scope(|scope| {
            let waiting = scope.spawn(|| listings.list(&luneta));
            let counted = (0..1000).any(|_| {
                thread::sleep(Duration::from_millis(1));
                locked(&listings.slots).waiters == WAITERS_MAX
            });
            assert!(counted, "the ask waits on the listing that runs, and counts itself");
            locked(&listings.slots).ran(&luneta, Some((Instant::now(), Ok(Listed::Unhosted))));
            listings.done.notify_all();
            waiting.join().unwrap()
        });
        assert_eq!(came, Ok(Listed::Unhosted), "the waiter takes the listing that came");
        assert_eq!(locked(&listings.slots).waiters, WAITERS_MAX - 1, "and counts itself no more");
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
