// World Radio — terminal edition.
//
// A braille / half-block rendered orthographic globe of internet radio stations
// you spin between — the terminal twin of the e-ink hardware concept. The
// projection + geometry core lives in globe.rs (shared idea with the browser and
// the planned ESP32 firmware); this file is the ratatui app shell.

mod borders;
mod coastline;
// globe::Center.lat is part of the shared core but unused by this binary.
#[allow(dead_code)]
mod globe;
#[allow(dead_code)]
mod stations;

use std::env;
use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use globe::{project, shortest_lon_delta, Center};
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        canvas::{Canvas, Circle, Context, Line as CLine, Points},
        Block, Clear, List, ListItem, ListState, Paragraph,
    },
    Frame, Terminal,
};

// ffmpeg/libavfilter chain that mimics a small AM/shortwave radio: narrow band-pass
// (no real bass or treble), broadcast-style compression, a touch of bit-crush grit,
// and a little makeup gain. Applied via the spawned player's audio-filter option.
const RADIO_AF: &str = "highpass=f=300,lowpass=f=3400,acompressor=ratio=4,acrusher=bits=12:mode=log:aa=1,volume=4dB";

// Owned station record so live (radio-browser) results can replace the seed set.
#[derive(Clone)]
struct Station {
    name: String,
    city: String,
    country: String,      // ISO 3166 alpha-2 code, e.g. "DE" (compact display)
    country_name: String, // full name, e.g. "Germany" (filter menu)
    lon: f64,
    lat: f64,
    genre: String,
    url: String,
}

fn seed_stations() -> Vec<Station> {
    stations::STATIONS
        .iter()
        .map(|s| Station {
            name: s.name.to_string(),
            city: s.city.to_string(),
            country: s.country.to_string(),
            country_name: s.country.to_string(), // curated only carries the code
            lon: s.lon,
            lat: s.lat,
            genre: s.genre.to_string(),
            url: s.url.to_string(),
        })
        .collect()
}

// ISO 3166 alpha-2 country code -> continent. Covers UN members + common
// territories; anything unknown falls under "Other".
fn continent_of(cc: &str) -> &'static str {
    match cc {
        "AL" | "AD" | "AT" | "BY" | "BE" | "BA" | "BG" | "HR" | "CZ" | "DK" | "EE" | "FO" | "FI"
        | "FR" | "DE" | "GI" | "GR" | "HU" | "IS" | "IE" | "IM" | "IT" | "XK" | "LV" | "LI" | "LT"
        | "LU" | "MT" | "MD" | "MC" | "ME" | "NL" | "MK" | "NO" | "PL" | "PT" | "RO" | "RU" | "SM"
        | "RS" | "SK" | "SI" | "ES" | "SE" | "CH" | "UA" | "GB" | "VA" | "JE" | "GG" => "Europe",
        "DZ" | "AO" | "BJ" | "BW" | "BF" | "BI" | "CM" | "CV" | "CF" | "TD" | "KM" | "CG" | "CD"
        | "CI" | "DJ" | "EG" | "GQ" | "ER" | "SZ" | "ET" | "GA" | "GM" | "GH" | "GN" | "GW" | "KE"
        | "LS" | "LR" | "LY" | "MG" | "MW" | "ML" | "MR" | "MU" | "MA" | "MZ" | "NA" | "NE" | "NG"
        | "RW" | "ST" | "SN" | "SC" | "SL" | "SO" | "ZA" | "SS" | "SD" | "TZ" | "TG" | "TN" | "UG"
        | "EH" | "ZM" | "ZW" => "Africa",
        "AF" | "AM" | "AZ" | "BH" | "BD" | "BT" | "BN" | "KH" | "CN" | "CY" | "GE" | "HK" | "IN"
        | "ID" | "IR" | "IQ" | "IL" | "JP" | "JO" | "KZ" | "KW" | "KG" | "LA" | "LB" | "MO" | "MY"
        | "MV" | "MN" | "MM" | "NP" | "KP" | "OM" | "PK" | "PS" | "PH" | "QA" | "SA" | "SG" | "KR"
        | "LK" | "SY" | "TW" | "TJ" | "TH" | "TL" | "TR" | "TM" | "AE" | "UZ" | "VN" | "YE" => "Asia",
        "AG" | "BS" | "BB" | "BZ" | "CA" | "CR" | "CU" | "DM" | "DO" | "SV" | "GD" | "GT" | "HT"
        | "HN" | "JM" | "MX" | "NI" | "PA" | "KN" | "LC" | "VC" | "TT" | "US" | "PR" | "GL"
        | "BM" => "North America",
        "AR" | "BO" | "BR" | "CL" | "CO" | "EC" | "FK" | "GF" | "GY" | "PY" | "PE" | "SR" | "UY"
        | "VE" => "South America",
        "AU" | "FJ" | "PF" | "GU" | "KI" | "MH" | "FM" | "NR" | "NC" | "NZ" | "PW" | "PG" | "WS"
        | "SB" | "TO" | "TV" | "VU" => "Oceania",
        _ => "Other",
    }
}

