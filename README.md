# <img src="assets/icon.png" width="44" align="top"> gpudash

A live terminal dashboard for your Slurm jobs, across several clusters at once.

You run `gpudash` on your laptop. It logs into each cluster over ssh, asks Slurm what you
have running and queued, and reads the temperature, usage and memory of every GPU your jobs
are on. Everything shows up in one full-screen view that refreshes itself, and your Mac
tells you when something changes.

![gpudash showing queued and recently ended jobs on two clusters](assets/screenshot.webp)

## Highlights

- **Every cluster on one screen.** Running, queued and recently ended jobs from all your
  clusters, side by side, refreshed every 30 seconds.
- **Notifications, so you don't have to watch.** Your Mac tells you when jobs are
  **queued**, **start**, **finish**, **fail**, run **out of memory** or hit their **time
  limit**. A batch of jobs submitted at once (say, by an agent or a sweep script) arrives as
  one notification: "5 jobs queued on explorer". Notifications carry the gpudash icon.
- **Live GPU readings.** Temperature for every GPU (blue when cool, red when hot), plus big
  usage and memory bars and a temperature graph for the selected job.
- **When will it start?** Slurm's estimated start time for every queued job, in your own
  time zone, or the reason it's waiting (like `Dependency`).
- **Logs in one keypress.** `enter` opens a job's output log, colored and scrollable,
  following the end while the job runs.
- **Shell into a job.** `s` opens a shell inside a running job on its GPU node, so
  `nvidia-smi` and `htop` show your job.
- **Command palette.** `k` opens a searchable list of every action and setting. Settings
  you change there are saved for next time.
- **Quick to use.** One shared ssh connection per cluster, so refreshes don't log in from
  scratch. A cluster that stops answering shows an error and retries; it never freezes the
  screen.

## Install

