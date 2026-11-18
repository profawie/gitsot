use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, FontId, Key, KeyboardShortcut, Layout, Modifiers, RichText,
    Sense, TextEdit, Vec2,
};
use git2::{Oid, Repository};
use serde::{Deserialize, Serialize};

use crate::cli::{self, Credentials, NetOp, NetResult};
use crate::git::{self, BranchInfo, ChangeKind, CommitFile, CommitInfo, FileChange, FileDiff, HeadInfo, LineKind};

const SETTINGS_KEY: &str = "gitsot-settings";
const MAX_RECENT: usize = 12;

// ---------------------------------------------------------------------------
// Persistent settings
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Settings {
    recent: Vec<PathBuf>,
    last_repo: Option<PathBuf>,
    clone_parent: Option<PathBuf>,
    light_mode: bool,
}

impl Settings {
    fn remember(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(MAX_RECENT);
        self.last_repo = Some(path.to_path_buf());
    }
}

// ---------------------------------------------------------------------------
// State of the open repository
// ---------------------------------------------------------------------------

struct RepoView {
    path: PathBuf,
    repo: Repository,
    head: HeadInfo,
    remote_url: Option<String>,

    changes: Vec<FileChange>,
    selected_change: Option<String>,
    change_diff: Option<Result<FileDiff, String>>,
    summary: String,
    description: String,

    history: Vec<CommitInfo>,
    history_loaded: bool,
    selected_commit: Option<Oid>,
    commit_files: Vec<CommitFile>,
    selected_commit_file: Option<String>,
    commit_diff: Option<Result<FileDiff, String>>,

    branches: Vec<BranchInfo>,
    remote_branches: Vec<String>,
    branch_filter: String,
}

impl RepoView {
    fn open(path: &Path) -> anyhow::Result<Self> {
        let repo = git::open(path)?;
        let path = git::workdir(&repo);
        let path = path.canonicalize().map(strip_unc).unwrap_or(path);
        let mut view = RepoView {
            path,
            head: HeadInfo::default(),
            remote_url: None,
            repo,
            changes: Vec::new(),
            selected_change: None,
            change_diff: None,
            summary: String::new(),
            description: String::new(),
            history: Vec::new(),
            history_loaded: false,
            selected_commit: None,
            commit_files: Vec::new(),
            selected_commit_file: None,
            commit_diff: None,
            branches: Vec::new(),
            remote_branches: Vec::new(),
            branch_filter: String::new(),
        };
        view.refresh()?;
        Ok(view)
    }

    fn name(&self) -> String {
        git::repo_name(&self.path)
    }

    /// Reload everything that can change behind our back.
    fn refresh(&mut self) -> anyhow::Result<()> {
        self.head = git::head_info(&self.repo);
        self.remote_url = git::remote_url(&self.repo);
        self.changes = git::status(&self.repo)?;
        self.branches = git::local_branches(&self.repo).unwrap_or_default();
        self.remote_branches = git::remote_branches(&self.repo).unwrap_or_default();

        if let Some(sel) = &self.selected_change {
            if !self.changes.iter().any(|c| &c.path == sel) {
                self.selected_change = None;
            }
        }
        if self.selected_change.is_none() {
            self.selected_change = self.changes.first().map(|c| c.path.clone());
        }
        self.load_change_diff();

        if self.history_loaded {
            self.load_history();
        }
        Ok(())
    }

    fn load_change_diff(&mut self) {
        self.change_diff = self
            .selected_change
            .as_ref()
            .map(|p| git::working_diff(&self.repo, p).map_err(|e| e.to_string()));
    }

    fn load_history(&mut self) {
        self.history = git::history(&self.repo, git::MAX_HISTORY).unwrap_or_default();
        self.history_loaded = true;
        let still_there = self
            .selected_commit
            .map(|id| self.history.iter().any(|c| c.id == id) && git::commit_exists(&self.repo, id))
            .unwrap_or(false);
        if !still_there {
            let first = self.history.first().map(|c| c.id);
            self.select_commit(first);
        }
    }

    fn select_commit(&mut self, id: Option<Oid>) {
        self.selected_commit = id;
        self.commit_files = id
            .and_then(|id| git::commit_files(&self.repo, id).ok())
            .unwrap_or_default();
        self.selected_commit_file = self.commit_files.first().map(|f| f.path.clone());
        self.load_commit_diff();
    }

    fn load_commit_diff(&mut self) {
        self.commit_diff = match (self.selected_commit, &self.selected_commit_file) {
            (Some(id), Some(path)) => {
                Some(git::commit_file_diff(&self.repo, id, path).map_err(|e| e.to_string()))
            }
            _ => None,
        };
    }

    fn staged_count(&self) -> usize {
        self.changes.iter().filter(|c| c.staged).count()
    }
}

/// `canonicalize` on Windows returns `\\?\C:\...`; show the friendly form.
fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

// ---------------------------------------------------------------------------
// UI helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Changes,
    History,
}

enum Dialog {
    Clone { url: String, parent: String, name: String, name_edited: bool },
    NewBranch { name: String },
    Identity { name: String, email: String, commit_after: bool },
    ConfirmDiscard { changes: Vec<FileChange> },
    ConfirmDeleteBranch { name: String },
    Message { title: String, text: String },
    SignIn { op: NetOp, host: String, username: String, password: String, retry: bool },
}

/// Things the user asked for while drawing the UI; applied afterwards so we
/// never mutate state while it is borrowed by a widget.
enum Action {
    OpenDialog,
    OpenPath(PathBuf),
    InitDialog,
    ShowClone,
    CloseRepo,
    Refresh,
    SelectChange(String),
    Stage(Vec<String>),
    Unstage(Vec<String>),
    StageAll,
    UnstageAll,
    AskDiscard(Vec<FileChange>),
    Discard(Vec<FileChange>),
    Commit,
    SetIdentity { name: String, email: String, commit_after: bool },
    Checkout(String),
    CheckoutRemote(String),
    ShowNewBranch,
    CreateBranch(String),
    AskDeleteBranch(String),
    DeleteBranch(String),
    Net(NetOp),
    NetWithCredentials(NetOp, Credentials),
    StartClone { url: String, parent: String, name: String },
    LoadHistory,
    SelectCommit(Oid),
    SelectCommitFile(String),
    RevealInFiles,
    CopyToClipboard(String),
    ToggleTheme,
    ForgetRecent(PathBuf),
}

