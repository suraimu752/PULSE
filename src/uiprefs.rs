//! Column order, resource order, and the performance splitter.
//!
//! Name stays visible. Hiding the selected resource falls forward to the next
//! visible one. Settings can be stored in a local file; nothing is sent out.

use std::path::PathBuf;

pub const COLUMN_COUNT: usize = 11;
pub const RESOURCE_COUNT: usize = 9;
pub const MIN_RESOURCE_WIDTH: f32 = 180.0;
pub const MIN_DETAIL_WIDTH: f32 = 260.0;
pub const SPLITTER_WIDTH: f32 = 6.0;

const COLUMN_NAMES: [&str; COLUMN_COUNT] = [
    "name",
    "pid",
    "cpu",
    "memory",
    "disk",
    "network",
    "gpu3d",
    "gpu-encode",
    "gpu-decode",
    "vram",
    "shared",
];

#[derive(Clone, Debug, PartialEq)]
pub struct UiPrefs {
    pub column_order: Vec<i32>,
    pub column_shown: Vec<bool>,
    pub resource_order: Vec<i32>,
    pub resource_shown: Vec<bool>,
    pub resource_width: f32,
    /// Left navigation. Missing `nav=` in an older file stays collapsed.
    pub nav_expanded: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            column_order: (0..COLUMN_COUNT as i32).collect(),
            column_shown: vec![true; COLUMN_COUNT],
            resource_order: (0..RESOURCE_COUNT as i32).collect(),
            resource_shown: vec![true; RESOURCE_COUNT],
            resource_width: 220.0,
            nav_expanded: false,
        }
    }
}

impl UiPrefs {
    pub fn toggle_column(&mut self, id: i32) {
        let Some(index) = checked_index(id, COLUMN_COUNT) else {
            return;
        };
        if index == 0 {
            self.column_shown[0] = true;
            return;
        }
        self.column_shown[index] = !self.column_shown[index];
    }

    #[cfg(test)]
    pub fn move_column(&mut self, id: i32, delta: i32) {
        move_in(&mut self.column_order, id, delta);
    }

    #[cfg(test)]
    pub fn drag_column(&mut self, id: i32, dx: f32, widths: &[f32]) {
        drag_in(&mut self.column_order, &self.column_shown, widths, id, dx);
    }

    /// Move `id` to the column gap under pointer `x` (row coordinates, pixels).
    pub fn place_column(&mut self, id: i32, x: f32, widths: &[f32]) -> bool {
        let Some(from) = self.column_order.iter().position(|item| *item == id) else {
            return false;
        };
        let (dest, _) = drop_slot(&self.column_order, &self.column_shown, widths, x);
        if dest == from || dest == from + 1 {
            return false;
        }
        let value = self.column_order.remove(from);
        let insert = if dest > from { dest - 1 } else { dest };
        self.column_order.insert(insert, value);
        true
    }

    /// X of the insertion gap in the same coordinates as `place_column`.
    pub fn column_drop_mark(&self, x: f32, widths: &[f32]) -> f32 {
        drop_slot(&self.column_order, &self.column_shown, widths, x).1
    }

    /// Row coordinate of a pointer that is `local` pixels into column `id`.
    pub fn column_pointer_x(&self, id: i32, local: f32, widths: &[f32]) -> f32 {
        if !local.is_finite() {
            return 0.0;
        }
        let mut cursor = 0.0_f32;
        for item in &self.column_order {
            if *item == id {
                return cursor + local.max(0.0);
            }
            cursor += visible_width(*item, &self.column_shown, widths);
        }
        local.max(0.0)
    }

    /// Returns the resource that should stay selected.
    pub fn toggle_resource(&mut self, id: i32, selected: i32) -> i32 {
        let Some(index) = checked_index(id, RESOURCE_COUNT) else {
            return selected;
        };
        if self.resource_shown[index] {
            let visible = self.resource_shown.iter().filter(|on| **on).count();
            if visible <= 1 {
                return selected;
            }
            self.resource_shown[index] = false;
            if selected == id {
                return self
                    .resource_order
                    .iter()
                    .copied()
                    .find(|item| self.resource_shown[*item as usize])
                    .unwrap_or(selected);
            }
        } else {
            self.resource_shown[index] = true;
        }
        selected
    }

