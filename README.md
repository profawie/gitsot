# Gitsot

A simple, friendly desktop Git client written in Rust. Same idea as GitHub Desktop, with fewer buttons.

Runs on **Windows** and **macOS** (Linux works too). Built with [egui/eframe](https://github.com/emilk/egui) for the UI, [git2](https://github.com/rust-lang/git2-rs) (libgit2) for local Git work and [alacritty_terminal](https://crates.io/crates/alacritty_terminal) for the built-in terminal.

## Features

- **Repositories**: your repositories as cards showing the branch, host and the **name/email that commits will use**. Organise them into **groups**: click *+ Group*, then drag cards onto a group (or use the card menu ☰ → *Move to group…*). Rename or delete a group with the pencil and bin icons next to its name, and click its name to fold it. Open a local folder, clone from a URL, or create a new one.
- **Changes**: see every changed file, tick the ones to include (stage), untick to unstage, and view a coloured diff.
- **Commit**: summary + description, then *Commit to &lt;branch&gt;* (or <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>Enter</kbd>). If your Git name/email are not set, Gitsot asks for them once.
- **Discard**: right-click a file (or *Discard all…*) to throw away changes, with a confirmation.
- **Branches**: switch, create, check out remote branches, and **merge** another branch into the current one (branch menu → *Merge into …*). If a merge has conflicts, the files are marked `!` in Changes; fix them and commit, or *Abort merge*. Delete local or remote branches with the bin icon in the branch menu (never the branch you are on).
- **Remote**: Fetch, Pull and Push, with ahead/behind counts. New branches get a *Publish branch* button.
- **History**: switch between a **Tree** view (the default) (coloured branch graph with branch, remote and tag labels) and a simple **List**, and see each commit's files and diffs.
- **Terminal**: every repository has a built-in terminal that opens right in its folder (the *Terminal* button on its card, *Terminal* in the sidebar, or <kbd>Ctrl</kbd>+<kbd>`</kbd>). See [Terminal](#terminal) below.
- Sidebar navigation, Settings and About pages, light and dark theme, auto-refresh when the window regains focus, <kbd>F5</kbd> to refresh.

## Identities (work vs personal accounts)

Settings → **Identity** has two tabs:

- **Global**: the name/email used everywhere by default (`~/.gitconfig`).
- **This repository**: a name/email for the open repository only (`.git/config`).

Settings → **Folder rules** lets you say "every repository inside `C:\work\company` commits as `me@company.com`". Gitsot writes a standard Git `includeIf "gitdir:…"` rule, so it also applies to `git` on the command line and to other tools.

Before a **commit**, if the email that would be used is different from the folder rule, Gitsot stops and offers *Use the rule's email and commit*, *Commit anyway*, or *Cancel*. Before a **push**, it checks every commit that is about to be sent and warns if any was authored with a different email.

Gitsot works with any Git host (GitHub, GitLab, Bitbucket, Gitea, self-hosted servers, …). It never uses `gh` or any host-specific tool.

Clone, fetch, pull and push use the `git` command installed on your computer, so saved credentials and SSH keys keep working. Credential helpers are never allowed to open their own sign-in windows (such as Git Credential Manager's GitHub page). When an HTTPS remote needs a password and nothing is saved, Gitsot shows one plain *Sign in* dialog (username + password or access token), and your credential store remembers it if one is configured. Everything else is done in-process with libgit2.

## Terminal

Clone or open a repository, click **Terminal**, and you are in a shell inside that folder. No separate terminal window to open and `cd` around in.

![The built-in terminal with the Gitsot prompt](docs/terminal.png)

- **Windows**: PowerShell (PowerShell 7 `pwsh` if it is installed, otherwise Windows PowerShell).
- **macOS**: zsh, started as a login shell like Terminal.app. **Linux**: your zsh or bash.

Your own shell profile is loaded as usual. After it, Gitsot adds its own prompt: before every command, one line of labelled chips shows

| Chip | Meaning |
| --- | --- |
| folder | where you are, relative to the repository |
| branch | the current branch (or `detached at <commit>`) |
| `2 to push` / `1 to pull` / `not published` | how the branch compares with its remote |
| `1 staged, 2 changed, 1 new` or `clean` | what is waiting to be committed; conflicts get their own red chip |
| person | the **email your next commit will use**, so a work/personal mix-up is visible before you commit |
| clock, `exit 1` | how long the last command took (3 s or more) and whether it failed |

The chips and icons are drawn by Gitsot itself: no Nerd Font, oh-my-posh or other install is needed. The files of your own shell setup are never changed. Prefer your own prompt? Turn off Settings → Terminal → **Gitsot prompt**.

Good to know:

- Each repository keeps its own terminal running while you look at other pages or repositories. *Restart* starts a fresh shell; *Close repository* ends it.
- When a command finishes, Changes and History are refreshed, so what you did in the terminal shows up right away.
- Copy with <kbd>Ctrl</kbd>+<kbd>C</kbd> when text is selected (otherwise it interrupts, as usual) or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd>; paste with <kbd>Ctrl</kbd>+<kbd>V</kbd> (<kbd>Cmd</kbd>+<kbd>C</kbd>/<kbd>V</kbd> on macOS). Scroll back with the mouse wheel or <kbd>Shift</kbd>+<kbd>PageUp</kbd>.

## Requirements

- [Rust](https://rustup.rs) (stable)
- [Git](https://git-scm.com) on your PATH (for clone/fetch/pull/push)
- **Windows**: the GNU toolchain is used (`x86_64-pc-windows-gnu`, not MSVC) and needs MinGW-w64 `gcc`:
  ```powershell
  winget install MSYS2.MSYS2
  C:\msys64\usr\bin\pacman.exe -S --needed mingw-w64-x86_64-gcc
  ```
  `build.ps1` picks up `C:\msys64\mingw64\bin` automatically and installs the `stable-x86_64-pc-windows-gnu` Rust toolchain if needed.
- **macOS**: Xcode command line tools (`xcode-select --install`).

## Build and run

### Windows (PowerShell)

```powershell
.\build.ps1                     # release build -> dist\gitsot\
.\build.ps1 -Dev                # debug build
.\run.ps1                       # build and start
.\run.ps1 C:\work\my-project    # build and open a specific repository
```

If PowerShell blocks the scripts, run once: `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned`.

### macOS / Linux

```bash
./build.sh                # release build -> dist/gitsot/
./build.sh --debug        # debug build
./build.sh --windows      # cross-compile for Windows -> dist/gitsot/ (needs mingw-w64)
./run.sh                  # build and start
./run.sh ~/code/project   # build and open a specific repository
```

### The release folder

Every build puts a ready-to-use release in `dist/gitsot`. It holds every file gitsot needs, so you can copy that one folder anywhere (another drive, another computer) and run gitsot from there:

| Platform | Contents of `dist/gitsot` |
| --- | --- |
| Windows | `gitsot.exe` (the program you start) and `gitsot.dll` (the application) |
| macOS | `gitsot.app`, with `libgitsot.dylib` inside `Contents/Frameworks` |
| Linux | `gitsot` (the program you start) and `libgitsot.so` (the application) |

Keep the files together: the program looks for the library in its own folder. On Windows no MinGW DLLs are normally needed, because Rust links them statically; if your MinGW setup does need some (for example `libwinpthread-1.dll`), `build.ps1` and `build.sh --windows` find them and copy them into the folder too.

## Project layout

gitsot is a Cargo workspace with three crates, built the way desktop applications usually are: a small executable that loads one shared library.

```
gitsot.exe ──loads──▶ gitsot.dll ──contains──▶ gitsot-core
(launcher)            (gitsot-app: the UI)     (Git, identities, terminal)
```

| Crate | Builds | What it does |
| --- | --- | --- |
| `crates/gitsot` | `gitsot.exe` / `gitsot` | The launcher. Finds the library next to itself, checks it is the same version, and starts it. Shows a message if the library is missing. |
| `crates/gitsot-app` | `gitsot.dll` / `libgitsot.so` / `libgitsot.dylib` | The application: window and the whole UI. Exports `gitsot_main` and `gitsot_version` with a C interface. |
| `crates/gitsot-core` | Rust library (linked into the dll) | Everything that is not UI. No dependency on the UI toolkit, so it can be tested on its own. |

| File | What it does |
| --- | --- |
| `crates/gitsot/src/main.rs` | The launcher |
| `crates/gitsot-app/src/lib.rs` | Window setup and the exported entry points |
| `crates/gitsot-app/src/app.rs` | All the UI: sidebar, repository cards, changes, history, settings, dialogs, diff viewer |
| `crates/gitsot-app/src/theme.rs` | Colours, text sizes and widgets (same look as termker), logo, cards |
| `crates/gitsot-app/src/term_view.rs` | Draws the terminal, including the prompt's chips and icons, and handles keys and mouse |
| `crates/gitsot-core/src/git.rs` | Local Git operations with libgit2 (status, diff, stage, commit, branches, history) |
| `crates/gitsot-core/src/identity.rs` | Global/repository identities, folder rules (`includeIf`) and mismatch checks |
| `crates/gitsot-core/src/graph.rs` | Lays out the history tree (lanes and branch lines) |
| `crates/gitsot-core/src/cli.rs` | Network operations through the `git` command, run in the background |
| `crates/gitsot-core/src/term.rs` | One built-in terminal: the shell in a pseudo-terminal and its screen (`alacritty_terminal`) |
| `crates/gitsot-core/src/shell.rs` | Picks the shell (PowerShell / zsh / bash) and loads the Gitsot prompt into it |
| `crates/gitsot-core/assets/prompt.ps1`, `prompt.sh` | The Gitsot prompt for PowerShell and for zsh/bash |

## Tests

```bash
cargo test
```
