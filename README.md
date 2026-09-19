# recover

Record commands and their output, then find them again later.

Shell history keeps the *command* but throws away the *output*, exit code, and timing.
`recover` opts a command into recording with a short prefix — `r echo test` — running it
transparently (you still see live output) while saving the command, its combined
stdout+stderr, exit code, timestamps, and duration into a single portable SQLite file. A
taskwarrior-style query CLI then lists, regex-searches, and date-filters those runs.

## Features

- **Faithful capture** — runs the command in a pseudo-terminal, so colors, progress bars,
  and interactive TUIs work normally; output is teed live and stored **verbatim** (binary-safe),
  with exit code, timestamps, and duration.
- **One portable file** — everything lives in a single SQLite database; copy it to move your
  history, or use `export --json` / `import`.
- **taskwarrior-style queries** — `recover` lists recent runs; regex-filter by command,
  output, note, or either (`cmd:` `out:` `com:` `reg:`); filter and combine by `date:`,
  `exit:`, `limit:`.
- **Flexible dates** — `date:today-5day`, ranges `date:14day..7day`, one-sided
  `date.before:`/`date.after:`, and shorthands (`date:3day`, `date:2h`).
- **Stable ids + negative indices** — `recover out 42` always means run 42; `-1` is the most
  recent, `-2` the one before, so you can act without listing first.
- **Act id-first or verb-first** — `recover 2 out` or `recover out 2`; also `cmd`, `com`,
  `show`, `rerun`, `rm`. `recover 2` alone shows the run.
- **Notes** — flag important runs with `recover 2 com "why"`; notes show in the list when
  there's room and are searchable with `com:`.
- **Colored, configurable list** — zebra-striped rows like taskwarrior, tuned in `~/.recoverc`
  (mode, color on/off/auto, hex bar and header colors).
