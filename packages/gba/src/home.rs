//! The console **home screen** — a Rust-rendered launcher, drawn by the core
//! itself and shown on boot. Styled after the Nintendo 3DS home menu: a light
//! gradient, a top bar, a grid of rounded app-icon tiles with a selection
//! cursor, and a bottom action bar. The host feeds it the installed-game list +
//! input each frame, can hit-test pointer taps, and polls for a launch request.
//! Storage stays host-side; the launcher only knows opaque `id`s.
//!
//! Hand-rolled immediate-mode renderer (no egui). It'll formally implement
//! `core_api::FrameSource` during the monorepo restructure.

use font8x8::legacy::BASIC_LEGACY;
use std::collections::HashMap;

/// Embedded icon size (NDS banner icons are 32×32 RGBA).
pub const ICON_SIZE: usize = 32;

/// Internal render resolution (host scales it to the window).
pub const HOME_W: usize = 480;
pub const HOME_H: usize = 320;

// Active-high button bits — the SAME layout the host feeds the GBA core.
const KEY_A: u32 = 1 << 0;
const KEY_B: u32 = 1 << 1;
const KEY_SELECT: u32 = 1 << 2;
const KEY_START: u32 = 1 << 3;
const KEY_RIGHT: u32 = 1 << 4;
const KEY_LEFT: u32 = 1 << 5;
const KEY_UP: u32 = 1 << 6;
const KEY_DOWN: u32 = 1 << 7;

// Settings rows: crisp-pixels toggle, clear-library, back.
const SETTINGS_ROWS: usize = 3;

// Palette (0xRRGGBB).
const BG_TOP: u32 = 0xBC_D6_F2; // light blue gradient, top…
const BG_BOT: u32 = 0x8F_B7_E2; // …to a deeper blue at the bottom
const BAR_BG: u32 = 0x33_40_5A; // top/bottom slate bars
const BAR_TEXT: u32 = 0xFF_FF_FF;
const CURSOR: u32 = 0x6F_E0_FF; // selection glow
const ADD_BG: u32 = 0xDD_E8_F2;
const ADD_FG: u32 = 0x5A_6B_82;
const LABEL: u32 = 0x16_24_36; // dark label text on the light field
const ICON_FG: u32 = 0xFF_FF_FF;

// Tile/grid geometry.
const TOPBAR: i32 = 30;
const BOTBAR: i32 = 34;
const MARGIN_X: i32 = 18;
const COLS: usize = 5;
const GAP: i32 = 13;
const TILE: i32 = 78;
const RADIUS: i32 = 14;
const GRID_TOP: i32 = 42;
const LABEL_H: i32 = 14;
const ROW_GAP: i32 = 6;
const ROW_H: i32 = TILE + LABEL_H + ROW_GAP; // full grid row pitch (icon + label + gap)

// App-icon colors, cycled by a cheap title hash.
const PALETTE: [u32; 8] = [
    0xE8_59_4F, 0xF0_A5_3E, 0x4F_B3_6B, 0x4F_9B_E8, 0x8A_6F_E0, 0xE8_5F_A0, 0x3F_C2_C2, 0xC9_A0_3F,
];

/// One installed game as the host knows it. `id` is opaque to the launcher.
pub struct HomeEntry {
    pub id: String,
    pub title: String,
    pub system: String, // short label badge, e.g. "GBA" / "NDS"
    pub playable: bool, // false → "coming soon" (no core yet)
}

/// What the user asked the host to do this frame.
pub enum HomeAction {
    None,
    /// The "+" tile — host should open its add-game file dialog.
    AddGame,
    /// Boot this game id.
    Launch(String),
    /// A game whose system has no core yet — host shows "coming soon".
    /// Carries the system label.
    ComingSoon(String),
    /// Display setting toggled — host applies + persists the new crisp value.
    SetCrisp(bool),
    /// Clear all installed games — host deletes them from storage.
    ClearAll,
}

