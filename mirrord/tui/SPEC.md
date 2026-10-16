# mirrord-tui — Specification

A terminal user interface for [mirrord](https://github.com/metalbear-co/mirrord).

This document describes the behavior of the application as a user experiences it.

## Running

There are two ways in:

- `mirrord tui`, the CLI subcommand;
- the standalone `mirrord-tui` binary (`cargo run -p mirrord-tui`), which is what the crate builds on
  its own.

Both run exactly the same interface. The only difference is logging (below).

Either way the application is launched from a terminal. It takes over the terminal (alternate screen, raw
input) while it is running and restores the terminal on exit, including when it exits due to an error. When
it was launched as `mirrord tui`, everything it took over — the terminal, the process's standard error, and
the panic hook — is handed back to the CLI before the subcommand returns.

Two environment variables affect startup **of the standalone binary**:

| Variable | Effect |
| --- | --- |
| `MIRRORD_LOG_FILE` | If set, diagnostic logs are written to the given file path. If unset, no logs are written anywhere. |
| `MIRRORD_LOG` | Log filter directive (same format as typical env-filter loggers, e.g. `debug`, `info`, `mirrord=trace`). Only takes effect when `MIRRORD_LOG_FILE` is set. |

Under `mirrord tui` these are not read: the CLI has already installed the process's logger by then, and the
interface does not replace it. `MIRRORD_LOG` still selects the level, since it is the CLI's own filter, but
the CLI writes its log to standard error — which the interface has taken over (below) — so those lines land
in the captured-output buffer rather than on the terminal. To read a `mirrord tui` session's logs, run
`mirrord-console` and set `MIRRORD_CONSOLE_ADDR`, which sends them over the network instead of to stderr.

While the application owns the terminal, its standard error is redirected away from it (on unix), so that
output from processes it starts — notably the kubeconfig's auth exec plugin, which Kubernetes clients run
with the caller's stderr inherited — cannot paint over the interface. The last lines written there are kept
in memory and shown in the connection error dialog. They are deliberately *not* logged: under `mirrord tui`
the CLI's logger writes to standard error, which is this same redirected pipe, so logging a captured line
would feed it straight back in without bound. The terminal's own stderr is restored before the application
exits, including when it exits by panicking, so error and panic messages still reach the terminal.

Example (from the README), run from this crate's directory:

```bash
MIRRORD_LOG_FILE=/tmp/mirrord-tui.log MIRRORD_LOG=debug cargo run
```

## Screen layout

Every frame is a sidebar on the left and the active view on the right, with the key bar along the bottom:

| Region | Size | Content |
| --- | --- | --- |
| Sidebar | 22 columns, 5 when the terminal is narrower than 90 | Every screen, where the user is, and how the connection is going (see below). |
| Body | the rest | The active view, framed in a rounded panel titled like `sessions[2]`. |
| Command line | 1 line, only while open | The `:` command line (see below). |
| Key bar | 1 line | The keys of whatever has the keyboard, as `<key> action`, keys in indigo. It starts with `<?> help` and ends with `<:> go to`, so the way to everything else is never the part cut off on a narrow terminal. |

Views are reached from the sidebar (a click), by name on the `:` command line (`home`, `run`, `targets`,
`sessions`, `previews`, `queues`, `dbs`, `config`, `resources`, `terminal`), or with `Tab` / `Shift+Tab`, which
step through them in the sidebar's order, skipping `terminal`: its shell takes every key, so cycling into it
would leave no key to cycle out with. The application opens on `run` when it was started inside a project (a
`package.json`, `Cargo.toml`, `go.mod` or `pyproject.toml` names it, or the folder has a command to run), and
on `home` anywhere else.

**Panels.** Views are framed in a rounded panel whose top border carries the view's name on the left, bold
indigo, with the number of rows after it in amber brackets (`sessions[2]`, or `[1/3]` while a search
narrows the list), and when the list was last refreshed on the right (`updated 13:01:02`, local time). A
short-lived message, such as the outcome of killing a session, sits on the bottom border.

## Colors

Screens that have been implemented are styled with the MetalBear brand palette: indigo `#756DF3` for panel
borders and titles, its lighter tint `#A8A5F7` for table headers, the brand dark `#2E2A5E` behind the selected
row, mint `#7DD3A8` for healthy states, amber `#FFCB7D` for in-progress states and warnings, and coral `#F2777A`
for errors. Only foreground colors, the selection background and small chips are painted, so the terminal
keeps its own background.

Colors mean the same thing on every screen, so a glance says where to look: mint is done, ready or safe;
amber is a guess worth checking or something still missing; coral is failed, or a choice that affects other
people; teal `#4DB6B0` and sky `#5B9DF3` are things in the cluster (targets and namespaces); indigo is the
brand, keys and focus. Most text stays lavender or grey: color goes on borders, short values and chips (a
short label on a solid color, such as `● READY`), not on whole paragraphs.

How much of that color reaches the terminal is decided once per process, from the environment, first match
wins:

| Environment | Colors drawn |
| --- | --- |
| `NO_COLOR` set to anything non-empty | None. Bold, dim and reverse video stay, so the selection and headers still stand out. |
| `COLORTERM` containing `truecolor` or `24bit` | The 24-bit brand colors. |
| Anything else | The 16 named terminal colors: each brand color has a hand-picked one (indigo becomes bright magenta, amber bright yellow, mint bright green, coral bright red, the selection background blue), and any other color becomes the nearest named one. |

This applies to everything on screen, every screen and dialog and the Terminal screen's shell output
included, since on a terminal without 24-bit support a 24-bit color is drawn as one flat grey.

## Sidebar

A rounded column down the left edge, top to bottom:

- the `◆ mirrord` chip (indigo);
- `⌂ Home`, then the screens under three headings: **START** `▶ Run`, `◎ Targets`; **WATCH** `◉ Sessions`,
  `◇ Previews`, `≋ Queues`, `▤ Databases`; **SETUP** `≡ Config`, `▦ Resources`, `❯ Terminal`. Every icon is
  one column wide on every terminal (no emoji). The current screen has an indigo `▌` bar and a filled row.
  The WATCH screens show how many things they hold at the right edge in mint (the Home dashboard's counts:
  sessions, previews, queue splits, database branches), when the count is known and not zero;
- at the bottom: the Kubernetes context (the kubeconfig's current one when none was picked), the namespace
  (italic `all namespaces` when none was picked), and the connection state:
  - `◌ connecting…` (amber) while a connection attempt is in flight and no result has arrived yet;
  - `● connected` (mint) while the most recent connection attempt succeeded;
  - `✗ can't connect` (coral) if the most recent attempt failed, followed by its summary (at most three
    lines, the last cut with `…`) and `<e> details`.

Clicking a screen goes there (unless a dialog is open). Narrower than 90 columns, the sidebar folds to
the icons and the connection's sign alone.

The summary is a condensation of the failure, never the error verbatim. Errors built around a payload are cut
back to the classification in front of it: a `kube` auth error quotes the entire failed credential-plugin
command, `KUBERNETES_EXEC_INFO`'s JSON included, so `auth error: auth exec command '<hundreds of characters>'
failed with status ...` is shown as `auth error: auth exec command`.

## Connection error dialog

Pressing `e` while the sidebar reports a failed connection opens a centered modal dialog, red-bordered
and titled `connection error`, over the body area. It captures all input while open; `Esc`, `q` and `e`
close it. A new connection attempt (triggered by changing the context or namespace) closes it too, since the
failure it describes is no longer the current one. It is never drawn over a context or namespace picker.

It shows, in order:

- the same summary as the sidebar, in bold;
- **captured output** — the last lines written to the inherited stderr, which for a failing credential
  plugin is where the actionable instruction (`Please run: $ gcloud auth login`) lives. Omitted when nothing
  has been captured;
- **details** — the full error chain, flattened onto one line and cut off after 400 characters. The
  untruncated version is in the log.

Everything is wrapped to the width of the dialog, breaking mid-word for runs (like the quoted JSON payload)
too long to fit a line.

## Help dialog

Pressing `?` opens a centered modal dialog titled `Keys`, drawn like the context and namespace pickers. It
lists the keys the active screen answers to in its current state under the screen's name (left out for
screens that list none), then the keys from the table below under `Everywhere`. It captures all input
while open; `Esc`, `q` and `?` close it. The Preview Environments screen answers `?` with its own help
dialog instead.

## Command line

Pressing `:` opens a command line above the key bar, with a list of suggestions above it that narrows as
the user types: view names first by prefix, then by any match, each with what it shows (`queues  queue
splits`). The rest of the highlighted suggestion is ghosted after the cursor.

| Typed | Does |
| --- | --- |
| a view name or alias (`run`/`start`/`go`, `sessions`/`ss`, `queues`/`qs`/`splits`, `dbs`/`databases`/`branches`, `previews`/`pv`, `targets`/`up`/`plan`, `config`/`cfg`, `resources`/`crds`/`api`, `terminal`/`term`/`shell`, `home`) | Switches to that view. |
| a mirrord resource type, by its plural or its kind in lower case (`mirrordsplitconfigs`, `mirrordsplitconfig`, `branchdatabases` ...) | Opens the Resources view on that type. The types are the ones the connected cluster serves, so the list follows what is installed. |
| `ctx` / `context`, optionally followed by a context name | Switches to that context, or opens the context picker without one. Context names from the kubeconfig are suggested. |
| `ns` / `namespace`, optionally followed by a namespace or `all` | Narrows lists to that namespace (`all` for every namespace), or opens the namespace picker without one. The connected cluster's namespaces are suggested, when it lets the application list them. |
| `help` / `keys`, `error`, `quit` / `q` | Open the help dialog, the connection error dialog, or quit. |

`Up` / `Down` move through the suggestions, `Tab` completes the highlighted one (adding the space before an
argument when the command takes one), `Enter` runs what was typed or, when it names nothing exactly, the
highlighted suggestion (`:se` then `Enter` reaches sessions), and `Esc` closes the line. While it is open it
takes every key.

## Command palette

Pressing `Ctrl+P` opens a filterable pick-one dialog titled `Command`, drawn and driven like the
context picker: typing narrows the list to entries containing the typed text (ignoring case), `Up` /
`Down` move, `Enter` runs the selected entry, and `Esc` clears the filter first and then closes. The
entries are:

| Entry | Does |
| --- | --- |
| `Go to <screen>`, one per screen in tab order | Switches to that screen. |
| `Switch kube context  (c)` | Opens the context picker. |
| `Switch namespace  (n)` | Opens the namespace picker. |
| `Show connection error  (e)` | Opens the connection error dialog, if the last connection attempt failed. |
| `Show keys for this screen  (?)` | Opens the help dialog. |
| `Quit  (q)` | Quits the application. |

The key in parentheses does the same thing without the palette.

## Quitting with unsaved changes

When `q`, `Ctrl+C` or the palette's `Quit` would leave edits unsaved (the Config screen's, today), a
centered `Unsaved changes` dialog asks first, listing what would be lost. It captures all input while open:

| Key | Action |
| --- | --- |
| `s` | Save, then quit. If a save fails, the dialog stays open and shows why (`Not saved: ...`). A config file mirrord rejects still counts as saved. |
| `q` / `Ctrl+C` | Quit without saving. |
| `Esc` | Close the dialog and stay. |

## Keys

The following keys are handled globally, regardless of which screen is active:

| Key | Action |
| --- | --- |
| `q` | Quit the application, asking first when there are unsaved edits. |
| `Ctrl+C` | Quit the application, asking first when there are unsaved edits. |
| `Tab` | Move to the next screen (wraps from the last back to the first), skipping the Terminal. |
| `Shift+Tab` | Move to the previous screen (wraps from the first back to the last), skipping the Terminal. Terminals that report it as `Tab` with Shift held work too. |
| `e` | Open the connection error dialog. Does nothing unless the last connection attempt failed. |
| `c` | Open the kube context picker. |
| `n` | Open the namespace picker. Does nothing until connected. |
| `?` | Open the help dialog. |
| `:` | Open the command line. |
| `Ctrl+P` | Open the command palette. |

The active screen sees every key first, and the global handling above only runs for keys the screen did not
consume. Screens that handle keys of their own list them in their own section below. The Terminal screen
consumes *every* key while the shell has the keyboard, so none of the global keys are available there until
`Ctrl+B` is pressed (see below).

Key releases are ignored; only presses and auto-repeats are acted on.

**Text boxes** (search lines, the command editor, the `:` line's text) all edit the same way: `Ctrl+A`
selects everything, `Shift` with `←` `→` `Home` `End` selects, typing or pasting replaces the selection,
`Backspace`/`Delete` remove it, `Ctrl`/`Alt` with the arrows move by word, `Ctrl+W`, `Alt+Backspace` and
`Ctrl+Backspace` delete the word before the cursor, `Ctrl+U`/`Ctrl+K` delete to the start/end, and `Ctrl+E`
jumps to the end. With the mouse, a click places the cursor, a drag selects and a double-click selects a
word.

**The mouse** is captured while the interface runs, so lists take clicks, double-clicks and the wheel. The
terminal's own text selection still works with `Option` (macOS) or `Shift` held.

## Screens

### Home

A dashboard of what mirrord is doing in the cluster, in a `home` panel, refreshed every 5 seconds and when
the connection or the namespace changes (a picked namespace narrows every card; otherwise every namespace
counts). Each card is read on its own, so a resource the cluster cannot list leaves only its card with the
explained error (`The mirrord operator is not answering. Is it running?`), and the rest still show.

| Card | Shows |
| --- | --- |
| `<1> SESSIONS` | How many mirrord sessions are running (preview sessions left out), and `N yours` (mint) when some belong to the person running the TUI: any part of the session's `username/k8sUsername@hostname` equals their login name (`$USER`). |
| `<2> PREVIEWS` | How many preview environments there are, and how many in each phase, colored by phase. |
| `<3> QUEUE SPLITS` | How many queue splits there are, and `N failed` (red) when any have failed. |
| `<4> DB BRANCHES` | How many branch databases there are, and how many in each phase. |
| `OPERATOR` | `operator <version>`, then `license <organization> · until <date> (<N> days left)`, amber under 14 days and red (`expired`) past the date. |
| `RECENT ACTIVITY` | Up to 20 events, newest first, each with how long ago it was (`4m`, `21d`): `<user> started a session on <target>`, `preview <key> · <phase>`, `queue split <name> · <phase>`, `db branch <id> · <phase>`, colored by phase. Events are when each was created, or started for sessions. |

At 70 columns or more the four count cards sit two to a row; narrower, one per row. The activity feed takes
the rest of the height.

| Key | Action |
| --- | --- |
| `1` | Go to Sessions. |
| `2` | Go to Preview Environments. |
| `3` | Go to Queues. |
| `4` | Go to Databases. |
| `r` | Go to Run. |

### Run

Starts a mirrord session in three short steps, each one a list to pick from: **Folders** (what to run),
**Targets** (what it runs against) and **Config** (how mirrord runs it). Everything that can be guessed is
guessed and already picked, so the usual run is `Enter`, `Enter`, `Enter`.

**Layout.** The panel is titled `run`, with the namespace searched (`in staging`, or `in all namespaces`) on
the right. Top to bottom:

| Row | Shows |
| --- | --- |
| Step bar | `1 Folders › 2 Targets › 3 Config › ▶ run`. The current step is on an indigo chip, done steps are mint with what was picked (`Folders ✓ 2`), later ones grey; `▶ run` turns mint once the plan is ready. Clicking a step goes there. |
| Question | The step's question in bold (`Which folders do you want to run?`) and one muted line of help. |
| List | The step's choices (see below), with a search line on top. |
| Details | A pane about the row under the cursor, next to the list from 90 columns, under it on narrower terminals that are tall enough. |
| Tray | What is picked, as chips with a coral `✕` that unpicks on a click (`picked  checkout ✕  payments ✕`, `+N more` when they do not fit). On the config step, the status chip instead: `● READY` on mint, `NEEDS` on amber, `FAILED` on coral. |
| Legend | The step's keys drawn as keycaps (` space  pick   enter  next   esc  back`). |

**Lists work the same in every step.** Each row has its pick column in the same place: `[✓]` / `[ ]` where
several can be picked, `(●)` / `( )` where one is; picked rows turn mint.

| Input | Does |
| --- | --- |
| `Space`, or a click | Picks or unpicks the row (on the config step, chooses it) |
| `↑` `↓` `PgUp` `PgDn`, the mouse wheel | Moves |
| Typing | Searches: every typed word has to appear in the row (its title, description or hidden keywords); matches in the title come first, and the matching text lights up amber. `Esc` clears the search. With nothing typed, `:` and `?` still open the command line and help. |
| `Enter` | Goes to the next step; on the config step, runs |
| `Esc` (nothing typed), `←` | Goes back a step |
| Double-click | Opens a folder, or picks a target |
| `Ctrl+A` (nothing typed) | Picks every folder shown, or none if all are picked |

Plain letters always go to the search, so a step's own actions are on `Ctrl` chords. The search line is a
full text box, like every other one in the interface: `Ctrl+A` selects all, `Shift`+arrows select, typing replaces
the selection, a click places the cursor, a drag selects and a double-click selects a word.

**1 Folders.** Opens on `projects` (`found on this computer and used before`), in sections: `picked` (so
each can be unpicked here even when it was found elsewhere), `used before` (folders something ran from,
marked `ran before` in mint), `projects` (found under the home folder), and `browse` with `~/ your home
folder`. Each row shows the file that makes it a project (`package.json`, `Cargo.toml`, `go.mod`,
`pyproject.toml`, `requirements.txt`, `pom.xml`, `build.gradle`, `Gemfile`, `composer.json`, `mix.exs`,
`Dockerfile`) in soft indigo; folders without one are grey.

- The search for projects runs once in the background when the application starts, at most 5 folders deep
  under the home folder, skipping hidden folders and `node_modules`, `target`, `vendor`, `dist`, `build`,
  `out`, `venv`, `Library` and the like, and stops after 4 seconds, 30,000 folders or 2,000 projects. Until
  it is done, the location line says `still looking in your home folder...`.
- `→` or a double-click goes into a folder, `←` (or `Backspace` with nothing typed) goes up with the cursor
  on the folder just left, and `~` or `/` typed first jumps to the home folder or the root. Inside a folder
  the location line says `in ~/code/shop`, the first row is `.` (the folder itself, to pick it), then its
  subfolders. `Esc` goes back to the project list.
- The details pane shows the project's name, its folder, its project file and the command it would run.
- `Enter` with nothing picked takes the folder under the cursor and goes on.
- The folder the application was started in is already picked when it is a project (a manifest names it,
  or it has a command to run), and the view then opens on the targets step.

**2 Targets.** Every target found, in sections:

- `suggested`: workloads named like a picked folder's project, best first, each with `★` (mint, a sure
  match: run from that folder before, or named exactly like it) or `☆` (amber, a looser match such as `close
  to` or `shares a word with`) and the reason (`named like echo-app`; `for checkout · ...` when several folders
  are picked). Pods and replica sets are never suggested: their workload is the better pick.
- `all targets`, then `pods and replica sets`, each sorted by namespace and path.

Target paths are teal and namespaces sky blue. Each folder's sure match is ticked once its targets are
known, unless the folder already has a target; unpicking it is never undone. A picked target goes to the
folder it fits best that has no target yet, or replaces the target of the folder it fits best when every
folder has one; the tray says which (`checkout → deployment/checkout ✕`) and `Ctrl+F` moves the target under
the cursor to the next folder, swapping targets if that folder has one. Folders without a target run with no
incoming traffic. The details pane shows the target's namespace, kind, why it is suggested, which folder it
runs for, and what reading it from the cluster found: its databases (with where their credentials come
from) and queues.

While there is nothing to list: `Waiting for the cluster... Enter goes on without a target.`, `Looking for
targets...`, the error when listing failed, or `Nothing to target here. Try :ns, or Enter to go on without a
target.` Discovery runs again whenever the connection or the namespace changes.

**3 Config.** One config is chosen, the first one by default. Configs `made from your picks` (marked `✦`):

| Config | Offered | Does |
| --- | --- | --- |
| `Only my requests` | when a target is picked; marked ` recommended ` | `mirrord up` split mode: requests with the session key reach you, and the targets' queues split the same way |
| `Only my requests + branch N databases` | when the picked targets use databases | the same, with every branchable database (or the team's presets) branched for this session |
| `Take over all traffic` | when a target is picked | replace mode: every request reaches you, the original is scaled down |
| `No incoming traffic` | always | targetless: the cluster's network, files and env only |

Then the `files in your folders` (marked `≡`): every `.json`, `.yaml`, `.yml` or `.toml` file in a picked
folder's `.mirrord` directory, titled by its path and described by its first options
(`feature.network.incoming: steal · +3 more`). A file applies to its own folder, as a `config_patch` with its
whole content; other folders run with only their own requests.

Every config carries the words a search should find it by: the made ones what they do (`split`, `steal`,
`takeover`, `targetless`, `db`, `database`, `branch`) plus the picked targets' engines, database variables
and queue names; files every key and value they hold. So `db`, `postgres`, `kafka`, `steal` or a variable
name narrows the list to the configs that do it.

The details pane (`what will run`) lists each folder: its path, `target` and `in` (namespace), `traffic`
(`only yours (session key)`, `all, original scaled down` or `none`, in the job's color), `runs` with the
command and where it came from under it (`ran last time` or `your pick` in mint, `detected - check it` in
amber), and `db  N branched` or `config  from the file` when the config adds something. `Ctrl+Y` switches it
to the exact YAML mirrord reads, and back. `Ctrl+E` changes a folder's command (asking which folder first
when there are several): a picker of past and detected commands plus `type my own command...`, which opens a
`Command for <folder>` box with the current command selected.

The plan has one service per folder, named after its target (or the project when there is none), with `-2`,
`-3` ... added when two would share a name; each runs in its own folder. It needs a folder and, for each
folder, a command: `NEEDS  a command for checkout - ctrl+e` otherwise.

**Databases and queues.** Each picked target, and the target under the cursor on the targets step, is read
from the cluster once in the background (read-only):

- *The team's presets come first.* A `MirrordSplitConfig` in the target's namespace that names the target
  (`targetRef`) or selects it by labels (`targetSelector.matchLabels`) lists databases under `dbBranches`;
  when any apply, they are the databases offered, and branching them writes their ids.
- *Otherwise the pod spec is read*: the targeted container (or every container but mesh sidecars), with the
  keys of Secrets and ConfigMaps it loads through `envFrom`. Secret values are never kept or shown; only key
  names are used.
  - A variable ending in `_URL`, `_URI`, `_DSN` or `_CONNECTION_STRING` is a database when its plain value
    starts with a database scheme (`postgres://`, `jdbc:mysql://`, `mongodb+srv://` ...) or its name names
    an engine (`PG`, `POSTGRES`, `MYSQL`, `MARIA`, `MONGO`, `MSSQL`, `CLICKHOUSE`, `REDIS`).
  - Variables sharing a prefix and ending in `HOST`, `PORT`, `USER`, `PASSWORD`, `NAME`/`DB`/`DATABASE`
    (`ORDERS_DB_HOST` ..., or `PGHOST`, `PGUSER` ...) are one database, when the prefix names an engine or
    the port is an engine's usual one (5432, 3306, 27017, 1433, 6379 ...). A prefix already covered by a URL
    variable is not counted twice.
  - Each database's credentials are shown by source: `url from secret app/url`, `password from configmap
    db/pass`, `url from secret app-env via envFrom`, or `set in the pod spec` (a plain value is never shown).
  - When no password was found (no password variable, or a plain URL without one) and the pod has a cloud
    identity, the branch signs in with it: an IRSA role on the service account, the EKS web-identity
    variables or static AWS keys mean AWS (`iam_auth: aws_rds`); a GKE Workload Identity annotation or a
    Cloud SQL proxy sidecar means Google Cloud (`iam_auth: gcp_cloud_sql`). The details pane says so (`signs in with
    the pod's AWS role (IRSA)`).
- *Queues* come from the same split configs (`queues`) and from older `MirrordWorkloadQueueRegistry`
  resources whose consumer is the target.

Branching writes a `config_patch` with `feature.db_branches` into the plan: the chosen presets' ids, or one
entry per chosen database with its engine, its connection (`url` with `type: env` or `env_from` and the
variable, or `params` naming each variable) and `iam_auth` when it signs in with a cloud identity. Queues need
nothing in the plan: in "only my requests" mode, mirrord splits every queue of the target by session key on
its own.

**How the guesses are made.**

- *Project names*, most telling first: the `name` in `package.json` (an npm scope such as `@team/` is left
  off), `[package].name` in `Cargo.toml`, `[project].name` in `pyproject.toml`, the last part of the `module`
  in `go.mod`, then the folder's own name.
- *Where to look*: the namespace picked with `:ns`, or, when none is, every namespace except `kube-system`,
  `kube-public`, `kube-node-lease` and `mirrord`. A cluster with more than 30 such namespaces, or one that does
  not allow listing them, is searched in its default namespace only.
- *Targets* are listed once, the same way the Targets view lists them, and ranked for each folder against
  its own names: a target run from that folder before first (`ran from here before`), then one named exactly like a project name (`named like checkout
  (package.json)`), then one whose name contains a project name or the other way round (`close to`), then
  one sharing a word with it (`shares a word with`). Deployments and rollouts win ties over
  statefulsets, services, pods and jobs. Targets matching nothing are still offered, last.
- *Containers*: in a workload with several, mesh sidecars and mirrord's agent (`istio-proxy`,
  `linkerd-proxy`, `envoy`, `mirrord-agent`, `cloud-sql-proxy`) are never picked; one named like the
  project wins, else the first, and the reason says which (`container app, sidecars skipped`).
- *Commands*: the same detection the Targets view offers (`npm run dev`, `cargo run`, `go run .` ...).

**Running.** `Enter` on the config step writes the plan to a temporary file and starts `mirrord up -f` on it,
exactly as the Targets view does, records each folder's command in its target's history, and replaces the
steps with the run's log pane, which starts with `$ mirrord  (plan: <file>)`. `x` stops it, `Up`/`Down`
scroll, `G` follows the tail, and `Esc` returns to the steps once it has stopped. If mirrord cannot be
started, the reason is shown on the config step instead.

### Targets

A wizard for assembling and running a multi-service session.

It lists everything mirrord can target in the current namespace as a flat tree with per-kind
badges. Choosing a target opens a settings dialog, prefilled from what was last run against that
target, and each confirmed target is appended to an ordered plan. `/` filters the list; the filter
line is the shared text input, so it edits like every other box.

The settings dialog (and the export dialog) is a form of labelled fields built from the shared
pieces, so it behaves like the rest of the interface:

- `↑`/`↓` (or a click, or the wheel) move between fields; the last row finishes (`Add to plan`,
  `Save`, `Write file`).
- Text fields edit with the shared text input. `Enter` or a click opens the field (a click puts the
  cursor where it landed, a drag selects, a double-click selects a word), `Ctrl+A` selects
  everything and `Shift` with the arrows selects, also on a field not opened yet, and typing replaces
  the selection. `Enter` applies, `Esc` drops the edit. A value too long for the field scrolls
  behind a `…` to keep the cursor in view.
- `Tab` completes the last word against the filesystem, or cycles the field's suggestions (the
  Command field suggests what ran for the target before and what the folder's project files run).
  The candidates show on a row under the fields, and a click on one puts it in. On a field not
  being edited, `←`/`→` cycle its suggestions straight into the value.
- Fields with a few fixed options (Mode, Format, Operator, Insecure TLS) show every option with the
  same `(●)` / `( )` pick mark as the pick lists; `←`/`→` or `Space` change it, and a click on an
  option picks it.

Under the fields sit the focused field's help, any error (a value the field rejects, or a run that
needs a missing command, which opens that field with the reason), and a legend of the keys for what
is focused. The same keys are on the key bar.