- **Fast at scale** — near-instant search on 10k+ runs (see [Performance](#performance)).

## Install

Needs a Rust toolchain (`rustc`/`cargo` — install from <https://rustup.rs> if you don't have
it). Then put `recover` on your PATH and add the `r` prefix to your shell:

```sh
# 1. build + install the binary into ~/.cargo/bin (already on your PATH)
cargo install --path .
#    — or, without installing, copy the release build somewhere on PATH:
#    cargo build --release && cp target/release/recover ~/.local/bin/

# 2. add the `r` prefix to bash (use ~/.zshrc for zsh)
echo 'r() { recover run "$@"; }' >> ~/.bashrc

# 3. reload your shell
source ~/.bashrc

# 4. (optional) install the man page
mkdir -p ~/.local/share/man/man1 && cp man/recover.1 ~/.local/share/man/man1/
```

You can now use it two ways: `recover …` for queries, and `r <command>` to record.
For a command whose name starts with a dash, use `recover run -- <command>`.

`recover help` prints a short command summary; `man recover` has the full reference
(filters, date expressions, configuration, and environment variables).

> A shell **function** is used rather than `alias r='recover run'` because it forwards
> arguments and quoting cleanly.

## Usage

```
r echo hello                  # run + record (you see output live)
r -c 'make 2>&1 | tail'       # record a whole shell pipeline (quote it)

recover                       # list the 10 most recent runs, newest first

recover 42                    # show a run in full (metadata + note + output)
recover 42 out                # just its output       (or: recover out 42)
recover 42 out --strip        # ...with ANSI colors removed
recover 42 out | grep panic   # pipe stored output onward
recover 42 cmd                # just the command text (or: recover cmd 42)
recover 42 com                # just its note
recover 42 com "why this mattered"   # add/replace the note
recover 42 show               # full detail (same as `recover 42`)
recover 42 rerun              # run the stored command again (records a new run)
recover 42 rm                 # delete a run;  recover clear --yes  deletes all

recover db path               # where the database lives
recover export --json out.json # dump all runs;  recover import out.json  loads them
```

Every run action works **id-first** (`recover 42 out`) or **verb-first** (`recover out 42`) —
whichever you prefer. The verbs are `out`, `cmd`, `com`, `show`, `rerun`, `rm`.

**Negative indices** (a quick-mode feature — see Configuration) count back from the newest
run, so you can act on a recent run without listing first: `-1` is the most recent, `-2`
the one before, etc. They work anywhere an id is accepted:

```
recover cmd -1                # the command you just ran
recover out -1 | less         # its output
recover rerun -2              # re-run the second-most-recent
```

The `list` table is zebra-striped (dark-blue alternate rows, like taskwarrior) when
printed to a terminal; piping or redirecting gives plain text. When a row has room to
spare, the run's note is shown after its command (`… # your note`). Set `NO_COLOR=1`, or
`color = off` in the config, to disable coloring.

## Configuration

Settings live in `~/.recoverc` (override the path with `RECOVER_CONFIG`). It's optional —
without it you get the defaults below. Format is `key = value`, one per line, with `#`
full-line comments.

```ini
# ~/.recoverc
mode = quick          # quick (default) | standard
color = auto          # auto (default: only on a terminal) | on | off
bar_color = #000033   # zebra stripe background (hex), default subtle dark blue
header_color = #ffffff # header text color (hex); omit for bold-only
```

- **`mode`** — `quick` (default) enables the convenience *shortcuts*: negative indices
  (`recover cmd -1`) and bare-duration dates (`date:3day`). `standard` turns those
  shortcuts off, requiring positive ids and explicit date anchors (`today-3day`). It does
  not affect colors.
- **`color`** — independent of mode. `auto` colors only when writing to a terminal; `on`
  forces it; `off` disables it. `NO_COLOR` in the environment always wins.
- **`bar_color` / `header_color`** — `#rrggbb` hex. Text on the stripe is auto-chosen
  (black or white) for contrast with your bar color.

### Filters (taskwarrior-style `key:value`, AND-combined)

```
recover cmd:'^git'            # regex over the command text
recover out:panic             # regex over the output
recover reg:deploy            # regex over command OR output
recover com:important         # regex over your notes (mark & find runs)
recover date:today-5day       # runs in the last 5 days (a window: at-or-after)
recover date:5day             # shorthand: same as date:today-5day
recover date:14day..7day      # last week, excluding this week (half-open [A, B))
recover date:2026-09-01..2026-09-10   # explicit half-open range
recover date.after:yesterday date.before:today
recover exit:1                # only failed runs
recover cmd:git date:today-5day limit:20    # combine freely; limit:0 = no cap
```

Date expressions: `now`, `today`, `yesterday`, `tomorrow`, `YYYY-MM-DD`, RFC3339, plus
one arithmetic term like `today-5day`, `now-2h`, `today+1week`
(units `s min h day week month year`). A **bare duration** with no anchor is shorthand for
minus-from-default: day/week/month units count from `today` (midnight), while hour/minute
units count from `now` — so `date:3day` == `date:today-3day` but `date:2h` == `date:now-2h`
(use `+` for the future, e.g. `date:+3day`). Bare durations are a *quick-mode* shortcut
(see Configuration). Regex is the Rust `regex` syntax and is case-sensitive by default —
use `(?i)` for insensitive.

## How it works

- **Capture:** the command runs inside a pseudo-terminal (PTY), so colors, progress bars,
  and interactive TUIs behave exactly as in a normal shell. Output is teed to your terminal
  and an in-memory buffer, then stored **verbatim** (binary-safe). ANSI codes are stripped
  only for regex matching and for `out --strip`.
- **Storage:** one SQLite file at `$XDG_DATA_HOME/recover/recover.db`
  (default `~/.local/share/recover/recover.db`), overridable with `RECOVER_DB`.
  **To move your history to another machine, copy that one file.** `recover export --json`
  and `recover import` give a portable text format as an alternative.
- **IDs are stable:** a run's number never changes; `recover out 42` always means that run.

## Performance

Search stays fast because date and exit-code filters are indexed in SQL, and the regex
runs only over the rows that survive them. Measured on **10,000 recorded runs**:

| query | ~300 B outputs (4 MB db) | ~4 KB outputs (45 MB db) |
|-------|--------------------------|--------------------------|
| `date:…` / `exit:…` (+ `limit:`) | < 5 ms | < 5 ms |
| regex over everything (`reg:…`, no other filter) | ~40 ms | ~0.5 s |
| regex narrowed by a `date:` window | ~10 ms | ~50 ms |

So a full-history regex is near-instant for normal command output, and still sub-second even
for a history of large build logs — add a `date:` window to keep it instant.

**Why not FTS5?** SQLite's FTS5 accelerates *tokenized* full-text `MATCH` (words, prefixes,
phrases), not arbitrary regular expressions, so it cannot index a pattern like `out:'pa.*c'`.
At this scale a scan is already fast. If you later keep a very large history of big outputs
and want literal-substring lookups indexed, FTS5 could be added as an optional fast path that
pre-selects candidate rows before the regex runs.

## Notes & limits

- A pipeline typed as `r echo test | grep t` records only `echo test` — your interactive
  shell owns the `|` before `r` sees it. Quote it or use `-c`: `r -c 'echo test | grep t'`.
- A command that reads piped stdin (`data | r some-filter`) is not fully supported: stdin is
  only forwarded when running on a real terminal. Record the producing command, or use `-c`.
- `rerun` re-executes the stored command through `$SHELL -c`.
