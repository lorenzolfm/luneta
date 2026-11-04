---
name: say
description: Speak aloud while you work with `luneta say`, heard on the machine the user sits at. Use when the user asks you to keep them updated out loud, talk them through it or narrate, or to turn voice on or off, as when they step away from the screen.
allowed-tools: Bash(luneta say:*)
---

# Say it out loud

The user can look away and still follow where the work is going: short updates as things change, and a complete handoff at the end. Voice is a companion to the work, and text stays the record.

```sh
luneta say "I found why the test fails. The fixture is stale, so I'm regenerating it."
luneta say --now "Stop reading that, this matters more."   # goes first, cutting off what plays
luneta say --stop                                          # stops, and lets go of what waits
```

`luneta say` queues one utterance and returns once lunetad has it, in milliseconds: run it in the foreground, as a call of its own, and go straight on with the work. Utterances play in order, on the machine the user sits at, whichever machine you run on. The user may have other agents talking too, so use `--now` only when the user asks for it or something urgent needs them.

Where `luneta say` fails, the work goes on without voice, and the next text you write says why, once. A denied call has a fix to name there: `Bash(luneta say:*)` in `permissions.allow` of their Claude Code settings, as a skill's own permission lasts one turn.

## Voice is a sticky mode

Asking for voice switches it on for this conversation: through follow-ups, new phases, errors, retries and finished subtasks, until the user explicitly turns it off ("stop talking", "text only"). A short message, a new topic or a written final answer leaves it on. When they turn it off, acknowledge once in text and stop.

## The loop

Repeat while you work:

1. **Orient.** One sentence as you begin: what you understood, and what you check first.
2. **Explore.** Before a meaningful investigation, say what question it should answer.
3. **Report.** As soon as a useful result lands, say the finding and why it matters.
4. **Adapt.** When a result changes the plan or something fails, say what changed and where you go next.
5. **Hand off.** End with the spoken handoff below, then the written answer.

Speak when the listener's picture of the work should change: a step begins, evidence lands, a hypothesis falls, a decision is made, the plan pivots. Trivial commands and each file opened go unsaid.

## Cadence

- One breath per update: 8 to 30 words, one or two short sentences.
- A useful update every 15 to 30 seconds of active work, and never more than 45 seconds of silence. Before a long command, say what runs and what result you wait for.
- A discovery right after the result that supports it, so findings arrive one at a time.
- When narration falls behind the work, one current-state summary in place of the backlog: being current beats being complete.
- When the user redirects you, acknowledge the pivot aloud at once, and narrate the new direction.

## The handoff

The final utterance stands alone, for a listener who missed everything before it. Lead with "I'm done" or the actual end state, then:

- the outcome
- the central findings or design
- what changed
- what you verified, and how it went
- risks, blockers and tests not run
- the most useful next step

Aim for 120 to 250 words. Split a longer one into two or three utterances, in order. The handoff closes the task, and voice stays on for the next one.

## Write for the ear

- Lead with the headline, then the detail: the listener cannot skim.
- Ordinary sentences, with signposts for structure: "There are three findings. First..."
- Identifiers as words: "the verify keys command". Paths, code, Markdown, tables and logs stay in the text.
- Acronyms spaced where they could be misread: "H T L C".
- Numbers as said: "three of five members", "commit c five two d f".
- Conclusions and the reasons behind decisions. Secrets, personal data and raw logs stay out of the air.

## Quoting

Wrap the utterance in double quotes, and write it in plain language with no double quotes, backticks, `$` or `!` inside, so the shell passes it as it is.

---

Adapted from the `sayit` skill of [Say It](https://github.com/callebtc/sayit), under the MIT license in `LICENSE` beside this file.
