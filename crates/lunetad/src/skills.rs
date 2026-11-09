//! The skills a pane's directory can run, for the menu's search, in a
//! harness: for Claude Code, each a `SKILL.md`, a command or a workflow, where
//! it finds them for that directory, and for pi, each a `SKILL.md` or a `.md`
//! where pi finds them.
//!
//! Claude Code names the skills it would run only as it starts a session:
//! `claude -p` with `--output-format stream-json` says them in the `init` it
//! prints first. That is a session's start for each search, with your hooks
//! and MCP servers, and a prompt that must not reach the model, for their names
//! alone: not what each does, which the search goes by too. So the finder
//! looks where Claude Code looks, and asks `claude plugin list`, which takes a
//! moment and starts no session, which plugins are on in that directory.
//! pi names its skills only in a session too, and has no plugins: the finder
//! looks where pi looks, which takes no program.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::{program, Harness};

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
    done"#;

/// How long the finder has to say what it found: `claude plugin list` takes
/// a second or two, and the rest no time.
const LIMIT: Duration = Duration::from_secs(15);

/// Where a plugin's manifest is in it.
const MANIFEST: &str = ".claude-plugin/plugin.json";

/// How long a description the menu keeps, in characters: it shows a line of
/// one at most, and the search goes by it.
const DESCRIPTION_CHARS: usize = 500;

/// A skill the directory can run, as a prompt runs it: its name, after the
/// `/`, what its frontmatter says it does, on one line, and who can run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: Name,
    pub description: String,
    pub runs: Runs,
}

/// Who can run a skill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runs {
    /// You, by its slash command, and an agent, told its name.
    ForAnyone,
    /// You alone: its frontmatter says `disable-model-invocation: true`, and
    /// an agent told its name cannot run it.
    OnlyForYou,
}

/// The name a skill runs by, after its `/`: one word, as the first word of a
/// prompt is the skill it runs. Claude Code would read a name with a space in
/// it as another skill, and the rest as what that one is asked.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(String);