struct Toast {
    text: String,
    error: bool,
    at: Instant,
}

pub struct GitsotApp {
    settings: Settings,
    repo: Option<RepoView>,
    tab: Tab,
    dialog: Option<Dialog>,
    toast: Option<Toast>,
    busy: Option<String>,
    tx: Sender<NetResult>,
    rx: Receiver<NetResult>,
    git_cli: bool,
    was_focused: bool,
    /// Username typed into the last sign-in dialog, to pre-fill a retry.
    last_username: String,
}

impl GitsotApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
            .unwrap_or_default();
        let (tx, rx) = channel();
        let mut app = Self {
            settings,
            repo: None,
            tab: Tab::Changes,
            dialog: None,
            toast: None,
            busy: None,
            tx,
            rx,
            git_cli: cli::git_available(),
            was_focused: true,
            last_username: String::new(),
        };
        app.apply_theme(&cc.egui_ctx);
        configure_style(&cc.egui_ctx);

        let start = std::env::args_os().nth(1).map(PathBuf::from).or_else(|| app.settings.last_repo.clone());
        if let Some(path) = start {
            if path.exists() {
                app.open_repo(&path);
                // Don't nag on startup if the last repository disappeared.
                if app.repo.is_none() {
                    app.toast = None;
                }
            }
        }
        app
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_theme(if self.settings.light_mode {
            egui::Theme::Light
        } else {
            egui::Theme::Dark
        });
    }

    fn info(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast { text: text.into(), error: false, at: Instant::now() });
    }

    fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text.len() > 160 || text.contains('\n') {
            self.dialog = Some(Dialog::Message { title: "Something went wrong".into(), text });
        } else {
            self.toast = Some(Toast { text, error: true, at: Instant::now() });
        }
    }

    fn open_repo(&mut self, path: &Path) {
        match RepoView::open(path) {
            Ok(view) => {
                self.settings.remember(&view.path);
                self.info(format!("Opened {}", view.name()));
                self.repo = Some(view);
                self.tab = Tab::Changes;
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn refresh(&mut self) {
        if let Some(r) = &mut self.repo {
            if let Err(e) = r.refresh() {
                let msg = format!("{e:#}");
                self.error(msg);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::OpenDialog => {
                if let Some(dir) = rfd::FileDialog::new().set_title("Open a Git repository").pick_folder() {
                    self.open_repo(&dir);
                }
            }
            Action::OpenPath(p) => self.open_repo(&p),
            Action::InitDialog => {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose a folder for the new repository")
                    .pick_folder()
                {
                    if Repository::open(&dir).is_ok() {
                        self.open_repo(&dir);
                    } else {
                        match git::init(&dir) {
                            Ok(_) => {
                                self.open_repo(&dir);
                                self.info("Created a new repository");
                            }
                            Err(e) => self.error(format!("{e:#}")),
                        }
                    }
                }
            }
            Action::ShowClone => {
                let parent = self
                    .settings
                    .clone_parent
                    .clone()
                    .or_else(default_clone_parent)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.dialog = Some(Dialog::Clone {
                    url: String::new(),
                    parent,
                    name: String::new(),
                    name_edited: false,
                });
            }
            Action::CloseRepo => {
                self.repo = None;
                self.settings.last_repo = None;
            }
            Action::Refresh => {
                self.refresh();
            }
            Action::ForgetRecent(p) => self.settings.recent.retain(|r| r != &p),
            Action::ToggleTheme => {
                self.settings.light_mode = !self.settings.light_mode;
                self.apply_theme(ctx);
            }
            Action::CopyToClipboard(text) => {
                ctx.copy_text(text);
                self.info("Copied to clipboard");
            }
            Action::StartClone { url, parent, name } => self.start_clone(ctx, url, parent, name),
            Action::Net(op) => self.start_net(ctx, op, None),
            Action::NetWithCredentials(op, creds) => {
                self.dialog = None;
                self.last_username = creds.username.clone();
                self.start_net(ctx, op, Some(creds));
            }
            Action::SetIdentity { name, email, commit_after } => {
                if name.trim().is_empty() || email.trim().is_empty() {
                    self.error("Please enter both a name and an email");
                    return;
                }
                match git::set_global_identity(&name, &email) {
                    Ok(()) => {
                        self.dialog = None;
                        if commit_after {
                            self.apply(ctx, Action::Commit);
                        } else {
                            self.info("Saved your Git name and email");
                        }
                    }
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
            other => {
                let Some(r) = self.repo.as_mut() else { return };
                let result: anyhow::Result<Option<String>> = (|| {
                    match other {
                        Action::SelectChange(p) => {
                            r.selected_change = Some(p);
                            r.load_change_diff();
                            return Ok(None);
                        }
                        Action::Stage(paths) => git::stage(&r.repo, &paths)?,
                        Action::Unstage(paths) => git::unstage(&r.repo, &paths)?,
                        Action::StageAll => git::stage_all(&r.repo)?,
                        Action::UnstageAll => git::unstage_all(&r.repo)?,
                        Action::AskDiscard(changes) => {
                            self.dialog = Some(Dialog::ConfirmDiscard { changes });
                            return Ok(None);
                        }
                        Action::Discard(changes) => {
                            git::discard(&r.repo, &changes)?;
                            r.refresh()?;
                            return Ok(Some(format!("Discarded changes in {} file(s)", changes.len())));
                        }
                        Action::Commit => {
                            if git::signature(&r.repo).is_err() {
                                let (name, email) = git::identity(Some(&r.repo));
                                self.dialog = Some(Dialog::Identity { name, email, commit_after: true });
                                return Ok(None);
                            }
                            let mut msg = r.summary.trim().to_string();
                            if msg.is_empty() {
                                anyhow::bail!("Please write a commit summary");
                            }
                            let desc = r.description.trim();
                            if !desc.is_empty() {
                                msg.push_str("\n\n");
                                msg.push_str(desc);
                            }
                            msg.push('\n');
                            let oid = git::commit(&r.repo, &msg)?;
                            r.summary.clear();
                            r.description.clear();
                            r.refresh()?;
                            if r.history_loaded {
                                r.select_commit(Some(oid));
                            }
                            return Ok(Some(format!(
                                "Committed {} to {}",
                                git::short(oid),
                                r.head.display_name()
                            )));
                        }
                        Action::Checkout(name) => {
                            git::checkout_branch(&r.repo, &name)?;
                            r.refresh()?;
                            return Ok(Some(format!("Switched to {name}")));
                        }
                        Action::CheckoutRemote(name) => {
                            let local = git::checkout_remote_branch(&r.repo, &name)?;
                            r.refresh()?;
                            return Ok(Some(format!("Switched to {local} (tracking {name})")));
                        }
                        Action::ShowNewBranch => {
                            self.dialog = Some(Dialog::NewBranch { name: String::new() });
                            return Ok(None);
                        }
                        Action::CreateBranch(name) => {
                            git::create_branch(&r.repo, &name, true)?;
                            self.dialog = None;
                            r.refresh()?;
                            return Ok(Some(format!("Created and switched to {}", name.trim())));
                        }
                        Action::AskDeleteBranch(name) => {
                            self.dialog = Some(Dialog::ConfirmDeleteBranch { name });
                            return Ok(None);
                        }
                        Action::DeleteBranch(name) => {
                            git::delete_branch(&r.repo, &name)?;
                            r.refresh()?;
                            return Ok(Some(format!("Deleted branch {name}")));
                        }
                        Action::LoadHistory => {
                            r.load_history();
                            return Ok(None);
                        }
                        Action::SelectCommit(id) => {
                            r.select_commit(Some(id));
                            return Ok(None);
                        }
                        Action::SelectCommitFile(p) => {
                            r.selected_commit_file = Some(p);
                            r.load_commit_diff();
                            return Ok(None);
                        }
                        Action::RevealInFiles => {
                            open_in_file_manager(&r.path);
                            return Ok(None);
                        }
                        _ => return Ok(None),
                    }
                    // Staging actions fall through to here.
                    r.refresh()?;
                    Ok(None)
                })();
                match result {
                    Ok(Some(msg)) => self.info(msg),
                    Ok(None) => {}
                    Err(e) => {
                        let msg = format!("{e:#}");
                        self.error(msg);
                    }
                }
            }
        }
    }

    fn start_clone(&mut self, ctx: &egui::Context, url: String, parent: String, name: String) {
        let url = url.trim().to_string();
        let name = name.trim().to_string();
        if url.is_empty() || parent.trim().is_empty() || name.is_empty() {
            self.error("Please fill in the URL, the folder and the name");
            return;
        }
        let parent = PathBuf::from(parent.trim());
        let dest = parent.join(&name);
        if dest.exists() && dest.read_dir().map(|mut d| d.next().is_some()).unwrap_or(true) {
            self.error(format!("'{}' already exists and is not empty", dest.display()));
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&parent) {
            self.error(format!("Could not create '{}': {e}", parent.display()));
            return;
        }
        self.settings.clone_parent = Some(parent);
        self.dialog = None;
        self.start_net(ctx, NetOp::Clone { url, dest }, None);
    }

    fn start_net(&mut self, ctx: &egui::Context, op: NetOp, creds: Option<Credentials>) {
        if self.busy.is_some() {
            return;
        }
        if !self.git_cli {
            self.git_cli = cli::git_available();
            if !self.git_cli {
                self.error(
                    "Git is not installed or not on your PATH.\n\nGitsot uses the git command for \
                     clone, fetch, pull and push. Install Git from https://git-scm.com and restart.",
                );
                return;
            }
        }
        let repo_path = match &op {
            NetOp::Clone { dest, .. } => dest.clone(),
            _ => match &self.repo {
                Some(r) => r.path.clone(),
                None => return,
            },
        };
        let detail = match &op {
            NetOp::Clone { url, .. } => format!("Cloning {url}…"),
            _ => format!("{}…", op.label()),
        };
        self.busy = Some(detail);
        cli::spawn(op, repo_path, creds, self.tx.clone(), ctx.clone());
    }

    fn poll_background(&mut self) {
        while let Ok(res) = self.rx.try_recv() {
            self.busy = None;
            match (&res.op, res.result) {
                (NetOp::Clone { dest, .. }, Ok(_)) => {
                    let dest = dest.clone();
                    self.open_repo(&dest);
                    self.info(format!("Cloned into {}", dest.display()));
                }
                (op, Ok(_)) => {
                    let label = match op {
                        NetOp::Fetch => "Fetched from remote",
                        NetOp::Pull => "Pulled latest changes",
                        NetOp::Push { .. } => "Pushed to remote",
                        NetOp::Clone { .. } => unreachable!(),
                    };
                    let same_repo = self.repo.as_ref().map(|r| r.path == res.repo).unwrap_or(false);
                    if same_repo {
                        self.refresh();
                    }
                    self.info(label);
                }
                (op, Err(_)) if res.auth_failed && self.remote_url_for(op).is_some_and(|u| cli::is_http_url(&u)) => {
                    // The remote needs a sign-in. Ask for it ourselves, the same way for
                    // every Git host, instead of letting a helper open its own window.
                    let retry = res.used_credentials;
                    let host = self.remote_url_for(op).map(|u| cli::host_of(&u)).unwrap_or_default();
                    self.dialog = Some(Dialog::SignIn {
                        op: op.clone(),
                        host,
                        username: if retry { self.last_username.clone() } else { String::new() },
                        password: String::new(),
                        retry,
                    });
                }
                (op, Err(e)) => {
                    let title = format!("{} failed", op.label());
                    self.dialog = Some(Dialog::Message { title, text: e });
                    self.refresh();
                }
            }
        }
    }

    /// The URL a network operation talks to.
    fn remote_url_for(&self, op: &NetOp) -> Option<String> {
        match op {
            NetOp::Clone { url, .. } => Some(url.clone()),
            _ => self.repo.as_ref().and_then(|r| r.remote_url.clone()),
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let refresh = ctx.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::F5) || i.consume_key(Modifiers::COMMAND, Key::R)
        });
        if refresh {
            actions.push(Action::Refresh);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            actions.push(Action::OpenDialog);
        }

        // Re-read the repository when the window regains focus, so edits made
        // in other tools show up without a manual refresh.
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.was_focused && self.busy.is_none() {
            actions.push(Action::Refresh);
        }
        self.was_focused = focused;
    }
}

fn configure_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
    });
}

