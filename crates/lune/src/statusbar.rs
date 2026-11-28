use std::num::NonZeroU64;
use std::ops::Range;
use std::time::{Duration, SystemTime};

use lune_protocol::git::Changed;
use lune_protocol::github::Verdict;
use lune_protocol::telemetry::{
    AgentTelemetry, AgentUsage, ContextWindow, TelemetryText, UsedPermille,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr;

use crate::agents;
use crate::cadence::Cadence;
use crate::icons;
use crate::moonlight;
use crate::pull::Link;
use crate::sidebar::{ACCENT, ERROR, Mark, SpinnerFrame};
use crate::spots::Spots;

pub const ROWS: u16 = 1;

const MOON: &str = "☾";
const NEW_DOT: &str = "•";
const SEP: &str = " › ";
const DOT_SEP: &str = " · ";
const RULE: &str = "─";

const WARN_PERMILLE: u16 = 500;
const HIGH_PERMILLE: u16 = 800;

const MIN_RULE_GAP: usize = 3;
const VIEW_LABEL_SOFT_MIN: usize = 12;

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 3600;
const MINUTES_PER_HOUR: u64 = 60;
const MINUTES_PER_DAY: u64 = 24 * MINUTES_PER_HOUR;
const HOURS_PER_DAY: u64 = 24;

const _: () = assert!(ROWS == 1);
const _: () = assert!(WARN_PERMILLE < HIGH_PERMILLE);
const _: () = assert!(HIGH_PERMILLE <= 1000);
const _: () = assert!(MIN_RULE_GAP >= 3);
const _: () = assert!(VIEW_LABEL_SOFT_MIN >= 4);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollapsedTallies {
    pub waiting: usize,
    pub unread: usize,
    pub busy: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectInfo {
    pub name: String,
    pub colour: Color,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestInfo {
    pub label: String,
    pub link: Option<Link>,
    pub colour: Color,
    pub verdict: Option<Verdict>,
    pub conflicting: bool,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub name: String,
    pub branch: Option<String>,
    pub git_status: String,
    pub changed: Option<Changed>,
    pub pull_request: Option<PullRequestInfo>,
    pub missing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewInfo {
    pub mark: Option<Mark>,
    pub icon: Option<&'static str>,
    pub label: String,
    pub harness: Option<agents::Harness>,
    pub telemetry: Option<AgentTelemetry>,
}

impl ViewInfo {
    #[must_use]
    pub fn plain(label: String) -> Self {
        Self {
            mark: None,
            icon: None,
            label,
            harness: None,
            telemetry: None,
        }
    }

    #[must_use]
    pub fn labeled(icon: Option<&'static str>, label: String) -> Self {
        Self {
            mark: None,
            icon,
            label,
            harness: None,
            telemetry: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub sidebar_focused: bool,
    pub sidebar_collapsed: bool,
    pub away: bool,
    pub tallies: CollapsedTallies,
    pub machine: Option<String>,
    pub project: Option<ProjectInfo>,
    pub worktree: Option<WorktreeInfo>,
    pub view: Option<ViewInfo>,
    pub fallback_usage: Option<(agents::Harness, AgentUsage)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FitTier {
    Full,
    Compact,
    Trimmed,
    NoView,
    NoGitDetails,
    NoProject,
    NoWorktree,
    NoModel,
    ContextOnly,
    Minimal,
}

impl FitTier {
    const DEGRADED: [Self; 9] = [
        Self::Compact,
        Self::Trimmed,
        Self::NoView,
        Self::NoGitDetails,
        Self::NoProject,
        Self::NoWorktree,
        Self::NoModel,
        Self::ContextOnly,
        Self::Minimal,
    ];

    const fn show_tallies(self) -> bool {
        (self as u8) <= (Self::NoModel as u8)
    }

    const fn show_model(self) -> bool {
        (self as u8) <= (Self::NoWorktree as u8)
    }

    const fn show_effort(self) -> bool {
        (self as u8) <= (Self::NoView as u8)
    }

    const fn show_project(self) -> bool {
        (self as u8) <= (Self::NoGitDetails as u8)
    }

    const fn show_worktree(self) -> bool {
        (self as u8) <= (Self::NoProject as u8)
    }

    const fn show_branch(self) -> bool {
        matches!(self, Self::Full)
    }

    const fn show_git_status(self) -> bool {
        (self as u8) <= (Self::NoView as u8)
    }

    const fn show_changed(self) -> bool {
        (self as u8) <= (Self::Compact as u8)
    }

    const fn show_pr(self) -> bool {
        (self as u8) <= (Self::NoView as u8)
    }

    const fn show_pr_marks(self) -> bool {
        (self as u8) <= (Self::Compact as u8)
    }

    const fn show_view(self) -> bool {
        (self as u8) <= (Self::Trimmed as u8)
    }

    const fn show_context(self) -> bool {
        (self as u8) <= (Self::ContextOnly as u8)
    }

    const fn show_usage(self) -> bool {
        (self as u8) <= (Self::NoModel as u8)
    }

    const fn show_resets(self) -> bool {
        matches!(self, Self::Full)
    }

    const fn project_max(self) -> usize {
        match self {
            Self::Full => 20,
            Self::Compact => 16,
            Self::Trimmed | Self::NoView => 14,
            Self::NoGitDetails => 12,
            Self::NoProject
            | Self::NoWorktree
            | Self::NoModel
            | Self::ContextOnly
            | Self::Minimal => 0,
        }
    }

    const fn worktree_max(self) -> usize {
        match self {
            Self::Full => 28,
            Self::Compact => 20,
            Self::Trimmed | Self::NoView => 16,
            Self::NoGitDetails | Self::NoProject => 14,
            Self::NoWorktree | Self::NoModel | Self::ContextOnly | Self::Minimal => 0,
        }
    }

    const fn default_view_max(self) -> usize {
        match self {
            Self::Full => 36,
            Self::Compact => 20,
            Self::Trimmed => 14,
            Self::NoView
            | Self::NoGitDetails
            | Self::NoProject
            | Self::NoWorktree
            | Self::NoModel
            | Self::ContextOnly
            | Self::Minimal => 0,
        }
    }
}

#[derive(Clone, Copy)]
struct FitPolicy {
    tier: FitTier,
    view_max: usize,
}

impl FitPolicy {
    const fn of(tier: FitTier) -> Self {
        Self {
            tier,
            view_max: tier.default_view_max(),
        }
    }
}

struct WorktreeRendered {
    spans: Vec<Span<'static>>,
    pr_spot: Option<(Range<usize>, Link)>,
}

struct Built {
    left: Vec<Span<'static>>,
    left_width: usize,
    right: Vec<Span<'static>>,
    right_width: usize,
    pr_spot: Option<(Range<usize>, Link)>,
    cadence: Cadence,
}

impl Built {
    fn needed_width(&self) -> usize {
        if self.left_width > 0 && self.right_width > 0 {
            self.left_width + 1 + self.right_width
        } else {
            self.left_width + self.right_width
        }
    }
}

pub fn draw(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &Snapshot,
    spinner_frame: SpinnerFrame,
    wall: SystemTime,
) -> (Spots<Link>, Cadence) {
    assert_eq!(
        buffer.area.intersection(area),
        area,
        "status bar paints inside the frame buffer"
    );
    if area.is_empty() {
        return (Spots::default(), Cadence::default());
    }
    buffer.set_style(area, Style::reset().fg(moonlight::TEXT));
    let width = usize::from(area.width);
    let pad = usize::from(width >= 4);
    let inner = width - 2 * pad;
    if inner == 0 {
        return (Spots::default(), Cadence::default());
    }

    let built = fit(snapshot, inner, spinner_frame, wall);
    if built.needed_width() > inner {
        let minimal = cut(MOON, inner);
        let style = luneta_style(snapshot);
        buffer.set_stringn(
            area.x + u16::try_from(pad).unwrap_or(0),
            area.y,
            minimal,
            inner,
            style,
        );
        return (Spots::default(), Cadence::default());
    }

    let mut spans: Vec<Span<'static>> =
        Vec::with_capacity(built.left.len() + built.right.len() + 5);
    if pad > 0 {
        spans.push(Span::raw(" "));
    }
    spans.extend(built.left);
    let gap = inner.saturating_sub(built.left_width + built.right_width);
    if built.left_width > 0 && built.right_width > 0 {
        if gap >= MIN_RULE_GAP {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                RULE.repeat(gap - 2),
                Style::new().fg(moonlight::LIFTED),
            ));
            spans.push(Span::raw(" "));
        } else if gap > 0 {
            spans.push(Span::raw(" ".repeat(gap)));
        }
    } else if built.right_width > 0 && gap > 0 {
        spans.push(Span::raw(" ".repeat(gap)));
    }
    spans.extend(built.right);
    if pad > 0 && built.right_width > 0 {
        spans.push(Span::raw(" "));
    }

    let line = Line::from(spans);
    assert!(
        line.width() <= width,
        "status bar line width {} exceeds area width {width}",
        line.width()
    );
    buffer.set_line(area.x, area.y, &line, area.width);

    let mut spots = Spots::default();
    if let Some((columns, link)) = built.pr_spot {
        spots.add(0, (columns.start + pad)..(columns.end + pad), link);
    }
    (spots.placed(area), built.cadence)
}

fn fit(snapshot: &Snapshot, inner: usize, spinner_frame: SpinnerFrame, wall: SystemTime) -> Built {
    let full_policy = FitPolicy::of(FitTier::Full);
    let full = build(snapshot, full_policy, spinner_frame, wall);
    if full.needed_width() <= inner {
        return full;
    }
    if let Some(view) = snapshot.view.as_ref() {
        let overflow = full.needed_width() - inner;
        let current_view_len = cells(&view.label).min(full_policy.view_max);
        if let Some(trimmed) = current_view_len.checked_sub(overflow)
            && trimmed >= VIEW_LABEL_SOFT_MIN
        {
            let soft = FitPolicy {
                tier: FitTier::Full,
                view_max: trimmed,
            };
            let built = build(snapshot, soft, spinner_frame, wall);
            if built.needed_width() <= inner {
                return built;
            }
        }
    }
    for tier in FitTier::DEGRADED {
        let built = build(snapshot, FitPolicy::of(tier), spinner_frame, wall);
        if built.needed_width() <= inner {
            return built;
        }
    }
    build(
        snapshot,
        FitPolicy::of(FitTier::Minimal),
        spinner_frame,
        wall,
    )
}

fn build(
    snapshot: &Snapshot,
    policy: FitPolicy,
    spinner_frame: SpinnerFrame,
    wall: SystemTime,
) -> Built {
    let mut left: Vec<Span<'static>> = Vec::new();
    let mut pr_spot: Option<(Range<usize>, Link)> = None;

    push_segment(&mut left, build_luneta(snapshot, policy));
    if let Some(model_effort) = build_model_effort(snapshot, policy) {
        push_segment(&mut left, model_effort);
    }
    if let Some(project) = build_project(snapshot, policy) {
        push_segment(&mut left, project);
    }
    if let Some(worktree) = build_worktree(snapshot, policy) {
        let base_col = spans_width(&left) + if left.is_empty() { 0 } else { SEP.width() };
        if let Some((range, link)) = worktree.pr_spot {
            pr_spot = Some(((base_col + range.start)..(base_col + range.end), link));
        }
        push_segment(&mut left, worktree.spans);
    }
    if let Some(view) = build_view(snapshot, policy, spinner_frame) {
        push_segment(&mut left, view);
    }

    let mut right: Vec<Span<'static>> = Vec::new();
    let mut cadence = Cadence::default();
    if let Some(context) = build_context(snapshot, policy) {
        push_segment(&mut right, context);
    }
    if let Some(usage) = build_usage(snapshot, policy, wall, &mut cadence) {
        push_segment(&mut right, usage);
    }

    let left_width = spans_width(&left);
    let right_width = spans_width(&right);
    Built {
        left,
        left_width,
        right,
        right_width,
        pr_spot,
        cadence,
    }
}

fn push_segment(target: &mut Vec<Span<'static>>, segment: Vec<Span<'static>>) {
    if segment.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push(Span::styled(SEP, Style::new().fg(moonlight::MUTE)));
    }
    target.extend(segment);
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

fn luneta_style(snapshot: &Snapshot) -> Style {
    if snapshot.away {
        Style::new().fg(ERROR).add_modifier(Modifier::BOLD)
    } else if snapshot.sidebar_focused {
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(ACCENT)
    }
}

fn build_luneta(snapshot: &Snapshot, policy: FitPolicy) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(MOON, luneta_style(snapshot))];
    if !policy.tier.show_tallies() || !snapshot.sidebar_collapsed {
        return spans;
    }
    let CollapsedTallies {
        waiting,
        unread,
        busy,
    } = snapshot.tallies;
    if waiting > 0 {
        let (glyph, style) = Mark::Waiting.glyph(SpinnerFrame::REST);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!("{glyph}{waiting}"), style));
    }
    if unread > 0 {
        let style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!("{NEW_DOT}{unread}"), style));
    }
    if busy > 0 {
        let (glyph, style) = Mark::Busy.glyph(SpinnerFrame::REST);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!("{glyph}{busy}"), style));
    }
    spans
}

