//! Pull requests: the list tab, one tab per pull request, and the review and
//! sign-in dialogs. The host is reached through `gitsot_core::forge`, so
//! nothing here knows which Git host it is talking to.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use eframe::egui::{self, Align, Align2, Color32, FontId, Layout, RichText, Sense, Stroke, TextEdit, Vec2};

use gitsot_core::forge::{self, Filter, Kind, PullDetail, PullState, PullSummary, Remote, ReviewEvent, ReviewState};

use crate::app::{diff_view, file_row, full_time, kind_badge, relative_time, Action};
use crate::theme;

/// Something read from the host in the background.
pub enum Load<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(forge::Error),
}

/// Pull request state of the open repository.
pub struct Pulls {
    /// Where the repository's remote points, if it is a Git host.
    pub remote: Option<Remote>,
    pub filter: Filter,
    pub list: Load<Vec<PullSummary>>,
    /// Access token typed in this session. Never saved.
    pub token: Option<String>,
    /// The account the host knows us as, once a pull request was read.
    pub viewer: Option<String>,
}

impl Default for Pulls {
    fn default() -> Self {
        Pulls { remote: None, filter: Filter::Open, list: Load::Idle, token: None, viewer: None }
    }
}

impl Pulls {
    /// `github_hosts` are hosts the user marked as GitHub Enterprise.
    pub fn new(remote_url: Option<&str>, github_hosts: &[String]) -> Self {
        let mut remote = remote_url.and_then(Remote::parse);
        if let Some(r) = &mut remote {
            if r.kind == Kind::Unknown && github_hosts.contains(&r.host) {
                r.kind = Kind::GitHub;
            }
        }
        Pulls { remote, ..Default::default() }
    }

    fn host(&self) -> &str {
        self.remote.as_ref().map(|r| r.host.as_str()).unwrap_or("the host")
    }

    fn supported(&self) -> bool {
        self.remote.as_ref().is_some_and(|r| r.provider().is_some())
    }
}

/// One pull request open in a tab.
pub struct PullTab {
    pub number: u64,
    pub title: String,
    pub detail: Load<PullDetail>,
    /// Being read (again) from the host.
    pub loading: bool,
    /// A review is on its way to the host.
    pub sending: bool,
    /// Index of the file whose diff is shown.
    pub selected: usize,
}

impl PullTab {
    pub fn new(number: u64, title: String) -> Self {
        PullTab { number, title, detail: Load::Loading, loading: true, sending: false, selected: 0 }
    }
}

pub enum Request {
    List(Filter),
    Detail(u64),
    Review { number: u64, event: ReviewEvent, body: String },
}

pub enum Payload {
    List(Vec<PullSummary>),
    Detail(Box<PullDetail>),
    Reviewed,
}

pub struct Reply {
    /// The repository the request was made for.
    pub repo: PathBuf,
    pub request: Request,
    pub result: forge::Result<Payload>,
}

/// Run `request` on a background thread and send the answer back. Without a
/// `token` typed by the user, the sign-in Git saved for the remote is used.
pub fn spawn(
    repo: PathBuf,
    remote_url: String,
    remote: Remote,
    token: Option<String>,
    request: Request,
    tx: Sender<Reply>,
    wake: impl FnOnce() + Send + 'static,
) {
    std::thread::spawn(move || {
        let result = (|| {
            let provider = remote.provider().ok_or_else(|| forge::Error {
                kind: forge::ErrorKind::Other,
                message: format!("Pull requests on {} are not supported yet.", remote.host),
            })?;
            let token = token.or_else(|| forge::saved_sign_in(&repo, &remote_url).map(|c| c.password));
            let http = forge::Http { token };
            match &request {
                Request::List(filter) => provider.list(&http, *filter).map(Payload::List),
                Request::Detail(number) => provider.detail(&http, *number).map(|d| Payload::Detail(Box::new(d))),
                Request::Review { number, event, body } => provider.review(&http, *number, *event, body).map(|()| Payload::Reviewed),
            }
        })();
        let _ = tx.send(Reply { repo, request, result });
        wake();
    });
}

// ---------------------------------------------------------------------------
// Ribbon
// ---------------------------------------------------------------------------