The plan pane holds the services that make up the `mirrord-up.yaml` the screen emits. From there
the plan can be written to a file, or run directly, which starts `mirrord up` as a child process
and streams its output into a pane until it exits or is stopped.

The three panes are panels like every other view's: `targets[N]` with the cluster and namespace (and a
spinner while loading) on the right, `plan[N]` with `mirrord` on the right, and `run` with the run's
state (`● mirrord · running (pid ...)`, `✓ ...` or `✗ ...`) on its bottom border. The pane that has the
keyboard has an indigo border. Its keys, adapted to what is selected (`Enter target deployment/api`, `s
skip` or `s unskip`), are on the key bar, and `?` lists them in the shared help dialog. A status message,
when there is one, takes up to three lines under the panes.

### Config

Edits the config file under `.mirrord/` in the directory the application was started from: the first of
`mirrord.json`, `mirrord.yaml`, `mirrord.yml` and `mirrord.toml` that exists, read and written in the format
its extension names, as `mirrord exec -f` does. With none of them there, a new `mirrord.json` is created on
save - the file the IDE extensions use. Every option the config file
accepts is listed, with its documentation and the plan it needs, read from the config schema compiled into
the binary - the same types the CLI parses - so the screen lists exactly the options this build accepts.

**Layout.** A bordered `Config` panel titled with the file path, how many options have a value, and
`unsaved` (amber) while there are edits the file does not have yet. On the left, the options as a tree; on
the right, details of the option under the cursor. Below the panel, the search box while one is in use, a
status line, and the key hints.

