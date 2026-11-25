use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, FontId, Key, KeyboardShortcut, Layout, Modifiers, Pos2, RichText,
    Sense, Stroke, TextEdit, Vec2,
};
use git2::{Oid, Repository};
use serde::{Deserialize, Serialize};

use gitsot_core::cli::{self, CheckResult, Credentials, NetOp, NetResult};
use gitsot_core::forge::{self, Filter, ReviewEvent};
use gitsot_core::git::{
    self, BranchInfo, ChangeKind, CommitFile, CommitInfo, FileChange, FileDiff, HeadInfo, LineKind, RefKind,
    RefLabel,
};
use gitsot_core::graph::{self, Graph};
use gitsot_core::identity::{self, FolderRule, Identity};
use gitsot_core::term::Terminal;
use crate::pulls::{self, Load, PullTab, Pulls};
use crate::{term_view, theme};
use gitsot_core::shell;
use std::collections::HashMap;

const SETTINGS_KEY: &str = "gitsot-settings";
const MAX_RECENT: usize = 12;
/// Git logo glyph from egui's bundled icon font, used as the "branch" icon.
const BRANCH_ICON: &str = "\u{E625}";
const TERMINAL_SHORTCUT: &str = "Ctrl+`";
/// How often the remote is checked for new commits in the background.
const CHECK_INTERVAL: Duration = Duration::from_secs(180);
/// Id of the pinned first tab.
const HOME: u64 = 0;

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
    /// Groups created with "New group" (they stay even when empty).
    group_names: Vec<String>,
    /// Groups folded shut on the Repositories page.
    collapsed_groups: Vec<String>,
    /// Load Gitsot's prompt (labelled chips) into the built-in terminal.
    terminal_prompt: bool,
    terminal_font_size: f32,
    /// Hosts the user marked as GitHub Enterprise, for pull requests.
    github_hosts: Vec<String>,
    /// Fetch from the remote every few minutes, so the commit counts stay current.
    background_check: bool,
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
            group_names: Vec::new(),
            collapsed_groups: Vec::new(),
            terminal_prompt: true,
            terminal_font_size: 14.0,
            github_hosts: Vec::new(),
            background_check: true,
        }
    }
}

impl Settings {
    /// Every group name, A–Z.
    fn all_groups(&self) -> Vec<String> {
        let mut all: Vec<String> = self.group_names.iter().chain(self.groups.values()).cloned().collect();
        all.sort_by_key(|g| g.to_lowercase());
        all.dedup();
        all
    }

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
    /// Goes up on every reload, so open diff tabs know when to reload too.
    generation: u64,
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

    /// When new commits were last fetched from the remote without an error
    /// (seconds since the Unix epoch).
    last_updated: Option<i64>,
    /// Why the last background check failed, until one works again.
    check_error: Option<String>,
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
            generation: 0,
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
            last_updated: None,
            check_error: None,
        };
        view.refresh()?;
        // Git rewrites FETCH_HEAD on every fetch, so its age is a fair first
        // guess until the app's own first check has run.
        view.last_updated = git::last_fetch_time(&view.repo);
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

        self.generation += 1;

        if self.history_loaded {
            self.load_history(self.history_all);
        }
        Ok(())
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

/// Pages that open in one closable tab each.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    Repositories,
    Settings,
    About,
    /// The list of pull requests of the open repository.
    Pulls,
}

/// Where the diff shown in a tab comes from.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum DiffSource {
    /// Uncommitted changes of a file. Reloaded when the repository changes.
    Working,
    Commit(Oid),
}

struct DiffTab {
    source: DiffSource,
    path: String,
    diff: Result<FileDiff, String>,
}

enum TabKind {
    /// The pinned first tab: changes, history and commit details of the open
    /// repository, or the repository list when none is open.
    Home,
    Page(Page),
    Diff(DiffTab),
    /// One shell session. It keeps running while other tabs are shown.
    Terminal { number: usize, terminal: Box<Terminal> },
    Pull(Box<PullTab>),
}