/// Who reviews are made as, with the buttons to change it.
fn account(ui: &mut egui::Ui, pulls: &Pulls, actions: &mut Vec<Action>) {
    let button = theme::ribbon_button;
    if !pulls.supported() {
        return;
    }
    let label = if pulls.viewer.is_some() || pulls.token.is_some() { "Change account" } else { "Sign in" };
    if button(ui, "👤", label, true, pulls.viewer.is_none())
        .on_hover_text("Use an access token for pull requests. It is kept for this session only.")
        .clicked()
    {
        actions.push(Action::AskToken);
    }
    ui.add_space(6.0);
    match &pulls.viewer {
        Some(viewer) => {
            ui.label(RichText::new(format!("Reviewing as @{viewer} on {}", pulls.host())).strong())
                .on_hover_text("Reviews and comments are made as this account. Check it is the right one for this repository.");
        }
        None => {
            ui.label(RichText::new("Not signed in").weak());
        }
    }
}

pub fn list_ribbon(ui: &mut egui::Ui, pulls: &Pulls, actions: &mut Vec<Action>) {
    let button = theme::ribbon_button;
    let supported = pulls.supported();
    let loading = matches!(pulls.list, Load::Loading);
    if button(ui, "🔄", "Refresh", supported && !loading, false).on_hover_text("Read the pull requests again").clicked() {
        actions.push(Action::ReloadPulls);
    }
    ui.separator();
    for (filter, icon, label) in [(Filter::Open, "🔀", "Open"), (Filter::Closed, "✔", "Closed"), (Filter::All, "☰", "All")] {
        if button(ui, icon, label, supported, pulls.filter == filter).clicked() && pulls.filter != filter {
            actions.push(Action::SetPullFilter(filter));
        }
    }
    ui.separator();
    if let Some(provider) = pulls.remote.as_ref().and_then(|r| r.provider()) {
        if button(ui, "🌐", "Open on host", true, false).on_hover_text("Show the pull requests in your browser").clicked() {
            actions.push(Action::OpenUrl(provider.web_url()));
        }
    }
    account(ui, pulls, actions);
    ui.separator();
}

pub fn pull_ribbon(ui: &mut egui::Ui, tab: &PullTab, pulls: &Pulls, actions: &mut Vec<Action>) {
    let button = theme::ribbon_button;
    let detail = match &tab.detail {
        Load::Ready(d) => Some(d),
        _ => None,
    };
    let open = detail.is_some_and(|d| d.summary.state == PullState::Open);
    let can_review = open && !tab.sending && !tab.loading && pulls.viewer.is_some();
    let why_not = if detail.is_none() {
        "The pull request is still loading"
    } else if !open {
        "This pull request is not open any more"
    } else if pulls.viewer.is_none() {
        "Sign in first"
    } else {
        ""
    };
    for (event, icon, hint) in [
        (ReviewEvent::Approve, "✔", "Say the changes are good to merge"),
        (ReviewEvent::RequestChanges, "✖", "Reject for now: ask the author for changes before it can be merged"),
        (ReviewEvent::Comment, "💬", "Leave a comment without approving or rejecting"),
    ] {
        let resp = button(ui, icon, event.label(), can_review, event == ReviewEvent::Approve);
        if resp.on_hover_text(hint).on_disabled_hover_text(why_not).clicked() {
            actions.push(Action::AskReview(tab.number, event));
        }
    }
    ui.separator();
    if button(ui, "🔄", "Refresh", !tab.loading, false).on_hover_text("Read this pull request again").clicked() {
        actions.push(Action::ReloadPull(tab.number));
    }
    if let Some(d) = detail {
        if button(ui, "🌐", "Open on host", true, false).on_hover_text("Show this pull request in your browser").clicked() {
            actions.push(Action::OpenUrl(d.summary.url.clone()));
        }
    }
    account(ui, pulls, actions);
    ui.separator();
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

fn state_chip(ui: &mut egui::Ui, state: PullState, draft: bool) {
    let (text, color) = match (state, draft) {
        (PullState::Open, true) => ("Draft", ui.visuals().weak_text_color()),
        (PullState::Open, false) => ("Open", Color32::from_rgb(63, 160, 80)),
        (PullState::Merged, _) => ("Merged", Color32::from_rgb(140, 110, 220)),
        (PullState::Closed, _) => ("Closed", theme::RED),
    };
    chip(ui, text, color);
}

fn chip(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .stroke(Stroke::new(1.0, color))
        .fill(color.gamma_multiply(0.15))
        .corner_radius(9)
        .inner_margin(egui::Margin::symmetric(8, 1))
        .show(ui, |ui| ui.label(RichText::new(text).small().color(color)));
}

/// A centred message with optional buttons under it.
fn notice(ui: &mut egui::Ui, title: &str, text: &str, buttons: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.set_max_width(560.0);
        ui.label(RichText::new(title).size(18.0));
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(text).weak()).wrap());
        ui.add_space(10.0);
        buttons(ui);
    });
}

