//! The app's state and what each key does. Nothing here touches the
//! terminal, so tests drive it with key events and read the screen from a
//! test backend.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

use crate::agents::Env;
use crate::cache;
use crate::github::{self, Spec};
use crate::library::{FoundServer, FoundSkill, Report, SkillEntry, Source, Store, UpdateOutcome};
use crate::market::{self, MarketServer, MarketSkill};
use crate::mcp::{Server, Transport};
use crate::skills::{self, Found};
use crate::theme::{Mode, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Mcp,
    Skills,
    Discover,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Mcp, Tab::Skills, Tab::Discover];
    fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Agents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mcp,
    Skills,
}

/// A row of the MCP or Skills list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Entry(String),
    /// Heads the skills from one place: how many the list shows, and
    /// whether they're folded away under it.
    Group {
        label: String,
        n: usize,
        folded: bool,
    },
    Header,
    Found(String),
}

impl Row {
    fn selectable(&self) -> bool {
        *self != Row::Header
    }
}

/// A group of more skills than this starts folded.
const FOLD_OVER: usize = 10;

/// The selectable row nearest `i`, looking forward or back first.
fn nearest(rows: &[Row], i: usize, forward: bool) -> usize {
    let after = || (i..rows.len()).find(|&j| rows[j].selectable());
    let before = || {
        (0..=i.min(rows.len() - 1))
            .rev()
            .find(|&j| rows[j].selectable())
    };
    let found = if forward {
        after().or_else(before)
    } else {
        before().or_else(after)
    };
    found.unwrap_or(i)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Ok,
    Err,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub at: Instant,
}

/// A one-line text box; the cursor is always at the end.
#[derive(Debug, Clone, Default)]
pub struct Input {
    pub text: String,
}

impl Input {
    pub fn new(s: impl Into<String>) -> Self {
        Input { text: s.into() }
    }

    /// Applies an editing key; false for a key it doesn't take.
    pub fn handle(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('u') if ctrl => self.text.clear(),
            KeyCode::Char('w') if ctrl => {
                let t = self.text.trim_end();
                let cut = t
                    .rfind(|c: char| c.is_whitespace() || c == '/' || c == ',')
                    .map_or(0, |i| i + 1);
                self.text.truncate(cut);
            }
            KeyCode::Char(c) if !ctrl => self.text.push(c),
            KeyCode::Backspace => {
                self.text.pop();
            }
            _ => return false,
        }
        true
    }
}

pub struct Field {
    pub label: String,
    pub input: Input,
    pub hint: String,
    pub secret: bool,
    /// A field that picks one of these; empty for free text.
    pub choices: Vec<&'static str>,
}

impl Field {
    fn text(label: &str, value: impl Into<String>, hint: impl Into<String>) -> Self {
        Field {
            label: label.to_string(),
            input: Input::new(value),
            hint: hint.into(),
            secret: false,
            choices: vec![],
        }
    }
}

pub enum FormPurpose {
    AddServer,
    EditServer(String),
    MarketInputs(Box<MarketServer>),
    AddSkills,
}

pub struct Form {
    pub title: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub purpose: FormPurpose,
    pub error: Option<String>,
}

impl Form {
    /// The fields shown: a server's command fields or its URL fields.
    pub fn visible(&self) -> Vec<usize> {
        match self.purpose {
            FormPurpose::AddServer | FormPurpose::EditServer(_) => {
                if self.fields[1].input.text == "stdio" {
                    vec![0, 1, 2, 3, 4]
                } else {
                    vec![0, 1, 5, 6]
                }
            }
            _ => (0..self.fields.len()).collect(),
        }
    }

    fn step(&mut self, d: isize) {
        let vis = self.visible();
        let at = vis.iter().position(|i| *i == self.focus).unwrap_or(0) as isize;
        self.focus = vis[(at + d).rem_euclid(vis.len() as isize) as usize];
    }

    fn last(&self) -> bool {
        self.visible().last() == Some(&self.focus)
    }
}

pub struct PickItem {
    pub label: String,
    pub detail: String,
    pub checked: bool,
    pub blocked: Option<String>,
}

pub enum PickPurpose {
    InstallSkills {
        _guard: Option<Arc<TempDir>>,
        found: Vec<Found>,
        origin: SkillOrigin,
    },
}

#[derive(Clone)]
pub enum SkillOrigin {
    Github { repo: String, reference: String },
    Local,
}

pub struct Picker {
    pub title: String,
    pub items: Vec<PickItem>,
    pub sel: usize,
    pub purpose: PickPurpose,
}

pub enum ConfirmAction {
    DeleteServer(String),
    DeleteSkill(String),
    /// Every server or skill in the library.
    DeleteAll(Kind),
    ImportSkill(String),
}

pub struct Confirm {
    pub title: String,
    pub body: String,
    pub action: ConfirmAction,
}

pub enum Modal {
    Form(Form),
    Picker(Picker),
    Confirm(Confirm),
    Help,
}

pub struct Discover {
    pub kind: Kind,
    pub query: Input,
    pub editing: bool,
    pub servers: Vec<MarketServer>,
    pub skills: Vec<MarketSkill>,
    pub sel: [usize; 2],
    pub loading: [bool; 2],
    pub error: Option<String>,
    /// The query each list shows results for.
    pub shown: [Option<String>; 2],
    pub abouts: HashMap<String, Option<String>>,
    asked: HashSet<String>,
    about_inflight: usize,
    abouts_dirty: Option<Instant>,
    sel_at: Instant,
    /// When the query last changed, for the search to run once typing stops.
    typed_at: Option<Instant>,
    /// skills.sh's most installed skills, searched locally while it answers.
    pub index: Vec<MarketSkill>,
    index_fresh: bool,
    index_loading: bool,
    /// skills.sh's answers to the searches made this session.
    searches: HashMap<String, Vec<MarketSkill>>,
}

impl Discover {
    fn k(&self) -> usize {
        self.kind as usize
    }
}

pub enum Msg {
    Registry {
        query: String,
        result: Result<Vec<MarketServer>, String>,
    },
    Skills {
        query: String,
        result: Result<Vec<MarketSkill>, String>,
    },
    Popular {
        result: Result<Vec<MarketSkill>, String>,
    },
    About {
        key: String,
        about: Option<String>,
    },
    Tree {
        key: String,
        result: Result<(TempDir, PathBuf), String>,
    },
}

/// What a downloaded repository is wanted for.
pub enum TreeUse {
    Browse {
        repo: String,
        reference: String,
        subpath: String,
    },
    Market(MarketSkill),
    Update {
        names: Vec<String>,
    },
}

/// A repository's files, downloaded this session.
struct Tree {
    guard: Arc<TempDir>,
    root: PathBuf,
    at: Instant,
}

/// How long a downloaded repository is used again for installs.
const TREE_TTL: Duration = Duration::from_secs(600);
const POPULAR_TTL: Duration = Duration::from_secs(6 * 3600);
const ABOUT_PARALLEL: usize = 4;

struct Done {
    label: String,
    msg: Msg,
}

pub struct App {
    pub store: Store,
    pub env: Env,
    pub tab: Tab,
    pub focus: Focus,
    /// Selected row of the MCP and Skills lists.
    pub sel: [usize; 2],
    pub agent_sel: usize,
    pub filter: [Input; 2],
    pub filtering: bool,
    /// Groups of skills folded (true) or opened (false) by hand.
    pub folds: BTreeMap<String, bool>,
    pub found_servers: Vec<FoundServer>,
    pub found_skills: Vec<FoundSkill>,
    pub discover: Discover,
    pub modal: Option<Modal>,
    pub toast: Option<Toast>,
    pub busy: Vec<String>,
    pub quit: bool,
    pub frame: usize,
    pub theme: Theme,
    /// Never reach the network (tests).
    pub offline: bool,
    trees: HashMap<String, Tree>,
    /// Quiet jobs still running.
    quiet: usize,
    /// Repositories being downloaded, and what each is waited on for.
    fetching: HashSet<String>,
    waiting: Vec<(String, TreeUse)>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
}