struct Tab {
    id: u64,
    kind: TabKind,
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

/// Ask for a comment, then send a review of pull request `number`.
struct ReviewForm {
    number: u64,
    event: ReviewEvent,
    body: String,
}

/// One remote being edited in the Remotes dialog.
struct RemoteRow {
    name: String,
    /// What Git has now, to tell whether the fields were edited.
    saved: git::RemoteInfo,
    url: String,
    /// Only shown when the remote has a separate push URL.
    push: String,
}

impl RemoteRow {
    fn edited(&self) -> bool {
        self.url.trim() != self.saved.url || self.push.trim() != self.saved.push_url.as_deref().unwrap_or("")
    }
}

fn remote_rows(repo: &Repository) -> Vec<RemoteRow> {
    git::remotes(repo)
        .into_iter()
        .map(|info| RemoteRow { name: info.name.clone(), url: info.url.clone(), push: info.push_url.clone().unwrap_or_default(), saved: info })
        .collect()
}

enum Dialog {
    /// See, change, add and remove the remotes of the open repository.
    Remotes { rows: Vec<RemoteRow>, new_name: String, new_url: String, confirm_remove: Option<String> },
    Review(ReviewForm),
    /// Access token for pull requests, kept for this session only.
    Token { token: String },
    Clone { url: String, parent: String, name: String, name_edited: bool },
    NewBranch { name: String },
    Identity { name: String, email: String, commit_after: bool },
    ConfirmDiscard { changes: Vec<FileChange> },
    /// `remote` is set for a branch on a remote (`origin`, `feat/x`).
    ConfirmDeleteBranch { name: String, remote: Option<String>, unmerged: bool },
    /// Pick a branch to merge into the current one.
    Merge {
        filter: String,
        selected: Option<String>,
        /// What merging the selected branch would do, and the branch it was worked out for.
        preview: Option<(String, Result<git::MergePreview, String>)>,
    },
    Message { title: String, text: String },
    SignIn { op: NetOp, host: String, username: String, password: String, retry: bool },
    /// About to commit or push with an identity that doesn't match the folder rule.
    IdentityWarning { push: Option<NetOp>, rule: FolderRule, effective: Identity, bad_commits: Vec<(String, String)> },
    SetGroup { path: PathBuf, name: String },
    /// Create a group, or rename `rename` when set.
    GroupName { name: String, rename: Option<String> },
}

/// Things the user asked for while drawing the UI; applied afterwards so we
/// never mutate state while it is borrowed by a widget.
pub(crate) enum Action {
    OpenDialog,
    OpenPath(PathBuf),
    /// Start a new terminal session in a repository (opening it first if
    /// needed) and show it in a new tab.
    OpenTerminal(PathBuf),
    /// A new terminal tab for the open repository.
    NewTerminal,
    RestartTerminal,
    Activate(u64),
    CloseTab(u64),
    /// Show one of the single pages in its tab, opening the tab if needed.
    Show(Page),
    /// Open a file's diff in a tab (or go to the tab that already shows it).
    OpenDiff(DiffSource, String),
    ReloadPulls,
    SetPullFilter(Filter),
    OpenPull { number: u64, title: String },
    ReloadPull(u64),
    AskReview(u64, ReviewEvent),
    SubmitReview { number: u64, event: ReviewEvent, body: String },
    AskToken,
    SetToken(Option<String>),
    TreatAsGitHub(String),
    ForgetGitHubHost(String),
    OpenUrl(String),
    SetTerminalPrompt(bool),
    SetTerminalFontSize(f32),
    InitDialog,
    ShowClone,
    CloseRepo,
    Refresh,
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
    AskDeleteRemoteBranch(String),
    ShowMerge(Option<String>),
    DeleteBranch(String),
    ShowRemotes,
    /// Change the URL (and the separate push URL, when there is one) of a remote.
    SaveRemote { name: String, url: String, push: Option<String> },
    AddRemote { name: String, url: String },
    RemoveRemote(String),
    Net(NetOp),
    /// Look for new commits on the remote now (the small button in the status bar).
    CheckNow,
    SetBackgroundCheck(bool),
    NetWithCredentials(NetOp, Credentials),
    StartClone { url: String, parent: String, name: String },
    SelectCommit(Oid),
    SelectCommitFile(String),
    RevealInFiles,
    CopyToClipboard(String),
    SetLightMode(bool),
    ForgetRecent(PathBuf),
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
    AskGroupName(Option<String>),
    ToggleGroup(String),
    SaveGroupName { name: String, rename: Option<String> },
    DeleteGroup(String),
}

struct Toast {
    text: String,
    error: bool,
    at: Instant,
}

pub struct GitsotApp {
    settings: Settings,
    repo: Option<RepoView>,
    /// Open tabs, the pinned Home tab first.
    tabs: Vec<Tab>,
    /// Id of the tab on show.
    active: u64,
    /// The tab shown before it, to go back to when the active tab closes.
    previous: u64,
    next_tab_id: u64,
    /// `RepoView::generation` the diff tabs were last loaded for.
    diffs_generation: u64,
    /// Pull requests of the open repository.
    pulls: Pulls,
    forge_tx: Sender<pulls::Reply>,
    forge_rx: Receiver<pulls::Reply>,
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
    check_tx: Sender<CheckResult>,
    check_rx: Receiver<CheckResult>,
    /// The repository a background check is running in.
    checking: Option<PathBuf>,
    /// When the next background check is due.
    next_check: Instant,
    /// An operation the user started during a background check. It runs as
    /// soon as the check ends, so two git commands never fight over the repository.
    queued: Option<(NetOp, PathBuf, Option<Credentials>)>,
    git_cli: bool,
    /// The `curl` command is installed (used for pull requests).
    curl_cli: bool,
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
        let (forge_tx, forge_rx) = channel();
        let (check_tx, check_rx) = channel();
        let mut app = Self {
            settings,
            repo: None,
            tabs: vec![Tab { id: HOME, kind: TabKind::Home }],
            active: HOME,
            previous: HOME,
            next_tab_id: HOME + 1,
            diffs_generation: 0,
            pulls: Pulls::default(),
            forge_tx,
            forge_rx,
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
            check_tx,
            check_rx,
            checking: None,
            next_check: Instant::now(),
            queued: None,
            git_cli: cli::git_available(),
            curl_cli: forge::curl_available(),
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
            Ok(mut view) => {
                self.settings.remember(&view.path);
                self.info(format!("Opened {}", view.name()));
                view.load_history(self.settings.history_view_tree);
                self.close_repo_tabs(false);
                self.pulls = Pulls::new(view.remote_url.as_deref(), &self.settings.github_hosts);
                self.repo = Some(view);
                self.active = HOME;
                self.next_check = Instant::now();
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
            Action::OpenTerminal(p) => {
                if self.repo.as_ref().is_none_or(|r| r.path != p) {
                    self.open_repo(&p);
                }
                if self.repo.as_ref().is_some_and(|r| r.path == p) {
                    self.new_terminal(ctx, &p);
                }
            }
            Action::NewTerminal => {
                if let Some(path) = self.repo.as_ref().map(|r| r.path.clone()) {
                    self.new_terminal(ctx, &path);
                }
            }
            Action::RestartTerminal => {
                // A fresh shell in the same tab and folder.
                let active = self.active;
                let prompt = self.settings.terminal_prompt;
                let mut failed = None;
                if let Some(TabKind::Terminal { terminal, .. }) = self.tabs.iter_mut().find(|t| t.id == active).map(|t| &mut t.kind) {
                    let path = terminal.repo.clone();
                    let wake = ctx.clone();
                    match Terminal::open(&path, shell::launch(&path, prompt), move || wake.request_repaint()) {
                        Ok(new) => **terminal = new,
                        Err(e) => failed = Some(format!("Could not start the terminal: {e:#}")),
                    }
                }
                if let Some(message) = failed {
                    self.error(message);
                }
            }
            Action::Activate(id) => self.activate(id),
            Action::CloseTab(id) => self.close_tab(id),
            Action::Show(page) => {
                match page {
                    Page::Repositories => self.reload_cards(),
                    Page::Settings => self.reload_identity(),
                    Page::Pulls if matches!(self.pulls.list, Load::Idle) => self.start_forge(ctx, pulls::Request::List(self.pulls.filter)),
                    _ => {}
                }
                // With no repository open, the Home tab is the repository list.
                let id = if page == Page::Repositories && self.repo.is_none() {
                    HOME
                } else {
                    match self.tabs.iter().find(|t| matches!(t.kind, TabKind::Page(p) if p == page)) {
                        Some(tab) => tab.id,
                        None => self.add_tab(TabKind::Page(page)),
                    }
                };
                self.activate(id);
            }
            Action::OpenDiff(source, path) => {
                let Some(r) = &self.repo else { return };
                let existing = self.tabs.iter().find(|t| matches!(&t.kind, TabKind::Diff(d) if d.source == source && d.path == path)).map(|t| t.id);
                let id = match existing {
                    Some(id) => id,
                    None => {
                        let diff = load_diff(r, &source, &path);
                        self.add_tab(TabKind::Diff(DiffTab { source, path, diff }))
                    }
                };
                self.activate(id);
            }
            Action::ReloadPulls => self.start_forge(ctx, pulls::Request::List(self.pulls.filter)),
            Action::SetPullFilter(filter) => {
                self.pulls.filter = filter;
                self.start_forge(ctx, pulls::Request::List(filter));
            }
            Action::OpenPull { number, title } => {
                let existing = self.tabs.iter().find(|t| matches!(&t.kind, TabKind::Pull(p) if p.number == number)).map(|t| t.id);
                let id = match existing {
                    Some(id) => id,
                    None => {
                        let id = self.add_tab(TabKind::Pull(Box::new(PullTab::new(number, title))));
                        self.start_forge(ctx, pulls::Request::Detail(number));
                        id
                    }
                };
                self.activate(id);
            }
            Action::ReloadPull(number) => self.start_forge(ctx, pulls::Request::Detail(number)),
            Action::AskReview(number, event) => {
                self.dialog = Some(Dialog::Review(ReviewForm { number, event, body: String::new() }));
            }
            Action::SubmitReview { number, event, body } => {
                self.dialog = None;
                self.start_forge(ctx, pulls::Request::Review { number, event, body });
            }
            Action::AskToken => self.dialog = Some(Dialog::Token { token: String::new() }),
            Action::SetToken(token) => {
                self.dialog = None;
                self.pulls.token = token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
                self.pulls.viewer = None;
                self.reload_all_pulls(ctx);
            }
            Action::TreatAsGitHub(host) => {
                if !self.settings.github_hosts.contains(&host) {
                    self.settings.github_hosts.push(host);
                }
                let url = self.repo.as_ref().and_then(|r| r.remote_url.clone());
                self.pulls = Pulls::new(url.as_deref(), &self.settings.github_hosts);
                self.reload_all_pulls(ctx);
            }
            Action::ForgetGitHubHost(host) => {
                self.settings.github_hosts.retain(|h| *h != host);
                let url = self.repo.as_ref().and_then(|r| r.remote_url.clone());
                self.pulls = Pulls::new(url.as_deref(), &self.settings.github_hosts);
            }
            Action::OpenUrl(url) => open_url(&url),
            Action::SetTerminalPrompt(on) => self.settings.terminal_prompt = on,
            Action::SetTerminalFontSize(size) => self.settings.terminal_font_size = size,
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
                // Closing a repository also ends its terminals.
                self.close_repo_tabs(true);
                self.repo = None;
                self.pulls = Pulls::default();
                self.settings.last_repo = None;
                self.active = HOME;
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
            Action::CheckNow => self.start_check(ctx),
            Action::SetBackgroundCheck(on) => {
                self.settings.background_check = on;
                self.next_check = Instant::now();
            }
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
                self.id_tab = IdTab::Repo;
                self.apply(ctx, Action::Show(Page::Settings));
            }
            action @ (Action::ShowRemotes | Action::SaveRemote { .. } | Action::AddRemote { .. } | Action::RemoveRemote(_)) => {
                self.edit_remotes(action);
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
            Action::ToggleGroup(name) => {
                if let Some(i) = self.settings.collapsed_groups.iter().position(|g| *g == name) {
                    self.settings.collapsed_groups.remove(i);
                } else {
                    self.settings.collapsed_groups.push(name);
                }
            }
            Action::AskGroupName(rename) => {
                let name = rename.clone().unwrap_or_default();
                self.dialog = Some(Dialog::GroupName { name, rename });
            }
            Action::SaveGroupName { name, rename } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                self.dialog = None;
                if let Some(old) = rename {
                    for g in self.settings.groups.values_mut().filter(|g| **g == old) {
                        *g = name.clone();
                    }
                    self.settings.group_names.retain(|g| *g != old);
                }
                if !self.settings.group_names.contains(&name) {
                    self.settings.group_names.push(name);
                }
            }
            Action::DeleteGroup(name) => {
                self.settings.groups.retain(|_, g| *g != name);
                self.settings.group_names.retain(|g| *g != name);
                self.info(format!("Deleted group {name}. Its repositories are now ungrouped."));
            }
            other => {
                let Some(r) = self.repo.as_mut() else { return };
                let result: anyhow::Result<Option<String>> = (|| {
                    match other {
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
                        Action::ShowMerge(selected) => {
                            self.dialog = Some(Dialog::Merge { filter: String::new(), selected, preview: None });
                            return Ok(None);
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
                            let unmerged = !git::branch_merged(&r.repo, &name);
                            self.dialog = Some(Dialog::ConfirmDeleteBranch { name, remote: None, unmerged });
                            return Ok(None);
                        }
                        Action::AskDeleteRemoteBranch(full) => {
                            let Some((remote, branch)) = full.split_once('/') else { return Ok(None) };
                            self.dialog = Some(Dialog::ConfirmDeleteBranch {
                                name: branch.to_string(),
                                remote: Some(remote.to_string()),
                                unmerged: false,
                            });
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
        if self.checking.is_some() && !matches!(op, NetOp::Clone { .. }) {
            self.queued = Some((op, repo_path, creds));
            return;
        }
        let ctx = ctx.clone();
        cli::spawn(op, repo_path, creds, self.tx.clone(), move || ctx.request_repaint());
    }

    /// Look for new commits on the remote without getting in the user's way.
    ///
    /// The check runs git inside the open repository, with that repository's
    /// own sign-in, and never with a password typed into Gitsot. See
    /// [`cli::spawn_check`].
    fn start_check(&mut self, ctx: &egui::Context) {
        self.next_check = Instant::now() + CHECK_INTERVAL;
        let Some(r) = &self.repo else { return };
        if self.busy.is_some() || self.checking.is_some() || !self.git_cli || !r.head.has_remote {
            return;
        }
        self.checking = Some(r.path.clone());
        let ctx = ctx.clone();
        cli::spawn_check(r.path.clone(), self.check_tx.clone(), move || ctx.request_repaint());
    }

    /// Start the background check when it is due, and take in its result.
    fn poll_check(&mut self, ctx: &egui::Context) {
        while let Ok(res) = self.check_rx.try_recv() {
            self.checking = None;
            let queued = self.queued.take();
            if let Some(r) = self.repo.as_mut().filter(|r| r.path == res.repo) {
                r.check_error = match res.result {
                    Ok(()) => {
                        r.last_updated = Some(chrono::Utc::now().timestamp());
                        None
                    }
                    Err(e) if res.auth_failed => {
                        Some(format!("No saved sign-in worked for this remote. Use Fetch to sign in.\n\n{e}"))
                    }
                    Err(e) => Some(e),
                };
                // A queued operation reads the repository again when it ends.
                if queued.is_none() {
                    self.refresh();
                }
            }
            if let Some((op, repo, creds)) = queued {
                let ctx = ctx.clone();
                cli::spawn(op, repo, creds, self.tx.clone(), move || ctx.request_repaint());
            }
        }
        let has_remote = self.repo.as_ref().is_some_and(|r| r.head.has_remote);
        if !self.settings.background_check || !has_remote {
            return;
        }
        let now = Instant::now();
        if now >= self.next_check {
            if self.busy.is_some() || self.checking.is_some() {
                // Something is running: look again once it is done.
                self.next_check = now + Duration::from_secs(5);
            } else {
                self.start_check(ctx);
            }
        }
        ctx.request_repaint_after(self.next_check.saturating_duration_since(now) + Duration::from_millis(50));
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
                (NetOp::Merge { branch }, Ok(_)) => {
                    let branch = branch.clone();
                    self.refresh();
                    let head = self.repo.as_ref().map(|r| r.head.display_name()).unwrap_or_default();
                    self.info(format!("Merged {branch} into {head}"));
                }
                (op, Ok(_)) => {
                    let label = match op {
                        NetOp::Fetch => "Fetched from remote",
                        NetOp::Pull => "Pulled latest changes",
                        NetOp::Push { .. } => "Pushed to remote",
                        NetOp::DeleteRemoteBranch { .. } => "Deleted the remote branch",
                        NetOp::Merge { .. } => unreachable!(),
                        NetOp::MergeAbort => "Merge aborted. Your branch is back to how it was.",
                        NetOp::Clone { .. } => unreachable!(),
                    };
                    let fetched = matches!(op, NetOp::Fetch | NetOp::Pull);
                    let same_repo = self.repo.as_ref().map(|r| r.path == res.repo).unwrap_or(false);
                    if same_repo {
                        if let (true, Some(r)) = (fetched, self.repo.as_mut()) {
                            r.last_updated = Some(chrono::Utc::now().timestamp());
                            r.check_error = None;
                        }
                        self.refresh();
                    }
                    match res.updates.filter(|_| same_repo) {
                        Some(u) if !u.is_clean() => {
                            self.dialog = Some(Dialog::Message {
                                title: "Some branches were not updated".into(),
                                text: branch_updates_warning(&u),
                            });
                        }
                        Some(u) if !u.updated.is_empty() => {
                            self.info(format!("Fetched and updated {}", updated_list(&u.updated)));
                        }
                        Some(_) => self.info("Fetched from remote. Every branch is up to date."),
                        None => self.info(label),
                    }
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
                (NetOp::Merge { branch }, Err(e)) if self.repo.as_ref().is_some_and(|r| r.repo.state() == git2::RepositoryState::Merge) => {
                    let branch = branch.clone();
                    self.refresh();
                    self.active = HOME;
                    self.dialog = Some(Dialog::Message {
                        title: "The merge has conflicts".into(),
                        text: format!(
                            "Some files were changed on both sides, so {branch} could not be merged automatically.\n\n\
                             They are marked ! in Changes. Open each one in your editor, choose what to keep \
                             (remove the <<<<<<< ======= >>>>>>> lines), tick it, then commit to finish the merge.\n\n\
                             To give up instead, use Abort merge at the top of Changes.\n\n{e}"
                        ),
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

    /// Open the Remotes dialog, or apply one change made in it.
    ///
    /// When the default remote ends up pointing somewhere else, everything
    /// that belonged to the old address is dropped: open pull request tabs
    /// and the pull request sign-in (it may be for another account or host).
    fn edit_remotes(&mut self, action: Action) {
        if self.busy.is_some() {
            return;
        }
        let Some(r) = self.repo.as_mut() else { return };
        let before = r.remote_url.clone();
        let result: anyhow::Result<Option<String>> = match &action {
            Action::SaveRemote { name, url, push } => {
                git::set_remote_url(&r.repo, name, url, push.as_deref()).map(|()| Some(format!("{name} now points to {}", url.trim())))
            }
            Action::AddRemote { name, url } => git::add_remote(&r.repo, name, url).map(|()| Some(format!("Added the remote {}", name.trim()))),
            Action::RemoveRemote(name) => git::remove_remote(&r.repo, name).map(|()| Some(format!("Removed the remote {name}"))),
            _ => Ok(None),
        };
        let refreshed = r.refresh();
        let rows = remote_rows(&r.repo);
        let after = r.remote_url.clone();
        // Keep what was typed for a new remote when adding it failed.
        let (new_name, new_url) = match (&action, &result) {
            (Action::AddRemote { name, url }, Err(_)) => (name.clone(), url.clone()),
            _ if rows.is_empty() => ("origin".to_string(), String::new()),
            _ => (String::new(), String::new()),
        };
        self.dialog = Some(Dialog::Remotes { rows, new_name, new_url, confirm_remove: None });
        if before != after {
            self.tabs.retain(|t| !matches!(t.kind, TabKind::Page(Page::Pulls) | TabKind::Pull(_)));
            if !self.tabs.iter().any(|t| t.id == self.active) {
                self.active = HOME;
            }
            self.pulls = Pulls::new(after.as_deref(), &self.settings.github_hosts);
            self.reload_cards();
        }
        match result.and_then(|msg| refreshed.map(|()| msg)) {
            Ok(Some(msg)) => self.info(msg),
            Ok(None) => {}
            Err(e) => self.error(format!("{e:#}")),
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
        let in_terminal = matches!(self.active_tab().kind, TabKind::Terminal { .. });
        // Ctrl+` goes to this repository's terminal (starting one if there is
        // none) and back to the first tab.
        if self.repo.is_some() && self.dialog.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Backtick)) {
            let path = self.repo.as_ref().map(|r| r.path.clone());
            let last = self.tabs.iter().rev().find(|t| matches!(&t.kind, TabKind::Terminal { terminal, .. } if Some(&terminal.repo) == path.as_ref()));
            actions.push(match (in_terminal, last) {
                (true, _) => Action::Activate(HOME),
                (false, Some(tab)) => Action::Activate(tab.id),
                (false, None) => Action::NewTerminal,
            });
        }
        // In the terminal every other key belongs to the shell (Ctrl+R
        // searches its history, Ctrl+W deletes a word).
        if !in_terminal {
            if self.active != HOME && self.dialog.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::W)) {
                actions.push(Action::CloseTab(self.active));
            }
            let refresh = ctx.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::F5) || i.consume_key(Modifiers::COMMAND, Key::R)
            });
            if refresh {
                actions.push(Action::Refresh);
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
                actions.push(Action::OpenDialog);
            }
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

/// The pinned tab of an open repository: changes, history and the selected
/// commit side by side, so there is no switching between them.
fn workspace(ui: &mut egui::Ui, r: &mut RepoView, history_tree: bool, open_diffs: &[String], actions: &mut Vec<Action>) {
    let column = |left: i8, right: i8| egui::Frame::new().inner_margin(egui::Margin { left, right, top: 8, bottom: 8 });
    egui::Panel::left("workspace_changes")
        .resizable(true)
        .default_size(310.0)
        .min_size(230.0)
        .frame(column(12, 8))
        .show(ui, |ui| changes_column(ui, r, open_diffs, actions));
    egui::Panel::right("workspace_commit")
        .resizable(true)
        .default_size(460.0)
        .min_size(260.0)
        .frame(column(8, 10))
        .show(ui, |ui| commit_column(ui, r, actions));
    egui::CentralPanel::default().frame(column(8, 8)).show(ui, |ui| {
        // The heading, with the List / Tree switch on its right.
        ui.horizontal(|ui| {
            theme::column_header(ui, &format!("History · {}", if history_tree { "all branches".to_string() } else { r.head.display_name() }));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| segmented(ui, history_tree, actions));
        });
        ui.add_space(4.0);
        history_column(ui, r, history_tree, actions);
    });
}

/// A file's diff in its own tab.
fn diff_page(ui: &mut egui::Ui, d: &DiffTab, r: &RepoView) {
    egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(egui::Margin::same(10))).show(ui, |ui| {
        ui.horizontal(|ui| {
            match &d.source {
                DiffSource::Working => {
                    if let Some(change) = r.changes.iter().find(|c| c.path == d.path) {
                        kind_badge(ui, change.kind);
                    }
                    ui.label(RichText::new(&d.path).strong().monospace());
                    ui.label(RichText::new("uncommitted changes").weak());
                }
                DiffSource::Commit(id) => {
                    ui.label(RichText::new(&d.path).strong().monospace());
                    let summary = r.history.iter().find(|c| c.id == *id).map(|c| c.summary.as_str()).unwrap_or_default();
                    ui.add(egui::Label::new(RichText::new(format!("in {}  {summary}", git::short(*id))).weak()).truncate());
                }
            }
        });
        ui.separator();
        match &d.diff {
            Ok(diff) => diff_view(ui, diff, &format!("tab_diff/{}", d.path)),
            Err(e) => {
                ui.label(RichText::new(e).color(theme::RED));
            }
        }
    });
}

fn load_diff(r: &RepoView, source: &DiffSource, path: &str) -> Result<FileDiff, String> {
    match source {
        DiffSource::Working => git::working_diff(&r.repo, path),
        DiffSource::Commit(id) => git::commit_file_diff(&r.repo, *id, path),
    }
    .map_err(|e| format!("{e:#}"))
}

fn close_tab_button(ui: &mut egui::Ui, id: u64, actions: &mut Vec<Action>) {
    if theme::ribbon_button(ui, "✖", "Close tab", true, false).on_hover_text("Close this tab (Ctrl+W)").clicked() {
        actions.push(Action::CloseTab(id));
    }
}

/// Shorten long names for buttons and tabs.
pub(crate) fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// Open a web page in the default browser. Only `http(s)` addresses.
fn open_url(url: &str) {
    if !cli::is_http_url(url) {
        return;
    }
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut cmd = std::process::Command::new("rundll32");
        cmd.arg("url.dll,FileProtocolHandler");
        cmd
    };
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    let _ = cmd.arg(url).spawn();
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

pub(crate) fn relative_time(secs: i64) -> String {
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

/// `1 commit`, `21 commits`.
/// `main (2 commits), feature (1 commit)`
fn updated_list(updated: &[(String, usize)]) -> String {
    updated.iter().map(|(name, n)| format!("{name} ({})", commits(*n))).collect::<Vec<_>>().join(", ")
}

/// The text shown after a Fetch that had to leave some branches alone.
fn branch_updates_warning(u: &cli::BranchUpdates) -> String {
    let mut parts = Vec::new();
    if !u.unpushed.is_empty() {
        let mut text = String::from("These branches have commits that are not pushed yet, so Fetch left them alone:\n");
        for (name, ahead, behind) in &u.unpushed {
            text.push_str(&format!("\n  •  {name}: {} not pushed, {} waiting on the remote", commits(*ahead), commits(*behind)));
        }
        text.push_str("\n\nSwitch to the branch and use Pull to merge the remote's commits into it, then Push.");
        parts.push(text);
    }
    if !u.failed.is_empty() {
        let mut text = String::from("These branches could not be updated:\n");
        for (name, reason) in &u.failed {
            let reason = cli::friendly_error(reason).replace('\n', "\n      ");
            text.push_str(&format!("\n  •  {name}: {reason}"));
        }
        parts.push(text);
    }
    if !u.updated.is_empty() {
        parts.push(format!("Updated: {}", updated_list(&u.updated)));
    }
    parts.join("\n\n")
}

fn commits(n: usize) -> String {
    if n == 1 { "1 commit".to_string() } else { format!("{n} commits") }
}

fn plural(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

pub(crate) fn full_time(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%a %d %b %Y, %H:%M:%S").to_string())
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

pub(crate) fn kind_badge(ui: &mut egui::Ui, kind: ChangeKind) {
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

pub(crate) fn file_row(ui: &mut egui::Ui, selected: bool, path: &str) -> egui::Response {
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

        self.poll_check(&ctx);
        self.poll_forge(&ctx);
        self.poll_terminals(&mut actions);
        self.sync_diff_tabs();

        self.ribbon(ui, &mut actions);
        self.status_bar(ui, &mut actions);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::same(0)))
            .show(ui, |ui| self.tab_content(ui, &mut actions));

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
    // -----------------------------------------------------------------------
    // Tabs
    // -----------------------------------------------------------------------

    fn active_tab(&self) -> &Tab {
        self.tabs.iter().find(|t| t.id == self.active).unwrap_or(&self.tabs[0])
    }

    fn add_tab(&mut self, kind: TabKind) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab { id, kind });
        id
    }

    fn activate(&mut self, id: u64) {
        if id == self.active || !self.tabs.iter().any(|t| t.id == id) {
            return;
        }
        // Commands typed in a terminal may have changed anything.
        if matches!(self.active_tab().kind, TabKind::Terminal { .. }) {
            self.refresh();
        }
        self.previous = self.active;
        self.active = id;
        if id == HOME && self.repo.is_none() {
            self.reload_cards();
        }
    }

    /// Close a tab (never the pinned one). When it was on show, go back to
    /// the tab shown before it, or to its left neighbour.
    fn close_tab(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id).filter(|_| id != HOME) else { return };
        self.tabs.remove(index);
        if self.active == id {
            let back = self.previous;
            self.active = if self.tabs.iter().any(|t| t.id == back) { back } else { self.tabs[index - 1].id };
            self.previous = HOME;
        }
    }

    /// Close the tabs that belong to the open repository: diffs and pull
    /// requests, and its terminals too when `terminals` is set.
    fn close_repo_tabs(&mut self, terminals: bool) {
        let path = self.repo.as_ref().map(|r| r.path.clone());
        self.tabs.retain(|t| match &t.kind {
            TabKind::Home | TabKind::Page(Page::Repositories | Page::Settings | Page::About) => true,
            TabKind::Page(Page::Pulls) | TabKind::Diff(_) | TabKind::Pull(_) => false,
            TabKind::Terminal { terminal, .. } => !(terminals && Some(&terminal.repo) == path.as_ref()),
        });
        if !self.tabs.iter().any(|t| t.id == self.active) {
            self.active = HOME;
        }
    }

    /// Start a new shell in `path` and show it in a new tab.
    fn new_terminal(&mut self, ctx: &egui::Context, path: &Path) {
        let launch = shell::launch(path, self.settings.terminal_prompt);
        let wake = ctx.clone();
        match Terminal::open(path, launch, move || wake.request_repaint()) {
            Ok(terminal) => {
                let number = 1 + self
                    .tabs
                    .iter()
                    .filter_map(|t| match &t.kind {
                        TabKind::Terminal { number, .. } => Some(*number),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0);
                let id = self.add_tab(TabKind::Terminal { number, terminal: Box::new(terminal) });
                self.activate(id);
            }
            Err(e) => self.error(format!("Could not start the terminal: {e:#}")),
        }
    }

    /// Read what every shell reported. When a prompt came back in the open
    /// repository a command has finished, so the repository is read again.
    fn poll_terminals(&mut self, actions: &mut Vec<Action>) {
        let path = self.repo.as_ref().map(|r| r.path.clone());
        let mut refresh = false;
        for tab in &mut self.tabs {
            if let TabKind::Terminal { terminal, .. } = &mut tab.kind {
                terminal.poll();
                refresh |= terminal.take_prompt_shown() && Some(&terminal.repo) == path.as_ref();
            }
        }
        if refresh {
            actions.push(Action::Refresh);
        }
    }

    /// After the repository was read again: reload the diffs of uncommitted
    /// files shown in tabs, and close those whose file has no changes left.
    fn sync_diff_tabs(&mut self) {
        let Some(r) = &self.repo else { return };
        if r.generation == self.diffs_generation {
            return;
        }
        self.diffs_generation = r.generation;
        self.tabs.retain_mut(|tab| match &mut tab.kind {
            TabKind::Diff(d) if d.source == DiffSource::Working => {
                let changed = r.changes.iter().any(|c| c.path == d.path);
                if changed {
                    d.diff = load_diff(r, &d.source, &d.path);
                }
                changed
            }
            _ => true,
        });
        if !self.tabs.iter().any(|t| t.id == self.active) {
            self.active = HOME;
        }
    }

    // -----------------------------------------------------------------------
    // Pull requests
    // -----------------------------------------------------------------------

    fn pull_tab_mut(&mut self, number: u64) -> Option<&mut PullTab> {
        self.tabs.iter_mut().find_map(|t| match &mut t.kind {
            TabKind::Pull(p) if p.number == number => Some(&mut **p),
            _ => None,
        })
    }

    /// Ask the Git host for something in the background.
    fn start_forge(&mut self, ctx: &egui::Context, request: pulls::Request) {
        let Some(r) = &self.repo else { return };
        let (Some(remote), Some(url)) = (self.pulls.remote.clone(), r.remote_url.clone()) else { return };
        if remote.provider().is_none() {
            return;
        }
        let path = r.path.clone();
        match &request {
            pulls::Request::List(_) => self.pulls.list = Load::Loading,
            pulls::Request::Detail(number) => {
                if let Some(tab) = self.pull_tab_mut(*number) {
                    tab.loading = true;
                }
            }
            pulls::Request::Review { number, .. } => {
                if let Some(tab) = self.pull_tab_mut(*number) {
                    tab.sending = true;
                }
            }
        }
        let wake = ctx.clone();
        pulls::spawn(path, url, remote, self.pulls.token.clone(), request, self.forge_tx.clone(), move || wake.request_repaint());
    }

    /// Load the list and every open pull request again (after a sign-in change).
    fn reload_all_pulls(&mut self, ctx: &egui::Context) {
        let numbers: Vec<u64> = self
            .tabs
            .iter()
            .filter_map(|t| match &t.kind {
                TabKind::Pull(p) => Some(p.number),
                _ => None,
            })
            .collect();
        let list_open = self.tabs.iter().any(|t| matches!(t.kind, TabKind::Page(Page::Pulls)));
        if list_open || !matches!(self.pulls.list, Load::Idle) {
            self.start_forge(ctx, pulls::Request::List(self.pulls.filter));
        }
        for number in numbers {
            self.start_forge(ctx, pulls::Request::Detail(number));
        }
    }

    fn poll_forge(&mut self, ctx: &egui::Context) {
        while let Ok(reply) = self.forge_rx.try_recv() {
            // An answer for a repository that was closed in the meantime, or
            // from a remote the repository no longer points at.
            let current = self.repo.as_ref().filter(|r| r.path == reply.repo);
            if current.and_then(|r| r.remote_url.as_deref()) != Some(reply.remote_url.as_str()) {
                continue;
            }
            match (reply.request, reply.result) {
                (pulls::Request::List(filter), result) => {
                    if filter == self.pulls.filter {
                        self.pulls.list = match result {
                            Ok(pulls::Payload::List(list)) => Load::Ready(list),
                            Ok(_) => Load::Idle,
                            Err(e) => Load::Failed(e),
                        };
                    }
                }
                (pulls::Request::Detail(number), result) => {
                    if let Ok(pulls::Payload::Detail(detail)) = &result {
                        if detail.viewer.is_some() {
                            self.pulls.viewer = detail.viewer.clone();
                        }
                    }
                    let Some(tab) = self.pull_tab_mut(number) else { continue };
                    tab.loading = false;
                    match result {
                        Ok(pulls::Payload::Detail(detail)) => {
                            tab.title = detail.summary.title.clone();
                            tab.selected = tab.selected.min(detail.files.len().saturating_sub(1));
                            tab.detail = Load::Ready(*detail);
                        }
                        Ok(_) => {}
                        Err(e) => tab.detail = Load::Failed(e),
                    }
                }
                (pulls::Request::Review { number, event, .. }, result) => {
                    if let Some(tab) = self.pull_tab_mut(number) {
                        tab.sending = false;
                    }
                    match result {
                        Ok(_) => {
                            self.info(match event {
                                ReviewEvent::Approve => format!("Approved pull request #{number}"),
                                ReviewEvent::RequestChanges => format!("Requested changes on pull request #{number}"),
                                ReviewEvent::Comment => format!("Commented on pull request #{number}"),
                            });
                            self.start_forge(ctx, pulls::Request::Detail(number));
                        }
                        Err(e) => {
                            self.dialog = Some(Dialog::Message { title: "The review was not sent".into(), text: e.message });
                        }
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Ribbon: title row, commands of the active tab, tab strip
    // -----------------------------------------------------------------------

    fn ribbon(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let p = theme::pal(ui);
        egui::Panel::top("ribbon")
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(p.panel))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                self.title_row(ui, actions);
                self.command_row(ui, actions);
                self.tab_row(ui, actions);
            });
    }

    /// The way to the repository list on the left, then what is open: the
    /// repository, where it comes from and where it is on this computer.
    /// The app's name and version are on the right.
    fn title_row(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let on_list = match self.tabs.iter().find(|t| t.id == self.active).map(|t| &t.kind) {
            Some(TabKind::Home) => self.repo.is_none(),
            Some(TabKind::Page(Page::Repositories)) => true,
            _ => false,
        };
        egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 8, top: 6, bottom: 2 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if theme::repositories_button(ui, on_list).on_hover_text("All your repositories: open, clone, create and group them").clicked() {
                    actions.push(Action::Show(Page::Repositories));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (label, page) in [("About", Page::About), ("Settings", Page::Settings)] {
                        if ui.add(egui::Button::new(label).frame_when_inactive(false)).clicked() {
                            actions.push(Action::Show(page));
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small().weak());
                    ui.add_space(4.0);
                    ui.label(RichText::new("gitsot").strong());
                    ui.add_space(12.0);
                    ui.add_space(8.0);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let Some(r) = &self.repo else {
                            ui.label(RichText::new("No repository open").weak());
                            return;
                        };
                        ui.label(RichText::new(r.name()).strong());
                        let source = match &r.remote_url {
                            Some(url) => forge::Remote::parse(url).map(|remote| remote.display()).unwrap_or_else(|| url.clone()),
                            None => "local only (no remote)".to_string(),
                        };
                        let source_label = egui::Label::new(RichText::new(format!("🌐 {source}")).weak()).truncate().sense(Sense::click());
                        match &r.remote_url {
                            Some(url) => {
                                let hover = format!("Source: {url}\nClick to copy. Change it with Remotes in the ribbon");
                                if ui.add(source_label).on_hover_text(hover).clicked() {
                                    actions.push(Action::CopyToClipboard(url.clone()));
                                }
                            }
                            None => {
                                if ui.add(source_label).on_hover_text("This repository has no remote yet. Click to add one").clicked() {
                                    actions.push(Action::ShowRemotes);
                                }
                            }
                        }
                        let folder = egui::Label::new(RichText::new(format!("📁 {}", r.path.display())).weak()).truncate().sense(Sense::click());
                        if ui.add(folder).on_hover_text("Current folder. Click to show it in the file manager").clicked() {
                            actions.push(Action::RevealInFiles);
                        }
                    });
                });
            });
        });
    }

    /// The buttons of the active tab.
    fn command_row(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let busy = self.busy.is_some();
        let active = self.active;
        let Self { tabs, repo, pulls, .. } = self;
        let Some(tab) = tabs.iter().find(|t| t.id == active) else { return };
        let button = theme::ribbon_button;

        egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 8, top: 2, bottom: 4 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(48.0);
                ui.spacing_mut().item_spacing.x = 2.0;
                match (&tab.kind, repo.as_mut()) {
                    (TabKind::Home, Some(r)) => {
                        remote_buttons(ui, r, busy, actions);
                        if button(ui, "🌐", "Remotes", !busy, false)
                            .on_hover_text("Where this repository is fetched from and pushed to: change the URL, add or remove remotes")
                            .clicked()
                        {
                            actions.push(Action::ShowRemotes);
                        }
                        ui.separator();
                        let branch = button(ui, BRANCH_ICON, &format!("{} ⏷", shorten(&r.head.display_name(), 26)), true, false)
                            .on_hover_text("Current branch: switch, create or delete branches");
                        egui::Popup::menu(&branch)
                            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                            .show(|ui| branch_menu(ui, r, actions));
                        if button(ui, "⤵", "Merge", true, false).on_hover_text(format!("Merge another branch into {}", r.head.display_name())).clicked() {
                            actions.push(Action::ShowMerge(None));
                        }
                        if r.repo.state() == git2::RepositoryState::Merge
                            && button(ui, "↩", "Abort merge", !busy, true).on_hover_text("Undo the merge and go back to how your branch was").clicked()
                        {
                            actions.push(Action::Net(NetOp::MergeAbort));
                        }
                        ui.separator();
                        if button(ui, ">_", "Terminal", true, false)
                            .on_hover_text(format!("New terminal session in this repository, in a new tab ({TERMINAL_SHORTCUT} goes to the last one)"))
                            .clicked()
                        {
                            actions.push(Action::NewTerminal);
                        }
                        if button(ui, "🔀", "Pull requests", r.remote_url.is_some(), false)
                            .on_hover_text("Review, approve or request changes on pull requests")
                            .clicked()
                        {
                            actions.push(Action::Show(Page::Pulls));
                        }
                        ui.separator();
                        if button(ui, "🔄", "Refresh", true, false).on_hover_text("Read the repository again (F5)").clicked() {
                            actions.push(Action::Refresh);
                        }
                        if button(ui, "📁", "Show folder", true, false).on_hover_text("Show this repository in the file manager").clicked() {
                            actions.push(Action::RevealInFiles);
                        }
                        if button(ui, "✖", "Close repo", true, false).on_hover_text("Close this repository (its terminals end)").clicked() {
                            actions.push(Action::CloseRepo);
                        }
                    }
                    (TabKind::Home, None) | (TabKind::Page(Page::Repositories), _) => {
                        if button(ui, "📂", "Open folder", true, false).on_hover_text("Open a repository on this computer (Ctrl+O)").clicked() {
                            actions.push(Action::OpenDialog);
                        }
                        if button(ui, "⬇", "Clone", true, false).on_hover_text("Clone a repository from a URL").clicked() {
                            actions.push(Action::ShowClone);
                        }
                        if button(ui, "✚", "New", true, false).on_hover_text("Create a new repository").clicked() {
                            actions.push(Action::InitDialog);
                        }
                        ui.separator();
                        if button(ui, "▣", "New group", true, false).on_hover_text("Create a group, then drag repository cards into it").clicked() {
                            actions.push(Action::AskGroupName(None));
                        }
                    }
                    (TabKind::Diff(d), Some(r)) => {
                        if d.source == DiffSource::Working {
                            if let Some(change) = r.changes.iter().find(|c| c.path == d.path) {
                                if change.fully_staged() {
                                    if button(ui, "☐", "Unstage", true, false).on_hover_text("Leave this file out of the next commit").clicked() {
                                        actions.push(Action::Unstage(vec![d.path.clone()]));
                                    }
                                } else if button(ui, "☑", "Stage", true, true).on_hover_text("Include this file in the next commit").clicked() {
                                    actions.push(Action::Stage(vec![d.path.clone()]));
                                }
                                if button(ui, "🗑", "Discard", true, false).on_hover_text("Throw away the changes in this file…").clicked() {
                                    actions.push(Action::AskDiscard(vec![change.clone()]));
                                }
                                ui.separator();
                            }
                        }
                        if button(ui, "📋", "Copy path", true, false).clicked() {
                            actions.push(Action::CopyToClipboard(d.path.clone()));
                        }
                        close_tab_button(ui, tab.id, actions);
                    }
                    (TabKind::Terminal { terminal, .. }, _) => {
                        let same_repo = repo.as_ref().is_some_and(|r| r.path == terminal.repo);
                        if button(ui, ">_", "New terminal", true, false).on_hover_text("Another session in the same folder, in a new tab").clicked() {
                            actions.push(if same_repo { Action::NewTerminal } else { Action::OpenTerminal(terminal.repo.clone()) });
                        }
                        if button(ui, "⟳", "Restart", true, false).on_hover_text("End this shell and start a new one").clicked() {
                            actions.push(Action::RestartTerminal);
                        }
                        close_tab_button(ui, tab.id, actions);
                        ui.separator();
                        ui.add_space(6.0);
                        let state = if terminal.is_running() { terminal.shell.clone() } else { format!("{} (ended)", terminal.shell) };
                        ui.add(egui::Label::new(RichText::new(format!("{state}  ·  {}", terminal.repo.display())).weak()).truncate());
                    }
                    (TabKind::Page(Page::Pulls), _) => {
                        pulls::list_ribbon(ui, pulls, actions);
                        close_tab_button(ui, tab.id, actions);
                    }
                    (TabKind::Pull(pull), _) => {
                        pulls::pull_ribbon(ui, pull, pulls, actions);
                        close_tab_button(ui, tab.id, actions);
                    }
                    _ => close_tab_button(ui, tab.id, actions),
                }
            });
        });
    }

    fn tab_row(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let repo_path = self.repo.as_ref().map(|r| r.path.clone());
        let home = match &self.repo {
            Some(r) => r.name(),
            None => "Repositories".to_string(),
        };
        egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 0 }).show(ui, |ui| {
            egui::ScrollArea::horizontal().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for tab in &self.tabs {
                        let (title, hint) = match &tab.kind {
                            TabKind::Home => (format!("{BRANCH_ICON}  {home}"), "Changes, history and commit details. This tab stays open.".to_string()),
                            TabKind::Page(Page::Repositories) => ("Repositories".to_string(), String::new()),
                            TabKind::Page(Page::Settings) => ("Settings".to_string(), String::new()),
                            TabKind::Page(Page::About) => ("About".to_string(), String::new()),
                            TabKind::Page(Page::Pulls) => ("Pull requests".to_string(), String::new()),
                            TabKind::Diff(d) => {
                                let name = split_path(&d.path).0;
                                match &d.source {
                                    DiffSource::Working => (name.to_string(), format!("{} (uncommitted changes)", d.path)),
                                    DiffSource::Commit(id) => (format!("{name} @ {}", git::short(*id)), format!("{} in commit {}", d.path, git::short(*id))),
                                }
                            }
                            TabKind::Terminal { number, terminal } => {
                                let mut title = format!(">_ Terminal {number}");
                                // A session of a repository other than the open one.
                                if Some(&terminal.repo) != repo_path.as_ref() {
                                    title.push_str(&format!(" · {}", git::repo_name(&terminal.repo)));
                                }
                                if !terminal.is_running() {
                                    title.push_str(" (ended)");
                                }
                                (title, terminal.repo.display().to_string())
                            }
                            TabKind::Pull(p) => (format!("#{} {}", p.number, shorten(&p.title, 28)), p.title.clone()),
                        };
                        let closable = tab.id != HOME;
                        let active = tab.id == self.active;
                        let out = theme::tab(ui, &title, active, closable);
                        let middle = out.response.middle_clicked();
                        if out.response.clicked() && !out.close {
                            actions.push(Action::Activate(tab.id));
                        }
                        if !hint.is_empty() {
                            out.response.on_hover_text(hint);
                        }
                        if closable && (out.close || middle) {
                            actions.push(Action::CloseTab(tab.id));
                        }
                    }
                });
            });
        });
    }

    /// The page of the active tab.
    fn tab_content(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let index = match self.tabs.iter().position(|t| t.id == self.active) {
            Some(index) => index,
            None => {
                self.active = HOME;
                0
            }
        };
        let page = match &self.tabs[index].kind {
            TabKind::Home if self.repo.is_none() => Some(Page::Repositories),
            TabKind::Page(page) => Some(*page),
            _ => None,
        };
        match page {
            Some(Page::Repositories) => return self.repositories_page(ui, actions),
            Some(Page::Settings) => return self.settings_page(ui, actions),
            Some(Page::About) => return about_page(ui),
            Some(Page::Pulls) => return pulls::list_page(ui, &self.pulls, actions),
            None => {}
        }

        let interactive = self.dialog.is_none();
        // Files with a diff tab are marked in the list of changes.
        let open_diffs: Vec<String> = self
            .tabs
            .iter()
            .filter_map(|t| match &t.kind {
                TabKind::Diff(d) if d.source == DiffSource::Working => Some(d.path.clone()),
                _ => None,
            })
            .collect();
        let Self { tabs, repo, settings, pulls, .. } = self;
        match (&mut tabs[index].kind, repo.as_mut()) {
            (TabKind::Home, Some(r)) => workspace(ui, r, settings.history_view_tree, &open_diffs, actions),
            (TabKind::Diff(d), Some(r)) => diff_page(ui, d, r),
            (TabKind::Terminal { terminal, .. }, _) => {
                let p = theme::pal(ui);
                let fill = term_view::background(&p, ui.visuals().dark_mode);
                egui::CentralPanel::default().frame(egui::Frame::new().fill(fill)).show(ui, |ui| {
                    if term_view::show(ui, terminal, settings.terminal_font_size, interactive).restart {
                        actions.push(Action::RestartTerminal);
                    }
                });
            }
            (TabKind::Pull(pull), _) => pulls::pull_page(ui, pull, pulls, actions),
            _ => {}
        }
    }

    fn repositories_page(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        page_frame(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    theme::page_header(ui, "Repositories", "Git repositories you work with. Drag a card onto a group to move it.");
                });
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    ui.add(TextEdit::singleline(&mut self.repo_filter).hint_text("Search repositories").desired_width(200.0));
                });
            });
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

            // Named groups A–Z (empty ones too, unless searching), then "Ungrouped".
            let mut groups: Vec<(String, Vec<&RepoCard>)> = self
                .settings
                .all_groups()
                .into_iter()
                .map(|g| (g, Vec::new()))
                .chain(std::iter::once((String::new(), Vec::new())))
                .collect();
            for card in cards {
                let g = self.settings.groups.get(&card.path).cloned().unwrap_or_default();
                if let Some((_, list)) = groups.iter_mut().find(|(name, _)| *name == g) {
                    list.push(card);
                }
            }
            let has_named = groups.iter().any(|(g, _)| !g.is_empty());
            groups.retain(|(g, list)| !list.is_empty() || (filter.is_empty() && (!g.is_empty() || has_named)));

            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                // Room for the outline of the first group while a card is dragged.
                ui.add_space(6.0);
                let dragged = egui::DragAndDrop::payload::<CardDrag>(ui.ctx());
                let mut over_group: Option<String> = None;
                for (group, list) in &groups {
                    let section = ui.scope(|ui| {
                        let collapsed = !group.is_empty() && filter.is_empty() && self.settings.collapsed_groups.contains(group);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let count = match list.len() {
                                1 => "1 repository".to_string(),
                                n => format!("{n} repositories"),
                            };
                            if group.is_empty() {
                                ui.label(RichText::new("Ungrouped").size(15.0).strong().weak());
                                ui.label(RichText::new(count).small().weak());
                            } else {
                                let arrow = theme::fold_arrow(ui, collapsed);
                                let name = ui.add(
                                    egui::Label::new(RichText::new(group.as_str()).size(15.0).strong())
                                        .truncate()
                                        .sense(Sense::click()),
                                );
                                if arrow
                                    .union(name)
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text(if collapsed { "Show repositories" } else { "Hide repositories" })
                                    .clicked()
                                {
                                    actions.push(Action::ToggleGroup(group.clone()));
                                }
                                ui.label(RichText::new(count).small().weak());
                                ui.add_space(8.0);
                                ui.spacing_mut().item_spacing.x = 2.0;
                                if theme::icon_button(ui, theme::Icon::Rename, "Rename group").clicked() {
                                    actions.push(Action::AskGroupName(Some(group.clone())));
                                }
                                if theme::icon_button(ui, theme::Icon::Delete, "Delete group (its repositories become ungrouped)").clicked() {
                                    actions.push(Action::DeleteGroup(group.clone()));
                                }
                            }
                        });
                        ui.add_space(4.0);
                        if collapsed {
                            return;
                        }
                        if list.is_empty() {
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(card_w, 56.0), Sense::hover());
                            ui.painter().rect_stroke(
                                rect,
                                8.0,
                                Stroke::new(1.0, theme::pal(ui).border),
                                egui::StrokeKind::Inside,
                            );
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "Drag repositories here",
                                egui::FontId::proportional(14.0),
                                ui.visuals().weak_text_color(),
                            );
                            ui.add_space(gap);
                        }
                        for chunk in list.chunks(per_row) {
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                for card in chunk {
                                    let is_current = current.as_ref() == Some(&card.path);
                                    let id = egui::Id::unique(("card-drag", &card.path));
                                    if let Some(grab) = drag_area(ui, id) {
                                        egui::DragAndDrop::set_payload(ui.ctx(), CardDrag { path: card.path.clone(), grab });
                                    }
                                    let lifted = dragged.as_ref().is_some_and(|d| d.path == card.path);
                                    let rect = repo_card(ui, card, is_current, lifted, card_w, actions);
                                    drag_area_drawn(ui, id, rect);
                                }
                            });
                            ui.add_space(gap);
                        }
                    });
                    let zone = section.response;
                    // While a card is dragged every group is outlined as a place
                    // to drop it, brighter under the pointer.
                    if dragged.is_some() {
                        let over = zone.contains_pointer();
                        if over {
                            over_group = Some(group.clone());
                        }
                        let rect = zone.rect.expand(6.0);
                        let accent = theme::pal(ui).accent_text;
                        if over {
                            ui.painter().rect_filled(rect, 8.0, accent.gamma_multiply(0.10));
                        }
                        let stroke = if over { Stroke::new(2.0, accent) } else { Stroke::new(1.0, accent.gamma_multiply(0.5)) };
                        ui.painter().rect_stroke(rect, 8.0, stroke, egui::StrokeKind::Inside);
                    }
                    if let Some(drop) = zone.dnd_release_payload::<CardDrag>() {
                        actions.push(Action::SetGroup(drop.path.clone(), group.clone()));
                    }
                    ui.add_space(8.0);
                }
                // The card itself comes along with the pointer.
                if let Some(drag) = &dragged {
                    if let Some(card) = self.cards.iter().find(|c| c.path == drag.path) {
                        let from = self.settings.groups.get(&card.path).map(String::as_str).unwrap_or_default();
                        let (hint, ok) = match over_group.as_deref() {
                            Some(g) if g == from => ("Already here".to_string(), false),
                            Some("") => ("Drop to ungroup".to_string(), true),
                            Some(g) => (format!("Drop to move to {g}"), true),
                            None => ("Drop on a group".to_string(), false),
                        };
                        let is_current = current.as_ref() == Some(&card.path);
                        // Nothing in the copy can be clicked, so what it asks for is dropped.
                        let mut ignored = Vec::new();
                        drag_float(ui.ctx(), drag.grab, &hint, ok, |ui| {
                            ui.style_mut().interaction.selectable_labels = false;
                            repo_card_body(ui, card, is_current, card_w, &mut ignored);
                        });
                    }
                }
            });
        });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        page_frame(ui, |ui| {
            theme::page_header(ui, "Settings", "Appearance, commit identities, terminal and pull requests.");
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.set_max_width(600.0);

                theme::section(ui, "Appearance", "Colors of the ribbon, pages and dialogs.");
                ui.horizontal(|ui| {
                    let mut light = self.settings.light_mode;
                    ui.selectable_value(&mut light, false, "Dark");
                    ui.selectable_value(&mut light, true, "Light");
                    if light != self.settings.light_mode {
                        actions.push(Action::SetLightMode(light));
                    }
                });

                ui.add_space(20.0);
                theme::section(
                    ui,
                    "Identity",
                    "The name and email recorded in every commit. Check it before you commit or push.",
                );
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.id_tab, IdTab::Global, "Global");
                    ui.add_enabled_ui(self.repo.is_some(), |ui| {
                        ui.selectable_value(&mut self.id_tab, IdTab::Repo, "This repository");
                    });
                });
                ui.add_space(4.0);
                match (self.id_tab, &self.repo) {
                    (IdTab::Repo, Some(r)) => {
                        ui.add(egui::Label::new(RichText::new(format!("{}  ·  {}", r.name(), r.path.display())).small().weak()).truncate());
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
                        field(ui, "Name (this repository only)", &mut self.local_form.name, "Uses global / folder rule");
                        field(ui, "Email (this repository only)", &mut self.local_form.email, "Uses global / folder rule");
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
                        field(ui, "Name", &mut self.identity.0, "Your Name");
                        field(ui, "Email", &mut self.identity.1, "you@example.com");
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

                ui.add_space(20.0);
                theme::section(
                    ui,
                    "Folder rules",
                    "Every repository inside a folder uses that folder's identity, for example all of \
                     C:\\work\\company uses your company email. Stored as Git includeIf rules, so the git \
                     command follows them too. Gitsot warns you before committing or pushing with a different email.",
                );
                if self.rules.is_empty() {
                    ui.label(RichText::new("No folder rules yet.").weak());
                }
                for rule in self.rules.clone() {
                    theme::card_frame(ui, false).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.set_max_width(ui.available_width() - 40.0);
                                ui.add(egui::Label::new(RichText::new(&rule.folder).monospace().strong()).truncate())
                                    .on_hover_text(&rule.folder);
                                ui.label(RichText::new(format!("uses {}", rule.identity.display())).small().weak());
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if theme::icon_button(ui, theme::Icon::Delete, "Remove rule").clicked() {
                                    actions.push(Action::RemoveRule(rule.clone()));
                                }
                            });
                        });
                    });
                }
                ui.add_space(8.0);
                ui.label(RichText::new("Add a rule").strong());
                ui.label(RichText::new("Folder").small().weak());
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.rule_form.0).hint_text("e.g. C:\\work\\company").desired_width(ui.available_width() - 90.0));
                    if ui.button("Browse…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_title("Folder for this identity").pick_folder() {
                            self.rule_form.0 = d.display().to_string();
                        }
                    }
                });
                field(ui, "Name", &mut self.rule_form.1.name, "Name for this folder");
                field(ui, "Email", &mut self.rule_form.1.email, "you@company.com");
                let ok = !self.rule_form.0.trim().is_empty() && !self.rule_form.1.email.trim().is_empty();
                if theme::primary_enabled(ui, ok, "Add rule").clicked() {
                    actions.push(Action::AddRule { folder: self.rule_form.0.clone(), identity: self.rule_form.1.clone() });
                }

                ui.add_space(20.0);
                theme::section(
                    ui,
                    "Terminal",
                    "The built-in terminal opens in the repository folder: PowerShell on Windows, zsh on macOS.",
                );
                let mut prompt = self.settings.terminal_prompt;
                if ui
                    .checkbox(&mut prompt, "Gitsot prompt")
                    .on_hover_text("Shows folder, branch, changes, what to push or pull and the commit identity before every command.")
                    .changed()
                {
                    actions.push(Action::SetTerminalPrompt(prompt));
                }
                ui.label(
                    RichText::new("Loaded after your own shell profile. Turn it off to keep your own prompt. Applies to terminals started from now on.")
                        .small()
                        .weak(),
                );
                ui.horizontal(|ui| {
                    ui.label("Text size");
                    let mut size = self.settings.terminal_font_size;
                    if ui.add(egui::Slider::new(&mut size, 11.0..=20.0).step_by(1.0).fixed_decimals(0)).changed() {
                        actions.push(Action::SetTerminalFontSize(size));
                    }
                });

                ui.add_space(20.0);
                theme::section(
                    ui,
                    "Remote updates",
                    "Gitsot fetches from the remote every 3 minutes, so Pull, Push and Fetch show how many commits are waiting.",
                );
                let mut check = self.settings.background_check;
                if ui
                    .checkbox(&mut check, "Check for new commits in the background")
                    .on_hover_text("Turn it off to fetch only when you click Fetch, Pull or the Check button in the status bar.")
                    .changed()
                {
                    actions.push(Action::SetBackgroundCheck(check));
                }
                ui.label(
                    RichText::new(
                        "The check uses the sign-in each repository already has saved in Git. It never asks for a password \
                         and never uses one typed into Gitsot. When no saved sign-in works, it stops quietly and the status bar says so.",
                    )
                    .small()
                    .weak(),
                );

                ui.add_space(20.0);
                theme::section(ui, "Git command", "");
                if self.git_cli {
                    ui.label(RichText::new("Found on PATH. Used for clone, fetch, pull and push.").weak());
                } else {
                    ui.label(
                        RichText::new("Not found. Install Git from git-scm.com to clone, fetch, pull and push.")
                            .color(theme::ORANGE),
                    );
                }

                ui.add_space(20.0);
                theme::section(
                    ui,
                    "Pull requests",
                    "Review, approve or request changes on pull requests of the open repository. \
                     GitHub and GitHub Enterprise are supported in this version.",
                );
                if self.curl_cli {
                    ui.label(RichText::new("curl command found on PATH. Used to talk to the Git host.").weak());
                } else {
                    ui.label(RichText::new("curl command not found. It ships with Windows 10 and later and with macOS.").color(theme::ORANGE));
                }
                ui.label(
                    RichText::new(
                        "Reviews are made with the sign-in Git has saved for the repository's HTTPS remote, \
                         or with an access token you enter for the session. gitsot never saves a token.",
                    )
                    .small()
                    .weak(),
                );
                for host in self.settings.github_hosts.clone() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&host).monospace());
                        ui.label(RichText::new("is treated as GitHub Enterprise").weak());
                        if theme::icon_button(ui, theme::Icon::Delete, "Forget this host").clicked() {
                            actions.push(Action::ForgetGitHubHost(host.clone()));
                        }
                    });
                }
                ui.add_space(12.0);
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
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
                        if r.head.has_remote {
                            ui.add_space(8.0);
                            let checking = self.checking.as_ref() == Some(&r.path);
                            last_updated(ui, r, checking, self.busy.is_some() || !self.git_cli, self.settings.background_check, actions);
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
            ui.set_width(if matches!(dialog, Dialog::Remotes { .. }) { 620.0 } else { 440.0 });
            match dialog {
                Dialog::Remotes { rows, new_name, new_url, confirm_remove } => {
                    if remotes_dialog(ui, rows, new_name, new_url, confirm_remove, busy, actions) {
                        close = true;
                    }
                }
                Dialog::Review(form) => {
                    if pulls::review_dialog(ui, form.number, form.event, &mut form.body, &self.pulls, actions) {
                        close = true;
                    }
                }
                Dialog::Token { token } => {
                    if pulls::token_dialog(ui, token, &self.pulls, actions) {
                        close = true;
                    }
                }
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
                        let enter = ui.input(|i| i.key_pressed(Key::Enter));
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
                Dialog::Merge { filter, selected, preview } => {
                    let Some(r) = self.repo.as_ref() else {
                        close = true;
                        return;
                    };
                    let current = r.head.display_name();
                    ui.set_max_width(400.0);
                    ui.heading(format!("Merge into {current}"));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Pick the branch whose changes you want to bring into your current branch.").weak());
                    ui.add_space(6.0);
                    ui.add(TextEdit::singleline(filter).hint_text("Filter branches").desired_width(400.0));
                    let f = filter.to_lowercase();
                    let local = r.branches.iter().filter(|b| !b.is_head).map(|b| b.name.clone());
                    let remote = r.remote_branches.iter().cloned();
                    let names: Vec<String> = local.chain(remote).filter(|n| n.to_lowercase().contains(&f)).collect();
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.set_min_width(400.0);
                        if names.is_empty() {
                            ui.label(RichText::new("No other branches.").weak());
                        }
                        for n in names {
                            let is_sel = selected.as_deref() == Some(n.as_str());
                            if ui.add(egui::Button::selectable(is_sel, n.as_str()).truncate()).clicked() {
                                *selected = Some(n);
                            }
                        }
                    });
                    // Work out what the merge would do, once per selected branch.
                    match selected.as_deref() {
                        Some(b) if preview.as_ref().is_none_or(|(name, _)| name != b) => {
                            *preview = Some((b.to_string(), git::merge_preview(&r.repo, b).map_err(|e| format!("{e:#}"))));
                        }
                        Some(_) => {}
                        None => *preview = None,
                    }
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);
                    let can_merge = merge_preview(ui, preview.as_ref(), &current);
                    // A local branch that is behind its remote would merge old commits.
                    let stale = selected.as_deref().and_then(|s| r.branches.iter().find(|b| b.name == s && b.behind > 0));
                    if let Some((b, up)) = stale.and_then(|b| b.upstream.as_ref().map(|up| (b, up))) {
                        ui.add_space(4.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "Your local {} is {} behind {up}. Pick {up} in the list to merge the newest.",
                                    b.name,
                                    commits(b.behind)
                                ))
                                .small()
                                .color(theme::ORANGE),
                            );
                        });
                    }
                    ui.add_space(8.0);
                    if theme::primary_wide(ui, can_merge, "Create a merge commit", 32.0).clicked() {
                        if let Some(b) = selected.clone() {
                            actions.push(Action::Net(NetOp::Merge { branch: b }));
                        }
                        close = true;
                    }
                    ui.add_space(4.0);
                    ui.vertical_centered(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::ConfirmDeleteBranch { name, remote, unmerged } => {
                    ui.heading("Delete branch?");
                    ui.add_space(4.0);
                    match remote {
                        Some(remote) => {
                            ui.label(format!("The branch '{name}' will be deleted from {remote}, for everyone who uses it."));
                            ui.label(RichText::new("Your local branches are not touched.").weak());
                        }
                        None => {
                            ui.label(format!("The local branch '{name}' will be deleted from this computer."));
                            if *unmerged {
                                ui.label(
                                    RichText::new("It has commits that are not in your current branch. They will be lost unless they were pushed.")
                                        .color(theme::ORANGE),
                                );
                            } else {
                                ui.label(RichText::new("All its commits are already in your current branch.").weak());
                            }
                        }
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Delete").color(theme::RED)).clicked() {
                            actions.push(match remote {
                                Some(remote) => Action::Net(NetOp::DeleteRemoteBranch { remote: remote.clone(), branch: name.clone() }),
                                None => Action::DeleteBranch(name.clone()),
                            });
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
                    let existing = self.settings.all_groups();
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
                        let enter = ui.input(|i| i.key_pressed(Key::Enter));
                        if theme::primary(ui, "Save").clicked() || enter {
                            actions.push(Action::SetGroup(path.clone(), name.clone()));
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::GroupName { name, rename } => {
                    ui.heading(if rename.is_some() { "Rename group" } else { "New group" });
                    ui.add_space(6.0);
                    let resp = ui.add(TextEdit::singleline(name).hint_text("Group name, e.g. Company").desired_width(320.0));
                    resp.request_focus();
                    ui.label(RichText::new("Drag repository cards onto a group to move them.").small().weak());
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let enter = ui.input(|i| i.key_pressed(Key::Enter));
                        let label = if rename.is_some() { "Rename" } else { "Create group" };
                        if (theme::primary_enabled(ui, !name.trim().is_empty(), label).clicked() || enter) && !name.trim().is_empty() {
                            actions.push(Action::SaveGroupName { name: name.clone(), rename: rename.clone() });
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

/// The Remotes dialog. Returns true when it should close.
fn remotes_dialog(
    ui: &mut egui::Ui,
    rows: &mut [RemoteRow],
    new_name: &mut String,
    new_url: &mut String,
    confirm_remove: &mut Option<String>,
    busy: bool,
    actions: &mut Vec<Action>,
) -> bool {
    let mut close = false;
    ui.heading("Remotes");
    ui.label(RichText::new("Where this repository is fetched from and pushed to. Changing a URL only changes this computer's copy.").weak());
    ui.add_space(8.0);

    if rows.is_empty() {
        ui.label(RichText::new("This repository has no remote yet. Add one below to fetch, pull and push.").weak());
    }
    for (i, row) in rows.iter_mut().enumerate() {
        theme::card_frame(ui, false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&row.name).strong());
                if i == 0 {
                    ui.label(RichText::new("default: used by Fetch, Publish branch and Pull requests").small().weak());
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::icon_button(ui, theme::Icon::Delete, "Remove this remote…").clicked() {
                        *confirm_remove = Some(row.name.clone());
                    }
                });
            });
            let has_push = row.saved.push_url.is_some();
            ui.horizontal(|ui| {
                let edited = row.edited();
                let valid = git::check_remote_url(&row.url).is_ok();
                // Right to left so the URL field takes whatever width is left.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(edited, egui::Button::new("Reset")).on_hover_text("Put back the saved URL").clicked() {
                        row.url = row.saved.url.clone();
                        row.push = row.saved.push_url.clone().unwrap_or_default();
                    }
                    let save = theme::primary_enabled(ui, edited && valid && !busy, "Save").on_hover_text("Use this URL from now on");
                    let field = ui.add(
                        TextEdit::singleline(&mut row.url)
                            .hint_text("https://host/user/project.git or git@host:user/project.git")
                            .desired_width(f32::INFINITY),
                    );
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if (save.clicked() || enter) && edited && valid && !busy {
                        actions.push(Action::SaveRemote { name: row.name.clone(), url: row.url.clone(), push: has_push.then(|| row.push.clone()) });
                    }
                });
            });
            if has_push {
                ui.add_space(2.0);
                field(ui, "Push URL (pushes go here instead; leave empty to push to the URL above)", &mut row.push, "");
            }
            if row.edited() {
                let (from, to) = (remote_host(&row.saved.url), remote_host(row.url.trim()));
                let note = if git::check_remote_url(&row.url).is_err() {
                    "Enter a URL to save.".to_string()
                } else if from != to {
                    format!("Host changes from {from} to {to}. Check that your commit email is right for the new host.")
                } else {
                    "Not saved yet.".to_string()
                };
                ui.label(RichText::new(note).small().color(theme::ORANGE));
            }
            if confirm_remove.as_deref() == Some(row.name.as_str()) {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "Remove '{}'? Its remote branches disappear from this computer and local branches stop following it. Nothing changes on the server.",
                        row.name
                    ));
                });
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, egui::Button::new(RichText::new("Remove").color(theme::RED))).clicked() {
                        actions.push(Action::RemoveRemote(row.name.clone()));
                    }
                    if ui.button("Keep").clicked() {
                        *confirm_remove = None;
                    }
                });
            }
        });
        ui.add_space(6.0);
    }

    ui.add_space(4.0);
    ui.label(RichText::new("Add a remote").strong());
    ui.horizontal(|ui| {
        let ready = !new_name.trim().is_empty() && git::check_remote_url(new_url).is_ok() && !busy;
        ui.add(TextEdit::singleline(new_name).hint_text("Name, e.g. upstream").desired_width(160.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let add = theme::primary_enabled(ui, ready, "Add");
            let field = ui.add(TextEdit::singleline(new_url).hint_text("URL").desired_width(f32::INFINITY));
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (add.clicked() || enter) && ready {
                actions.push(Action::AddRemote { name: new_name.clone(), url: new_url.clone() });
            }
        });
    });
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui.button("Close").clicked() {
            close = true;
        }
    });
    close
}

