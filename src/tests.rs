//! End-to-end: the app driven by key presses, as a user would, against a
//! fake home folder; checked by what the screen shows and what lands in the
//! agents' files.

use std::fs;
use std::path::{Path, PathBuf};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use crate::agents::Env;
use crate::app::{App, Kind, Modal, Row, Tab};
use crate::library::Store;
use crate::skills;

struct T {
    _d: tempfile::TempDir,
    home: PathBuf,
    app: App,
    term: Terminal<TestBackend>,
}

impl T {
    /// Claude Code, Codex and Claude Desktop installed; Claude Code already
    /// has a server of the user's own.
    fn new() -> T {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_path_buf();
        for dir in [".claude", ".codex", ".config/Claude"] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        fs::write(
            home.join(".claude.json"),
            r#"{"theme":"dark","mcpServers":{"mine":{"type":"stdio","command":"my-server","args":[],"env":{}}}}"#,
        )
        .unwrap();
        T::open(d, home)
    }

    fn open(d: tempfile::TempDir, home: PathBuf) -> T {
        let env = Env::rooted(&home);
        let store = Store::open(&env, home.join(".siu")).unwrap();
        let mut app = App::new(store, env);
        app.offline = true;
        T {
            _d: d,
            home,
            app,
            term: Terminal::new(TestBackend::new(120, 34)).unwrap(),
        }
    }

    fn screen(&mut self) -> String {
        self.app.tick();
        self.term
            .draw(|f| crate::ui::draw(f, &mut self.app))
            .unwrap();
        let buf = self.term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn press(&mut self, code: KeyCode) -> &mut Self {
        self.app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        self
    }

    fn ctrl(&mut self, c: char) -> &mut Self {
        self.app
            .on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        self
    }

    fn keys(&mut self, s: &str) -> &mut Self {
        for c in s.chars() {
            self.press(KeyCode::Char(c));
        }
        self
    }

    fn toast(&self) -> String {
        self.app
            .toast
            .as_ref()
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    fn claude(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.home.join(".claude.json")).unwrap()).unwrap()
    }

    fn codex(&self) -> String {
        fs::read_to_string(self.home.join(".codex/config.toml")).unwrap_or_default()
    }
}

fn skill_tree(root: &Path, names: &[&str]) {
    for n in names {
        let d = root.join(n);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {n}\ndescription: Helps with {n}\n---\nv1"),
        )
        .unwrap();
    }
}

#[test]
fn first_launch_shows_found_servers_and_agents() {
    let mut t = T::new();
    let s = t.screen();
    assert!(
        s.contains("siu")
            && s.contains("MCP 0")
            && s.contains("Skills 0")
            && s.contains("Discover"),
        "{s}"
    );
    assert!(s.contains("3 agents"), "{s}");
    assert!(s.contains("found in agents"), "{s}");
    assert!(s.contains("mine") && s.contains("not managed"), "{s}");
    t.keys("2");
    assert!(t.screen().contains("No skills yet."));
}

