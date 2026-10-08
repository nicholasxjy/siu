//! Drawing the app.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Focus, Form, FormPurpose, Kind, Modal, Row, Tab, ToastKind, tilde};
use crate::library::{Source, State};
use crate::market::Place;
use crate::theme::Theme;

thread_local! {
    /// The theme of the frame being drawn.
    static THEME: std::cell::Cell<Theme> = const { std::cell::Cell::new(Theme::DARK) };
}

fn t() -> Theme {
    THEME.with(|c| c.get())
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn dim() -> Style {
    Style::new().fg(t().muted)
}

/// The mark beside the selected row: seen whatever the background.
fn sel_mark() -> Line<'static> {
    Line::styled("▎", Style::new().fg(t().accent))
}

/// A list that marks its selected row with a bar and a background.
fn list<'a>(items: Vec<ListItem<'a>>, block: Block<'a>, focused: bool) -> List<'a> {
    let style = Style::new().bg(t().sel_bg);
    List::new(items)
        .block(block)
        .highlight_style(if focused {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        })
        .highlight_symbol(sel_mark())
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always)
}

fn block(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Style::new().fg(t().accent)
        } else {
            Style::new().fg(t().faint)
        })
        // titles are text: they don't take the faint color of the lines
        .title_style(Style::new().fg(ratatui::style::Color::Reset))
        .title(title.into())
}

/// `s` cut to `w` columns, with an ellipsis when cut, padded to `w`.
fn fit(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    if s.width() <= w {
        out.push_str(s);
        used = s.width();
    } else {
        for c in s.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw + 1 > w {
                break;
            }
            out.push(c);
            used += cw;
        }
        out.push('…');
        used += 1;
    }
    out.push_str(&" ".repeat(w.saturating_sub(used)));
    out
}

/// A column: `s` fitted to `w` columns, the last one kept as a gap.
fn col(s: &str, w: usize) -> String {
    format!("{} ", fit(s, w.saturating_sub(1)))
}

/// Words of `s` in lines of at most `w` columns.
fn wrap(s: &str, w: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if !cur.is_empty() && cur.width() + 1 + word.width() > w {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `wrap`, keeping a blank line as one.
fn wrap_or_blank(w: usize) -> impl Fn(&str) -> Vec<String> {
    move |s| {
        let v = wrap(s, w);
        if v.is_empty() { vec![String::new()] } else { v }
    }
}

fn mask(key: &str, v: &str) -> String {
    let k = key.to_uppercase();
    let secret = ["KEY", "TOKEN", "SECRET", "PASS", "AUTH", "COOKIE"]
        .iter()
        .any(|s| k.contains(s));
    if !secret || v.chars().count() <= 4 {
        return v.to_string();
    }
    let tail: String = v
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("••••{tail}")
}

fn ago(t: u64) -> String {
    let now = crate::library::now();
    if t == 0 || t > now {
        return "—".into();
    }
    let d = now - t;
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", d / 60),
        3600..86400 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86400),
    }
}