fn failure(ui: &mut egui::Ui, error: &forge::Error, retry: Action, actions: &mut Vec<Action>) {
    let auth = error.kind == forge::ErrorKind::Auth;
    let title = if auth { "Sign in to see this" } else { "Could not read from the host" };
    notice(ui, title, &error.message, |ui| {
        ui.horizontal(|ui| {
            // Centre the two buttons.
            ui.add_space((ui.available_width() - 190.0).max(0.0) / 2.0);
            if auth && theme::primary(ui, "Sign in…").clicked() {
                actions.push(Action::AskToken);
            }
            if ui.button("Try again").clicked() {
                actions.push(retry);
            }
        });
    });
}

pub fn list_page(ui: &mut egui::Ui, pulls: &Pulls, actions: &mut Vec<Action>) {
    egui::Frame::new().inner_margin(egui::Margin::same(16)).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        let Some(remote) = &pulls.remote else {
            notice(ui, "No Git host", "This repository has no remote on a Git host, so there are no pull requests to show.", |_| {});
            return;
        };
        if !pulls.supported() {
            let text = format!(
                "gitsot can review pull requests on GitHub in this version. {} ({}) is not supported yet; \
                 GitLab, Gitea / Forgejo and Bitbucket are planned.",
                remote.host,
                remote.kind.name()
            );
            notice(ui, "Pull requests are not available for this host yet", &text, |ui| {
                if remote.kind == Kind::Unknown
                    && ui.button(format!("{} is a GitHub Enterprise server", remote.host)).on_hover_text("gitsot will use the GitHub API on this host from now on").clicked()
                {
                    actions.push(Action::TreatAsGitHub(remote.host.clone()));
                }
            });
            return;
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("Pull requests").size(20.0).strong());
            ui.label(RichText::new(remote.display()).weak());
            if matches!(pulls.list, Load::Loading) {
                ui.spinner();
            }
        });
        ui.add_space(8.0);

        match &pulls.list {
            Load::Idle | Load::Loading => {}
            Load::Failed(e) => failure(ui, e, Action::ReloadPulls, actions),
            Load::Ready(list) if list.is_empty() => {
                let what = match pulls.filter {
                    Filter::Open => "No open pull requests",
                    Filter::Closed => "No closed pull requests",
                    Filter::All => "No pull requests",
                };
                notice(ui, what, "Use Open, Closed and All in the ribbon to see others.", |_| {});
            }
            Load::Ready(list) => {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for pull in list {
                        if list_row(ui, pull).clicked() {
                            actions.push(Action::OpenPull { number: pull.number, title: pull.title.clone() });
                        }
                    }
                    if list.len() >= forge::LIST_LIMIT {
                        ui.add_space(6.0);
                        ui.label(RichText::new(format!("Showing the {} most recently updated.", forge::LIST_LIMIT)).small().weak());
                    }
                });
            }
        }
    });
}