#[test]
fn add_toggle_pause_edit_and_delete_a_server() {
    let mut t = T::new();
    t.keys("a").keys("weather");
    t.press(KeyCode::Tab)
        .press(KeyCode::Tab)
        .keys("npx -y @acme/weather");
    t.press(KeyCode::Tab)
        .press(KeyCode::Tab)
        .keys("API_KEY=secret123");
    let s = t.screen();
    assert!(
        s.contains("Add MCP server") && s.contains("KEY=value"),
        "{s}"
    );
    t.ctrl('s');
    assert!(t.app.modal.is_none(), "{}", t.screen());
    // Claude Desktop takes a command server too
    assert_eq!(t.toast(), "Added weather for 3 agents");
    assert_eq!(
        t.claude()["mcpServers"]["weather"]["args"],
        serde_json::json!(["-y", "@acme/weather"])
    );
    assert_eq!(
        t.claude()["mcpServers"]["weather"]["env"]["API_KEY"],
        "secret123"
    );
    assert_eq!(t.claude()["theme"], "dark");
    assert!(t.codex().contains("[mcp_servers.weather]"));
    let s = t.screen();
    assert!(s.contains("● weather") && s.contains("3/3"), "{s}");
    assert!(
        s.contains("API_KEY=••••t123") && !s.contains("secret123"),
        "{s}"
    );
    assert!(s.contains("[✓] Claude Code"), "{s}");

    // off for Claude Code only
    t.press(KeyCode::Enter).press(KeyCode::Char(' '));
    assert!(t.claude()["mcpServers"].get("weather").is_none());
    assert!(t.codex().contains("weather"));
    let s = t.screen();
    assert!(s.contains("[ ] Claude Code") && s.contains("2/3"), "{s}");
    assert!(s.contains("space toggle"), "agents panel hints: {s}");

    // pause everywhere, then resume
    t.press(KeyCode::Esc).press(KeyCode::Char(' '));
    assert!(!t.codex().contains("weather"));
    assert!(t.screen().contains("Paused"));
    t.press(KeyCode::Char(' '));
    assert!(t.codex().contains("weather"));

    // edit to a remote server
    t.keys("e");
    t.press(KeyCode::Tab).press(KeyCode::Right); // type: http
    t.press(KeyCode::Tab)
        .keys("https://weather.example.com/mcp");
    t.ctrl('s');
    assert!(t.app.modal.is_none(), "{}", t.screen());
    assert!(
        t.codex()
            .contains("url = \"https://weather.example.com/mcp\""),
        "{}",
        t.codex()
    );

    t.keys("d");
    assert!(t.screen().contains("Delete server weather?"));
    t.keys("y");
    assert!(!t.codex().contains("weather"));
    assert_eq!(
        t.claude()["mcpServers"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["mine"]
    );
}

#[test]
fn form_errors_keep_the_form_open() {
    let mut t = T::new();
    t.keys("a").keys("bad name");
    t.ctrl('s');
    let s = t.screen();
    assert!(matches!(t.app.modal, Some(Modal::Form(_))));
    assert!(s.contains("a command is needed"), "{s}");
    t.press(KeyCode::Tab).press(KeyCode::Tab).keys("run");
    t.ctrl('s');
    assert!(t.screen().contains("use letters, digits"));
    t.press(KeyCode::Esc);
    assert!(t.app.modal.is_none());
    assert!(t.app.store.lib.mcp.is_empty());
}

#[test]
fn a_found_server_is_imported_with_enter() {
    let mut t = T::new();
    t.screen();
    // the cursor skips the "found in agents" heading
    assert_eq!(t.app.current(), Some(crate::app::Row::Found("mine".into())));
    t.press(KeyCode::Enter);
    assert!(t.toast().contains("Imported mine"));
    let s = t.screen();
    assert!(s.contains("● mine") && !s.contains("not managed"), "{s}");
    // siu now owns it: turning it off for Claude Code takes it out
    t.press(KeyCode::Enter).press(KeyCode::Char(' '));
    assert!(t.claude()["mcpServers"].get("mine").is_none());
}

#[test]
fn filter_narrows_the_list() {
    let mut t = T::new();
    for name in ["alpha", "beta"] {
        t.keys("a")
            .keys(name)
            .press(KeyCode::Tab)
            .press(KeyCode::Tab)
            .keys("cmd");
        t.ctrl('s');
    }
    t.keys("/").keys("bet");
    let s = t.screen();
    assert!(s.contains("● beta") && !s.contains("● alpha"), "{s}");
    t.press(KeyCode::Esc);
    assert!(t.screen().contains("● alpha"));
}

#[test]
fn discover_installs_featured_servers() {
    let mut t = T::new();
    t.keys("3");
    let s = t.screen();
    assert!(
        s.contains("★ Context7")
            && s.contains("★ Playwright")
            && s.contains("Search the official MCP Registry"),
        "{s}"
    );

    // typing narrows the picked list at once
    t.keys("/").keys("play");
    let s = t.screen();
    assert!(s.contains("Playwright") && !s.contains("Context7"), "{s}");
    t.press(KeyCode::Enter).press(KeyCode::Enter);
    assert!(
        t.toast().starts_with("Installed playwright → "),
        "{}",
        t.toast()
    );
    assert_eq!(t.claude()["mcpServers"]["playwright"]["command"], "npx");
    assert!(t.screen().contains("✓ Playwright"));
    t.press(KeyCode::Enter);
    assert!(t.toast().contains("Already installed"));

    // one that needs a value asks for it, and won't go on without it
    t.keys("/")
        .ctrl('u')
        .keys("filesystem")
        .press(KeyCode::Enter)
        .press(KeyCode::Enter);
    assert!(t.screen().contains("Install Filesystem"));
    t.ctrl('s');
    assert!(t.screen().contains("Folder is required"));
    t.keys("~/code").ctrl('s');
    let args = t.claude()["mcpServers"]["filesystem"]["args"].clone();
    assert_eq!(args[2], t.home.join("code").to_string_lossy().as_ref());

    // a remote one skips Claude Desktop
    t.keys("/")
        .ctrl('u')
        .keys("deepwiki")
        .press(KeyCode::Enter)
        .press(KeyCode::Enter);
    let desktop =
        fs::read_to_string(t.home.join(".config/Claude/claude_desktop_config.json")).unwrap();
    assert!(
        desktop.contains("playwright") && !desktop.contains("deepwiki"),
        "{desktop}"
    );
    t.keys("1");
    let s = t.screen();
    assert!(s.contains("● deepwiki") && s.contains("2/2"), "{s}");
    t.press(KeyCode::Enter);
    assert!(t.screen().contains("remote servers go in Connectors"));
}

#[test]
fn skills_from_a_folder_install_update_and_delete() {
    let mut t = T::new();
    let src = t.home.join("my-skills");
    skill_tree(&src, &["pdf", "docx", "xlsx"]);
    t.keys("2")
        .keys("a")
        .keys("~/my-skills")
        .press(KeyCode::Enter);
    let s = t.screen();
    assert!(
        s.contains("3 skills in ~/my-skills") && s.contains("Helps with pdf"),
        "{s}"
    );
    t.keys("a"); // pick all
    t.press(KeyCode::Down).press(KeyCode::Char(' ')); // but not docx... the list is sorted: docx, pdf, xlsx
    t.press(KeyCode::Enter);
    assert!(t.app.modal.is_none());
    assert_eq!(t.toast(), "Installed docx, xlsx → 2 agents");
    let lib = t.app.store.skill_dir("docx");
    assert!(skills::links_to(&t.home.join(".claude/skills/docx"), &lib));
    assert!(skills::links_to(&t.home.join(".codex/skills/docx"), &lib));
    assert!(!t.home.join(".claude/skills/pdf").exists());
    let s = t.screen();
    assert!(s.contains("● docx") && s.contains("local"), "{s}");

    // update picks up the change through the links
    fs::write(
        src.join("docx/SKILL.md"),
        "---\nname: docx\ndescription: v2\n---\nv2",
    )
    .unwrap();
    t.keys("u");
    assert_eq!(t.toast(), "Updated docx");
    assert!(
        fs::read_to_string(t.home.join(".claude/skills/docx/SKILL.md"))
            .unwrap()
            .contains("v2")
    );
    t.keys("U");
    assert_eq!(t.toast(), "All 2 up to date");

    // the same folder again offers only what's new
    t.keys("a").keys("~/my-skills").press(KeyCode::Enter);
    assert!(t.screen().contains("installed"));
    t.press(KeyCode::Esc);

    t.keys("d").keys("y");
    assert!(fs::symlink_metadata(t.home.join(".claude/skills/docx")).is_err());
    assert!(!lib.exists());
}

#[test]
fn skills_sharing_a_name_install_one_at_a_time() {
    let mut t = T::new();
    let src = t.home.join("my-skills");
    skill_tree(&src.join("a"), &["pdf"]);
    skill_tree(&src.join("b"), &["pdf"]);
    skill_tree(&src, &["docx"]);
    t.keys("2")
        .keys("a")
        .keys("~/my-skills")
        .press(KeyCode::Enter);
    let s = t.screen();
    assert!(s.contains("a/pdf") && s.contains("b/pdf"), "{s}");

    t.keys("a"); // all: one of each name
    assert!(t.screen().contains("install 2"));
    let picked = |t: &T| -> Vec<String> {
        let Some(Modal::Picker(p)) = &t.app.modal else {
            panic!("no picker")
        };
        p.items
            .iter()
            .filter(|i| i.checked)
            .map(|i| i.label.clone())
            .collect()
    };
    assert_eq!(picked(&t), ["pdf", "docx"]);

    // picking the other pdf drops the first
    t.press(KeyCode::Down).press(KeyCode::Char(' '));
    assert!(t.screen().contains("install 2"));
    t.press(KeyCode::Enter);
    assert_eq!(t.toast(), "Installed pdf, docx → 2 agents");
    let lib = t.app.store.skill_dir("pdf");
    assert!(skills::links_to(&t.home.join(".claude/skills/pdf"), &lib));
    let e = t.app.store.skill("pdf").unwrap();
    assert!(
        matches!(&e.source, Some(crate::library::Source::Local { dir }) if dir.ends_with("b/pdf"))
    );
}

#[test]
fn a_skill_already_in_an_agent_is_imported_after_confirming() {
    let mut t = T::new();
    skill_tree(&t.home.join(".claude/skills"), &["mine"]);
    t.app
        .on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    t.keys("2");
    let s = t.screen();
    assert!(s.contains("◇ mine") && s.contains("Helps with mine"), "{s}");
    t.press(KeyCode::Enter);
    assert!(t.screen().contains("Import skill mine?"));
    t.press(KeyCode::Char('n'));
    assert!(t.app.store.skill("mine").is_none());
    t.press(KeyCode::Enter).keys("y");
    assert!(skills::links_to(
        &t.home.join(".claude/skills/mine"),
        &t.app.store.skill_dir("mine")
    ));
    let s = t.screen();
    assert!(s.contains("● mine") && s.contains("imported"), "{s}");
}

/// The left panel's lines, trimmed.
fn left(screen: &str) -> Vec<String> {
    screen
        .lines()
        .map(|l| {
            let l: String = l.chars().take(50).collect();
            l.trim_matches(|c: char| c == '│' || c.is_whitespace())
                .to_string()
        })
        .collect()
}

#[test]
fn skills_are_grouped_by_where_they_came_from() {
    let mut t = T::new();
    skill_tree(&t.home.join("team-a"), &["pdf", "docx"]);
    skill_tree(&t.home.join("team-b"), &["zeta"]);
    skill_tree(&t.home.join(".claude/skills"), &["mine"]);
    t.keys("r").keys("2");
    t.keys("a").keys("~/team-b").press(KeyCode::Enter);
    t.press(KeyCode::Enter);
    t.keys("a").keys("~/team-a").press(KeyCode::Enter);
    t.keys("a").press(KeyCode::Enter);
    t.keys("G").press(KeyCode::Enter).keys("y"); // import mine
    assert!(t.app.store.skill("mine").is_some());

    // one heading over each folder's skills, by name within; imported last
    let s = t.screen();
    let lines = left(&s);
    let at = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no {needle} in\n{s}"))
    };
    let order = [
        at("~/team-a"),
        at("● docx"),
        at("● pdf"),
        at("~/team-b"),
        at("● zeta"),
        at("imported from agents"),
        at("● mine"),
    ];
    assert!(order.is_sorted(), "{order:?}\n{s}");
    assert!(lines[at("~/team-a")].ends_with('2'), "{s}");

    // headings are rows of their own, the cursor goes over them in order
    t.keys("g");
    assert_eq!(t.app.current(), Some(group("~/team-a", 2, false)));
    t.keys("jjj");
    assert_eq!(t.app.current(), Some(group("~/team-b", 1, false)));
    t.keys("j");
    assert_eq!(t.app.current(), Some(Row::Entry("zeta".into())));

    // a group's name filters to its skills, heading kept
    t.keys("/team-b").press(KeyCode::Enter);
    assert_eq!(
        t.app.rows(Kind::Skills),
        vec![group("~/team-b", 1, false), Row::Entry("zeta".into())]
    );
}