/// Which screen the launcher is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Grid,
    Settings,
}

pub struct Home {
    entries: Vec<HomeEntry>,
    selected: usize,
    fb: Vec<u8>,
    prev_buttons: u32,
    /// Per-game-id 32×32 RGBA icons (e.g. decoded NDS banners). Kept separate
    /// from `entries` so they survive `set_games` and are pushed independently.
    icons: HashMap<String, Vec<u8>>,
    // Vertical scroll offset of the grid (pixels of content scrolled above the viewport).
    scroll: i32,
    // Settings.
    view: View,
    crisp: bool,
    set_sel: usize,
    confirm_clear: bool,
}

impl Default for Home {
    fn default() -> Self {
        Self::new()
    }
}

impl Home {
    pub fn new() -> Self {
        let mut h = Home {
            entries: Vec::new(),
            selected: 0,
            fb: vec![0; HOME_W * HOME_H * 4],
            prev_buttons: 0,
            icons: HashMap::new(),
            scroll: 0,
            view: View::Grid,
            crisp: true,
            set_sel: 0,
            confirm_clear: false,
        };
        h.render();
        h
    }

    /// Attach a 32×32 RGBA icon to a game id (host pushes these before
    /// `set_games`). Ignored if the buffer isn't icon-sized.
    pub fn set_icon(&mut self, id: &str, rgba: &[u8]) {
        if rgba.len() == ICON_SIZE * ICON_SIZE * 4 {
            self.icons.insert(id.to_string(), rgba.to_vec());
        }
    }

    /// Host pushes the persisted display setting so the toggle reflects it.
    pub fn set_crisp(&mut self, crisp: bool) {
        self.crisp = crisp;
        self.render();
    }

    fn open_settings(&mut self) {
        self.view = View::Settings;
        self.set_sel = 0;
        self.confirm_clear = false;
    }

    /// Replace the game list from parallel newline-joined columns: `id`,
    /// `title`, `system` (badge label), and `playable` ("1"/"0").
    pub fn set_games_from_str(
        &mut self,
        ids_newline: &str,
        titles_newline: &str,
        systems_newline: &str,
        playables_newline: &str,
    ) {
        let ids = ids_newline.split('\n').filter(|s| !s.is_empty());
        let mut titles = titles_newline.split('\n');
        let mut systems = systems_newline.split('\n');
        let mut playables = playables_newline.split('\n');
        self.entries = ids
            .map(|id| HomeEntry {
                id: id.to_string(),
                title: titles.next().unwrap_or("").to_string(),
                system: systems.next().unwrap_or("").to_string(),
                playable: playables.next() == Some("1"),
            })
            .collect();
        let max = self.entries.len();
        if self.selected > max {
            self.selected = max;
        }
        self.clamp_scroll();
        self.render();
    }

    fn tile_count(&self) -> usize {
        self.entries.len() + 1 // games + the trailing "+" tile
    }

    /// Top-left + size of tile `i`'s icon square (screen-space, scroll applied).
    fn tile_rect(&self, i: usize) -> (i32, i32, i32, i32) {
        let col = (i % COLS) as i32;
        let row = (i / COLS) as i32;
        let x = MARGIN_X + col * (TILE + GAP);
        let y = GRID_TOP + row * ROW_H - self.scroll;
        (x, y, TILE, TILE)
    }

    // ---- scrolling -------------------------------------------------------