/// Labelled single-line text field that fills the available width (termker's field).
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(RichText::new(label).small().weak());
    ui.add(TextEdit::singleline(value).hint_text(hint).desired_width(f32::INFINITY));
}

/// Standard padding around a full page.
fn page_frame(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .inner_margin(egui::Margin::same(24))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add(ui)
        });
}

/// Drag-and-drop payload: the repository card being moved and how it was
/// picked up.
struct CardDrag {
    path: PathBuf,
    grab: Grab,
}

/// How a card was picked up, kept while it is dragged so its copy under the
/// pointer stays where the hand took hold of it.
#[derive(Clone, Copy)]
struct Grab {
    /// From the top left corner of the card to the pointer.
    offset: Vec2,
    /// Size of the card where it lies.
    size: Vec2,
    /// When it was picked up, in egui's clock.
    at: f64,
}

/// Makes an area draggable without taking clicks away from the buttons drawn
/// in it. Call before drawing the area, then [`drag_area_drawn`] with the space
/// it took. The drag is registered *before* the content so the buttons end up
/// on top of it; where the content was is remembered from the last frame.
/// Returns how it was picked up in the frame a drag starts.
fn drag_area(ui: &mut egui::Ui, id: egui::Id) -> Option<Grab> {
    let rect = ui.data(|d| d.get_temp::<egui::Rect>(id))?;
    if !ui.interact(rect, id, Sense::drag()).drag_started() {
        return None;
    }
    let (pressed, at) = ui.input(|i| (i.pointer.press_origin(), i.time));
    Some(Grab { offset: pressed.unwrap_or(rect.center()) - rect.min, size: rect.size(), at })
}

