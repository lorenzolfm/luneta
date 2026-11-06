{ pkgs, ... }:
{
  name = "ci";
  usage = "[pr number]";
  help = "what CI says of this head, or of a PR's; only reads, and exits 0 only on proof";
  tools = [
    pkgs.coreutils
    pkgs.gh
    pkgs.git
    pkgs.gnused
  ];
  text = ''
    export LC_ALL=C

    usage() {
      echo "usage: dev ci [pr number]" >&2
      exit 2
    }

    case "$#:$*" in
      0:)
        number=""
        ;;
      1:*)
        number="$1"
        [[ "$number" =~ ^[0-9]+$ ]] || usage
        ;;
      *)
        usage
        ;;
    esac

    said="$(mktemp)"
    trap 'rm -f "$said"' EXIT

    reasons=()
    line() {
      printf '%-5s %s\n' "$1" "$2"
    }
    short() {
      printf '%.7s' "$1"
    }

    fields=number,state,headRefOid,headRefName,baseRefName,mergeable,isCrossRepository
    shape='[.number, .state, .headRefOid, .headRefName, .baseRefName, .mergeable, .isCrossRepository] | @tsv'

    branch="$(git symbolic-ref -q HEAD)" || branch=""
    branch="''${branch#refs/heads/}"
    if [ -z "$number" ] && [ -z "$branch" ]; then
      detached="$(git rev-parse HEAD)"
      if ! number="$(
        gh api "repos/{owner}/{repo}/commits/$detached/pulls" \
          --jq "map(select(.head.sha == \"$detached\")) | sort_by(.state != \"open\") | .[0].number // empty" 2>"$said"
      )"; then
        if [[ "$(<"$said")" != *"No commit found"* ]]; then
          cat "$said" >&2
          exit 1
        fi
        number=""
      fi
    fi
    pr=""
    if [ -n "$number" ]; then
      pr="$(gh pr view "$number" --json "$fields" --jq "$shape")"
    elif [ -n "$branch" ] && ! pr="$(gh pr view --json "$fields" --jq "$shape" 2>"$said")"; then
      if [[ "$(<"$said")" != *"no pull requests found"* ]]; then
        cat "$said" >&2
        exit 1
      fi
      pr=""
    fi

    state=OPEN
    base=main
    if [ -n "$pr" ]; then
      IFS=$'\t' read -r number state head name base mergeable fork <<<"$pr"
      echo "PR #$number, $name, $state"
      here=""
      if [ -z "''${1:-}" ]; then
        here="$(git rev-parse HEAD)"
      elif [ "$fork" = false ]; then
        here="$(git rev-parse -q --verify "refs/heads/$name")" || here=""
      fi
      if [ -z "$here" ] && [ "$fork" = true ]; then
        line head "$(short "$head"), from a fork"
      elif [ -z "$here" ]; then
        line head "$(short "$head"), and no branch $name here"
      elif [ "$here" = "$head" ]; then
        line head "$(short "$head"), the same here"
      else
        line head "$(short "$head"), but $(short "$here") here"
        reasons+=("the PR's head is not the local one")
      fi
      if [ "$state" = OPEN ] && [ "$mergeable" = CONFLICTING ]; then
        line head "conflicts with $base, and GitHub starts no run on a conflict"
        reasons+=("it conflicts with $base")
      fi
    else
      head="$(git rev-parse HEAD)"
      echo "''${branch:-HEAD}, no PR at this head"
      line head "$(short "$head")"
    fi

    run="$(
      gh run list --commit "$head" --workflow ci.yml --limit 1 \
        --json databaseId,status,conclusion,event,url \
        --jq '.[] | [.databaseId, (if .status == "completed" then .conclusion else .status end), .status, .event, .url] | @tsv'
    )"
    if [ -z "$run" ]; then
      line run "none for $(short "$head")"
      reasons+=("no run exists for this head")
    else
      IFS=$'\t' read -r id conclusion progress event url <<<"$run"
      line run "$conclusion, on $event, $url"
      if [ "$progress" != completed ]; then
        reasons+=("the run has not finished")
      elif [ "$conclusion" != success ]; then
        reasons+=("the run ended in $conclusion")
      fi
    fi

    if [ "$state" = OPEN ]; then
      if ! compared="$(
        gh api "repos/{owner}/{repo}/compare/$base...$head" \
          --jq '[.behind_by, .merge_base_commit.sha] | @tsv' 2>"$said"
      )"; then
        line "$base" "can't compare with this head: $(<"$said")"
        reasons+=("GitHub can't compare this head with $base")
      else
        IFS=$'\t' read -r behind merge_base <<<"$compared"
        if [ "$behind" -eq 0 ]; then
          line "$base" "still at the merge base, $(short "$merge_base")"
        else
          line "$base" "$behind past the merge base, $(short "$merge_base")"
          reasons+=("$base has moved past the merge base, so green proves nothing of the merge")
        fi
      fi
    fi

    if [ -n "$run" ]; then
      jobs="$(
        gh run view "$id" --json jobs --jq '
          .jobs[]
          | [
              (if .status == "completed" then .conclusion else .status end),
              (if .status == "completed" and .conclusion != "skipped" and .completedAt >= .startedAt then ((.completedAt | fromdate) - (.startedAt | fromdate)) | "\(. / 60 | floor)m\(. % 60)s" else "-" end),
              (.databaseId | tostring),
              .name,
              ([.steps[] | select(.conclusion == "failure") | .name] | join(", "))
            ]
          | @tsv'
      )"
      echo jobs
      while IFS=$'\t' read -r job duration job_id job_name failed_steps; do
        printf '  %-12s %-8s %s\n' "$job" "$duration" "$job_name"
        case "$job" in
          failure | timed_out) ;;
          *) continue ;;
        esac
        [ -z "$failed_steps" ] || line "" "      step $failed_steps"

        log="$PWD/target/ci/$job_id.log"
        if [ ! -s "$log" ]; then
          mkdir -p target/ci
          if ! gh api --allow-escape-sequences "repos/{owner}/{repo}/actions/jobs/$job_id/logs" 2>"$said" |
            sed -E \
              -e 's/\x1b\[[0-?]*[ -\/]*[@-~]//g' \
              -e 's/[\x01-\x08\x0b-\x1f\x7f]//g' \
              -e 's/\xc2[\x80-\x9f]/?/g' >"$log.part"; then
            rm -f "$log.part"
            line "" "      no log: $(<"$said")"
            continue
          fi
          mv "$log.part" "$log"
        fi

        sed -nE \
          -e 's/.*[[:space:]](FAIL \+ LEAK|FAIL|XFAIL|LEAK-FAIL|TIMEOUT|ABORT SIG [0-9]+|ABORT|SIG[A-Z]+)[[:space:]]+\[[^]]*\][[:space:]]+(\([^)]*\)[[:space:]]+)?(.*)$/\3/p' \
          -e 's/.*[[:space:]]test (.* - .* \(line [0-9]+\)) \.\.\. FAILED$/\1/p' \
          "$log" |
          sort -u |
          while IFS= read -r test_name; do
            line "" "      test $test_name"
          done
        line "" "      log  $log"
      done <<<"$jobs"
    fi

    if [ "''${#reasons[@]}" -gt 0 ]; then
      echo
      for reason in "''${reasons[@]}"; do
        echo "not proven: $reason"
      done
      exit 1
    fi
    echo
    if [ "$state" = OPEN ]; then
      echo "proven: CI passed on this head, on $base as it stands"
    else
      echo "CI passed on its last head; the PR is $state"
    fi
  '';
}