**The tree.** Each row shows the option's key, indented under the section it belongs to, a fold marker
(`▸` folded, `▾` open) on rows that open into more options, a `team` or `enterprise` badge (amber) on options
that need that plan, and on the right the value: the value itself (indigo tint, key in bold), `N set` for a
section holding values, or `-` when the option is not set. The tree starts with every section folded.

A row opens into more options when it is a section, or when the option takes several forms and one of them
is a set of settings (`feature.network.incoming` is either on/off, a mode, or settings). Such an option opens
into its settings while it is unset or holds settings, and stays closed while it holds another form.

**Details.** The option's path, what it takes (`one of: mirror, steal, off`, `whole number from 0 to
65535`, `list of text`, or the forms it takes), the plan it needs (`needs mirrord for Teams`), its current
value pretty-printed or `not set - mirrord's default applies`, and its documentation. `PageUp` / `PageDown`
scroll the details.

The documentation is drawn from its markdown: paragraphs re-flowed to the panel's width, headings in bold,
`-`, `*` and numbered list items with their text indented under the marker, `>` quotes behind a `│`, fenced
code blocks indented and tinted (cut with `…` rather than wrapped), inline code tinted, `**bold**` and
`*italic*` styled, and links shown as their underlined label without the URL. `{#anchor}` suffixes on
headings and `<!-- -->` comments are left out.

