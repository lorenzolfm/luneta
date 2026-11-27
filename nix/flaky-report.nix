{
  pkgs,
  maxAttempts,
  derivationName,
}:
let
  script = pkgs.writeText "lune-flaky-report.py" ''
    import argparse
    from dataclasses import dataclass
    from enum import Enum
    import os
    from pathlib import Path
    import re
    import sys
    import xml.etree.ElementTree as ET

    FLAKY_RECORDS_MAX = 1000
    FLAKY_SUMMARY_MAX = 200
    FLAKY_ANNOTATIONS_MAX = 10

    ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
    RULE_RE = re.compile(r"^[-─]{12}$")
    NEXTEST_START_RE = re.compile(
        r"^(?: Nextest run ID | {4}Starting [0-9]+ tests? across )"
    )
    SUMMARY_RE = re.compile(
        r"^ {0,5}Summary \[[^]]*\] +[0-9]+(?:/[0-9]+)? tests? run:"
    )
    SUMMARY_FLAKY_RE = re.compile(r"\b([1-9][0-9]*) flaky\b")
    FLAKY_LINE_RE = re.compile(
        r"^ {0,3}FLAKY ([0-9]+)/([0-9]+) \[[^]]*\] +(?:\([^)]*\) +)?(.+?)\s*$"
    )
    MID_RUN_PASS_RE = re.compile(
        r"^ {0,3}TRY ([0-9]+) PASS \[[^]]*\] +(?:\([^)]*\) +)?(.+?)\s*$"
    )
    FINAL_STATUS_LINE_RE = re.compile(
        r"^\s*(?:FAIL|LEAK|LEAK-FAIL|PASS|SKIP|TIMEOUT|ABORT|EXECFAIL|SLOW|SIG[A-Z0-9]+)\b"
    )


    class DaemonOutcome(Enum):
        SUCCESS = "success"
        FAILURE = "failure"


    class ParserPhase(Enum):
        BEFORE_SUMMARY = 0
        IN_SUMMARY = 1
        AFTER_SUMMARY = 2


    @dataclass(frozen=True)
    class FlakyTest:
        name: str
        try_num: int
        total_num: int

        def __post_init__(self) -> None:
            if not self.name or "\n" in self.name or "\r" in self.name:
                raise ValueError(
                    f"flaky test name must be a non-empty single line: {self.name!r}"
                )
            if self.try_num < 2 or self.try_num > self.total_num:
                raise ValueError(
                    f"invalid attempt count {self.try_num}/{self.total_num} for {self.name}"
                )


    @dataclass(frozen=True)
    class JunitOutcome:
        records: tuple[FlakyTest, ...]
        total_flaky: int
        truncated: bool


    @dataclass(frozen=True)
    class LogSummaryOutcome:
        summary_flaky: int
        records: tuple[FlakyTest, ...]
        total_parsed: int
        truncated: bool


    @dataclass(frozen=True)
    class NoSummaryOutcome:
        saw_nextest: bool
        mid_run_records: tuple[FlakyTest, ...]
        mid_run_total: int
        mid_run_truncated: bool


    ReportOutcome = JunitOutcome | LogSummaryOutcome | NoSummaryOutcome


    def local_tag(tag: str) -> str:
        if "}" in tag:
            return tag.rsplit("}", 1)[1]
        return tag


    def escape_command_data(value: str) -> str:
        return value.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


    def escape_command_property(value: str) -> str:
        return (
            escape_command_data(value)
            .replace(":", "%3A")
            .replace(",", "%2C")
        )


    def warning(title: str, message: str) -> None:
        safe_title = escape_command_property(title)
        safe_message = escape_command_data(message)
        print(f"::warning title={safe_title}::{safe_message}")


    def error(message: str) -> None:
        print(f"::error::{escape_command_data(message)}")


    def code_span(text: str) -> str:
        longest = 0
        current = 0
        for ch in text:
            if ch == "`":
                current += 1
                if current > longest:
                    longest = current
            else:
                current = 0
        fence = "`" * (longest + 1)
        if text.startswith("`") or text.endswith("`"):
            return f"{fence} {text} {fence}"
        return f"{fence}{text}{fence}"


    def bound_sorted(items: dict[str, FlakyTest]) -> tuple[tuple[FlakyTest, ...], int, bool]:
        ordered = [items[key] for key in sorted(items)]
        total = len(ordered)
        return tuple(ordered[:FLAKY_RECORDS_MAX]), total, total > FLAKY_RECORDS_MAX


    def parse_junit(junit_path: Path, max_attempts: int) -> JunitOutcome:
        try:
            tree = ET.parse(junit_path)
        except (OSError, ET.ParseError) as exc:
            raise ValueError(f"failed to parse junit report {junit_path}: {exc}") from exc
        root = tree.getroot()
        if local_tag(root.tag) not in ("testsuites", "testsuite"):
            raise ValueError(f"unexpected junit root element <{root.tag}> in {junit_path}")
        seen: dict[str, FlakyTest] = {}
        for testcase in root.iter():
            if local_tag(testcase.tag) != "testcase":
                continue
            flaky_count = 0
            hard_failure = False
            for child in testcase:
                tag = local_tag(child.tag)
                if tag in ("flakyFailure", "flakyError"):
                    flaky_count += 1
                elif tag in ("failure", "error"):
                    hard_failure = True
            if flaky_count == 0 or hard_failure:
                continue
            name = (testcase.get("name") or "").strip()
            classname = (testcase.get("classname") or "").strip()
            full_name = f"{classname} {name}" if classname and name else (name or classname)
            if not full_name:
                raise ValueError(f"flaky <testcase> without name in {junit_path}")
            try_num = flaky_count + 1
            total_num = max(max_attempts, try_num)
            if full_name not in seen:
                seen[full_name] = FlakyTest(
                    name=full_name,
                    try_num=try_num,
                    total_num=total_num,
                )
        records, total, truncated = bound_sorted(seen)
        return JunitOutcome(records=records, total_flaky=total, truncated=truncated)


    def parse_log(
        log_path: Path,
        derivation: str,
        max_attempts: int,
    ) -> LogSummaryOutcome | NoSummaryOutcome:
        if not log_path.is_file():
            return NoSummaryOutcome(
                saw_nextest=False,
                mid_run_records=(),
                mid_run_total=0,
                mid_run_truncated=False,
            )
        prefix = f"{derivation}> "
        phase = ParserPhase.BEFORE_SUMMARY
        saw_nextest = False
        prev_was_rule = False
        summary_flaky = 0
        seen: dict[str, FlakyTest] = {}
        mid_run: dict[str, FlakyTest] = {}
        with log_path.open("r", encoding="utf-8", errors="replace") as handle:
            for raw_line in handle:
                cleaned = ANSI_RE.sub("", raw_line).rstrip("\r\n")
                if not cleaned.startswith(prefix):
                    continue
                body = cleaned[len(prefix):]
                if phase is ParserPhase.BEFORE_SUMMARY:
                    if RULE_RE.match(body):
                        prev_was_rule = True
                        continue
                    was_rule = prev_was_rule
                    prev_was_rule = False
                    if NEXTEST_START_RE.match(body):
                        saw_nextest = True
                        continue
                    if not saw_nextest:
                        continue
                    mid_match = MID_RUN_PASS_RE.match(body)
                    if mid_match:
                        try_num = int(mid_match.group(1))
                        test_name = mid_match.group(2)
                        if try_num >= 2 and test_name and test_name not in mid_run:
                            mid_run[test_name] = FlakyTest(
                                name=test_name,
                                try_num=try_num,
                                total_num=max(max_attempts, try_num),
                            )
                        continue
                    if was_rule and SUMMARY_RE.match(body):
                        phase = ParserPhase.IN_SUMMARY
                        flaky_match = SUMMARY_FLAKY_RE.search(body)
                        summary_flaky = int(flaky_match.group(1)) if flaky_match else 0
                    continue
                if phase is ParserPhase.IN_SUMMARY:
                    flaky_match = FLAKY_LINE_RE.match(body)
                    if flaky_match:
                        try_num = int(flaky_match.group(1))
                        total_num = int(flaky_match.group(2))
                        test_name = flaky_match.group(3)
                        record = FlakyTest(
                            name=test_name,
                            try_num=try_num,
                            total_num=total_num,
                        )
                        if test_name not in seen:
                            seen[test_name] = record
                        continue
                    if FINAL_STATUS_LINE_RE.match(body):
                        continue
                    phase = ParserPhase.AFTER_SUMMARY
        if phase is ParserPhase.BEFORE_SUMMARY:
            mid_records, mid_total, mid_truncated = bound_sorted(mid_run)
            return NoSummaryOutcome(
                saw_nextest=saw_nextest,
                mid_run_records=mid_records,
                mid_run_total=mid_total,
                mid_run_truncated=mid_truncated,
            )
        records, total_parsed, truncated = bound_sorted(seen)
        return LogSummaryOutcome(
            summary_flaky=summary_flaky,
            records=records,
            total_parsed=total_parsed,
            truncated=truncated,
        )


    def append_summary(
        step_summary: Path | None,
        runner_label: str,
        records: tuple[FlakyTest, ...],
        total_records: int,
        notes: list[str],
    ) -> None:
        if step_summary is None:
            return
        if not records and not notes:
            return
        heading = f"{chr(35) * 3} Flaky tests on {runner_label}"
        lines = [heading, ""]
        if records:
            lines.append(
                "Each failed and then passed on a retry, so the job went on. One that is here again after its fix is a real failure."
            )
            lines.append("")
            limit = min(len(records), FLAKY_SUMMARY_MAX)
            for record in records[:limit]:
                lines.append(
                    f"- {code_span(record.name)}, which passed on try {record.try_num} of {record.total_num}"
                )
            if total_records > limit:
                lines.append(
                    f"- and {total_records - limit} more flaky test(s) omitted; see the build log"
                )
        for note in notes:
            if records:
                lines.append("")
            lines.append(note)
        lines.append("")
        with step_summary.open("a", encoding="utf-8") as handle:
            handle.write("\n".join(lines))


    def emit_flaky_annotations(
        runner_label: str,
        records: tuple[FlakyTest, ...],
        total_records: int,
        annotations_left: int,
    ) -> None:
        if not records or annotations_left <= 0:
            return
        if total_records <= annotations_left and len(records) == total_records:
            for record in records:
                warning(
                    f"Flaky test on {runner_label}",
                    f"{record.name}, which passed on try {record.try_num} of {record.total_num}",
                )
            return
        individual_limit = max(annotations_left - 1, 0)
        shown = records[:individual_limit]
        for record in shown:
            warning(
                f"Flaky test on {runner_label}",
                f"{record.name}, which passed on try {record.try_num} of {record.total_num}",
            )
        omitted = total_records - len(shown)
        warning(
            f"More flaky tests on {runner_label}",
            f"{omitted} more flaky test(s) omitted from annotations; see the step summary",
        )


    def write_attic_skip(
        github_env: Path | None,
        out_link: Path | None,
        skip: bool,
    ) -> None:
        if github_env is None:
            return
        if not skip:
            with github_env.open("a", encoding="utf-8") as handle:
                handle.write("ATTIC_SKIP_PATH=\n")
            return
        if out_link is None or not out_link.exists():
            return
        resolved = os.path.realpath(out_link)
        if not resolved.startswith("/nix/store/") or "\n" in resolved or "\r" in resolved:
            raise ValueError(f"out-link did not resolve to a nix store path: {resolved}")
        with github_env.open("a", encoding="utf-8") as handle:
            handle.write(f"ATTIC_SKIP_PATH={resolved}\n")


    def emit_report(
        outcome: ReportOutcome,
        runner_label: str,
        step_summary: Path | None,
        github_env: Path | None,
        out_link: Path | None,
    ) -> None:
        annotations_left = FLAKY_ANNOTATIONS_MAX
        notes: list[str] = []
        records: tuple[FlakyTest, ...] = ()
        total_records = 0

        match outcome:
            case JunitOutcome(records=recs, total_flaky=total, truncated=truncated):
                records = recs
                total_records = total
                if truncated:
                    msg = f"more than {FLAKY_RECORDS_MAX} flaky tests were read; check the junit report"
                    warning(f"Flaky test list truncated on {runner_label}", msg)
                    notes.append(msg)
                    annotations_left -= 1
                write_attic_skip(github_env, out_link, skip=total > 0)
            case LogSummaryOutcome(
                summary_flaky=summary_flaky,
                records=recs,
                total_parsed=total_parsed,
                truncated=truncated,
            ):
                records = recs
                total_records = total_parsed
                if truncated:
                    msg = f"more than {FLAKY_RECORDS_MAX} flaky tests were read; check the log"
                    warning(f"Flaky test list truncated on {runner_label}", msg)
                    notes.append(msg)
                    annotations_left -= 1
                if total_parsed != summary_flaky:
                    msg = (
                        f"nextest summary counted {summary_flaky} flaky test(s), "
                        f"but {total_parsed} FLAKY line(s) were read; check the log"
                    )
                    warning(f"Flaky test count mismatch on {runner_label}", msg)
                    notes.append(msg)
                    annotations_left -= 1
            case NoSummaryOutcome(
                saw_nextest=saw_nextest,
                mid_run_records=mid_records,
                mid_run_total=mid_total,
                mid_run_truncated=mid_truncated,
            ):
                records = mid_records
                total_records = mid_total
                if mid_truncated:
                    msg = f"more than {FLAKY_RECORDS_MAX} mid-run retried tests were read; check the log"
                    warning(f"Flaky test list truncated on {runner_label}", msg)
                    notes.append(msg)
                    annotations_left -= 1
                if saw_nextest:
                    msg = "nextest started and ended without printing a final Summary line; check the log"
                    warning(f"Nextest summary missing on {runner_label}", msg)
                    notes.append(msg)
                    annotations_left -= 1

        append_summary(step_summary, runner_label, records, total_records, notes)
        emit_flaky_annotations(runner_label, records, total_records, annotations_left)


    def main(argv: list[str]) -> int:
        parser = argparse.ArgumentParser(prog="lune-flaky-report")
        parser.add_argument(
            "--outcome",
            required=True,
            choices=[item.value for item in DaemonOutcome],
        )
        parser.add_argument("--log", required=True, type=Path)
        parser.add_argument("--junit", type=Path)
        parser.add_argument("--out-link", type=Path)
        parser.add_argument("--derivation", default="${derivationName}")
        parser.add_argument("--max-attempts", type=int, default=${toString maxAttempts})
        parser.add_argument("--runner-os", default=os.environ.get("RUNNER_OS", "unknown"))
        parser.add_argument("--runner-arch", default=os.environ.get("RUNNER_ARCH", "unknown"))
        parser.add_argument("--step-summary", type=Path)
        parser.add_argument("--github-env", type=Path)
        args = parser.parse_args(argv)

        if not args.derivation:
            error("derivation name must be non-empty")
            return 1
        if args.max_attempts < 1:
            error(f"max-attempts must be >= 1, got {args.max_attempts}")
            return 1

        daemon_outcome = DaemonOutcome(args.outcome)
        step_summary = args.step_summary
        if step_summary is None and os.environ.get("GITHUB_STEP_SUMMARY"):
            step_summary = Path(os.environ["GITHUB_STEP_SUMMARY"])
        github_env = args.github_env
        if github_env is None and os.environ.get("GITHUB_ENV"):
            github_env = Path(os.environ["GITHUB_ENV"])

        try:
            if daemon_outcome is DaemonOutcome.SUCCESS:
                if args.junit is None or not args.junit.is_file():
                    error(f"expected junit report at {args.junit}")
                    return 1
                report: ReportOutcome = parse_junit(args.junit, args.max_attempts)
            else:
                report = parse_log(
                    args.log,
                    args.derivation,
                    args.max_attempts,
                )
            emit_report(
                report,
                f"{args.runner_os} {args.runner_arch}",
                step_summary,
                github_env,
                args.out_link,
            )
        except ValueError as exc:
            error(str(exc))
            return 1
        return 0


    if __name__ == "__main__":
        sys.exit(main(sys.argv[1:]))
  '';

  bin = pkgs.writeShellApplication {
    name = "lune-flaky-report";
    runtimeInputs = [ pkgs.python3 ];
    text = ''
      exec python3 ${script} "$@"
    '';
  };

  testScript = pkgs.writeText "lune-flaky-report-test.py" ''
    from pathlib import Path
    import subprocess
    import sys
    import tempfile

    BIN = "${pkgs.lib.getExe bin}"


    def run_cli(*args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [BIN, *args],
            text=True,
            capture_output=True,
            env=env or {},
            check=False,
        )


    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)

        clean_junit = root / "clean.xml"
        clean_junit.write_text(
            '<testsuites name="nextest-run" tests="2" failures="0" errors="0">'
            '<testsuite name="luned::daemon" tests="2" failures="0" errors="0">'
            '<testcase name="a" classname="luned::daemon" time="0.1"/>'
            '<testcase name="b" classname="luned::daemon" time="0.2"/>'
            "</testsuite></testsuites>",
            encoding="utf-8",
        )
        summary = root / "clean-summary.md"
        gh_env = root / "clean.env"
        gh_env.write_text("ATTIC_SKIP_PATH=/nix/store/preexisting-luned\n", encoding="utf-8")
        res = run_cli(
            "--outcome", "success",
            "--log", str(root / "missing.log"),
            "--junit", str(clean_junit),
            "--out-link", "${bin}",
            "--runner-os", "Linux",
            "--runner-arch", "X64",
            "--step-summary", str(summary),
            "--github-env", str(gh_env),
        )
        assert res.returncode == 0, res
        assert res.stdout == "", res.stdout
        assert not summary.exists()
        assert gh_env.read_text(encoding="utf-8").splitlines()[-1] == "ATTIC_SKIP_PATH="

        flaky_junit = root / "flaky.xml"
        flaky_junit.write_text(
            '<testsuites name="nextest-run" tests="3" failures="0" errors="0">'
            '<testsuite name="luned::daemon" tests="3" failures="0" errors="0">'
            '<testcase name="layouts::second_try" classname="luned::daemon" time="0.2">'
            '<flakyFailure type="test failure">boom 1</flakyFailure>'
            "</testcase>"
            '<testcase name="git::third`try%100" classname="luned::unit" time="0.3">'
            '<flakyFailure type="test failure">boom 1</flakyFailure>'
            '<flakyError type="test error">boom 2</flakyError>'
            "</testcase>"
            '<testcase name="hard::fail" classname="luned::daemon" time="0.1">'
            '<failure type="test failure">hard</failure>'
            '<flakyFailure type="test failure">ignore</flakyFailure>'
            "</testcase>"
            "</testsuite></testsuites>",
            encoding="utf-8",
        )
        summary = root / "flaky-summary.md"
        gh_env = root / "flaky.env"
        res = run_cli(
            "--outcome", "success",
            "--log", str(root / "missing.log"),
            "--junit", str(flaky_junit),
            "--out-link", "${bin}",
            "--runner-os", "macOS",
            "--runner-arch", "ARM64",
            "--step-summary", str(summary),
            "--github-env", str(gh_env),
        )
        assert res.returncode == 0, res
        assert "::warning title=Flaky test on macOS ARM64::luned::daemon layouts::second_try, which passed on try 2 of ${toString maxAttempts}" in res.stdout, res.stdout
        assert "::warning title=Flaky test on macOS ARM64::luned::unit git::third`try%25100, which passed on try 3 of ${toString maxAttempts}" in res.stdout, res.stdout
        assert "hard::fail" not in res.stdout, res.stdout
        summary_text = summary.read_text(encoding="utf-8")
        assert "``luned::unit git::third`try%100``" in summary_text, summary_text
        env_text = gh_env.read_text(encoding="utf-8")
        assert env_text.startswith("ATTIC_SKIP_PATH=/nix/store/"), env_text

        nl = f"&{chr(35)}10;"
        newline_junit = root / "newline.xml"
        newline_junit.write_text(
            '<testsuites name="nextest-run" tests="1" failures="0" errors="0">'
            '<testsuite name="luned::daemon" tests="1" failures="0" errors="0">'
            f'<testcase name="bad{nl}{nl}inject" classname="luned::daemon" time="0.1">'
            '<flakyFailure type="test failure">boom</flakyFailure>'
            "</testcase></testsuite></testsuites>",
            encoding="utf-8",
        )
        gh_env = root / "newline.env"
        gh_env.write_text("ATTIC_SKIP_PATH=/nix/store/keep-me\n", encoding="utf-8")
        res = run_cli(
            "--outcome", "success",
            "--log", str(root / "missing.log"),
            "--junit", str(newline_junit),
            "--out-link", "${bin}",
            "--runner-os", "Linux",
            "--runner-arch", "X64",
            "--github-env", str(gh_env),
        )
        assert res.returncode != 0, res
        assert "::error::flaky test name must be a non-empty single line" in res.stdout, res.stdout
        assert gh_env.read_text(encoding="utf-8") == "ATTIC_SKIP_PATH=/nix/store/keep-me\n"

        res = run_cli(
            "--outcome", "success",
            "--log", str(root / "missing.log"),
            "--junit", str(root / "no-such.xml"),
            "--runner-os", "Linux",
            "--runner-arch", "X64",
        )
        assert res.returncode != 0, res
        assert "::error::expected junit report" in res.stdout, res.stdout

        scoped_log = root / "scoped.log"
        scoped_log.write_text(
            "\n".join([
                "other-drv> ────────────",
                "other-drv>      Summary [   1.000s] 1 test run: 1 passed (1 flaky)",
                "other-drv>    FLAKY 2/3 [   0.010s] other::bin foreign_test",
                "${derivationName}> ------------",
                "${derivationName}>      Summary [   0.001s] 1 test run: 1 passed, 0 failed",
                "${derivationName}> ------------",
                "${derivationName}>  Nextest run ID 1234 with nextest profile: ci",
                "${derivationName}>     Starting 3 tests across 1 binary",
                "${derivationName}>   TRY 1 FAIL [   0.010s] (---) luned::daemon layouts::hard_fail",
                "${derivationName}>   stdout ---",
                "${derivationName}>     ------------",
                "${derivationName}>          Summary [   0.001s] 1 test run: 1 passed, 0 failed",
                "${derivationName}>        FLAKY 1/1 [   0.001s] fake::bin spoofed_in_stdout",
                "${derivationName}>       TRY 2 PASS [   0.001s] fake::bin spoofed_mid_run",
                "${derivationName}>   TRY 1 FAIL [   0.010s] (---) luned::daemon layouts::one",
                "${derivationName}>   TRY 2 PASS [   0.010s] (1/3) luned::daemon layouts::one",
                "${derivationName}> ------------",
                "${derivationName}>      Summary [   2.000s] 3 tests run: 2 passed (1 flaky), 1 failed",
                "${derivationName}>    FLAKY 2/3 [   0.010s] (1/3) luned::daemon layouts::one",
                "${derivationName}>   TRY 3 FAIL [   0.020s] (2/3) luned::daemon layouts::hard_fail",
                "${derivationName}> error: test run failed",
                "${derivationName}> ------------",
                "${derivationName}>      Summary [   0.001s] 1 test run: 1 passed, 0 failed",
                "${derivationName}>    FLAKY 1/1 [   0.001s] luned::doctest spoofed_after_summary",
            ]) + "\n",
            encoding="utf-8",
        )
        summary = root / "scoped-summary.md"
        res = run_cli(
            "--outcome", "failure",
            "--log", str(scoped_log),
            "--runner-os", "Linux",
            "--runner-arch", "X64",
            "--step-summary", str(summary),
        )
        assert res.returncode == 0, res
        assert "luned::daemon layouts::one, which passed on try 2 of 3" in res.stdout, res.stdout
        assert "foreign_test" not in res.stdout, res.stdout
        assert "spoofed_in_stdout" not in res.stdout, res.stdout
        assert "spoofed_mid_run" not in res.stdout, res.stdout
        assert "spoofed_after_summary" not in res.stdout, res.stdout
        assert "mismatch" not in res.stdout.lower(), res.stdout

        partial_log = root / "partial.log"
        partial_log.write_text(
            "\n".join([
                "${derivationName}> ────────────",
                "${derivationName}>  Nextest run ID 5678 with nextest profile: ci",
                "${derivationName}>     Starting 3 tests across 1 binary",
                "${derivationName}>   TRY 2 PASS [   0.010s] (1/3) luned::daemon partial::flaky",
                "${derivationName}> ────────────",
                "${derivationName}>      Summary [   0.026s] 2/3 tests run: 1 passed (1 flaky), 1 failed, 1 skipped",
                "${derivationName}>    FLAKY 2/3 [   0.010s] (1/3) luned::daemon partial::flaky",
                "${derivationName}>   TRY 3 FAIL [   0.010s] (2/3) luned::daemon partial::hard_fail",
            ]) + "\n",
            encoding="utf-8",
        )
        summary = root / "partial-summary.md"
        res = run_cli(
            "--outcome", "failure",
            "--log", str(partial_log),
            "--runner-os", "Linux",
            "--runner-arch", "X64",
            "--step-summary", str(summary),
        )
        assert res.returncode == 0, res
        assert "luned::daemon partial::flaky, which passed on try 2 of 3" in res.stdout, res.stdout
        assert "summary missing" not in res.stdout.lower(), res.stdout

        partial_clean_log = root / "partial-clean.log"
        partial_clean_log.write_text(
            "\n".join([
                "${derivationName}> ------------",
                "${derivationName}>     Starting 3 tests across 1 binary",
                "${derivationName}> ------------",
                "${derivationName}>      Summary [   0.026s] 2/3 tests run: 1 passed, 1 failed, 1 skipped",
            ]) + "\n",
            encoding="utf-8",
        )
        summary = root / "partial-clean-summary.md"
        res = run_cli(
            "--outcome", "failure",
            "--log", str(partial_clean_log),
            "--runner-os", "Linux",
            "--runner-arch", "X64",
            "--step-summary", str(summary),
        )
        assert res.returncode == 0, res
        assert res.stdout == "", res.stdout
        assert not summary.exists()

        trunc_lines = [
            "${derivationName}>     Starting 1005 tests across 1 binary",
            "${derivationName}> ────────────",
            "${derivationName}>      Summary [   9.000s] 1005 tests run: 1004 passed (1005 flaky), 1 failed",
        ]
        for idx in range(1005):
            trunc_lines.append(
                f"${derivationName}>    FLAKY 2/3 [   0.001s] luned::t test_{idx:04d}"
            )
        trunc_log = root / "trunc.log"
        trunc_log.write_text("\n".join(trunc_lines) + "\n", encoding="utf-8")
        summary = root / "trunc-summary.md"
        res = run_cli(
            "--outcome", "failure",
            "--log", str(trunc_log),
            "--runner-os", "Linux",
            "--runner-arch", "X64",
            "--step-summary", str(summary),
        )
        assert res.returncode == 0, res
        warnings = [line for line in res.stdout.splitlines() if line.startswith("::warning ")]
        assert len(warnings) == 10, len(warnings)
        assert "Flaky test list truncated" in res.stdout, res.stdout
        assert "count mismatch" not in res.stdout, res.stdout
        assert "805 more flaky test(s) omitted" in summary.read_text(encoding="utf-8")

        mismatch_log = root / "mismatch.log"
        mismatch_log.write_text(
            "\n".join([
                "${derivationName}>     Starting 2 tests across 1 binary",
                "${derivationName}> ------------",
                "${derivationName}>      Summary [   1.000s] 2 tests run: 1 passed (1 flaky), 1 failed",
            ]) + "\n",
            encoding="utf-8",
        )
        summary = root / "mismatch-summary.md"
        res = run_cli(
            "--outcome", "failure",
            "--log", str(mismatch_log),
            "--runner-os", "Linux",
            "--runner-arch", "X64",
            "--step-summary", str(summary),
        )
        assert res.returncode == 0, res
        assert "Flaky test count mismatch" in res.stdout, res.stdout
        assert "nextest summary counted 1 flaky test(s)" in summary.read_text(encoding="utf-8")

        aborted_log = root / "aborted.log"
        aborted_log.write_text(
            "\n".join([
                "${derivationName}>     Starting 10 tests across 1 binary",
                "${derivationName}>   TRY 2 PASS [   0.010s] (1/10) luned::daemon mid_run::flaky",
            ]) + "\n",
            encoding="utf-8",
        )
        summary = root / "aborted-summary.md"
        res = run_cli(
            "--outcome", "failure",
            "--log", str(aborted_log),
            "--runner-os", "Linux",
            "--runner-arch", "X64",
            "--step-summary", str(summary),
        )
        assert res.returncode == 0, res
        assert "Nextest summary missing" in res.stdout, res.stdout
        assert "luned::daemon mid_run::flaky, which passed on try 2" in res.stdout, res.stdout

        compile_fail_log = root / "compile-fail.log"
        compile_fail_log.write_text(
            "${derivationName}> error[E0308]: mismatched types\n",
            encoding="utf-8",
        )
        summary = root / "compile-fail-summary.md"
        res = run_cli(
            "--outcome", "failure",
            "--log", str(compile_fail_log),
            "--runner-os", "Linux",
            "--runner-arch", "X64",
            "--step-summary", str(summary),
        )
        assert res.returncode == 0, res
        assert res.stdout == "", res.stdout
        assert not summary.exists()

    sys.exit(0)
  '';

  check = pkgs.runCommand "lune-flaky-report-test" { } ''
    ${pkgs.lib.getExe pkgs.python3} ${testScript}
    touch "$out"
  '';
in
{
  inherit bin check;
}
