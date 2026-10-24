---
description: >-
  The kit's tmux reference — detecting/querying sessions, session/window/pane
  commands, targets, formats, and the scripting gotchas that bite agents
  driving tmux: send-keys paste-detection dropping the trailing Enter on long
  prompts (Claude Code), capture-pane against live TUIs, and --porcelain output
  for scripting. Background knowledge, not an action: consult it whenever
  composing tmux commands or debugging why a send-keys / capture-pane didn't
  behave as expected.
allowed-tools: Bash(tmux *), Bash(printenv TMUX)
user-invocable: false
---

# tmux Reference

## Detecting a tmux Session

The `TMUX` environment variable is set when a shell is running inside a tmux
session. Use it to guard commands:

```bash
if [ -n "$TMUX" ]; then
  echo "inside tmux"
fi
```

Or from the shell directly:
```bash
printenv TMUX        # prints socket path if inside tmux, empty if not
```

---

## Querying Current Context

These commands reveal where you are right now. All use `display-message -p` to
print to stdout:

```bash
# Current session name
tmux display-message -p '#{session_name}'

# Current session working directory
tmux display-message -p '#{session_path}'

# Current window index and name
tmux display-message -p '#{window_index}: #{window_name}'

# Current pane index and working directory
tmux display-message -p '#{pane_index} #{pane_current_path}'

# Current pane ID
tmux display-message -p '#{pane_id}'

# All of the above at once
tmux display-message -p 'session=#{session_name} window=#{window_index} pane=#{pane_index} path=#{pane_current_path}'
```

---

## Common One-Liners

```bash
# List all sessions (name + path)
tmux list-sessions -F '#{session_name}  #{session_path}'

# List all windows in current session
tmux list-windows -F '#{window_index}: #{window_name}  #{window_active}'

# List all panes in current window
tmux list-panes -F '#{pane_index}: #{pane_current_command}  #{pane_current_path}'

# Check if a session exists
tmux has-session -t mysession 2>/dev/null && echo "exists"

# Create a new detached session rooted at a path
tmux new-session -d -s mysession -c ~/Work/myproject

# Switch to a session (from within tmux)
tmux switch-client -t mysession

# Kill a session
tmux kill-session -t mysession

# Create a new window in current session
tmux new-window -c "#{pane_current_path}"

# Split current pane horizontally, inheriting cwd
tmux split-window -h -c "#{pane_current_path}"

# Send a command to a specific pane and press Enter
tmux send-keys -t mysession:1.0 "cargo build" Enter

# Move current window to another session
tmux move-window -s "$(tmux display-message -p '#{session_name}:#{window_index}')" -t othersession
```

---

## Targets

Most commands accept `-t` to specify a target. Targets take the form:

- `session` — matches by name prefix or exact name (prefix with `=` for exact)
- `session:window` — window by index, ID (`@1`), or name
- `session:window.pane` — pane by index or ID (`%1`)

Special tokens: `{last}` / `!`, `{next}` / `+`, `{previous}` / `-`, `{marked}` /
`~`

---

## Sessions

```
new-session [-d] [-s name] [-c start-dir]         Create a new session (-d = detached)
attach-session [-t target]                         Attach to an existing session
detach-client [-t target]                          Detach the current client
switch-client [-t target-session]                  Switch to another session (from within tmux)
kill-session [-t target]                           Kill a session
has-session [-t target]                            Check if a session exists (exit code 0/1)
list-sessions [-F format]                          List all sessions
rename-session [-t target] new-name                Rename a session
```

---

## Windows

```
new-window [-d] [-c start-dir] [-n name]           Create a new window
kill-window [-t target]                            Kill a window and all its panes
select-window [-t target]                          Switch to a window
rename-window [-t target] new-name                 Rename a window
move-window [-s src] [-t dst]                      Move a window to another session/index
swap-window [-s src] [-t dst]                      Swap two windows
list-windows [-t session] [-F format]              List windows in a session
```

---

## Panes

```
split-window [-h] [-v] [-c start-dir] [-l size]    Split pane horizontally (-h) or vertically (-v)
kill-pane [-t target]                              Kill a pane
select-pane [-t target] [-L/-R/-U/-D]              Select a pane by target or direction
resize-pane [-t target] [-L/-R/-U/-D] [-Z]         Resize pane; -Z toggles zoom
swap-pane [-s src] [-t dst]                        Swap two panes
move-pane [-s src] [-t dst] [-h/-v]                Move a pane into another window
break-pane [-d] [-P -F format] [-t dst]            Break pane into its own window; -P prints new window info
list-panes [-t target] [-F format]                 List panes in a window
```