fn default_clone_parent() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let docs = PathBuf::from(&home).join("Documents");
    let base = if docs.is_dir() { docs } else { PathBuf::from(home) };
    Some(base.join("GitHub"))
}

fn open_in_file_manager(path: &Path) {
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}

fn relative_time(secs: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let d = (now - secs).max(0);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => plural(d / 60, "minute"),
        3600..=86_399 => plural(d / 3600, "hour"),
        86_400..=2_591_999 => plural(d / 86_400, "day"),
        _ => chrono::DateTime::from_timestamp(secs, 0)
            .map(|t| t.with_timezone(&chrono::Local).format("%d %b %Y").to_string())
            .unwrap_or_default(),
    }
}

fn plural(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

fn full_time(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%a %d %b %Y, %H:%M").to_string())
        .unwrap_or_default()
}

fn kind_color(kind: ChangeKind, dark: bool) -> Color32 {
    match (kind, dark) {
        (ChangeKind::Added | ChangeKind::Untracked, true) => Color32::from_rgb(87, 171, 90),
        (ChangeKind::Added | ChangeKind::Untracked, false) => Color32::from_rgb(26, 127, 55),
        (ChangeKind::Deleted, true) => Color32::from_rgb(229, 83, 75),
        (ChangeKind::Deleted, false) => Color32::from_rgb(207, 34, 46),
        (ChangeKind::Conflicted, _) => Color32::from_rgb(219, 109, 40),
        (ChangeKind::Renamed | ChangeKind::TypeChange, _) => Color32::from_rgb(130, 120, 220),
        (ChangeKind::Modified, true) => Color32::from_rgb(210, 153, 34),
        (ChangeKind::Modified, false) => Color32::from_rgb(154, 103, 0),
    }
}

fn kind_badge(ui: &mut egui::Ui, kind: ChangeKind) {
    let dark = ui.visuals().dark_mode;
    ui.label(RichText::new(kind.letter()).monospace().strong().color(kind_color(kind, dark)))
        .on_hover_text(kind.description());
}

/// Split `dir/sub/file.rs` into (`file.rs`, `dir/sub/`).
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[i + 1..], &path[..=i]),
        None => (path, ""),
    }
}