fn count(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    THEME.with(|c| c.set(app.theme));
    let [header, main, status] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .areas(f.area());
    draw_header(f, app, header);
    match app.tab {
        Tab::Mcp => draw_library(f, app, main, Kind::Mcp),
        Tab::Skills => draw_library(f, app, main, Kind::Skills),
        Tab::Discover => draw_discover(f, app, main),
    }
    draw_status(f, app, status);
    match &app.modal {
        Some(Modal::Form(form)) => draw_form(f, app, form),
        Some(Modal::Picker(_)) => draw_picker(f, app),
        Some(Modal::Confirm(c)) => {
            // the body's lines as wrapped in the dialog
            const W: usize = 64 - 4;
            let mut lines: Vec<Line> = c
                .body
                .lines()
                .flat_map(wrap_or_blank(W))
                .map(Line::from)
                .collect();
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled(" y ", Style::new().fg(t().on_accent).bg(t().accent)),
                Span::raw(" confirm   "),
                Span::styled("n", Style::new().bold()),
                Span::styled(" / esc  cancel", dim()),
            ]));
            let body = Text::from(lines);
            let area = centered(f.area(), W as u16 + 4, body.height() as u16 + 4);
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(body).wrap(Wrap { trim: false }).block(
                    block(Line::from(format!(" {} ", c.title)).bold(), true)
                        .padding(ratatui::widgets::Padding::horizontal(1)),
                ),
                area,
            );
        }
        Some(Modal::Help) => draw_help(f),
        None => {}
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(
            " siu ",
            Style::new().fg(t().on_accent).bg(t().accent).bold(),
        ),
        Span::raw("  "),
    ];
    let counts = [app.store.lib.mcp.len(), app.store.lib.skills.len()];
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let (name, n) = match tab {
            Tab::Mcp => ("MCP", Some(counts[0])),
            Tab::Skills => ("Skills", Some(counts[1])),
            Tab::Discover => ("Discover", None),
        };
        let on = *tab == app.tab;
        spans.push(Span::styled(format!("{} ", i + 1), dim()));
        spans.push(Span::styled(
            name,
            if on {
                Style::new().fg(t().accent).bold().underlined()
            } else {
                Style::new()
            },
        ));
        if let Some(n) = n {
            spans.push(Span::styled(
                format!(" {n}"),
                if on {
                    Style::new().fg(t().accent)
                } else {
                    dim()
                },
            ));
        }
        spans.push(Span::raw("    "));
    }
    let left = Line::from(spans);
    let detected: Vec<_> = app.store.detected().map(|a| a.name).collect();
    let right = if detected.is_empty() {
        Line::from(Span::styled("no agents found ", Style::new().fg(t().warn)))
    } else {
        Line::from(vec![
            Span::styled(
                format!("{} agents ", detected.len()),
                Style::new().fg(t().ok),
            ),
            Span::styled("detected ", dim()),
        ])
    };
    let [l, r] = Layout::horizontal([
        Constraint::Min(10),
        Constraint::Length(right.width() as u16),
    ])
    .areas(Rect { height: 1, ..area });
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(right), r);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let hints: &[(&str, &str)] = match (&app.modal, app.tab, app.focus) {
        (Some(Modal::Form(_)), ..) => &[
            ("tab", "next"),
            ("⏎", "next/save"),
            ("ctrl-s", "save"),
            ("esc", "cancel"),
        ],
        (Some(Modal::Picker(_)), ..) => &[
            ("space", "pick"),
            ("a", "all"),
            ("⏎", "install"),
            ("esc", "cancel"),
        ],
        (Some(_), ..) => &[("esc", "close")],
        _ if app.filtering => &[("type", "filter"), ("⏎", "done"), ("esc", "clear")],
        (_, Tab::Discover, _) if app.discover.editing => &[
            ("type", "search"),
            ("tab", "mcp/skills"),
            ("↓", "results"),
            ("esc", "done"),
        ],
        (_, Tab::Discover, _) => &[
            ("/", "search"),
            ("←→", "mcp/skills"),
            ("⏎", "install"),
            ("tab", "next tab"),
            ("?", "help"),
        ],
        (_, _, Focus::Agents) => &[
            ("space", "toggle"),
            ("a", "all"),
            ("n", "none"),
            ("esc", "back"),
        ],
        (_, Tab::Skills, _) if matches!(app.current(), Some(Row::Group { .. })) => &[
            ("⏎", "fold/unfold"),
            ("←→", "fold/unfold"),
            ("/", "filter"),
            ("?", "help"),
        ],
        (_, Tab::Skills, _) if matches!(app.current(), Some(Row::Found(_))) => {
            &[("⏎/i", "import"), ("/", "filter"), ("?", "help")]
        }
        (_, Tab::Mcp, _) if matches!(app.current(), Some(Row::Found(_))) => {
            &[("⏎/i", "import"), ("/", "filter"), ("?", "help")]
        }
        (_, Tab::Skills, _) => &[
            ("⏎", "agents"),
            ("space", "on/off"),
            ("a", "add"),
            ("u", "update"),
            ("U", "update all"),
            ("d", "delete"),
            ("?", "help"),
        ],
        _ => &[
            ("⏎", "agents"),
            ("space", "on/off"),
            ("a", "add"),
            ("e", "edit"),
            ("d", "delete"),
            ("/", "filter"),
            ("?", "help"),
        ],
    };
    let right = if let Some(toast) = &app.toast {
        let (icon, color) = match toast.kind {
            ToastKind::Ok => ("✓ ", t().ok),
            ToastKind::Err => ("✗ ", t().err),
            ToastKind::Info => ("• ", t().accent),
        };
        Line::from(vec![
            Span::styled(icon, Style::new().fg(color)),
            Span::styled(toast.text.clone(), Style::new().fg(color)),
            Span::raw(" "),
        ])
    } else if let Some(b) = app.busy.last() {
        Line::from(vec![
            Span::styled(
                SPINNER[app.frame % SPINNER.len()],
                Style::new().fg(t().accent),
            ),
            Span::styled(format!(" {b}… "), dim()),
        ])
    } else {
        Line::raw("")
    };
    let rw = (right.width() as u16).min(area.width * 2 / 3);
    let room = area.width.saturating_sub(rw + 2) as usize;
    let mut spans = vec![Span::raw(" ")];
    let mut used = 1;
    for (k, v) in hints {
        let piece = format!(" {v}  ");
        if used + k.width() + piece.width() > room {
            break;
        }
        used += k.width() + piece.width();
        spans.push(Span::styled(*k, Style::new().fg(t().accent)));
        spans.push(Span::styled(piece, dim()));
    }
    let [l, r] = Layout::horizontal([Constraint::Min(0), Constraint::Length(rw)]).areas(area);
    f.render_widget(Paragraph::new(Line::from(spans)), l);
    f.render_widget(Paragraph::new(right).right_aligned(), r);
}

