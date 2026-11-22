# recover

Taskwarrior inspired recorder for commands and their output.

## Features

- Records command, output, exit code and timing to one SQLite file
- Runs in a pseudo terminal so colors and TUIs work (piped input works too)
- Caps stored output per run (16 MiB by default), keeping its start and end
- Lists, regex search (command, output, note), date and exit filters
- Page long listings in a scrollable pager (`recover list` or `-l`), uncapped, with in-pager regex search
- Stable ids plus negative indexes to reach recent runs
- Notes on runs, shown in the list and searchable
- Colored zebra list and default list size, tuned in ~/.config/recover/config
- Delete runs by id (`recover del <id...>`)
- Date filters in local time; history file kept private (0600)