impl App {
    pub fn new(store: Store, env: Env) -> Self {
        let (tx, rx) = channel();
        let mut app = App {
            store,
            env,
            tab: Tab::Mcp,
            focus: Focus::List,
            sel: [0, 0],
            agent_sel: 0,
            filter: [Input::default(), Input::default()],
            filtering: false,
            folds: BTreeMap::new(),
            found_servers: vec![],
            found_skills: vec![],
            discover: Discover {
                kind: Kind::Mcp,
                query: Input::default(),
                editing: false,
                servers: market::featured(),
                skills: vec![],
                sel: [0, 0],
                loading: [false, false],
                error: None,
                shown: [Some(String::new()), None],
                abouts: HashMap::new(),
                asked: HashSet::new(),
                about_inflight: 0,
                abouts_dirty: None,
                sel_at: Instant::now(),
                typed_at: None,
                index: vec![],
                index_fresh: false,
                index_loading: false,
                searches: HashMap::new(),
            },
            modal: None,
            toast: None,
            busy: vec![],
            quit: false,
            frame: 0,
            theme: Theme::DARK,
            offline: false,
            trees: HashMap::new(),
            quiet: 0,
            fetching: HashSet::new(),
            waiting: vec![],
            tx,
            rx,
        };
        app.rescan();
        let cache = app.cache_dir();
        if let Some(c) = cache::read(&cache, "skill-folds", Duration::MAX) {
            app.folds = c.data;
        }
        app.clamp();
        if let Some(c) = cache::read::<Vec<MarketSkill>>(&cache, "skills-popular", POPULAR_TTL) {
            app.discover.index = c.data;
            app.discover.index_fresh = c.fresh;
        }
        if let Some(c) = cache::read(&cache, "skills-about", Duration::MAX) {
            app.discover.abouts = c.data;
        }
        app
    }

    /// Starts what's worth having before it's asked for: the popular
    /// skills, when the cached list is old.
    pub fn start(&mut self) {
        if !self.discover.index_fresh {
            self.fetch_popular();
        }
    }

    /// Saves what's only kept in memory.
    pub fn shutdown(&mut self) {
        if self.discover.abouts_dirty.take().is_some() {
            cache::write(&self.cache_dir(), "skills-about", &self.discover.abouts);
        }
    }

    fn cache_dir(&self) -> PathBuf {
        self.store.root.join("cache")
    }

    // ---- background work ----------------------------------------------------