fn list_row(ui: &mut egui::Ui, pull: &PullSummary) -> egui::Response {
    let p = theme::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 50.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let painter = ui.painter().with_clip_rect(rect.shrink2(Vec2::new(8.0, 0.0)));
    let (state, color) = match (pull.state, pull.draft) {
        (PullState::Open, true) => ("Draft", p.text_dim),
        (PullState::Open, false) => ("Open", Color32::from_rgb(63, 160, 80)),
        (PullState::Merged, _) => ("Merged", Color32::from_rgb(140, 110, 220)),
        (PullState::Closed, _) => ("Closed", theme::RED),
    };
    let number = painter.text(
        rect.left_top() + Vec2::new(10.0, 7.0),
        Align2::LEFT_TOP,
        format!("#{}", pull.number),
        FontId::proportional(14.0),
        p.text_dim,
    );
    painter.text(
        egui::pos2(number.right() + 8.0, rect.top() + 7.0),
        Align2::LEFT_TOP,
        &pull.title,
        FontId::proportional(14.0),
        ui.visuals().strong_text_color(),
    );
    let state_rect = painter.text(rect.left_top() + Vec2::new(10.0, 29.0), Align2::LEFT_TOP, state, FontId::proportional(12.0), color);
    painter.text(
        egui::pos2(state_rect.right() + 8.0, rect.top() + 29.0),
        Align2::LEFT_TOP,
        format!("{} wants to merge {} into {}  ·  updated {}", pull.author, pull.head, pull.base, relative_time(pull.updated)),
        FontId::proportional(12.0),
        p.text_dim,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn pull_page(ui: &mut egui::Ui, tab: &mut PullTab, _pulls: &Pulls, actions: &mut Vec<Action>) {
    let detail = match &tab.detail {
        Load::Ready(detail) => detail,
        Load::Failed(e) => {
            failure(ui, e, Action::ReloadPull(tab.number), actions);
            return;
        }
        Load::Idle | Load::Loading => {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.label(RichText::new(format!("Reading pull request #{}…", tab.number)).weak());
            });
            return;
        }
    };
    let s = &detail.summary;

    egui::Panel::left("pull_info")
        .resizable(true)
        .default_size(390.0)
        .min_size(260.0)
        .frame(egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 10, top: 10, bottom: 8 }))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(&s.title).size(18.0).strong()).wrap());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("#{}", s.number)).weak());
                state_chip(ui, s.state, s.draft);
                if tab.loading || tab.sending {
                    ui.spinner();
                }
            });
            ui.add(egui::Label::new(RichText::new(format!("{} wants to merge {} into {}", s.author, s.head, s.base)).weak()).wrap());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("+{}", detail.additions)).color(Color32::from_rgb(63, 160, 80)));
                ui.label(RichText::new(format!("−{}", detail.deletions)).color(theme::RED));
                ui.label(RichText::new(format!("{} file(s)  ·  updated {}", detail.files.len(), relative_time(s.updated))).weak());
            });
            ui.separator();
            egui::ScrollArea::vertical().id_salt(("pull_conversation", tab.number)).auto_shrink(false).show(ui, |ui| {
                theme::column_header(ui, "Description");
                let body = strip_comments(&detail.body);
                if body.trim().is_empty() {
                    ui.label(RichText::new("No description.").weak());
                } else {
                    ui.add(egui::Label::new(body.trim()).wrap());
                }
                ui.add_space(12.0);
                theme::column_header(ui, "Reviews and comments");
                if detail.notes.is_empty() {
                    ui.label(RichText::new("Nothing yet.").weak());
                }
                for note in &detail.notes {
                    theme::card_frame(ui, false).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&note.author).strong());
                            match note.review {
                                Some(ReviewState::Approved) => chip(ui, "Approved", Color32::from_rgb(63, 160, 80)),
                                Some(ReviewState::ChangesRequested) => chip(ui, "Changes requested", theme::RED),
                                Some(ReviewState::Dismissed) => chip(ui, "Dismissed", ui.visuals().weak_text_color()),
                                Some(ReviewState::Pending) => chip(ui, "Pending", theme::ORANGE),
                                Some(ReviewState::Commented) | None => {}
                            }
                            ui.label(RichText::new(relative_time(note.time)).small().weak()).on_hover_text(full_time(note.time));
                        });
                        let body = strip_comments(&note.body);
                        if !body.trim().is_empty() {
                            ui.add(egui::Label::new(body.trim()).wrap());
                        }
                    });
                }
            });
        });

    egui::Panel::left("pull_files")
        .resizable(true)
        .default_size(250.0)
        .min_size(140.0)
        .frame(egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 8, top: 10, bottom: 8 }))
        .show(ui, |ui| {
            theme::column_header(ui, &format!("Files · {}", detail.files.len()));
            ui.add_space(4.0);
            egui::ScrollArea::vertical().id_salt(("pull_files_list", tab.number)).auto_shrink(false).show(ui, |ui| {
                for (i, file) in detail.files.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            // Room for the scroll bar.
                            ui.add_space(8.0);
                            kind_badge(ui, file.kind);
                            ui.label(RichText::new(format!("+{} −{}", file.additions, file.deletions)).small().weak());
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                if file_row(ui, tab.selected == i, &file.path).clicked() {
                                    tab.selected = i;
                                }
                            });
                        });
                    });
                }
                if detail.files_truncated {
                    ui.label(RichText::new(format!("Only the first {} files are shown.", detail.files.len())).small().weak());
                }
            });
        });

    egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(egui::Margin::same(10))).show(ui, |ui| {
        match detail.files.get(tab.selected) {
            Some(file) => {
                ui.horizontal(|ui| {
                    kind_badge(ui, file.kind);
                    ui.add(egui::Label::new(RichText::new(&file.path).strong().monospace()).truncate());
                });
                ui.separator();
                // Each file keeps its own scroll position.
                diff_view(ui, &file.diff, &format!("pull_diff/{}/{}", tab.number, file.path));
            }
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("This pull request changes no files").weak());
                });
            }
        }
    });
}

/// Descriptions are shown as plain text; hidden `<!-- … -->` notes are left out.
fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Dialogs. They return `true` when they should close.
// ---------------------------------------------------------------------------