fn file_row(ui: &mut egui::Ui, selected: bool, path: &str) -> egui::Response {
    let (name, dir) = split_path(path);
    let mut job = egui::text::LayoutJob::default();
    let strong = ui.visuals().strong_text_color();
    let weak = ui.visuals().weak_text_color();
    job.append(name, 0.0, egui::TextFormat { color: strong, ..Default::default() });
    if !dir.is_empty() {
        job.append(
            &format!("  {dir}"),
            0.0,
            egui::TextFormat { color: weak, font_id: FontId::proportional(12.0), ..Default::default() },
        );
    }
    let resp = ui.add(egui::Button::selectable(selected, job).truncate());
    resp.on_hover_text(path)
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

impl eframe::App for GitsotApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_background();

        let mut actions = Vec::new();
        self.handle_shortcuts(&ctx, &mut actions);

        self.toolbar(ui, &mut actions);
        self.status_bar(ui);

        if self.repo.is_some() {
            self.repo_ui(ui, &mut actions);
        } else {
            self.welcome(ui, &mut actions);
        }

        self.dialogs(&ctx, &mut actions);

        for a in actions {
            self.apply(&ctx, a);
        }

        if self.busy.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some(t) = &self.toast {
            let ttl = if t.error { 8.0 } else { 4.0 };
            if t.at.elapsed().as_secs_f32() > ttl {
                self.toast = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }
    }
}

impl GitsotApp {
    fn toolbar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // Repository picker
                let repo_label = self
                    .repo
                    .as_ref()
                    .map(|r| r.name())
                    .unwrap_or_else(|| "No repository".into());
                ui.vertical(|ui| {
                    ui.label(RichText::new("Current repository").small().weak());
                    ui.menu_button(RichText::new(format!("{repo_label}  ⏷")).strong(), |ui| {
                        ui.set_min_width(260.0);
                        if ui.button("Open local repository…").clicked() {
                            actions.push(Action::OpenDialog);
                            ui.close();
                        }
                        if ui.button("Clone repository…").clicked() {
                            actions.push(Action::ShowClone);
                            ui.close();
                        }
                        if ui.button("Create new repository…").clicked() {
                            actions.push(Action::InitDialog);
                            ui.close();
                        }
                        if self.repo.is_some() {
                            if ui.button("Show in file manager").clicked() {
                                actions.push(Action::RevealInFiles);
                                ui.close();
                            }
                            if ui.button("Close repository").clicked() {
                                actions.push(Action::CloseRepo);
                                ui.close();
                            }
                        }
                        if !self.settings.recent.is_empty() {
                            ui.separator();
                            ui.label(RichText::new("Recent").small().weak());
                            for p in &self.settings.recent {
                                let current = self.repo.as_ref().map(|r| &r.path == p).unwrap_or(false);
                                if ui
                                    .add(egui::Button::selectable(current, git::repo_name(p)))
                                    .on_hover_text(p.display().to_string())
                                    .clicked()
                                {
                                    actions.push(Action::OpenPath(p.clone()));
                                    ui.close();
                                }
                            }
                        }
                    });
                });

