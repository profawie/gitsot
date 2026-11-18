use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, FontId, Key, KeyboardShortcut, Layout, Modifiers, Pos2, RichText,
    Sense, Stroke, TextEdit, Vec2,
};
use git2::{Oid, Repository};
use serde::{Deserialize, Serialize};

use crate::cli::{self, Credentials, NetOp, NetResult};
use crate::git::{
    self, BranchInfo, ChangeKind, CommitFile, CommitInfo, FileChange, FileDiff, HeadInfo, LineKind, RefKind,
    RefLabel,
};
use crate::graph::{self, Graph};
use crate::identity::{self, FolderRule, Identity};
use crate::theme;
use std::collections::HashMap;

const SETTINGS_KEY: &str = "gitsot-settings";
const MAX_RECENT: usize = 12;
/// Git logo glyph from egui's bundled icon font, used as the "branch" icon.
const BRANCH_ICON: &str = "\u{E625}";

// ---------------------------------------------------------------------------
// Persistent settings
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    recent: Vec<PathBuf>,
    last_repo: Option<PathBuf>,
    clone_parent: Option<PathBuf>,
    light_mode: bool,
    /// Show history as a branch tree (default) instead of a plain list.
    history_view_tree: bool,
    /// Group name for each repository on the Repositories page.
    groups: HashMap<PathBuf, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            recent: Vec::new(),
            last_repo: None,
            clone_parent: None,
            light_mode: false,
            history_view_tree: true,
            groups: HashMap::new(),
        }
    }
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
    /// History includes every branch (tree view) rather than just HEAD.
    history_all: bool,
    graph: Graph,
    labels: HashMap<Oid, Vec<RefLabel>>,
    selected_commit: Option<Oid>,
    commit_files: Vec<CommitFile>,
    selected_commit_file: Option<String>,
    commit_diff: Option<Result<FileDiff, String>>,

    branches: Vec<BranchInfo>,
    remote_branches: Vec<String>,
    branch_filter: String,

    /// Folder identity rules (kept in sync by the app) and this repo's identity check.
    rules: Vec<FolderRule>,
    id_status: identity::Status,
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
            history_all: false,
            graph: Graph::default(),
            labels: HashMap::new(),
            selected_commit: None,
            commit_files: Vec::new(),
            selected_commit_file: None,
            commit_diff: None,
            branches: Vec::new(),
            remote_branches: Vec::new(),
            branch_filter: String::new(),
            rules: identity::rules(),
            id_status: identity::Status { effective: Identity::default(), source: identity::Source::Missing, rule: None },
        };
        view.refresh()?;
        Ok(view)
    }

    fn name(&self) -> String {
        git::repo_name(&self.path)
    }

    /// Reload everything that can change behind our back.
    fn refresh(&mut self) -> anyhow::Result<()> {
        self.id_status = identity::status(&self.repo, &self.path, &self.rules);
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
            self.load_history(self.history_all);
        }
        Ok(())
    }

    fn load_change_diff(&mut self) {
        self.change_diff = self
            .selected_change
            .as_ref()
            .map(|p| git::working_diff(&self.repo, p).map_err(|e| e.to_string()));
    }

    fn load_history(&mut self, all_refs: bool) {
        self.history = git::history(&self.repo, git::MAX_HISTORY, all_refs).unwrap_or_default();
        self.history_loaded = true;
        self.history_all = all_refs;
        self.labels = git::ref_labels(&self.repo);
        self.graph = if all_refs { graph::layout(&self.history) } else { Graph::default() };
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
enum Page {
    Repositories,
    Changes,
    History,
    Settings,
    About,
}

/// What the Repositories page shows for one remembered repository.
struct RepoCard {
    path: PathBuf,
    name: String,
    branch: Option<String>,
    remote: Option<String>,
    exists: bool,
    identity: Option<identity::Status>,
}

impl RepoCard {
    fn load(path: &Path, rules: &[FolderRule]) -> Self {
        let repo = git::open(path).ok();
        RepoCard {
            path: path.to_path_buf(),
            name: git::repo_name(path),
            branch: repo.as_ref().map(|r| git::head_info(r).display_name()),
            remote: repo.as_ref().and_then(git::remote_url),
            exists: repo.is_some(),
            identity: repo.as_ref().map(|r| identity::status(r, path, rules)),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IdTab {
    Global,
    Repo,
}

enum Dialog {
    Clone { url: String, parent: String, name: String, name_edited: bool },
    NewBranch { name: String },
    Identity { name: String, email: String, commit_after: bool },
    ConfirmDiscard { changes: Vec<FileChange> },
    ConfirmDeleteBranch { name: String },
    Message { title: String, text: String },
    SignIn { op: NetOp, host: String, username: String, password: String, retry: bool },
    /// About to commit or push with an identity that doesn't match the folder rule.
    IdentityWarning { push: Option<NetOp>, rule: FolderRule, effective: Identity, bad_commits: Vec<(String, String)> },
    SetGroup { path: PathBuf, name: String },
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
    SelectCommit(Oid),
    SelectCommitFile(String),
    RevealInFiles,
    CopyToClipboard(String),
    SetLightMode(bool),
    ForgetRecent(PathBuf),
    GoTo(Page),
    SetHistoryTree(bool),
    SetLocalIdentity(Identity),
    EditRepoIdentity,
    ClearLocalIdentity,
    AddRule { folder: String, identity: Identity },
    RemoveRule(FolderRule),
    /// Use the folder rule's identity for this repository, then commit.
    AdoptRuleAndCommit(Identity),
    CommitIgnoringIdentity,
    PushIgnoringIdentity(NetOp),
    AskGroup(PathBuf),
    SetGroup(PathBuf, String),
}

struct Toast {
    text: String,
    error: bool,
    at: Instant,
}

pub struct GitsotApp {
    settings: Settings,
    repo: Option<RepoView>,
    page: Page,
    cards: Vec<RepoCard>,
    repo_filter: String,
    identity: (String, String),
    rules: Vec<FolderRule>,
    id_tab: IdTab,
    local_form: Identity,
    rule_form: (String, Identity),
    skip_identity_check: bool,
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
            page: Page::Repositories,
            cards: Vec::new(),
            repo_filter: String::new(),
            identity: { let g = identity::global(); (g.name, g.email) },
            rules: identity::rules(),
            id_tab: IdTab::Global,
            local_form: Identity::default(),
            rule_form: (String::new(), Identity::default()),
            skip_identity_check: false,
            dialog: None,
            toast: None,
            busy: None,
            tx,
            rx,
            git_cli: cli::git_available(),
            was_focused: true,
            last_username: String::new(),
        };
        theme::apply(&cc.egui_ctx, app.settings.light_mode);

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
        app.reload_cards();
        app
    }

    fn reload_cards(&mut self) {
        let rules = self.rules.clone();
        self.cards = self.settings.recent.iter().map(|p| RepoCard::load(p, &rules)).collect();
    }

    /// Re-read identity rules and push them to the open repository.
    fn reload_identity(&mut self) {
        self.rules = identity::rules();
        let g = identity::global();
        self.identity = (g.name, g.email);
        if let Some(r) = &mut self.repo {
            r.rules = self.rules.clone();
            r.id_status = identity::status(&r.repo, &r.path, &r.rules);
            self.local_form = identity::local(&r.repo).unwrap_or_default();
        }
        self.reload_cards();
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
                self.page = Page::Changes;
                self.reload_identity();
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
                self.page = Page::Repositories;
                self.reload_cards();
            }
            Action::Refresh => {
                self.refresh();
            }
            Action::ForgetRecent(p) => {
                self.settings.recent.retain(|r| r != &p);
                self.reload_cards();
            }
            Action::SetLightMode(light) => {
                self.settings.light_mode = light;
                theme::apply(ctx, light);
            }
            Action::GoTo(page) => {
                if page == Page::Repositories {
                    self.reload_cards();
                }
                if page == Page::Settings {
                    self.reload_identity();
                }
                if page == Page::History {
                    if let Some(r) = &mut self.repo {
                        if !r.history_loaded || r.history_all != self.settings.history_view_tree {
                            r.load_history(self.settings.history_view_tree);
                        }
                    }
                }
                self.page = page;
            }
            Action::SetHistoryTree(tree) => {
                self.settings.history_view_tree = tree;
                if let Some(r) = &mut self.repo {
                    r.load_history(tree);
                }
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
                match identity::set_global(&Identity { name: name.clone(), email: email.clone() }) {
                    Ok(()) => {
                        self.reload_identity();
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
            Action::AddRule { folder, identity: id } => {
                match identity::add_rule(Path::new(&folder), &id) {
                    Ok(()) => {
                        self.rule_form = (String::new(), Identity::default());
                        self.reload_identity();
                        self.info(format!("Repositories under {folder} will now use {}", id.display()));
                    }
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
            Action::RemoveRule(rule) => match identity::remove_rule(&rule) {
                Ok(()) => {
                    self.reload_identity();
                    self.info(format!("Removed the rule for {}", rule.folder));
                }
                Err(e) => self.error(format!("{e:#}")),
            },
            Action::SetLocalIdentity(id) => {
                let Some(r) = &self.repo else { return };
                match identity::set_local(&r.repo, &id) {
                    Ok(()) => {
                        self.reload_identity();
                        self.info(format!("This repository now commits as {}", id.display()));
                    }
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
            Action::ClearLocalIdentity => {
                let Some(r) = &self.repo else { return };
                match identity::clear_local(&r.repo) {
                    Ok(()) => {
                        self.reload_identity();
                        self.info("This repository now uses the global identity or folder rule");
                    }
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
            Action::AdoptRuleAndCommit(id) => {
                self.dialog = None;
                if let Some(r) = &self.repo {
                    if let Err(e) = identity::set_local(&r.repo, &id) {
                        self.error(format!("{e:#}"));
                        return;
                    }
                }
                self.reload_identity();
                self.apply(ctx, Action::Commit);
            }
            Action::CommitIgnoringIdentity => {
                self.dialog = None;
                self.skip_identity_check = true;
                self.apply(ctx, Action::Commit);
                self.skip_identity_check = false;
            }
            Action::PushIgnoringIdentity(op) => {
                self.dialog = None;
                self.skip_identity_check = true;
                self.start_net(ctx, op, None);
                self.skip_identity_check = false;
            }
            Action::EditRepoIdentity => {
                self.reload_identity();
                self.id_tab = IdTab::Repo;
                self.page = Page::Settings;
            }
            Action::AskGroup(path) => {
                let name = self.settings.groups.get(&path).cloned().unwrap_or_default();
                self.dialog = Some(Dialog::SetGroup { path, name });
            }
            Action::SetGroup(path, name) => {
                self.dialog = None;
                let name = name.trim().to_string();
                if name.is_empty() {
                    self.settings.groups.remove(&path);
                } else {
                    self.settings.groups.insert(path, name);
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
                                let id = identity::effective(&r.repo);
                                self.dialog = Some(Dialog::Identity { name: id.name, email: id.email, commit_after: true });
                                return Ok(None);
                            }
                            r.id_status = identity::status(&r.repo, &r.path, &r.rules);
                            if !self.skip_identity_check {
                                if let Some(rule) = r.id_status.mismatch() {
                                    if r.summary.trim().is_empty() {
                                        anyhow::bail!("Please write a commit summary");
                                    }
                                    self.dialog = Some(Dialog::IdentityWarning {
                                        push: None,
                                        rule: rule.clone(),
                                        effective: r.id_status.effective.clone(),
                                        bad_commits: Vec::new(),
                                    });
                                    return Ok(None);
                                }
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
        if let (NetOp::Push { .. }, Some(r), false) = (&op, &self.repo, self.skip_identity_check) {
            if let Some(rule) = identity::rule_for(&r.rules, &r.path) {
                let bad: Vec<(String, String)> = identity::unpushed_authors(&r.repo)
                    .into_iter()
                    .filter(|(_, email)| !email.eq_ignore_ascii_case(rule.identity.email.trim()))
                    .collect();
                if !bad.is_empty() {
                    self.dialog = Some(Dialog::IdentityWarning {
                        push: Some(op),
                        rule: rule.clone(),
                        effective: identity::effective(&r.repo),
                        bad_commits: bad,
                    });
                    return;
                }
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

        if self.repo.is_none() && matches!(self.page, Page::Changes | Page::History) {
            self.page = Page::Repositories;
        }

        self.nav_sidebar(ui, &mut actions);
        self.status_bar(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::same(0)))
            .show(ui, |ui| match self.page {
                Page::Repositories => self.repositories_page(ui, &mut actions),
                Page::Changes | Page::History => self.repo_page(ui, &mut actions),
                Page::Settings => self.settings_page(ui, &mut actions),
                Page::About => about_page(ui),
            });

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
    fn nav_sidebar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let dark = ui.visuals().dark_mode;
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(200.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::nav_bg(dark))
                    .inner_margin(egui::Margin { left: 12, right: 12, top: 16, bottom: 10 }),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    theme::logo(ui, 26.0);
                    ui.label(RichText::new("gitsot").size(20.0).strong());
                });
                ui.add_space(18.0);

                let page = self.page;
                if theme::nav_item(ui, page == Page::Repositories, "Repositories", None).clicked() {
                    actions.push(Action::GoTo(Page::Repositories));
                }

                if let Some(r) = &self.repo {
                    ui.add_space(14.0);
                    ui.add(egui::Label::new(RichText::new(r.name().to_uppercase()).small().weak()).truncate())
                        .on_hover_text(r.path.display().to_string());
                    ui.add_space(2.0);
                    if theme::nav_item(ui, page == Page::Changes, "Changes", Some(r.changes.len())).clicked() {
                        actions.push(Action::GoTo(Page::Changes));
                    }
                    if theme::nav_item(ui, page == Page::History, "History", None).clicked() {
                        actions.push(Action::GoTo(Page::History));
                    }
                }

                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small().weak());
                    ui.add_space(4.0);
                    if theme::nav_item(ui, page == Page::About, "About", None).clicked() {
                        actions.push(Action::GoTo(Page::About));
                    }
                    if theme::nav_item(ui, page == Page::Settings, "Settings", None).clicked() {
                        actions.push(Action::GoTo(Page::Settings));
                    }
                });
            });
    }

    fn repositories_page(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        page_frame(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Repositories").size(26.0).strong());
                    ui.label(RichText::new("Git repositories you work with. Any Git host works.").weak());
                });
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if theme::primary(ui, "+ New").on_hover_text("Create a new repository").clicked() {
                        actions.push(Action::InitDialog);
                    }
                    if theme::primary(ui, "Clone").on_hover_text("Clone a repository from a URL").clicked() {
                        actions.push(Action::ShowClone);
                    }
                    if theme::primary(ui, "Open folder").on_hover_text("Open a repository on this computer (Ctrl+O)").clicked() {
                        actions.push(Action::OpenDialog);
                    }
                    ui.add(TextEdit::singleline(&mut self.repo_filter).hint_text("Search repositories").desired_width(200.0));
                });
            });
            ui.add_space(18.0);

            if self.cards.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    theme::logo(ui, 56.0);
                    ui.add_space(10.0);
                    ui.label(RichText::new("No repositories yet").size(18.0));
                    ui.label(RichText::new("Open a folder, clone from a URL, or create a new repository.").weak());
                });
                return;
            }

            let filter = self.repo_filter.to_lowercase();
            let cards: Vec<&RepoCard> = self
                .cards
                .iter()
                .filter(|c| filter.is_empty() || c.name.to_lowercase().contains(&filter) || c.path.to_string_lossy().to_lowercase().contains(&filter))
                .collect();

            let current = self.repo.as_ref().map(|r| r.path.clone());
            let card_w = 300.0;
            let gap = 14.0;
            let per_row = (((ui.available_width() + gap) / (card_w + gap)).floor() as usize).max(1);

            // Group the cards; named groups A–Z first, then "Ungrouped".
            let mut groups: Vec<(String, Vec<&RepoCard>)> = Vec::new();
            for card in cards {
                let g = self.settings.groups.get(&card.path).cloned().unwrap_or_default();
                match groups.iter_mut().find(|(name, _)| *name == g) {
                    Some((_, list)) => list.push(card),
                    None => groups.push((g, vec![card])),
                }
            }
            groups.sort_by_key(|(name, _)| (name.is_empty(), name.to_lowercase()));

            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for (group, list) in &groups {
                    let title = if group.is_empty() { "Ungrouped" } else { group.as_str() };
                    ui.label(RichText::new(format!("{title}  ({})", list.len())).strong().weak());
                    ui.add_space(6.0);
                    for chunk in list.chunks(per_row) {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            for card in chunk {
                                let is_current = current.as_ref() == Some(&card.path);
                                repo_card(ui, card, is_current, card_w, actions);
                            }
                        });
                        ui.add_space(gap);
                    }
                    ui.add_space(8.0);
                }
            });
        });
    }

    fn repo_page(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let busy = self.busy.is_some();
        let page = self.page;
        let history_tree = self.settings.history_view_tree;
        let Some(r) = self.repo.as_mut() else { return };

        // Header: repository name, branch picker and remote buttons.
        egui::Panel::top("repo_header")
            .frame(egui::Frame::new().inner_margin(egui::Margin { left: 20, right: 20, top: 14, bottom: 10 }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if page == Page::Changes { "Changes" } else { "History" }).size(24.0).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        remote_buttons(ui, r, busy, actions);
                        ui.add_space(4.0);
                        ui.menu_button(RichText::new(format!("{BRANCH_ICON}  {}  ⏷", r.head.display_name())), |ui| {
                            branch_menu(ui, r, actions)
                        })
                        .response
                        .on_hover_text("Current branch: switch or create branches");
                        ui.menu_button("☰", |ui| {
                            if ui.button("Refresh (F5)").clicked() {
                                actions.push(Action::Refresh);
                                ui.close();
                            }
                            if ui.button("Show in file manager").clicked() {
                                actions.push(Action::RevealInFiles);
                                ui.close();
                            }
                            if ui.button("Close repository").clicked() {
                                actions.push(Action::CloseRepo);
                                ui.close();
                            }
                        });
                        if page == Page::History {
                            ui.add_space(8.0);
                            segmented(ui, history_tree, actions);
                        }
                    });
                });
                ui.add(
                    egui::Label::new(RichText::new(format!("{}  ·  {}", r.name(), r.path.display())).weak()).truncate(),
                );
            });

        match page {
            Page::Changes => {
                egui::Panel::left("changes_list")
                    .resizable(true)
                    .default_size(340.0)
                    .min_size(240.0)
                    .frame(egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 8, top: 4, bottom: 8 }))
                    .show(ui, |ui| changes_sidebar(ui, r, actions));
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().inner_margin(egui::Margin::same(10)))
                    .show(ui, |ui| changes_main(ui, r, actions));
            }
            _ => {
                egui::Panel::left(if history_tree { "history_tree" } else { "history_list" })
                    .resizable(true)
                    .default_size(if history_tree { 520.0 } else { 360.0 })
                    .min_size(240.0)
                    .frame(egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 8, top: 4, bottom: 8 }))
                    .show(ui, |ui| history_sidebar(ui, r, history_tree, actions));
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().inner_margin(egui::Margin::same(10)))
                    .show(ui, |ui| history_main(ui, r, actions));
            }
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        page_frame(ui, |ui| {
            ui.label(RichText::new("Settings").size(26.0).strong());
            ui.add_space(14.0);

            theme::card_frame(ui, false).show(ui, |ui| {
                ui.set_width(560.0);
                ui.label(RichText::new("Appearance").strong());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let light = self.settings.light_mode;
                    if ui.add(egui::Button::selectable(!light, "Dark")).clicked() && light {
                        actions.push(Action::SetLightMode(false));
                    }
                    if ui.add(egui::Button::selectable(light, "Light")).clicked() && !light {
                        actions.push(Action::SetLightMode(true));
                    }
                });
            });
            ui.add_space(12.0);

            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            theme::card_frame(ui, false).show(ui, |ui| {
                ui.set_width(560.0);
                ui.label(RichText::new("Identity").strong());
                ui.label(
                    RichText::new("The name and email recorded in every commit. Check it before you commit or push.")
                        .small()
                        .weak(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (tab, label) in [(IdTab::Global, "Global"), (IdTab::Repo, "This repository")] {
                        let enabled = tab == IdTab::Global || self.repo.is_some();
                        let resp = if self.id_tab == tab {
                            theme::primary(ui, label)
                        } else {
                            ui.add_enabled(enabled, egui::Button::new(label))
                        };
                        if resp.clicked() {
                            self.id_tab = tab;
                        }
                    }
                });
                ui.add_space(8.0);
                match (self.id_tab, &self.repo) {
                    (IdTab::Repo, Some(r)) => {
                        ui.label(RichText::new(format!("{}  ·  {}", r.name(), r.path.display())).small().weak());
                        ui.horizontal(|ui| {
                            ui.label("Commits are made as:");
                            identity_line(ui, &r.id_status, false);
                        });
                        if let Some(rule) = &r.id_status.rule {
                            ui.label(
                                RichText::new(format!("Folder rule for {}: {}", rule.folder, rule.identity.display()))
                                    .small()
                                    .weak(),
                            );
                        }
                        ui.add_space(6.0);
                        ui.label("Name (this repository only)");
                        ui.add(TextEdit::singleline(&mut self.local_form.name).hint_text("Uses global / folder rule").desired_width(f32::INFINITY));
                        ui.label("Email (this repository only)");
                        ui.add(TextEdit::singleline(&mut self.local_form.email).hint_text("Uses global / folder rule").desired_width(f32::INFINITY));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if theme::primary(ui, "Save for this repository").clicked() {
                                actions.push(Action::SetLocalIdentity(self.local_form.clone()));
                            }
                            if let Some(rule) = &r.id_status.rule {
                                if ui.button(format!("Use folder rule ({})", rule.identity.email)).clicked() {
                                    actions.push(Action::ClearLocalIdentity);
                                }
                            } else if identity::local(&r.repo).is_some() && ui.button("Use global").clicked() {
                                actions.push(Action::ClearLocalIdentity);
                            }
                        });
                        ui.label(RichText::new("Saved in this repository's .git/config. It wins over global and folder rules.").small().weak());
                    }
                    _ => {
                        ui.label("Name");
                        ui.add(TextEdit::singleline(&mut self.identity.0).hint_text("Your Name").desired_width(f32::INFINITY));
                        ui.label("Email");
                        ui.add(TextEdit::singleline(&mut self.identity.1).hint_text("you@example.com").desired_width(f32::INFINITY));
                        ui.add_space(6.0);
                        if theme::primary(ui, "Save global").clicked() {
                            actions.push(Action::SetIdentity {
                                name: self.identity.0.clone(),
                                email: self.identity.1.clone(),
                                commit_after: false,
                            });
                        }
                        ui.label(RichText::new("Used everywhere unless a folder rule or repository setting says otherwise.").small().weak());
                    }
                }
            });
            ui.add_space(12.0);

            theme::card_frame(ui, false).show(ui, |ui| {
                ui.set_width(560.0);
                ui.label(RichText::new("Folder rules").strong());
                ui.label(
                    RichText::new(
                        "Every repository inside a folder uses that folder's identity, for example all of \
                         C:\\work\\company uses your company email. Stored as Git includeIf rules, so the git \
                         command follows them too. Gitsot warns you before committing or pushing with a different email.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(6.0);
                if self.rules.is_empty() {
                    ui.label(RichText::new("No folder rules yet.").weak());
                }
                for rule in self.rules.clone() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("📁").weak());
                        ui.label(RichText::new(&rule.folder).strong());
                        ui.label(RichText::new(format!("→ {}", rule.identity.display())).weak());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                actions.push(Action::RemoveRule(rule.clone()));
                            }
                        });
                    });
                }
                ui.separator();
                ui.label(RichText::new("Add a rule").small().strong());
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.rule_form.0).hint_text("Folder, e.g. C:\\work\\company").desired_width(400.0));
                    if ui.button("Browse…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_title("Folder for this identity").pick_folder() {
                            self.rule_form.0 = d.display().to_string();
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.rule_form.1.name).hint_text("Name").desired_width(180.0));
                    ui.add(TextEdit::singleline(&mut self.rule_form.1.email).hint_text("Email").desired_width(220.0));
                    let ok = !self.rule_form.0.trim().is_empty() && !self.rule_form.1.email.trim().is_empty();
                    if theme::primary_enabled(ui, ok, "Add rule").clicked() {
                        actions.push(Action::AddRule { folder: self.rule_form.0.clone(), identity: self.rule_form.1.clone() });
                    }
                });
            });
            ui.add_space(12.0);

            theme::card_frame(ui, false).show(ui, |ui| {
                ui.set_width(560.0);
                ui.label(RichText::new("Git command").strong());
                if self.git_cli {
                    ui.label(RichText::new("Found on PATH. Used for clone, fetch, pull and push.").weak());
                } else {
                    ui.label(
                        RichText::new("Not found. Install Git from git-scm.com to clone, fetch, pull and push.")
                            .color(theme::ORANGE),
                    );
                }
            });
            });
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
                        if theme::primary_enabled(ui, ok, "Clone").clicked() {
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
                        if theme::primary_enabled(ui, !name.trim().is_empty(), "Create branch").clicked()
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
                        if theme::primary(ui, label).clicked() {
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
                        if theme::primary_enabled(ui, ok, format!("Sign in and {}", op.verb())).clicked()
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
                Dialog::IdentityWarning { push, rule, effective, bad_commits } => {
                    ui.label(RichText::new("⚠ Check your identity").size(20.0).color(theme::ORANGE));
                    ui.add_space(6.0);
                    ui.label(format!("This repository is inside {}, which uses:", rule.folder));
                    ui.label(RichText::new(rule.identity.display()).strong());
                    ui.add_space(4.0);
                    if push.is_none() {
                        ui.label("But this commit would be made as:");
                        ui.label(RichText::new(effective.display()).strong().color(theme::RED));
                    } else {
                        ui.label(format!("But {} commit(s) you are about to push were made with another email:", bad_commits.len()));
                        egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                            for (id, email) in bad_commits.iter() {
                                ui.label(RichText::new(format!("{id}  {email}")).monospace().color(theme::RED));
                            }
                        });
                        ui.label(
                            RichText::new("Pushing shares these commits with everyone on the remote.")
                                .small()
                                .weak(),
                        );
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        match push {
                            None => {
                                if theme::primary(ui, format!("Use {} and commit", rule.identity.email)).clicked() {
                                    actions.push(Action::AdoptRuleAndCommit(rule.identity.clone()));
                                }
                                if ui.button("Commit anyway").clicked() {
                                    actions.push(Action::CommitIgnoringIdentity);
                                }
                            }
                            Some(op) => {
                                if ui.button(RichText::new("Push anyway").color(theme::RED)).clicked() {
                                    actions.push(Action::PushIgnoringIdentity(op.clone()));
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::SetGroup { path, name } => {
                    ui.heading("Move to group");
                    ui.add_space(4.0);
                    ui.label(RichText::new(path.display().to_string()).small().weak());
                    ui.add_space(6.0);
                    let resp = ui.add(TextEdit::singleline(name).hint_text("Group name, e.g. Company").desired_width(f32::INFINITY));
                    resp.request_focus();
                    let mut existing: Vec<String> = self.settings.groups.values().cloned().collect();
                    existing.sort();
                    existing.dedup();
                    if !existing.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Existing:").small().weak());
                            for g in existing {
                                if ui.small_button(&g).clicked() {
                                    *name = g;
                                }
                            }
                        });
                    }
                    ui.label(RichText::new("Leave empty to remove it from its group.").small().weak());
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if theme::primary(ui, "Save").clicked() || enter {
                            actions.push(Action::SetGroup(path.clone(), name.clone()));
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
                        if theme::primary(ui, "OK").clicked() {
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

/// Standard padding around a full page.
fn page_frame(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .inner_margin(egui::Margin { left: 24, right: 24, top: 18, bottom: 12 })
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add(ui)
        });
}

fn repo_card(ui: &mut egui::Ui, card: &RepoCard, is_current: bool, width: f32, actions: &mut Vec<Action>) {
    theme::card_frame(ui, is_current).show(ui, |ui| {
        ui.with_layout(Layout::top_down(Align::Min), |ui| repo_card_body(ui, card, is_current, width, actions));
    });
}

fn repo_card_body(ui: &mut egui::Ui, card: &RepoCard, is_current: bool, width: f32, actions: &mut Vec<Action>) {
    {
        ui.set_min_width(width - 28.0);
        ui.set_max_width(width - 28.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button("☰", |ui| {
                    if ui.button("Move to group…").clicked() {
                        actions.push(Action::AskGroup(card.path.clone()));
                        ui.close();
                    }
                    if ui.button("Remove from list").on_hover_text("Files on disk are kept").clicked() {
                        actions.push(Action::ForgetRecent(card.path.clone()));
                        ui.close();
                    }
                });
                if is_current {
                    ui.label(RichText::new("Current").small().color(theme::BLUE));
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(&card.name).size(16.0).strong()).truncate());
                });
            });
        });
        ui.add(egui::Label::new(RichText::new(card.path.display().to_string()).small().weak()).truncate())
            .on_hover_text(card.path.display().to_string());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if card.exists {
                let branch = card.branch.clone().unwrap_or_default();
                ui.label(RichText::new(format!("{BRANCH_ICON} {branch}")).small());
                let host = card.remote.as_deref().map(remote_host).unwrap_or_else(|| "local only".into());
                ui.add(egui::Label::new(RichText::new(host).small().weak()).truncate());
            } else {
                ui.label(RichText::new("Folder not found").small().color(theme::RED));
            }
        });
        if let Some(st) = &card.identity {
            identity_line(ui, st, true);
        }
        ui.add_space(6.0);
        let label = if is_current { "Go to changes" } else { "Open" };
        if theme::primary_wide(ui, card.exists, label, 30.0).clicked() {
            actions.push(if is_current { Action::GoTo(Page::Changes) } else { Action::OpenPath(card.path.clone()) });
        }
    }
}