fn group(label: &str, n: usize, folded: bool) -> Row {
    Row::Group {
        label: label.into(),
        n,
        folded,
    }
}

#[test]
fn big_groups_start_folded_and_folds_are_kept() {
    let mut t = T::new();
    let many: Vec<String> = (1..=12).map(|i| format!("s{i:02}")).collect();
    let many: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
    skill_tree(&t.home.join("big"), &many);
    skill_tree(&t.home.join("small"), &["zeta"]);
    t.keys("2");
    t.keys("a").keys("~/big").press(KeyCode::Enter);
    t.keys("a").press(KeyCode::Enter);
    t.keys("a").keys("~/small").press(KeyCode::Enter);
    t.press(KeyCode::Enter);

    // more than ten skills: only the heading shows
    let s = t.screen();
    assert!(s.contains("▸ ~/big") && s.contains("▾ ~/small"), "{s}");
    assert!(!s.contains("● s01") && s.contains("● zeta"), "{s}");
    t.keys("g");
    assert_eq!(t.app.current(), Some(group("~/big", 12, true)));
    assert!(t.screen().contains("12 skills"));

    // ⏎ unfolds, ← on a skill folds its group back onto the heading
    t.press(KeyCode::Enter);
    assert!(t.screen().contains("● s01"));
    t.keys("jj");
    assert_eq!(t.app.current(), Some(Row::Entry("s02".into())));
    t.press(KeyCode::Left);
    assert_eq!(t.app.current(), Some(group("~/big", 12, true)));
    t.press(KeyCode::Right);
    assert_eq!(t.app.current(), Some(group("~/big", 12, false)));

    // a filter shows every match, folded or not
    t.keys("G").press(KeyCode::Left); // fold small
    t.keys("/zeta").press(KeyCode::Enter);
    assert_eq!(
        t.app.rows(Kind::Skills),
        vec![group("~/small", 1, false), Row::Entry("zeta".into())]
    );
    t.press(KeyCode::Esc);

    // a new skill in a folded group is shown, its group opened
    t.keys("g").keys("h");
    skill_tree(&t.home.join("big"), &["s13"]);
    t.keys("a").keys("~/big").press(KeyCode::Enter);
    t.keys("a").press(KeyCode::Enter);
    assert_eq!(t.app.current(), Some(Row::Entry("s13".into())));

    // what was folded by hand stays so next time
    let T { _d, home, .. } = t;
    let mut t = T::open(_d, home);
    t.keys("2");
    let rows = t.app.rows(Kind::Skills);
    assert_eq!(rows[0], group("~/big", 13, false));
    assert_eq!(rows.last(), Some(&group("~/small", 1, true)));
}