                if let Some(r) = &mut self.repo {
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Current branch").small().weak());
                        ui.menu_button(
                            RichText::new(format!("{}  ⏷", r.head.display_name())).strong(),
                            |ui| branch_menu(ui, r, actions),
                        );
                    });

                    ui.separator();
                    let busy = self.busy.is_some();
                    let remote = r.head.has_remote;
                    let on_branch = r.head.branch.is_some();

                    ui.vertical(|ui| {
                        ui.label(RichText::new("Remote").small().weak());
                        ui.horizontal(|ui| {
                            let fetch = ui
                                .add_enabled(remote && !busy, egui::Button::new("⟳ Fetch"))
                                .on_hover_text("Download new commits and branches from the remote");
                            if fetch.clicked() {
                                actions.push(Action::Net(NetOp::Fetch));
                            }

                            let pull_label = if r.head.behind > 0 {
                                format!("⬇ Pull ({})", r.head.behind)
                            } else {
                                "⬇ Pull".into()
                            };
                            let can_pull = remote && !busy && r.head.upstream.is_some();
                            if ui
                                .add_enabled(can_pull, egui::Button::new(pull_label))
                                .on_hover_text("Bring the remote's commits into this branch")
                                .clicked()
                            {
                                actions.push(Action::Net(NetOp::Pull));
                            }

                            let (push_label, set_upstream) = if r.head.upstream.is_none() {
                                let remote_name = git::default_remote(&r.repo).unwrap_or_else(|| "origin".into());
                                let branch = r.head.branch.clone().unwrap_or_default();
                                ("⬆ Publish branch".to_string(), Some((remote_name, branch)))
                            } else if r.head.ahead > 0 {
                                (format!("⬆ Push ({})", r.head.ahead), None)
                            } else {
                                ("⬆ Push".to_string(), None)
                            };
                            let can_push = remote && !busy && on_branch && !r.head.short_id.is_empty();
                            if ui
                                .add_enabled(can_push, egui::Button::new(push_label))
                                .on_hover_text("Upload your commits to the remote")
                                .clicked()
                            {
                                actions.push(Action::Net(NetOp::Push { set_upstream }));
                            }
                        });
                    });
                    if !remote {
                        ui.label(RichText::new("No remote configured").weak().small());
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let icon = if self.settings.light_mode { "🌙" } else { "☀" };
                    if ui.button(icon).on_hover_text("Toggle light / dark theme").clicked() {
                        actions.push(Action::ToggleTheme);
                    }
                    if self.repo.is_some()
                        && ui.button("⟲").on_hover_text("Refresh (F5)").clicked()
                    {
                        actions.push(Action::Refresh);
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(b) = &self.busy {
                    ui.spinner();
                    ui.label(b);
                } else if let Some(t) = &self.toast {
                    let color = if t.error {
                        Color32::from_rgb(229, 83, 75)
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.label(RichText::new(&t.text).color(color));
                } else if let Some(r) = &self.repo {
                    ui.label(RichText::new(r.path.display().to_string()).weak());
                } else {
                    ui.label(RichText::new("Ready").weak());
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(r) = &self.repo {
                        if let Some(state) = &r.head.state {
                            ui.label(RichText::new(state).color(Color32::from_rgb(219, 109, 40)));
                        }
                        if let Some(up) = &r.head.upstream {
                            ui.label(
                                RichText::new(format!("⬆{} ⬇{}  {}", r.head.ahead, r.head.behind, up))
                                    .weak(),
                            );
                        }
                    }
                    if !self.git_cli {
                        ui.label(RichText::new("git not found on PATH").color(Color32::from_rgb(219, 109, 40)))
                            .on_hover_text("Install Git to clone, fetch, pull and push.");
                    }
                });
            });
        });
    }

    fn welcome(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.12).max(16.0));
                ui.heading(RichText::new("Welcome to Gitsot").size(28.0));
                ui.label(RichText::new("A simple Git client. Pick a repository to get started.").weak());
                ui.add_space(24.0);

                let size = Vec2::new(280.0, 36.0);
                if ui.add_sized(size, egui::Button::new("📂  Open a local repository")).clicked() {
                    actions.push(Action::OpenDialog);
                }
                if ui.add_sized(size, egui::Button::new("⬇  Clone a repository from a URL")).clicked() {
                    actions.push(Action::ShowClone);
                }
                if ui.add_sized(size, egui::Button::new("✚  Create a new repository")).clicked() {
                    actions.push(Action::InitDialog);
                }

                if !self.settings.recent.is_empty() {
                    ui.add_space(28.0);
                    ui.label(RichText::new("Recent repositories").strong());
                    ui.add_space(4.0);
                    for p in self.settings.recent.clone() {
                        ui.horizontal(|ui| {
                            let w = 360.0_f32.min(ui.available_width());
                            ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                            let resp = ui.add_sized(
                                Vec2::new(w - 36.0, 28.0),
                                egui::Button::new(format!("{}   {}", git::repo_name(&p), p.display())).truncate(),
                            );
                            if resp.clicked() {
                                actions.push(Action::OpenPath(p.clone()));
                            }
                            if ui.small_button("✖").on_hover_text("Remove from list").clicked() {
                                actions.push(Action::ForgetRecent(p.clone()));
                            }
                        });
                    }
                }
            });
        });
    }

    fn repo_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let tab = &mut self.tab;
        let Some(r) = self.repo.as_mut() else { return };

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(340.0)
            .min_size(240.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.columns(2, |cols| {
                    let label = format!("Changes ({})", r.changes.len());
                    if cols[0]
                        .add_sized([cols[0].available_width(), 28.0], egui::Button::selectable(*tab == Tab::Changes, label))
                        .clicked()
                    {
                        *tab = Tab::Changes;
                    }
                    if cols[1]
                        .add_sized([cols[1].available_width(), 28.0], egui::Button::selectable(*tab == Tab::History, "History"))
                        .clicked()
                    {
                        *tab = Tab::History;
                        if !r.history_loaded {
                            actions.push(Action::LoadHistory);
                        }
                    }
                });
                ui.separator();
                match *tab {
                    Tab::Changes => changes_sidebar(ui, r, actions),
                    Tab::History => history_sidebar(ui, r, actions),
                }
            });

        egui::CentralPanel::default().show(ui, |ui| match *tab {
            Tab::Changes => changes_main(ui, r, actions),
            Tab::History => history_main(ui, r, actions),
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(dialog) = &mut self.dialog else { return };
        let mut close = false;
        let busy = self.busy.is_some();

        let modal = egui::Modal::new(egui::Id::unique("gitsot-dialog")).show(ctx, |ui| {
            ui.set_width(440.0);
            match dialog {
                Dialog::Clone { url, parent, name, name_edited } => {
                    ui.heading("Clone a repository");
                    ui.add_space(8.0);
                    ui.label("Repository URL");
                    let resp = ui.add(
                        TextEdit::singleline(url)
                            .hint_text("https://github.com/user/project.git")
                            .desired_width(f32::INFINITY),
                    );
                    if resp.changed() && !*name_edited {
                        *name = cli::name_from_url(url);
                    }
                    ui.add_space(4.0);
                    ui.label("Save in folder");
                    ui.horizontal(|ui| {
                        ui.add(TextEdit::singleline(parent).desired_width(ui.available_width() - 90.0));
                        if ui.button("Browse…").clicked() {
                            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                                *parent = d.display().to_string();
                            }
                        }
                    });
                    ui.add_space(4.0);
                    ui.label("Folder name");
                    if ui.add(TextEdit::singleline(name).desired_width(f32::INFINITY)).changed() {
                        *name_edited = true;
                    }
                    if !parent.is_empty() && !name.is_empty() {
                        ui.label(
                            RichText::new(format!("Will clone into {}", Path::new(parent.as_str()).join(name.as_str()).display()))
                                .small()
                                .weak(),
                        );
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let ok = !url.trim().is_empty() && !name.trim().is_empty() && !busy;
                        if ui.add_enabled(ok, egui::Button::new("Clone")).clicked() {
                            actions.push(Action::StartClone {
                                url: url.clone(),
                                parent: parent.clone(),
                                name: name.clone(),
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::NewBranch { name } => {
                    ui.heading("Create a branch");
                    ui.add_space(8.0);
                    ui.label("Name");
                    let resp = ui.add(TextEdit::singleline(name).hint_text("my-feature").desired_width(f32::INFINITY));
                    resp.request_focus();
                    ui.label(RichText::new("The new branch starts from your current commit and you will switch to it.").small().weak());
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Create branch")).clicked()
                            || (enter && !name.trim().is_empty())
                        {
                            actions.push(Action::CreateBranch(name.clone()));
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Identity { name, email, commit_after } => {
                    ui.heading("Who are you?");
                    ui.add_space(4.0);
                    ui.label("Git records a name and email with every commit. These are saved in your global Git config.");
                    ui.add_space(8.0);
                    ui.label("Name");
                    ui.add(TextEdit::singleline(name).hint_text("Your Name").desired_width(f32::INFINITY));
                    ui.label("Email");
                    ui.add(TextEdit::singleline(email).hint_text("you@example.com").desired_width(f32::INFINITY));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let label = if *commit_after { "Save and commit" } else { "Save" };
                        if ui.button(label).clicked() {
                            actions.push(Action::SetIdentity {
                                name: name.clone(),
                                email: email.clone(),
                                commit_after: *commit_after,
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::ConfirmDiscard { changes } => {
                    ui.heading("Discard changes?");
                    ui.add_space(4.0);
                    let n = changes.len();
                    ui.label(if n == 1 {
                        format!("Your changes to '{}' will be lost. This cannot be undone.", changes[0].path)
                    } else {
                        format!("Your changes to {n} files will be lost. This cannot be undone.")
                    });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Discard").color(Color32::from_rgb(229, 83, 75))).clicked() {
                            actions.push(Action::Discard(changes.clone()));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::ConfirmDeleteBranch { name } => {
                    ui.heading("Delete branch?");
                    ui.add_space(4.0);
                    ui.label(format!("The local branch '{name}' will be deleted. Commits that only exist on it may be lost."));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Delete").color(Color32::from_rgb(229, 83, 75))).clicked() {
                            actions.push(Action::DeleteBranch(name.clone()));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::SignIn { op, host, username, password, retry } => {
                    ui.heading(format!("Sign in to {host}"));
                    ui.add_space(4.0);
                    if *retry {
                        ui.label(
                            RichText::new("That didn't work. Check your username and password or token.")
                                .color(Color32::from_rgb(229, 83, 75)),
                        );
                    }
                    ui.label("This remote needs a username and password to continue.");
                    ui.add_space(8.0);
                    ui.label("Username");
                    let user_resp = ui.add(TextEdit::singleline(username).desired_width(f32::INFINITY));
                    if username.is_empty() && password.is_empty() && !user_resp.has_focus() {
                        user_resp.request_focus();
                    }
                    ui.label("Password or access token");
                    let pass_resp = ui.add(TextEdit::singleline(password).password(true).desired_width(f32::INFINITY));
                    ui.label(
                        RichText::new(
                            "Many hosts (GitHub, GitLab, Bitbucket, Gitea…) need a personal access token \
                             instead of your account password. If Git has a credential store configured, \
                             it will remember this sign-in.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let ok = !username.trim().is_empty() && !password.is_empty() && !busy;
                        let enter = pass_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if ui.add_enabled(ok, egui::Button::new(format!("Sign in and {}", op.verb()))).clicked()
                            || (enter && ok)
                        {
                            actions.push(Action::NetWithCredentials(
                                op.clone(),
                                Credentials { username: username.trim().to_string(), password: password.clone() },
                            ));
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Message { title, text } => {
                    ui.heading(title.as_str());
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(text.as_str()).monospace()).wrap());
                    });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            close = true;
                        }
                        if ui.button("Copy").clicked() {
                            actions.push(Action::CopyToClipboard(text.clone()));
                        }
                    });
                }
            }
        });

        if modal.should_close() && !busy {
            close = true;
        }
        if close {
            self.dialog = None;
        }
    }
}

fn branch_menu(ui: &mut egui::Ui, r: &mut RepoView, actions: &mut Vec<Action>) {
    ui.set_min_width(300.0);
    if ui.button("✚  New branch…").clicked() {
        actions.push(Action::ShowNewBranch);
        ui.close();
    }
    ui.separator();
    ui.add(TextEdit::singleline(&mut r.branch_filter).hint_text("Filter branches").desired_width(280.0));
    let filter = r.branch_filter.to_lowercase();

    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
        ui.label(RichText::new("Local branches").small().weak());
        for b in r.branches.iter().filter(|b| b.name.to_lowercase().contains(&filter)) {
            ui.horizontal(|ui| {
                let label = if b.is_head { format!("✔ {}", b.name) } else { b.name.clone() };
                let resp = ui.add(egui::Button::selectable(b.is_head, label));
                let resp = match &b.upstream {
                    Some(u) => resp.on_hover_text(format!("Tracks {u}")),
                    None => resp.on_hover_text("Not published yet"),
                };
                if resp.clicked() && !b.is_head {
                    actions.push(Action::Checkout(b.name.clone()));
                    ui.close();
                }
                if !b.is_head {
                    resp.context_menu(|ui| {
                        if ui.button("Delete branch…").clicked() {
                            actions.push(Action::AskDeleteBranch(b.name.clone()));
                            ui.close();
                        }
                    });
                }
            });
        }

        let local: Vec<&str> = r.branches.iter().map(|b| b.name.as_str()).collect();
        let remote: Vec<&String> = r
            .remote_branches
            .iter()
            .filter(|n| n.to_lowercase().contains(&filter))
            .filter(|n| {
                // Hide remote branches that already have a local branch of the same name.
                let short = n.split_once('/').map(|(_, b)| b).unwrap_or(n);
                !local.contains(&short)
            })
            .collect();
        if !remote.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Remote branches").small().weak());
            for n in remote {
                if ui
                    .add(egui::Button::selectable(false, n.as_str()))
                    .on_hover_text("Check out as a new local branch")
                    .clicked()
                {
                    actions.push(Action::CheckoutRemote(n.clone()));
                    ui.close();
                }
            }
        }
    });
    ui.label(RichText::new("Tip: right-click a branch to delete it.").small().weak());
}

fn changes_sidebar(ui: &mut egui::Ui, r: &mut RepoView, actions: &mut Vec<Action>) {
    // Commit box at the bottom of the sidebar.
    egui::Panel::bottom("commit_box").show(ui, |ui| {
        ui.add_space(6.0);
        ui.add(
            TextEdit::singleline(&mut r.summary)
                .hint_text("Summary (required)")
                .desired_width(f32::INFINITY),
        );
        ui.add(
            TextEdit::multiline(&mut r.description)
                .hint_text("Description")
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
        let staged = r.staged_count();
        let branch = r.head.branch.clone().unwrap_or_else(|| "HEAD".into());
        let can_commit = staged > 0 && !r.summary.trim().is_empty();
        let shortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
        let pressed = ui.input_mut(|i| i.consume_shortcut(&shortcut));
        let btn = ui
            .add_enabled(
                can_commit,
                egui::Button::new(RichText::new(format!("Commit to {branch}")).strong())
                    .min_size(Vec2::new(ui.available_width(), 32.0)),
            )
            .on_hover_text(format!("{} ({})", "Commit staged files", ui.ctx().format_shortcut(&shortcut)));
        if btn.clicked() || (pressed && can_commit) {
            actions.push(Action::Commit);
        }
        let hint = if staged == 0 && !r.changes.is_empty() {
            "Tick the files you want to include in the commit.".to_string()
        } else if staged > 0 {
            format!("{staged} file(s) staged")
        } else {
            String::new()
        };
        if !hint.is_empty() {
            ui.label(RichText::new(hint).small().weak());
        }
        ui.add_space(4.0);
    });

    if r.changes.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No local changes").strong());
            ui.label(RichText::new("Edit files in this repository and they will show up here.").weak());
        });
        return;
    }

    // Header with "select all".
    ui.horizontal(|ui| {
        let all_staged = r.changes.iter().all(|c| c.fully_staged());
        let any_staged = r.changes.iter().any(|c| c.staged);
        let mut checked = all_staged;
        let resp = ui.add(egui::Checkbox::new(&mut checked, "").indeterminate(any_staged && !all_staged));
        if resp.clicked() {
            actions.push(if all_staged { Action::UnstageAll } else { Action::StageAll });
        }
        ui.label(format!("{} changed file(s)", r.changes.len()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("Discard all…").clicked() {
                actions.push(Action::AskDiscard(r.changes.clone()));
            }
        });
    });
    ui.separator();

    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for c in &r.changes {
            ui.horizontal(|ui| {
                let mut checked = c.fully_staged();
                let resp = ui
                    .add(egui::Checkbox::new(&mut checked, "").indeterminate(c.staged && !c.fully_staged()))
                    .on_hover_text(if c.fully_staged() { "Staged: will be committed" } else { "Click to stage" });
                if resp.clicked() {
                    actions.push(if c.fully_staged() {
                        Action::Unstage(vec![c.path.clone()])
                    } else {
                        Action::Stage(vec![c.path.clone()])
                    });
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    kind_badge(ui, c.kind);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        let selected = r.selected_change.as_deref() == Some(c.path.as_str());
                        let row = file_row(ui, selected, &c.path);
                        if row.clicked() {
                            actions.push(Action::SelectChange(c.path.clone()));
                        }
                        row.context_menu(|ui| {
                            if c.fully_staged() {
                                if ui.button("Unstage").clicked() {
                                    actions.push(Action::Unstage(vec![c.path.clone()]));
                                    ui.close();
                                }
                            } else if ui.button("Stage").clicked() {
                                actions.push(Action::Stage(vec![c.path.clone()]));
                                ui.close();
                            }
                            if ui.button("Discard changes…").clicked() {
                                actions.push(Action::AskDiscard(vec![c.clone()]));
                                ui.close();
                            }
                            if ui.button("Copy path").clicked() {
                                actions.push(Action::CopyToClipboard(c.path.clone()));
                                ui.close();
                            }
                        });
                    });
                });
            });
        }
    });
}