---

## Key Bindings

```
bind-key [-T key-table] [-r] key command           Bind a key to a command
unbind-key [-T key-table] key                      Remove a key binding
```

Key tables: `root` (no prefix needed), `prefix` (default, after prefix key),
`copy-mode-vi`

---

## Scripting

```
run-shell [-d delay] shell-command                 Run a shell command from tmux
send-keys [-t target] keys [Enter]                 Send keystrokes to a pane
display-message [-p] [-t target] message           Show a message; -p prints to stdout
command-prompt -p "label:" "command %%"            Show an input prompt; %% = user input
display-popup [-E] [-w width] [-h height] command  Open a floating popup; -E closes on exit
source-file path                                   Load and execute a tmux config file
if-shell shell-command command                     Run tmux command if shell command succeeds
```

### `send-keys` gotcha — paste-detection suppresses auto-Enter for long prompts

Some TUIs (Claude Code, some terminal editors) run paste-detection heuristics on
incoming keystrokes. When a `send-keys "TEXT" C-m` call crosses an internal
length/rate threshold, the TUI categorises the whole burst as a **pasted block**
and intentionally strips the trailing newline to prevent accidental submission
of multi-line paste content. The text lands in the input buffer but the app sits
idle waiting for a real keystroke.

Empirically validated against Claude Code:
- Short prompts (e.g. ~20 chars) → single-call `send-keys "text" C-m` submits
  fine.
- Long prompts (~180+ chars) → Enter is suppressed; user must press Enter
  manually.

**Fix — split the text and the Enter into separate calls, with a sleep between
them so the paste-detection window closes before C-m arrives:**

```bash
tmux send-keys -t "%42" "your long prompt here"
sleep 1
tmux send-keys -t "%42" C-m
```

For plain shells (bash/zsh), `send-keys "cmd" C-m` in one call works regardless
of length — shells don't run paste-detection on send-keys input (tmux doesn't
wrap the input in bracketed-paste markers).

### `capture-pane` and TUIs

`tmux capture-pane -p <target>` returns the visible content of a pane. **It
works for TUIs** (Claude Code, vim, less, htop) — the pane's screen content is
captured whether the TUI is using the primary or the alternate screen buffer.

Useful flags:
- `-p` print to stdout (most common)
- `-S -<N>` include N lines of scrollback history
- `-J` join wrapped lines
- `-a` capture the alternate screen buffer specifically (only populated
  if the TUI has flipped into it and is holding content there)

If a capture comes back blank against a running TUI, it's typically a timing
issue — the TUI was mid-redraw at capture time, not a fundamental limitation.
Retry after a small sleep.

---

## Buffers & Clipboard

```
load-buffer [-b name] path                         Load file contents into a paste buffer
paste-buffer [-t target] [-p]                      Paste buffer into a pane; -p = bracketed paste
list-buffers                                       List all paste buffers
```

---

## Configuration

```
set-option [-g] [-w] [-s] option value             Set an option; -g = global, -w = window, -s = server
show-options [-g] [-w] [option]                    Show current option values
```

---

## Formats (`-F`)

Formats are used with `-F` flags to customize output. Common variables:

| Variable | Description |
|---|---|
| `#{session_name}` | Session name |
| `#{session_path}` | Session working directory |
| `#{window_index}` | Window index |
| `#{window_id}` | Window ID (e.g. `@1`) |
| `#{window_name}` | Window name |
| `#{pane_index}` | Pane index |
| `#{pane_id}` | Pane ID (e.g. `%1`) |
| `#{pane_current_path}` | Pane current working directory |
| `#{pane_current_command}` | Command running in pane |

---

## Porcelain Output

`list-sessions --porcelain`, `list-windows --porcelain`, `list-panes
--porcelain`, and `worktree list --porcelain` output machine-readable key/value
pairs, one per line, separated by blank lines per entry. Useful for scripting
with `awk` or `grep`.

---

## Notes

- `display-message -p` prints to stdout — useful for capturing values in scripts
  via `$(tmux display-message -p '#{format}')`
- `command-prompt` is async and cannot be captured in a script — pass the result
  as an argument using `%%` in the tmux binding instead
- `break-pane -P -F '#{session_name}:#{window_index}'` captures the new window
  address atomically
- New panes inherit the working directory of the pane they are split from only
  when `-c "#{pane_current_path}"` is passed to `split-window`