fn build_model_effort(snapshot: &Snapshot, policy: FitPolicy) -> Option<Vec<Span<'static>>> {
    if !policy.tier.show_model() {
        return None;
    }
    let view = snapshot.view.as_ref()?;
    let telemetry = view.telemetry.as_ref()?;
    let model = telemetry.model().map(TelemetryText::as_str);
    let effort = policy
        .tier
        .show_effort()
        .then(|| telemetry.effort().map(TelemetryText::as_str))
        .flatten();
    if model.is_none() && effort.is_none() {
        return None;
    }
    let mut spans = Vec::new();
    if let Some(harness) = view.harness {
        spans.push(Span::styled(
            icons::harness(harness),
            Style::new().fg(moonlight::CYAN),
        ));
        spans.push(Span::raw(" "));
    }
    if let Some(model) = model {
        spans.push(Span::styled(
            model.to_owned(),
            Style::new().fg(moonlight::CYAN),
        ));
    }
    if let Some(effort) = effort {
        if model.is_some() {
            spans.push(Span::styled(DOT_SEP, Style::new().fg(moonlight::MUTE)));
        }
        spans.push(Span::styled(effort.to_owned(), effort_style(effort)));
    }
    Some(spans)
}

fn effort_style(effort: &str) -> Style {
    match effort.to_ascii_lowercase().as_str() {
        "off" | "none" | "minimal" => Style::new().fg(moonlight::MUTE),
        "low" => Style::new().fg(moonlight::GREEN),
        "medium" | "med" => Style::new().fg(moonlight::YELLOW),
        "high" => Style::new().fg(moonlight::BLUE),
        "xhigh" | "max" => Style::new()
            .fg(moonlight::MAUVE)
            .add_modifier(Modifier::BOLD),
        _ => Style::new().fg(moonlight::TEXT),
    }
}