// ---- MCP and Skills ---------------------------------------------------------------

fn draw_library(f: &mut Frame, app: &mut App, area: Rect, kind: Kind) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(area);
    let rows = app.rows(kind);
    let w = left.width.saturating_sub(4) as usize;
    let agents = app.panel_agents(kind);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| match r {
            Row::Group { label, n, folded } => {
                let n = n.to_string();
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if *folded { "▸ " } else { "▾ " },
                        Style::new().fg(t().accent),
                    ),
                    Span::styled(
                        fit(label, w.saturating_sub(n.width() + 3)),
                        Style::new().fg(t().accent).bold(),
                    ),
                    Span::styled(format!(" {n}"), dim()),
                ]))
            }
            Row::Header => ListItem::new(Line::from(Span::styled(
                "  found in agents · not managed by siu".to_string(),
                dim().add_modifier(Modifier::ITALIC),
            ))),
            Row::Entry(name) => {
                let (enabled, on, total, tag) = match kind {
                    Kind::Mcp => {
                        let e = app.store.server(name).unwrap();
                        let states: Vec<_> = agents
                            .iter()
                            .map(|a| app.store.server_state(e, app.store.agent(a).unwrap()))
                            .collect();
                        let on = e
                            .agents
                            .iter()
                            .filter(|a| agents.contains(&a.as_str()))
                            .count();
                        let total = states
                            .iter()
                            .filter(|s| !matches!(s, State::Unsupported(_)))
                            .count();
                        (e.enabled, on, total, e.server.transport.label().to_string())
                    }
                    Kind::Skills => {
                        let e = app.store.skill(name).unwrap();
                        let on = e
                            .agents
                            .iter()
                            .filter(|a| agents.contains(&a.as_str()))
                            .count();
                        let tag = match &e.source {
                            Some(Source::Github { .. }) => "github",
                            Some(Source::Local { .. }) => "local",
                            None => "imported",
                        };
                        (e.enabled, on, agents.len(), tag.to_string())
                    }
                };
                let (dot, color) = if !enabled {
                    ("‖", t().warn)
                } else if on == 0 {
                    ("○", t().muted)
                } else {
                    ("●", t().ok)
                };
                let right = format!("{tag:>8} {on:>2}/{total:<2}");
                let name_w = w.saturating_sub(right.width() + 2);
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{dot} "), Style::new().fg(color)),
                    Span::styled(
                        fit(name, name_w),
                        if enabled { Style::new() } else { dim() },
                    ),
                    Span::styled(right, dim()),
                ]))
            }
            Row::Found(name) => {
                let at = match kind {
                    Kind::Mcp => app
                        .found_servers
                        .iter()
                        .find(|x| &x.server.name == name)
                        .map(|x| x.agents.join(", ")),
                    Kind::Skills => app.found_skills.iter().find(|x| &x.name == name).map(|x| {
                        x.at.iter()
                            .map(|(a, _)| a.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }),
                }
                .unwrap_or_default();
                let name_w = (w.saturating_sub(3)).min(w * 3 / 5);
                ListItem::new(Line::from(vec![
                    Span::styled("◇ ", Style::new().fg(t().warn)),
                    Span::raw(col(name, name_w)),
                    Span::styled(fit(&at, w.saturating_sub(name_w + 2)), dim()),
                ]))
            }
        })
        .collect();

    let title = match kind {
        Kind::Mcp => " MCP servers ",
        Kind::Skills => " Skills ",
    };
    let filter = &app.filter[kind as usize].text;
    let mut title_spans = vec![Span::styled(title, Style::new().bold())];
    if app.filtering || !filter.is_empty() {
        title_spans.push(Span::styled("/ ", Style::new().fg(t().accent)));
        title_spans.push(Span::raw(format!(
            "{filter}{} ",
            if app.filtering { "▏" } else { "" }
        )));
    }
    let list_focused = app.focus == Focus::List;
    let lb =
        block(Line::from(title_spans), list_focused).padding(ratatui::widgets::Padding::right(1));
    if items.is_empty() {
        let msg = if !filter.is_empty() {
            vec![Line::styled("Nothing matches the filter.", dim())]
        } else {
            let what = if kind == Kind::Mcp {
                "MCP servers"
            } else {
                "skills"
            };
            vec![
                Line::raw(""),
                Line::from(format!("No {what} yet.")).bold(),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("a", Style::new().fg(t().accent)),
                    Span::styled(" add one yourself", dim()),
                ]),
                Line::from(vec![
                    Span::styled("3", Style::new().fg(t().accent)),
                    Span::styled(" discover and install from the web", dim()),
                ]),
            ]
        };
        f.render_widget(Paragraph::new(msg).centered().block(lb), left);
    } else {
        let mut state = ListState::default().with_selected(Some(app.sel[kind as usize]));
        f.render_stateful_widget(list(items, lb, list_focused), left, &mut state);
    }

    match app.current() {
        Some(Row::Entry(name)) => draw_entry(f, app, right, kind, &name),
        Some(Row::Found(name)) => draw_found(f, app, right, kind, &name),
        Some(Row::Group { label, folded, .. }) => draw_group(f, app, right, &label, folded),
        _ => f.render_widget(block("", false), right),
    }
}