fn changes_main(ui: &mut egui::Ui, r: &mut RepoView, _actions: &mut Vec<Action>) {
    let Some(path) = r.selected_change.clone() else {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("Select a file to see its changes").weak());
        });
        return;
    };
    let kind = r.changes.iter().find(|c| c.path == path).map(|c| c.kind);
    ui.horizontal(|ui| {
        if let Some(k) = kind {
            kind_badge(ui, k);
        }
        ui.label(RichText::new(&path).strong().monospace());
    });
    ui.separator();
    match &r.change_diff {
        Some(Ok(d)) => diff_view(ui, d, "change_diff"),
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(Color32::from_rgb(229, 83, 75)));
        }
        None => {}
    }
}

fn history_sidebar(ui: &mut egui::Ui, r: &mut RepoView, actions: &mut Vec<Action>) {
    if r.history.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No commits yet").strong());
        });
        return;
    }
    let row_h = 46.0;
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, row_h, r.history.len(), |ui, range| {
        for c in &r.history[range] {
            let selected = r.selected_commit == Some(c.id);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
            let visuals = ui.visuals();
            if selected {
                ui.painter().rect_filled(rect, 4.0, visuals.selection.bg_fill);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
            }
            let text_color = if selected { visuals.selection.stroke.color } else { visuals.strong_text_color() };
            let weak = if selected { visuals.selection.stroke.color } else { visuals.weak_text_color() };
            let painter = ui.painter().with_clip_rect(rect.shrink(2.0));
            painter.text(
                rect.left_top() + Vec2::new(8.0, 6.0),
                Align2::LEFT_TOP,
                &c.summary,
                FontId::proportional(14.0),
                text_color,
            );
            painter.text(
                rect.left_top() + Vec2::new(8.0, 26.0),
                Align2::LEFT_TOP,
                format!("{} · {}", c.author, relative_time(c.time)),
                FontId::proportional(12.0),
                weak,
            );
            if resp.clicked() && !selected {
                actions.push(Action::SelectCommit(c.id));
            }
            resp.context_menu(|ui| {
                if ui.button("Copy commit id").clicked() {
                    actions.push(Action::CopyToClipboard(c.id.to_string()));
                    ui.close();
                }
            });
        }
    });
}