/// "👤 Name <email>" with where it comes from; orange with a warning when it
/// doesn't match the folder rule.
fn identity_line(ui: &mut egui::Ui, st: &identity::Status, small: bool) {
    let source = match &st.source {
        identity::Source::Local => "this repo",
        identity::Source::Rule(_) => "folder rule",
        identity::Source::Global => "global",
        identity::Source::Missing => "not set",
    };
    let mismatch = st.mismatch();
    let text = if mismatch.is_some() {
        format!("⚠ {}  ({source})", st.effective.display())
    } else {
        format!("👤 {}  ({source})", st.effective.display())
    };
    let mut rt = RichText::new(text);
    if small {
        rt = rt.small();
    }
    rt = match (mismatch, &st.source) {
        (Some(_), _) | (None, identity::Source::Missing) => rt.color(theme::ORANGE),
        _ => rt.weak(),
    };
    let resp = ui.add(egui::Label::new(rt).truncate());
    match mismatch {
        Some(rule) => resp.on_hover_text(format!(
            "This folder's rule says {} ({}). Fix it in Settings → Identity.",
            rule.identity.display(),
            rule.folder
        )),
        None => resp.on_hover_text("Name and email recorded in commits"),
    };
}

/// `https://gitlab.com/a/b.git` -> `gitlab.com`, `git@host:a/b` -> `host`.
fn remote_host(url: &str) -> String {
    if url.starts_with('/') || url.starts_with('.') || url.contains(":\\") || url.starts_with("file://") {
        "local remote".into()
    } else if cli::is_http_url(url) || url.contains("://") {
        cli::host_of(url)
    } else {
        url.split('@').next_back().unwrap_or(url).split(':').next().unwrap_or(url).to_string()
    }
}