fn build_project(snapshot: &Snapshot, policy: FitPolicy) -> Option<Vec<Span<'static>>> {
    let project_max = policy.tier.project_max();
    if !policy.tier.show_project() || project_max == 0 {
        return None;
    }
    let project = snapshot.project.as_ref()?;
    let mut spans = Vec::new();
    if let Some(machine) = &snapshot.machine {
        let host = cut(machine, 12);
        spans.push(Span::styled(
            format!("{host}:"),
            Style::new().fg(moonlight::MUTE),
        ));
    }
    spans.push(Span::styled(
        cut(&project.name, project_max),
        Style::new().fg(project.colour).add_modifier(Modifier::BOLD),
    ));
    Some(spans)
}

fn build_worktree(snapshot: &Snapshot, policy: FitPolicy) -> Option<WorktreeRendered> {
    let worktree_max = policy.tier.worktree_max();
    if !policy.tier.show_worktree() || worktree_max == 0 {
        return None;
    }
    let worktree = snapshot.worktree.as_ref()?;
    let mut spans = vec![Span::styled(
        cut(&worktree.name, worktree_max),
        Style::new().fg(moonlight::TEXT),
    )];
    if policy.tier.show_branch()
        && let Some(branch) = &worktree.branch
    {
        let branch = cut(branch, worktree_max);
        spans.push(Span::styled(
            format!(" ({branch})"),
            Style::new().fg(moonlight::MUTE),
        ));
    }
    if worktree.missing {
        spans.push(Span::styled(" missing", Style::new().fg(ERROR)));
    }
    if policy.tier.show_git_status() && !worktree.git_status.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            worktree.git_status.clone(),
            Style::new().fg(moonlight::PEACH),
        ));
    }
    if policy.tier.show_changed()
        && let Some(changed) = worktree.changed
    {
        push_changed(&mut spans, changed);
    }
    let mut pr_spot = None;
    if policy.tier.show_pr()
        && let Some(pr) = &worktree.pull_request
    {
        spans.push(Span::raw(" "));
        let start = spans_width(&spans);
        spans.push(Span::styled(pr.label.clone(), Style::new().fg(pr.colour)));
        let end = spans_width(&spans);
        if let Some(link) = &pr.link {
            pr_spot = Some((start..end, link.clone()));
        }
        if policy.tier.show_pr_marks() {
            if let Some(verdict) = pr.verdict {
                spans.push(verdict_span(verdict, pr.stale));
            }
            if pr.conflicting {
                spans.push(conflict_span(pr.stale));
            }
        }
    }
    Some(WorktreeRendered { spans, pr_spot })
}