fn history_main(ui: &mut egui::Ui, r: &mut RepoView, actions: &mut Vec<Action>) {
    let Some(id) = r.selected_commit else {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("Select a commit").weak());
        });
        return;
    };
    let Some(c) = r.history.iter().find(|c| c.id == id).cloned() else { return };

    egui::Panel::top("commit_header").show(ui, |ui| {
        ui.add_space(4.0);
        ui.label(RichText::new(&c.summary).heading());
        let body = c.message.split_once('\n').map(|x| x.1).unwrap_or("").trim();
        if !body.is_empty() {
            egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                ui.label(body);
            });
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("{} <{}>", c.author, c.email)).weak());
            ui.label(RichText::new(full_time(c.time)).weak());
            if ui
                .link(RichText::new(&c.short_id).monospace())
                .on_hover_text("Copy full commit id")
                .clicked()
            {
                actions.push(Action::CopyToClipboard(c.id.to_string()));
            }
            ui.label(RichText::new(format!("{} file(s) changed", r.commit_files.len())).weak());
        });
        ui.add_space(4.0);
    });

    egui::Panel::left("commit_files")
        .resizable(true)
        .default_size(240.0)
        .min_size(140.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for f in &r.commit_files {
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            kind_badge(ui, f.kind);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let sel = r.selected_commit_file.as_deref() == Some(f.path.as_str());
                                if file_row(ui, sel, &f.path).clicked() {
                                    actions.push(Action::SelectCommitFile(f.path.clone()));
                                }
                            });
                        });
                    });
                }
            });
        });

    egui::CentralPanel::default().show(ui, |ui| match &r.commit_diff {
        Some(Ok(d)) => diff_view(ui, d, "commit_diff"),
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(Color32::from_rgb(229, 83, 75)));
        }
        None => {
            ui.label(RichText::new("Select a file").weak());
        }
    });
}