impl Name {
    /// `name`, if a prompt can run it: a word, with no space or control
    /// character in it.
    #[must_use]
    pub fn new(name: String) -> Option<Self> {
        let spaced = |c: char| c.is_whitespace() || c.is_control();
        (!name.is_empty() && !name.contains(spaced)).then_some(Name(name))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The skills `cwd` can run in `harness`, as the finder finds them on this
/// machine, or why it found none.
pub fn found_in(cwd: &str, harness: Harness) -> Result<Vec<Skill>, String> {
    match harness {
        Harness::ClaudeCode => claude_found_in(cwd),
        Harness::Pi => {
            let set = |name: &str| env::var_os(name).map(PathBuf::from);
            let home = set("HOME");
            let agent = set("PI_CODING_AGENT_DIR").or_else(|| Some(home.as_ref()?.join(PI_AGENT)));
            Ok(pi_found(Path::new(cwd), home.as_deref(), agent.as_deref()))
        },
    }
}

/// The skills `cwd` can run in Claude Code, as `FIND` finds them, or why it
/// found none.
fn claude_found_in(cwd: &str) -> Result<Vec<Skill>, String> {
    let mut finder = Command::new("sh");
    finder.args(["-c", FIND, "sh", cwd]);
    program::run(&mut finder, LIMIT)
        .map_err(|error| format!("skills: the finder could not be run: {error}"))
        .and_then(|ran| ran.answer().map_err(|why| format!("skills: {why}")))
        .map(|said| found(&said))
}

/// What `FIND` printed, as the skills you can run, by name. A plugin's skill
/// runs as `plugin:skill`, and only while `claude plugin list` says the
/// plugin is on. A skill found twice is the first found: the one nearest the
/// directory. One whose frontmatter says it is not for you to run is left
/// out, and so is one named what no slash command runs. A plugin's skills are
/// named once all of its manifest is read, wherever it was printed. Output
/// cut short ends with the last skill printed whole.
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
    let found = files.into_iter().filter_map(|(at, path, text)| match (at, plugins.get(at)) {
        ("", _) => own(path, text),
        (_, Some(plugin)) => plugin.name(path).map(|name| (name, Front::read(text))),
        (_, None) => None,
    });
    listed(found.filter(|(_, front)| front.invocable))
}

/// The skills `found`, by name, in the order found: a skill found twice is
/// the first found. One named what no slash command runs is left out.
fn listed(found: impl Iterator<Item = (String, Front)>) -> Vec<Skill> {
    let mut seen = HashSet::new();
    let mut skills = Vec::new();
    for (name, front) in found {
        let Some(name) = Name::new(name) else {
            continue;
        };
        if seen.insert(name.clone()) {
            // A tab, say, has no width to draw, and would throw the line off.
            let shown = |c: char| if c.is_control() { ' ' } else { c };
            let description = front.description.chars().map(shown).take(DESCRIPTION_CHARS);
            skills.push(Skill { name, description: description.collect(), runs: front.runs });
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// pi's own folder in your home, where `PI_CODING_AGENT_DIR` does not name
/// another, which holds your skills in `skills`.
const PI_AGENT: &str = ".pi/agent";

/// How many folders deep the finder looks for pi's skills under where pi
/// keeps them, as `FIND` looks in a plugin.
const PI_DEPTH: usize = 6;

/// The most bytes of a file the finder reads for its frontmatter.
const FRONT_BYTES: u64 = 64 * 1024;

/// Where pi finds skills: in a folder of its own, as `.pi/skills`, where a
/// `.md` at the top is a skill too, or in an Agent Skills folder, as
/// `.agents/skills`, where one below the top is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kept {
    Pi,
    Agents,
}

/// The skills `cwd` can run in pi, where pi 1.5 finds them, nearest first: in
/// `.pi/skills` of the directory, in `.agents/skills` of it and of each one
/// above it up to its repository's top, or up to `/` outside of one, in
/// `skills` of `agent`, pi's own folder of yours, and in `.agents/skills` of
/// `home`. A folder with a `SKILL.md` is a skill, named for the folder, and
/// holds no other. A skill goes by the name its frontmatter gives it, or else
/// by its folder's, and one that says nothing of what it does is none. pi
/// loads the directory's own only once you trust it, which it asks as it
/// starts. The skills your settings or pi's packages name are not where the
/// finder looks, nor does it leave out what an ignore file does.
fn pi_found(cwd: &Path, home: Option<&Path>, agent: Option<&Path>) -> Vec<Skill> {
    let ours = home.map(|home| home.join(".agents").join("skills"));
    let mut places = vec![(cwd.join(".pi").join("skills"), Kept::Pi)];
    let top = cwd.ancestors().find(|dir| dir.join(".git").exists());
    for dir in cwd.ancestors() {
        let skills = dir.join(".agents").join("skills");
        if Some(&skills) != ours.as_ref() {
            places.push((skills, Kept::Agents));
        }
        if Some(dir) == top {
            break;
        }
    }
    places.extend(agent.map(|agent| (agent.join("skills"), Kept::Pi)));
    places.extend(ours.map(|ours| (ours, Kept::Agents)));
    let mut files = Vec::new();
    for (root, kept) in &places {
        skill_files(root, root, *kept, 0, &mut files);
    }
    let found = files.into_iter().filter_map(|file| {
        let mut front = Front::read(&frontmatter(&file)?);
        if front.description.trim().is_empty() {
            return None;
        }
        let folder = file.parent()?.file_name()?.to_str()?;
        let name = front.name.take().filter(|name| !name.is_empty());
        Some((name.unwrap_or_else(|| folder.to_owned()), front))
    });
    listed(found)
}

/// Adds the skills in `dir`, of `root`, `depth` folders under it, to `files`,
/// as pi finds them: its `SKILL.md` alone where it has one, or else a `.md`
/// where `kept` takes one at that depth, and the skills of each folder in it
/// but those whose names start with a `.` and `node_modules`. A link is what
/// it links to. In order of their names, as a folder lists them in no order.
fn skill_files(dir: &Path, root: &Path, kept: Kept, depth: usize, files: &mut Vec<PathBuf>) {
    let Ok(listed) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(String, PathBuf)> = listed
        .filter_map(Result::ok)
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .collect();
    entries.sort();
    let skill = dir.join("SKILL.md");
    if skill.is_file() {
        files.push(skill);
        return;
    }
    let top = dir == root;
    for (name, path) in entries {
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let md = (kept == Kept::Pi) == top;
        if md && path.extension().is_some_and(|extension| extension == "md") && path.is_file() {
            files.push(path);
        } else if depth < PI_DEPTH && path.is_dir() {
            skill_files(&path, root, kept, depth + 1, files);
        }
    }
}

/// The frontmatter `file` starts with, as `FIND` prints one: up to its 64th
/// line. None for a file with none, or that cannot be read.
fn frontmatter(file: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(file).ok()?.take(FRONT_BYTES).read_to_string(&mut text).ok()?;
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let front: Vec<&str> =
        lines.take(64).take_while(|line| line.trim_end() != "---").collect();
    Some(front.join("\n"))
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
        let front = Front { name: None, description, invocable: true, runs: Runs::ForAnyone };
        return Some((name, front));
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
/// and so does a list luneta cannot read: Claude Code changed how it lists
/// them, and the finder must follow.
fn plugins_on(listed: &str) -> HashMap<String, Plugin> {
    let Ok(installed) = serde_json::from_str::<Vec<Installed>>(listed) else {
        return HashMap::new();
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
    /// The name it gives, which pi goes by, and Claude Code does not.
    name: Option<String>,
    description: String,
    /// False for a skill that says `user-invocable: false`: Claude Code runs
    /// it, but no slash command does.
    invocable: bool,
    runs: Runs,
}

impl Front {
    /// The YAML of a frontmatter, read as far as the menu needs: a key's
    /// value on its line, quoted or not, and the lines indented under it, as
    /// a block scalar or a plain one runs on, joined by spaces.
    fn read(front: &str) -> Self {
        let mut read = Front {
            name: None,
            description: String::new(),
            invocable: true,
            runs: Runs::ForAnyone,
        };
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
                "name" => read.name = Some(unquote(&parts.join(" "))),
                "description" => read.description = unquote(&parts.join(" ")),
                "user-invocable" => read.invocable = unquote(&parts.join(" ")) != "false",
                "disable-model-invocation" if truthy(&unquote(&parts.join(" "))) => {
                    read.runs = Runs::OnlyForYou;
                },
                _ => {},
            }
        }
        read
    }
}

/// Whether Claude Code takes a frontmatter's value for true.
fn truthy(value: &str) -> bool {
    ["1", "true", "yes", "on"].contains(&value.to_ascii_lowercase().as_str())
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

#[cfg(test)]
mod tests {
    use super::*;


    fn skill(name: &str, description: &str) -> Skill {
        let name = Name::new(name.to_owned()).expect("a name");
        Skill { name, description: description.to_owned(), runs: Runs::ForAnyone }
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
        for front in ["disable-model-invocation: true", "disable-model-invocation: \"Yes\""] {
            assert_eq!(Front::read(front).runs, Runs::OnlyForYou, "{front:?}");
        }
        for front in ["disable-model-invocation: false", "name: x"] {
            assert_eq!(Front::read(front).runs, Runs::ForAnyone, "{front:?}");
        }
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

    /// The finder runs on a directory of skills of its own: yours are left
    /// out by a `CLAUDE_CONFIG_DIR` with none, and the plugins by a `PATH`
    /// with no `claude` on it.
    /// Writes `text` to `file` in `root`, and the folders it is in.
    fn put(root: &Path, file: &str, text: &str) {
        let path = root.join(file);
        let made = path.parent().map_or(Ok(()), std::fs::create_dir_all);
        assert!(made.and_then(|()| std::fs::write(&path, text)).is_ok(), "{file}");
    }

    fn only_you(name: &str, description: &str) -> Skill {
        Skill { runs: Runs::OnlyForYou, ..skill(name, description) }
    }

    /// pi's skills are where pi finds them: the directory's own `.pi/skills`,
    /// `.agents/skills` of it and of each one above it up to its repository's
    /// top, yours in pi's folder and in `~/.agents/skills`, the nearest of a
    /// name first. A folder with a `SKILL.md` holds no other skill; a `.md` is
    /// a skill at the top of pi's folders and below the top of the Agent
    /// Skills ones, named for the folder it is in, as pi names it. A skill
    /// goes by the name its frontmatter gives first, and one that says
    /// nothing of what it does is none, nor is what is in a folder
    /// whose name starts with a `.`, or in `node_modules`.
    #[test]
    fn the_finder_finds_what_pi_runs_where_pi_finds_it() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let root = scratch.path();
        let (home, agent) = (root.join("home"), root.join("home/.pi/agent"));
        let front = |description: &str| format!("---\ndescription: {description}\n---\nDo it.\n");
        put(root, "outside/.agents/skills/far/SKILL.md", &front("Above the repository."));
        put(root, "outside/repo/.git/HEAD", "");
        put(root, "outside/repo/.agents/skills/top/SKILL.md", &front("At the top."));
        put(root, "outside/repo/.agents/skills/group/nested.md", &front("Below the top."));
        put(root, "outside/repo/.agents/skills/flat.md", &front("At the top of Agent Skills."));
        put(root, "outside/repo/inner/.pi/skills/near/SKILL.md", &front("Near."));
        put(root, "outside/repo/inner/.pi/skills/near/more/SKILL.md", &front("Held by near."));
        put(root, "outside/repo/inner/.pi/skills/loose.md", &front("At the top of pi's."));
        put(root, "outside/repo/inner/.agents/skills/near/SKILL.md", &front("Farther."));
        let named = "---\nname: renamed\ndescription: Named.\n---\n";
        put(root, "home/.pi/agent/skills/named/SKILL.md", named);
        let mine = "---\ndescription: Mine.\ndisable-model-invocation: true\n---\n";
        put(root, "home/.pi/agent/skills/mine/SKILL.md", mine);
        put(root, "home/.pi/agent/skills/mute/SKILL.md", "---\nname: mute\n---\n");
        put(root, "home/.pi/agent/skills/bare/SKILL.md", "No frontmatter.\n");
        put(root, "home/.pi/agent/skills/.hidden/SKILL.md", &front("Hidden."));
        put(root, "home/.pi/agent/skills/node_modules/dep/SKILL.md", &front("A dependency."));
        put(root, "home/.agents/skills/shared/SKILL.md", &front("Shared."));
        let cwd = root.join("outside/repo/inner");
        assert_eq!(pi_found(&cwd, Some(&home), Some(&agent)), [
            skill("group", "Below the top."),
            only_you("mine", "Mine."),
            skill("near", "Near."),
            skill("renamed", "Named."),
            skill("shared", "Shared."),
            skill("skills", "At the top of pi's."),
            skill("top", "At the top."),
        ]);
    }

    /// Outside of a repository, the finder looks up to `/`; without a home,
    /// only where the directory is.
    #[test]
    fn outside_a_repository_pi_s_skills_are_found_up_to_the_root() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let root = scratch.path();
        put(root, "a/.agents/skills/up/SKILL.md", "---\ndescription: Up.\n---\n");
        let found = pi_found(&root.join("a/b/c"), None, None);
        assert!(found.contains(&skill("up", "Up.")), "{found:?}");
    }

    #[test]
    fn the_finder_finds_the_skills_of_a_directory_and_those_above_it() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let root = scratch.path();
        let inner = root.join("repo").join("inner");
        let skill_dir = root.join("repo").join(".claude").join("skills").join("near");
        let made = [std::fs::create_dir_all(&inner), std::fs::create_dir_all(&skill_dir)];
        assert!(made.iter().all(Result::is_ok));
        let wrote = std::fs::write(skill_dir.join("SKILL.md"), "---\ndescription: Near.\n---\n");
        assert!(wrote.is_ok());
        let path = std::env::var_os("PATH").unwrap_or_default();
        let unclaude = std::env::split_paths(&path).filter(|dir| !dir.join("claude").exists());
        let Ok(path) = std::env::join_paths(unclaude) else {
            panic!("a PATH");
        };
        let said = Command::new("/bin/sh")
            .args(["-c", FIND, "sh"])
            .arg(&inner)
            .env("CLAUDE_CONFIG_DIR", root.join("none"))
            .env("PATH", path)
            .output();
        let Ok(said) = said else {
            panic!("sh");
        };
        assert_eq!(found(&said.stdout), [skill("near", "Near.")]);
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
        use std::process::Stdio;

        /// The slash commands of the `init` of `claude -p` in `dir`, with
        /// `args`, on a model that is none, so that nothing reaches one. Out
        /// of the panes luneta places agents in, and of a Claude Code
        /// session this may run in, but for where yours keeps its config.
        fn commands(dir: &Path, args: &[&str]) -> BTreeSet<String> {
            let mut claude = Command::new("claude");
            for (key, _) in std::env::vars() {
                let session = key.starts_with("CLAUDE") && key != "CLAUDE_CONFIG_DIR";
                if session || key.starts_with("ZELLIJ") || key.starts_with("LUNETA") {
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

        let none = tempfile::tempdir().expect("a directory");
        let own = commands(none.path(), &["--setting-sources", "local"]);

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
}