fn push_changed(spans: &mut Vec<Span<'static>>, changed: Changed) {
    let (added, deleted) = (changed.added(), changed.deleted());
    if added > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("+{added}"),
            Style::new().fg(moonlight::GREEN),
        ));
    }
    if deleted > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("-{deleted}"),
            Style::new().fg(moonlight::RED),
        ));
    }
}

fn verdict_span(verdict: Verdict, stale: bool) -> Span<'static> {
    let (glyph, colour) = match verdict {
        Verdict::Failing => ("✗", moonlight::RED),
        Verdict::Pending => ("●", moonlight::YELLOW),
        Verdict::Passing => ("✓", moonlight::GREEN),
    };
    let colour = if stale { moonlight::MUTE } else { colour };
    Span::styled(glyph, Style::new().fg(colour))
}

fn conflict_span(stale: bool) -> Span<'static> {
    let colour = if stale {
        moonlight::MUTE
    } else {
        moonlight::RED
    };
    Span::styled("≠", Style::new().fg(colour))
}

fn build_view(
    snapshot: &Snapshot,
    policy: FitPolicy,
    spinner_frame: SpinnerFrame,
) -> Option<Vec<Span<'static>>> {
    if !policy.tier.show_view() || policy.view_max == 0 {
        return None;
    }
    let view = snapshot.view.as_ref()?;
    let label_style = match view.mark {
        Some(Mark::Waiting) => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        Some(_) => Style::new().fg(moonlight::TEXT),
        None => Style::new().fg(moonlight::MUTE),
    };
    let mut spans = Vec::new();
    if let Some(mark) = &view.mark {
        let (glyph, style) = mark.glyph(spinner_frame);
        spans.push(Span::styled(glyph, style));
        spans.push(Span::raw(" "));
    }
    if let Some(icon) = view.icon {
        spans.push(Span::styled(icon, label_style));
        spans.push(Span::raw(" "));
    }
    let shown = cut(&view.label, policy.view_max);
    if shown.is_empty() && spans.is_empty() {
        return None;
    }
    spans.push(Span::styled(shown, label_style));
    Some(spans)
}