**Editing.** What `Enter` does depends on what the option takes:

| Takes | `Enter` |
| --- | --- |
| A section | Folds or opens it. |
| On or off | Flips it; an unset option turns on. `Space` does the same. |
| One of a fixed set | Opens a `Choose a value` picker. |
| Several forms, while unset | Opens a `Choose a form` picker; picking one starts the option off as that form (on, the first value, an empty set of settings), or opens the editor when it needs typing. `v` opens this picker at any time. |
| Anything else | Opens a one-line editor in the details panel, prefilled with the current value. |

The editor takes text, numbers, comma-separated lists (`80, 443`), colon-separated pairs (`80:8080,
443:8443` for port mappings) and, for anything else, JSON. Its keys are the shared text editing keys
(see Queues). `Enter` keeps the value, refusing it with the reason in red when it does not fit (`70000 is
above the highest allowed value, 65535`); `Esc` cancels; an empty value unsets the option. `d` or `Delete`
unsets the option under the cursor. Unsetting removes the key from the file, and any section left empty by
that, so the option falls back to mirrord's default.

**Search and filters.** `/` searches option paths (`network.incoming` finds `feature.network.incoming` and
what is under it). When no path matches, the options' documentation is searched instead. `m` lists only the
options that have a value, and pressing it again lists them all. While searching or filtering, every section
with a match inside is shown open.