#[test]
fn remove_all_clears_the_library_and_only_what_siu_wrote() {
    let mut t = T::new();
    let src = t.home.join("my-skills");
    skill_tree(&src, &["pdf", "docx"]);
    skill_tree(&t.home.join(".claude/skills"), &["own"]);
    t.keys("r");
    t.keys("a")
        .keys("weather")
        .press(KeyCode::Tab)
        .press(KeyCode::Tab)
        .keys("npx -y @acme/weather");
    t.ctrl('s');
    t.keys("2")
        .keys("a")
        .keys("~/my-skills")
        .press(KeyCode::Enter)
        .keys("a")
        .press(KeyCode::Enter);
    assert_eq!(t.app.store.lib.skills.len(), 2);

    // asks first, saying who loses what; n keeps everything
    t.keys("D");
    let s = t.screen();
    assert!(
        s.contains("Remove all 2 skills?")
            && s.contains("out of Claude Code,")
            && s.contains("Codex."),
        "{s}"
    );
    t.keys("n");
    assert!(t.app.modal.is_none());
    assert_eq!(t.app.store.lib.skills.len(), 2);

    t.keys("D").keys("y");
    assert_eq!(t.toast(), "Removed all 2 skills");
    assert!(t.app.store.lib.skills.is_empty());
    for agent in [".claude", ".codex"] {
        assert!(fs::symlink_metadata(t.home.join(agent).join("skills/pdf")).is_err());
    }
    assert!(!t.app.store.skill_dir("pdf").exists());
    // the user's own folder and the agent's own skill stay
    assert!(src.join("pdf/SKILL.md").exists());
    assert!(t.home.join(".claude/skills/own/SKILL.md").exists());

    // nothing left: nothing to ask
    t.keys("D");
    assert!(t.app.modal.is_none());
    assert_eq!(t.toast(), "No skills to remove");

    // servers likewise; the agent's own server stays
    t.keys("1").keys("D");
    assert!(t.screen().contains("Remove all 1 server?"));
    t.keys("y");
    assert_eq!(t.toast(), "Removed all 1 server");
    assert!(t.app.store.lib.mcp.is_empty());
    let servers = &t.claude()["mcpServers"];
    assert!(servers.get("weather").is_none() && servers.get("mine").is_some());
}