pub fn review_dialog(ui: &mut egui::Ui, number: u64, event: ReviewEvent, body: &mut String, pulls: &Pulls, actions: &mut Vec<Action>) -> bool {
    let mut close = false;
    ui.heading(match event {
        ReviewEvent::Approve => format!("Approve pull request #{number}"),
        ReviewEvent::RequestChanges => format!("Request changes on pull request #{number}"),
        ReviewEvent::Comment => format!("Comment on pull request #{number}"),
    });
    ui.add_space(4.0);
    ui.label(match event {
        ReviewEvent::Approve => "Tells the author the changes are good to merge.",
        ReviewEvent::RequestChanges => "Rejects the pull request for now: the author has to make changes before it can be merged.",
        ReviewEvent::Comment => "Adds your comment without approving or rejecting.",
    });
    ui.add_space(6.0);
    // The account matters as much as the commit identity does.
    match &pulls.viewer {
        Some(viewer) => {
            ui.horizontal_wrapped(|ui| {
                ui.label("You are reviewing as");
                ui.label(RichText::new(format!("@{viewer}")).strong().color(theme::pal(ui).accent_text));
                ui.label(format!("on {}.", pulls.host()));
            });
        }
        None => {
            ui.label(RichText::new("You are not signed in.").color(theme::ORANGE));
        }
    }
    ui.add_space(6.0);
    let hint = if event.needs_body() { "Comment (required)" } else { "Comment (optional)" };
    egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
        ui.add(TextEdit::multiline(body).hint_text(hint).desired_rows(5).desired_width(f32::INFINITY));
    });
    ui.label(RichText::new("Everyone who can see the pull request can read this.").small().weak());
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        let ok = pulls.viewer.is_some() && (!event.needs_body() || !body.trim().is_empty());
        if theme::primary_enabled(ui, ok, event.label()).clicked() {
            actions.push(Action::SubmitReview { number, event, body: body.clone() });
        }
        if ui.button("Cancel").clicked() {
            close = true;
        }
    });
    close
}

pub fn token_dialog(ui: &mut egui::Ui, token: &mut String, pulls: &Pulls, actions: &mut Vec<Action>) -> bool {
    let mut close = false;
    ui.heading(format!("Sign in to {} for pull requests", pulls.host()));
    ui.add_space(4.0);
    let http = pulls.remote.as_ref().is_some_and(|r| r.http);
    ui.add(
        egui::Label::new(if http {
            "gitsot uses the sign-in Git has saved for this remote, so reviews are made as the account that pushes. \
             Enter an access token here to use another account, or when nothing is saved."
        } else {
            "This repository uses SSH, and an SSH key cannot be used for pull requests. \
             Enter an access token of the account that should review."
        })
        .wrap(),
    );
    ui.add_space(8.0);
    ui.label("Access token");
    let resp = ui.add(TextEdit::singleline(token).password(true).desired_width(f32::INFINITY));
    if token.is_empty() && !resp.has_focus() {
        resp.request_focus();
    }
    ui.add(
        egui::Label::new(
            RichText::new(
                "The token needs access to pull requests (on GitHub: the repo scope, or Pull requests: read and write). \
                 It is kept in memory until you close gitsot or open another repository. It is never saved.",
            )
            .small()
            .weak(),
        )
        .wrap(),
    );
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let ok = !token.trim().is_empty();
        if theme::primary_enabled(ui, ok, "Sign in").clicked() || (enter && ok) {
            actions.push(Action::SetToken(Some(token.clone())));
        }
        if pulls.token.is_some() && ui.button("Forget the token").on_hover_text("Go back to the sign-in Git has saved").clicked() {
            actions.push(Action::SetToken(None));
        }
        if ui.button("Cancel").clicked() {
            close = true;
        }
    });
    close
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enterprise_hosts_become_github() {
        let pulls = Pulls::new(Some("https://git.company.example/team/app.git"), &[]);
        assert!(!pulls.supported());
        let pulls = Pulls::new(Some("https://git.company.example/team/app.git"), &["git.company.example".to_string()]);
        assert!(pulls.supported());
        assert!(Pulls::new(Some("git@github.com:a/b.git"), &[]).supported());
        assert!(Pulls::new(None, &[]).remote.is_none());
    }

    #[test]
    fn hidden_notes_are_left_out() {
        assert_eq!(strip_comments("<!-- a -->Hello<!-- b\nc --> there"), "Hello there");
        assert_eq!(strip_comments("Open <!-- never closed"), "Open ");
        assert_eq!(strip_comments("plain"), "plain");
    }

    #[test]
    fn long_titles_are_shortened() {
        use crate::app::shorten;
        assert_eq!(shorten("short", 10), "short");
        assert_eq!(shorten("a very long title", 8), "a very …");
    }
}