    fn view_h(&self) -> i32 {
        (HOME_H as i32 - BOTBAR) - GRID_TOP
    }
    fn content_h(&self) -> i32 {
        let rows = ((self.tile_count() + COLS - 1) / COLS) as i32;
        (rows * ROW_H - ROW_GAP).max(0)
    }
    fn max_scroll(&self) -> i32 {
        (self.content_h() - self.view_h()).max(0)
    }
    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0, self.max_scroll());
    }
    /// Auto-scroll so the selected tile is fully within the viewport.
    fn ensure_visible(&mut self) {
        let row = (self.selected / COLS) as i32;
        let top = row * ROW_H;
        let bot = top + TILE + LABEL_H;
        if top < self.scroll {
            self.scroll = top;
        } else if bot > self.scroll + self.view_h() {
            self.scroll = bot - self.view_h();
        }
        self.clamp_scroll();
    }
    /// Host wheel/drag scroll.
    pub fn scroll_by(&mut self, delta: i32) {
        self.scroll += delta;
        self.clamp_scroll();
        self.render();
    }

    /// Advance one frame: edge-triggered nav, redraw, return action.
    pub fn run_frame(&mut self, buttons: u32) -> HomeAction {
        let pressed = buttons & !self.prev_buttons;
        self.prev_buttons = buttons;
        let action = match self.view {
            View::Grid => self.run_grid(pressed),
            View::Settings => self.run_settings(pressed),
        };
        self.render();
        action
    }

    fn run_grid(&mut self, pressed: u32) -> HomeAction {
        let n = self.tile_count();
        if pressed & KEY_RIGHT != 0 && self.selected + 1 < n {
            self.selected += 1;
        }
        if pressed & KEY_LEFT != 0 {
            self.selected = self.selected.saturating_sub(1);
        }
        if pressed & KEY_DOWN != 0 && self.selected + COLS < n {
            self.selected += COLS;
        }
        if pressed & KEY_UP != 0 {
            self.selected = self.selected.saturating_sub(COLS);
        }
        self.ensure_visible();
        if pressed & KEY_SELECT != 0 {
            self.open_settings();
            return HomeAction::None;
        }
        if pressed & (KEY_A | KEY_START) != 0 {
            return self.activate(self.selected);
        }
        HomeAction::None
    }

    fn run_settings(&mut self, pressed: u32) -> HomeAction {
        if pressed & (KEY_SELECT | KEY_B) != 0 {
            self.view = View::Grid;
            self.confirm_clear = false;
            return HomeAction::None;
        }
        if pressed & KEY_DOWN != 0 && self.set_sel + 1 < SETTINGS_ROWS {
            self.set_sel += 1;
            self.confirm_clear = false;
        }
        if pressed & KEY_UP != 0 {
            self.set_sel = self.set_sel.saturating_sub(1);
            self.confirm_clear = false;
        }
        if pressed & (KEY_A | KEY_START) != 0 {
            return self.activate_setting(self.set_sel);
        }
        HomeAction::None
    }

    fn activate_setting(&mut self, row: usize) -> HomeAction {
        match row {
            0 => {
                self.crisp = !self.crisp;
                HomeAction::SetCrisp(self.crisp)
            }
            1 => {
                if self.confirm_clear {
                    self.confirm_clear = false;
                    HomeAction::ClearAll
                } else {
                    self.confirm_clear = true;
                    HomeAction::None
                }
            }
            _ => {
                self.view = View::Grid;
                HomeAction::None
            }
        }
    }

    /// Hit-test a pointer tap (in launcher pixel space).
    pub fn pointer(&mut self, x: i32, y: i32) -> HomeAction {
        let action = match self.view {
            View::Grid => self.pointer_grid(x, y),
            View::Settings => self.pointer_settings(x, y),
        };
        self.render();
        action
    }

    fn pointer_grid(&mut self, x: i32, y: i32) -> HomeAction {
        // The "SET" button lives in the top-right of the top bar.
        if y < TOPBAR && x > HOME_W as i32 - 64 {
            self.open_settings();
            return HomeAction::None;
        }
        // Taps in the top/bottom bars don't hit tiles.
        if y < GRID_TOP || y >= HOME_H as i32 - BOTBAR {
            return HomeAction::None;
        }
        for i in 0..self.tile_count() {
            let (tx, ty, tw, th) = self.tile_rect(i);
            if x >= tx && x < tx + tw && y >= ty && y < ty + th + LABEL_H {
                self.selected = i;
                return self.activate(i);
            }
        }
        HomeAction::None
    }

    fn pointer_settings(&mut self, x: i32, y: i32) -> HomeAction {
        for row in 0..SETTINGS_ROWS {
            let (rx, ry, rw, rh) = self.setting_row_rect(row);
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
                if row != self.set_sel {
                    self.confirm_clear = false;
                }
                self.set_sel = row;
                return self.activate_setting(row);
            }
        }
        HomeAction::None
    }

    fn setting_row_rect(&self, row: usize) -> (i32, i32, i32, i32) {
        let w = HOME_W as i32 - 2 * MARGIN_X;
        let h = 40;
        let y = GRID_TOP + 8 + row as i32 * (h + 10);
        (MARGIN_X, y, w, h)
    }

    fn activate(&self, i: usize) -> HomeAction {
        if i == self.entries.len() {
            HomeAction::AddGame
        } else if let Some(e) = self.entries.get(i) {
            if e.playable {
                HomeAction::Launch(e.id.clone())
            } else {
                HomeAction::ComingSoon(e.system.clone())
            }
        } else {
            HomeAction::None
        }
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.fb
    }

    // ---- rendering -------------------------------------------------------

    fn render(&mut self) {
        match self.view {
            View::Grid => self.render_grid(),
            View::Settings => self.render_settings(),
        }
    }

    fn render_grid(&mut self) {
        self.vgradient(BG_TOP, BG_BOT);

        let n = self.tile_count();
        let add_idx = self.entries.len();
        for i in 0..n {
            let (x, y, w, h) = self.tile_rect(i);
            // Skip tiles fully outside the scroll viewport (bars cover the edges).
            if y + h + LABEL_H <= GRID_TOP || y >= HOME_H as i32 - BOTBAR {
                continue;
            }
            let sel = i == self.selected;
            if sel {
                self.round_rect(x - 3, y - 3, w + 6, h + 6, RADIUS + 3, CURSOR);
            }

            if i == add_idx {
                self.round_rect(x, y, w, h, RADIUS, ADD_BG);
                self.text(x + (w - 8 * 4) / 2, y + (h - 8 * 4) / 2, 4, ADD_FG, "+");
            } else {
                let title = self.entries[i].title.clone();
                let system = self.entries[i].system.clone();
                let playable = self.entries[i].playable;
                let icon = self.icons.get(&self.entries[i].id).cloned();
                if let Some(icon) = icon {
                    // Real embedded artwork (e.g. NDS banner) on a light tile.
                    let bg = 0xF2_F4_F8;
                    self.round_rect(x, y, w, h, RADIUS, bg);
                    let d = (ICON_SIZE as i32) * 2; // draw at 2× = 64px
                    self.blit_icon(x + (w - d) / 2, y + (h - d) / 2, &icon, bg);
                } else {
                    let color = if playable { palette_for(&title) } else { 0x5A_64_72 };
                    self.round_rect(x, y, w, h, RADIUS, color);
                    let inits = initials(&title);
                    let iw = inits.len() as i32 * 8 * 4;
                    let init_color = if playable { ICON_FG } else { 0xB8_C0_CC };
                    self.text(x + (w - iw) / 2, y + (h - 8 * 4) / 2, 4, init_color, &inits);
                }
                if !system.is_empty() {
                    self.badge(x + 6, y + 6, &system);
                }
                if !playable {
                    self.soon_ribbon(x, y, w, h);
                }
                self.label(x, y + h + 2, w, &title);
            }
        }

        self.draw_scrollbar();

        // Bars drawn last so they cover tiles scrolled past the top/bottom edges.
        self.fill_rect(0, 0, HOME_W as i32, TOPBAR, BAR_BG);
        self.text(12, 8, 2, BAR_TEXT, "EMULATORS");
        self.text(HOME_W as i32 - 52, 8, 2, CURSOR, "SET");

        // Bottom action bar: selected name + the OPEN hint.
        let by = HOME_H as i32 - BOTBAR;
        self.fill_rect(0, by, HOME_W as i32, BOTBAR, BAR_BG);
        let name = if self.selected == self.entries.len() {
            "Add game".to_string()
        } else {
            self.entries
                .get(self.selected)
                .map(|e| e.title.clone())
                .unwrap_or_default()
        };
        self.label_color(12, by + 10, 28, &name, BAR_TEXT, 2);
        self.text(HOME_W as i32 - 96, by + 10, 2, CURSOR, "A OPEN");
    }

    fn draw_scrollbar(&mut self) {
        let max = self.max_scroll();
        if max <= 0 {
            return;
        }
        let track_h = self.view_h();
        let content = self.content_h();
        let thumb_h = (track_h * track_h / content.max(1)).clamp(16, track_h);
        let thumb_y = GRID_TOP + (track_h - thumb_h) * self.scroll / max;
        self.fill_rect(HOME_W as i32 - 5, thumb_y, 3, thumb_h, CURSOR);
    }

    fn render_settings(&mut self) {
        self.vgradient(BG_TOP, BG_BOT);
        self.fill_rect(0, 0, HOME_W as i32, TOPBAR, BAR_BG);
        self.text(12, 8, 2, BAR_TEXT, "SETTINGS");

        let rows: [(&str, String); SETTINGS_ROWS] = [
            ("Crisp pixels", if self.crisp { "ON".to_string() } else { "OFF".to_string() }),
            ("Clear all games", if self.confirm_clear { "Press again".to_string() } else { String::new() }),
            ("Back", String::new()),
        ];
        for row in 0..SETTINGS_ROWS {
            let label = rows[row].0;
            let value = rows[row].1.clone();
            let (rx, ry, rw, rh) = self.setting_row_rect(row);
            if row == self.set_sel {
                self.round_rect(rx - 3, ry - 3, rw + 6, rh + 6, 13, CURSOR);
            }
            self.round_rect(rx, ry, rw, rh, 10, 0xEE_F2_F8);
            self.text(rx + 14, ry + (rh - 16) / 2, 2, LABEL, label);
            if !value.is_empty() {
                let vw = value.len() as i32 * 16;
                self.text(rx + rw - 14 - vw, ry + (rh - 16) / 2, 2, 0x2E_6A_A8, &value);
            }
        }

        let by = HOME_H as i32 - BOTBAR;
        self.fill_rect(0, by, HOME_W as i32, BOTBAR, BAR_BG);
        self.text(12, by + 10, 2, CURSOR, "A SELECT");
        self.text(HOME_W as i32 - 112, by + 10, 2, BAR_TEXT, "B BACK");
    }

    fn vgradient(&mut self, top: u32, bot: u32) {
        for y in 0..HOME_H {
            let t = y as f32 / (HOME_H as f32 - 1.0);
            let c = lerp(top, bot, t);
            for x in 0..HOME_W {
                let i = (y * HOME_W + x) * 4;
                self.fb[i] = (c >> 16) as u8;
                self.fb[i + 1] = (c >> 8) as u8;
                self.fb[i + 2] = c as u8;
                self.fb[i + 3] = 0xFF;
            }
        }
    }

    #[inline]
    fn px(&mut self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 || x >= HOME_W as i32 || y >= HOME_H as i32 {
            return;
        }
        let i = (y as usize * HOME_W + x as usize) * 4;
        self.fb[i] = (color >> 16) as u8;
        self.fb[i + 1] = (color >> 8) as u8;
        self.fb[i + 2] = color as u8;
        self.fb[i + 3] = 0xFF;
    }

    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        for dy in 0..h {
            for dx in 0..w {
                self.px(x + dx, y + dy, color);
            }
        }
    }

    /// Filled rounded rectangle (corner radius `r`).
    fn round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, color: u32) {
        for dy in 0..h {
            for dx in 0..w {
                // Clamp the sample to the nearest corner center; pixels in a
                // corner zone are kept only inside the quarter-circle.
                let cx = if dx < r {
                    r
                } else if dx >= w - r {
                    w - 1 - r
                } else {
                    dx
                };
                let cy = if dy < r {
                    r
                } else if dy >= h - r {
                    h - 1 - r
                } else {
                    dy
                };
                let (ddx, ddy) = (dx - cx, dy - cy);
                if ddx * ddx + ddy * ddy <= r * r {
                    self.px(x + dx, y + dy, color);
                }
            }
        }
    }

    fn glyph(&mut self, x: i32, y: i32, scale: i32, color: u32, ch: u8) {
        if ch >= 128 {
            return;
        }
        let g = BASIC_LEGACY[ch as usize];
        for (row, bits) in g.iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) != 0 {
                    self.fill_rect(x + col * scale, y + row as i32 * scale, scale, scale, color);
                }
            }
        }
    }

    fn text(&mut self, x: i32, y: i32, scale: i32, color: u32, s: &str) {
        let mut cx = x;
        for ch in s.bytes() {
            self.glyph(cx, y, scale, color, ch);
            cx += 8 * scale;
        }
    }

    /// Blit a 32×32 RGBA icon at 2× (nearest-neighbor). Transparent pixels
    /// (alpha 0) fall back to `bg` so they blend into the tile.
    fn blit_icon(&mut self, x: i32, y: i32, icon: &[u8], bg: u32) {
        for iy in 0..ICON_SIZE {
            for ix in 0..ICON_SIZE {
                let o = (iy * ICON_SIZE + ix) * 4;
                let color = if icon[o + 3] == 0 {
                    bg
                } else {
                    ((icon[o] as u32) << 16) | ((icon[o + 1] as u32) << 8) | (icon[o + 2] as u32)
                };
                let px = x + ix as i32 * 2;
                let py = y + iy as i32 * 2;
                self.fill_rect(px, py, 2, 2, color);
            }
        }
    }

    /// Small dark pill with a system label, top-left of an icon.
    fn badge(&mut self, x: i32, y: i32, label: &str) {
        let tw = label.len() as i32 * 8 + 6;
        self.round_rect(x, y, tw, 12, 3, 0x16_24_36);
        self.text(x + 3, y + 2, 1, 0xCF_E0_F0, label);
    }

    /// "SOON" strip across an icon for systems without a core yet.
    fn soon_ribbon(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let sy = y + h / 2 - 9;
        self.fill_rect(x, sy, w, 18, 0x16_24_36);
        let tw = 4 * 8 * 2; // "SOON", scale 2
        self.text(x + (w - tw) / 2, sy + 1, 2, 0xFF_D2_4D, "SOON");
    }

    /// Centered, width-clamped single-line label (dark, scale 1).
    fn label(&mut self, x: i32, y: i32, w: i32, s: &str) {
        self.label_color(x, y, w, s, LABEL, 1);
    }

    fn label_color(&mut self, x: i32, y: i32, w: i32, s: &str, color: u32, scale: i32) {
        let cw = 8 * scale;
        let max = (w / cw).max(1) as usize;
        let shown: String = if s.len() > max {
            s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
        } else {
            s.to_string()
        };
        // The ellipsis isn't in the legacy font; render it as '~' fallback.
        let shown = shown.replace('…', "~");
        let tw = shown.len() as i32 * cw;
        self.text(x + (w - tw).max(0) / 2, y, scale, color, &shown);
    }
}