#[test]
fn help_and_tabs() {
    let mut t = T::new();
    t.keys("?");
    assert!(t.screen().contains("this help"));
    t.press(KeyCode::Esc);
    t.press(KeyCode::Tab);
    assert_eq!(t.app.tab, Tab::Skills);
    t.press(KeyCode::BackTab).press(KeyCode::BackTab);
    assert_eq!(t.app.tab, Tab::Discover);
    t.keys("q");
    assert!(t.app.quit);
}

#[test]
fn library_persists_across_launches() {
    let mut t = T::new();
    t.keys("a")
        .keys("keep")
        .press(KeyCode::Tab)
        .press(KeyCode::Tab)
        .keys("run");
    t.ctrl('s');
    let T { _d, home, .. } = t;
    let mut again = T::open(_d, home);
    assert!(again.screen().contains("● keep"));
}

/// Against the real MCP Registry, skills.sh and GitHub:
/// `cargo test -- --ignored online`.
#[test]
#[ignore]
fn online_search_and_install() {
    let mut t = T::new();
    t.app.offline = false;

    // the popular skills are fetched at launch and kept for the next one
    t.app.start();
    t.app.settle();
    assert!(
        t.app.discover.index.len() > 100,
        "{}",
        t.app.discover.index.len()
    );
    let T { _d, home, .. } = t;
    let mut t = T::open(_d, home);
    assert!(!t.app.discover.index.is_empty(), "read from the cache");
    t.app.offline = false;
    t.keys("3").press(KeyCode::Right);
    assert!(t.app.discover.skills.len() >= 50 && t.app.busy.is_empty());

    // descriptions near the cursor arrive a few at a time, in the background
    t.app.tick();
    t.app.settle();
    t.app.tick();
    t.app.settle();
    let known = t
        .app
        .discover
        .skills
        .iter()
        .take(8)
        .filter(|s| t.app.discover.abouts.contains_key(&s.key()))
        .count();
    assert!(known >= 6, "{known} of 8 described");
    t.app.shutdown();
    assert!(t.home.join(".siu/cache/skills-about.json").is_file());
    t.press(KeyCode::Left);
    t.keys("3").keys("/").keys("time").press(KeyCode::Enter);
    t.app.settle();
    assert!(t.app.discover.error.is_none(), "{:?}", t.app.discover.error);
    assert!(
        t.app.discover.servers.iter().any(|m| !m.featured),
        "registry gave nothing"
    );

    t.press(KeyCode::Right);
    t.app.discover.query.text.clear();
    t.keys("/").keys("frontend-design").press(KeyCode::Enter);
    t.app.settle();
    let i = t
        .app
        .discover
        .skills
        .iter()
        .position(|s| s.source == "anthropics/skills" && s.skill_id == "frontend-design");
    let i = i.expect("skills.sh search");
    t.app.discover.sel[1] = i;
    t.press(KeyCode::Enter);
    t.app.settle();
    assert!(
        t.toast().starts_with("Installed frontend-design"),
        "{}",
        t.toast()
    );
    assert!(
        t.home
            .join(".claude/skills/frontend-design/SKILL.md")
            .is_file()
    );
    let s = t.screen();
    assert!(s.contains("✓ frontend-design"), "{s}");

    // update finds it unchanged
    t.keys("2").keys("U");
    t.app.settle();
    assert_eq!(t.toast(), "Already up to date");
}

