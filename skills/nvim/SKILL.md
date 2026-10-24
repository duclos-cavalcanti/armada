---
name: nvim
description: Reference and assistant for launching Neovim and driving it via CLI flags, ex-commands. Use when asked to "open <file> at line N in nvim", "open these files in splits/tabs", or any request to put files in front of the user in Neovim.
allowed-tools: Bash(nvim *), Bash(tmux *), Bash(printenv TMUX), Bash(nvr *)
---

# Neovim Reference

## The one thing to get right: where does the editor appear?

An agent runs inside a **non-interactive shell**. If you run `nvim file` directly
with the Bash tool, the editor opens in *that* invisible shell — the user never
sees it, and the call **blocks forever** waiting for input you can't give.

So "open this file for me" almost never means "run `nvim` yourself." It means
**send the launch command into the user's live terminal pane.** Pick the path
that matches the environment:

### Path A — user is in tmux/cmux (most common here)
Send the keystrokes into their active pane. Guard on `$TMUX` first.

```bash
# Is the agent's shell inside tmux? (it often is, under cmux)
printenv TMUX

# Send a command to the user's CURRENT window/pane and press Enter.
# The trailing C-m is the Enter key.
tmux send-keys "nvim +42 path/to/file.lua" C-m

# Target a specific pane instead of the active one:
tmux send-keys -t mysession:0.1 "nvim file.lua" C-m
```

Use `tmux list-panes -F '#{pane_index} #{pane_current_command} #{pane_current_path}'`
to find a pane sitting at a shell (e.g. `zsh`) before sending. Don't send `nvim`
into a pane that's already running `nvim` — you'd be typing into the editor.

### Path B — drive an already-open nvim instance (remote)
If the user already has nvim open and it's listening on a socket, target it
without spawning a new process. nvim listens automatically on `$NVIM` inside its
own terminals; otherwise it must have been started with `--listen <addr>`.

```bash
# Open a file at a line in the RUNNING instance:
nvim --server /tmp/nvimsocket --remote +42 path/to/file.lua

# Send raw keystrokes / an ex-command to it:
nvim --server /tmp/nvimsocket --remote-send ':vsplit other.lua<CR>'

# nvr (neovim-remote), if installed, is the friendlier wrapper:
nvr --remote +42 file.lua
nvr --remote-send ':only<CR>'
```

### Path C — the user explicitly says "just run it" / you're in an interactive context
Then a plain `nvim ...` invocation is fine. Otherwise prefer A or B.

When unsure which path applies, **ask** — or default to building the command
string and showing it, rather than launching a blocking process.

---

## Launch invocations (the CLI flags worth memorizing)

These are the building blocks. They work as the command in any path above.

```bash
nvim file                      # open one file
nvim +42 file                  # open at line 42 (cursor on line 42)
nvim +42 file +"normal! zz"    # ...and center that line in the window
nvim file +42                  # same as +42 file; the +N can trail
nvim "+/pattern" file          # open at first match of /pattern
nvim +'normal! G' file         # open at last line

nvim a.lua b.lua               # open both; b is hidden behind a (buffer list)
nvim -O a.lua b.lua            # open in VERTICAL splits (side by side)  ← "vertical splits"
nvim -o a.lua b.lua            # open in HORIZONTAL splits (stacked)
nvim -p a.lua b.lua            # open each in its own TAB page
nvim -O3 a.lua b.lua c.lua     # force 3 vertical splits

nvim -R file                   # read-only (view mode)
nvim -d a.lua b.lua            # diff mode (built-in vimdiff) for two files
nvim .                         # open the file explorer at cwd
```

Combine line + split: `nvim -O "+42" a.lua b.lua` opens both vertically with
`a.lua`'s cursor on line 42. To put each split at its own line, run an ex-command
per window after launch (e.g. via `--remote-send`) or use `+"argdo ..."`.

`+CMD` runs an ex-command after loading; you can pass multiple `+` / `-c` flags
and they execute in order. `-c 'cmd'` is the long form of `+cmd`.

---

## Ex-commands (`:` commands) — the vocabulary

```
:e[dit] {file}        open/replace buffer with file
:vs[plit] {file}      vertical split (left/right)
:sp[lit] {file}       horizontal split (top/bottom)
:tabe[dit] {file}     open file in a new tab
:b {name|N}           switch to buffer by name fragment or number
:ls / :buffers        list open buffers
:bd[elete]            close current buffer
:on[ly]               close all other windows
:q / :qa / :wq / :x   quit / quit-all / write+quit / write-if-changed+quit
:42                   jump to line 42
:%s/old/new/g         substitute in whole file (add c flag to confirm)
:g/pat/d              delete every line matching pat (global)
:normal! {keys}       run normal-mode keys (! = ignore user mappings)
:argdo {cmd}          run cmd in every file in the arglist
:set wrap! / :noh     toggle wrap / clear search highlight
```

---

## Motions & navigation (normal mode)

```
h j k l        left / down / up / right
w b e          next word / back word / end of word
0 ^ $          line start / first non-blank / line end
gg G           top of file / bottom of file
{ }            previous / next paragraph
Ctrl-d Ctrl-u  half page down / up
%              jump to matching bracket
42G  or  :42   go to line 42
*  #           search word under cursor forward / back
n N            next / previous search match
Ctrl-o Ctrl-i  jump back / forward in jumplist
zz zt zb       center / top / bottom the cursor line in the window
gd  K          (LSP) goto definition / hover docs
```

Window movement: `Ctrl-w h/j/k/l` move between splits, `Ctrl-w =` equalize sizes,
`Ctrl-w o` keep only current window.

---
## Headless / scripted edits (no UI, safe for the agent to run directly)

When the task is *edit a file*, not *show it to the user*, run nvim headless. This
does NOT block and produces no UI — good for applying an ex-command or plugin
command programmatically.

```bash
# Apply a substitution to a file and save:
nvim --headless -c '%s/foo/bar/g' -c 'wq' file.lua

# Run Lua against a file:
nvim --headless -c 'luafile script.lua' -c 'qa'

# Format the whole buffer via LSP, then save (needs LSP to attach):
nvim --headless file.lua -c 'lua vim.lsp.buf.format()' -c 'wq'

# Run a plugin command in batch (e.g. generate something), then quit:
nvim --headless -c 'SomePluginCommand' -c 'qa!'
```

Always end a headless invocation with `-c 'qa'` / `-c 'wq'` / `-c 'qa!'` so it
exits — otherwise it hangs waiting like the interactive case.