fn drag_area_drawn(ui: &egui::Ui, id: egui::Id, rect: egui::Rect) {
    ui.data_mut(|d| d.insert_temp(id, rect));
}

/// Seconds a card takes to rise off the page once it is picked up.
const LIFT_TIME: f64 = 0.12;
/// How much of a card stays visible where it lies while its copy is dragged.
const DRAGGED_OPACITY: f32 = 0.3;

/// The dragged card itself, lifted off the page: it follows the pointer from
/// the spot it was taken by, a little larger and with a shadow under it.
/// `content` draws the inside of the card. A line under the card says what
/// dropping here does; `ok` is false when dropping here changes nothing.
fn drag_float(ctx: &egui::Context, grab: Grab, hint: &str, ok: bool, content: impl FnOnce(&mut egui::Ui)) {
    let Some(pointer) = ctx.pointer_latest_pos() else { return };
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    // 0 on the page, 1 fully lifted.
    let lift = (((ctx.input(|i| i.time) - grab.at) / LIFT_TIME) as f32).clamp(0.0, 1.0);
    if lift < 1.0 {
        ctx.request_repaint();
    }
    let dark = ctx.global_style().visuals.dark_mode;
    let shadow = egui::Shadow {
        offset: [0, (10.0 * lift) as i8],
        blur: (28.0 * lift) as u8,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 150 } else { 70 }),
    };
    let area = egui::Area::new(egui::Id::unique("gitsot-drag-float"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pointer - grab.offset - Vec2::new(0.0, 3.0 * lift))
        .interactable(false)
        .show(ctx, |ui| {
            let p = theme::pal(ui);
            ui.spacing_mut().item_spacing.y = 8.0;
            let frame = theme::card_frame(ui, true).shadow(shadow);
            let margin = frame.inner_margin.sum();
            frame.show(ui, |ui| {
                ui.set_width(grab.size.x - margin.x);
                ui.set_min_height(grab.size.y - margin.y);
                content(ui);
            });
            let (fill, text) = if ok { (p.accent, Color32::WHITE) } else { (p.panel, p.text_dim) };
            egui::Frame::new()
                .fill(fill)
                .stroke(Stroke::new(1.0, if ok { p.accent } else { p.border }))
                .corner_radius(10)
                .inner_margin(egui::Margin::symmetric(10, 3))
                .show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(hint).small().color(text)).extend());
                });
        });
    // Grown around the pointer, so the spot it is held by does not move.
    let scale = 1.0 + 0.03 * lift;
    ctx.set_transform_layer(
        area.response.layer_id,
        egui::emath::TSTransform::from_translation(pointer.to_vec2())
            * egui::emath::TSTransform::from_scaling(scale)
            * egui::emath::TSTransform::from_translation(-pointer.to_vec2()),
    );
}