/// A group of skills: what's in it, and how many agents have each.
fn draw_group(f: &mut Frame, app: &App, area: Rect, label: &str, folded: bool) {
    let skills: Vec<_> = app
        .store
        .lib
        .skills
        .iter()
        .filter(|e| app.skill_group(e).1 == label)
        .collect();
    let paused = skills.iter().filter(|e| !e.enabled).count();
    let mut lines = vec![Line::from(vec![
        Span::raw(format!(
            "{} skill{}",
            skills.len(),
            if skills.len() == 1 { "" } else { "s" }
        )),
        Span::styled(
            if paused > 0 {
                format!(" · {paused} paused")
            } else {
                String::new()
            },
            Style::new().fg(t().warn),
        ),
    ])];
    lines.push(Line::raw(""));
    let mut names: Vec<&str> = skills.iter().map(|e| e.name.as_str()).collect();
    names.sort();
    let w = area.width.saturating_sub(4) as usize;
    for line in wrap(&names.join(", "), w) {
        lines.push(Line::styled(line, dim()));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("⏎", Style::new().fg(t().accent)),
        Span::styled(
            if folded {
                " show its skills"
            } else {
                " fold them away"
            },
            dim(),
        ),
    ]));
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            block(Line::from(format!(" {label} ")).bold(), false)
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        area,
    );
}

fn kv(k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:<9}"), dim()),
        Span::raw(v.into()),
    ])
}

fn section(title: &str) -> Line<'static> {
    Line::styled(title.to_uppercase(), dim().add_modifier(Modifier::BOLD))
}

fn draw_entry(f: &mut Frame, app: &App, area: Rect, kind: Kind, name: &str) {
    let home = &app.env.home;
    let mut lines: Vec<Line> = vec![];
    let enabled;
    match kind {
        Kind::Mcp => {
            let e = app.store.server(name).unwrap();
            let s = &e.server;
            enabled = e.enabled;
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", s.transport.label()),
                    Style::new().fg(t().accent),
                ),
                Span::raw(if s.remote() {
                    s.url.clone()
                } else {
                    s.command.clone()
                }),
            ]));
            if !s.remote() && !s.args.is_empty() {
                lines.push(kv("args", crate::app::join_args(&s.args)));
            }
            for (k, v) in &s.env {
                lines.push(kv("env", format!("{k}={}", mask(k, v))));
            }
            for (k, v) in &s.headers {
                lines.push(kv("header", format!("{k}: {}", mask(k, v))));
            }
            if let Some(o) = &e.origin {
                lines.push(kv("from", o.clone()));
            }
        }
        Kind::Skills => {
            let e = app.store.skill(name).unwrap();
            enabled = e.enabled;
            if !e.description.is_empty() {
                lines.push(Line::raw(e.description.clone()));
                lines.push(Line::raw(""));
            }
            lines.push(kv(
                "source",
                e.source
                    .as_ref()
                    .map(|s| s.label())
                    .unwrap_or_else(|| "imported from an agent".into()),
            ));
            lines.push(kv("updated", ago(e.updated)));
            lines.push(kv("files", tilde(&app.store.skill_dir(name), home)));
        }
    }
    if !enabled {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "‖ Paused — kept in the library, written to no agent. space to resume.",
            Style::new().fg(t().warn),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(section("Agents"));
    let focus = app.focus == Focus::Agents;
    let agents = app.panel_agents(kind);
    let name_w = agents
        .iter()
        .filter_map(|a| app.store.agent(a))
        .map(|a| a.name.width())
        .max()
        .unwrap_or(10)
        + 2;
    for (i, id) in agents.iter().enumerate() {
        let a = app.store.agent(id).unwrap();
        let state = match kind {
            Kind::Mcp => app.store.server_state(app.store.server(name).unwrap(), a),
            Kind::Skills => app.store.skill_state(app.store.skill(name).unwrap(), a),
        };
        let (mark, color, note) = match state {
            State::On => ("[✓]", t().ok, String::new()),
            State::Off => ("[ ]", t().muted, String::new()),
            State::Paused => ("[‖]", t().warn, "paused".into()),
            State::Unsupported(why) => ("[–]", t().muted, why.to_string()),
        };
        let where_ = match kind {
            Kind::Mcp => a.mcp.as_ref().map(|m| tilde(&m.path, home)),
            Kind::Skills => a.skills.as_ref().map(|p| tilde(p, home)),
        }
        .unwrap_or_default();
        let cursor = focus && i == app.agent_sel;
        let row_style = if cursor {
            Style::new().bg(t().sel_bg)
        } else {
            Style::new()
        };
        lines.push(
            Line::from(vec![
                Span::styled(
                    if cursor { "› " } else { "  " },
                    Style::new().fg(t().accent),
                ),
                Span::styled(mark, Style::new().fg(color)),
                Span::raw(" "),
                Span::styled(
                    fit(a.name, name_w),
                    if matches!(state, State::Unsupported(_)) {
                        dim()
                    } else {
                        Style::new()
                    },
                ),
                Span::styled(if note.is_empty() { where_ } else { note }, dim()),
            ])
            .style(row_style),
        );
    }
    if agents.is_empty() {
        lines.push(Line::styled("  No installed agent can take this.", dim()));
    }
    if !focus && !agents.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("⏎", Style::new().fg(t().accent)),
            Span::styled(" choose agents", dim()),
        ]));
    }
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(name.to_string(), Style::new().bold()),
        Span::raw(" "),
    ]);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block(title, focus).padding(ratatui::widgets::Padding::horizontal(1))),
        area,
    );
}