    fn spawn(&mut self, label: impl Into<String>, f: impl FnOnce() -> Msg + Send + 'static) {
        let label = label.into();
        self.busy.push(label.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Done { label, msg: f() });
        });
    }

    /// Work in the background that the status bar doesn't announce.
    fn spawn_quiet(&mut self, f: impl FnOnce() -> Msg + Send + 'static) {
        self.quiet += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Done {
                label: String::new(),
                msg: f(),
            });
        });
    }

    /// Handles finished background work; true when anything finished.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok(done) = self.rx.try_recv() {
            self.finish(done);
            any = true;
        }
        any
    }

    fn finish(&mut self, done: Done) {
        if done.label.is_empty() {
            self.quiet = self.quiet.saturating_sub(1);
        } else if let Some(i) = self.busy.iter().position(|l| *l == done.label) {
            self.busy.remove(i);
        }
        self.on_msg(done.msg);
    }

    /// Waits for every background job, quiet ones too (tests).
    #[cfg(test)]
    pub fn settle(&mut self) {
        while !self.busy.is_empty() || self.quiet > 0 {
            match self.rx.recv_timeout(Duration::from_secs(120)) {
                Ok(done) => self.finish(done),
                Err(_) => break,
            }
        }
    }

    /// Called every frame: animates, expires the toast and fetches the
    /// description of a skill the cursor has rested on.
    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        if let Some(t) = &self.toast {
            let ttl = if t.kind == ToastKind::Err { 8 } else { 4 };
            if t.at.elapsed() > Duration::from_secs(ttl) {
                self.toast = None;
            }
        }
        // skills.sh answers in a second or two whatever is asked, so it's
        // asked sooner; the registry's list narrows locally meanwhile
        let wait = if self.discover.kind == Kind::Skills {
            250
        } else {
            450
        };
        if self
            .discover
            .typed_at
            .is_some_and(|t| t.elapsed() > Duration::from_millis(wait))
        {
            self.discover.typed_at = None;
            self.run_search();
        }
        if self
            .discover
            .abouts_dirty
            .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
        {
            self.discover.abouts_dirty = None;
            cache::write(&self.cache_dir(), "skills-about", &self.discover.abouts);
        }
        if self.tab == Tab::Discover && self.discover.kind == Kind::Skills && !self.offline {
            self.fetch_abouts();
            self.prefetch_selected();
        }
    }

    /// Fetches the descriptions of the selected skill and the ones near
    /// it, a few at a time.
    fn fetch_abouts(&mut self) {
        let d = &self.discover;
        let sel = d.sel[1];
        let near = d.skills.iter().skip(sel.saturating_sub(2)).take(12);
        let wanted: Vec<MarketSkill> = d
            .skills
            .get(sel)
            .into_iter()
            .chain(near)
            .filter(|s| !d.abouts.contains_key(&s.key()) && !d.asked.contains(&s.key()))
            .cloned()
            .collect();
        for s in wanted {
            if self.discover.about_inflight >= ABOUT_PARALLEL {
                break;
            }
            let key = s.key();
            if !self.discover.asked.insert(key.clone()) {
                continue;
            }
            self.discover.about_inflight += 1;
            self.spawn_quiet(move || Msg::About {
                key,
                about: market::skill_about(&s),
            });
        }
    }

    /// Downloads the repository of a skill the cursor rests on, so that
    /// installing it takes no wait.
    fn prefetch_selected(&mut self) {
        let d = &self.discover;
        if d.editing || d.sel_at.elapsed() < Duration::from_millis(900) || !self.fetching.is_empty()
        {
            return;
        }
        let Some(s) = d.skills.get(d.sel[1]).cloned() else {
            return;
        };
        let key = tree_key(&s.source, "HEAD");
        if self.have_skill(&s).is_none()
            && self.skill_name_taken(&s).is_none()
            && !self.trees.contains_key(&key)
        {
            self.request_tree(&s.source, "HEAD", None, false);
        }
    }

    fn fetch_popular(&mut self) {
        if self.offline || self.discover.index_loading {
            return;
        }
        self.discover.index_loading = true;
        self.spawn_quiet(|| Msg::Popular {
            result: market::popular_skills().map_err(|e| format!("{e:#}")),
        });
    }

    /// Gets a repository's files for `use_`: from this session's downloads
    /// when `reuse` and one is recent, else joining or starting a download.
    /// With no use it's a prefetch, done quietly.
    fn request_tree(&mut self, repo: &str, reference: &str, use_: Option<TreeUse>, reuse: bool) {
        let key = tree_key(repo, reference);
        if reuse
            && let Some(t) = self.trees.get(&key)
            && t.at.elapsed() < TREE_TTL
        {
            if let Some(u) = use_ {
                let (guard, root) = (t.guard.clone(), t.root.clone());
                self.use_tree(u, guard, root);
            }
            return;
        }
        if let Some(u) = use_ {
            self.waiting.push((key.clone(), u));
            // shown even when a quiet prefetch of it is already under way
            let label = tree_label(&key);
            if !self.offline && !self.busy.contains(&label) {
                self.busy.push(label);
            }
        }
        if self.offline || !self.fetching.insert(key.clone()) {
            return;
        }
        let (repo, reference) = (repo.to_string(), reference.to_string());
        self.spawn_quiet(move || Msg::Tree {
            result: github::download(&repo, &reference).map_err(|e| format!("{e:#}")),
            key,
        });
    }

    fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Registry { query, result } => {
                self.discover.loading[0] = false;
                if self.discover.query.text.trim() != query {
                    return;
                }
                match result {
                    Ok(list) => {
                        self.discover.servers.extend(list);
                        self.discover.error = None;
                    }
                    Err(e) => self.discover.error = Some(format!("MCP Registry: {e}")),
                }
            }
            Msg::Skills { query, result } => {
                let current = self.discover.query.text.trim() == query;
                if current {
                    self.discover.loading[1] = false;
                }
                match result {
                    Ok(list) => {
                        self.discover.searches.insert(query.clone(), list);
                        if current {
                            self.show_skills(&query);
                            self.discover.error = None;
                        }
                    }
                    Err(e) if current => self.discover.error = Some(format!("skills.sh: {e}")),
                    Err(_) => {}
                }
            }
            Msg::Popular { result } => {
                self.discover.index_loading = false;
                // the first page of skills is waiting on it
                let waited =
                    self.discover.index.is_empty() && self.discover.shown[1].as_deref() == Some("");
                match result {
                    Ok(list) if !list.is_empty() => {
                        cache::write(&self.cache_dir(), "skills-popular", &list);
                        self.discover.index = list;
                        self.discover.index_fresh = true;
                        if self.discover.shown[1].is_some() {
                            let q = self.discover.query.text.trim().to_string();
                            if q.is_empty() {
                                self.discover.loading[1] = false;
                            }
                            self.show_skills(&q);
                        }
                    }
                    Ok(_) if waited => {
                        self.discover.loading[1] = false;
                        self.discover.error =
                            Some("skills.sh: its page has no list of skills".into());
                    }
                    Err(e) if waited => {
                        self.discover.loading[1] = false;
                        self.discover.error = Some(format!("skills.sh: {e}"));
                    }
                    _ => {}
                }
            }
            Msg::About { key, about } => {
                self.discover.about_inflight = self.discover.about_inflight.saturating_sub(1);
                // a failed fetch is tried again next session, not remembered
                if about.is_some() {
                    self.discover.abouts.insert(key, about);
                    self.discover.abouts_dirty.get_or_insert_with(Instant::now);
                }
            }
            Msg::Tree { key, result } => {
                self.fetching.remove(&key);
                let label = tree_label(&key);
                self.busy.retain(|l| *l != label);
                let (uses, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
                    .into_iter()
                    .partition(|(k, _)| *k == key);
                self.waiting = rest;
                match result {
                    Err(e) => {
                        if !uses.is_empty() {
                            self.notify(ToastKind::Err, e);
                        }
                    }
                    Ok((guard, root)) => {
                        let guard = Arc::new(guard);
                        self.trees.retain(|_, t| t.at.elapsed() < TREE_TTL);
                        self.trees.insert(
                            key,
                            Tree {
                                guard: guard.clone(),
                                root: root.clone(),
                                at: Instant::now(),
                            },
                        );
                        for (_, u) in uses {
                            self.use_tree(u, guard.clone(), root.clone());
                        }
                    }
                }
            }
        }
    }

    fn use_tree(&mut self, use_: TreeUse, guard: Arc<TempDir>, root: PathBuf) {
        match use_ {
            TreeUse::Browse {
                repo,
                reference,
                subpath,
            } => {
                let found: Vec<Found> = skills::find(&root)
                    .into_iter()
                    .filter(|f| {
                        subpath.is_empty()
                            || f.path == subpath
                            || f.path.starts_with(&format!("{subpath}/"))
                    })
                    .collect();
                if found.is_empty() {
                    self.notify(
                        ToastKind::Err,
                        format!(
                            "No SKILL.md found in {repo}{}",
                            if subpath.is_empty() {
                                String::new()
                            } else {
                                format!("/{subpath}")
                            }
                        ),
                    );
                    return;
                }
                self.open_skill_picker(
                    format!("github.com/{repo}"),
                    found,
                    Some(guard),
                    SkillOrigin::Github { repo, reference },
                );
            }
            TreeUse::Market(s) => {
                if self.have_skill(&s).is_some() {
                    return; // asked twice while it was downloading
                }
                let found = skills::find(&root);
                let hit = found
                    .iter()
                    .find(|f| skills::last_part(&f.path) == s.skill_id)
                    .or_else(|| {
                        found.iter().find(|f| {
                            f.meta.name == s.skill_id || f.name() == skills::sanitize(&s.skill_id)
                        })
                    });
                let Some(f) = hit.cloned() else {
                    self.notify(
                        ToastKind::Err,
                        format!("{} has no skill {} any more", s.source, s.skill_id),
                    );
                    return;
                };
                let origin = SkillOrigin::Github {
                    repo: s.source.clone(),
                    reference: "HEAD".into(),
                };
                self.install_found(&[f], &origin);
            }
            TreeUse::Update { names } => {
                let (mut updated, mut same, mut errors) = (vec![], 0, vec![]);
                for n in &names {
                    match self.store.update_skill_from(n, &root) {
                        Ok(UpdateOutcome::Updated) => updated.push(n.clone()),
                        Ok(UpdateOutcome::UpToDate) => same += 1,
                        Err(e) => errors.push(format!("{n}: {e:#}")),
                    }
                }
                if let Err(e) = self.store.save() {
                    errors.push(format!("{e:#}"));
                }
                self.report_updates(updated, same, errors);
            }
        }
    }

    fn report_updates(&mut self, updated: Vec<String>, same: usize, errors: Vec<String>) {
        if !errors.is_empty() {
            self.notify(ToastKind::Err, errors.join(" · "));
        } else if updated.is_empty() {
            self.notify(
                ToastKind::Info,
                if same == 1 {
                    "Already up to date".into()
                } else {
                    format!("All {same} up to date")
                },
            );
        } else {
            self.notify(
                ToastKind::Ok,
                format!(
                    "Updated {}{}",
                    updated.join(", "),
                    if same > 0 {
                        format!(" · {same} up to date")
                    } else {
                        String::new()
                    }
                ),
            );
        }
    }

    pub fn notify(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            kind,
            at: Instant::now(),
        });
    }

    // ---- the library --------------------------------------------------------

    fn rescan(&mut self) {
        self.found_servers = self.store.found_servers();
        self.found_skills = self.store.found_skills();
    }

    /// Saves the library and writes it into the agents, then says how it went.
    fn commit(&mut self, ok: impl Into<String>) {
        match self.store.commit() {
            Ok(Report { issues, .. }) if !issues.is_empty() => {
                self.notify(ToastKind::Err, issues.join(" · "))
            }
            Ok(_) => {
                let ok = ok.into();
                if !ok.is_empty() {
                    self.notify(ToastKind::Ok, ok);
                }
            }
            Err(e) => self.notify(ToastKind::Err, format!("{e:#}")),
        }
        self.store.prune();
        self.rescan();
    }

    pub fn rows(&self, kind: Kind) -> Vec<Row> {
        let q = self.filter[kind as usize].text.to_lowercase();
        let hit = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        let mut rows: Vec<Row> = vec![];

        let found: Vec<String> = match kind {
            Kind::Mcp => {
                rows.extend(
                    self.store
                        .lib
                        .mcp
                        .iter()
                        .filter(|e| hit(&e.server.name) || hit(&e.server.summary()))
                        .map(|e| Row::Entry(e.server.name.clone())),
                );
                self.found_servers
                    .iter()
                    .filter(|f| hit(&f.server.name))
                    .map(|f| f.server.name.clone())
                    .collect()
            }
            Kind::Skills => {
                // by where they came from, as magpie's Library shows them:
                // headings only when there's more than one place; a filter
                // shows every match, folded or not
                let mut groups: BTreeMap<(u8, String), Vec<&SkillEntry>> = BTreeMap::new();
                for e in &self.store.lib.skills {
                    groups.entry(self.skill_group(e)).or_default().push(e);
                }
                let headed = groups.len() > 1;
                for ((_, label), mut es) in groups {
                    let folded = headed && q.is_empty() && self.folded(&label, es.len());
                    es.retain(|e| hit(&e.name) || hit(&e.description) || hit(&label));
                    if es.is_empty() {
                        continue;
                    }
                    es.sort_by(|a, b| a.name.cmp(&b.name));
                    if headed {
                        rows.push(Row::Group {
                            label,
                            n: es.len(),
                            folded,
                        });
                    }
                    if !folded {
                        rows.extend(es.into_iter().map(|e| Row::Entry(e.name.clone())));
                    }
                }
                self.found_skills
                    .iter()
                    .filter(|f| hit(&f.name))
                    .map(|f| f.name.clone())
                    .collect()
            }
        };
        if !found.is_empty() {
            rows.push(Row::Header);
            rows.extend(found.into_iter().map(Row::Found));
        }
        rows
    }

    /// Where a skill came from, as its group in the list: a GitHub
    /// repository, the folder it sat in, or the agents it was imported
    /// from; the first part orders the groups.
    pub fn skill_group(&self, e: &SkillEntry) -> (u8, String) {
        match &e.source {
            Some(Source::Github { repo, .. }) => (0, repo.clone()),
            Some(Source::Local { dir }) => (1, tilde(dir.parent().unwrap_or(dir), &self.env.home)),
            None => (2, "imported from agents".into()),
        }
    }

    fn folded(&self, label: &str, n: usize) -> bool {
        self.folds.get(label).copied().unwrap_or(n > FOLD_OVER)
    }

    /// Folds or opens a group of skills, keeping the cursor on its heading.
    fn fold(&mut self, label: &str, fold: bool) {
        if !self.filter[Kind::Skills as usize].text.is_empty() {
            self.notify(
                ToastKind::Info,
                "A filter shows every match — esc clears it",
            );
            return;
        }
        self.folds.insert(label.to_string(), fold);
        cache::write(&self.cache_dir(), "skill-folds", &self.folds);
        if let Some(i) = self
            .rows(Kind::Skills)
            .iter()
            .position(|r| matches!(r, Row::Group { label: l, .. } if l == label))
        {
            self.sel[Kind::Skills as usize] = i;
        }
    }

    /// The heading over the selected skill, when it has one.
    fn group_above(&self) -> Option<String> {
        let rows = self.rows(Kind::Skills);
        rows[..=self.sel[Kind::Skills as usize].min(rows.len().checked_sub(1)?)]
            .iter()
            .rev()
            .find_map(|r| match r {
                Row::Group { label, .. } => Some(label.clone()),
                _ => None,
            })
    }

    pub fn kind(&self) -> Option<Kind> {
        match self.tab {
            Tab::Mcp => Some(Kind::Mcp),
            Tab::Skills => Some(Kind::Skills),
            Tab::Discover => None,
        }
    }

    pub fn current(&self) -> Option<Row> {
        let k = self.kind()?;
        self.rows(k).get(self.sel[k as usize]).cloned()
    }

    fn select_name(&mut self, kind: Kind, name: &str) {
        // a skill in a folded group: open the group to show it
        let shown = |app: &Self| {
            app.rows(kind)
                .iter()
                .any(|r| matches!(r, Row::Entry(n) if n == name))
        };
        if kind == Kind::Skills
            && !shown(self)
            && let Some(e) = self.store.skill(name)
        {
            let (_, label) = self.skill_group(e);
            self.folds.insert(label, false);
            cache::write(&self.cache_dir(), "skill-folds", &self.folds);
        }
        if let Some(i) = self
            .rows(kind)
            .iter()
            .position(|r| matches!(r, Row::Entry(n) if n == name))
        {
            self.sel[kind as usize] = i;
        }
    }

    fn clamp(&mut self) {
        for k in [Kind::Mcp, Kind::Skills] {
            let rows = self.rows(k);
            let i = &mut self.sel[k as usize];
            *i = (*i).min(rows.len().saturating_sub(1));
            if !rows.is_empty() {
                *i = nearest(&rows, *i, true);
            }
        }
    }

    fn move_sel(&mut self, d: isize) {
        let Some(k) = self.kind() else { return };
        let rows = self.rows(k);
        if rows.is_empty() {
            return;
        }
        let i = (self.sel[k as usize] as isize + d).clamp(0, rows.len() as isize - 1);
        self.sel[k as usize] = nearest(&rows, i as usize, d > 0);
    }

    /// The agents shown under an entry: every detected one that can take it.
    pub fn panel_agents(&self, kind: Kind) -> Vec<&'static str> {
        match kind {
            Kind::Mcp => self.store.mcp_agents().iter().map(|a| a.id).collect(),
            Kind::Skills => self.store.skill_agents().iter().map(|a| a.id).collect(),
        }
    }

    // ---- keys ---------------------------------------------------------------

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if self.modal.is_some() {
            self.on_modal_key(k);
            return;
        }
        if self.filtering {
            let f = &mut self.filter[self.kind().unwrap_or(Kind::Mcp) as usize];
            match k.code {
                KeyCode::Enter => self.filtering = false,
                KeyCode::Esc => {
                    f.text.clear();
                    self.filtering = false;
                }
                KeyCode::Down | KeyCode::Up => {
                    self.filtering = false;
                    self.move_sel(if k.code == KeyCode::Down { 1 } else { -1 });
                }
                _ => {
                    f.handle(k);
                    let kind = self.kind().unwrap_or(Kind::Mcp);
                    self.sel[kind as usize] = 0;
                }
            }
            self.clamp();
            return;
        }
        if self.tab == Tab::Discover && self.discover.editing {
            self.on_search_key(k);
            return;
        }
        // keys that work everywhere
        match k.code {
            KeyCode::Char('q') => {
                self.quit = true;
                return;
            }
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
                return;
            }
            KeyCode::Char(c @ '1'..='3') => {
                self.switch_tab(Tab::ALL[c as usize - '1' as usize]);
                return;
            }
            KeyCode::Tab => {
                self.switch_tab(Tab::ALL[(self.tab.index() + 1) % 3]);
                return;
            }
            KeyCode::BackTab => {
                self.switch_tab(Tab::ALL[(self.tab.index() + 2) % 3]);
                return;
            }
            KeyCode::Char('r') if self.tab != Tab::Discover => {
                self.reload();
                return;
            }
            KeyCode::Char('t') => {
                self.theme = self.theme.toggled();
                let name = if self.theme.mode == Mode::Light {
                    "Light"
                } else {
                    "Dark"
                };
                self.notify(
                    ToastKind::Info,
                    format!("{name} theme · SIU_THEME=dark|light keeps one"),
                );
                return;
            }
            _ => {}
        }
        match self.tab {
            Tab::Discover => self.on_discover_key(k),
            _ if self.focus == Focus::Agents => self.on_agents_key(k),
            _ => self.on_list_key(k),
        }
    }

    pub fn switch_tab(&mut self, t: Tab) {
        self.tab = t;
        self.focus = Focus::List;
        if t == Tab::Discover
            && self.discover.kind == Kind::Skills
            && self.discover.shown[1].is_none()
        {
            self.run_search();
        }
    }

    fn reload(&mut self) {
        match Store::open(&self.env, self.store.root.clone()) {
            Ok(s) => {
                self.store = s;
                self.rescan();
                self.clamp();
                self.notify(ToastKind::Info, "Reloaded");
            }
            Err(e) => self.notify(ToastKind::Err, format!("{e:#}")),
        }
    }

    fn on_list_key(&mut self, k: KeyEvent) {
        let kind = self.kind().unwrap();
        if let Some(Row::Group { label, folded, .. }) = self.current() {
            match k.code {
                KeyCode::Enter | KeyCode::Char(' ') => return self.fold(&label, !folded),
                KeyCode::Right | KeyCode::Char('l') => return self.fold(&label, false),
                KeyCode::Left | KeyCode::Char('h') => return self.fold(&label, true),
                _ => {}
            }
        }
        match k.code {
            KeyCode::Left | KeyCode::Char('h') => {
                if let (Kind::Skills, Some(Row::Entry(_))) = (kind, self.current())
                    && let Some(label) = self.group_above()
                {
                    self.fold(&label, true);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::Home | KeyCode::Char('g') => self.move_sel(-10_000),
            KeyCode::End | KeyCode::Char('G') => self.move_sel(10_000),
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Esc if !self.filter[kind as usize].text.is_empty() => {
                self.filter[kind as usize].text.clear();
                self.clamp();
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => match self.current() {
                Some(Row::Entry(_)) => {
                    if self.panel_agents(kind).is_empty() {
                        self.notify(ToastKind::Err, "No agents installed that can use this");
                    } else {
                        self.focus = Focus::Agents;
                        self.agent_sel = self.agent_sel.min(self.panel_agents(kind).len() - 1);
                    }
                }
                Some(Row::Found(name)) if k.code == KeyCode::Enter => self.import(kind, &name),
                _ => {}
            },
            KeyCode::Char(' ') => {
                if let Some(Row::Entry(name)) = self.current() {
                    let r = match kind {
                        Kind::Mcp => self.store.toggle_server_enabled(&name),
                        Kind::Skills => self.store.toggle_skill_enabled(&name),
                    };
                    match r {
                        Ok(true) => self.commit(format!("{name} enabled")),
                        Ok(false) => self.commit(format!(
                            "{name} paused — removed from every agent, choices kept"
                        )),
                        Err(e) => self.notify(ToastKind::Err, format!("{e:#}")),
                    }
                }
            }
            KeyCode::Char('a') => self.open_add(kind),
            KeyCode::Char('e') if kind == Kind::Mcp => {
                if let Some(Row::Entry(name)) = self.current() {
                    self.open_server_form(Some(name));
                }
            }
            KeyCode::Char('d') | KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(Row::Entry(name)) = self.current() {
                    let (title, body, action) = match kind {
                        Kind::Mcp => (
                            format!("Delete server {name}?"),
                            "It is removed from the library and from every agent siu wrote it to."
                                .to_string(),
                            ConfirmAction::DeleteServer(name),
                        ),
                        Kind::Skills => (
                            format!("Delete skill {name}?"),
                            "Its files and its links in every agent are removed.".to_string(),
                            ConfirmAction::DeleteSkill(name),
                        ),
                    };
                    self.modal = Some(Modal::Confirm(Confirm {
                        title,
                        body,
                        action,
                    }));
                }
            }
            KeyCode::Char('D') => self.confirm_delete_all(kind),
            KeyCode::Char('i') => {
                if let Some(Row::Found(name)) = self.current() {
                    self.import(kind, &name);
                }
            }
            KeyCode::Char('u') if kind == Kind::Skills => {
                if let Some(Row::Entry(name)) = self.current() {
                    self.update_skills(vec![name]);
                }
            }
            KeyCode::Char('U') if kind == Kind::Skills => {
                let names = self
                    .store
                    .lib
                    .skills
                    .iter()
                    .filter(|s| s.source.is_some())
                    .map(|s| s.name.clone())
                    .collect::<Vec<_>>();
                if names.is_empty() {
                    self.notify(ToastKind::Info, "No skills with a source to update from");
                } else {
                    self.update_skills(names);
                }
            }
            _ => {}
        }
    }

    fn on_agents_key(&mut self, k: KeyEvent) {
        let kind = self.kind().unwrap();
        let agents = self.panel_agents(kind);
        let Some(Row::Entry(name)) = self.current() else {
            self.focus = Focus::List;
            return;
        };
        match k.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::List,
            KeyCode::Down | KeyCode::Char('j') => {
                self.agent_sel = (self.agent_sel + 1).min(agents.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.agent_sel = self.agent_sel.saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::Enter => {
                let Some(id) = agents.get(self.agent_sel).copied() else {
                    return;
                };
                let agent = self.store.agent(id).unwrap().clone();
                let r = match kind {
                    Kind::Mcp => {
                        let e = self.store.server(&name).unwrap();
                        if let crate::library::State::Unsupported(why) =
                            self.store.server_state(e, &agent)
                        {
                            self.notify(ToastKind::Err, format!("{}: {why}", agent.name));
                            return;
                        }
                        self.store.toggle_server_agent(&name, id)
                    }
                    Kind::Skills => self.store.toggle_skill_agent(&name, id),
                };
                match r {
                    Ok(on) => self.commit(format!(
                        "{name} {} {}",
                        if on { "on for" } else { "off for" },
                        agent.name
                    )),
                    Err(e) => self.notify(ToastKind::Err, format!("{e:#}")),
                }
            }
            KeyCode::Char('a') | KeyCode::Char('n') => {
                let all = k.code == KeyCode::Char('a');
                let r = match kind {
                    Kind::Mcp => {
                        let s = self.store.server(&name).unwrap().server.clone();
                        let set = if all {
                            self.store.default_server_agents(&s)
                        } else {
                            BTreeSet::new()
                        };
                        self.store.set_server_agents(&name, set)
                    }
                    Kind::Skills => {
                        let set = if all {
                            agents.iter().map(|a| a.to_string()).collect()
                        } else {
                            BTreeSet::new()
                        };
                        self.store.set_skill_agents(&name, set)
                    }
                };
                match r {
                    Ok(()) => self.commit(format!(
                        "{name} {}",
                        if all {
                            "on for every agent"
                        } else {
                            "off for every agent"
                        }
                    )),
                    Err(e) => self.notify(ToastKind::Err, format!("{e:#}")),
                }
            }
            _ => {}
        }
    }

    /// Asks before taking everything of a kind out of the library, saying
    /// which agents lose it.
    fn confirm_delete_all(&mut self, kind: Kind) {
        let (n, agents): (usize, BTreeSet<&String>) = match kind {
            Kind::Mcp => (
                self.store.lib.mcp.len(),
                self.store.lib.mcp.iter().flat_map(|e| &e.agents).collect(),
            ),
            Kind::Skills => (
                self.store.lib.skills.len(),
                self.store
                    .lib
                    .skills
                    .iter()
                    .flat_map(|e| &e.agents)
                    .collect(),
            ),
        };
        if n == 0 {
            let what = match kind {
                Kind::Mcp => "servers",
                Kind::Skills => "skills",
            };
            self.notify(ToastKind::Info, format!("No {what} to remove"));
            return;
        }
        let names: Vec<&str> = agents
            .iter()
            .filter_map(|a| self.store.agent(a))
            .map(|a| a.name)
            .collect();
        let taken = if names.is_empty() {
            "They are taken out of the library; no agent has any of them.".to_string()
        } else {
            format!(
                "They are taken out of the library and out of {}.",
                names.join(", ")
            )
        };
        let kept = match kind {
            Kind::Mcp => "Servers your agents have that siu doesn't manage stay as they are.",
            Kind::Skills => {
                "Folders of your own they were installed from, and skills your agents have that siu doesn't manage, stay as they are."
            }
        };
        self.modal = Some(Modal::Confirm(Confirm {
            title: format!("Remove all {}?", things(kind, n)),
            body: format!("{taken}\n{kept}"),
            action: ConfirmAction::DeleteAll(kind),
        }));
    }

    fn import(&mut self, kind: Kind, name: &str) {
        match kind {
            Kind::Mcp => {
                let Some(f) = self
                    .found_servers
                    .iter()
                    .find(|f| f.server.name == name)
                    .cloned()
                else {
                    return;
                };
                match self.store.import_server(&f) {
                    Ok(()) => {
                        self.commit(format!("Imported {name} — siu manages it now"));
                        self.select_name(Kind::Mcp, name);
                    }
                    Err(e) => self.notify(ToastKind::Err, format!("{e:#}")),
                }
            }
            Kind::Skills => {
                let Some(f) = self.found_skills.iter().find(|f| f.name == name) else {
                    return;
                };
                let wheres =
                    f.at.iter()
                        .map(|(_, p)| tilde(p, &self.env.home))
                        .collect::<Vec<_>>()
                        .join("\n  ");
                self.modal = Some(Modal::Confirm(Confirm {
                    title: format!("Import skill {name}?"),
                    body: format!(
                        "Its files are copied into siu's library, and each agent's copy is moved to ~/.siu/backups and replaced by a link:\n  {wheres}"
                    ),
                    action: ConfirmAction::ImportSkill(name.to_string()),
                }));
            }
        }
    }

    // ---- adding things --------------------------------------------------------

    fn open_add(&mut self, kind: Kind) {
        match kind {
            Kind::Mcp => self.open_server_form(None),
            Kind::Skills => {
                self.modal = Some(Modal::Form(Form {
                    title: "Add skills".into(),
                    fields: vec![Field::text(
                        "From",
                        "",
                        "owner/repo · a github.com URL (…/tree/main/skills/pdf) · a local folder (~/code/my-skill)",
                    )],
                    focus: 0,
                    purpose: FormPurpose::AddSkills,
                    error: None,
                }))
            }
        }
    }

    fn open_server_form(&mut self, edit: Option<String>) {
        let s = edit
            .as_ref()
            .and_then(|n| self.store.server(n))
            .map(|e| e.server.clone())
            .unwrap_or_default();
        let fields = vec![
            Field::text("Name", s.name.clone(), "letters, digits, - and _"),
            Field {
                choices: vec!["stdio", "http", "sse"],
                ..Field::text("Type", s.transport.label(), "←/→ to change")
            },
            Field::text("Command", s.command.clone(), "e.g. npx"),
            Field::text(
                "Args",
                join_args(&s.args),
                "space separated; quote to keep spaces",
            ),
            Field {
                secret: true,
                ..Field::text("Env", join_pairs(&s.env, "="), "KEY=value, KEY2=value")
            },
            Field::text("URL", s.url.clone(), "https://…"),
            Field {
                secret: true,
                ..Field::text(
                    "Headers",
                    join_pairs(&s.headers, ": "),
                    "Authorization: Bearer …, X-Key: …",
                )
            },
        ];
        self.modal = Some(Modal::Form(Form {
            title: if edit.is_some() {
                format!("Edit {}", s.name)
            } else {
                "Add MCP server".into()
            },
            fields,
            focus: 0,
            purpose: match edit {
                Some(n) => FormPurpose::EditServer(n),
                None => FormPurpose::AddServer,
            },
            error: None,
        }));
    }

    fn on_modal_key(&mut self, k: KeyEvent) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal {
            Modal::Help => {}
            Modal::Confirm(c) => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.confirmed(c.action)
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {}
                _ => self.modal = Some(Modal::Confirm(c)),
            },
            Modal::Form(mut f) => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                match k.code {
                    KeyCode::Esc => return,
                    KeyCode::Char('s') if ctrl => return self.submit(f),
                    KeyCode::Enter if f.last() => return self.submit(f),
                    KeyCode::Enter | KeyCode::Tab | KeyCode::Down => f.step(1),
                    KeyCode::BackTab | KeyCode::Up => f.step(-1),
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                        if !f.fields[f.focus].choices.is_empty() =>
                    {
                        let field = &mut f.fields[f.focus];
                        let i = field
                            .choices
                            .iter()
                            .position(|c| *c == field.input.text)
                            .unwrap_or(0);
                        let d = if k.code == KeyCode::Left {
                            field.choices.len() - 1
                        } else {
                            1
                        };
                        field.input.text = field.choices[(i + d) % field.choices.len()].to_string();
                    }
                    _ if f.fields[f.focus].choices.is_empty() => {
                        f.fields[f.focus].input.handle(k);
                        f.error = None;
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Form(f));
            }
            Modal::Picker(mut p) => {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => return,
                    KeyCode::Down | KeyCode::Char('j') => {
                        p.sel = (p.sel + 1).min(p.items.len().saturating_sub(1))
                    }
                    KeyCode::Up | KeyCode::Char('k') => p.sel = p.sel.saturating_sub(1),
                    KeyCode::Char(' ') => {
                        if let Some(it) = p.items.get_mut(p.sel)
                            && it.blocked.is_none()
                        {
                            it.checked = !it.checked;
                            // Only one of several with a name can be had.
                            if it.checked {
                                let label = it.label.clone();
                                for (i, o) in p.items.iter_mut().enumerate() {
                                    o.checked &= i == p.sel || o.label != label;
                                }
                            }
                        }
                    }
                    KeyCode::Char('a') => {
                        let open = p.items.iter().filter(|i| i.blocked.is_none());
                        let all = open.clone().all(|i| {
                            i.checked || open.clone().any(|o| o.checked && o.label == i.label)
                        });
                        let mut seen = HashSet::new();
                        for it in p.items.iter_mut().filter(|i| i.blocked.is_none()) {
                            it.checked = !all && seen.insert(it.label.clone());
                        }
                    }
                    KeyCode::Enter => return self.picked(p),
                    _ => {}
                }
                self.modal = Some(Modal::Picker(p));
            }
        }
    }

    fn confirmed(&mut self, action: ConfirmAction) {
        let r = match &action {
            ConfirmAction::DeleteServer(n) => {
                self.store.remove_server(n).map(|_| format!("Deleted {n}"))
            }
            ConfirmAction::DeleteSkill(n) => {
                self.store.remove_skill(n).map(|_| format!("Deleted {n}"))
            }
            ConfirmAction::DeleteAll(kind) => {
                let n = match kind {
                    Kind::Mcp => std::mem::take(&mut self.store.lib.mcp).len(),
                    Kind::Skills => std::mem::take(&mut self.store.lib.skills).len(),
                };
                Ok(format!("Removed all {}", things(*kind, n)))
            }
            ConfirmAction::ImportSkill(n) => {
                match self.found_skills.iter().find(|f| f.name == *n).cloned() {
                    Some(f) => self
                        .store
                        .import_skill(&f)
                        .map(|_| format!("Imported {n} — siu manages it now")),
                    None => return,
                }
            }
        };
        match r {
            Ok(msg) => {
                self.commit(msg);
                if let ConfirmAction::ImportSkill(n) = &action {
                    self.select_name(Kind::Skills, n);
                }
                self.clamp();
            }
            Err(e) => self.notify(ToastKind::Err, format!("{e:#}")),
        }
    }

    fn submit(&mut self, mut f: Form) {
        let r = match &f.purpose {
            FormPurpose::AddServer | FormPurpose::EditServer(_) => self.submit_server(&f),
            FormPurpose::MarketInputs(m) => {
                let missing = m
                    .inputs
                    .iter()
                    .zip(&f.fields)
                    .find(|(i, fld)| i.required && fld.input.text.trim().is_empty());
                if let Some((i, _)) = missing {
                    Err(format!("{} is required", i.label))
                } else {
                    let values: Vec<String> =
                        f.fields.iter().map(|x| x.input.text.clone()).collect();
                    let s = m.fill(&values, &self.env.home);
                    self.install_server(s, m.id.clone());
                    Ok(())
                }
            }
            FormPurpose::AddSkills => self.add_skills(f.fields[0].input.text.clone()),
        };
        if let Err(e) = r {
            f.error = Some(e);
            self.modal = Some(Modal::Form(f));
        }
    }

    fn submit_server(&mut self, f: &Form) -> Result<(), String> {
        let v = |i: usize| f.fields[i].input.text.trim().to_string();
        let transport = match v(1).as_str() {
            "http" => Transport::Http,
            "sse" => Transport::Sse,
            _ => Transport::Stdio,
        };
        let mut s = Server {
            name: v(0),
            transport,
            ..Default::default()
        };
        if transport == Transport::Stdio {
            let mut words = split_args(&v(2));
            if words.is_empty() {
                return Err("a command is needed".into());
            }
            s.command = words.remove(0);
            s.args = words;
            s.args.extend(split_args(&v(3)));
            s.env = parse_pairs(&v(4), '=')?;
        } else {
            s.url = v(5);
            s.headers = parse_pairs(&v(6), ':')?;
        }
        match &f.purpose {
            FormPurpose::EditServer(old) => {
                self.store
                    .edit_server(old, s.clone())
                    .map_err(|e| format!("{e:#}"))?;
                self.commit(format!("Saved {}", s.name));
            }
            _ => {
                let agents = self.store.default_server_agents(&s);
                let n = agents.len();
                self.store
                    .add_server(s.clone(), agents, None)
                    .map_err(|e| format!("{e:#}"))?;
                self.commit(format!(
                    "Added {} for {n} agent{}",
                    s.name,
                    if n == 1 { "" } else { "s" }
                ));
            }
        }
        self.select_name(Kind::Mcp, &s.name);
        Ok(())
    }

    fn install_server(&mut self, s: Server, origin: String) {
        let name = s.name.clone();
        let agents = self.store.default_server_agents(&s);
        let names: Vec<_> = agents
            .iter()
            .filter_map(|a| self.store.agent(a))
            .map(|a| a.name)
            .collect();
        match self.store.add_server(s, agents, Some(origin)) {
            Ok(()) => {
                let to = if names.is_empty() {
                    "no agent yet".to_string()
                } else {
                    names.join(", ")
                };
                self.commit(format!("Installed {name} → {to}"));
                self.select_name(Kind::Mcp, &name);
            }
            Err(e) => self.notify(ToastKind::Err, format!("{e:#}")),
        }
    }

    fn add_skills(&mut self, from: String) -> Result<(), String> {
        match github::parse_spec(&from, &self.env.home) {
            None => Err("Use owner/repo, a github.com URL or a folder path".into()),
            Some(Spec::Local(dir)) => {
                if !dir.is_dir() {
                    return Err(format!("{} isn't a folder", dir.display()));
                }
                let found = skills::find(&dir);
                if found.is_empty() {
                    return Err(format!("No SKILL.md under {}", dir.display()));
                }
                self.open_skill_picker(
                    tilde(&dir, &self.env.home),
                    found,
                    None,
                    SkillOrigin::Local,
                );
                Ok(())
            }
            Some(Spec::Github {
                repo,
                reference,
                subpath,
            }) => {
                if self.offline {
                    return Err("offline".into());
                }
                let use_ = TreeUse::Browse {
                    repo: repo.clone(),
                    reference: reference.clone(),
                    subpath,
                };
                self.request_tree(&repo, &reference, Some(use_), true);
                Ok(())
            }
        }
    }

    fn open_skill_picker(
        &mut self,
        from: String,
        found: Vec<Found>,
        guard: Option<Arc<TempDir>>,
        origin: SkillOrigin,
    ) {
        let names: Vec<String> = found.iter().map(Found::name).collect();
        let items: Vec<PickItem> = found
            .iter()
            .zip(&names)
            .map(|(f, name)| {
                let blocked = self.store.skill(name).map(|_| "installed".to_string());
                // Namesakes are told apart by where they are.
                let detail = if names.iter().filter(|n| *n == name).count() > 1 {
                    format!("{} · {}", f.path, f.meta.description)
                } else {
                    f.meta.description.clone()
                };
                PickItem {
                    checked: blocked.is_none() && found.len() == 1,
                    label: name.clone(),
                    detail,
                    blocked,
                }
            })
            .collect();
        self.modal = Some(Modal::Picker(Picker {
            title: format!(
                "{} skill{} in {from}",
                found.len(),
                if found.len() == 1 { "" } else { "s" }
            ),
            items,
            sel: 0,
            purpose: PickPurpose::InstallSkills {
                _guard: guard,
                found,
                origin,
            },
        }));
    }

    fn picked(&mut self, p: Picker) {
        let PickPurpose::InstallSkills {
            found,
            origin,
            _guard,
        } = p.purpose;
        let chosen: Vec<Found> = found
            .into_iter()
            .zip(&p.items)
            .filter(|(_, it)| it.checked)
            .map(|(f, _)| f)
            .collect();
        if chosen.is_empty() {
            self.notify(ToastKind::Info, "Nothing picked — space to pick, a for all");
            return;
        }
        self.install_found(&chosen, &origin);
    }

    fn install_found(&mut self, found: &[Found], origin: &SkillOrigin) {
        let agents = self.store.default_skill_agents();
        let source = |f: &Found| match origin {
            SkillOrigin::Github { repo, reference } => Some(Source::Github {
                repo: repo.clone(),
                reference: reference.clone(),
                path: f.path.clone(),
            }),
            SkillOrigin::Local => Some(Source::Local { dir: f.dir.clone() }),
        };
        match self.store.install_skills(found, source, &agents) {
            Ok(names) => {
                let n = agents.len();
                self.commit(format!(
                    "Installed {} → {n} agent{}",
                    names.join(", "),
                    if n == 1 { "" } else { "s" }
                ));
                if let Some(first) = names.first() {
                    self.select_name(Kind::Skills, first);
                }
            }
            Err(e) => self.notify(ToastKind::Err, format!("{e:#}")),
        }
    }

    fn update_skills(&mut self, names: Vec<String>) {
        let mut by_repo: HashMap<(String, String), Vec<String>> = HashMap::new();
        let (mut updated, mut same, mut errors) = (vec![], 0, vec![]);
        for n in names {
            match self.store.skill(&n).and_then(|s| s.source.clone()) {
                Some(Source::Github {
                    repo, reference, ..
                }) => by_repo.entry((repo, reference)).or_default().push(n),
                Some(Source::Local { dir }) => match self.store.update_skill_from(&n, &dir) {
                    Ok(UpdateOutcome::Updated) => updated.push(n),
                    Ok(UpdateOutcome::UpToDate) => same += 1,
                    Err(e) => errors.push(format!("{n}: {e:#}")),
                },
                None => errors.push(format!("{n} has no source to update from")),
            }
        }
        if !updated.is_empty() || same > 0 || !errors.is_empty() {
            let _ = self.store.save();
            if by_repo.is_empty() {
                self.report_updates(updated, same, errors);
            }
        }
        for ((repo, reference), names) in by_repo {
            // an update wants what's there now, not a download from earlier
            self.request_tree(&repo, &reference, Some(TreeUse::Update { names }), false);
        }
    }

    // ---- discover -------------------------------------------------------------

    fn on_search_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Enter => {
                self.discover.editing = false;
                if self.discover.typed_at.take().is_some()
                    || self.discover.shown[self.discover.k()].is_none()
                {
                    self.run_search();
                }
            }
            KeyCode::Esc => self.discover.editing = false,
            KeyCode::Down => self.discover.editing = false,
            KeyCode::Tab => {
                self.toggle_kind();
            }
            _ => {
                if !self.discover.query.handle(k) {
                    return;
                }
                self.discover.typed_at = Some(Instant::now());
                // the list narrows at once from what's known; the search
                // runs when typing stops
                let q = self.discover.query.text.trim().to_string();
                if self.discover.kind == Kind::Mcp {
                    self.discover.servers = market::featured_matching(&q);
                    self.discover.shown[0] = None;
                    self.discover.sel[0] = 0;
                } else {
                    self.discover.sel[1] = 0;
                    self.show_skills(&q);
                    if self.discover.searches.contains_key(&q) {
                        self.discover.typed_at = None; // answered already
                    }
                }
            }
        }
    }

    fn toggle_kind(&mut self) {
        self.discover.kind = if self.discover.kind == Kind::Mcp {
            Kind::Skills
        } else {
            Kind::Mcp
        };
        let q = self.discover.query.text.trim().to_string();
        if self.discover.shown[self.discover.k()].as_deref() != Some(q.as_str()) {
            self.run_search();
        }
    }

    fn on_discover_key(&mut self, k: KeyEvent) {
        let d = &mut self.discover;
        let i = d.k();
        let len = if d.kind == Kind::Mcp {
            d.servers.len()
        } else {
            d.skills.len()
        };
        let moved = |d: &mut Discover, n: usize| {
            d.sel[i] = n;
            d.sel_at = Instant::now();
        };
        match k.code {
            KeyCode::Char('/') | KeyCode::Char('s') => d.editing = true,
            KeyCode::Esc => {
                if !d.query.text.is_empty() {
                    d.query.text.clear();
                    self.run_search();
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                self.toggle_kind()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                moved(d, (d.sel[i] + 1).min(len.saturating_sub(1)))
            }
            KeyCode::Up | KeyCode::Char('k') if d.sel[i] == 0 => d.editing = true,
            KeyCode::Up | KeyCode::Char('k') => moved(d, d.sel[i] - 1),
            KeyCode::PageDown => moved(d, (d.sel[i] + 10).min(len.saturating_sub(1))),
            KeyCode::PageUp => moved(d, d.sel[i].saturating_sub(10)),
            KeyCode::Home | KeyCode::Char('g') => moved(d, 0),
            KeyCode::End | KeyCode::Char('G') => moved(d, len.saturating_sub(1)),
            KeyCode::Enter | KeyCode::Char('i') => self.install_selected(),
            _ => {}
        }
    }

    fn run_search(&mut self) {
        let q = self.discover.query.text.trim().to_string();
        let i = self.discover.k();
        self.discover.typed_at = None;
        self.discover.error = None;
        self.discover.sel[i] = 0;
        match self.discover.kind {
            Kind::Mcp => {
                self.discover.servers = market::featured_matching(&q);
                self.discover.shown[0] = Some(q.clone());
                if q.is_empty() || self.offline {
                    return;
                }
                self.discover.loading[0] = true;
                self.spawn(format!("searching MCP Registry for “{q}”"), move || {
                    Msg::Registry {
                        result: market::search_registry(&q).map_err(|e| format!("{e:#}")),
                        query: q,
                    }
                });
            }
            Kind::Skills => {
                self.show_skills(&q);
                if q.is_empty() {
                    // the cached list is shown; an old or missing one is fetched
                    if !self.discover.index_fresh {
                        self.discover.loading[1] = self.discover.index.is_empty() && !self.offline;
                        self.fetch_popular();
                    }
                    return;
                }
                if self.offline || self.discover.searches.contains_key(&q) {
                    return;
                }
                self.discover.loading[1] = true;
                self.spawn(format!("searching skills.sh for “{q}”"), move || {
                    Msg::Skills {
                        result: market::search_skills(&q).map_err(|e| format!("{e:#}")),
                        query: q,
                    }
                });
            }
        }
    }

    /// Shows the skills for a query from what's known now: skills.sh's
    /// answer if it gave one, else the matches among the popular skills and
    /// earlier answers. The cursor stays on the skill it was on.
    fn show_skills(&mut self, q: &str) {
        let d = &mut self.discover;
        let was = d.skills.get(d.sel[1]).map(MarketSkill::key);
        d.skills = if q.is_empty() {
            market::popular_view(&d.index)
        } else {
            let mut known: Vec<MarketSkill> = d.index.clone();
            for list in d.searches.values() {
                known.extend(list.iter().cloned());
            }
            known.sort_by_key(MarketSkill::key);
            known.dedup_by_key(|s| s.key());
            let local = market::search_local(&known, q, 50);
            match d.searches.get(q) {
                Some(remote) => market::merge(remote.clone(), &local),
                None => local,
            }
        };
        d.shown[1] = Some(q.to_string());
        d.sel[1] = was
            .and_then(|k| d.skills.iter().position(|s| s.key() == k))
            .unwrap_or(0);
    }

    /// The library entry a market server already is, if any.
    pub fn have_server(&self, m: &MarketServer) -> Option<String> {
        let sum = m.server.summary();
        self.store
            .lib
            .mcp
            .iter()
            .find(|e| e.server.summary() == sum || e.origin.as_deref() == Some(&m.id))
            .map(|e| e.server.name.clone())
    }

    pub fn have_skill(&self, m: &MarketSkill) -> Option<String> {
        self.store
            .lib
            .skills
            .iter()
            .find(|s| match &s.source {
                Some(Source::Github { repo, path, .. }) => {
                    repo.eq_ignore_ascii_case(&m.source)
                        && (skills::last_part(path) == m.skill_id || s.name == m.skill_id)
                }
                _ => false,
            })
            .map(|s| s.name.clone())
    }

    /// Where the skill that already has this one's name came from, when it
    /// isn't this one.
    pub fn skill_name_taken(&self, m: &MarketSkill) -> Option<String> {
        if self.have_skill(m).is_some() {
            return None;
        }
        let s = self.store.skill(&skills::sanitize(&m.skill_id))?;
        Some(
            s.source
                .as_ref()
                .map_or("your agents".into(), Source::label),
        )
    }

    fn install_selected(&mut self) {
        let d = &self.discover;
        match d.kind {
            Kind::Mcp => {
                let Some(m) = d.servers.get(d.sel[0]).cloned() else {
                    return;
                };
                if let Some(have) = self.have_server(&m) {
                    self.notify(
                        ToastKind::Info,
                        format!("Already installed as {have} — see the MCP tab"),
                    );
                    return;
                }
                let mut m = m;
                // a name the library already uses for another server gets a suffix
                let base = m.server.name.clone();
                let mut n = 2;
                while self.store.server(&m.server.name).is_some() {
                    m.server.name = format!("{base}-{n}");
                    n += 1;
                }
                if m.inputs.is_empty() {
                    let id = m.id.clone();
                    self.install_server(m.server, id);
                } else {
                    let fields = m
                        .inputs
                        .iter()
                        .map(|i| Field {
                            label: i.label.clone(),
                            input: Input::default(),
                            hint: [
                                if i.required { "required" } else { "optional" },
                                i.hint.as_str(),
                            ]
                            .iter()
                            .filter(|s| !s.is_empty())
                            .copied()
                            .collect::<Vec<_>>()
                            .join(" · "),
                            secret: i.secret,
                            choices: vec![],
                        })
                        .collect();
                    self.modal = Some(Modal::Form(Form {
                        title: format!("Install {}", m.title),
                        fields,
                        focus: 0,
                        purpose: FormPurpose::MarketInputs(Box::new(m)),
                        error: None,
                    }));
                }
            }
            Kind::Skills => {
                let Some(s) = d.skills.get(d.sel[1]).cloned() else {
                    return;
                };
                if let Some(have) = self.have_skill(&s) {
                    self.notify(
                        ToastKind::Info,
                        format!("Already installed as {have} — see the Skills tab"),
                    );
                    return;
                }
                if let Some(from) = self.skill_name_taken(&s) {
                    self.notify(
                        ToastKind::Info,
                        format!("{} is taken by the one from {from}", s.skill_id),
                    );
                    return;
                }
                let repo = s.source.clone();
                self.request_tree(&repo, "HEAD", Some(TreeUse::Market(s)), true);
            }
        }
    }
}