**Status line.** One to three lines, wrapped to the screen width:

| Situation | Status |
| --- | --- |
| The file exists but cannot be read as a map of options in its format (templates mirrord renders before parsing, a syntax error) | `✗ <path> could not be read (...)`, and saving is refused so the file is not replaced. |
| The options as edited would not parse as a config file (unknown key, wrong type) | `✗` and the reason, red. |
| Saved, and mirrord accepts the file | `✓ saved to .mirrord/mirrord.json and accepted by mirrord`, green. |
| Saved, but mirrord rejects the file | `✗ Saved, but mirrord rejects it: ...`, red. |
| A YAML or TOML file with `#` comments, otherwise valid | `✓ valid · saving rewrites <path> without its comments`, amber. |
| Otherwise | `✓ valid`, green. |

**Saving.** `Ctrl+S` writes the file, creating `.mirrord/` when needed, in its own format (pretty-printed
JSON, YAML or TOML) holding only the options that have a value. Keys set to `null` in the file are read as
not set, and are left out when it is written. It then runs the file through the same resolution and checks as `mirrord
verify-config` without `--ide`, which catches combinations no single option shows (steal mode without a
target, for instance). A file that saved but was rejected stays saved.

The screen handles these keys:

| Key | Action |
| --- | --- |
| `Down` / `j`, `Up` / `k` | Move the cursor. |
| `Home` / `End` | First / last option. |
| `Left` / `h` | Fold the section under the cursor, or move to the enclosing one. |
| `Right` / `l` | Open the section under the cursor, or move into it. |
| `Enter` | Edit, as described above. |
| `Space` | Flip an on/off option. |
| `v` | Choose which form an option takes. |
| `d` / `Delete` | Unset the option. |
| `/` | Search. |
| `m` | List only options that have a value, or all of them again. |
| `Ctrl+S` | Save. |
| `PageUp` / `PageDown` | Scroll the details. |

### Sessions

The view the application opens on: the mirrord sessions the operator reports, refreshed every 5 seconds and
immediately whenever the cluster connection or the selected namespace changes. Preview environment sessions
are left out; they have their own view. If a namespace is selected, only that namespace is listed;
otherwise sessions from all namespaces are listed, sorted by namespace and then by name so that a refresh
never reshuffles rows under the selection.

| Column | Content |
| --- | --- |
| `NAMESPACE` | Namespace of the session, or `-` (gray). |
| `ID` | The session's id (lavender). |
| `USER` | The developer who started it. |
| `TARGET` | The workload it targets (light indigo). |
| `PORTS` | The ports the session holds, `(filtered)` after a port with an HTTP filter, in amber; `-` when it holds none. |
| `KEY` | The session key, or `-` (gray). |
| `AGE` | How long it has been running, e.g. `45s`, `4m`, `3h25m`, `2d2h` (gray). |

The table, the `/` search box (matching every column) and the messages shown instead of the table work as on
the Queues view, with `No active sessions` when the listing is empty and `No sessions match the search` when
the search excludes every row.

| Key | Action |
| --- | --- |
| `Enter` | Describe the selected session: the whole resource as YAML (keys tinted), in a panel titled `describe(session <name>)` with the visible line range on the right. `Up`/`Down`/`j`/`k` scroll, `PageUp`/`PageDown`/`Space` page, `g`/`G` jump to the ends, `/` searches (matching lines in bold amber, the view jumping to the first), `n` jumps to the next match, `Esc` clears the search and then goes back. |
| `Ctrl+D` | Ask to kill the selected session: a red dialog `Kill session <name>?` with its user and target. `Enter` or `y` sends the same request `mirrord session kill` does; `Esc` or `n` cancels. The outcome, `killed session <name>` (mint) or `could not kill <name>: <reason>` (red), stays on the panel's bottom border for 6 seconds. |
| `r` | Refresh now. |

### Databases

Lists the branch databases the operator reports, in a `dbs` panel, as a table that can be narrowed by
search. The table (its selection keys and cells cut short with `…`), the search box and the messages shown
instead of the table work exactly as on the Queues view, with `No active branch databases` when the listing
is empty and `No branch databases match the search` when the search excludes every row. `Enter` opens the
details of a branch, in a panel titled `branch(<name>)`; `y`, from the list or the details, shows the branch
whole as YAML in the shared describe view (see Sessions).

### Queues

Lists every queue-splitting session the mirrord operator reports, in a `queues` panel. The list is
read from the `queuesplits` resource of the operator's aggregated Kubernetes API — the same live data behind
`mirrord queues status`.