fn build_context(snapshot: &Snapshot, policy: FitPolicy) -> Option<Vec<Span<'static>>> {
    if !policy.tier.show_context() {
        return None;
    }
    let context = snapshot.view.as_ref()?.telemetry.as_ref()?.context()?;
    Some(vec![Span::styled(
        format_context(&context),
        pressure_style(context.used_permille),
    )])
}

fn build_usage(
    snapshot: &Snapshot,
    policy: FitPolicy,
    wall: SystemTime,
    cadence: &mut Cadence,
) -> Option<Vec<Span<'static>>> {
    if !policy.tier.show_usage() {
        return None;
    }
    if let Some(usage) = snapshot
        .view
        .as_ref()
        .and_then(|view| view.telemetry.as_ref())
        .and_then(AgentTelemetry::usage)
    {
        return render_usage(usage, None, policy.tier.show_resets(), wall, cadence);
    }
    let (harness, usage) = snapshot.fallback_usage.as_ref()?;
    render_usage(
        usage,
        Some(*harness),
        policy.tier.show_resets(),
        wall,
        cadence,
    )
}

fn render_usage(
    usage: &AgentUsage,
    fallback_harness: Option<agents::Harness>,
    show_resets: bool,
    wall: SystemTime,
    cadence: &mut Cadence,
) -> Option<Vec<Span<'static>>> {
    if usage.has_windows() {
        let mut spans = Vec::new();
        if let Some(harness) = fallback_harness {
            spans.push(Span::styled(
                icons::harness(harness),
                Style::new().fg(moonlight::MUTE),
            ));
            spans.push(Span::raw(" "));
        }
        for (index, window) in usage.windows().iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(DOT_SEP, Style::new().fg(moonlight::MUTE)));
            }
            spans.push(Span::styled(
                window.label.as_str().to_owned(),
                Style::new().fg(moonlight::MUTE),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format_percent(window.used_permille, false),
                pressure_style(window.used_permille),
            ));
            if show_resets
                && let Some(resets_at) = window.resets_at
                && let Some((countdown, next_tick)) = format_countdown(resets_at, wall)
            {
                cadence.clock_at(next_tick, wall);
                spans.push(Span::raw(" "));
                spans.push(Span::styled(countdown, Style::new().fg(moonlight::MUTE)));
            }
        }
        return Some(spans);
    }
    let micro_usd = usage.session_cost_micro_usd()?;
    Some(vec![Span::styled(
        format_cost(micro_usd),
        Style::new().fg(moonlight::PEACH),
    )])
}

#[must_use]
fn pressure_style(used: UsedPermille) -> Style {
    let permille = used.get();
    if permille >= HIGH_PERMILLE {
        Style::new().fg(moonlight::RED).add_modifier(Modifier::BOLD)
    } else if permille >= WARN_PERMILLE {
        Style::new().fg(moonlight::YELLOW)
    } else {
        Style::new().fg(moonlight::GREEN)
    }
}

