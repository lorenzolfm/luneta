use std::io;
use std::num::NonZeroU32;
use std::panic;
use std::process::Command;
use std::thread;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::api::list_work_response::{Issue as SaidIssue, Pull as SaidPull};
use crate::api::ListWorkResponse;
use crate::projects::Dir;
use crate::{git, program};

pub const LISTED_MAX: usize = 50;

pub const LABELS_MAX: usize = 16;

pub const TITLE_BYTES_MAX: usize = 1024;

pub const PATIENCE: Duration = Duration::from_secs(30);

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
        if issues.len() > LISTED_MAX || pulls.len() > LISTED_MAX {
            return Err(format!("at most {LISTED_MAX} issues and {LISTED_MAX} pull requests"));
        }
        let titles = issues.iter().map(|it| &it.title).chain(pulls.iter().map(|it| &it.title));
        if let Some(title) = titles.into_iter().find(|title| title.len() > TITLE_BYTES_MAX) {
            let start: String = title.chars().take(20).collect();
            return Err(format!("{start}… is a title longer than {TITLE_BYTES_MAX} bytes"));
        }
        if let Some(issue) = issues.iter().find(|issue| issue.labels.len() > LABELS_MAX) {
            return Err(format!("#{} has more than {LABELS_MAX} labels", issue.number));
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

pub fn listed(dir: &Dir) -> Result<Work, String> {
    let folder = dir.as_str().to_owned();
    let pulls = thread::Builder::new()
        .name("work-pulls".to_owned())
        .spawn(move || gh::<ListedPull>(&folder, &PULLS, PULL_FIELDS))
        .map_err(|error| format!("no thread to list pull requests in: {error}"))?;
    let issues = gh::<ListedIssue>(dir.as_str(), &ISSUES, ISSUE_FIELDS);
    let pulls = pulls.join().unwrap_or_else(|panic| panic::resume_unwind(panic));
    let issues = issues?.into_iter().map(|ListedIssue { number, title, labels }| {
        let labels = labels.into_iter().take(LABELS_MAX).map(|label| label.name).collect();
        Issue { number, title: cut(title, TITLE_BYTES_MAX), labels }
    });
    let pulls = pulls?.into_iter().map(|pull| Pull {
        number: pull.number,
        title: cut(pull.title, TITLE_BYTES_MAX),
        draft: pull.is_draft,
        branch: pull.head_ref_name,
        author: pull.author.map(|author| author.login).filter(|login| !login.is_empty()),
    });
    let work = Work::new(issues.collect(), pulls.collect());
    Ok(work.expect("gh's lists are cut to LISTED_MAX, their titles to TITLE_BYTES_MAX and labels to LABELS_MAX"))
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
        let refused = "at most 50 issues and 50 pull requests";
        assert_eq!(Work::new(many, vec![]), Err(refused.to_owned()));
        let long = issue(7, &"é".repeat(TITLE_BYTES_MAX));
        assert!(Work::new(vec![long], vec![]).unwrap_err().ends_with("longer than 1024 bytes"));
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