    pub fn move_resource(&mut self, id: i32, delta: i32) {
        move_in(&mut self.resource_order, id, delta);
    }

    pub fn hide_column_named(&mut self, name: &str) -> bool {
        let Some(id) = column_id(name) else {
            return false;
        };
        if id == 0 {
            return false;
        }
        self.column_shown[id as usize] = false;
        true
    }

    pub fn hide_resource_named(&mut self, name: &str) -> bool {
        let Some(id) = resource_id(name) else {
            return false;
        };
        if self.resource_shown.iter().filter(|on| **on).count() <= 1 {
            return false;
        }
        self.resource_shown[id as usize] = false;
        true
    }

    pub fn to_file(&self) -> String {
        let columns = self
            .column_order
            .iter()
            .map(|id| COLUMN_NAMES[*id as usize])
            .collect::<Vec<_>>()
            .join(",");
        let hidden = (0..COLUMN_COUNT)
            .filter(|id| !self.column_shown[*id])
            .map(|id| COLUMN_NAMES[id])
            .collect::<Vec<_>>()
            .join(",");
        let resources = self
            .resource_order
            .iter()
            .map(|id| resource_name(*id))
            .collect::<Vec<_>>()
            .join(",");
        let resource_hidden = (0..RESOURCE_COUNT)
            .filter(|id| !self.resource_shown[*id])
            .map(|id| resource_name(id as i32))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "columns={columns}\nhidden={hidden}\nresources={resources}\nresource-hidden={resource_hidden}\nsplit={}\nnav={}\n",
            self.resource_width,
            if self.nav_expanded {
                "expanded"
            } else {
                "collapsed"
            }
        )
    }

    pub fn parse(text: &str) -> Self {
        let mut prefs = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.trim().split_once('=') else {
                continue;
            };
            match key {
                "columns" => {
                    if let Some(order) = parse_names(value, column_id, COLUMN_COUNT) {
                        prefs.column_order = order;
                    }
                }
                "hidden" => {
                    prefs.column_shown = vec![true; COLUMN_COUNT];
                    for name in value.split(',').filter(|name| !name.is_empty()) {
                        if let Some(id) = column_id(name) {
                            if id != 0 {
                                prefs.column_shown[id as usize] = false;
                            }
                        }
                    }
                }
                "resources" => {
                    if let Some(order) = parse_names(value, resource_id, RESOURCE_COUNT) {
                        prefs.resource_order = order;
                    }
                }
                "resource-hidden" => {
                    prefs.resource_shown = vec![true; RESOURCE_COUNT];
                    for name in value.split(',').filter(|name| !name.is_empty()) {
                        if let Some(id) = resource_id(name) {
                            prefs.resource_shown[id as usize] = false;
                        }
                    }
                    if !prefs.resource_shown.iter().any(|on| *on) {
                        prefs.resource_shown[0] = true;
                    }
                }
                "split" => {
                    if let Ok(width) = value.trim().parse::<f32>() {
                        if width.is_finite() && width > 0.0 {
                            prefs.resource_width = width;
                        }
                    }
                }
                // Absent on purpose: older files have no `nav=` and stay collapsed.
                "nav" => {
                    prefs.nav_expanded = matches!(value.trim(), "expanded" | "1" | "true");
                }
                _ => {}
            }
        }
        prefs
    }
}

pub fn column_id(name: &str) -> Option<i32> {
    COLUMN_NAMES
        .iter()
        .position(|item| *item == name)
        .map(|id| id as i32)
}

pub fn resource_id(name: &str) -> Option<i32> {
    match name {
        "cpu" => Some(0),
        "memory" => Some(1),
        "disk" => Some(2),
        "network" => Some(3),
        "gpu3d" => Some(4),
        "gpu-encode" => Some(5),
        "gpu-decode" => Some(6),
        "vram" => Some(7),
        "shared" => Some(8),
        _ => None,
    }
}