The panel title carries the number of splits, or `shown/total` while a search narrows the list, and when the
list was last refreshed. The keys are on the key bar.

Splits are listed in one row each, sorted by namespace and then by name so that a refresh never reshuffles rows
under the selection. The columns are:

| Column | Content |
| --- | --- |
| `NAME` | Name of the split. |
| `SESSION` | The mirrord session id the split belongs to. |
| `USER` | The developer who started the session, as `username/k8sUsername@hostname`. |
| `NAMESPACE` | Namespace of the split. |
| `TARGET` | Targeted workload, as `Kind/name`, or the label selector for a set of pods. |
| `PHASE` | `Pending` (amber), `Ready` (green), `Failed` (red), gray for a phase this build does not recognise, or `-` if the operator reported no status yet. |
| `QUEUES` | How many queues the operator resolved for the split. |
| `PODS` | How many of the target pods are patched and ready, out of the total. Green when all of them are, amber otherwise. |
| `DURATION` | How long the split has existed, e.g. `45s`, `4m12s`, `3h25m`, `2d2h`. |

The selected row is highlighted. When there are rows and nothing is selected, the first row is selected; when the
list shrinks past the selection, the last row is selected instead.

A cell that is too long for its column — a column header included — is cut short and ends in `…`, so a narrow
terminal never passes a clipped value off as the whole one.

Instead of the table, the body shows a centered message when there is nothing to list:

| Situation | Message |
| --- | --- |
| No cluster connection yet | `Waiting for a connection to the cluster...` (gray) |
| The first listing is still in flight | `Loading...` (gray) after a turning spinner |
| The cluster answered 404, i.e. it does not serve queue splits | `This cluster does not serve queue splits. Is the mirrord operator installed?` (amber) |
| The operator's API answered 503, i.e. nothing is serving it | `The mirrord operator is not answering. Is it running?` (red) |
| The listing was refused with 403 | `Not allowed to list queue splits: <reason>` (red) |
| The listing failed for any other reason | The error message (red) |
| The listing succeeded but found nothing | `No active queue-splitting sessions` (gray) |
| Every split was filtered out by the search | `No queue-splitting sessions match the search` (gray) |

The list is refreshed every 5 seconds, and immediately whenever the connection scope changes or a new connection
attempt finishes. If an explicit namespace is selected, only that namespace is listed; otherwise splits from all
namespaces are listed.

The list handles the following keys:

| Key | Action |
| --- | --- |
| `Down` / `j` | Select the next split. |
| `Up` / `k` | Select the previous split. |
| `Home` / `g` | Select the first split. |
| `End` / `G` | Select the last split. |
| `PageDown` / `PageUp` | Scroll the list by 10 rows. |
| `Enter` | Open the details of the selected split. |
| `y` | Show the selected split whole as YAML, in the shared describe view (see Sessions). |
| `/` | Start typing a search phrase. |
| `Esc` | Clear the search phrase. |
| `r` | Refresh the list now. |

#### Search

`/` opens a search box on the line between the panel and the footer, showing `/`, the phrase typed so far, and a
block cursor. Every keystroke narrows the list right away: a split is listed only when its `SESSION`, `USER`,
`NAMESPACE`, `TARGET` or `PHASE` cell contains the phrase, each matched on its own, ignoring case. The selection
returns to the first row on every change of the phrase.

While the box takes input, every key goes to it — `q` does not quit and `Tab` does not switch screens. The
phrase is edited like a shell's command line, with the cursor drawn where typing goes:

| Key | Action |
| --- | --- |
| (any character) | Insert at the cursor. |
| `Backspace` / `Delete` | Delete the character before / under the cursor. |
| `Left` / `Right` | Move by a character; with `Alt` or `Ctrl`, by a word. |
| `Home` / `End`, `Ctrl+A` / `Ctrl+E` | Move to the start / end. |
| `Alt+B` / `Alt+F` | Move back / forward by a word. |
| `Ctrl+W` | Delete the word before the cursor. |
| `Ctrl+U` / `Ctrl+K` | Delete everything before / after the cursor. |
| `Enter` | Stop typing, keeping the list narrowed. |
| `Esc` | Stop typing and clear the phrase, listing every split again. |

Pasted text is inserted at the cursor, with line breaks turned into spaces.

The box stays on the screen with the phrase in it, cursor hidden, so that the list is never silently narrowed. It
is hidden while the split details are open, and the phrase still applies to the list underneath.

#### Split details

`Enter` replaces the list with a nested view of everything the operator reports about the selected split. The
panel is titled `split(namespace/name)`. The details are re-read from
the same background refresh as the list, so they keep up with the split while the view is open.

The details are laid out as a label column and a value column, in sections:

| Section | Fields |
| --- | --- |
| (top) | `Name`, `Namespace`, `Session`, `Phase` (colored as in the list), `Message` (only when the operator reported one, colored as the phase), `Created` (the creation timestamp in UTC, followed by the age in parentheses). |
| `OWNER` | `Username`, `Kubernetes user`, `Hostname`, `User ID`. |
| `TARGET` | `Kind`, `Name` and `API version` for a Kubernetes resource; `Kind` (`PodSet`) and `Label selector` for a set of pods. Then `Container`, or `-` when the session did not resolve one. |
| `QUEUES (n)` | One entry per queue the operator resolved, headed by the queue id and its broker type. Below each, whichever of `Queue`, `Topic`, `Consumer group` and `Subscription` the broker type has. |
| `FILTERS (n)` | One entry per filter from the user's mirrord config, headed by the filter id and its broker type. Below each, one line per message filter (attribute name as the label, regex as the value) and a `jq` line when the filter has a jq expression. |
| `TARGET PODS (n/m)` | How many of the target pods are patched and ready, out of the total, then one line per pod: its name and `patched, ready` (green) or the failing combination (amber). A pod that is not ready is followed by what keeps it from being ready, in gray, indented and wrapped to the panel width. |

Sections with nothing in them show `none`.

When the details do not fit the panel, they scroll, and a scrollbar is drawn on the right edge of the panel. The
scroll offset is clamped to the end of the details, so the details shrinking never scrolls past their last line.

Instead of the details, the view shows a centered message when the split cannot be shown:

| Situation | Message |
| --- | --- |
| No cluster connection yet | `Waiting for a connection to the cluster...` (gray) |
| The first listing is still in flight | `Loading...` (gray) |
| The listing failed | The error message (red) |
| The split is no longer listed | `This queue-splitting session has ended` (amber) |

The details view handles the following keys:

| Key | Action |
| --- | --- |
| `Down` / `Up` | Scroll by one line. |
| `PageDown` / `PageUp` | Scroll by 10 lines. |
| `Home` | Scroll back to the first line. |
| `End` | Scroll to the last line. |
| `r` | Refresh now. |
| `y` | Show the split whole as YAML, in the shared describe view (see Sessions). |
| `q` / `Esc` | Go back to the list. Because the screen consumes `q` here, it does not quit the application. |

### Preview Environments

Displays every preview environment (`PreviewSession` resource) visible on the connected cluster, as a live-updating watch — no polling or manual refresh needed. This reflects only the connected (primary) cluster; multi-cluster replica state is not shown.

While a connection attempt is in flight, the panel shows `Waiting for a connection to the cluster...`
(gray). If the connection fails, or the watch itself errors, it shows the error (red) instead.