#[must_use]
fn format_percent(used: UsedPermille, decimal_below_ten: bool) -> String {
    let permille = used.get();
    if decimal_below_ten && permille < 100 && !permille.is_multiple_of(10) {
        format!("{}.{}%", permille / 10, permille % 10)
    } else {
        format!("{}%", (permille + 5) / 10)
    }
}

#[must_use]
fn format_tokens(tokens: NonZeroU64) -> String {
    let count = tokens.get();
    if count >= 1_000_000 {
        let tenths = (count + 50_000) / 100_000;
        let whole = tenths / 10;
        let frac = tenths % 10;
        if frac == 0 {
            format!("{whole}M")
        } else {
            format!("{whole}.{frac}M")
        }
    } else if count >= 1_000 {
        let thousands = (count + 500) / 1_000;
        format!("{thousands}k")
    } else {
        count.to_string()
    }
}

#[must_use]
fn format_context(context: &ContextWindow) -> String {
    let percent = format_percent(context.used_permille, true);
    match context.window_tokens {
        Some(window) => format!("{percent}/{}", format_tokens(window)),
        None => percent,
    }
}

#[must_use]
fn format_cost(micro_usd: u64) -> String {
    if micro_usd == 0 {
        return "$0.00".to_owned();
    }
    if micro_usd < 10_000 {
        let millis = micro_usd.saturating_add(500) / 1_000;
        return format!("$0.{millis:03}");
    }
    let cents = micro_usd.saturating_add(5_000) / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

#[must_use]
fn format_countdown(resets_at: SystemTime, wall: SystemTime) -> Option<(String, SystemTime)> {
    let remaining = resets_at.duration_since(wall).ok()?;
    if remaining.is_zero() {
        return None;
    }
    let rounded_secs = remaining
        .as_secs()
        .saturating_add(u64::from(remaining.subsec_nanos() > 0));
    let total_mins = rounded_secs.div_ceil(SECONDS_PER_MINUTE);
    assert!(total_mins >= 1, "positive remaining time rounds to >= 1m");

    let (text, next_remaining_secs) = if total_mins == 1 {
        ("1m".to_owned(), 0)
    } else if total_mins < MINUTES_PER_HOUR {
        (
            format!("{total_mins}m"),
            (total_mins - 1) * SECONDS_PER_MINUTE,
        )
    } else if total_mins < MINUTES_PER_DAY {
        let hours = total_mins / MINUTES_PER_HOUR;
        let mins = total_mins % MINUTES_PER_HOUR;
        let text = if mins == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{mins}m")
        };
        (text, (total_mins - 1) * SECONDS_PER_MINUTE)
    } else if total_mins == MINUTES_PER_DAY {
        ("1d".to_owned(), (MINUTES_PER_DAY - 1) * SECONDS_PER_MINUTE)
    } else {
        let total_hours = total_mins.div_ceil(MINUTES_PER_HOUR);
        assert!(total_hours > HOURS_PER_DAY);
        let days = total_hours / HOURS_PER_DAY;
        let hours = total_hours % HOURS_PER_DAY;
        let text = if hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d{hours}h")
        };
        (text, (total_hours - 1) * SECONDS_PER_HOUR)
    };

    let next_tick = resets_at
        .checked_sub(Duration::from_secs(next_remaining_secs))
        .expect("next countdown tick lies between wall and resets_at");
    assert!(
        next_tick > wall,
        "next countdown tick lands strictly after wall time"
    );
    Some((text, next_tick))
}

#[must_use]
fn cells(text: &str) -> usize {
    text.graphemes(true)
        .filter(|grapheme| !grapheme.chars().any(char::is_control))
        .map(UnicodeWidthStr::width)
        .sum()
}

