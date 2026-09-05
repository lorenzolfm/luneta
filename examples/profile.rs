//! Profiling harness. Drives the real modules with synthetic data and reports
//! wall time plus allocation counts per operation. Not part of the plugin.
#![allow(
    dead_code,
    unused_imports,
    reason = "the src modules are pulled in whole; this harness drives only part of each"
)]

#[path = "../src/agents.rs"] mod agents;
#[path = "../src/cursor.rs"] mod cursor;
#[path = "../src/dirs.rs"] mod dirs;
#[path = "../src/elapsed.rs"] mod elapsed;
#[path = "../src/fetch.rs"] mod fetch;
#[path = "../src/layout.rs"] mod layout;
#[path = "../src/paint.rs"] mod paint;
#[path = "../src/panes.rs"] mod panes;
#[path = "../src/places.rs"] mod places;
#[path = "../src/render.rs"] mod render;
#[path = "../src/sessions.rs"] mod sessions;

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use agents::{AgentSet, Live};
use dirs::DirSet;
use elapsed::Age;
use panes::Peeks;
use places::Places;
use sessions::{MatchSet, Selection, Session, Sessions};

// ---------- counting allocator ----------

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

#[expect(
    unsafe_code,
    reason = "a GlobalAlloc is unsafe by signature; counting allocations needs one"
)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size(), Relaxed);
        let live = LIVE.fetch_add(l.size(), Relaxed) + l.size();
        PEAK.fetch_max(live, Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        System.dealloc(p, l);
    }
}

#[global_allocator]
static A: Counting = Counting;

fn reset() {
    ALLOCS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    PEAK.store(LIVE.load(Relaxed), Relaxed);
}

// ---------- synthetic data ----------

fn sessions(live: usize, dead: usize) -> Sessions {
    Sessions {
        live: (0..live)
            .map(|i| Session {
                name: format!("session-{i:03}-work"),
                age: Age::new(Duration::from_secs(i as u64 * 97)),
            })
            .collect(),
        dead: (0..dead)
            .map(|i| Session {
                name: format!("dead-{i:03}-old"),
                age: Age::new(Duration::from_secs(i as u64 * 9_000)),
            })
            .collect(),
    }
}

fn zoxide(n: usize) -> Vec<u8> {
    let mut out = String::new();
    for i in 0..n {
        writeln!(
            out,
            "{:>9.1} /home/you/Projects/group-{}/repo-{:04}-name",
            10_000.0 - i as f64,
            i % 12,
            i
        )
        .expect("writing to a String cannot fail");
    }
    out.into_bytes()
}

fn claude_ps(n: usize) -> Vec<u8> {
    let rows: Vec<String> = (0..n)
        .map(|i| {
            format!(
                r#"{{"status":"{}","status_age":{},"cwd":"/home/you/Projects/group-{}/repo-{:04}-name","name":"agent-{:03}","name_source":"user","zellij":{{"session":"session-{:03}-work","pane":"{}"}}}}"#,
                ["waiting", "idle", "busy", "shell"][i % 4],
                i * 13,
                i % 12,
                i,
                i,
                i % 24,
                i
            )
        })
        .collect();
    format!("[{}]", rows.join(",")).into_bytes()
}

fn list_panes(session: usize, panes: usize) -> Vec<u8> {
    let rows: Vec<String> = (0..panes)
        .map(|i| {
            format!(
                r#"{{"id":{},"is_plugin":false,"is_suppressed":false,"pane_cwd":"/home/you/Projects/group-{}/repo-{:04}-name","pane_command":"claude","title":"✳ working"}}"#,
                i,
                i % 12,
                i
            )
        })
        .collect();
    let _ = session;
    format!("[{}]", rows.join(",")).into_bytes()
}

/// A realistic 64-line pane dump: box drawing, SGR colour, wide glyphs.
fn dump(lines: usize, cols: usize) -> Vec<u8> {
    let mut out = String::new();
    for i in 0..lines {
        out.push_str("\u{1b}[38;5;244m");
        write!(out, "{i:>4} ").expect("writing to a String cannot fail");
        out.push_str("\u{1b}[m");
        let mut w = 5;
        while w < cols {
            out.push_str("\u{1b}[1;32m");
            out.push_str("fn ");
            out.push_str("\u{1b}[m");
            out.push_str("value_of(x: usize) -> usize { x + 1 } ");
            out.push_str("日本語 ");
            w += 45;
        }
        out.push('\n');
    }
    out.into_bytes()
}

// ---------- timing ----------