// The curated stations that actually have a stream URL — the only ones worth
// showing. (The seed table also carries placeholder entries with no URL; those
// can never play, so we never display them.)
fn curated_with_urls() -> Vec<Station> {
    seed_stations().into_iter().filter(|s| !s.url.is_empty()).collect()
}

// Messages from the live-load background thread back to the UI.
enum LiveMsg {
    Progress(String),
    Add(Station), // a verified station, appended to the list the moment it passes
    Done(usize),  // verification finished; payload = total working found
    Failed(String),
}

// Filter overlay model.
#[derive(Clone)]
enum FilterOp {
    Clear,
    Continent(Option<String>),
    Country(Option<String>),
}

enum FilterItem {
    Header(String),
    Choice { label: String, op: FilterOp, active: bool },
}

#[derive(Clone, Copy)]
struct Palette {
    coast: Color,
    border: Color,
    grat: Color,
    limb: Color,
    marker: Color,
    sel: Color,
    label: Color,
}

fn palette(color: bool) -> Palette {
    if color {
        Palette {
            coast: Color::Cyan,
            border: Color::Rgb(170, 140, 90),
            grat: Color::DarkGray,
            limb: Color::Blue,
            marker: Color::Yellow,
            sel: Color::LightRed,
            label: Color::LightYellow,
        }
    } else {
        // Brightness hierarchy: coast brightest, borders mid, graticule dimmest.
        Palette {
            coast: Color::White,
            border: Color::DarkGray,
            grat: Color::DarkGray,
            limb: Color::Gray,
            marker: Color::White,
            sel: Color::White,
            label: Color::White,
        }
    }
}

struct App {
    all_stations: Vec<Station>, // everything loaded
    stations: Vec<Station>,     // the filtered subset actually shown / navigated
    filter_continent: Option<String>,
    filter_country: Option<String>,
    filter_open: bool,
    filter_items: Vec<FilterItem>,
    filter_sel: usize,
    selected: usize,
    cur_lon: f64,
    cur_lat: f64,
    target_lon: f64,
    target_lat: f64,
    color: bool,
    graticule: bool,
    borders: bool,
    zoom: bool,
    globe_scale: f64, // 1.0 = fit panel; larger zooms into the earth
    radio_fx: bool,
    status: String,
    list_state: ListState,
    player: Option<Child>,
    live_rx: Option<Receiver<LiveMsg>>,
    seen_urls: std::collections::HashSet<String>,
    needs_redraw: bool,
    quit: bool,
}

impl App {
    fn new() -> Self {
        // Start with only the curated stations that have a URL (all known-good);
        // startup verification streams in the full set on top of these.
        let stations = curated_with_urls();
        let s = stations[0].clone();
        let seen_urls = stations.iter().map(|s| s.url.clone()).collect();
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        App {
            all_stations: stations.clone(),
            stations,
            filter_continent: None,
            filter_country: None,
            filter_open: false,
            filter_items: Vec::new(),
            filter_sel: 0,
            selected: 0,
            cur_lon: s.lon,
            cur_lat: s.lat,
            target_lon: s.lon,
            target_lat: s.lat,
            color: true,
            graticule: false,
            borders: true,
            zoom: false,
            globe_scale: 1.0,
            radio_fx: false,
            status: "Ready — ←/→ tune · x random · [ ] size · z maximize · r radio · L reload.".into(),
            list_state,
            player: None,
            live_rx: None,
            seen_urls,
            needs_redraw: true,
            quit: false,
        }
    }

    fn select(&mut self, idx: usize) {
        let n = self.stations.len();
        if n == 0 {
            return;
        }
        self.selected = (idx % n + n) % n;
        let s = self.stations[self.selected].clone();
        self.target_lon = s.lon;
        self.target_lat = s.lat;
        self.list_state.select(Some(self.selected));
        // The selected station is already shown on the line above; leave the status
        // line for system/player messages so we don't print the name twice.
        self.needs_redraw = true;
        if self.player.is_some() {
            self.play();
        }
    }

    fn tick(&mut self) {
        let dlon = shortest_lon_delta(self.cur_lon, self.target_lon);
        let dlat = self.target_lat - self.cur_lat;
        if dlon.abs() < 0.05 && dlat.abs() < 0.05 {
            if self.cur_lon != self.target_lon || self.cur_lat != self.target_lat {
                self.cur_lon = self.target_lon;
                self.cur_lat = self.target_lat;
                self.needs_redraw = true; // final settle frame
            }
            return;
        }
        self.cur_lon += dlon * 0.2;
        self.cur_lat += dlat * 0.2;
        self.needs_redraw = true;
    }

