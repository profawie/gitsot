# Gitsot

A simple, friendly desktop Git client written in Rust. Same idea as GitHub Desktop, with fewer buttons.

Runs on **Windows** and **macOS** (Linux works too). Built with [egui/eframe](https://github.com/emilk/egui) for the UI and [git2](https://github.com/rust-lang/git2-rs) (libgit2) for local Git work.

## Features

- **Repositories**: your repositories as cards (branch + host at a glance). Open a local folder, clone from a URL, or create a new one.
- **Changes**: see every changed file, tick the ones to include (stage), untick to unstage, and view a coloured diff.
- **Commit**: summary + description, then *Commit to &lt;branch&gt;* (or <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>Enter</kbd>). If your Git name/email are not set, Gitsot asks for them once.
- **Discard**: right-click a file (or *Discard all…*) to throw away changes, with a confirmation.
- **Branches**: switch, create, delete (right-click), and check out remote branches.
- **Remote**: Fetch, Pull and Push, with ahead/behind counts. New branches get a *Publish branch* button.
- **History**: switch between a simple **List** and a **Tree** view (coloured branch graph with branch, remote and tag labels), and see each commit's files and diffs.
- Sidebar navigation, Settings (theme, Git name/email) and About pages, light and dark theme, auto-refresh when the window regains focus, <kbd>F5</kbd> to refresh.

Gitsot works with any Git host (GitHub, GitLab, Bitbucket, Gitea, self-hosted servers, …). It never uses `gh` or any host-specific tool.

Clone, fetch, pull and push use the `git` command installed on your computer, so saved credentials and SSH keys keep working. Credential helpers are never allowed to open their own sign-in windows (such as Git Credential Manager's GitHub page). When an HTTPS remote needs a password and nothing is saved, Gitsot shows one plain *Sign in* dialog (username + password or access token), and your credential store remembers it if one is configured. Everything else is done in-process with libgit2.

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
.\build.ps1                     # release build -> dist\gitsot.exe
.\build.ps1 -Dev                # debug build
.\run.ps1                       # build and start
.\run.ps1 C:\work\my-project    # build and open a specific repository
```

If PowerShell blocks the scripts, run once: `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned`.

### macOS / Linux

```bash
./build.sh                # release build -> dist/gitsot (and dist/Gitsot.app on macOS)
./build.sh --debug        # debug build
./build.sh --windows      # cross-compile dist/gitsot.exe (needs mingw-w64)
./run.sh                  # build and start
./run.sh ~/code/project   # build and open a specific repository
```

The Windows `.exe` is self-contained: no MinGW DLLs are needed at runtime.

## Project layout

| File | What it does |
| --- | --- |
| `src/main.rs` | Window setup |
| `src/app.rs` | All the UI: sidebar, repository cards, changes, history, settings, dialogs, diff viewer |
| `src/theme.rs` | Colours, blue buttons, logo, cards |
| `src/graph.rs` | Lays out the history tree (lanes and branch lines) |
| `src/git.rs` | Local Git operations with libgit2 (status, diff, stage, commit, branches, history) |
| `src/cli.rs` | Network operations through the `git` command, run in the background |

## Tests

```bash
cargo test
```