/// Draws a repository card and returns the space it took.
/// `lifted` leaves only a faint copy, while the card is carried around.
fn repo_card(ui: &mut egui::Ui, card: &RepoCard, is_current: bool, lifted: bool, width: f32, actions: &mut Vec<Action>) -> egui::Rect {
    ui.scope(|ui| {
        if lifted {
            ui.set_opacity(DRAGGED_OPACITY);
        }
        theme::card_frame(ui, is_current)
            .show(ui, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                ui.with_layout(Layout::top_down(Align::Min), |ui| repo_card_body(ui, card, is_current, width, actions));
            })
            .response
            .rect
    })
    .inner
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
                    ui.label(RichText::new("Current").small().color(theme::pal(ui).accent_text));
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(&card.name).size(15.0).strong()).truncate());
                });
            });
        });
        ui.add(egui::Label::new(RichText::new(card.path.display().to_string()).monospace().small().weak()).truncate())
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
        let label = if is_current { "Go to repository" } else { "Open" };
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let terminal = ui
                    .add_enabled(card.exists, egui::Button::new(">_  Terminal").min_size(Vec2::new(0.0, 28.0)))
                    .on_hover_text("Open a new terminal session in this repository");
                if terminal.clicked() {
                    actions.push(Action::OpenTerminal(card.path.clone()));
                }
                if theme::primary_wide(ui, card.exists, label, 28.0).clicked() {
                    actions.push(if is_current { Action::Activate(HOME) } else { Action::OpenPath(card.path.clone()) });
                }
            });
        });
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
    let button = |ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool, accent: bool, count: usize, up: bool| {
        theme::ribbon_button_badge(ui, icon, label, enabled, accent, Some(theme::Badge { count, up }))
    };
    // Fetch counts what waits on the other branches; Pull and Push count the current one.
    let others: Vec<&BranchInfo> = r.branches.iter().filter(|b| !b.is_head && b.behind > 0).collect();
    let elsewhere: usize = others.iter().map(|b| b.behind).sum();
    let fetch_hint = if elsewhere > 0 {
        let names: Vec<&str> = others.iter().take(4).map(|b| b.name.as_str()).collect();
        let more = if others.len() > names.len() { ", …" } else { "" };
        format!(
            "Download new commits and bring all your branches up to date\n{} waiting on your other branches: {}{more}\nClick to update them. A branch with commits that are not pushed yet is left alone.",
            commits(elsewhere),
            names.join(", ")
        )
    } else {
        "Download new commits and bring all your branches up to date".to_string()
    };
    if button(ui, "⟳", "Fetch", remote && !busy, false, elsewhere, false).on_hover_text(fetch_hint).clicked() {
        actions.push(Action::Net(NetOp::Fetch));
    }
    let can_pull = remote && !busy && r.head.upstream.is_some();
    let pull_hint = match (&r.head.upstream, r.head.behind) {
        (Some(up), n) if n > 0 => format!("Bring {} from {up} into this branch", commits(n)),
        _ => "Bring the remote's commits into this branch".to_string(),
    };
    if button(ui, "⬇", "Pull", can_pull, r.head.behind > 0, r.head.behind, false).on_hover_text(pull_hint).clicked()
    {
        actions.push(Action::Net(NetOp::Pull));
    }
    let (push_label, set_upstream) = if r.head.upstream.is_none() {
        let remote_name = git::default_remote(&r.repo).unwrap_or_else(|| "origin".into());
        let branch = r.head.branch.clone().unwrap_or_default();
        ("Publish branch".to_string(), Some((remote_name, branch)))
    } else {
        ("Push".to_string(), None)
    };
    let can_push = remote && !busy && on_branch && !r.head.short_id.is_empty();
    let waiting = r.head.ahead > 0 || r.head.upstream.is_none();
    let push_hint = match (remote, &r.head.upstream, r.head.ahead) {
        (false, ..) => "This repository has no remote".to_string(),
        (true, Some(up), n) if n > 0 => format!("Upload {} to {up}", commits(n)),
        (true, ..) => "Upload your commits to the remote".to_string(),
    };
    if button(ui, "⬆", &push_label, can_push, waiting, r.head.ahead, true).on_hover_text(push_hint).clicked()
    {
        actions.push(Action::Net(NetOp::Push { set_upstream }));
    }
}

