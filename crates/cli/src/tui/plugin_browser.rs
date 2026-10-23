//! `/plugins`: browse, install and manage plugins — tabs Discover ·
//! Installed · Marketplaces · Errors, with type-to-search.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dcode_ai_common::plugins::{Registry, installed_in, plugins_dir, read_manifest};
use dcode_ai_core::plugin_installer::{self, KnownMarkets};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

use super::render_helpers::truncate_chars;
use super::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserTab {
    Discover,
    Installed,
    Marketplaces,
    Errors,
}

impl BrowserTab {
    const ALL: [BrowserTab; 4] = [
        BrowserTab::Discover,
        BrowserTab::Installed,
        BrowserTab::Marketplaces,
        BrowserTab::Errors,
    ];
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoverRow {
    pub name: String,
    pub market: String,
    pub description: String,
    pub category: Option<String>,
    pub author: Option<String>,
    pub installed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstalledRow {
    pub name: String,
    pub version: Option<String>,
    pub description: String,
    pub enabled: bool,
    pub adds: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarketRow {
    pub name: String,
    pub source: String,
    pub count: usize,
    pub updated: String,
}

/// Work the browser asks the REPL to do (git and disk, off the UI thread).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserAction {
    Install {
        market: String,
        name: String,
    },
    Uninstall(String),
    SetEnabled(String, bool),
    UpdatePlugin(String),
    AddMarket(String),
    /// Anthropic's default marketplaces not added yet.
    AddDefaultMarkets(Vec<String>),
    UpdateMarket(String),
    RemoveMarket(String),
}

impl BrowserAction {
    /// Progress line while it runs.
    pub fn progress(&self) -> String {
        match self {
            Self::Install { name, .. } => format!("Installing {name}…"),
            Self::Uninstall(n) => format!("Uninstalling {n}…"),
            Self::SetEnabled(n, true) => format!("Enabling {n}…"),
            Self::SetEnabled(n, false) => format!("Disabling {n}…"),
            Self::UpdatePlugin(n) => format!("Updating {n}…"),
            Self::AddMarket(s) => format!("Adding marketplace {s}…"),
            Self::AddDefaultMarkets(s) => {
                format!("Adding Anthropic's marketplaces ({})…", s.len())
            }
            Self::UpdateMarket(n) => format!("Updating marketplace {n}…"),
            Self::RemoveMarket(n) => format!("Removing marketplace {n}…"),
        }
    }

    /// Do the work; returns a one-line result message.
    pub fn run(&self) -> Result<String, String> {
        let plugins = plugins_dir().ok_or("no home directory")?;
        let markets = plugin_installer::marketplaces_dir().ok_or("no home directory")?;
        match self {
            Self::Install { market, name } => {
                let p = plugin_installer::install_from_market(&plugins, &markets, market, name)?;
                Ok(format!(
                    "Installed {}{} — ready in this session",
                    p.name,
                    p.version.map(|v| format!(" v{v}")).unwrap_or_default()
                ))
            }
            Self::Uninstall(n) => {
                plugin_installer::remove(&plugins, n)?;
                Ok(format!("Uninstalled {n}"))
            }
            Self::SetEnabled(n, on) => {
                plugin_installer::set_enabled(&plugins, n, *on)?;
                Ok(format!("{} {n}", if *on { "Enabled" } else { "Disabled" }))
            }
            Self::UpdatePlugin(n) => {
                plugin_installer::update(&plugins, Some(n))?;
                Ok(format!("Updated {n}"))
            }
            Self::AddMarket(s) => {
                let name = plugin_installer::add_market(&markets, s)?;
                Ok(format!("Added marketplace {name}"))
            }
            Self::AddDefaultMarkets(sources) => {
                let (added, errors) = plugin_installer::add_default_markets(&markets, sources);
                if added.is_empty() && !errors.is_empty() {
                    return Err(errors.join("; "));
                }
                let mut msg = format!("Added marketplaces: {}", added.join(", "));
                if !errors.is_empty() {
                    msg.push_str(&format!(" (failed: {})", errors.join("; ")));
                }
                Ok(msg)
            }
            Self::UpdateMarket(n) => {
                plugin_installer::update_market(&markets, n)?;
                Ok(format!("Updated marketplace {n}"))
            }
            Self::RemoveMarket(n) => {
                plugin_installer::remove_market(&markets, n)?;
                Ok(format!("Removed marketplace {n}"))
            }
        }
    }
}

/// What a key did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserKey {
    Handled,
    Close,
    Run(BrowserAction),
}

#[derive(Debug, Clone)]
pub struct PluginBrowser {
    pub tab: BrowserTab,
    pub search: String,
    pub index: usize,
    pub discover: Vec<DiscoverRow>,
    pub installed: Vec<InstalledRow>,
    pub markets: Vec<MarketRow>,
    pub errors: Vec<String>,
    /// Action in flight (shown instead of the hints).
    pub busy: Option<String>,
    /// Last result / error line.
    pub status: Option<(String, bool)>,
    /// `a` on Marketplaces: source being typed.
    pub adding_market: Option<String>,
    /// `x` pressed once on this row; a second `x` confirms.
    pub confirm: Option<String>,
    /// Enter: show the selected plugin's details.
    pub details: bool,
}

impl PluginBrowser {
    /// Read installed plugins and marketplaces from disk.
    pub fn load() -> Self {
        let mut b = Self {
            tab: BrowserTab::Discover,
            search: String::new(),
            index: 0,
            discover: Vec::new(),
            installed: Vec::new(),
            markets: Vec::new(),
            errors: Vec::new(),
            busy: None,
            status: None,
            adding_market: None,
            confirm: None,
            details: false,
        };
        b.reload();
        b
    }