struct Stat {
    name: &'static str,
    per_op_ns: f64,
    allocs: f64,
    bytes: f64,
    peak: usize,
}

fn bench(name: &'static str, iters: usize, mut f: impl FnMut()) -> Stat {
    for _ in 0..(iters / 10).max(1) {
        f();
    }
    reset();
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    let elapsed = start.elapsed();
    Stat {
        name,
        per_op_ns: elapsed.as_nanos() as f64 / iters as f64,
        allocs: ALLOCS.load(Relaxed) as f64 / iters as f64,
        bytes: BYTES.load(Relaxed) as f64 / iters as f64,
        peak: PEAK.load(Relaxed),
    }
}

fn report(title: &str, stats: &[Stat]) {
    eprintln!("\n== {title} ==");
    eprintln!(
        "{:<34} {:>12} {:>10} {:>14}",
        "operation", "per op", "allocs/op", "bytes/op"
    );
    for s in stats {
        let time = if s.per_op_ns > 1_000_000.0 {
            format!("{:.2} ms", s.per_op_ns / 1e6)
        } else if s.per_op_ns > 1_000.0 {
            format!("{:.1} us", s.per_op_ns / 1e3)
        } else {
            format!("{:.0} ns", s.per_op_ns)
        };
        eprintln!(
            "{:<34} {:>12} {:>10.0} {:>14.0}",
            s.name, time, s.allocs, s.bytes
        );
    }
}

const ROWS: usize = 30;
const COLS: usize = 120;