**Layout.** One dense table in a `previews[N]` panel (`[shown/total]` while a filter narrows it), with a
header row (`NAME`, `PHASE`, `STATUS`, `MODE`, `IMAGE`) and one row per namespace, target and environment,
in tree order:

| Row | Content |
| --- | --- |
| Namespace | `▾ <namespace> (<count>)` in bold blue; `▸` while folded. |
| Target | `  ▾ <kind>/<name> (<count>)` in teal, indented under its namespace: the workload the previews copy their pod spec from. |
| Environment | Its key (lavender) indented under its target, then its phase colored by phase, its status worded as `mirrord preview status` words it (`running (2h13m remaining)`, `idle (waiting for traffic)`, the failure message ...), `steal` or `mirror` (gray, `-` when it takes no traffic), and its image (gray). |

Phases are colored green for `Ready`, yellow for `Idle`, purple for `Waiting`, dim purple for `Initializing`,
red for `Failed`, dim yellow for `Paused`, gray for `Unknown`. `Paused` takes `Idle`'s hue dimmed, the same
way `Initializing` takes `Waiting`'s: those two pairs are each a phase and its quieter sibling. The
namespace blue and target teal are deliberately outside this set, so a group row never reads as a phase.

On a terminal without 24-bit color these colors become named ones as described under Colors, each phase and
level keeping a named color of its own: `Ready` bright green, `Idle` bright yellow, `Paused` yellow, `Waiting`
bright magenta, `Initializing` magenta, `Failed` bright red, `Unknown` dark grey, namespaces bright blue,
targets bright cyan.

The selected row is highlighted across its full width. The table scrolls just enough to keep it in view.
Cells too long for their column are cut with `…`.

**Describing.** `Enter` on an environment shows it whole as YAML, in the shared describe view (see Sessions);
`Enter` on a namespace or target row folds or opens it. `y` describes the environment under the cursor and
does nothing on a group row.

**Filtering.** Pressing `/` opens a filter line under the table and enters edit mode. Every keystroke there
immediately re-filters the visible environments live, keeping only those whose `key` contains the typed text
as a substring (a namespace/target group with no surviving environments is hidden entirely, not shown
empty). `Enter` locks in whatever is currently typed and returns to navigation over the filtered set,
leaving the line showing the filter; `Esc` clears the filter entirely and also returns to navigation.

**Go to.** Pressing `g` opens a `go to` line in the same place (the two are mutually exclusive) and starts an
incremental search: every keystroke live-jumps the cursor to the nearest namespace or target row whose name
contains the typed text so far, searching forward from the row that was focused when `g` was pressed and
wrapping around to the top if nothing matches before reaching the end. Unlike filtering, nothing is hidden.
The typed text renders in red when it currently matches nothing. `Enter` locks in wherever the search landed;
`Esc` restores the exact row that was focused before `g` was pressed.

**Stopping.** Pressing `s` opens a centered modal confirmation dialog for stopping preview environments —
nothing is deleted until it's explicitly confirmed. Its scope follows whatever is focused when `s` is
pressed:

- An environment focused: just that one environment.
- A target row focused: every environment under that target.
- A namespace row focused: every environment in that namespace.

The dialog's headline states the scope explicitly (e.g. `Stop preview environment "pr-123"?` for a single one, or `Stop ALL 4 preview environments under Deployment/checkout-api in namespace "staging"?` for a target/namespace scope), rendered bold and in red whenever it covers more than one environment so a bulk stop never reads the same as stopping a single one. Below the headline, every environment the action would affect is listed by its key and namespace — resolved once when `s` is pressed, so the dialog always shows (and, if confirmed, deletes) the exact list it displayed even if the underlying data changes while it's open. The dialog is bordered in red to signal it's destructive.

A target or namespace scope additionally requires typing the word `target` or `namespace` (matching the scope) into a prompt inside the dialog before `Enter` does anything — a single environment needs no typed confirmation, just `Enter`. The typed text renders in red until it's an exact match, then green. While open the dialog captures all input — every key is a no-op except `Enter` (confirm, once any required word is typed exactly — deletes every listed environment), `Esc` (cancel, no changes), and, for a bulk scope, ordinary characters/`Backspace` toward the required word.

**Keys.** The keys that do something in the current mode are on the key bar, and `?` lists them in the
shared help dialog.

#### Keys (Preview Environments only)

While browsing:

| Key | Action |
| --- | --- |
| `/` | Open the filter text box, continuing from the current filter text (if any). |
| `g` | Open the go-to box and start an incremental namespace/target search. |
| `s` | Open the stop-confirmation dialog, scoped to whatever is focused. |
| `↑` / `k` | Move the cursor to the previous visible row. |
| `↓` / `j` | Move the cursor to the next visible row. |
| `←` / `h` | On a header: collapse it. On an already-collapsed header, or on an environment: move focus to the parent. |
| `→` / `l` | On a collapsed header: expand it. On an already-expanded header: move focus to its first visible child. No effect on an environment. |
| `n` / `N` | Jump to the next / previous namespace header, skipping everything in between. Clamped: no effect past the first or last namespace. |
| `t` / `T` | Jump to the next / previous target header, skipping everything in between (may cross into an adjacent namespace). Clamped the same way. |
| `e` / `E` | Jump to the next / previous environment, skipping headers in between (may cross into an adjacent target/namespace). Clamped the same way. |
| `Enter` | On an environment, describe it as YAML; on a namespace or target row, fold or open it. |
| `y` | Describe the environment under the cursor as YAML. |
| `Esc` | Clear the active filter, if one is set. |

While the filter text box is open (after pressing `/`):

| Key | Action |
| --- | --- |
| (any character) | Append to the filter text; the visible list re-filters immediately. |
| `Backspace` | Remove the last character; the visible list re-filters immediately. |
| `Enter` | Lock in the filter, close the text box, and return to browsing. |
| `Esc` | Clear the filter, close the text box, and return to browsing. |

While the go-to box is open (after pressing `g`):

| Key | Action |
| --- | --- |
| (any character) | Append to the search text; the cursor live-jumps to the nearest match. |
| `Backspace` | Remove the last character; the cursor re-jumps based on the shorter text. |
| `Enter` | Lock in the current position, close the box, and return to browsing. |
| `Esc` | Restore the row focused before `g` was pressed, close the box, and return to browsing. |

While the stop-confirmation dialog is open (after pressing `s`):

| Key | Action |
| --- | --- |
| `Enter` | For a single environment: delete it, close the dialog, and return to browsing. For a target/namespace scope: only if the required word has been typed exactly — otherwise no effect. |
| (any character) | For a target/namespace scope only: append to the typed confirmation text. No effect for a single environment. |
| `Backspace` | For a target/namespace scope only: remove the last character of the typed confirmation text. No effect for a single environment. |
| `Esc` | Cancel — nothing is deleted — close the dialog, and return to browsing. |
| (anything else) | No effect. |

### Resources

Every mirrord resource type the connected cluster serves, and the objects of any of them, with no code per
type: a type added to the operator later appears here, and on the `:` command line, on its own.