/// "Last updated on 24/11/2026 09:44:17 PM" with a small button to check
/// again, at the right of the status bar. Laid out right to left.
fn last_updated(ui: &mut egui::Ui, r: &RepoView, checking: bool, busy: bool, automatic: bool, actions: &mut Vec<Action>) {
    if let Some(e) = &r.check_error {
        ui.label(RichText::new("⚠ Last check failed").color(theme::ORANGE)).on_hover_text(e);
    }
    let text = match (checking, r.last_updated) {
        (true, _) => "Checking for new commits…".to_string(),
        (false, Some(secs)) => format!("Last updated on {}", update_time(secs)),
        (false, None) => "Not updated yet".to_string(),
    };
    let how = if automatic {
        "When new commits were last fetched from the remote. Gitsot checks every 3 minutes."
    } else {
        "When new commits were last fetched from the remote. The automatic check is off in Settings."
    };
    ui.label(RichText::new(text).weak()).on_hover_text(how);
    if checking {
        ui.add(egui::Spinner::new().size(12.0));
    } else if ui.add_enabled(!busy, egui::Button::new(RichText::new("⟳ Check").small()).small())
        .on_hover_text("Check the remote for new commits now")
        .clicked()
    {
        actions.push(Action::CheckNow);
    }
}

/// `24/11/2026 09:44:17 PM` in local time.
fn update_time(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%d/%m/%Y %I:%M:%S %p").to_string())
        .unwrap_or_default()
}