fn draw_found(f: &mut Frame, app: &App, area: Rect, kind: Kind, name: &str) {
    let home = &app.env.home;
    let mut lines: Vec<Line> = vec![];
    match kind {
        Kind::Mcp => {
            let Some(x) = app.found_servers.iter().find(|x| x.server.name == name) else {
                return;
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", x.server.transport.label()),
                    Style::new().fg(t().accent),
                ),
                Span::raw(x.server.summary()),
            ]));
            lines.push(Line::raw(""));
            lines.push(section("Found in"));
            for a in &x.agents {
                let ag = app.store.agent(a).unwrap();
                lines.push(Line::from(vec![
                    Span::raw(format!("  {} ", ag.name)),
                    Span::styled(
                        ag.mcp
                            .as_ref()
                            .map(|m| tilde(&m.path, home))
                            .unwrap_or_default(),
                        dim(),
                    ),
                ]));
            }
        }
        Kind::Skills => {
            let Some(x) = app.found_skills.iter().find(|x| x.name == name) else {
                return;
            };
            if !x.description.is_empty() {
                lines.push(Line::raw(x.description.clone()));
                lines.push(Line::raw(""));
            }
            lines.push(section("Found in"));
            for (a, p) in &x.at {
                let ag = app.store.agent(a).unwrap();
                lines.push(Line::from(vec![
                    Span::raw(format!("  {} ", ag.name)),
                    Span::styled(tilde(p, home), dim()),
                ]));
            }
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "siu didn't install this; it leaves it alone.",
        dim(),
    ));
    lines.push(Line::from(vec![
        Span::styled("⏎", Style::new().fg(t().accent)),
        Span::styled(
            " import it to turn it on and off for every agent from here",
            dim(),
        ),
    ]));
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(name.to_string(), Style::new().bold()),
        Span::styled(" · not managed ", Style::new().fg(t().warn)),
    ]);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block(title, false).padding(ratatui::widgets::Padding::horizontal(1))),
        area,
    );
}

// ---- Discover ----------------------------------------------------------------------