You need [Rust](https://rustup.rs). On a Mac you also want Apple's Command Line Tools,
which most developers already have (check with `xcode-select -p`; install with
`xcode-select --install`).

```sh
cargo install --git https://github.com/RollingRo11/gpudash
```

or from a clone:

```sh
git clone https://github.com/RollingRo11/gpudash
cd gpudash
cargo install --path .
```

This builds two things:

- **`gpudash`**, the program you run, in `~/.cargo/bin`.
- On macOS, **a small notification app** (`gpudash.app`) built into it. The first time
  gpudash sends a notification it installs that app to
  `~/Library/Application Support/gpudash/` and macOS asks whether gpudash may send
  notifications. Click **Allow**. This app is what puts the gpudash icon on notifications;
  see [Notifications](#notifications).

If the Command Line Tools are missing, the build still works and prints a warning, and
notifications fall back to the Script Editor icon. Install the tools and reinstall to get
the icon.

## Setup

### 1. ssh

gpudash logs in with plain `ssh <name>`, so every cluster needs an entry in `~/.ssh/config`
that works **without typing a password** (an ssh key). Check with:

```sh
ssh -o BatchMode=yes mycluster true
```

If that prints nothing and returns, you're good. Example entry:

```
Host explorer
    HostName login.explorer.northeastern.edu
    User myname
    IdentityFile ~/.ssh/id_ed25519
```

You don't need to set up connection sharing (`ControlMaster`) yourself; gpudash does it.

### 2. Tell gpudash your clusters

Easiest: run gpudash with one cluster, then add the rest from inside the app.

```sh
gpudash explorer
```

Press `k`, type `add`, press `enter`, and type the next ssh name. gpudash saves your
clusters to its config file, so next time plain `gpudash` shows them all.

Or write the config yourself. It lives at `~/.config/gpudash/config.toml`:

```toml
[[cluster]]
host = "explorer"
prefix = "/projects/myname/"

[[cluster]]
host = "KISSKI"
prefix = "/mnt/vast-kisski/projects/myproject/"
```

- **`host`**: the `Host` name from your `~/.ssh/config`. It's also the title of the
  cluster's box.
- **`prefix`** (optional): cut off the start of the *folder* column, so
  `/projects/myname/experiments/run3` shows as `experiments/run3`.
- Clusters are shown in the order they're listed.

All the other settings are in [Configuration](#configuration).

## Using it

```sh
gpudash                 # watch every cluster in the config
gpudash explorer        # watch just one
gpudash -n 60           # refresh every 60 seconds this time
gpudash --help
```

### What's on screen

Each cluster gets a box with one row per job:

| column           | meaning                                                                                        |
| ---------------- | ---------------------------------------------------------------------------------------------- |
| **id**           | Slurm job id                                                                                   |
| **name**         | job name                                                                                       |
| **state**        | `● run`, `◌ wait`, or how it ended: `✓ done`, `✗ fail`, `✗ oom`, `⏱ time`, `⊘ cncl`             |
| **gpu**          | GPU type and count. For running jobs this comes from the card itself (`nvidia-smi`).           |
| **time**         | running: time used / time limit, with a bar that turns red near the limit. Queued: the limit.  |
| **temp / start** | running: each GPU's temperature. Queued: estimated start. Ended: when, and the exit code.      |
| **folder**       | the folder the job was submitted from                                                          |

The panel at the bottom shows the selected job in more detail: full folder and node, and
for running jobs a big **usage** bar (orange to pink) and **memory** bar (gold to peach) for
each GPU, plus a temperature graph.

Jobs that ended in the last 24 hours come after the active ones: the 5 most recent per
cluster by default. `h` switches to all of them, or none.

### Keys

| key              | action                                              |
| ---------------- | --------------------------------------------------- |
| `k` / `:`        | **command palette**                                 |
| `↑` `↓` / `j`    | move                                                |
| `g` / `G`        | first / last job                                    |
| `enter` / `l`    | open the job's log                                  |
| `s`              | shell into the job (on its GPU node if running)     |
| `x`              | cancel the job (asks first)                         |
| `y`              | copy the job id                                     |
| `/`              | filter by name, folder or id (`esc` clears)         |
| `h`              | ended jobs: recent → all from 24h → hidden          |
| `n`              | notifications on / off                              |
| `r`              | refresh now                                         |
| `?`              | show all keys                                       |
| `q` / `esc`      | quit                                                |

### Logs

`enter` opens the selected job's log full screen. While the job runs, it reloads every few
seconds and stays at the end; scroll up to stop following. `e` switches between stdout and
stderr, `r` reloads, `esc` closes. Scroll with `↑↓` / `j` `k`, `pgup` `pgdn`, `g` `G`.

Logs are colored. If your program prints its own colors (rich, loguru, colored tracebacks…)
you see those. Otherwise gpudash highlights timestamps, log levels, errors and tracebacks,
warnings, numbers, strings, paths, URLs, `key=value` pairs and progress bars.

gpudash finds a job's log even after Slurm has forgotten the job: it remembers every log path
it sees, and as a last resort looks for a `.out` / `.log` / `.err` / `.txt` file with the job
id in its name in the job's folder.

### Command palette

Press `k`, type a few letters to search (`clock`, `add`, `notif`…), choose with `↑↓`, run
with `enter`, go back with `esc`. Items ending in `›` open a short list of choices. The
current value of each setting is shown on the right.

| group       | what's in it                                                                    |
| ----------- | ------------------------------------------------------------------------------- |
| **job**     | open log, shell, cancel, copy job id, copy folder path (for the selected job)   |
| **view**    | refresh now, pause / resume auto-refresh, filter                                |
| **setting** | refresh interval, ended jobs, how many recent, notifications (all / on queue / on start / sound), °C or °F, 12h or 24h clock, temperature graph |
| **cluster** | add a cluster, remove one, set a cluster's folder prefix, reconnect ssh         |
| **app**     | send a test notification, edit config file, keyboard shortcuts, quit            |

Settings are saved to the config file right away. *Pause auto-refresh* lasts until you quit.

## Notifications

While gpudash is open (it can sit in a background terminal tab), it notifies you on macOS
when:

| event                          | example                                                   |
| ------------------------------ | --------------------------------------------------------- |
| one job is queued              | **◌ sweep-lr queued** · explorer · 1× H200 · starts in 20m |
| several jobs are queued at once | **◌ 5 jobs queued on explorer** · sweep-lr, eval-big       |
| a job starts                   | **▶ sweep-lr started** · explorer · job 11005358           |
| a job finishes                 | **✓ sweep-lr finished** · explorer · job 11005358          |
| a job fails                    | **✗ sweep-lr failed** · exit 1:0 · explorer · job 11005358 |
| a job runs out of memory       | **✗ sweep-lr ran out of memory**                           |
| a job hits its time limit      | **⏱ sweep-lr hit its time limit**                          |
| a job is cancelled             | **sweep-lr cancelled**                                     |

Turn each kind on or off from the command palette (`k` → type `notif`): all
notifications, queued, started, and sound. `n` turns them all off and on quickly. To check
they work, use **Send a test notification** in the palette.

**About the icon.** macOS puts the Script Editor icon on any notification sent with
`osascript`, and that can't be changed. So gpudash carries its own tiny notification app
(a few dozen lines of Swift in [`notifier/main.swift`](notifier/main.swift)) whose only job
is to post a notification with the gpudash icon. It's compiled when you install gpudash and
set up the first time it's needed. You can manage its permission in **System Settings →
Notifications → gpudash**.

## Configuration

Everything lives in `~/.config/gpudash/config.toml` (or `$XDG_CONFIG_HOME/gpudash/`). You
rarely need to edit it by hand, since the command palette changes and saves these for you.
These are the defaults:

```toml
refresh_seconds = 30        # how often to poll the clusters
ended_jobs = "recent"       # jobs that ended in the last 24h: "recent", "all" or "hidden"
recent_ended = 5            # how many ended jobs per cluster "recent" shows
notifications = true        # macOS notifications at all
notify_on_queue = true      #   ...when new jobs are queued
notify_on_start = true      #   ...when a queued job starts
notify_sound = false        # play a sound with notifications
temperature_unit = "C"      # "C" or "F"
clock = "12h"               # "12h" or "24h"
temperature_graph = true    # temperature graph for running jobs

[[cluster]]
host = "explorer"           # ssh host name
prefix = "/projects/me/"    # optional
```

To edit it by hand while gpudash is running, use **Edit config file** in the palette: it
opens the file in `$EDITOR` and reloads it when you close the editor.

## How it works

On each refresh, gpudash sends one small shell script to each cluster's login node over
ssh. It runs:

- `squeue` for your active jobs (state, times, GPUs, folder, estimated start) and log paths
- `sacct` for jobs that ended in the last 24 hours
- `srun --jobid=<id> --overlap nvidia-smi …` for each running job, to read its GPUs. This
  runs inside your existing job, so it uses no new resources and only sees your job's GPUs.

Clusters are polled at the same time, and the screen keeps the last results while a refresh
runs. gpudash keeps one shared ssh connection per cluster open for 10 minutes after its last
use. Every ssh step has a time limit, and the shared connection closes itself if the cluster
stops answering, so a dropped Wi-Fi or a sleeping laptop shows up as a short error, not a
frozen box.

Files gpudash writes on your laptop:

| path                                                 | what                                     |
| ---------------------------------------------------- | ---------------------------------------- |
| `~/.config/gpudash/config.toml`                      | settings and clusters                    |
| `~/.cache/gpudash/<cluster>.tsv`                     | log paths it has seen, to find old logs  |
| `~/Library/Application Support/gpudash/gpudash.app`  | the notification app (macOS)             |

## Requirements and limits

- **Clusters**: Slurm with `squeue`, `sacct` and `srun --overlap` (Slurm 20.11 or newer).
  GPU readings need `nvidia-smi` on the compute nodes. Tested on Slurm 23.11 and 26.05.
- **Your laptop**: macOS or Linux. Notifications and copying (`pbcopy`) are macOS only; on
  Linux they quietly do nothing.
- Only the first node of a multi-node job is checked for GPU readings.
- Some clusters have a slow `squeue` (several seconds). That cluster's box just updates
  later than the others.

## License

MIT