/// The middle of the Merge dialog: a tick and "This will merge 21 commits
/// from main into main_syukri", or why there is nothing to merge. Returns
/// whether the merge can go ahead.
fn merge_preview(ui: &mut egui::Ui, preview: Option<&(String, Result<git::MergePreview, String>)>, current: &str) -> bool {
    let p = theme::pal(ui);
    let green = Color32::from_rgb(63, 185, 80);
    let font = egui::TextStyle::Body.resolve(ui.style());
    // Plain words dim, the numbers and branch names bright.
    let sentence = |parts: &[(&str, bool)]| {
        let mut job = egui::text::LayoutJob::default();
        for (text, strong) in parts {
            let color = if *strong { p.text } else { p.text_dim };
            job.append(text, 0.0, egui::TextFormat::simple(font.clone(), color));
        }
        job.halign = Align::Center;
        job
    };
    let mut ok = false;
    ui.vertical_centered(|ui| match preview {
        None => {
            ui.add_space(22.0);
            ui.label(RichText::new("Pick a branch to see what will be merged.").weak());
        }
        Some((_, Err(e))) => {
            ui.label(RichText::new("⚠").size(16.0).color(theme::ORANGE));
            ui.label(RichText::new(e).color(p.danger));
        }
        Some((branch, Ok(m))) if m.commits == 0 => {
            ui.label(RichText::new("✔").size(16.0).color(green));
            ui.label(sentence(&[(current, true), (" is already up to date with ", false), (branch, true)]));
        }
        Some((branch, Ok(m))) => {
            ok = true;
            let count = commits(m.commits);
            if m.conflicts > 0 {
                let files = if m.conflicts == 1 { "1 conflicted file".to_string() } else { format!("{} conflicted files", m.conflicts) };
                ui.label(RichText::new("⚠").size(16.0).color(theme::ORANGE));
                ui.label(sentence(&[
                    ("There will be ", false),
                    (&files, true),
                    (" when merging ", false),
                    (&count, true),
                    (" from ", false),
                    (branch, true),
                    (" into ", false),
                    (current, true),
                ]));
                ui.label(RichText::new("You fix them in Changes after the merge starts, or abort it.").small().weak());
            } else {
                ui.label(RichText::new("✔").size(16.0).color(green));
                ui.label(sentence(&[
                    ("This will merge ", false),
                    (&count, true),
                    (" from ", false),
                    (branch, true),
                    (" into ", false),
                    (current, true),
                ]));
                if m.fast_forward {
                    ui.label(
                        RichText::new(format!("{current} has no commits of its own, so it just moves forward. No extra merge commit is needed."))
                            .small()
                            .weak(),
                    );
                }
            }
        }
    });
    ok
}