fn resource_name(id: i32) -> &'static str {
    match id {
        0 => "cpu",
        1 => "memory",
        2 => "disk",
        3 => "network",
        4 => "gpu3d",
        5 => "gpu-encode",
        6 => "gpu-decode",
        7 => "vram",
        _ => "shared",
    }
}

/// Keep both panes usable. `host` is the width available to the list, the
/// splitter, and the detail card together.
#[cfg_attr(not(test), allow(dead_code))]
pub fn clamp_resource_width(preferred: f32, host: f32) -> f32 {
    let max_left = (host - MIN_DETAIL_WIDTH - SPLITTER_WIDTH).max(MIN_RESOURCE_WIDTH);
    if !preferred.is_finite() {
        return MIN_RESOURCE_WIDTH.min(max_left);
    }
    preferred.clamp(MIN_RESOURCE_WIDTH, max_left)
}

pub fn config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("PULSE_CONFIG_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("ui.cfg"));
        }
    }
    config_dir().map(|base| base.join("PULSE").join("ui.cfg"))
}

fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }
}

pub fn load() -> UiPrefs {
    let Some(path) = config_path() else {
        return UiPrefs::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return UiPrefs::default();
    };
    UiPrefs::parse(&text)
}

pub fn save(prefs: &UiPrefs) {
    let Some(path) = config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = std::fs::write(path, prefs.to_file());
}

fn checked_index(id: i32, len: usize) -> Option<usize> {
    let index = usize::try_from(id).ok()?;
    (index < len).then_some(index)
}

fn move_in(order: &mut [i32], id: i32, delta: i32) {
    let Some(from) = order.iter().position(|item| *item == id) else {
        return;
    };
    let dest = from as i32 + delta;
    if dest < 0 || dest >= order.len() as i32 {
        return;
    }
    order.swap(from, dest as usize);
}

fn visible_width(id: i32, shown: &[bool], widths: &[f32]) -> f32 {
    let index = usize::try_from(id).unwrap_or(usize::MAX);
    if shown.get(index).copied().unwrap_or(false) {
        widths.get(index).copied().unwrap_or(0.0).max(0.0)
    } else {
        0.0
    }
}

fn drop_slot(order: &[i32], shown: &[bool], widths: &[f32], x: f32) -> (usize, f32) {
    if !x.is_finite() || widths.len() < order.len() {
        return (0, 0.0);
    }
    let mut cursor = 0.0_f32;
    for (index, item) in order.iter().copied().enumerate() {
        let width = visible_width(item, shown, widths);
        if width <= 0.0 {
            continue;
        }
        if x < cursor + width / 2.0 {
            return (index, cursor);
        }
        cursor += width;
    }
    (order.len(), cursor)
}

#[cfg(test)]
fn drag_in(order: &mut [i32], shown: &[bool], widths: &[f32], id: i32, dx: f32) {
    if !dx.is_finite() || dx.abs() < 8.0 || widths.len() < order.len() {
        return;
    }
    let Some(from) = order.iter().position(|item| *item == id) else {
        return;
    };
    let mut mids = Vec::with_capacity(order.len());
    let mut cursor = 0.0;
    for item in order.iter() {
        let index = *item as usize;
        let width = if shown.get(index).copied().unwrap_or(false) {
            widths.get(index).copied().unwrap_or(0.0).max(0.0)
        } else {
            0.0
        };
        mids.push(cursor + width / 2.0);
        cursor += width;
    }
    let target = mids[from] + dx;
    let mut dest = from;
    if dx > 0.0 {
        for (index, mid) in mids.iter().enumerate().skip(from + 1) {
            if target > *mid {
                dest = index;
            }
        }
    } else {
        for (index, mid) in mids.iter().enumerate().take(from) {
            if target < *mid {
                dest = index;
                break;
            }
        }
    }
    if dest == from {
        return;
    }
    let value = order[from];
    if dest > from {
        order.copy_within(from + 1..=dest, from);
    } else {
        order.copy_within(dest..from, dest + 1);
    }
    order[dest] = value;
}