fn lerp(a: u32, b: u32, t: f32) -> u32 {
    let mix = |sa: u32, sb: u32| {
        let v = sa as f32 + (sb as f32 - sa as f32) * t;
        (v.round() as u32) & 0xFF
    };
    let r = mix((a >> 16) & 0xFF, (b >> 16) & 0xFF);
    let g = mix((a >> 8) & 0xFF, (b >> 8) & 0xFF);
    let bl = mix(a & 0xFF, b & 0xFF);
    (r << 16) | (g << 8) | bl
}

fn palette_for(title: &str) -> u32 {
    let h: u32 = title.bytes().fold(0u32, |acc, c| acc.wrapping_mul(31).wrapping_add(c as u32));
    PALETTE[(h as usize) % PALETTE.len()]
}

/// Up to two leading alphanumeric chars, uppercased — the icon monogram.
fn initials(title: &str) -> String {
    let s: String = title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    if s.is_empty() {
        "?".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_navigates() {
        let mut h = Home::new();
        h.set_games_from_str("id1\nid2", "Zelda\nMetroid", "GBA\nGBA", "1\n1");
        // The light gradient background means it's not all one flat color.
        let first = &h.framebuffer()[0..3];
        assert!(h.framebuffer().chunks(4).any(|p| &p[0..3] != first));

        h.run_frame(KEY_RIGHT);
        match h.run_frame(KEY_A) {
            HomeAction::Launch(id) => assert_eq!(id, "id2"),
            _ => panic!("expected launch of id2"),
        }
    }

    #[test]
    fn taps_select_and_launch() {
        let mut h = Home::new();
        h.set_games_from_str("a\nb\nc", "Alpha\nBeta\nGamma", "GBA\nGBA\nGBA", "1\n1\n1");
        let (x, y, w, _) = h.tile_rect(2); // third game's icon
        match h.pointer(x + w / 2, y + 4) {
            HomeAction::Launch(id) => assert_eq!(id, "c"),
            _ => panic!("expected launch of c"),
        }
        // Tapping the "+" tile (index == game count) asks to add.
        let add = h.entries.len();
        let (x, y, w, _) = h.tile_rect(add);
        assert!(matches!(h.pointer(x + w / 2, y + 4), HomeAction::AddGame));
    }

    #[test]
    fn settings_toggle_and_clear() {
        let mut h = Home::new();
        h.set_games_from_str("a", "Alpha", "GBA", "1");
        h.run_frame(KEY_SELECT); // open settings
        // Row 0 = crisp toggle (starts ON → OFF).
        match h.run_frame(KEY_A) {
            HomeAction::SetCrisp(v) => assert!(!v),
            _ => panic!("expected SetCrisp"),
        }
        h.run_frame(KEY_DOWN); // → "Clear all games"
        assert!(matches!(h.run_frame(KEY_A), HomeAction::None)); // arms confirm
        h.run_frame(0); // release
        assert!(matches!(h.run_frame(KEY_A), HomeAction::ClearAll)); // confirms
    }

    #[test]
    fn scrolls_to_keep_selection_visible() {
        let mut h = Home::new();
        let ids = (0..30).map(|i| format!("g{i}")).collect::<Vec<_>>().join("\n");
        let titles = (0..30).map(|i| format!("Game {i}")).collect::<Vec<_>>().join("\n");
        let sys = vec!["GBA"; 30].join("\n");
        let pl = vec!["1"; 30].join("\n");
        h.set_games_from_str(&ids, &titles, &sys, &pl);
        assert_eq!(h.scroll, 0);
        for _ in 0..8 {
            h.run_frame(KEY_DOWN);
            h.run_frame(0); // release for the next edge
        }
        assert!(h.scroll > 0, "grid should scroll to follow the selection");
        assert!(h.scroll <= h.max_scroll());
        // Scrolling back up returns to the top.
        for _ in 0..10 {
            h.run_frame(KEY_UP);
            h.run_frame(0);
        }
        assert_eq!(h.scroll, 0);
    }

    #[test]
    fn unplayable_is_coming_soon() {
        let mut h = Home::new();
        h.set_games_from_str("g\nd", "Gbagame\nDsgame", "GBA\nNDS", "1\n0");
        let (x, y, w, _) = h.tile_rect(1); // the NDS game — no core yet
        match h.pointer(x + w / 2, y + 4) {
            HomeAction::ComingSoon(label) => assert_eq!(label, "NDS"),
            _ => panic!("expected coming soon"),
        }
    }
}