    pub fn reload(&mut self) {
        self.errors.clear();
        self.installed.clear();
        if let Some(dir) = plugins_dir() {
            for p in installed_in(&dir) {
                self.installed.push(InstalledRow {
                    name: p.name().to_string(),
                    version: p.manifest.version.clone(),
                    description: p.manifest.description.clone().unwrap_or_default(),
                    enabled: p.enabled,
                    adds: crate::tui::plugin_summary::contributions(&p),
                    source: Registry::load_from(&dir)
                        .get(p.name())
                        .map(|e| e.source.clone())
                        .unwrap_or_default(),
                });
            }
            for e in Registry::load_from(&dir).plugins {
                if let Err(err) = read_manifest(&dir.join(&e.name)) {
                    self.errors.push(format!("plugin `{}`: {err}", e.name));
                }
            }
        }
        self.discover.clear();
        self.markets.clear();
        if let Some(mdir) = plugin_installer::marketplaces_dir() {
            let (entries, errors) = plugin_installer::all_entries(&mdir);
            self.errors.extend(errors);
            for m in KnownMarkets::load_from(&mdir).markets {
                self.markets.push(MarketRow {
                    count: entries.iter().filter(|(n, _)| *n == m.name).count(),
                    name: m.name,
                    source: m.source,
                    updated: m.updated_at.unwrap_or_else(|| "never".into()),
                });
            }
            for (market, e) in entries {
                self.discover.push(DiscoverRow {
                    installed: self.installed.iter().any(|i| i.name == e.name),
                    name: e.name,
                    market,
                    description: e.description,
                    category: e.category,
                    author: e.author,
                });
            }
            // Anthropic's own listings first (official, then knowledge-work),
            // then the community and other marketplaces.
            let rank = |m: &str| match m {
                "claude-plugins-official" => 0,
                "knowledge-work-plugins" => 1,
                "claude-community" => 3,
                _ => 2,
            };
            self.discover.sort_by(|a, b| {
                rank(&a.market)
                    .cmp(&rank(&b.market))
                    .then_with(|| a.name.cmp(&b.name))
            });
        }
        self.clamp();
    }

    fn matches(&self, hay: &[&str]) -> bool {
        let q = self.search.trim().to_ascii_lowercase();
        q.is_empty()
            || q.split_whitespace()
                .all(|w| hay.iter().any(|h| h.to_ascii_lowercase().contains(w)))
    }

    /// Indexes of rows on the current tab that match the search, best
    /// name matches first.
    pub fn visible(&self) -> Vec<usize> {
        let mut rows = self.matching();
        let q = self.search.trim().to_ascii_lowercase();
        if !q.is_empty() {
            let name = |i: usize| match self.tab {
                BrowserTab::Discover => self.discover[i].name.to_ascii_lowercase(),
                BrowserTab::Installed => self.installed[i].name.to_ascii_lowercase(),
                _ => String::new(),
            };
            // 0 exact, 1 prefix, 2 in name, 3 elsewhere.
            rows.sort_by_key(|&i| {
                let n = name(i);
                if n == q {
                    0
                } else if n.starts_with(&q) {
                    1
                } else if n.contains(&q) {
                    2
                } else {
                    3
                }
            });
        }
        rows
    }