    fn play(&mut self) {
        if let Some(mut child) = self.player.take() {
            let _ = child.kill();
        }
        if self.stations.is_empty() {
            return;
        }
        let s = self.stations[self.selected.min(self.stations.len() - 1)].clone();
        if s.url.is_empty() {
            self.status = "No stream URL for this station.".into();
            return;
        }
        let fx = self.radio_fx;
        for player in ["mpv", "ffplay", "cvlc"] {
            let mut cmd = Command::new(player);
            match player {
                "mpv" => {
                    cmd.args(["--no-video", "--really-quiet"]);
                    if fx {
                        cmd.arg(format!("--af=lavfi=[{RADIO_AF}]"));
                    }
                    cmd.arg(&s.url);
                }
                "ffplay" => {
                    cmd.args(["-nodisp", "-autoexit", "-loglevel", "quiet"]);
                    if fx {
                        cmd.args(["-af", RADIO_AF]);
                    }
                    cmd.arg(&s.url);
                }
                // cvlc has no simple CLI filter path; plays clean.
                _ => { cmd.args(["--no-video", "--quiet", &s.url]); }
            }
            if let Ok(child) = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                self.player = Some(child);
                // Name + ▶ LIVE already show on the line above; keep this concise.
                let fxn = if fx && player != "cvlc" { " · radio fx" } else { "" };
                self.status = format!("playing via {player}{fxn}");
                return;
            }
        }
        self.status = "No CLI audio player found (install mpv or ffmpeg).".into();
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.player.take() {
            let _ = child.kill();
            self.status = "■ stopped".into();
        }
    }

    fn load_live(&mut self) {
        if self.live_rx.is_some() {
            return;
        }
        // Reset to the known-good curated few; verified live stations stream in on top.
        self.all_stations = curated_with_urls();
        self.filter_continent = None;
        self.filter_country = None;
        self.stations = self.all_stations.clone();
        self.seen_urls = self.stations.iter().map(|s| s.url.clone()).collect();
        self.selected = 0;
        self.list_state.select(Some(0));
        self.status = "Loading stations from radio-browser.info…".into();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || run_live(tx));
        self.live_rx = Some(rx);
    }

    fn poll_live(&mut self) {
        // Drain everything queued this frame; act on the terminal messages.
        loop {
            let msg = match &self.live_rx {
                Some(rx) => match rx.try_recv() {
                    Ok(m) => m,
                    Err(TryRecvError::Empty) => return,
                    Err(TryRecvError::Disconnected) => {
                        self.live_rx = None;
                        return;
                    }
                },
                None => return,
            };
            self.needs_redraw = true;
            match msg {
                LiveMsg::Progress(s) => self.status = s,
                LiveMsg::Add(st) => {
                    // Append the moment it verifies (dedup by URL): always to the
                    // master set, and to the visible list if it passes the filter.
                    if self.seen_urls.insert(st.url.clone()) {
                        if self.passes(&st) {
                            self.stations.push(st.clone());
                            if self.list_state.selected().is_none() {
                                self.list_state.select(Some(0));
                            }
                        }
                        self.all_stations.push(st);
                    }
                }
                LiveMsg::Done(n) => {
                    self.status = format!("{n} working stations loaded. ←/→ to browse · L to refresh.");
                    self.live_rx = None;
                    return;
                }
                LiveMsg::Failed(e) => {
                    self.status = format!("Live load failed: {e} — keeping current list.");
                    self.live_rx = None;
                    return;
                }
            }
        }
    }

    // --- filtering ---

    fn passes(&self, s: &Station) -> bool {
        self.filter_continent.as_deref().map_or(true, |c| continent_of(&s.country) == c)
            && self.filter_country.as_deref().map_or(true, |c| s.country_name == c)
    }

    fn filters_active(&self) -> bool {
        self.filter_continent.is_some() || self.filter_country.is_some()
    }

    // Rebuild the visible (filtered) station list from the master set.
    fn apply_filters(&mut self) {
        self.stations = self.all_stations.iter().filter(|s| self.passes(s)).cloned().collect();
        self.selected = 0;
        self.list_state.select(if self.stations.is_empty() { None } else { Some(0) });
        if let Some(s) = self.stations.first() {
            self.target_lon = s.lon;
            self.target_lat = s.lat;
        }
        let mut parts = Vec::new();
        if let Some(c) = &self.filter_continent { parts.push(c.clone()); }
        if let Some(c) = &self.filter_country { parts.push(c.clone()); }
        self.status = if parts.is_empty() {
            format!("Filters cleared — {} stations.", self.stations.len())
        } else {
            format!("Filter: {} — {} stations.", parts.join(" · "), self.stations.len())
        };
        self.needs_redraw = true;
    }

    fn count_by(&self, key: impl Fn(&Station) -> Option<String>) -> Vec<(String, usize)> {
        let mut m: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for s in &self.all_stations {
            if let Some(k) = key(s) {
                *m.entry(k).or_insert(0) += 1;
            }
        }
        let mut v: Vec<(String, usize)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    fn build_filter_items(&self) -> Vec<FilterItem> {
        let mut items = vec![FilterItem::Choice {
            label: "Clear all filters".into(),
            op: FilterOp::Clear,
            active: !self.filters_active(),
        }];

        items.push(FilterItem::Header("Continent".into()));
        items.push(FilterItem::Choice { label: "All continents".into(), op: FilterOp::Continent(None), active: self.filter_continent.is_none() });
        for (val, n) in self.count_by(|s| Some(continent_of(&s.country).to_string())) {
            let active = self.filter_continent.as_deref() == Some(val.as_str());
            items.push(FilterItem::Choice { label: format!("{val}  ({n})"), op: FilterOp::Continent(Some(val)), active });
        }

        items.push(FilterItem::Header("Country".into()));
        items.push(FilterItem::Choice { label: "All countries".into(), op: FilterOp::Country(None), active: self.filter_country.is_none() });
        for (val, n) in self.count_by(|s| (!s.country_name.is_empty()).then(|| s.country_name.clone())) {
            let active = self.filter_country.as_deref() == Some(val.as_str());
            items.push(FilterItem::Choice { label: format!("{val}  ({n})"), op: FilterOp::Country(Some(val)), active });
        }
        items
    }

    fn open_filter(&mut self) {
        self.filter_items = self.build_filter_items();
        self.filter_open = true;
        self.filter_sel = self
            .filter_items
            .iter()
            .position(|i| matches!(i, FilterItem::Choice { .. }))
            .unwrap_or(0);
    }

    fn filter_step(&mut self, dir: i32) {
        let n = self.filter_items.len();
        if n == 0 {
            return;
        }
        let mut i = self.filter_sel as i32;
        for _ in 0..n {
            i = (i + dir).rem_euclid(n as i32);
            if matches!(self.filter_items[i as usize], FilterItem::Choice { .. }) {
                break;
            }
        }
        self.filter_sel = i as usize;
    }

    fn filter_apply_selected(&mut self) {
        let op = match self.filter_items.get(self.filter_sel) {
            Some(FilterItem::Choice { op, .. }) => op.clone(),
            _ => {
                self.filter_open = false;
                return;
            }
        };
        match op {
            FilterOp::Clear => {
                self.filter_continent = None;
                self.filter_country = None;
            }
            FilterOp::Continent(v) => self.filter_continent = v,
            FilterOp::Country(v) => self.filter_country = v,
        }
        self.apply_filters();
        self.filter_open = false;
    }

    fn on_filter_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.filter_step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.filter_step(1),
            KeyCode::Enter | KeyCode::Char(' ') => self.filter_apply_selected(),
            KeyCode::Esc | KeyCode::Char('f') | KeyCode::Char('q') => self.filter_open = false,
            _ => {}
        }
    }

    fn on_key(&mut self, code: KeyCode) {
        self.needs_redraw = true;
        if self.filter_open {
            self.on_filter_key(code);
            return;
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.stop();
                self.quit = true;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Down | KeyCode::Char('j') => {
                self.select(self.selected + 1)
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Up | KeyCode::Char('k') => {
                self.select(self.selected + self.stations.len().saturating_sub(1))
            }
            KeyCode::Char('x') => {
                let n = self.stations.len();
                if n > 0 {
                    self.select(random_index(n));
                }
            }
            KeyCode::Char(']') | KeyCode::Char('+') | KeyCode::Char('=') => {
                self.globe_scale = (self.globe_scale * 1.15).min(6.0);
            }
            KeyCode::Char('[') | KeyCode::Char('-') | KeyCode::Char('_') => {
                self.globe_scale = (self.globe_scale / 1.15).max(0.3);
            }
            KeyCode::Char('c') => self.color = !self.color,
            KeyCode::Char('g') => self.graticule = !self.graticule,
            KeyCode::Char('b') => self.borders = !self.borders,
            KeyCode::Char('z') => self.zoom = !self.zoom,
            KeyCode::Char('f') => self.open_filter(),
            KeyCode::Char('r') => {
                self.radio_fx = !self.radio_fx;
                if self.player.is_some() {
                    self.play(); // re-spawn the stream with / without the filter
                } else {
                    self.status = if self.radio_fx {
                        "Radio filter ON — lo-fi AM sound when you play (p).".into()
                    } else {
                        "Radio filter off.".into()
                    };
                }
            }
            KeyCode::Char('L') => self.load_live(),
            KeyCode::Char('p') | KeyCode::Char(' ') => {
                if self.player.is_some() { self.stop() } else { self.play() }
            }
            _ => {}
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        // Maximized: hand the whole terminal to the globe so braille gets the most
        // cells (= the most dots = finest detail). One status line at the bottom.
        if self.zoom {
            let v = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(frame.area());
            self.draw_globe(frame, v[0]);
            self.draw_zoom_status(frame, v[1]);
        } else {
            let main = Layout::horizontal([Constraint::Min(40), Constraint::Length(32)]).split(frame.area());
            let left = Layout::vertical([Constraint::Min(8), Constraint::Length(4)]).split(main[0]);
            let side = Layout::vertical([Constraint::Min(6), Constraint::Length(15)]).split(main[1]);
            self.draw_globe(frame, left[0]);
            self.draw_status(frame, left[1]);
            self.draw_list(frame, side[0]);
            self.draw_controls(frame, side[1]);
        }
        if self.filter_open {
            self.draw_filter(frame);
        }
    }

    fn draw_filter(&self, frame: &mut Frame) {
        let area = frame.area();
        let w = 46u16.min(area.width.saturating_sub(4)).max(20);
        let h = ((area.height as f32 * 0.8) as u16).min(area.height.saturating_sub(2)).max(6);
        let popup = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        frame.render_widget(Clear, popup);
        let items: Vec<ListItem> = self
            .filter_items
            .iter()
            .map(|it| match it {
                FilterItem::Header(t) => {
                    ListItem::new(Line::from(Span::styled(format!("─ {t} ─"), Style::default().fg(Color::DarkGray))))
                }
                FilterItem::Choice { label, active, .. } => {
                    let style = if *active {
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(if *active { "● " } else { "  " }, Style::default().fg(Color::Green)),
                        Span::styled(label.clone(), style),
                    ]))
                }
            })
            .collect();
        let mut st = ListState::default();
        st.select(Some(self.filter_sel));
        let list = List::new(items)
            .block(Block::bordered().title(" Filter · ↑/↓ · Enter apply · Esc close "))
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, popup, &mut st);
    }

    fn draw_zoom_status(&self, frame: &mut Frame, area: Rect) {
        if self.stations.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " WORLD RADIO  ·  no stations match filter — z windowed, then f to change",
                    Style::default().fg(Color::DarkGray),
                ))),
                area,
            );
            return;
        }
        let s = &self.stations[self.selected.min(self.stations.len().saturating_sub(1))];
        let playing = self.player.is_some();
        let line = Line::from(vec![
            Span::styled(" WORLD RADIO ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(format!("  {} · {}·{}  ", s.name, s.city, s.country)),
            Span::styled(
                if playing { "▶ LIVE" } else { "■ IDLE" },
                Style::default().fg(if playing { Color::Green } else { Color::DarkGray }),
            ),
            Span::styled("   z windowed · ←/→ tune · q quit", Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn draw_controls(&self, frame: &mut Frame, area: Rect) {
        let key = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
        let on = Style::default().fg(Color::Green);
        let off = Style::default().fg(Color::DarkGray);
        let state = |b: bool| if b { ("on", on) } else { ("off", off) };
        // key label padded to a fixed column, then the description (+ live state)
        let row = |k: &str, rest: Vec<Span<'static>>| {
            let mut v = vec![Span::styled(format!("{k:<4}"), key)];
            v.extend(rest);
            Line::from(v)
        };
        let (bv, bs) = state(self.borders);
        let (cv, cs) = state(self.color);
        let (gv, gs) = state(self.graticule);
        let (zv, zs) = state(self.zoom);
        let (rv, rs) = state(self.radio_fx);
        let lines = vec![
            row("←/→", vec![Span::raw("tune station")]),
            row("x", vec![Span::raw("random station")]),
            row("f", vec![Span::raw("filter stations")]),
            row("[ ]", vec![Span::raw(format!("globe size: {:.1}x", self.globe_scale))]),
            row("p", vec![Span::raw("play / stop")]),
            row("r", vec![Span::raw("radio fx: "), Span::styled(rv, rs)]),
            row("z", vec![Span::raw("maximize: "), Span::styled(zv, zs)]),
            row("b", vec![Span::raw("borders: "), Span::styled(bv, bs)]),
            row("c", vec![Span::raw("color: "), Span::styled(cv, cs)]),
            row("g", vec![Span::raw("grid: "), Span::styled(gv, gs)]),
            row("L", vec![Span::raw("reload stations")]),
            row("q", vec![Span::raw("quit")]),
        ];
        frame.render_widget(Paragraph::new(lines).block(Block::bordered().title(" Controls ")), area);
    }

    fn draw_globe(&self, frame: &mut Frame, area: Rect) {
        let pal = palette(self.color);
        let center = Center::new(self.cur_lon, self.cur_lat);
        let want_grat = self.graticule;
        let want_borders = self.borders;

        // Keep the globe circular for the braille (2x4 dot) grid.
        let iw = area.width.saturating_sub(2).max(1) as f64;
        let ih = area.height.saturating_sub(2).max(1) as f64;
        // Shrinking the bounds by `globe_scale` zooms the earth in (it overflows the
        // panel and clips at the edges); growing them shrinks the globe with margin.
        let dots_w = iw * 2.0;
        let dots_h = ih * 4.0;
        let s = self.globe_scale;
        let (xb, yb) = if dots_w >= dots_h {
            let r = dots_w / dots_h;
            ([-(r / s), r / s], [-(1.05 / s), 1.05 / s])
        } else {
            let r = dots_h / dots_w;
            ([-(1.05 / s), 1.05 / s], [-(r / s), r / s])
        };

        // Markers (project once here so the closure owns plain coords).
        let mut markers: Vec<(f64, f64)> = Vec::new();
        let mut sel_pt: Option<(f64, f64)> = None;
        for (i, s) in self.stations.iter().enumerate() {
            let p = project(s.lon, s.lat, &center);
            if !p.front {
                continue;
            }
            if i == self.selected {
                sel_pt = Some((p.x, p.y));
            } else {
                markers.push((p.x, p.y));
            }
        }
        let sel_name = self.stations.get(self.selected).map(|s| s.name.clone()).unwrap_or_default();

        let total = self.stations.len();
        let cur = if total == 0 { 0 } else { self.selected + 1 };
        let title = format!(" WORLD RADIO  {cur}/{total} ");
        let canvas = Canvas::default()
            .block(Block::bordered().title(title))
            .marker(Marker::Braille)
            .x_bounds(xb)
            .y_bounds(yb)
            .paint(move |ctx: &mut Context| {
                if want_grat {
                    for line in globe::graticule(30) {
                        draw_polyline(ctx, &center, &line, pal.grat);
                    }
                }
                ctx.layer();
                for poly in coastline::COASTLINE {
                    draw_polyline_f32(ctx, &center, poly, pal.coast);
                }
                if want_borders {
                    for poly in borders::BORDERS {
                        draw_polyline_f32(ctx, &center, poly, pal.border);
                    }
                }
                ctx.draw(&Circle { x: 0.0, y: 0.0, radius: 1.0, color: pal.limb });
                ctx.layer();
                if !markers.is_empty() {
                    ctx.draw(&Points { coords: &markers, color: pal.marker });
                }
                if let Some((sx, sy)) = sel_pt {
                    ctx.draw(&Circle { x: sx, y: sy, radius: 0.05, color: pal.sel });
                    ctx.draw(&Points { coords: &[(sx, sy)], color: pal.sel });
                    ctx.print(
                        sx + 0.06,
                        sy + 0.06,
                        Line::from(Span::styled(
                            format!(" {} ", sel_name),
                            Style::default().fg(pal.label).add_modifier(Modifier::BOLD),
                        )),
                    );
                }
            });
        frame.render_widget(canvas, area);
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        if self.stations.is_empty() {
            let lines = vec![
                Line::from(Span::styled("No stations match the filter.", Style::default().fg(Color::Yellow))),
                Line::from(Span::styled(self.status.clone(), Style::default().fg(Color::Gray))),
            ];
            frame.render_widget(Paragraph::new(lines).block(Block::bordered()), area);
            return;
        }
        let s = &self.stations[self.selected.min(self.stations.len().saturating_sub(1))];
        let playing = self.player.is_some();
        let mut head = vec![
            Span::styled(s.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("   {} · {}", s.city, s.country)),
        ];
        if !s.genre.is_empty() {
            head.push(Span::styled(format!(" · {}", s.genre), Style::default().fg(Color::DarkGray)));
        }
        head.push(Span::styled(
            if playing { "   ▶ LIVE" } else { "   ■ IDLE" },
            Style::default().fg(if playing { Color::Green } else { Color::DarkGray }),
        ));
        let lines = vec![
            Line::from(head),
            Line::from(Span::styled(self.status.clone(), Style::default().fg(Color::Gray))),
        ];
        frame.render_widget(Paragraph::new(lines).block(Block::bordered()), area);
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .stations
            .iter()
            .map(|s| {
                ListItem::new(Line::from(vec![
                    Span::raw(s.name.clone()),
                    Span::styled(format!("  {}·{}", s.city, s.country), Style::default().fg(Color::DarkGray)),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(Block::bordered().title(" Stations "))
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }
}

fn draw_polyline(ctx: &mut Context, center: &Center, coords: &[(f64, f64)], color: Color) {
    let mut prev: Option<globe::Proj> = None;
    for &(lon, lat) in coords {
        let p = project(lon, lat, center);
        if let Some(pp) = prev {
            if pp.front && p.front {
                ctx.draw(&CLine { x1: pp.x, y1: pp.y, x2: p.x, y2: p.y, color });
            }
        }
        prev = Some(p);
    }
}

// A random index without pulling in the `rand` crate — RandomState is seeded with
// fresh OS entropy on every construction, so its hasher's initial state varies.
fn random_index(n: usize) -> usize {
    use std::hash::{BuildHasher, Hasher};
    let r = std::collections::hash_map::RandomState::new().build_hasher().finish();
    (r % n as u64) as usize
}

fn draw_polyline_f32(ctx: &mut Context, center: &Center, coords: &[(f32, f32)], color: Color) {
    let mut prev: Option<globe::Proj> = None;
    for &(lon, lat) in coords {
        let p = project(lon as f64, lat as f64, center);
        if let Some(pp) = prev {
            if pp.front && p.front {
                ctx.draw(&CLine { x1: pp.x, y1: pp.y, x2: p.x, y2: p.y, color });
            }
        }
        prev = Some(p);
    }
}

// How many verified stations we aim to show, and how many candidates to probe.
const VERIFY_TARGET: usize = 300; // aim to show this many verified stations
const PROBE_CAP: usize = 800; // candidates to probe (popularity-ordered); early-stops at target
const PROBE_WORKERS: usize = 24;

// radio-browser endpoints — `all.api` is the official round-robin alias across all
// live mirrors; `de1` is a stable named one. (Other numbered names like nl1/at1
// don't reliably resolve.) Tried in order, with retries, since the service
// rate-limits / 503s under load.
const RB_SERVERS: &[&str] = &[
    "all.api.radio-browser.info",
    "de1.api.radio-browser.info",
];

// Fetch geolocated candidate stations from radio-browser.info (via curl to avoid
// a TLS dependency), preferring the resolved direct stream URL, ordered by
// popularity, thinned to a ~1° grid.
fn fetch_candidates() -> Result<Vec<Station>, String> {
    let path = "/json/stations/search?has_geo_info=true&hidebroken=true&order=clickcount&reverse=true&limit=2000";
    let mut last = String::from("no servers reachable");
    // The service rate-limits / 503s under load, so retry a couple of rounds.
    for attempt in 0..3 {
        for srv in RB_SERVERS {
            match try_fetch(&format!("https://{srv}{path}")) {
                Ok(v) if !v.is_empty() => return Ok(v),
                Ok(_) => last = format!("{srv}: no usable stations"),
                Err(e) => last = format!("{srv}: {e}"),
            }
        }
        if attempt < 2 {
            thread::sleep(Duration::from_millis(1500));
        }
    }
    Err(last)
}

// Fetch + parse one radio-browser URL. `--fail` makes curl error on HTTP >= 400
// (e.g. 503), so an overloaded server is treated as a failure, not as data.
fn try_fetch(url: &str) -> Result<Vec<Station>, String> {
    let out = Command::new("curl")
        .args(["-s", "--fail", "--max-time", "20", "-A", "world-radio-tui/0.1", url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err("server unavailable".into());
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| "bad JSON".to_string())?;
    let arr = json.as_array().ok_or("unexpected response")?;

    let num = |v: &serde_json::Value| -> Option<f64> {
        v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    };
    let get = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();

    // Dedup by URL only (not by location) — keep many stations per city, the way
    // Radio Garden stacks dozens on one place.
    let mut seen = std::collections::HashSet::new();
    let mut out_v = Vec::new();
    for s in arr {
        let lon = s.get("geo_long").and_then(|v| num(v));
        let lat = s.get("geo_lat").and_then(|v| num(v));
        let (lon, lat) = match (lon, lat) {
            (Some(a), Some(b)) if a.is_finite() && b.is_finite() => (a, b),
            _ => continue,
        };
        let u = {
            let r = get(s, "url_resolved");
            if r.is_empty() { get(s, "url") } else { r }
        };
        if u.is_empty() || !seen.insert(u.clone()) {
            continue;
        }
        let name: String = get(s, "name").trim().chars().take(40).collect();
        let state = get(s, "state");
        let country_name = get(s, "country");
        out_v.push(Station {
            name: if name.is_empty() { "Unknown".into() } else { name },
            city: if state.is_empty() { country_name.clone() } else { state },
            country: get(s, "countrycode"),
            country_name,
            lon,
            lat,
            genre: get(s, "tags").split(',').next().unwrap_or("").to_string(),
            url: u,
        });
    }
    Ok(out_v)
}

// Verify a stream actually plays. ffprobe opens the URL and confirms a decodable
// audio track — the true "will mpv play it" test (far stricter than an HTTP check:
// it rejects redirects-to-HTML, dead hosts, and undecodable junk). Falls back to a
// curl HTTP check only if ffprobe isn't installed.
fn probe(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    match Command::new("ffprobe")
        .args([
            "-v", "error", "-rw_timeout", "4000000",
            "-analyzeduration", "2000000", "-probesize", "500000",
            "-select_streams", "a:0", "-show_entries", "stream=codec_name",
            "-of", "default=nw=1:nk=1", "-i", url,
        ])
        .output()
    {
        // success + a codec name on stdout => there's a real, decodable audio track.
        Ok(out) => return out.status.success() && !out.stdout.iter().all(u8::is_ascii_whitespace),
        Err(_) => {} // ffprobe missing — fall through to the HTTP check
    }
    let out = match Command::new("curl")
        .args([
            "-s", "-L", "--connect-timeout", "3", "-m", "4",
            "-A", "world-radio-tui/0.1", "-o", "/dev/null",
            "-w", "%{http_code} %{content_type}", url,
        ])
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.split_whitespace();
    let code = it.next().unwrap_or("");
    let ctype = it.next().unwrap_or("").to_ascii_lowercase();
    code.starts_with('2')
        && !(ctype.starts_with("text/") || ctype.contains("html") || ctype.contains("json") || ctype.contains("xml"))
}

// Coordinator (runs on a background thread): fetch a big candidate pool, verify
// with a bounded pool of concurrent ffprobe checks, and stream each working
// station to the UI as it passes. Early-stops once the target is reached.
fn run_live(tx: Sender<LiveMsg>) {
    let candidates = match fetch_candidates() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(LiveMsg::Failed(e));
            return;
        }
    };
    let candidates: Vec<Station> = candidates.into_iter().take(PROBE_CAP).collect();
    let total = candidates.len();
    let _ = tx.send(LiveMsg::Progress(format!("Verifying {total} streams — stations appear as they pass…")));

    let checked = Arc::new(AtomicUsize::new(0));
    let found = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let n_workers = PROBE_WORKERS.min(total).max(1);

    // Round-robin so each worker gets a popularity-mixed slice.
    let mut buckets: Vec<Vec<Station>> = (0..n_workers).map(|_| Vec::new()).collect();
    for (i, st) in candidates.into_iter().enumerate() {
        buckets[i % n_workers].push(st);
    }

    let mut handles = Vec::new();
    for bucket in buckets {
        let tx = tx.clone();
        let checked = Arc::clone(&checked);
        let found = Arc::clone(&found);
        let stop = Arc::clone(&stop);
        handles.push(thread::spawn(move || {
            for st in bucket {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let good = probe(&st.url);
                let c = checked.fetch_add(1, Ordering::Relaxed) + 1;
                if good {
                    let f = found.fetch_add(1, Ordering::Relaxed) + 1;
                    let _ = tx.send(LiveMsg::Add(st)); // appears in the list immediately
                    if f >= VERIFY_TARGET {
                        stop.store(true, Ordering::Relaxed);
                    }
                }
                if c % 12 == 0 {
                    let _ = tx.send(LiveMsg::Progress(format!(
                        "Verifying… {c}/{total} checked, {} working",
                        found.load(Ordering::Relaxed)
                    )));
                }
            }
        }));
    }

    for h in handles {
        let _ = h.join();
    }
    let n = found.load(Ordering::Relaxed).min(VERIFY_TARGET);
    if n == 0 {
        let _ = tx.send(LiveMsg::Failed("no streams passed verification".into()));
    } else {
        let _ = tx.send(LiveMsg::Done(n));
    }
}

fn run(terminal: &mut Terminal<impl ratatui::backend::Backend>, app: &mut App) -> io::Result<()> {
    let tick = Duration::from_millis(33);
    let mut last = Instant::now();
    loop {
        app.poll_live();
        // Only repaint when something changed — keeps idle CPU near zero even with
        // high-detail 50m data.
        if app.needs_redraw {
            terminal.draw(|f| app.draw(f))?;
            app.needs_redraw = false;
        }
        let timeout = tick.saturating_sub(last.elapsed());
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k.code),
                Event::Resize(_, _) => app.needs_redraw = true,
                _ => {}
            }
        }
        if last.elapsed() >= tick {
            app.tick();
            last = Instant::now();
        }
        if app.quit {
            return Ok(());
        }
    }
}

// Render one settled frame to a fixed-size test backend and print it as text, so
// rendering can be verified without an interactive TTY.
//   cargo run -- --snapshot            (braille wireframe)
//   cargo run -- --snapshot=filled     (half-block filled land)
fn snapshot(zoom: bool, scale: f64, filter: bool) -> io::Result<()> {
    use ratatui::backend::TestBackend;
    let mut app = App::new();
    app.globe_scale = scale;
    if filter {
        app.open_filter();
    }
    if let Some(i) = app.stations.iter().position(|s| s.name == "Deutschlandfunk") {
        app.selected = i;
        app.list_state.select(Some(i));
        let s = app.stations[i].clone();
        app.cur_lon = s.lon;
        app.cur_lat = s.lat;
    }
    app.zoom = zoom;
    let mut terminal = Terminal::new(TestBackend::new(110, 40))?;
    terminal.draw(|f| app.draw(f))?;
    let buf = terminal.backend().buffer().clone();
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    print!("{out}");
    Ok(())
}

fn fetch_test() -> io::Result<()> {
    match fetch_candidates() {
        Ok(list) => {
            let sample = 24.min(list.len());
            println!("fetched {} candidates; probing first {sample}…", list.len());
            let mut ok = 0;
            for s in list.iter().take(sample) {
                let good = probe(&s.url);
                if good {
                    ok += 1;
                }
                let name: String = s.name.chars().take(30).collect();
                println!("  [{}] {:<30} {}", if good { "OK  " } else { "DEAD" }, name, s.country);
            }
            println!("{ok}/{sample} working ({}% of this sample)", ok * 100 / sample.max(1));
        }
        Err(e) => println!("fetch failed: {e}"),
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if let Some(a) = args.iter().find(|a| a.starts_with("--snapshot")) {
        let scale = if a.contains("big") { 2.5 } else if a.contains("small") { 0.5 } else { 1.0 };
        return snapshot(a.contains("zoom"), scale, a.contains("filter"));
    }
    if args.iter().any(|a| a == "--fetch-test") {
        return fetch_test();
    }
    let mut terminal = ratatui::init();
    let mut app = App::new();
    // Verify + load working stations right away, so the list is all-working from
    // the start (the curated few show instantly while this runs in the background).
    app.load_live();
    let res = run(&mut terminal, &mut app);
    ratatui::restore();
    res
}