fn draw_discover(f: &mut Frame, app: &mut App, area: Rect) {
    let [search, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);
    let d = &app.discover;
    let kinds = [(Kind::Mcp, "MCP servers"), (Kind::Skills, "Skills")];
    let mut title = vec![Span::raw(" ")];
    for (i, (k, label)) in kinds.iter().enumerate() {
        if i > 0 {
            title.push(Span::styled(" │ ", dim()));
        }
        title.push(if *k == d.kind {
            Span::styled(*label, Style::new().fg(t().accent).bold())
        } else {
            Span::styled(*label, dim())
        });
    }
    title.push(Span::raw(" "));
    let placeholder = match d.kind {
        Kind::Mcp => "Search the official MCP Registry…",
        Kind::Skills => "Search skills.sh…",
    };
    let line = if d.query.text.is_empty() && !d.editing {
        Line::from(vec![
            Span::styled("/ ", Style::new().fg(t().accent)),
            Span::styled(placeholder, dim()),
        ])
    } else {
        let mut v = vec![
            Span::styled("› ", Style::new().fg(t().accent)),
            Span::raw(d.query.text.clone()),
        ];
        if d.editing {
            v.push(Span::styled("▏", Style::new().fg(t().accent)));
            if d.query.text.is_empty() {
                v.push(Span::styled(placeholder, dim()));
            }
        }
        Line::from(v)
    };
    let source = match d.kind {
        Kind::Mcp => "registry.modelcontextprotocol.io ",
        Kind::Skills => "skills.sh ",
    };
    f.render_widget(
        Paragraph::new(line).block(
            block(Line::from(title), d.editing)
                .padding(ratatui::widgets::Padding::horizontal(1))
                .title_bottom(Line::styled(source, dim()).right_aligned()),
        ),
        search,
    );

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(body);
    let w = left.width.saturating_sub(4) as usize;
    let i = d.kind as usize;
    let loading = d.loading[i];
    let spin = SPINNER[app.frame % SPINNER.len()];
    let items: Vec<ListItem> = match d.kind {
        Kind::Mcp => d
            .servers
            .iter()
            .map(|m| {
                let have = app.have_server(m).is_some();
                let (mark, color) = if have {
                    ("✓ ", t().ok)
                } else if m.featured {
                    ("★ ", t().muted)
                } else {
                    ("  ", t().muted)
                };
                let tag = m.server.transport.label();
                let pub_w = (w / 3).min(18);
                let name_w = w.saturating_sub(pub_w + 2 + 5);
                ListItem::new(Line::from(vec![
                    Span::styled(mark, Style::new().fg(color)),
                    Span::raw(col(&m.title, name_w)),
                    Span::styled(col(&m.publisher, pub_w), dim()),
                    Span::styled(format!("{tag:>5}"), dim()),
                ]))
            })
            .collect(),
        Kind::Skills => d
            .skills
            .iter()
            .map(|s| {
                let have = app.have_skill(s).is_some();
                let (mark, color) = if have {
                    ("✓ ", t().ok)
                } else if s.official {
                    ("★ ", t().muted)
                } else {
                    ("  ", t().muted)
                };
                let n = count(s.installs);
                let src_w = (w / 3).min(24);
                let name_w = w.saturating_sub(src_w + 2 + 6);
                ListItem::new(Line::from(vec![
                    Span::styled(mark, Style::new().fg(color)),
                    Span::raw(col(&s.name, name_w)),
                    Span::styled(col(&s.source, src_w), dim()),
                    Span::styled(format!("{n:>6}"), dim()),
                ]))
            })
            .collect(),
    };
    let count_label = if loading {
        format!(" {spin} searching ")
    } else {
        format!(" {} ", items.len())
    };
    let lb = block(
        Line::from(vec![
            Span::styled(" Results ", Style::new().bold()),
            Span::styled(count_label, dim()),
        ]),
        !d.editing,
    )
    .padding(ratatui::widgets::Padding::right(1));
    if items.is_empty() {
        let msg = if loading {
            Line::styled(format!("{spin} Searching…"), dim())
        } else if let Some(e) = &d.error {
            Line::styled(e.clone(), Style::new().fg(t().err))
        } else if d.shown[i].is_none() && d.kind == Kind::Mcp {
            Line::styled("Press ⏎ to search the registry", dim())
        } else {
            Line::styled("Nothing found", dim())
        };
        f.render_widget(
            Paragraph::new(vec![Line::raw(""), msg])
                .centered()
                .wrap(Wrap { trim: true })
                .block(lb),
            left,
        );
    } else {
        let mut state = ListState::default().with_selected(Some(d.sel[i]));
        f.render_stateful_widget(list(items, lb, !d.editing), left, &mut state);
    }

    let mut lines: Vec<Line> = vec![];
    let mut title = Line::raw("");
    match d.kind {
        Kind::Mcp => {
            if let Some(m) = d.servers.get(d.sel[0]) {
                title = Line::from(vec![
                    Span::raw(" "),
                    Span::styled(m.title.clone(), Style::new().bold()),
                    Span::raw(" "),
                ]);
                let mut by = vec![Span::styled(m.publisher.clone(), dim())];
                if m.featured {
                    by.push(Span::styled("  ★ picked by siu", Style::new().fg(t().star)));
                }
                lines.push(Line::from(by));
                lines.push(Line::raw(""));
                if !m.description.is_empty() {
                    lines.push(Line::raw(m.description.clone()));
                    lines.push(Line::raw(""));
                }
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} ", m.server.transport.label()),
                        Style::new().fg(t().accent),
                    ),
                    Span::raw(m.server.summary()),
                ]));
                lines.push(kv("name", m.server.name.clone()));
                if !m.homepage.is_empty() {
                    lines.push(kv("home", m.homepage.clone()));
                }
                if !m.inputs.is_empty() {
                    lines.push(Line::raw(""));
                    lines.push(section("Asks for"));
                    for inp in &m.inputs {
                        let place = match inp.place {
                            Place::Env => "env",
                            Place::Header => "header",
                            Place::Arg => "arg",
                        };
                        lines.push(Line::from(vec![
                            Span::raw(format!("  {} ", inp.label)),
                            Span::styled(
                                format!(
                                    "{place}{}{}",
                                    if inp.secret { " · secret" } else { "" },
                                    if inp.required { "" } else { " · optional" }
                                ),
                                dim(),
                            ),
                        ]));
                    }
                }
                lines.push(Line::raw(""));
                lines.push(match app.have_server(m) {
                    Some(n) => Line::styled(format!("✓ Installed as {n}"), Style::new().fg(t().ok)),
                    None => {
                        let n = app.store.default_server_agents(&m.server).len();
                        Line::from(vec![
                            Span::styled("⏎", Style::new().fg(t().accent)),
                            Span::styled(
                                format!(" install for {n} agent{}", if n == 1 { "" } else { "s" }),
                                dim(),
                            ),
                        ])
                    }
                });
            }
        }
        Kind::Skills => {
            if let Some(s) = d.skills.get(d.sel[1]) {
                title = Line::from(vec![
                    Span::raw(" "),
                    Span::styled(s.name.clone(), Style::new().bold()),
                    Span::raw(" "),
                ]);
                let mut by = vec![Span::styled(format!("github.com/{}", s.source), dim())];
                if s.official {
                    by.push(Span::styled("  ★ official", Style::new().fg(t().star)));
                }
                lines.push(Line::from(by));
                lines.push(Line::raw(""));
                match d.abouts.get(&s.key()) {
                    Some(Some(about)) => lines.push(Line::raw(about.clone())),
                    Some(None) => lines.push(Line::styled("No description.", dim())),
                    None if app.offline => {}
                    None => lines.push(Line::styled(format!("{spin} loading description…"), dim())),
                }
                lines.push(Line::raw(""));
                lines.push(kv("installs", count(s.installs)));
                lines.push(kv("skill", s.skill_id.clone()));
                lines.push(Line::raw(""));
                lines.push(match app.have_skill(s) {
                    Some(n) => Line::styled(format!("✓ Installed as {n}"), Style::new().fg(t().ok)),
                    None => {
                        let n = app.store.default_skill_agents().len();
                        Line::from(vec![
                            Span::styled("⏎", Style::new().fg(t().accent)),
                            Span::styled(
                                format!(" install for {n} agent{}", if n == 1 { "" } else { "s" }),
                                dim(),
                            ),
                        ])
                    }
                });
            }
        }
    }
    if let Some(e) = &d.error
        && !lines.is_empty()
    {
        lines.push(Line::raw(""));
        lines.push(Line::styled(e.clone(), Style::new().fg(t().err)));
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block(title, false).padding(ratatui::widgets::Padding::horizontal(1))),
        right,
    );
}