#[test]
fn skills_discover_is_instant_from_the_cache() {
    let t = T::new();
    let cache = t.home.join(".siu/cache");
    let sk = |source: &str, id: &str, installs: u64| crate::market::MarketSkill {
        source: source.into(),
        skill_id: id.into(),
        name: id.into(),
        installs,
        official: false,
    };
    crate::cache::write(
        &cache,
        "skills-popular",
        &vec![
            sk("vercel-labs/agent-skills", "react-best-practices", 900),
            sk("anthropics/skills", "pdf", 800),
            sk("acme/x", "react-native", 10),
        ],
    );
    let about: std::collections::HashMap<String, Option<String>> = [(
        "anthropics/skills/pdf".to_string(),
        Some("Read and write PDF files".to_string()),
    )]
    .into();
    crate::cache::write(&cache, "skills-about", &about);
    let T { _d, home, .. } = t;
    let mut t = T::open(_d, home);

    // no network: the popular list comes from the cache at once
    t.keys("3").press(KeyCode::Right);
    let s = t.screen();
    assert!(
        s.contains("react-best-practices") && s.contains("pdf"),
        "{s}"
    );
    assert!(!s.contains("Searching"), "{s}");

    // each key narrows the list right away, before any request
    t.keys("/").keys("react");
    let s = t.screen();
    assert!(
        s.contains("react-best-practices")
            && s.contains("react-native")
            && !s.contains("anthropics/skills"),
        "{s}"
    );

    // a description fetched in an earlier run shows without fetching
    t.press(KeyCode::Esc);
    t.app.discover.query.text.clear();
    t.keys("/").keys("pdf").press(KeyCode::Enter);
    let s = t.screen();
    assert!(s.contains("Read and write PDF files"), "{s}");
}