/// Fast, virtualised diff renderer: only visible rows are laid out.
fn diff_view(ui: &mut egui::Ui, diff: &FileDiff, id: &str) {
    if diff.binary {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("Binary file: no text diff to show").weak());
        });
        return;
    }
    if diff.lines.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("No content changes (for example a mode change or an empty file)").weak());
        });
        return;
    }

    let font = FontId::monospace(13.0);
    let (row_h, char_w) = ui.ctx().fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, 'M')));
    let row_h = row_h + 2.0;
    let dark = ui.visuals().dark_mode;
    let gutter_chars = 13usize; // "12345 12345  "
    let longest = diff.lines.iter().map(|l| l.text.chars().count()).max().unwrap_or(0);
    let content_w = ((gutter_chars + 2 + longest) as f32 * char_w + 24.0).max(ui.available_width());

    let (add_bg, del_bg, hunk_bg) = if dark {
        (
            Color32::from_rgba_unmultiplied(46, 160, 67, 40),
            Color32::from_rgba_unmultiplied(248, 81, 73, 40),
            Color32::from_rgba_unmultiplied(56, 139, 253, 30),
        )
    } else {
        (
            Color32::from_rgb(218, 251, 225),
            Color32::from_rgb(255, 235, 233),
            Color32::from_rgb(221, 244, 255),
        )
    };
    let text_col = ui.visuals().text_color();
    let weak_col = ui.visuals().weak_text_color();

    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::both()
        .id_salt(id)
        .auto_shrink(false)
        .show_rows(ui, row_h, diff.lines.len() + usize::from(diff.truncated), |ui, range| {
            for i in range {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(content_w, row_h), Sense::hover());
                let painter = ui.painter();
                let Some(line) = diff.lines.get(i) else {
                    painter.text(
                        rect.left_center() + Vec2::new(8.0, 0.0),
                        Align2::LEFT_CENTER,
                        format!("… diff truncated after {} lines", git::MAX_DIFF_LINES),
                        font.clone(),
                        weak_col,
                    );
                    continue;
                };
                let (bg, sign) = match line.kind {
                    LineKind::Added => (Some(add_bg), "+"),
                    LineKind::Removed => (Some(del_bg), "-"),
                    LineKind::Hunk => (Some(hunk_bg), ""),
                    _ => (None, " "),
                };
                if let Some(bg) = bg {
                    painter.rect_filled(rect, 0.0, bg);
                }
                let num = |n: Option<u32>| n.map(|n| format!("{n:>5}")).unwrap_or_else(|| "     ".into());
                let gutter = match line.kind {
                    LineKind::Hunk | LineKind::Info => "             ".to_string(),
                    _ => format!("{} {}  ", num(line.old_no), num(line.new_no)),
                };
                painter.text(
                    rect.left_center() + Vec2::new(4.0, 0.0),
                    Align2::LEFT_CENTER,
                    &gutter,
                    font.clone(),
                    weak_col,
                );
                let body = format!("{sign}{}", line.text.replace('\t', "    "));
                let color = match line.kind {
                    LineKind::Hunk | LineKind::Info => weak_col,
                    _ => text_col,
                };
                painter.text(
                    rect.left_center() + Vec2::new(4.0 + gutter_chars as f32 * char_w, 0.0),
                    Align2::LEFT_CENTER,
                    body,
                    font.clone(),
                    color,
                );
            }
        });
}