/// List / Tree switch for the history column.
fn segmented(ui: &mut egui::Ui, tree: bool, actions: &mut Vec<Action>) {
    // Small buttons with a gap between them, so the heading row stays low.
    ui.spacing_mut().item_spacing.x = 6.0;
    ui.spacing_mut().button_padding = Vec2::new(10.0, 2.0);
    ui.spacing_mut().interact_size.y = 22.0;
    // Right-to-left layout: add "Tree" first so it ends up on the right.
    for (label, value, hint) in [("Tree", true, "Every branch as a graph"), ("List", false, "Only the commits of the current branch")] {
        let selected = tree == value;
        let resp = if selected { theme::primary(ui, label) } else { ui.button(label) };
        if resp.on_hover_text(hint).clicked() && !selected {
            actions.push(Action::SetHistoryTree(value));
        }
    }
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
    ui.set_min_width(320.0);
    ui.set_max_width(320.0);
    if ui.button("✚  New branch…").clicked() {
        actions.push(Action::ShowNewBranch);
        ui.close();
    }
    if ui.button(format!("⤵  Merge into {}…", r.head.display_name())).clicked() {
        actions.push(Action::ShowMerge(None));
        ui.close();
    }
    ui.separator();
    ui.add(TextEdit::singleline(&mut r.branch_filter).hint_text("Filter branches").desired_width(f32::INFINITY));
    let filter = r.branch_filter.to_lowercase();

    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
        ui.label(RichText::new("Local branches").small().weak());
        for b in r.branches.iter().filter(|b| b.name.to_lowercase().contains(&filter)) {
            ui.horizontal(|ui| {
                // The delete icon goes on the right; the current branch can't be deleted.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if b.is_head {
                        ui.add_space(26.0);
                    } else if theme::icon_button(ui, theme::Icon::Delete, "Delete this branch…").clicked() {
                        actions.push(Action::AskDeleteBranch(b.name.clone()));
                        ui.close();
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        let label = if b.is_head { format!("✔ {}", b.name) } else { b.name.clone() };
                        let resp = ui.add(egui::Button::selectable(b.is_head, label).truncate());
                        let resp = match (&b.upstream, b.is_head) {
                            (_, true) => resp.on_hover_text("The branch you are on (switch to another branch to delete it)"),
                            (Some(u), _) if b.ahead + b.behind > 0 => {
                                resp.on_hover_text(format!("Tracks {u}: {} to pull, {} to push", b.behind, b.ahead))
                            }
                            (Some(u), _) => resp.on_hover_text(format!("Tracks {u}")),
                            (None, _) => resp.on_hover_text("Not published yet"),
                        };
                        if resp.clicked() && !b.is_head {
                            actions.push(Action::Checkout(b.name.clone()));
                            ui.close();
                        }
                    });
                });
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
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::icon_button(ui, theme::Icon::Delete, "Delete this branch from the remote…").clicked() {
                            actions.push(Action::AskDeleteRemoteBranch(n.clone()));
                            ui.close();
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            if ui
                                .add(egui::Button::selectable(false, n.as_str()).truncate())
                                .on_hover_text("Check out as a new local branch")
                                .clicked()
                            {
                                actions.push(Action::CheckoutRemote(n.clone()));
                                ui.close();
                            }
                        });
                    });
                });
            }
        }
    });
    ui.label(RichText::new("Click a branch to switch to it. Use the bin icon to delete one.").small().weak());
}

/// First column: changed files to tick, and the commit box under them.
fn changes_column(ui: &mut egui::Ui, r: &mut RepoView, open_diffs: &[String], actions: &mut Vec<Action>) {
    if r.repo.state() == git2::RepositoryState::Merge {
        egui::Panel::top("merge_banner").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Abort merge").on_hover_text("Undo the merge and go back to how your branch was").clicked() {
                        actions.push(Action::Net(NetOp::MergeAbort));
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new("Merging: fix the ! files, tick them, then commit").color(theme::ORANGE)).wrap());
                    });
                });
            });
            ui.add_space(2.0);
        });
    }
    // Commit box at the bottom of the column.
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

    theme::column_header(ui, "Changes");
    ui.add_space(4.0);
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
                        // Open in a tab already: shown as selected.
                        let row = file_row(ui, open_diffs.contains(&c.path), &c.path);
                        if row.clicked() {
                            actions.push(Action::OpenDiff(DiffSource::Working, c.path.clone()));
                        }
                        row.context_menu(|ui| {
                            if ui.button("Open in a tab").clicked() {
                                actions.push(Action::OpenDiff(DiffSource::Working, c.path.clone()));
                                ui.close();
                            }
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

/// Second column: the commits, as a branch tree or a plain list.
fn history_column(ui: &mut egui::Ui, r: &mut RepoView, tree: bool, actions: &mut Vec<Action>) {
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
            let resp = resp.on_hover_ui_at_pointer(|ui| {
                ui.label(format!("Commit date: {}", full_time(c.time)));
            });
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

/// Third column: the selected commit, its files and the diff of one file.
fn commit_column(ui: &mut egui::Ui, r: &mut RepoView, actions: &mut Vec<Action>) {
    theme::column_header(ui, "Commit");
    ui.add_space(4.0);
    let Some(id) = r.selected_commit else {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("Select a commit").weak());
        });
        return;
    };
    let Some(c) = r.history.iter().find(|c| c.id == id).cloned() else { return };

    egui::Panel::top("commit_header").show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(&c.summary).size(17.0).strong()).wrap());
        let body = c.message.split_once('\n').map(|x| x.1).unwrap_or("").trim();
        if !body.is_empty() {
            egui::ScrollArea::vertical().id_salt("commit_body").max_height(100.0).show(ui, |ui| {
                ui.add(egui::Label::new(body).wrap());
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

    egui::Panel::top("commit_files")
        .resizable(true)
        .default_size(130.0)
        .min_size(60.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("commit_files_list").auto_shrink(false).show(ui, |ui| {
                for f in &r.commit_files {
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            kind_badge(ui, f.kind);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let sel = r.selected_commit_file.as_deref() == Some(f.path.as_str());
                                let row = file_row(ui, sel, &f.path).on_hover_text("Double-click to open in a tab");
                                if row.double_clicked() {
                                    actions.push(Action::OpenDiff(DiffSource::Commit(id), f.path.clone()));
                                } else if row.clicked() {
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
pub(crate) fn diff_view(ui: &mut egui::Ui, diff: &FileDiff, id: &str) {
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