fn main() {
    let scale: usize = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(1);
    let n_live = 12 * scale;
    let n_dead = 20 * scale;
    let n_dirs = 400 * scale;
    let n_agents = 12 * scale;

    eprintln!(
        "scale x{scale} -> {n_live} live sessions, {n_dead} dead, {n_dirs} zoxide dirs, \
         {n_agents} agents, {ROWS}x{COLS} pane"
    );

    let sess = sessions(n_live, n_dead);

    // --- fixtures ---
    let mut places = Places::default();
    for s in 0..n_live {
        places.ingest(
            format!("session-{s:03}-work"),
            Some(0),
            &list_panes(s, 24),
        );
    }

    let mut peeks = Peeks::default();
    peeks.ingest(
        ("session-000-work".to_owned(), 0),
        Some(0),
        &dump(64, 200),
        b"",
    );

    let zox = zoxide(n_dirs);
    let ps = claude_ps(n_agents);

    // --- session matching ---
    let mut stats = Vec::new();

    let mut matches = MatchSet::default();
    matches.refresh(&sess, Some("session-000-work".to_owned()));
    stats.push(bench("MatchSet::refresh (empty term)", 2000, || {
        let mut matches = MatchSet::default();
        matches.refresh(&sess, Some("session-000-work".to_owned()));
        std::hint::black_box(&matches);
    }));

    stats.push(bench("MatchSet::set_search_term (typed)", 2000, || {
        matches.set_search_term("rep".to_owned(), &sess);
        std::hint::black_box(&matches);
    }));
    matches.set_search_term(String::new(), &sess);

    // --- dirs ---
    let mut dirs = DirSet::default();
    stats.push(bench("DirSet::ingest (zoxide parse)", 200, || {
        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), &zox, b"");
        std::hint::black_box(&dirs);
    }));
    dirs.ingest(Some(0), &zox, b"");

    stats.push(bench("DirSet::rebuild (empty term)", 200, || {
        dirs.rebuild("", &sess, Some("session-000-work"), Selection::Hold);
        std::hint::black_box(&dirs);
    }));
    stats.push(bench("DirSet::rebuild (typed 'rep')", 200, || {
        dirs.rebuild("rep", &sess, Some("session-000-work"), Selection::Hold);
        std::hint::black_box(&dirs);
    }));
    dirs.rebuild("", &sess, Some("session-000-work"), Selection::Hold);

    // --- agents ---
    let mut agents = AgentSet::default();
    stats.push(bench("AgentSet::ingest (claude-ps json)", 2000, || {
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), &ps, b"");
        std::hint::black_box(&agents);
    }));
    agents.ingest(Some(0), &ps, b"");

    stats.push(bench("AgentSet::rebuild (empty term)", 2000, || {
        let live = Live::new(Some("session-000-work"), &places);
        agents.rebuild("", &live, None, Age::from_secs(1), Selection::Hold);
        std::hint::black_box(&agents);
    }));

    // --- places ---
    stats.push(bench("Places::ingest (list-panes json)", 2000, || {
        let mut built = Places::default();
        built.ingest("s".to_owned(), Some(0), &list_panes(0, 24));
        std::hint::black_box(&built);
    }));

    report("data path (per poll / per keystroke)", &stats);

    // --- ansi / text primitives ---
    let dumped = String::from_utf8(dump(1, 200)).expect("the synthetic dump is utf-8");
    let line = dumped.lines().next().expect("the dump has one line").to_owned();
    let plain = "value_of(x: usize) -> usize { x + 1 } ".repeat(5);

    let mut prim = Vec::new();
    prim.push(bench("panes::sgr_only (1 line)", 20000, || {
        std::hint::black_box(panes::sgr_only(&line));
    }));
    prim.push(bench("panes::columns (1 line)", 20000, || {
        std::hint::black_box(panes::columns(&line));
    }));
    prim.push(bench("panes::fitted (1 line, 56 cols)", 20000, || {
        std::hint::black_box(panes::fitted(&line, 56));
    }));
    prim.push(bench("layout::truncate (185 chars)", 20000, || {
        std::hint::black_box(layout::truncate(&plain, 56));
    }));
    prim.push(bench("Peeks::ingest (64-line dump)", 500, || {
        let mut peeks = Peeks::default();
        peeks.ingest(("s".to_owned(), 0), Some(0), &dump(64, 200), b"");
        std::hint::black_box(&peeks);
    }));
    report("text primitives", &prim);

    // --- render (stdout should be redirected) ---
    let mut ag = AgentSet::default();
    ag.ingest(Some(0), &ps, b"");
    ag.rebuild(
        "",
        &Live::new(Some("session-000-work"), &places),
        None,
        Age::from_secs(1),
        Selection::Hold,
    );

    // BYTES=1 prints exactly one frame to stdout, so `| wc -c` gives the
    // payload zellij has to parse per render.
    if std::env::var("BYTES").is_ok() {
        render::render_search(&matches, &peeks, None, ROWS, COLS);
        return;
    }

    let mut frames = Vec::new();
    // RENDER_ONLY=1 spins the render loop long enough for `perf record`.
    let reps = if std::env::var("RENDER_ONLY").is_ok() { 20000 } else { 2000 };
    frames.push(bench("render::render_search", reps, || {
        render::render_search(&matches, &peeks, None, ROWS, COLS);
    }));
    frames.push(bench("render::render_dirs", reps, || {
        render::render_dirs(&dirs, "", ROWS, COLS);
    }));
    frames.push(bench("render::render_agents", reps, || {
        render::render_agents(&ag, &peeks, "", ROWS, COLS, 7);
    }));
    report("render (one frame, 10 frames/sec budget = 100 ms)", &frames);

    // --- where does a frame's time go? ---
    let inner = 56usize;
    let mut parts = Vec::new();
    parts.push(bench("Line build+finish (one 56-col row)", 50000, || {
        let mut line = layout::Line::new();
        line.push("> ", 3);
        line.push_hits("session-000-work", 1, 3, &[0, 1, 2]);
        line.pad_to(40);
        line.push("2h ago", 2);
        std::hint::black_box(line.finish(inner));
    }));
    parts.push(bench("that row onto the wire", 50000, || {
        let mut line = layout::Line::new();
        line.push("> ", 3);
        line.push_hits("session-000-work", 1, 3, &[0, 1, 2]);
        line.pad_to(40);
        line.push("2h ago", 2);
        paint::print_at(&line.finish(inner), 1, 1, inner + 2);
    }));
    let rect = layout::Rect { x: 0, y: 0, width: 60, height: 26 };
    parts.push(bench("Rect::top + rule_indices (border)", 50000, || {
        let border = rect.top("luneta", "2/40");
        std::hint::black_box(border.rule_indices());
    }));
    report("render building blocks (x ~26 rows per frame)", &parts);

    // --- split a frame into its two boxes ---
    let empty = Peeks::default();
    let mut sp = Vec::new();
    sp.push(bench("render_search, preview EMPTY", 2000, || {
        render::render_search(&matches, &empty, None, ROWS, COLS);
    }));
    sp.push(bench("render_search, preview 64 lines", 2000, || {
        render::render_search(&matches, &peeks, None, ROWS, COLS);
    }));
    sp.push(bench("render_search, no preview box (60 cols)", 2000, || {
        render::render_search(&matches, &peeks, None, ROWS, 50);
    }));
    report("frame split: list box vs preview box", &sp);



    eprintln!("\npeak live heap during run: {} KiB", PEAK.load(Relaxed) / 1024);
}