/// "1 server", "3 skills".
fn things(kind: Kind, n: usize) -> String {
    let what = match kind {
        Kind::Mcp => "server",
        Kind::Skills => "skill",
    };
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

fn tree_key(repo: &str, reference: &str) -> String {
    format!("{repo}@{reference}")
}

fn tree_label(key: &str) -> String {
    format!("fetching {}", key.split('@').next().unwrap_or(key))
}

// ---- small text helpers --------------------------------------------------------

pub fn tilde(p: &std::path::Path, home: &std::path::Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// Splits like a shell: spaces separate, quotes keep them.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn join_pairs(m: &std::collections::BTreeMap<String, String>, sep: &str) -> String {
    m.iter()
        .map(|(k, v)| format!("{k}{sep}{v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_pairs(s: &str, sep: char) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut out = std::collections::BTreeMap::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((k, v)) = part.split_once(sep) else {
            return Err(format!("“{part}” needs a {sep}"));
        };
        out.insert(k.trim().to_string(), v.trim().to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_in(home: &std::path::Path) -> App {
        for dir in [".claude", ".codex"] {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        let env = Env::rooted(home);
        let mut app = App::new(Store::open(&env, home.join(".siu")).unwrap(), env);
        app.offline = true;
        app.tab = Tab::Discover;
        app.discover.kind = Kind::Skills;
        app
    }

    /// A downloaded repository with two skills, as github::download gives it.
    fn repo_tree() -> (TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("skills-abc123");
        for n in ["pdf", "docx"] {
            let d = root.join("skills").join(n);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("SKILL.md"),
                format!("---\nname: {n}\ndescription: d\n---\n"),
            )
            .unwrap();
        }
        (t, root)
    }

    fn market(id: &str) -> MarketSkill {
        MarketSkill {
            source: "anthropics/skills".into(),
            skill_id: id.into(),
            name: id.into(),
            installs: 1,
            official: true,
        }
    }

    fn enter(app: &mut App) {
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn a_second_skill_from_the_same_repo_needs_no_download() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_in(d.path());
        let (guard, root) = repo_tree();
        app.trees.insert(
            tree_key("anthropics/skills", "HEAD"),
            Tree {
                guard: Arc::new(guard),
                root,
                at: Instant::now(),
            },
        );
        app.discover.skills = vec![market("pdf"), market("docx")];
        for i in 0..2 {
            app.discover.sel[1] = i;
            enter(&mut app);
        }
        assert!(app.busy.is_empty() && app.fetching.is_empty());
        assert!(app.store.skill("pdf").is_some() && app.store.skill("docx").is_some());
        assert!(d.path().join(".claude/skills/docx/SKILL.md").is_file());
    }

    #[test]
    fn a_namesake_from_another_repo_is_not_installed() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_in(d.path());
        let (guard, root) = repo_tree();
        app.trees.insert(
            tree_key("anthropics/skills", "HEAD"),
            Tree {
                guard: Arc::new(guard),
                root,
                at: Instant::now(),
            },
        );
        let other = MarketSkill {
            source: "acme/skills".into(),
            ..market("pdf")
        };
        app.discover.skills = vec![market("pdf"), other.clone()];
        enter(&mut app);
        assert_eq!(app.have_skill(&market("pdf")).as_deref(), Some("pdf"));
        assert_eq!(app.have_skill(&other), None);

        app.discover.sel[1] = 1;
        enter(&mut app);
        assert!(
            app.busy.is_empty() && app.fetching.is_empty(),
            "no download"
        );
        assert!(
            app.toast
                .as_ref()
                .unwrap()
                .text
                .contains("github.com/anthropics/skills")
        );
    }

    #[test]
    fn install_joins_a_download_already_under_way() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_in(d.path());
        app.offline = false;
        let key = tree_key("anthropics/skills", "HEAD");
        app.fetching.insert(key.clone()); // a quiet prefetch of it
        app.discover.skills = vec![market("pdf")];
        enter(&mut app);
        enter(&mut app); // impatient
        assert_eq!(app.busy, ["fetching anthropics/skills"]);
        assert_eq!(app.waiting.len(), 2);
        assert!(app.store.skill("pdf").is_none());

        app.on_msg(Msg::Tree {
            key: key.clone(),
            result: Ok(repo_tree()),
        });
        assert!(app.busy.is_empty() && app.waiting.is_empty() && app.fetching.is_empty());
        assert!(app.store.skill("pdf").is_some());
        assert!(app.trees.contains_key(&key));
        assert!(
            app.toast
                .as_ref()
                .unwrap()
                .text
                .starts_with("Installed pdf")
        );
    }

    #[test]
    fn a_failed_prefetch_says_nothing() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_in(d.path());
        let key = tree_key("a/b", "HEAD");
        app.fetching.insert(key.clone());
        app.on_msg(Msg::Tree {
            key,
            result: Err("boom".into()),
        });
        assert!(app.toast.is_none());
    }

    #[test]
    fn a_late_search_answer_is_kept_for_later() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_in(d.path());
        app.discover.query.text = "pdf".into();
        app.on_msg(Msg::Skills {
            query: "react".into(),
            result: Ok(vec![market("react-x")]),
        });
        assert!(
            app.discover.skills.is_empty(),
            "not shown under another query"
        );
        app.discover.query.text = "react".into();
        app.run_search();
        assert_eq!(app.discover.skills[0].skill_id, "react-x");
        assert!(app.busy.is_empty(), "answered from memory");
    }

    #[test]
    fn args_split_like_a_shell() {
        assert_eq!(
            split_args(r#"-y "@a/b c" --x='1 2' """#),
            ["-y", "@a/b c", "--x=1 2", ""]
        );
        assert_eq!(join_args(&split_args(r#"a "b c""#)), r#"a "b c""#);
        assert_eq!(parse_pairs("A=1, B = x=y", '=').unwrap()["B"], "x=y");
        assert!(parse_pairs("nope", '=').is_err());
    }
}