    fn matching(&self) -> Vec<usize> {
        match self.tab {
            BrowserTab::Discover => (0..self.discover.len())
                .filter(|&i| {
                    let r = &self.discover[i];
                    self.matches(&[
                        &r.name,
                        &r.description,
                        r.category.as_deref().unwrap_or(""),
                        &r.market,
                    ])
                })
                .collect(),
            BrowserTab::Installed => (0..self.installed.len())
                .filter(|&i| {
                    let r = &self.installed[i];
                    self.matches(&[&r.name, &r.description])
                })
                .collect(),
            BrowserTab::Marketplaces => (0..self.markets.len()).collect(),
            BrowserTab::Errors => (0..self.errors.len()).collect(),
        }
    }

    fn clamp(&mut self) {
        let n = self.visible().len();
        self.index = self.index.min(n.saturating_sub(1));
    }

    fn switch(&mut self, forward: bool) {
        let i = BrowserTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);
        let n = BrowserTab::ALL.len();
        self.tab = BrowserTab::ALL[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }];
        self.index = 0;
        self.search.clear();
        self.confirm = None;
        self.details = false;
    }

    /// The selected row's real index on the current tab.
    fn selected(&self) -> Option<usize> {
        self.visible().get(self.index).copied()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> BrowserKey {
        // Typing a marketplace source.
        if let Some(input) = self.adding_market.as_mut() {
            match key.code {
                KeyCode::Esc => self.adding_market = None,
                KeyCode::Enter => {
                    let source = input.trim().to_string();
                    self.adding_market = None;
                    if !source.is_empty() {
                        return BrowserKey::Run(BrowserAction::AddMarket(source));
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.push(c);
                }
                _ => {}
            }
            return BrowserKey::Handled;
        }
        if self.busy.is_some() {
            return match key.code {
                KeyCode::Esc => BrowserKey::Close,
                _ => BrowserKey::Handled,
            };
        }
        let searchable = matches!(self.tab, BrowserTab::Discover | BrowserTab::Installed);
        let action = match key.code {
            KeyCode::Esc if self.details => {
                self.details = false;
                None
            }
            KeyCode::Esc if !self.search.is_empty() => {
                self.search.clear();
                self.index = 0;
                None
            }
            KeyCode::Esc => return BrowserKey::Close,
            KeyCode::Tab | KeyCode::Right => {
                self.switch(true);
                None
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.switch(false);
                None
            }
            KeyCode::Up => {
                self.index = self.index.saturating_sub(1);
                self.confirm = None;
                None
            }
            KeyCode::Down => {
                self.index = (self.index + 1).min(self.visible().len().saturating_sub(1));
                self.confirm = None;
                None
            }
            KeyCode::PageUp => {
                self.index = self.index.saturating_sub(8);
                None
            }
            KeyCode::PageDown => {
                self.index = (self.index + 8).min(self.visible().len().saturating_sub(1));
                None
            }
            KeyCode::Enter if matches!(self.tab, BrowserTab::Discover | BrowserTab::Installed) => {
                self.details = !self.details && self.selected().is_some();
                None
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.primary_action(),
            KeyCode::Backspace if searchable => {
                self.search.pop();
                self.index = 0;
                None
            }
            KeyCode::Char('u')
                if !searchable || self.search.is_empty() && self.tab == BrowserTab::Installed =>
            {
                self.update_action()
            }
            KeyCode::Char('x')
                if !searchable || self.search.is_empty() && self.tab == BrowserTab::Installed =>
            {
                self.remove_action()
            }
            KeyCode::Char('a') if self.tab == BrowserTab::Marketplaces => {
                self.adding_market = Some(String::new());
                None
            }
            KeyCode::Char(c) if searchable && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.push(c);
                self.index = 0;
                self.details = false;
                None
            }
            _ => None,
        };
        match action {
            Some(a) => BrowserKey::Run(a),
            None => BrowserKey::Handled,
        }
    }

    fn primary_action(&mut self) -> Option<BrowserAction> {
        let i = self.selected()?;
        match self.tab {
            BrowserTab::Discover => {
                let r = &self.discover[i];
                Some(if r.installed {
                    BrowserAction::Uninstall(r.name.clone())
                } else {
                    BrowserAction::Install {
                        market: r.market.clone(),
                        name: r.name.clone(),
                    }
                })
            }
            BrowserTab::Installed => {
                let r = &self.installed[i];
                Some(BrowserAction::SetEnabled(r.name.clone(), !r.enabled))
            }
            BrowserTab::Marketplaces => {
                Some(BrowserAction::UpdateMarket(self.markets[i].name.clone()))
            }
            BrowserTab::Errors => None,
        }
    }

    fn update_action(&mut self) -> Option<BrowserAction> {
        let i = self.selected()?;
        match self.tab {
            BrowserTab::Installed => {
                Some(BrowserAction::UpdatePlugin(self.installed[i].name.clone()))
            }
            BrowserTab::Marketplaces => {
                Some(BrowserAction::UpdateMarket(self.markets[i].name.clone()))
            }
            _ => None,
        }
    }

    fn remove_action(&mut self) -> Option<BrowserAction> {
        let i = self.selected()?;
        let (key, action) = match self.tab {
            BrowserTab::Installed => {
                let n = self.installed[i].name.clone();
                (n.clone(), BrowserAction::Uninstall(n))
            }
            BrowserTab::Marketplaces => {
                let n = self.markets[i].name.clone();
                (n.clone(), BrowserAction::RemoveMarket(n))
            }
            _ => return None,
        };
        if self.confirm.as_deref() == Some(key.as_str()) {
            self.confirm = None;
            Some(action)
        } else {
            self.confirm = Some(key);
            None
        }
    }

    /// Record an action's result and refresh from disk.
    pub fn finish(&mut self, result: Result<String, String>) {
        self.busy = None;
        self.status = Some(match result {
            Ok(m) => (m, true),
            Err(e) => (e, false),
        });
        self.reload();
    }

    // ── Rendering ──────────────────────────────────────────────────────

    pub fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let margin_x = if area.width > 100 { 2 } else { 0 };
        let rect = Rect::new(
            area.x + margin_x,
            area.y,
            area.width.saturating_sub(margin_x * 2),
            area.height,
        );
        // Clear the whole pane (margins too) so nothing behind shows through.
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default().style(Style::default().bg(theme::bg())),
            area,
        );
        let block = Block::default()
            .borders(Borders::TOP)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(theme::accent()))
            .padding(Padding::horizontal(1))
            .style(Style::default().bg(theme::bg()));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let lines = self.lines(inner.width as usize, inner.height as usize);
        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    pub fn lines(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        let accent = Style::default().fg(theme::accent());
        let muted = Style::default().fg(theme::muted());
        let text = Style::default().fg(theme::text());
        let mut out: Vec<Line<'static>> = Vec::new();

        // Tabs.
        let mut tabs = vec![Span::styled(
            "Plugins  ",
            accent.add_modifier(Modifier::BOLD),
        )];
        for t in BrowserTab::ALL {
            let label = match t {
                BrowserTab::Discover => "Discover".to_string(),
                BrowserTab::Installed => format!("Installed ({})", self.installed.len()),
                BrowserTab::Marketplaces => "Marketplaces".to_string(),
                BrowserTab::Errors if self.errors.is_empty() => "Errors".to_string(),
                BrowserTab::Errors => format!("Errors ({})", self.errors.len()),
            };
            let style = if t == self.tab {
                Style::default()
                    .fg(theme::on_accent())
                    .bg(theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                text
            };
            tabs.push(Span::styled(format!(" {label} "), style));
            tabs.push(Span::raw("  "));
        }
        out.push(Line::from(tabs));
        out.push(Line::default());

        let visible = self.visible();
        let header = match self.tab {
            BrowserTab::Discover => format!(
                "Discover plugins ({}/{})",
                if visible.is_empty() {
                    0
                } else {
                    self.index + 1
                },
                visible.len()
            ),
            BrowserTab::Installed => format!("Installed plugins ({})", visible.len()),
            BrowserTab::Marketplaces => format!("Marketplaces ({})", self.markets.len()),
            BrowserTab::Errors => format!("Errors ({})", self.errors.len()),
        };
        out.push(Line::from(Span::styled(
            header,
            text.add_modifier(Modifier::BOLD),
        )));

        if matches!(self.tab, BrowserTab::Discover | BrowserTab::Installed) {
            let search = if self.search.is_empty() {
                Span::styled("⌕ Search…", muted)
            } else {
                Span::styled(format!("⌕ {}▏", self.search), text)
            };
            out.push(Line::from(vec![Span::raw(" "), search]));
        }
        if self.tab == BrowserTab::Discover && !self.markets.is_empty() {
            let summary = self
                .markets
                .iter()
                .map(|m| format!("{} {}", m.name, m.count))
                .collect::<Vec<_>>()
                .join(" · ");
            out.push(Line::from(vec![
                Span::styled(format!("All {}", self.discover.len()), accent),
                Span::styled(format!(" · {summary}"), muted),
            ]));
        }
        out.push(Line::default());

        // Footer takes 2 rows.
        let body_rows = height.saturating_sub(out.len() + 2).max(2);
        let row_h = match self.tab {
            BrowserTab::Discover | BrowserTab::Installed | BrowserTab::Marketplaces => 2,
            BrowserTab::Errors => 1,
        };

        if self.details
            && let Some(i) = self.selected()
        {
            out.extend(self.detail_lines(i, width));
        } else if visible.is_empty() {
            out.push(Line::from(Span::styled(self.empty_message(), muted)));
        } else {
            let per_page = (body_rows / row_h).max(1);
            let start = self
                .index
                .saturating_sub(per_page.saturating_sub(1))
                .min(visible.len().saturating_sub(per_page));
            for (pos, &i) in visible.iter().enumerate().skip(start).take(per_page) {
                let sel = pos == self.index;
                out.extend(self.row_lines(i, sel, width));
            }
            if start + per_page < visible.len() {
                out.push(Line::from(Span::styled("↓ more below", muted)));
            }
        }

        while out.len() + 2 < height {
            out.push(Line::default());
        }
        out.truncate(height.saturating_sub(2));
        out.push(Line::default());
        out.push(self.footer());
        out
    }

    fn empty_message(&self) -> String {
        match self.tab {
            BrowserTab::Discover if self.markets.is_empty() => {
                "No marketplaces yet — Tab to Marketplaces, press a to add one.".into()
            }
            BrowserTab::Discover | BrowserTab::Installed if !self.search.is_empty() => {
                "No plugins match.".into()
            }
            BrowserTab::Installed => "No plugins installed — find some in Discover.".into(),
            BrowserTab::Marketplaces => format!(
                "No marketplaces — press a and enter {}",
                plugin_installer::DEFAULT_MARKETPLACE
            ),
            BrowserTab::Errors => "No errors.".into(),
            BrowserTab::Discover => "Marketplaces list no plugins.".into(),
        }
    }

    fn row_lines(&self, i: usize, sel: bool, width: usize) -> Vec<Line<'static>> {
        let muted = Style::default().fg(theme::muted());
        let name_style = if sel {
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::text())
                .add_modifier(Modifier::BOLD)
        };
        let cursor = Span::styled(
            if sel { "❯ " } else { "  " },
            Style::default().fg(theme::accent()),
        );
        let desc = |d: &str| {
            Line::from(Span::styled(
                format!("    {}", truncate_chars(d, width.saturating_sub(6))),
                muted,
            ))
        };
        match self.tab {
            BrowserTab::Discover => {
                let r = &self.discover[i];
                let (dot, dot_style) = if r.installed {
                    ("● ", Style::default().fg(theme::success()))
                } else {
                    ("○ ", muted)
                };
                let mut meta = format!(" · {}", r.market);
                if let Some(c) = &r.category {
                    meta.push_str(&format!(" · {c}"));
                }
                if r.installed {
                    meta.push_str(" · installed");
                }
                vec![
                    Line::from(vec![
                        cursor,
                        Span::styled(dot, dot_style),
                        Span::styled(r.name.clone(), name_style),
                        Span::styled(
                            truncate_chars(&meta, width.saturating_sub(r.name.len() + 6)),
                            muted,
                        ),
                    ]),
                    desc(&r.description),
                ]
            }
            BrowserTab::Installed => {
                let r = &self.installed[i];
                let (dot, dot_style) = if r.enabled {
                    ("● ", Style::default().fg(theme::success()))
                } else {
                    ("○ ", muted)
                };
                let mut meta = r
                    .version
                    .as_deref()
                    .map(|v| format!(" v{v}"))
                    .unwrap_or_default();
                meta.push_str(&format!(" · {}", r.adds));
                if !r.enabled {
                    meta.push_str(" · disabled");
                }
                if self.confirm.as_deref() == Some(r.name.as_str()) {
                    meta.push_str("  — press x again to uninstall");
                }
                vec![
                    Line::from(vec![
                        cursor,
                        Span::styled(dot, dot_style),
                        Span::styled(r.name.clone(), name_style),
                        Span::styled(meta, muted),
                    ]),
                    desc(&r.description),
                ]
            }
            BrowserTab::Marketplaces => {
                let r = &self.markets[i];
                let mut meta = format!(" · {} plugins · updated {}", r.count, r.updated);
                if self.confirm.as_deref() == Some(r.name.as_str()) {
                    meta.push_str("  — press x again to remove");
                }
                vec![
                    Line::from(vec![
                        cursor,
                        Span::styled(r.name.clone(), name_style),
                        Span::styled(meta, muted),
                    ]),
                    desc(&r.source),
                ]
            }
            BrowserTab::Errors => vec![Line::from(vec![
                cursor,
                Span::styled(
                    truncate_chars(&self.errors[i], width.saturating_sub(3)),
                    Style::default().fg(theme::error()),
                ),
            ])],
        }
    }

    fn detail_lines(&self, i: usize, width: usize) -> Vec<Line<'static>> {
        let muted = Style::default().fg(theme::muted());
        let text = Style::default().fg(theme::text());
        let mut out = Vec::new();
        let field = |k: &str, v: String| {
            Line::from(vec![
                Span::styled(format!("  {k:<12}"), muted),
                Span::styled(v, text),
            ])
        };
        let (title, description, rows): (String, String, Vec<(&str, String)>) = match self.tab {
            BrowserTab::Discover => {
                let r = &self.discover[i];
                (
                    r.name.clone(),
                    r.description.clone(),
                    vec![
                        ("marketplace", r.market.clone()),
                        ("category", r.category.clone().unwrap_or("—".into())),
                        ("author", r.author.clone().unwrap_or("—".into())),
                        (
                            "status",
                            if r.installed {
                                "installed".into()
                            } else {
                                "not installed".into()
                            },
                        ),
                        (
                            "install",
                            format!("dcode-ai plugin install {}@{}", r.name, r.market),
                        ),
                    ],
                )
            }
            _ => {
                let r = &self.installed[i];
                (
                    r.name.clone(),
                    r.description.clone(),
                    vec![
                        ("version", r.version.clone().unwrap_or("—".into())),
                        ("adds", r.adds.clone()),
                        ("enabled", r.enabled.to_string()),
                        ("source", r.source.clone()),
                    ],
                )
            }
        };
        out.push(Line::from(Span::styled(
            title,
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )));
        for l in textwrap::wrap(&description, width.saturating_sub(2).max(10)) {
            out.push(Line::from(Span::styled(format!("  {l}"), text)));
        }
        out.push(Line::default());
        for (k, v) in rows {
            out.push(field(k, v));
        }
        out
    }

    fn footer(&self) -> Line<'static> {
        let muted = Style::default()
            .fg(theme::muted())
            .add_modifier(Modifier::ITALIC);
        if let Some(input) = &self.adding_market {
            return Line::from(vec![
                Span::styled(
                    "Add marketplace (owner/repo, git URL or dir): ",
                    Style::default().fg(theme::text()),
                ),
                Span::styled(format!("{input}▏"), Style::default().fg(theme::accent())),
                Span::styled("  Enter add · Esc cancel", muted),
            ]);
        }
        if let Some(busy) = &self.busy {
            return Line::from(Span::styled(
                format!("◐ {busy}"),
                Style::default().fg(theme::warn()),
            ));
        }
        if let Some((msg, ok)) = &self.status {
            let color = if *ok {
                theme::success()
            } else {
                theme::error()
            };
            return Line::from(Span::styled(msg.clone(), Style::default().fg(color)));
        }
        let hint = match self.tab {
            BrowserTab::Discover => {
                "Type to search · Space to install/uninstall · Enter details · Tab next tab · Esc back"
            }
            BrowserTab::Installed => {
                "Type to search · Space enable/disable · u update · x uninstall · Enter details · Esc back"
            }
            BrowserTab::Marketplaces => {
                "Space/u update · a add · x remove · Tab next tab · Esc back"
            }
            BrowserTab::Errors => "Tab next tab · Esc back",
        };
        Line::from(Span::styled(hint, muted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser() -> PluginBrowser {
        PluginBrowser {
            tab: BrowserTab::Discover,
            search: String::new(),
            index: 0,
            discover: vec![
                DiscoverRow {
                    name: "code-review".into(),
                    market: "official".into(),
                    description: "Review pull requests".into(),
                    category: Some("development".into()),
                    author: None,
                    installed: false,
                },
                DiscoverRow {
                    name: "canva".into(),
                    market: "official".into(),
                    description: "Design with Canva".into(),
                    category: Some("design".into()),
                    author: None,
                    installed: true,
                },
            ],
            installed: vec![InstalledRow {
                name: "canva".into(),
                version: Some("1.0.0".into()),
                description: "Design".into(),
                enabled: true,
                adds: "1 MCP server".into(),
                source: "marketplace:official/canva".into(),
            }],
            markets: vec![MarketRow {
                name: "official".into(),
                source: "anthropics/claude-plugins-official".into(),
                count: 2,
                updated: "today".into(),
            }],
            errors: vec!["plugin `x`: broken".into()],
            busy: None,
            status: None,
            adding_market: None,
            confirm: None,
            details: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn search_filters_and_space_installs_or_uninstalls() {
        let mut b = browser();
        for c in "review".chars() {
            b.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(b.visible(), vec![0]);
        b.search = "c".into();
        // `canva` and `code-review` both start with c; order is stable.
        assert_eq!(b.visible(), vec![0, 1]);
        b.search = "canva".into();
        assert_eq!(b.visible(), vec![1]);
        b.search = "review".into();
        assert_eq!(
            b.handle_key(key(KeyCode::Char(' '))),
            BrowserKey::Run(BrowserAction::Install {
                market: "official".into(),
                name: "code-review".into()
            })
        );
        b.handle_key(key(KeyCode::Esc)); // clears search first
        assert!(b.search.is_empty());
        b.handle_key(key(KeyCode::Down));
        assert_eq!(
            b.handle_key(key(KeyCode::Char(' '))),
            BrowserKey::Run(BrowserAction::Uninstall("canva".into()))
        );
        assert_eq!(b.handle_key(key(KeyCode::Esc)), BrowserKey::Close);
    }

    #[test]
    fn installed_tab_toggles_and_confirms_uninstall() {
        let mut b = browser();
        b.handle_key(key(KeyCode::Tab));
        assert_eq!(b.tab, BrowserTab::Installed);
        assert_eq!(
            b.handle_key(key(KeyCode::Char(' '))),
            BrowserKey::Run(BrowserAction::SetEnabled("canva".into(), false))
        );
        assert_eq!(b.handle_key(key(KeyCode::Char('x'))), BrowserKey::Handled);
        assert_eq!(
            b.handle_key(key(KeyCode::Char('x'))),
            BrowserKey::Run(BrowserAction::Uninstall("canva".into()))
        );
    }

    #[test]
    fn marketplaces_tab_adds_by_typing_a_source() {
        let mut b = browser();
        b.handle_key(key(KeyCode::Tab));
        b.handle_key(key(KeyCode::Tab));
        assert_eq!(b.tab, BrowserTab::Marketplaces);
        b.handle_key(key(KeyCode::Char('a')));
        for c in "me/market".chars() {
            b.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            b.handle_key(key(KeyCode::Enter)),
            BrowserKey::Run(BrowserAction::AddMarket("me/market".into()))
        );
    }

    #[test]
    fn renders_tabs_rows_and_hints() {
        let b = browser();
        let text: String = b
            .lines(100, 20)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(text.contains(" Discover "), "{text}");
        assert!(text.contains("Errors (1)"));
        assert!(text.contains("Discover plugins (1/2)"));
        assert!(
            text.contains("❯ ○ code-review · official · development"),
            "{text}"
        );
        assert!(text.contains("● canva"));
        assert!(text.contains("Space to install"));
    }
}