// ---- modals ------------------------------------------------------------------------

fn draw_form(f: &mut Frame, app: &App, form: &Form) {
    let vis = form.visible();
    let mut lines: Vec<Line> = vec![Line::raw("")];
    let label_w = vis
        .iter()
        .map(|&i| form.fields[i].label.width())
        .max()
        .unwrap_or(8)
        .clamp(8, 22);
    let form_w: u16 = 78;
    let text_w = (form_w as usize).saturating_sub(label_w + 8).max(10);
    for &i in &vis {
        let fld = &form.fields[i];
        let focused = i == form.focus;
        let label_style = if focused {
            Style::new().fg(t().accent).bold()
        } else {
            dim()
        };
        let label = fit(&fld.label, label_w);
        let mut spans = vec![Span::styled(
            format!("{:>w$}  ", label.trim_end(), w = label_w),
            label_style,
        )];
        if !fld.choices.is_empty() {
            for c in &fld.choices {
                if *c == fld.input.text {
                    spans.push(Span::styled(
                        format!(" {c} "),
                        if focused {
                            Style::new().fg(t().on_accent).bg(t().accent).bold()
                        } else {
                            Style::new().fg(t().accent).bg(t().sel_bg)
                        },
                    ));
                } else {
                    spans.push(Span::styled(format!(" {c} "), dim()));
                }
            }
        } else {
            let text = if fld.secret && !focused && !fld.input.text.is_empty() {
                "•".repeat(fld.input.text.chars().count().min(24))
            } else {
                fld.input.text.clone()
            };
            spans.push(Span::raw(text));
            if focused {
                spans.push(Span::styled("▏", Style::new().fg(t().accent)));
            }
        }
        lines.push(Line::from(spans));
        if focused && !fld.hint.is_empty() {
            for part in wrap(&fld.hint, text_w) {
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(label_w + 2)),
                    Span::styled(part, dim()),
                ]));
            }
        }
        lines.push(Line::raw(""));
    }
    if let FormPurpose::MarketInputs(m) = &form.purpose {
        lines.insert(0, Line::styled(format!(" {}", m.server.summary()), dim()));
    }
    if let Some(e) = &form.error {
        lines.push(Line::styled(format!(" ✗ {e}"), Style::new().fg(t().err)));
    }
    let _ = app;
    let h = lines.len() as u16 + 3;
    let area = centered(f.area(), form_w, h);
    f.render_widget(Clear, area);
    let footer = Line::from(vec![
        Span::styled(" ⏎", Style::new().fg(t().accent)),
        Span::styled(" next  ", dim()),
        Span::styled("ctrl-s", Style::new().fg(t().accent)),
        Span::styled(" save  ", dim()),
        Span::styled("esc", Style::new().fg(t().accent)),
        Span::styled(" cancel ", dim()),
    ]);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            block(Line::from(format!(" {} ", form.title)).bold(), true)
                .title_bottom(footer.right_aligned())
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        area,
    );
}