fn remote_buttons(ui: &mut egui::Ui, r: &RepoView, busy: bool, actions: &mut Vec<Action>) {
    let remote = r.head.has_remote;
    let on_branch = r.head.branch.is_some();
    // Laid out right-to-left: Push, Pull, Fetch.
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
    if theme::primary_enabled(ui, can_push, push_label)
        .on_hover_text("Upload your commits to the remote")
        .clicked()
    {
        actions.push(Action::Net(NetOp::Push { set_upstream }));
    }
    let pull_label = if r.head.behind > 0 { format!("⬇ Pull ({})", r.head.behind) } else { "⬇ Pull".into() };
    let can_pull = remote && !busy && r.head.upstream.is_some();
    if theme::primary_enabled(ui, can_pull, pull_label)
        .on_hover_text("Bring the remote's commits into this branch")
        .clicked()
    {
        actions.push(Action::Net(NetOp::Pull));
    }
    if theme::primary_enabled(ui, remote && !busy, "⟳ Fetch")
        .on_hover_text("Download new commits and branches from the remote")
        .clicked()
    {
        actions.push(Action::Net(NetOp::Fetch));
    }
    if !remote {
        ui.label(RichText::new("No remote").small().weak());
    }
}

/// List / Tree switch for the history page.
fn segmented(ui: &mut egui::Ui, tree: bool, actions: &mut Vec<Action>) {
    // Right-to-left layout: add "Tree" first so it ends up on the right.
    ui.spacing_mut().item_spacing.x = 2.0;
    for (label, value) in [("Tree", true), ("List", false)] {
        let selected = tree == value;
        let resp = if selected {
            theme::primary(ui, label)
        } else {
            ui.button(label)
        };
        if resp.clicked() && !selected {
            actions.push(Action::SetHistoryTree(value));
        }
    }
    ui.spacing_mut().item_spacing.x = 8.0;
}