**Types.** On every new connection the application lists the cluster's API groups, keeps those ending in
`metalbear.co`, and asks each one, on its own, which types it serves at its preferred version, keeping the
types that can be listed. A group that does not answer (the operator's own API while the operator is down)
does not hide the others: it is named on the panel's bottom border, `not answering: operator.metalbear.co`
(amber).

Without a type open, the view lists the types in a `resources[N]` panel, sorted by plural, with columns
`TYPE`, `KIND`, `GROUP`, `VERSION` and `SCOPE` (`namespaced` or `cluster`). `Enter` opens the type under the
cursor.

**Objects.** With a type open, the panel is titled with its plural and count (`mirrordsplitconfigs[5]`) and
when it was last refreshed. The objects are listed in the API server's table format, the same columns
`kubectl get` prints for the type (those it always shows, not the wide ones), with dates shown as ages
(`29d`). When every namespace is listed, a `NAMESPACE` column comes first. A server that does not offer the
table format gets `NAME` and `AGE`. Each column gets room in proportion to the longest value in it. The list
is refreshed every 5 seconds, when the type, namespace or connection changes, and on `r`.

| Key | Action |
| --- | --- |
| `Enter` | Describe the object under the cursor: it is fetched whole and shown as YAML in the shared describe view (see Sessions). |
| `Ctrl+D` | Ask to delete it: a red dialog `Delete <Kind> <name> in <namespace>?`. `Enter` or `y` deletes, `Esc` or `n` cancels. The outcome stays on the bottom border for 6 seconds. |
| `Esc` | Back to the list of types. |
| `/` | Search the listed cells. |
| `r` | Refresh now. |

Messages instead of a table follow the shared rules (`None in this namespace`, `None match the search`, and
the explained errors such as `The mirrord operator is not answering. Is it running?`).

### Terminal

Hosts a real interactive shell, as a full terminal emulator with a rounded border, filling the body — with a
panel alongside it while that shell has mirrord sessions running. The shell is started the first time the
screen is drawn — the user's `$SHELL`, run in the directory the application was started from, with
`TERM=xterm-256color`. If it cannot be started, the shell pane shows the error in red instead.

**Layout.** By default the shell has the whole body to itself. The session panel only takes columns when it
has at least one session to show: it then appears on the right at a fixed width of 34 columns, and the shell
takes whatever is left. Widening the window therefore grows the shell and not the panel. The panel is not
shown at all when

- there is no mirrord session running under the shell,
- the shell has exited or could not be started,
- or the window is too narrow to leave the shell at least 40 columns.

**The pane sizes the shell.** The pty is sized to the *shell pane's* inner rectangle, so `stty size` inside
the shell reports the pane, not the window the application runs in. Anything that changes the split resizes
the pty, which the shell sees as `SIGWINCH` and reflows to — including the panel appearing when a session
starts and going away again when it ends.

**Keyboard focus.** The shell holds the keyboard by default: every keystroke is re-encoded into the bytes a
terminal would have sent and written to the pty, so `Ctrl+C`, `Ctrl+D`, `Ctrl+Z` and `Ctrl+L` keep their usual
meaning and the global keys (`q`, `Tab`, ...) do not fire.

`Option`/`Alt` with `Left` or `Right` is the one keystroke that is not passed on literally: it is sent as
meta-`b` / meta-`f`, the word-motion keys that zsh, bash and everything else built on readline bind out of the
box, so `Option+Left` jumps back a word. The xterm encoding for those keys is bound by nothing by default, and
forwarding it verbatim leaves the shell printing the tail of the escape sequence instead. Every other
combination, `Option` with any other key included, is forwarded as the terminal encodes it. `Ctrl+B` takes the keyboard back for the
application. Pasted text is forwarded as a single paste, bracketed when the shell asked for bracketed paste,
so a multi-line paste lands in the shell's line editor as text rather than being run line by line.

While the keyboard is held back (after `Ctrl+B`):

| Key | Action |
| --- | --- |
| `Enter` | Give the keyboard back to the shell. |
| `Ctrl+B` | Send a literal `Ctrl+B` to the shell; the keyboard stays held back. |
| `PageUp` / `PageDown` | Scroll half a screen back / forward through the scrollback (1000 lines are kept). |
| `r` | Refresh the session panel now instead of waiting for the next poll. |
| (anything else) | Passed on to the global key handling, so `q` quits and `Tab` / `Shift+Tab` change screens. |

The border title shows the current state, and the border is colored to match:

| State | Title | Color |
| --- | --- | --- |
| Shell has the keyboard, showing live output | `shell <cols>x<rows> — C-b to unfocused from shell` | indigo |
| Keyboard held back | `⟨C-b⟩ - enter to continue, r to refresh` | amber |
| Scrolled back | `scrollback -<lines>` | selection |
| Shell exited | `exited: <code> — press any key to restart` | coral |

The host terminal's own cursor is placed at the shell's cursor position, so it blinks and takes the shape the
user configured. It is hidden while the keyboard is held back, while scrolled back, once the shell has exited,
and whenever the shell hides it.

When the shell exits, its exit code is shown in the title and the next key press starts a fresh shell.

#### The session panel

The panel, titled `mirrord`, lists the mirrord sessions running **under this pane's shell**, oldest first. It
is only on screen while there is at least one such session; with none, the shell has the full width and there
is no panel to see.

Sessions are read from the local session registry mirrord's internal proxy maintains in
`~/.mirrord/sessions` — the same source as `mirrord session list`, and independent of the cluster connection
shown in the sidebar. That registry is per-user rather than per-shell, so a session is attributed to this pane by
its **process tree**: the panel keeps a session when the pane's shell is an ancestor of any process running
under it. Walking the whole ancestry (rather than only comparing against the immediate parent) is what makes
indirectly started sessions show up — `mirrord exec` under `npm run`, a `make` recipe, or a wrapper script —
while a session started from a different shell or another terminal is left out. Processes that have already
exited are still linked to their parents using what the session itself recorded, so a session does not
disappear from the panel the moment the command it ran finishes.

The registry is polled every 2 seconds, and immediately when a shell is started or `Ctrl+B` `r` is pressed, so
the panel appears within a couple of seconds of a session starting and goes away again a couple of seconds
after it ends. A session that cannot be read within 2 seconds is skipped rather than reported — a sentinel
file left behind by a crashed session is indistinguishable from a slow one, and cleaning those up is
`mirrord session`'s job. If the registry cannot be read at all, the panel simply stays away and the reason is
written to the log.

Each session is drawn as a block of lines:

| Line | Content |
| --- | --- |
| Target | The session's target, in indigo. |
| Scope | `context/namespace`, whichever of the two the session reports, or `(default)`. |
| Ports | One line per port subscription: the mode (amber for `steal`, mint otherwise) and `:<port>`. |
| Command | The command line of the topmost process the session knows about. |
| Summary | `operator` or `oss`, and how long the session has been running. |

Anything wider than the panel is cut and marked with `…` rather than wrapped. The line under the panel's
bottom border shows how many sessions are listed and how long ago the registry was last read.

## Connecting to Kubernetes

On startup the application immediately begins a connection attempt to the Kubernetes cluster. The sidebar reflects the progress of that attempt:

1. `connecting...` is shown while the attempt is in flight.
2. It changes to `connected` on success, or to the error message on failure.

The connection is built from the ambient mirrord configuration — the same sources mirrord itself reads from (kubeconfig, environment, mirrord config file). This includes:

- the kubeconfig location,
- whether invalid TLS certificates are accepted,
- the current Kubernetes context.

If the connection scope ever changes, any in-flight connection attempt is cancelled and a new one is started immediately, with the sidebar reverting to `connecting…`. In the current version there is no in-app way to change the scope, so this only happens once, at startup.