fn draw_picker(f: &mut Frame, app: &App) {
    let Some(Modal::Picker(p)) = &app.modal else {
        return;
    };
    let area = centered(f.area(), 84, (p.items.len() as u16 + 4).clamp(8, 28));
    f.render_widget(Clear, area);
    let w = area.width.saturating_sub(4) as usize;
    let name_w = p
        .items
        .iter()
        .map(|i| i.label.width())
        .max()
        .unwrap_or(10)
        .min(w / 2)
        + 2;
    let items: Vec<ListItem> = p
        .items
        .iter()
        .map(|it| {
            let (mark, color) = match (&it.blocked, it.checked) {
                (Some(_), _) => ("[–]", t().muted),
                (None, true) => ("[✓]", t().ok),
                (None, false) => ("[ ]", t().muted),
            };
            let detail = it.blocked.clone().unwrap_or_else(|| it.detail.clone());
            ListItem::new(Line::from(vec![
                Span::styled(format!("{mark} "), Style::new().fg(color)),
                Span::styled(
                    fit(&it.label, name_w),
                    if it.blocked.is_some() {
                        dim()
                    } else {
                        Style::new()
                    },
                ),
                Span::styled(fit(&detail, w.saturating_sub(name_w + 4)), dim()),
            ]))
        })
        .collect();
    let picked = p.items.iter().filter(|i| i.checked).count();
    let footer = Line::from(vec![
        Span::styled(" space", Style::new().fg(t().accent)),
        Span::styled(" pick  ", dim()),
        Span::styled("a", Style::new().fg(t().accent)),
        Span::styled(" all  ", dim()),
        Span::styled("⏎", Style::new().fg(t().accent)),
        Span::styled(format!(" install {picked}  "), dim()),
        Span::styled("esc", Style::new().fg(t().accent)),
        Span::styled(" cancel ", dim()),
    ]);
    let mut state = ListState::default().with_selected(Some(p.sel));
    let b = block(Line::from(format!(" {} ", p.title)).bold(), true)
        .title_bottom(footer.right_aligned())
        .padding(ratatui::widgets::Padding::right(1));
    f.render_stateful_widget(list(items, b, true), area, &mut state);
}

fn draw_help(f: &mut Frame) {
    let groups: &[(&str, &[(&str, &str)])] = &[
        (
            "Everywhere",
            &[
                ("1 2 3 / tab", "switch tab"),
                ("?", "this help"),
                ("r", "reload from disk"),
                ("t", "dark / light theme"),
                ("q / ctrl-c", "quit"),
            ],
        ),
        (
            "MCP & Skills",
            &[
                ("↑↓ / j k", "move"),
                ("⏎ / →", "choose agents"),
                ("space", "turn on or pause everywhere"),
                ("a", "add (server, or skills from GitHub/folder)"),
                ("e", "edit server"),
                ("u / U", "update skill / all skills"),
                ("d / D", "delete / remove everything"),
                ("i", "import one found in an agent"),
                ("⏎ / ← →", "fold or open a group of skills"),
                ("/", "filter"),
            ],
        ),
        (
            "Agents panel",
            &[
                ("space", "toggle agent"),
                ("a / n", "all / none"),
                ("esc / ←", "back"),
            ],
        ),
        (
            "Discover",
            &[
                ("/", "search"),
                ("← →", "MCP servers / skills"),
                ("⏎", "install"),
            ],
        ),
    ];
    let mut lines = vec![];
    for (title, keys) in groups {
        lines.push(section(title));
        for (k, v) in *keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<14}"), Style::new().fg(t().accent)),
                Span::raw(*v),
            ]));
        }
        lines.push(Line::raw(""));
    }
    lines.push(Line::styled(
        "siu writes only what it installed; everything else in your agents' files stays as it was.",
        dim(),
    ));
    let area = centered(f.area(), 66, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            block(Line::from(" Keys ").bold(), true)
                .padding(ratatui::widgets::Padding::horizontal(1)),
        ),
        area,
    );
}
