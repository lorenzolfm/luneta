use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lune_domain::agent::Harness;
use serde::Deserialize;

use crate::{log, program, Directory};

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

const LIMIT: Duration = Duration::from_secs(15);

const MANIFEST: &str = ".claude-plugin/plugin.json";

const DESCRIPTION_CHARS: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: Name,
    pub description: String,
    pub runs: Runs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runs {
    ForAnyone,
    OnlyForYou,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(String);

impl Name {
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

fn env_dir(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub fn found_in(cwd: &Directory, harness: Harness) -> Result<Vec<Skill>, String> {
    match harness {
        Harness::ClaudeCode => claude_found_in(cwd),
        Harness::Pi => {
            let home = env_dir("HOME");
            let agent = env_dir("PI_CODING_AGENT_DIR").or_else(|| Some(home.as_ref()?.join(PI_AGENT)));
            Ok(pi_found(Path::new(cwd.as_str()), home.as_deref(), agent.as_deref()))
        },
        Harness::Antigravity => {
            let home = env_dir("HOME");
            Ok(agy_found(Path::new(cwd.as_str()), home.as_deref()))
        },
    }
}

fn claude_found_in(cwd: &Directory) -> Result<Vec<Skill>, String> {
    let mut finder = Command::new("sh");
    finder.args(["-c", FIND, "sh", cwd.as_str()]);
    program::run(&mut finder, LIMIT)
        .map_err(|error| format!("skills: the finder could not be run: {error}"))
        .and_then(|ran| ran.answer().map_err(|why| format!("skills: {why}")))
        .map(|said| found(&said))
}

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

fn listed(found: impl Iterator<Item = (String, Front)>) -> Vec<Skill> {
    let mut seen = HashSet::new();
    let mut skills = Vec::new();
    for (name, front) in found {
        let Some(name) = Name::new(name) else {
            continue;
        };
        if seen.insert(name.clone()) {
            let shown = |c: char| if c.is_control() { ' ' } else { c };
            let description = front.description.chars().map(shown).take(DESCRIPTION_CHARS);
            skills.push(Skill { name, description: description.collect(), runs: front.runs });
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

const PI_AGENT: &str = ".pi/agent";

const PI_DEPTH: usize = 6;

const FRONT_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kept {
    Pi,
    Agents,
}

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
        let mut front = Front::read(&frontmatter(&file).ok().flatten()?);
        if front.description.trim().is_empty() {
            return None;
        }
        let folder = file.parent()?.file_name()?.to_str()?;
        let name = front.name.take().filter(|name| !name.is_empty());
        Some((name.unwrap_or_else(|| folder.to_owned()), front))
    });
    listed(found)
}

const AGY_ROOTS: [&str; 4] = [".agents", ".agent", "_agents", "_agent"];

const AGY_PLUGINS_MAX: usize = 256;

const AGY_SKILLS_PER_ROOT_MAX: usize = 512;

const AGY_SKILL_FILES_MAX: usize = 2048;

const AGY_DIR_ENTRIES_MAX: usize = 4096;

const _: () = assert!(AGY_PLUGINS_MAX > 0);
const _: () = assert!(AGY_SKILLS_PER_ROOT_MAX > 0);
const _: () = assert!(AGY_SKILLS_PER_ROOT_MAX <= AGY_SKILL_FILES_MAX);
const _: () = assert!(AGY_SKILLS_PER_ROOT_MAX <= AGY_DIR_ENTRIES_MAX);
const _: () = assert!(AGY_PLUGINS_MAX <= AGY_DIR_ENTRIES_MAX);

fn push_unique_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        paths.push(path);
    }
}

fn sorted_dir_entries(
    dir: &Path,
    limit: usize,
    keep: impl Fn(&str, &Path) -> bool,
) -> Vec<(String, PathBuf)> {
    assert!(limit > 0);
    let listed = match fs::read_dir(dir) {
        Ok(listed) => listed,
        Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            log::say(&format!("skills in {}: {error}", dir.display()));
            return Vec::new();
        },
    };
    let mut entries = Vec::new();
    for (seen, entry) in listed.enumerate() {
        if seen >= AGY_DIR_ENTRIES_MAX {
            log::say(&format!(
                "skills in {}: more than {AGY_DIR_ENTRIES_MAX} directory entries",
                dir.display()
            ));
            break;
        }
        match entry {
            Ok(entry) => {
                if let Ok(name) = entry.file_name().into_string() {
                    let path = entry.path();
                    if keep(&name, &path) {
                        entries.push((name, path));
                    }
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => log::say(&format!("skills in {}: {error}", dir.display())),
        }
    }
    entries.sort();
    entries.truncate(limit);
    assert!(entries.len() <= limit);
    entries
}

fn agy_found(cwd: &Path, home: Option<&Path>) -> Vec<Skill> {
    assert!(cwd.is_absolute());
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut seen_roots: HashSet<PathBuf> = HashSet::new();
    let mut plugin_roots: Vec<PathBuf> = Vec::new();
    let mut seen_plugins: HashSet<PathBuf> = HashSet::new();
    if let Some(home) = home {
        push_unique_path(&mut roots, &mut seen_roots, home.join(".gemini/antigravity-cli/skills"));
        push_unique_path(&mut roots, &mut seen_roots, home.join(".gemini/skills"));
    }
    let top = cwd.ancestors().find(|dir| dir.join(".git").exists());
    for dir in cwd.ancestors() {
        for name in AGY_ROOTS {
            let base = dir.join(name);
            push_unique_path(&mut roots, &mut seen_roots, base.join("skills"));
            push_unique_path(&mut plugin_roots, &mut seen_plugins, base.join("plugins"));
        }
        if top.is_none() || Some(dir) == top {
            break;
        }
    }
    if let Some(home) = home {
        push_unique_path(&mut roots, &mut seen_roots, home.join(".gemini/config/skills"));
        push_unique_path(&mut roots, &mut seen_roots, home.join(".agents/skills"));
        push_unique_path(&mut plugin_roots, &mut seen_plugins, home.join(".gemini/config/plugins"));
    }
    for plugins in &plugin_roots {
        for skills in agy_plugin_skills(plugins) {
            push_unique_path(&mut roots, &mut seen_roots, skills);
        }
    }
    let mut files = Vec::new();
    for root in &roots {
        let remaining = AGY_SKILL_FILES_MAX - files.len();
        if remaining == 0 {
            break;
        }
        files.extend(agy_skill_files(root, remaining.min(AGY_SKILLS_PER_ROOT_MAX)));
    }
    assert!(files.len() <= AGY_SKILL_FILES_MAX);
    let found = files.into_iter().filter_map(|file| {
        let mut front = match frontmatter(&file) {
            Ok(Some(text)) => Front::read(&text),
            Ok(None) => Front::default(),
            Err(error) => {
                if error.kind() != ErrorKind::NotFound {
                    log::say(&format!("skills in {}: {error}", file.display()));
                }
                return None;
            },
        };
        let folder = file.parent()?.file_name()?.to_str()?;
        let name = front.name.take().filter(|name| !name.is_empty());
        Some((name.unwrap_or_else(|| folder.to_owned()), front))
    });
    listed(found.filter(|(_, front)| front.invocable))
}

fn agy_plugin_skills(plugins_dir: &Path) -> Vec<PathBuf> {
    let entries = sorted_dir_entries(plugins_dir, AGY_PLUGINS_MAX, |name, path| {
        !name.starts_with('.') && path.is_dir() && path.join("skills").is_dir()
    });
    let roots: Vec<PathBuf> = entries.into_iter().map(|(_, path)| path.join("skills")).collect();
    assert!(roots.len() <= AGY_PLUGINS_MAX);
    roots
}

fn agy_skill_files(dir: &Path, limit: usize) -> Vec<PathBuf> {
    assert!(limit > 0);
    let entries = sorted_dir_entries(dir, limit, |name, path| {
        !name.starts_with('.')
            && name != "node_modules"
            && path.is_dir()
            && path.join("SKILL.md").is_file()
    });
    let files: Vec<PathBuf> = entries.into_iter().map(|(_, path)| path.join("SKILL.md")).collect();
    assert!(files.len() <= limit);
    files
}

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

fn frontmatter(file: &Path) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    File::open(file)?.take(FRONT_BYTES).read_to_end(&mut bytes)?;
    let truncated = u64::try_from(bytes.len()).is_ok_and(|len| len == FRONT_BYTES);
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])
                .map_err(|utf8| io::Error::new(ErrorKind::InvalidData, utf8))?
        },
        Err(error) => return Err(io::Error::new(ErrorKind::InvalidData, error)),
    };
    let mut lines = text.lines();
    if lines.next().is_none_or(|line| line.trim_end() != "---") {
        return Ok(None);
    }
    let front: Vec<&str> =
        lines.take(64).take_while(|line| line.trim_end() != "---").collect();
    Ok(Some(front.join("\n")))
}

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

