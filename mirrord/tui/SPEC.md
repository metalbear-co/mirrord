# mirrord-tui — Specification

A terminal user interface for [mirrord](https://github.com/metalbear-co/mirrord).

This document describes the behavior of the application as a user experiences it. Screens marked as **placeholder** currently show only a "not implemented yet" message.

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
with the caller's stderr inherited — cannot paint over the interface. That output is logged instead, and its
last lines are shown in the connection error dialog. The terminal's own stderr is restored before the
application exits, including when it exits by panicking, so error and panic messages still reach the
terminal.

Example (from the README), run from this crate's directory:

```bash
MIRRORD_LOG_FILE=/tmp/mirrord-tui.log MIRRORD_LOG=debug cargo run
```

## Screen layout

Every frame is laid out vertically with fixed chrome:

| Region | Height | Content |
| --- | --- | --- |
| Tabs bar | 1 line | The list of screens. The active screen is highlighted in reverse video. |
| Body | remaining | The content of the active screen. |
| Status bar | 1 line | Current connection scope and status (see below). |

The tabs bar lists, in order: `Home`, `Targets`, `Sessions`, `Databases`, `Queues`, `Preview Environments`, `Terminal`.

## Colors

Screens that have been implemented are styled with the MetalBear brand palette: indigo `#756DF3` for panel
borders and titles, its lighter tint `#A8A5F7` for table headers, the brand dark `#2E2A5E` behind the selected
row, mint `#7DD3A8` for healthy states, amber `#FFCB7D` for in-progress states and warnings, and coral `#F2777A`
for errors. Only foreground colors and the selection background are painted, so the terminal keeps its own
background.

## Status bar

The status bar is a single line showing three fields separated by `  ·  `:

```
context <name>  ·  namespace <name>  ·  <state>
```

- **context** — the Kubernetes context the application is connecting to. Shown as italic `(current)` when no explicit context has been selected (the ambient current context is used).
- **namespace** — the Kubernetes namespace scope. Shown as italic `(default)` when no explicit namespace has been selected.
- **state** — one of:
  - `◌ connecting…` (amber) while a connection attempt is in flight and no result has arrived yet;
  - `● connected` (mint) while the most recent connection attempt succeeded;
  - `✗ <summary>` (coral) followed by `·  e for details` (gray) if the most recent attempt failed.

The summary is a one-line condensation of the failure, never the error verbatim. Errors built around a
payload are cut back to the classification in front of it — a `kube` auth error quotes the entire failed
credential-plugin command, `KUBERNETES_EXEC_INFO`'s JSON included, so `auth error: auth exec command
'<hundreds of characters>' failed with status ...` is shown as `auth error: auth exec command`. Whatever is
still too wide for the terminal is cut with a trailing `…`, so the status bar never spills onto the screen
above it.

## Connection error dialog

Pressing `e` while the status bar reports a failed connection opens a centered modal dialog, red-bordered
and titled `connection error`, over the body area. It captures all input while open; `Esc`, `q` and `e`
close it. A new connection attempt (triggered by changing the context or namespace) closes it too, since the
failure it describes is no longer the current one. It is never drawn over a context or namespace picker.

It shows, in order:

- the same one-line summary as the status bar, in bold;
- **captured output** — the last lines written to the inherited stderr, which for a failing credential
  plugin is where the actionable instruction (`Please run: $ gcloud auth login`) lives. Omitted when nothing
  has been captured;
- **details** — the full error chain, flattened onto one line and cut off after 400 characters. The
  untruncated version is in the log.

Everything is wrapped to the width of the dialog, breaking mid-word for runs (like the quoted JSON payload)
too long to fit a line.

## Keys

The following keys are handled globally, regardless of which screen is active:

| Key | Action |
| --- | --- |
| `q` | Quit the application. |
| `Ctrl+C` | Quit the application. |
| `Tab` | Move to the next screen (wraps from the last back to the first). |
| `Shift+Tab` | Move to the previous screen (wraps from the first back to the last). |
| `e` | Open the connection error dialog. Does nothing unless the last connection attempt failed. |

The active screen sees every key first, and the global handling above only runs for keys the screen did not
consume. Screens that handle keys of their own list them in their own section below. The Terminal screen
consumes *every* key while the shell has the keyboard, so none of the global keys are available there until
`Ctrl+B` is pressed (see below).

Key releases are ignored; only presses and auto-repeats are acted on.

## Screens

### Home

Displays the centered text `Welcome!` in the body area. Has no other content and does not react to input.

### Targets — placeholder

Body shows a centered, red `not implemented yet` message. Intended to display the workloads in the cluster that a mirrord session can target once implemented.

### Sessions — placeholder

Body shows a centered, red `not implemented yet` message. Intended to display the user's active mirrord sessions once implemented.

### Databases — placeholder

Body shows a centered, red `not implemented yet` message. Intended to display branch databases once implemented.

### Queues

Lists every queue-splitting session the mirrord operator reports, in a bordered `Queue Splits` panel. The list is
read from the `queuesplits` resource of the operator's aggregated Kubernetes API — the same live data behind
`mirrord queues status`.

The panel title carries the number of splits on the right, or `shown/total splits` while a search narrows the
list. Below the panel, a footer line shows the key hints on
the left and how long ago the list was last refreshed on the right.

Splits are listed in one row each, sorted by namespace and then by name so that a refresh never reshuffles rows
under the selection. The columns are:

| Column | Content |
| --- | --- |
| `NAME` | Name of the split. |
| `SESSION` | The mirrord session id the split belongs to. |
| `USER` | The developer who started the session, as `username/k8sUsername@hostname`. |
| `NAMESPACE` | Namespace of the split. |
| `TARGET` | Targeted workload, as `Kind/name`, or the label selector for a set of pods. |
| `PHASE` | `Init`/`Pending` (amber), `Ready` (green), `Failed` (red), gray for a phase this build does not recognise, or `-` if the operator reported no status yet. |
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
| The first listing is still in flight | `Loading...` (gray) |
| The cluster answered 404, i.e. it does not serve queue splits | `This cluster does not serve queue splits. Is the mirrord operator installed?` (amber) |
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
| `/` | Start typing a search phrase. |
| `Esc` | Clear the search phrase. |
| `r` | Refresh the list now. |

#### Search

`/` opens a search box on the line between the panel and the footer, showing `/`, the phrase typed so far, and a
block cursor. Every keystroke narrows the list right away: a split is listed only when its `SESSION`, `USER`,
`NAMESPACE`, `TARGET` or `PHASE` cell contains the phrase, each matched on its own, ignoring case. The selection
returns to the first row on every change of the phrase.

While the box takes input, every key goes to it — `q` does not quit and `Tab` does not switch screens — except:

| Key | Action |
| --- | --- |
| `Backspace` | Delete the last character of the phrase. |
| `Enter` | Stop typing, keeping the list narrowed. |
| `Esc` | Stop typing and clear the phrase, listing every split again. |

The box stays on the screen with the phrase in it, cursor hidden, so that the list is never silently narrowed. It
is hidden while the split details are open, and the phrase still applies to the list underneath.

#### Split details

`Enter` replaces the list with a nested view of everything the operator reports about the selected split. The
panel is titled `Queue Split`, with the `namespace/name` of the split on the right. The details are re-read from
the same background refresh as the list, so they keep up with the split while the view is open.

The details are laid out as a label column and a value column, in sections:

| Section | Fields |
| --- | --- |
| (top) | `Name`, `Namespace`, `Session`, `Phase` (colored as in the list), `Message` (only when the operator reported one, colored as the phase), `Created` (the creation timestamp in UTC, followed by the age in parentheses). |
| `OWNER` | `Username`, `Kubernetes user`, `Hostname`, `User ID`. |
| `TARGET` | `Kind`, `Name` and `API version` for a Kubernetes resource; `Kind` (`PodSet`) and `Label selector` for a set of pods. Then `Container`, or `-` when the session did not resolve one. |
| `QUEUES (n)` | One entry per queue the operator resolved, headed by the queue id and its broker type. Below each, whichever of `Queue`, `Topic`, `Consumer group` and `Subscription` the broker type has. |
| `FILTERS (n)` | One entry per filter from the user's mirrord config, headed by the filter id and its broker type. Below each, one line per message filter (attribute name as the label, regex as the value) and a `jq` line when the filter has a jq expression. |
| `TARGET PODS (n/m)` | How many of the target pods are patched and ready, out of the total, then one line per pod: its name and `patched, ready` (green) or the failing combination (amber). |

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
| `q` / `Esc` | Go back to the list. Because the screen consumes `q` here, it does not quit the application. |

### Preview Environments

Displays every preview environment (`PreviewSession` resource) visible on the connected cluster, as a live-updating watch — no polling or manual refresh needed. This reflects only the connected (primary) cluster; multi-cluster replica state is not shown.

While a connection attempt is in flight, the body shows a centered, gray `connecting...` message. If the connection fails, or the watch itself errors, the body shows the error message in light red instead.

**Layout.** Environments are grouped into a two-level nesting of colorful, solid-rounded-border rectangles: a namespace box containing one box per target workload (the `kind`/`name` the preview copies its pod spec from — not by container), each in turn containing one card per preview environment. Every namespace box uses the same fixed blue, and every target box the same fixed teal — one color per level, not per box, so the color itself tells you which kind of box you're looking at. Each level's border insets the next, so no manual indentation is drawn. Sibling boxes at every level — env cards within a target, target boxes within a namespace, namespace boxes within the whole list — have one blank row of space between them (never before the first or after the last, which would just look like wasted space inside the parent box). The whole tree scrolls continuously — content can be clipped exactly at the screen edge (a box may be cut off mid-border at the bottom or top), so navigating never leaves a blank gap or jumps a partially-fitting box to a "next page."

Every box — namespace, target, and env card — uses the same border style (solid, rounded corners; never dashed or double-lined, which looked inconsistent on some terminal fonts). The **focused** box (see Keys, below) is distinguished by a bolded border and title, not a different border type; a focused box of any kind — namespace, target, or env card — additionally gets a subtle dark background tint across its whole area (border and interior alike), distinct from its own colored border. Namespace and target headers additionally carry a collapse arrow (`▾`/`▸`) and an env count in their title.

Each env card's border is colored by its phase: green for `Ready`, yellow for `Idle`, purple for `Waiting`, dim purple for `Initializing`, red for `Failed`, dim yellow for `Paused`, gray for `Unknown`. `Paused` takes `Idle`'s hue dimmed, the same way `Initializing` takes `Waiting`'s: those two pairs are each a phase and its quieter sibling. `Idle` and `Paused` are both scaled to zero, and only `Idle` wakes again on incoming traffic. The namespace/target blue and teal above are deliberately outside this set — MetalBear's brand palette has only three real hues (purple, yellow, mint) plus ink/grey neutrals, and phase already uses all of them, so distinguishing "namespace" and "target" from every phase color (and from each other) needs two colors the brand palette doesn't define.

Every one of these colors checks, once per process (cached), whether the terminal appears to support 24-bit truecolor via the `COLORTERM` environment variable (`truecolor`/`24bit` — the same heuristic tools like `bat` and `delta` use). When it does, colors render as the real MetalBear brand hex values (and this screen's own blue/teal choices for namespace/target). When it doesn't — truecolor support isn't guaranteed in every terminal (e.g. tmux without `Tc`/`RGB` in `terminal-overrides`) — every color falls back to ratatui's standard named (4-bit ANSI) palette instead, since on an unsupported terminal `Color::Rgb` was rendering as a flat gray, with bold text falling back to plain white on top of that.

A card never repeats its target's kind/name — the containing target box's own title already shows that.

An env card's size and level of detail is controlled separately from focus — moving the cursor across cards no longer changes their size (it used to auto-expand the focused card, which shifted every card below it on every navigation step). Instead, `Enter` toggles expanded state for every env card the focused selection covers:

- On an env card: just that one card.
- On a target or namespace header: every env card under it, together, as a group — if every one of them is already expanded, `Enter` collapses them all; otherwise it expands whichever aren't yet. A single focused env card is just the one-card case of this same rule.

Collapsed (the default) shows only the env's key (bold, unlabeled — it's the card's primary identity) and a one-line status (e.g. `running (2h13m remaining)`, `idle (waiting for traffic)`, `paused`, `initializing`, or the failure message), worded to match `mirrord preview status`. Expanded additionally shows a set of detail fields: image, replica count, container (if set), share URL (if the operator minted one), incoming-traffic mode (steal/mirror), queue-splitting and database-branching filter counts (if configured), idle configuration (if set, as prose — e.g. `starts idle, sleeps after 5m`), and the full failure message (if `Failed`, in red). Each detail field is shown as a dim, right-aligned, all-caps label (e.g. `IMAGE`, `REPLICAS`) followed by its value with no colon — the label's own visual weight separates it from the value, rather than "label: value" text.

A single-line, always-visible key-hint bar is shown at the bottom of the screen, styled as a row of honey-background/ink-text chips, listing only the keys that currently do something for the active mode (see Keys, below) — so the available actions never have to be memorized.

**Filtering.** Pressing `/` replaces the card list's top rows with a bordered `filter` text box and enters edit mode. Every keystroke there immediately re-filters the visible environments live, keeping only those whose `key` contains the typed text as a substring (a namespace/target group with no surviving environments is hidden entirely, not shown empty). `Enter` locks in whatever is currently typed, closes the text box, and returns focus to navigation over the filtered set; `Esc` clears the filter entirely and also returns to navigation.

**Go to.** Pressing `g` replaces the card list's top rows with a bordered `go to` text box (the same slot the filter box uses — the two are mutually exclusive) and starts an incremental search: every keystroke live-jumps the cursor to the nearest namespace or target header whose name contains the typed text so far, searching forward from the row that was focused when `g` was pressed and wrapping around to the top if nothing matches before reaching the end. Unlike filtering, nothing is hidden — the rest of the tree stays visible throughout. The typed text renders in red when it currently matches nothing. `Enter` locks in wherever the search landed and returns to browsing; `Esc` restores the exact row that was focused before `g` was pressed and returns to browsing.

**Stopping.** Pressing `s` opens a centered modal confirmation dialog for stopping preview environments — nothing is deleted until it's explicitly confirmed. Its scope follows whatever is focused when `s` is pressed:

- An environment card focused: just that one environment.
- A target header focused: every environment under that target.
- A namespace header focused: every environment in that namespace.

The dialog's headline states the scope explicitly (e.g. `Stop preview environment "pr-123"?` for a single one, or `Stop ALL 4 preview environments under Deployment/checkout-api in namespace "staging"?` for a target/namespace scope), rendered bold and in red whenever it covers more than one environment so a bulk stop never reads the same as stopping a single one. Below the headline, every environment the action would affect is listed by its key and namespace — resolved once when `s` is pressed, so the dialog always shows (and, if confirmed, deletes) the exact list it displayed even if the underlying data changes while it's open. The dialog is bordered in red to signal it's destructive.

A target or namespace scope additionally requires typing the word `target` or `namespace` (matching the scope) into a prompt inside the dialog before `Enter` does anything — a single environment needs no typed confirmation, just `Enter`. The typed text renders in red until it's an exact match, then green. While open the dialog captures all input — every key is a no-op except `Enter` (confirm, once any required word is typed exactly — deletes every listed environment), `Esc` (cancel, no changes), and, for a bulk scope, ordinary characters/`Backspace` toward the required word.

**Help.** Pressing `?` opens a centered modal dialog explaining the screen: what the phase colors on env cards mean, and every key listed in the tables below (namespace/target box colors aren't explained there since they're purely decorative, not meaningful status). While open it captures all input — every key is a no-op except `Esc`, which closes it and returns to normal browsing.

#### Keys (Preview Environments only)

While browsing:

| Key | Action |
| --- | --- |
| `/` | Open the filter text box, continuing from the current filter text (if any). |
| `g` | Open the go-to box and start an incremental namespace/target search. |
| `s` | Open the stop-confirmation dialog, scoped to whatever is focused. |
| `?` | Open the help dialog. |
| `↑` / `k` | Move the cursor to the previous visible row. |
| `↓` / `j` | Move the cursor to the next visible row. |
| `←` / `h` | On a header: collapse it. On an already-collapsed header, or on an environment card: move focus to the parent. |
| `→` / `l` | On a collapsed header: expand it. On an already-expanded header: move focus to its first visible child. No effect on an environment card. |
| `n` / `N` | Jump to the next / previous namespace header, skipping everything in between. Clamped: no effect past the first or last namespace. |
| `t` / `T` | Jump to the next / previous target header, skipping everything in between (may cross into an adjacent namespace). Clamped the same way. |
| `e` / `E` | Jump to the next / previous env card, skipping headers in between (may cross into an adjacent target/namespace). Clamped the same way. |
| `Enter` | Toggle expanded (detail) state for every environment card the focus covers: just the focused card, or every card under a focused target/namespace header, as a group. |
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

While the help dialog is open (after pressing `?`):

| Key | Action |
| --- | --- |
| `Esc` | Close the dialog and return to browsing. |
| (anything else) | No effect. |
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
in the status bar. That registry is per-user rather than per-shell, so a session is attributed to this pane by
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

On startup the application immediately begins a connection attempt to the Kubernetes cluster. The status bar reflects the progress of that attempt:

1. `connecting...` is shown while the attempt is in flight.
2. It changes to `connected` on success, or to the error message on failure.

The connection is built from the ambient mirrord configuration — the same sources mirrord itself reads from (kubeconfig, environment, mirrord config file). This includes:

- the kubeconfig location,
- whether invalid TLS certificates are accepted,
- the current Kubernetes context.

If the connection scope ever changes, any in-flight connection attempt is cancelled and a new one is started immediately, with the status bar reverting to `connecting...`. In the current version there is no in-app way to change the scope, so this only happens once, at startup.
