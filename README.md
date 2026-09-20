# recover

Taskwarrior inspired recorder for commands and their output.

## Features

- Records command, output, exit code and timing to one SQLite file
- Runs in a pseudo terminal so colors and TUIs work
- Lists, regex search (command, output, note), date and exit filters
- Page long listings in a scrollable pager (`recover list` or `-l`), uncapped, with in-pager regex search
- Stable ids plus negative indexes to reach recent runs
- Notes on runs, shown in the list and searchable
- Colored zebra list and default list size, tuned in ~/.recoverc
- Delete runs by id (`recover del <id...>`)