fn about_page(ui: &mut egui::Ui) {
    page_frame(ui, |ui| {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            theme::logo(ui, 72.0);
            ui.add_space(10.0);
            ui.label(RichText::new("gitsot").size(30.0).strong());
            ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).weak());
            ui.add_space(10.0);
            ui.label("A simple, friendly Git client for any Git host.");
            ui.label(RichText::new("Built with Rust, egui and libgit2. Works on Windows and macOS.").weak());
        });
    });
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
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Change").on_hover_text("Set the identity for this repository").clicked() {
                    actions.push(Action::EditRepoIdentity);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| identity_line(ui, &r.id_status, false));
            });
        });
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
        let btn = theme::primary_wide(ui, can_commit, format!("Commit to {branch}"), 32.0)
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

fn history_sidebar(ui: &mut egui::Ui, r: &mut RepoView, tree: bool, actions: &mut Vec<Action>) {
    if r.history.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No commits yet").strong());
        });
        return;
    }
    let tree = tree && r.graph.rows.len() == r.history.len();
    let row_h = if tree { 30.0 } else { 46.0 };
    const LANE_W: f32 = 16.0;
    let lanes_shown = r.graph.width.min(24);
    let graph_w = if tree { lanes_shown as f32 * LANE_W + 10.0 } else { 0.0 };

    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, row_h, r.history.len(), |ui, range| {
        for i in range {
            let c = &r.history[i];
            let selected = r.selected_commit == Some(c.id);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
            let visuals = ui.visuals().clone();
            if selected {
                ui.painter().rect_filled(rect, 4.0, visuals.selection.bg_fill.gamma_multiply(if tree { 0.35 } else { 1.0 }));
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
            }
            let on_blue = selected && !tree;
            let text_color = if on_blue { Color32::WHITE } else { visuals.strong_text_color() };
            let weak = if on_blue { Color32::WHITE } else { visuals.weak_text_color() };
            let painter = ui.painter().with_clip_rect(rect);

            if tree {
                paint_graph_row(&painter, rect, &r.graph.rows[i], LANE_W, lanes_shown, visuals.panel_fill);
                let mut x = rect.left() + graph_w;
                let y = rect.center().y;
                if let Some(labels) = r.labels.get(&c.id) {
                    let color = theme::lane_color(r.graph.rows[i].color);
                    for l in labels.iter().take(4) {
                        x = paint_ref_pill(&painter, Pos2::new(x, y), l, color, text_color) + 6.0;
                    }
                }
                let galley = painter.layout_no_wrap(c.summary.clone(), FontId::proportional(13.5), text_color);
                let w = galley.size().x;
                painter.galley(Pos2::new(x, y - galley.size().y / 2.0), galley, text_color);
                painter.text(
                    Pos2::new(x + w + 10.0, y),
                    Align2::LEFT_CENTER,
                    format!("{} · {}", c.author, relative_time(c.time)),
                    FontId::proportional(12.0),
                    weak,
                );
            } else {
                let painter = painter.with_clip_rect(rect.shrink(2.0));
                painter.text(
                    rect.left_top() + Vec2::new(8.0, 6.0),
                    Align2::LEFT_TOP,
                    &c.summary,
                    FontId::proportional(14.0),
                    text_color,
                );
                let mut meta = format!("{} · {}", c.author, relative_time(c.time));
                if let Some(labels) = r.labels.get(&c.id) {
                    let names: Vec<&str> = labels.iter().filter(|l| l.kind != RefKind::Remote).map(|l| l.name.as_str()).collect();
                    if !names.is_empty() {
                        meta = format!("{}  ·  {meta}", names.join(", "));
                    }
                }
                painter.text(rect.left_top() + Vec2::new(8.0, 26.0), Align2::LEFT_TOP, meta, FontId::proportional(12.0), weak);
            }

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

/// Paint one row of the branch graph: straight lanes, curves into and out of the dot, the dot.
fn paint_graph_row(painter: &egui::Painter, rect: egui::Rect, row: &graph::GraphRow, lane_w: f32, max_lanes: usize, bg: Color32) {
    let x = |col: usize| rect.left() + 8.0 + col.min(max_lanes.saturating_sub(1)) as f32 * lane_w;
    let (top, mid, bottom) = (rect.top(), rect.center().y, rect.bottom());
    let width = 2.0;
    let curve = |a: Pos2, b: Pos2, c: Pos2, d: Pos2, color: Color32| {
        painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
            [a, b, c, d],
            false,
            Color32::TRANSPARENT,
            Stroke::new(width, color),
        ));
    };
    for s in &row.pass {
        painter.line_segment([Pos2::new(x(s.from_col), top), Pos2::new(x(s.from_col), bottom)], Stroke::new(width, theme::lane_color(s.color)));
    }
    let node = Pos2::new(x(row.col), mid);
    for s in &row.incoming {
        let color = theme::lane_color(s.color);
        let from = Pos2::new(x(s.from_col), top);
        if s.from_col == row.col {
            painter.line_segment([from, node], Stroke::new(width, color));
        } else {
            curve(from, Pos2::new(from.x, mid), Pos2::new(from.x, mid), node, color);
        }
    }
    for s in &row.outgoing {
        let color = theme::lane_color(s.color);
        let to = Pos2::new(x(s.to_col), bottom);
        if s.to_col == row.col {
            painter.line_segment([node, to], Stroke::new(width, color));
        } else {
            curve(node, Pos2::new(to.x, mid), Pos2::new(to.x, mid), to, color);
        }
    }
    let color = theme::lane_color(row.color);
    painter.circle(node, 5.0, color, Stroke::new(1.5, bg));
}

/// Draw a branch/tag label like `⎇ main` or `🌐 origin/main`; returns the right edge.
fn paint_ref_pill(painter: &egui::Painter, left_center: Pos2, label: &RefLabel, color: Color32, text_color: Color32) -> f32 {
    let icon: &str = match label.kind {
        RefKind::Local => BRANCH_ICON,
        RefKind::Remote => "🌐",
        RefKind::Tag => "🏷",
    };
    let text = if label.is_head { format!("✔ {icon} {}", label.name) } else { format!("{icon} {}", label.name) };
    let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), text_color);
    let size = galley.size() + Vec2::new(12.0, 6.0);
    let rect = egui::Rect::from_min_size(Pos2::new(left_center.x, left_center.y - size.y / 2.0), size);
    let fill = if label.kind == RefKind::Remote { Color32::TRANSPARENT } else { color.gamma_multiply(0.18) };
    painter.rect(rect, 4.0, fill, Stroke::new(1.0, color), egui::StrokeKind::Inside);
    painter.galley(rect.min + Vec2::new(6.0, 3.0), galley, text_color);
    rect.right()
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
