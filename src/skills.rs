//! The skills a pane's directory can run, for the menu's search: each a
//! `SKILL.md`, a command or a workflow, where Claude Code finds them for that
//! directory.
//!
//! Claude Code names the skills it would run only as it starts a session:
//! `claude -p` with `--output-format stream-json` says them in the `init` it
//! prints first. That is a session's start for each search, with your hooks
//! and MCP servers, and a prompt that must not reach the model, for their names
//! alone: not what each does, which the search goes by too. So the finder
//! looks where Claude Code looks, and asks `claude plugin list`, which takes a
//! moment and starts no session, which plugins are on in that directory.

use std::collections::{BTreeMap, HashMap, HashSet};

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::Deserialize;
use zellij_tile::prelude::*;

use crate::checkouts::DIR_KEY;
use crate::context::CONTEXT_KEY;
use crate::cursor::{Cursor, Stops};
use crate::fetch::Fetch;

pub const CONTEXT_VALUE: &str = "skills";

/// Prints what `claude plugin list --json` says of the plugins in the
/// directory `$1`, then for each skill, command and workflow found for it:
/// where it was found, its path there and what it says of itself, each ending
/// in a NUL. A skill is a directory's `SKILL.md`, a command a `.md` in
/// `commands`, and a workflow a `.js` in `workflows`. They are found where
/// Claude Code finds them: in the `.claude` of the directory and of each one
/// above it up to its repository's top, or up to `/` outside of one, then in
/// yours, where they were found being none, then in each plugin installed,
/// where they were found being its install path. A plugin keeps its skills
/// where its manifest says, which is printed whole: every `SKILL.md` in it is
/// printed, but in a `node_modules` or a `.git`. Of a `.md`, what it
/// says of itself is its frontmatter, and of a workflow the `meta` it begins
/// with, each its first 64 lines, with any NUL in it left out: it would end
/// the field early, and every field after it would be read as the one before.
/// All of it is cut at 256 KiB: zellij hands it to the plugin as a JSON array
/// of numbers, some four bytes to a byte.
const FIND: &str = r#"dir=$(cd "$1" 2>/dev/null && pwd -P) || exit 0
config=${CLAUDE_CONFIG_DIR:-$HOME/.claude}
found() {
    printf '%s\0%s\0' "$1" "${3#"$2"/}"
    case $3 in
    *.js) awk '{ print } /^}/ || NR >= 64 { exit }' "$3" 2>/dev/null ;;
    *) awk 'NR == 1 { if ($0 !~ /^---[[:space:]]*$/) exit; next }
        /^---[[:space:]]*$/ || NR > 65 { exit }
        { print }' "$3" 2>/dev/null ;;
    esac | tr -d '\000'
    printf '\0'
}
under() {
    for f in "$2"/skills/*/SKILL.md "$2"/commands/*.md "$2"/workflows/*.js; do
        if [ -f "$f" ]; then found "$1" "$2" "$f"; fi
    done
}
{
    plugins=$(cd "$dir" && claude plugin list --json 2>/dev/null)
    printf '%s\0' "$plugins"
    top=$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null) || top=/
    while :; do
        under '' "${dir%/}/.claude"
        case $dir in "$top" | /) break ;; esac
        dir=${dir%/*}
        dir=${dir:-/}
    done
    under '' "$config"
    printf '%s\n' "$plugins" |
        sed -n 's/^ *"installPath": *"\(.*\)",\{0,1\} *$/\1/p' |
        sort -u |
        while IFS= read -r path; do
            if [ -f "$path/.claude-plugin/plugin.json" ]; then
                printf '%s\0%s\0' "$path" .claude-plugin/plugin.json
                head -c 65536 "$path/.claude-plugin/plugin.json" | tr -d '\000'
                printf '\0'
            fi
            find -L "$path" -maxdepth 6 \( -name node_modules -o -name .git \) -prune \
                -o -name SKILL.md -type f -print 2>/dev/null |
                while IFS= read -r f; do found "$path" "$path" "$f"; done
            for f in "$path"/commands/*.md; do
                if [ -f "$f" ]; then found "$path" "$path" "$f"; fi
            done
        done
} | head -c 262144"#;

/// Where a plugin's manifest is in it.
const MANIFEST: &str = ".claude-plugin/plugin.json";

/// How long a description the menu keeps, in characters: it shows a line of
/// one at most, and the search goes by it.
const DESCRIPTION_CHARS: usize = 500;

/// A skill the directory can run, as a prompt runs it: its name, after the
/// `/`, and what its frontmatter says it does, on one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: Name,
    pub description: String,
}

/// The name a skill runs by, after its `/`: one word, as the first word of a
/// prompt is the skill it runs. Claude Code would read a name with a space in
/// it as another skill, and the rest as what that one is asked.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(String);

impl Name {
    /// `name`, if a prompt can run it: a word, with no space or control
    /// character in it. Else `name` back, to say which.
    pub fn new(name: String) -> Result<Self, String> {
        let spaced = |c: char| c.is_whitespace() || c.is_control();
        if name.is_empty() || name.contains(spaced) { Err(name) } else { Ok(Name(name)) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Asks the finder for the skills `cwd` can run, with `context`, which the
/// answer carries back with the directory it is for.
fn ask(cwd: &str, mut context: BTreeMap<String, String>) {
    context.insert(CONTEXT_KEY.to_owned(), CONTEXT_VALUE.to_owned());
    context.insert(DIR_KEY.to_owned(), cwd.to_owned());
    run_command(&["sh", "-c", FIND, "sh", cwd], context);
}

/// The skills the finder found, or what went wrong, as the first line it said.
pub fn answer(exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Fetch<Vec<Skill>> {
    if exit_code == Some(0) {
        return Fetch::Ready(found(stdout));
    }
    let said = String::from_utf8_lossy(stderr);
    match said.lines().next().map(str::trim) {
        Some(said) if !said.is_empty() => Fetch::Failed(format!("skills: {said}")),
        _ => Fetch::Failed("skills: the finder failed".to_owned()),
    }
}

/// What `FIND` printed, as the skills you can run, by name. A plugin's skill
/// runs as `plugin:skill`, and only while `claude plugin list` says the
/// plugin is on. A skill found twice is the first found: the one nearest the
/// directory. One whose frontmatter says it is not for you to run is left
/// out, and so is one named what no slash command runs, which the log says.
/// A plugin's skills are named once all of its manifest is read, wherever it
/// was printed. Output cut short ends with the last skill printed whole.
fn found(stdout: &[u8]) -> Vec<Skill> {
    let printed = String::from_utf8_lossy(stdout);
    let whole = printed.rfind('\0').map_or("", |end| &printed[..=end]);
    let mut fields = whole.split_terminator('\0');
    let mut plugins = plugins_on(fields.next().unwrap_or(""));
    let mut files = Vec::new();
    while let (Some(at), Some(path), Some(text)) = (fields.next(), fields.next(), fields.next()) {
        files.push((at, path, text));
    }
    for (at, path, text) in &files {
        if let (Some(plugin), MANIFEST) = (plugins.get_mut(*at), *path) {
            plugin.keeps(text);
        }
    }
    let mut seen = HashSet::new();
    let mut skills = Vec::new();
    for (at, path, text) in files {
        let found = match (at, plugins.get(at)) {
            ("", _) => own(path, text),
            (_, Some(plugin)) => plugin.name(path).map(|name| (name, Front::read(text))),
            (_, None) => None,
        };
        let Some((name, front)) = found else {
            continue;
        };
        if !front.invocable {
            continue;
        }
        let name = match Name::new(name) {
            Ok(name) => name,
            Err(name) => {
                eprintln!("luneta skills: no slash command runs {path} by its name, {name:?}");
                continue;
            },
        };
        if seen.insert(name.clone()) {
            // A tab, say, has no width to draw, and would throw the line off.
            let shown = |c: char| if c.is_control() { ' ' } else { c };
            let description = front.description.chars().map(shown).take(DESCRIPTION_CHARS);
            skills.push(Skill { name, description: description.collect() });
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// A skill, a command or a workflow of yours, or of a directory's, by its
/// path in its `.claude`, and what it says of itself in `text`: its name, and
/// what the menu reads of it. A skill is named for its directory, as
/// `skills/<name>/SKILL.md`, and a command for its file, as
/// `commands/<name>.md`. A workflow, `workflows/<file>.js`, goes by the name
/// its `meta` gives it, or else by its file's.
fn own(path: &str, text: &str) -> Option<(String, Front)> {
    let workflow = path.strip_prefix("workflows/").and_then(|path| path.strip_suffix(".js"));
    if let Some(file) = workflow.filter(|file| one_name(file)) {
        let name = meta(text, "name").unwrap_or_else(|| file.to_owned());
        let description = meta(text, "description").unwrap_or_default();
        return Some((name, Front { description, invocable: true }));
    }
    let skill = path.strip_prefix("skills/").and_then(|path| path.strip_suffix("/SKILL.md"));
    let name = skill.filter(|name| one_name(name)).or_else(|| command(path))?;
    Some((name.to_owned(), Front::read(text)))
}

/// The name of a command, by its path in what holds it: `commands/<name>.md`.
fn command(path: &str) -> Option<&str> {
    path.strip_prefix("commands/")?.strip_suffix(".md").filter(|name| one_name(name))
}

/// Whether `name` is the name of one file or directory, and not a path.
fn one_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/')
}

/// A plugin as `claude plugin list --json` lists it, once for each place it
/// is installed in. A list with no `enabled` or `installPath` is one luneta
/// cannot read, and no plugin of it is on: rather than every plugin off.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Installed {
    id: String,
    enabled: bool,
    install_path: String,
}

/// A plugin that is on: its name, which is its id up to the `@` of its
/// marketplace, and where in it it keeps its skills: `skills`, and the paths
/// its manifest names besides, each a skill's own directory or one that holds
/// skills.
struct Plugin {
    name: String,
    keeps: Vec<String>,
}

/// Each plugin that is on, by its install path. `claude` missing lists none,
/// and so does a list luneta cannot read, which the log says: Claude Code
/// changed how it lists them, and the finder must follow.
fn plugins_on(listed: &str) -> HashMap<String, Plugin> {
    if listed.trim().is_empty() {
        return HashMap::new();
    }
    let installed: Vec<Installed> = match serde_json::from_str(listed) {
        Ok(installed) => installed,
        Err(error) => {
            eprintln!("luneta skills: claude plugin list said what luneta cannot read: {error}");
            return HashMap::new();
        },
    };
    installed
        .into_iter()
        .filter(|plugin| plugin.enabled && !plugin.install_path.is_empty())
        .map(|plugin| {
            let name = plugin.id.split('@').next().unwrap_or_default().to_owned();
            (plugin.install_path, Plugin { name, keeps: vec!["skills".to_owned()] })
        })
        .collect()
}

impl Plugin {
    /// Takes up where the plugin's manifest says it keeps skills: a path,
    /// or a list of them, under `skills`.
    fn keeps(&mut self, manifest: &str) {
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(manifest) else {
            return;
        };
        let paths = match manifest.get("skills") {
            Some(serde_json::Value::String(path)) => vec![path.as_str()],
            Some(serde_json::Value::Array(paths)) => {
                paths.iter().filter_map(serde_json::Value::as_str).collect()
            },
            _ => Vec::new(),
        };
        self.keeps.extend(paths.into_iter().map(|path| bare(path).to_owned()));
    }

    /// The name a skill or a command at `path` in the plugin runs as, if it
    /// is one: a `SKILL.md` where the plugin keeps skills, or a `.md` in
    /// `commands`.
    fn name(&self, path: &str) -> Option<String> {
        if let Some(command) = command(path) {
            return Some(format!("{}:{command}", self.name));
        }
        let dir = path.strip_suffix("/SKILL.md")?;
        let (holder, skill) = dir.rsplit_once('/').unwrap_or(("", dir));
        let kept = self.keeps.iter().any(|kept| kept == dir || kept == holder);
        kept.then(|| format!("{}:{skill}", self.name))
    }
}

/// A path as a manifest names it, as `FIND` prints one in the plugin:
/// without a `./` at its start or a `/` at its end.
fn bare(path: &str) -> &str {
    let mut path = path.trim_end_matches('/');
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    path
}

/// What the menu reads of a frontmatter.
struct Front {
    description: String,
    /// False for a skill that says `user-invocable: false`: Claude Code runs
    /// it, but no slash command does.
    invocable: bool,
}

impl Front {
    /// The YAML of a frontmatter, read as far as the menu needs: a key's
    /// value on its line, quoted or not, and the lines indented under it, as
    /// a block scalar or a plain one runs on, joined by spaces.
    fn read(front: &str) -> Self {
        let mut read = Front { description: String::new(), invocable: true };
        let mut lines = front.lines().peekable();
        while let Some(line) = lines.next() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            let block = value.starts_with('>') || value.starts_with('|');
            let mut parts: Vec<&str> = Vec::new();
            if !block && !value.is_empty() {
                parts.push(value);
            }
            let under = |next: &&str| next.starts_with([' ', '\t']) || next.trim().is_empty();
            while let Some(next) = lines.next_if(under) {
                parts.extend(Some(next.trim()).filter(|part| !part.is_empty()));
            }
            match key {
                "description" => read.description = unquote(&parts.join(" ")),
                "user-invocable" => read.invocable = unquote(&parts.join(" ")) != "false",
                _ => {},
            }
        }
        read
    }
}

/// The string `key` has in a workflow's `meta`: the first line that starts
/// with the key, and the literal after its colon, in quotes of any kind that
/// JavaScript takes. None for a key with no literal on its line.
fn meta(meta: &str, key: &str) -> Option<String> {
    meta.lines().find_map(|line| {
        let value = line.trim_start().strip_prefix(key)?.trim_start().strip_prefix(':')?;
        literal(value.trim_start())
    })
}

/// The JavaScript string literal `text` starts with, what its backslashes
/// escape taken as the characters they stand for, and a line break as a
/// space. None for text that starts with none, or ends before it does.
fn literal(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let quote = chars.next().filter(|quote| matches!(quote, '\'' | '"' | '`'))?;
    let mut read = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => read.push(match chars.next()? {
                'n' | 'r' | 't' => ' ',
                escaped => escaped,
            }),
            c if c == quote => return Some(read),
            c => read.push(c),
        }
    }
    None
}

/// A YAML scalar with its quotes taken off, and what they escape.
fn unquote(value: &str) -> String {
    let quoted = |quote: char| {
        value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote)
    };
    if quoted('"') {
        return value[1..value.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\");
    }
    if quoted('\'') {
        return value[1..value.len() - 1].replace("''", "'");
    }
    value.to_owned()
}

/// How well a skill matches a search: one named the term itself beats any
/// other, then one whose name matches beats one only its description does,
/// and then the higher score wins. A few letters land somewhere in nearly
/// any description, so those come last.
type Fit = (bool, bool, i64);

/// A skill that matches the term: where it stands among the skills, and the
/// characters of its name the term lit.
pub struct Found {
    at: usize,
    hits: Vec<usize>,
}

impl Stops for Found {}

/// The search box over the skills of a directory, while it is open: the
/// directory, what is typed, the skills, once the finder has said, and those
/// that match what is typed, best first, the cursor on one. With nothing
/// typed, every skill matches, by name.
pub struct Search {
    cwd: String,
    term: String,
    skills: Fetch<Vec<Skill>>,
    found: Cursor<Found>,
}

impl Search {
    /// A box over the skills `cwd` can run, once it has asked the finder for
    /// them with `context`: no box waits on an answer nothing asked for.
    pub fn ask(cwd: &str, context: BTreeMap<String, String>) -> Self {
        ask(cwd, context);
        Search {
            cwd: cwd.to_owned(),
            term: String::new(),
            skills: Fetch::Waiting,
            found: Cursor::default(),
        }
    }

    /// The directory whose skills the box lists.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn term(&self) -> &str {
        &self.term
    }

    pub fn skills(&self) -> &Fetch<Vec<Skill>> {
        &self.skills
    }

    /// The skills that match, best first, each with the characters of its
    /// name the term lit.
    pub fn found(&self) -> impl Iterator<Item = (&Skill, &[usize])> {
        let skills = match &self.skills {
            Fetch::Ready(skills) => skills.as_slice(),
            Fetch::Waiting | Fetch::Failed(_) => &[],
        };
        self.found.iter().filter_map(|found| Some((skills.get(found.at)?, found.hits.as_slice())))
    }

    /// How many skills match.
    pub fn count(&self) -> usize {
        self.found.len()
    }

    /// Where the cursor is among the skills that match.
    pub fn selected(&self) -> Option<usize> {
        self.found.selected()
    }

    /// The skill the cursor is on.
    pub fn chosen(&self) -> Option<&Skill> {
        let found = self.found.selected_row()?;
        match &self.skills {
            Fetch::Ready(skills) => skills.get(found.at),
            Fetch::Waiting | Fetch::Failed(_) => None,
        }
    }

    /// Takes what the finder said, while the box waits for it, and says
    /// whether it did. An answer that comes after the first is for a box
    /// closed before it came.
    pub fn fill(&mut self, skills: Fetch<Vec<Skill>>) -> bool {
        if self.skills != Fetch::Waiting {
            return false;
        }
        self.skills = skills;
        self.narrow();
        true
    }

    pub fn push(&mut self, c: char) {
        self.term.push(c);
        self.narrow();
    }

    /// Takes back the last character typed, and says whether there was one.
    pub fn pop(&mut self) -> bool {
        let popped = self.term.pop().is_some();
        if popped {
            self.narrow();
        }
        popped
    }

    pub fn move_by(&mut self, delta: isize) {
        self.found.move_selection(delta);
    }

    /// Keeps the skills that match the term, best first, and puts the cursor
    /// on the best.
    fn narrow(&mut self) {
        let Fetch::Ready(skills) = &self.skills else {
            return;
        };
        let matcher = SkimMatcherV2::default();
        let term = self.term.as_str();
        let mut found: Vec<(Fit, Found)> = skills
            .iter()
            .enumerate()
            .filter_map(|(at, skill)| {
                if term.is_empty() {
                    return Some(((false, false, 0), Found { at, hits: Vec::new() }));
                }
                let name = skill.name.as_str();
                if let Some((score, hits)) = matcher.fuzzy_indices(name, term) {
                    return Some(((name == term, true, score), Found { at, hits }));
                }
                let score = matcher.fuzzy_match(&skill.description, term)?;
                Some(((false, false, score), Found { at, hits: Vec::new() }))
            })
            .collect();
        found.sort_by(|(a, one), (b, other)| b.cmp(a).then(one.at.cmp(&other.at)));
        self.found.replace_at(found.into_iter().map(|(_, found)| found).collect(), Some(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, description: &str) -> Skill {
        let name = Name::new(name.to_owned()).expect("a name");
        Skill { name, description: description.to_owned() }
    }

    /// What `FIND` prints: the plugins, then where, the path and the
    /// frontmatter of each skill.
    fn printed(plugins: &str, skills: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut printed = format!("{plugins}\0");
        for (at, path, front) in skills {
            printed.extend([*at, "\0", path, "\0", front, "\0"]);
        }
        printed.into_bytes()
    }

    const PLUGINS: &str = r#"[
  {
    "id": "review@market",
    "scope": "user",
    "enabled": true,
    "installPath": "/c/plugins/review/1.0.0"
  },
  {
    "id": "off@market",
    "scope": "user",
    "enabled": false,
    "installPath": "/c/plugins/off/1.0.0"
  }
]"#;

    const REVIEW: &str = "/c/plugins/review/1.0.0";

    /// A skill is named for its directory and a command for its file, and a
    /// plugin's for its plugin too, while the plugin is on. The first of two
    /// skills of a name is the one nearest the directory, and one not for you
    /// to run is left out.
    #[test]
    fn the_finder_names_each_skill_as_a_slash_command_runs_it() {
        let stdout = printed(
            PLUGINS,
            &[
                ("", "skills/tigerstyle/SKILL.md", "name: tiger\ndescription: Apply TigerStyle."),
                ("", "skills/handoff/SKILL.md", "description: here"),
                ("", "commands/handoff.md", "description: yours"),
                ("", "skills/background/SKILL.md", "user-invocable: false"),
                ("", "commands/investigate.md", ""),
                (REVIEW, "skills/deep/SKILL.md", "description: Review deeply."),
                (REVIEW, "commands/quick.md", "description: Review quickly."),
                ("/c/plugins/off/1.0.0", "skills/gone/SKILL.md", "description: Off."),
            ],
        );
        assert_eq!(
            found(&stdout),
            [
                skill("handoff", "here"),
                skill("investigate", ""),
                skill("review:deep", "Review deeply."),
                skill("review:quick", "Review quickly."),
                skill("tigerstyle", "Apply TigerStyle."),
            ]
        );
        // `claude` missing, or saying what luneta cannot read: no plugin is
        // on, and your own skills are still found.
        let stdout = printed(
            "",
            &[(REVIEW, "skills/deep/SKILL.md", ""), ("", "skills/tdd/SKILL.md", "")],
        );
        assert_eq!(found(&stdout), [skill("tdd", "")]);
    }

    /// A skill named what no slash command runs is left out, rather than
    /// named some other way: the first word of a prompt is the skill it runs,
    /// so a name with a space in it would run another, and no name runs none.
    #[test]
    fn a_skill_no_slash_command_runs_is_left_out() {
        let stdout = printed(
            "[]",
            &[
                ("", "workflows/empty.js", "export const meta = {\n  name: '',\n}"),
                ("", "workflows/spaced.js", "export const meta = {\n  name: 'review pr',\n}"),
                ("", "workflows/tabbed.js", "export const meta = {\n  name: 'a\tb',\n}"),
                ("", "skills/two words/SKILL.md", ""),
                ("", "commands/one-word.md", ""),
            ],
        );
        assert_eq!(found(&stdout), [skill("one-word", "")]);
    }

    /// A plugin keeps its skills in `skills`, and where its manifest says
    /// besides: a skill's own directory, or one that holds skills, whether
    /// the manifest comes before them or after. A `SKILL.md` anywhere else in
    /// it is no skill of the plugin's.
    #[test]
    fn a_plugin_s_skills_are_where_its_manifest_says() {
        let stdout = printed(
            PLUGINS,
            &[
                (REVIEW, MANIFEST, r#"{"name": "review", "skills": ["./extra/own", "./many/"]}"#),
                (REVIEW, "skills/deep/SKILL.md", ""),
                (REVIEW, "extra/own/SKILL.md", ""),
                (REVIEW, "many/one/SKILL.md", ""),
                (REVIEW, "extra/other/SKILL.md", ""),
                (REVIEW, "docs/skills/example/SKILL.md", ""),
                (REVIEW, "SKILL.md", ""),
            ],
        );
        let names: Vec<String> =
            found(&stdout).iter().map(|skill| skill.name.to_string()).collect();
        assert_eq!(names, ["review:deep", "review:one", "review:own"]);

        let stdout = printed(
            PLUGINS,
            &[(REVIEW, "extra/own/SKILL.md", ""), (REVIEW, MANIFEST, r#"{"skills": "extra"}"#)],
        );
        assert_eq!(found(&stdout), [skill("review:own", "")]);
    }

    /// Output cut at 256 KiB ends with the last skill printed whole.
    #[test]
    fn output_cut_short_keeps_the_skills_printed_whole() {
        let stdout = printed(
            "[]",
            &[
                ("", "skills/one/SKILL.md", "description: 1"),
                ("", "skills/two/SKILL.md", "description: 2"),
            ],
        );
        // Each skill takes 36 bytes: no plugin's path, its path and its
        // frontmatter, each with its NUL.
        for (cut, names) in [
            (0, &["one", "two"][..]),
            (1, &["one"]),
            (36, &["one"]),
            (37, &[]),
            (stdout.len() - 1, &[]),
        ] {
            let skills = found(&stdout[..stdout.len() - cut]);
            let found: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
            assert_eq!(found, names, "cut {cut}");
        }
    }

    #[test]
    fn a_description_is_read_as_yaml_writes_it() {
        for (front, description) in [
            ("description: plain", "plain"),
            ("description: \"double \\\"quoted\\\"\"", "double \"quoted\""),
            ("description: 'it''s single'", "it's single"),
            ("description: >\n  folded over\n  two lines\nname: x", "folded over two lines"),
            ("description: |-\n  kept\n\n  apart", "kept apart"),
            ("description: runs\n  on\nname: x", "runs on"),
            ("description: a: colon", "a: colon"),
            ("name: x\r\ndescription: crlf\r\n", "crlf"),
            ("name: x", ""),
        ] {
            assert_eq!(Front::read(front).description, description, "{front:?}");
        }
        assert!(Front::read("user-invocable: true").invocable);
        assert!(!Front::read("user-invocable: false").invocable);
        let long = format!("description: {}", "é".repeat(DESCRIPTION_CHARS * 2));
        let skills = found(&printed("[]", &[("", "skills/long/SKILL.md", &long)]));
        let description = skills.first().map(|skill| skill.description.chars().count());
        assert_eq!(description, Some(DESCRIPTION_CHARS));
        let tabbed = found(&printed("[]", &[("", "skills/t/SKILL.md", "description: a\tb")]));
        assert_eq!(tabbed, [skill("t", "a b")]);
    }

    /// A workflow goes by the name its `meta` gives it, which Claude Code
    /// runs it by, and says what it does in a string of any quotes.
    #[test]
    fn a_workflow_goes_by_the_name_its_meta_gives_it() {
        let meta = r#"export const meta = {
  name: 'from-meta',
  description: "Builds \"it\",\nthen ships",
  phases: [{ title: 'Plan' }],
}"#;
        let stdout = printed(
            "[]",
            &[
                ("", "workflows/from-file.js", meta),
                ("", "workflows/nameless.js", "export const meta = {\n  description: `Tests`,\n}"),
                ("", "workflows/broken.js", "export const meta = {\n  name: 'unclosed,\n}"),
            ],
        );
        assert_eq!(
            found(&stdout),
            [
                skill("broken", ""),
                skill("from-meta", "Builds \"it\", then ships"),
                skill("nameless", "Tests"),
            ]
        );
    }

    /// What the finder finds in `LUNETA_SKILLS_DIR`, or in this directory,
    /// against the slash commands Claude Code says it has there, in the
    /// `init` that `claude -p` prints first. Each skill the finder names is
    /// one of them, and each of them is one the finder found, but for those
    /// Claude Code brings itself, which the menu leaves out: what it has with
    /// none of your settings, skills or plugins, in a directory of none, and
    /// the skills it brings from claude.ai, in the namespaces it keeps for
    /// them. The finder looks where Claude Code looks, as far as luneta knows,
    /// and only this says when a Claude Code looks elsewhere.
    #[test]
    #[ignore = "starts `claude`: run it with `dev skills [dir]`"]
    fn the_finder_finds_what_claude_code_runs() {
        use std::collections::BTreeSet;
        use std::path::Path;
        use std::process::{Command, Stdio};

        /// The slash commands of the `init` of `claude -p` in `dir`, with
        /// `args`, on a model that is none, so that nothing reaches one. Out
        /// of the panes claude-ps places agents in, and of a Claude Code
        /// session this may run in, but for where yours keeps its config.
        fn commands(dir: &Path, args: &[&str]) -> BTreeSet<String> {
            let mut claude = Command::new("claude");
            for (key, _) in std::env::vars() {
                let session = key.starts_with("CLAUDE") && key != "CLAUDE_CONFIG_DIR";
                if session || key.starts_with("ZELLIJ") {
                    claude.env_remove(key);
                }
            }
            let said = claude
                .current_dir(dir)
                .args(["-p", "x", "--output-format", "stream-json", "--verbose"])
                .args(["--model", "luneta-no-such-model"])
                .args(args)
                .stdin(Stdio::null())
                .output()
                .expect("claude");
            let init = String::from_utf8_lossy(&said.stdout)
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .find(|event| event["subtype"] == "init")
                .expect("claude -p said its init");
            let names = init["slash_commands"].as_array().into_iter().flatten();
            names.filter_map(serde_json::Value::as_str).map(str::to_owned).collect()
        }

        let dir = std::env::var("LUNETA_SKILLS_DIR").unwrap_or_else(|_| ".".to_owned());
        let finder = Command::new("sh").args(["-c", FIND, "sh", &dir]).output().expect("sh");
        let luneta: BTreeSet<String> =
            found(&finder.stdout).iter().map(|skill| skill.name.to_string()).collect();

        let none = std::env::temp_dir().join(format!("luneta-skills-{}", std::process::id()));
        std::fs::create_dir_all(&none).expect("a directory");
        let own = commands(&none, &["--setting-sources", "local"]);
        std::fs::remove_dir_all(&none).expect("the directory goes");

        let there = commands(Path::new(&dir), &[]);
        let synced = ["anthropic-skills:", "claude-ai:"];
        let brought =
            |name: &&String| own.contains(*name) || synced.iter().any(|at| name.starts_with(at));
        let finder_alone: Vec<&String> = luneta.difference(&there).collect();
        let claude_alone: Vec<&String> =
            there.difference(&luneta).filter(|name| !brought(name)).collect();
        assert!(
            finder_alone.is_empty() && claude_alone.is_empty(),
            "in {dir}, the finder alone finds {finder_alone:?}, and Claude Code alone runs \
             {claude_alone:?}"
        );
    }

    #[test]
    fn a_finder_that_failed_says_why() {
        assert_eq!(answer(Some(0), b"[]\0", b""), Fetch::Ready(Vec::new()));
        let failed = answer(Some(2), b"", b"sh: 1: awk: not found\nmore");
        assert_eq!(failed, Fetch::Failed("skills: sh: 1: awk: not found".to_owned()));
        assert_eq!(answer(None, b"", b""), Fetch::Failed("skills: the finder failed".to_owned()));
    }

    fn names(search: &Search) -> Vec<&str> {
        search.found().map(|(skill, _)| skill.name.as_str()).collect()
    }

    /// The skills that match what is typed come best first: a skill named
    /// the term, then the ones whose names match, then the ones only their
    /// descriptions match. Typing goes on while the finder looks, and the
    /// skills it finds are narrowed by what was typed by then.
    #[test]
    fn a_search_narrows_the_skills_to_the_best_match_first() {
        let mut search = Search::ask("/w", BTreeMap::new());
        search.push('r');
        search.push('e');
        assert_eq!(search.count(), 0);
        assert!(search.chosen().is_none());
        assert!(search.fill(Fetch::Ready(vec![
            skill("code-review", "Review the changes since a fixed point."),
            skill("prototype", "Build a throwaway prototype."),
            skill("re", "Exactly the term."),
            skill("tigerstyle", "Review code as TigerBeetle writes it."),
        ])));
        search.push('v');
        assert_eq!(names(&search), ["code-review", "tigerstyle"]);
        assert_eq!(search.chosen().map(|skill| skill.name.as_str()), Some("code-review"));
        let hits: Vec<&[usize]> = search.found().map(|(_, hits)| hits).collect();
        assert_eq!(hits, [&[5, 6, 7][..], &[]]);

        assert!(search.pop());
        assert_eq!(names(&search)[0], "re");
        search.move_by(1);
        assert_eq!(search.selected(), Some(1));
        assert!(search.pop());
        assert!(search.pop());
        assert!(!search.pop());
        assert_eq!(names(&search), ["code-review", "prototype", "re", "tigerstyle"]);

        // A second answer is for a box closed before the first came.
        assert!(!search.fill(Fetch::Ready(Vec::new())));
        assert_eq!(search.count(), 4);
    }
}