fn parse_names(value: &str, id_of: fn(&str) -> Option<i32>, count: usize) -> Option<Vec<i32>> {
    let mut order = Vec::new();
    for name in value.split(',').filter(|name| !name.is_empty()) {
        let id = id_of(name.trim())?;
        if order.contains(&id) {
            return None;
        }
        order.push(id);
    }
    if order.len() != count {
        return None;
    }
    Some(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_column_stays_visible_and_others_toggle() {
        let mut prefs = UiPrefs::default();
        prefs.toggle_column(0);
        assert!(prefs.column_shown[0]);
        prefs.toggle_column(4);
        assert!(!prefs.column_shown[4]);
        prefs.toggle_column(4);
        assert!(prefs.column_shown[4]);
    }

    #[test]
    fn move_and_drag_reorder_columns() {
        let mut prefs = UiPrefs::default();
        prefs.move_column(2, -1);
        assert_eq!(prefs.column_order[1], 2);
        assert_eq!(prefs.column_order[2], 1);
        let widths = vec![100.0; COLUMN_COUNT];
        prefs.drag_column(2, 150.0, &widths);
        assert_eq!(prefs.column_order[2], 2);
    }

    #[test]
    fn drop_places_the_column_at_the_pointer_gap() {
        let mut prefs = UiPrefs::default();
        let widths = vec![100.0; COLUMN_COUNT];
        assert!((prefs.column_drop_mark(260.0, &widths) - 300.0).abs() < 0.1);
        assert!(prefs.place_column(0, 260.0, &widths));
        assert_eq!(&prefs.column_order[..4], &[1, 2, 0, 3]);
        assert!(!prefs.place_column(1, 40.0, &widths));
        assert!(prefs.column_shown[0]);
        prefs.toggle_column(0);
        assert!(prefs.column_shown[0]);
    }

    #[test]
    fn hiding_the_selected_resource_falls_forward() {
        let mut prefs = UiPrefs::default();
        let next = prefs.toggle_resource(0, 0);
        assert_eq!(next, 1);
        assert!(!prefs.resource_shown[0]);
        for id in 1..RESOURCE_COUNT as i32 - 1 {
            prefs.toggle_resource(id, next);
        }
        let last = prefs.toggle_resource(8, 8);
        assert_eq!(last, 8);
        assert!(prefs.resource_shown[8]);
    }

    #[test]
    fn narrow_window_keeps_both_panes() {
        assert_eq!(clamp_resource_width(400.0, 500.0), 500.0 - 260.0 - 6.0);
        assert_eq!(clamp_resource_width(100.0, 800.0), 180.0);
        assert_eq!(clamp_resource_width(220.0, 700.0), 220.0);
    }

    #[test]
    fn file_round_trip_keeps_hidden_columns() {
        let mut prefs = UiPrefs::default();
        prefs.toggle_column(5);
        prefs.move_column(3, -1);
        prefs.toggle_resource(4, 0);
        prefs.resource_width = 240.0;
        prefs.nav_expanded = true;
        let parsed = UiPrefs::parse(&prefs.to_file());
        assert_eq!(parsed, prefs);
    }

    #[test]
    fn nav_stays_collapsed_when_the_file_does_not_say() {
        assert!(!UiPrefs::default().nav_expanded);
        let old = UiPrefs::parse("columns=name,pid\nsplit=220\n");
        assert!(!old.nav_expanded);
        assert!(!UiPrefs::parse("nav=collapsed\n").nav_expanded);
        assert!(UiPrefs::parse("nav=expanded\n").nav_expanded);
        let open = UiPrefs {
            nav_expanded: true,
            ..UiPrefs::default()
        };
        assert!(UiPrefs::parse(&open.to_file()).nav_expanded);
        let shut = UiPrefs {
            nav_expanded: false,
            ..open
        };
        assert!(!UiPrefs::parse(&shut.to_file()).nav_expanded);
    }
}