fn command(path: &str) -> Option<&str> {
    path.strip_prefix("commands/")?.strip_suffix(".md").filter(|name| one_name(name))
}

fn one_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/')
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Installed {
    id: String,
    enabled: bool,
    install_path: String,
}

struct Plugin {
    name: String,
    keeps: Vec<String>,
}

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

fn bare(path: &str) -> &str {
    let mut path = path.trim_end_matches('/');
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    path
}

struct Front {
    name: Option<String>,
    description: String,
    invocable: bool,
    runs: Runs,
}

impl Default for Front {
    fn default() -> Self {
        Front { name: None, description: String::new(), invocable: true, runs: Runs::ForAnyone }
    }
}

impl Front {
    fn read(front: &str) -> Self {
        let mut read = Front::default();
        let mut user_invocable: Option<bool> = None;
        let mut disable_slash = false;
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
                "user-invocable" => {
                    user_invocable = Some(!unquote(&parts.join(" ")).eq_ignore_ascii_case("false"));
                },
                "disable-slash-command" if truthy(&unquote(&parts.join(" "))) => {
                    disable_slash = true;
                },
                "disable-model-invocation" if truthy(&unquote(&parts.join(" "))) => {
                    read.runs = Runs::OnlyForYou;
                },
                _ => {},
            }
        }
        read.invocable = !disable_slash && user_invocable.unwrap_or(true);
        read
    }
}