/// Renders screens in both themes into an HTML page of colored cells, for
/// looking at: `SIU_SNAPSHOT=out.html cargo test -- --ignored snapshot`.
#[test]
#[ignore]
fn snapshot() {
    use ratatui::style::{Color, Modifier};
    let Ok(out) = std::env::var("SIU_SNAPSHOT") else {
        return;
    };
    let hex = |c: Color, default: &str| match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Reset => default.to_string(),
        other => panic!("a color outside the theme: {other:?}"),
    };
    let mut html = String::from("<html><body style='margin:0;font:13px/1.25 Menlo,monospace'>");
    // (theme, terminal background, terminal text)
    let terms = [
        (
            crate::theme::Theme::DARK,
            "#1e1e2e",
            "#cdd6f4",
            "dark · Catppuccin",
        ),
        (
            crate::theme::Theme::DARK,
            "#000000",
            "#e5e5e5",
            "dark · black",
        ),
        (
            crate::theme::Theme::LIGHT,
            "#ffffff",
            "#24292f",
            "light · white",
        ),
        (
            crate::theme::Theme::LIGHT,
            "#fdf6e3",
            "#586e75",
            "light · Solarized",
        ),
    ];
    let mut t = T::new();
    let src = t.home.join("my-skills");
    skill_tree(&src, &["pdf", "docx"]);
    skill_tree(&t.home.join(".claude/skills"), &["notes"]);
    t.keys("a")
        .keys("weather")
        .press(KeyCode::Tab)
        .press(KeyCode::Tab)
        .keys("npx -y @acme/weather");
    t.press(KeyCode::Tab)
        .press(KeyCode::Tab)
        .keys("API_KEY=secret123");
    t.ctrl('s');
    t.keys("3")
        .keys("/")
        .keys("deep")
        .press(KeyCode::Enter)
        .press(KeyCode::Enter);
    t.keys("1")
        .keys(" ")
        .press(KeyCode::Down)
        .press(KeyCode::Down);
    t.keys("1")
        .press(KeyCode::Up)
        .press(KeyCode::Up)
        .press(KeyCode::Enter)
        .press(KeyCode::Down);
    t.keys("2")
        .keys("a")
        .keys("~/my-skills")
        .press(KeyCode::Enter)
        .keys("a")
        .press(KeyCode::Enter);
    t.keys("r");
    t.app.toast = None;
    type Screen = (&'static str, fn(&mut T));
    let screens: [Screen; 4] = [
        ("MCP · agents panel", |t: &mut T| {
            t.keys("1");
        }),
        ("Skills", |t: &mut T| {
            t.press(KeyCode::Esc).keys("2");
        }),
        ("Discover", |t: &mut T| {
            t.keys("3").press(KeyCode::Esc);
            t.app.discover.query.text.clear();
            t.keys("j");
        }),
        ("Form", |t: &mut T| {
            t.keys("1")
                .keys("a")
                .keys("my-api")
                .press(KeyCode::Tab)
                .press(KeyCode::Right);
            t.app.notify(
                crate::app::ToastKind::Ok,
                "Installed deepwiki → Claude Code, Codex",
            );
        }),
    ];
    for (name, go) in screens {
        go(&mut t);
        for (theme, bg, fg, label) in terms {
            t.app.theme = theme;
            t.screen();
            let buf = t.term.backend().buffer().clone();
            html.push_str(&format!("<div style='display:inline-block;vertical-align:top;margin:8px;background:{bg};color:{fg};padding:6px'><div style='font:bold 12px sans-serif;color:{fg};opacity:.6'>{name} — {label}</div><pre style='margin:0'>"));
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    let c = &buf[(x, y)];
                    let mut style = format!("color:{};background:{}", hex(c.fg, fg), hex(c.bg, bg));
                    if c.modifier.contains(Modifier::BOLD) {
                        style.push_str(";font-weight:bold");
                    }
                    if c.modifier.contains(Modifier::UNDERLINED) {
                        style.push_str(";text-decoration:underline");
                    }
                    let sym = c.symbol().replace('&', "&amp;").replace('<', "&lt;");
                    html.push_str(&format!("<span style='{style}'>{sym}</span>"));
                }
                html.push('\n');
            }
            html.push_str("</pre></div>");
        }
        html.push_str("<br>");
    }
    t.press(KeyCode::Esc);
    fs::write(out, html).unwrap();
}