#[must_use]
fn cut(text: &str, room: usize) -> String {
    if room == 0 {
        return String::new();
    }
    if cells(text) <= room {
        return text
            .chars()
            .filter(|character| !character.is_control())
            .collect();
    }
    let keep = room.saturating_sub(1);
    let mut out = String::new();
    let mut width = 0;
    for grapheme in text.graphemes(true) {
        if grapheme.chars().any(char::is_control) {
            continue;
        }
        let grapheme_width = grapheme.width();
        if width + grapheme_width > keep {
            break;
        }
        out.push_str(grapheme);
        width += grapheme_width;
    }
    out.push('…');
    assert!(cells(&out) <= room, "cut string fits its room");
    out
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use lune_protocol::telemetry::UsageWindow;

    use super::*;
    use crate::clock::Moment;

    fn text(value: &str) -> TelemetryText {
        TelemetryText::parse(value).unwrap()
    }

    fn row_text(buffer: &Buffer, area: Rect) -> String {
        (area.left()..area.right())
            .map(|x| buffer[(x, area.y)].symbol())
            .collect()
    }

    fn full_snapshot(wall: SystemTime) -> Snapshot {
        let usage = AgentUsage::new(
            vec![
                UsageWindow {
                    label: text("5h"),
                    used_permille: UsedPermille::try_new(350).unwrap(),
                    resets_at: Some(wall + Duration::from_mins(130)),
                },
                UsageWindow {
                    label: text("7d"),
                    used_permille: UsedPermille::try_new(820).unwrap(),
                    resets_at: Some(wall + Duration::from_hours(52)),
                },
            ],
            Some(260_000),
        )
        .unwrap();
        let telemetry = AgentTelemetry::new(
            Some(text("Opus 4.7")),
            Some(text("high")),
            Some(ContextWindow {
                used_permille: UsedPermille::try_new(19).unwrap(),
                window_tokens: NonZeroU64::new(1_000_000),
                used_tokens: Some(19_000),
            }),
            Some(usage),
        )
        .unwrap();
        Snapshot {
            sidebar_focused: false,
            sidebar_collapsed: false,
            away: false,
            tallies: CollapsedTallies::default(),
            machine: None,
            project: Some(ProjectInfo {
                name: "lune".to_owned(),
                colour: moonlight::BLUE,
            }),
            worktree: Some(WorktreeInfo {
                name: "statusbar".to_owned(),
                branch: None,
                git_status: "!⇡".to_owned(),
                changed: Some(Changed::new(12, 3).unwrap()),
                pull_request: Some(PullRequestInfo {
                    label: "#334".to_owned(),
                    link: Link::parse("https://github.com/lorenzolfm/lune/pull/334"),
                    colour: moonlight::GREEN,
                    verdict: Some(Verdict::Passing),
                    conflicting: false,
                    stale: false,
                }),
                missing: false,
            }),
            view: Some(ViewInfo {
                mark: Some(Mark::Busy),
                icon: icons::of("claude"),
                label: "Implement status bar".to_owned(),
                harness: Some(agents::Harness::ClaudeCode),
                telemetry: Some(telemetry),
            }),
            fallback_usage: None,
        }
    }

    #[test]
    fn full_status_bar_renders_all_segments_rule_colors_and_pr_spot() {
        let wall = UNIX_EPOCH + Duration::from_secs(1_800_000_007);
        let snapshot = full_snapshot(wall);
        let area = Rect::new(0, 0, 140, 1);
        let mut buffer = Buffer::empty(area);

        let (spots, cadence) = draw(&mut buffer, area, &snapshot, SpinnerFrame::F2, wall);
        let rendered = row_text(&buffer, area);
        assert!(rendered.contains("☾ › "), "{rendered}");
        assert!(rendered.contains("Opus 4.7 · high"), "{rendered}");
        assert!(
            rendered.contains(" › lune › statusbar !⇡ +12 -3 #334✓ › "),
            "{rendered}"
        );
        assert!(rendered.contains("⠹ "), "{rendered}");
        assert!(rendered.contains("Implement status bar"), "{rendered}");
        assert!(rendered.contains("───"), "{rendered}");
        assert!(
            rendered.contains("1.9%/1M › 5h 35% 2h10m · 7d 82% 2d4h"),
            "{rendered}"
        );

        let pr_start = rendered
            .find("#334")
            .expect("PR label is rendered")
            .checked_sub(0)
            .unwrap();
        let pr_col = u16::try_from(rendered[..pr_start].width()).unwrap();
        assert!(
            spots
                .at(ratatui::layout::Position::new(pr_col, 0))
                .is_some()
        );
        assert!(
            !cadence.spinning(),
            "status bar never forces spinner wakeups"
        );

        let drawn = Moment {
            steady: std::time::Instant::now(),
            system: wall,
        };
        assert_eq!(cadence.wait(drawn, drawn), Some(Duration::from_mins(1)));
    }

    #[test]
    fn narrow_widths_degrade_gracefully_and_never_overflow() {
        let wall = UNIX_EPOCH + Duration::from_secs(1_800_000_007);
        let snapshot = full_snapshot(wall);
        for width in 0..=160u16 {
            let area = Rect::new(0, 0, width, 1);
            let mut buffer = Buffer::empty(area);
            let (_, cadence) = draw(&mut buffer, area, &snapshot, SpinnerFrame::REST, wall);
            assert!(!cadence.spinning());
            if width >= 4 {
                let rendered = row_text(&buffer, area);
                assert!(rendered.contains(MOON), "width={width}: {rendered:?}");
            }
        }
    }

    #[test]
    fn collapsed_sidebar_shows_static_tallies_and_shell_shows_fallback_usage() {
        let wall = UNIX_EPOCH + Duration::from_secs(1_800_000_007);
        let fallback = AgentUsage::new(
            vec![UsageWindow {
                label: text("5h"),
                used_permille: UsedPermille::try_new(620).unwrap(),
                resets_at: Some(wall + Duration::from_mins(90)),
            }],
            None,
        )
        .unwrap();
        let snapshot = Snapshot {
            sidebar_focused: false,
            sidebar_collapsed: true,
            away: false,
            tallies: CollapsedTallies {
                waiting: 2,
                unread: 1,
                busy: 3,
            },
            machine: Some("vps".to_owned()),
            project: Some(ProjectInfo {
                name: "lune".to_owned(),
                colour: moonlight::MAUVE,
            }),
            worktree: Some(WorktreeInfo {
                name: "main".to_owned(),
                branch: Some("feat".to_owned()),
                git_status: String::new(),
                changed: None,
                pull_request: None,
                missing: false,
            }),
            view: Some(ViewInfo {
                mark: None,
                icon: icons::of("zsh"),
                label: "lune/main".to_owned(),
                harness: None,
                telemetry: None,
            }),
            fallback_usage: Some((agents::Harness::ClaudeCode, fallback)),
        };
        let area = Rect::new(0, 0, 120, 1);
        let mut buffer = Buffer::empty(area);
        let (_, cadence) = draw(&mut buffer, area, &snapshot, SpinnerFrame::F5, wall);
        let rendered = row_text(&buffer, area);
        assert!(
            rendered.contains("☾ ●2 •1 ⠋3 › vps:lune › main (feat)"),
            "{rendered}"
        );
        assert!(rendered.contains("5h 62% 1h30m"), "{rendered}");
        assert!(
            rendered.contains(icons::harness(agents::Harness::ClaudeCode)),
            "{rendered}"
        );
        assert!(!cadence.spinning());
    }

    #[test]
    fn pi_session_cost_renders_cleanly_when_no_windows_exist() {
        let wall = UNIX_EPOCH + Duration::from_secs(1_800_000_007);
        let usage = AgentUsage::new(Vec::new(), Some(260_000)).unwrap();
        let telemetry = AgentTelemetry::new(
            Some(text("Opus 4.7")),
            Some(text("medium")),
            None,
            Some(usage),
        )
        .unwrap();
        let snapshot = Snapshot {
            view: Some(ViewInfo {
                mark: Some(Mark::Idle),
                icon: icons::of("pi"),
                label: "task".to_owned(),
                harness: Some(agents::Harness::Pi),
                telemetry: Some(telemetry),
            }),
            ..Snapshot::default()
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buffer = Buffer::empty(area);
        let (_, cadence) = draw(&mut buffer, area, &snapshot, SpinnerFrame::REST, wall);
        let rendered = row_text(&buffer, area);
        assert!(rendered.contains("$0.26"), "{rendered}");
        assert_eq!(cadence, Cadence::default());
        assert_eq!(format_cost(0), "$0.00");
        assert_eq!(format_cost(4_200), "$0.004");
        assert_eq!(format_cost(12_345_000), "$12.35");
    }

    #[test]
    fn countdown_formatting_and_next_tick_agree_at_every_boundary() {
        let base = UNIX_EPOCH + Duration::from_secs(1_800_000_007);
        assert_eq!(format_countdown(base, base), None);
        assert_eq!(format_countdown(base - Duration::from_secs(5), base), None);

        let cases = [
            (Duration::from_millis(500), "1m"),
            (Duration::from_mins(1), "1m"),
            (Duration::from_secs(61), "2m"),
            (Duration::from_mins(59), "59m"),
            (Duration::from_hours(1), "1h"),
            (Duration::from_mins(135), "2h15m"),
            (Duration::from_hours(24), "1d"),
            (Duration::from_secs(24 * 3600 + 1), "1d1h"),
            (Duration::from_hours(48), "2d"),
            (Duration::from_hours(52), "2d4h"),
        ];
        for (offset, expected) in cases {
            let resets_at = base + offset;
            let (shown, next_tick) = format_countdown(resets_at, base).unwrap();
            assert_eq!(shown, expected, "offset={offset:?}");
            assert!(next_tick > base, "offset={offset:?}");
            let after = format_countdown(resets_at, next_tick).map(|(text, _)| text);
            assert_ne!(
                after.as_deref(),
                Some(shown.as_str()),
                "display must change at next_tick for offset={offset:?}"
            );
        }
    }
}