fn truthy(value: &str) -> bool {
    ["1", "true", "yes", "on"].contains(&value.to_ascii_lowercase().as_str())
}

fn meta(meta: &str, key: &str) -> Option<String> {
    meta.lines().find_map(|line| {
        let value = line.trim_start().strip_prefix(key)?.trim_start().strip_prefix(':')?;
        literal(value.trim_start())
    })
}

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
        let stdout = printed(
            "",
            &[(REVIEW, "skills/deep/SKILL.md", ""), ("", "skills/tdd/SKILL.md", "")],
        );
        assert_eq!(found(&stdout), [skill("tdd", "")]);
    }

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

    fn put(root: &Path, file: &str, text: &str) {
        let path = root.join(file);
        let made = path.parent().map_or(Ok(()), std::fs::create_dir_all);
        assert!(made.and_then(|()| std::fs::write(&path, text)).is_ok(), "{file}");
    }

    fn only_you(name: &str, description: &str) -> Skill {
        Skill { runs: Runs::OnlyForYou, ..skill(name, description) }
    }

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
    fn the_finder_finds_what_antigravity_runs_where_it_finds_it() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let root = scratch.path();
        let home = root.join("home");
        let front = |description: &str| format!("---\ndescription: {description}\n---\nDo it.\n");
        put(root, "outside/.agents/skills/far/SKILL.md", &front("Above the repository."));
        put(root, "outside/repo/.git/HEAD", "");
        put(root, "outside/repo/.agents/skills/top/SKILL.md", &front("At the top."));
        put(root, "outside/repo/.agents/skills/deep/nested/SKILL.md", &front("Too deep."));
        put(root, "outside/repo/inner/.agent/skills/near/SKILL.md", &front("Near."));
        put(
            root,
            "outside/repo/inner/.agents/skills/hidden-cmd/SKILL.md",
            "---\ndescription: Hidden.\ndisable-slash-command: true\n---\n",
        );
        put(root, "home/.gemini/antigravity-cli/skills/near/SKILL.md", &front("CLI wins."));
        put(root, "home/.gemini/skills/shared/SKILL.md", &front("Shared."));
        put(root, "home/.gemini/config/skills/global/SKILL.md", &front("Global."));
        put(root, "home/.agents/skills/user-agent/SKILL.md", &front("Home agents."));
        put(
            root,
            "home/.gemini/config/plugins/review/skills/deep-review/SKILL.md",
            "---\nname: deep-review\ndescription: Plugin review.\n---\n",
        );
        let cwd = root.join("outside/repo/inner");
        assert_eq!(agy_found(&cwd, Some(&home)), [
            skill("deep-review", "Plugin review."),
            skill("global", "Global."),
            skill("near", "CLI wins."),
            skill("shared", "Shared."),
            skill("top", "At the top."),
            skill("user-agent", "Home agents."),
        ]);
        put(root, "plain/parent/.agents/skills/above/SKILL.md", &front("Above."));
        put(root, "plain/parent/child/.agents/skills/here/SKILL.md", &front("Here."));
        assert_eq!(agy_found(&root.join("plain/parent/child"), None), [skill("here", "Here.")]);

        put(root, "home/.git/HEAD", "");
        assert_eq!(agy_found(&home, Some(&home)), [
            skill("deep-review", "Plugin review."),
            skill("global", "Global."),
            skill("near", "CLI wins."),
            skill("shared", "Shared."),
            skill("user-agent", "Home agents."),
        ]);

        let invalid_dir = root.join("plain/parent/child/.agents/skills/corrupt");
        fs::create_dir_all(&invalid_dir).unwrap();
        fs::write(invalid_dir.join("SKILL.md"), [0xff, 0xfe, 0x00]).unwrap();
        put(root, "plain/parent/child/.agents/skills/bare/SKILL.md", "No frontmatter.\n");
        let header = "---\ndescription: Long UTF-8.\n---\n";
        let pad = usize::try_from(FRONT_BYTES).unwrap() - header.len() - 1;
        let long_utf8 = format!("{header}{}é\n", "a".repeat(pad));
        put(root, "plain/parent/child/.agents/skills/long/SKILL.md", &long_utf8);
        assert_eq!(agy_found(&root.join("plain/parent/child"), None), [
            skill("bare", ""),
            skill("here", "Here."),
            skill("long", "Long UTF-8."),
        ]);
        let filtered = root.join("filtered");
        put(&filtered, ".DS_Store", "");
        put(&filtered, ".hidden/SKILL.md", &front("Hidden."));
        fs::create_dir_all(filtered.join("aaa-empty")).unwrap();
        put(&filtered, "node_modules/dep/SKILL.md", &front("Dep."));
        put(&filtered, "real/SKILL.md", &front("Real."));
        assert_eq!(agy_skill_files(&filtered, 1), [filtered.join("real/SKILL.md")]);
        assert!(!Front::read("disable-slash-command: true\nuser-invocable: true").invocable);
        assert!(!Front::read("user-invocable: False").invocable);
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

    #[test]
    #[ignore = "starts `claude`: run it with `dev skills [dir]`"]
    fn the_finder_finds_what_claude_code_runs() {
        use std::collections::BTreeSet;
        use std::path::Path;
        use std::process::Stdio;

        fn commands(dir: &Path, args: &[&str]) -> BTreeSet<String> {
            let mut claude = Command::new("claude");
            for (key, _) in std::env::vars() {
                let session = key.starts_with("CLAUDE") && key != "CLAUDE_CONFIG_DIR";
                if session || key.starts_with("LUNE") {
                    claude.env_remove(key);
                }
            }
            let said = claude
                .current_dir(dir)
                .args(["-p", "x", "--output-format", "stream-json", "--verbose"])
                .args(["--model", "lune-no-such-model"])
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

        let dir = std::env::var("LUNE_SKILLS_DIR").unwrap_or_else(|_| ".".to_owned());
        let finder = Command::new("sh").args(["-c", FIND, "sh", &dir]).output().expect("sh");
        let lune: BTreeSet<String> =
            found(&finder.stdout).iter().map(|skill| skill.name.to_string()).collect();

        let none = tempfile::tempdir().expect("a directory");
        let own = commands(none.path(), &["--setting-sources", "local"]);

        let there = commands(Path::new(&dir), &[]);
        let synced = ["anthropic-skills:", "claude-ai:"];
        let brought =
            |name: &&String| own.contains(*name) || synced.iter().any(|at| name.starts_with(at));
        let finder_alone: Vec<&String> = lune.difference(&there).collect();
        let claude_alone: Vec<&String> =
            there.difference(&lune).filter(|name| !brought(name)).collect();
        assert!(
            finder_alone.is_empty() && claude_alone.is_empty(),
            "in {dir}, the finder alone finds {finder_alone:?}, and Claude Code alone runs \
             {claude_alone:?}"
        );
    }
}
