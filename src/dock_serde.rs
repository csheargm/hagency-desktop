//! JIT PORT: serde for the saved dock state.
//!
//! The fork's makepad derived serde on `LiveId`, `DockItem` and the splitter
//! types behind a `serde` feature. Upstream makepad has no serde, so these
//! `#[serde(with = ..)]` helpers write the same JSON the fork did: a `LiveId`
//! is its `u64`, and a `DockItem` is an externally tagged enum.

use std::collections::HashMap;

use makepad_widgets::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Serialize, Deserialize)]
enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Serialize, Deserialize)]
enum Align {
    FromA(f64),
    FromB(f64),
    Weighted(f64),
}

#[derive(Serialize, Deserialize)]
enum Item {
    Splitter { axis: Axis, align: Align, a: u64, b: u64 },
    Tabs {
        tabs: Vec<u64>,
        selected: usize,
        closable: bool,
        #[serde(default)]
        hide_tab_bar: bool,
    },
    Tab { name: String, template: u64, kind: u64 },
}

fn to_item(item: &DockItem) -> Item {
    match item {
        DockItem::Splitter { axis, align, a, b } => Item::Splitter {
            axis: match axis {
                SplitterAxis::Horizontal => Axis::Horizontal,
                SplitterAxis::Vertical => Axis::Vertical,
            },
            align: match *align {
                SplitterAlign::FromA(v) => Align::FromA(v),
                SplitterAlign::FromB(v) => Align::FromB(v),
                SplitterAlign::Weighted(v) => Align::Weighted(v),
            },
            a: a.0,
            b: b.0,
        },
        DockItem::Tabs { tabs, selected, closable, hide_tab_bar } => Item::Tabs {
            tabs: tabs.iter().map(|id| id.0).collect(),
            selected: *selected,
            closable: *closable,
            hide_tab_bar: *hide_tab_bar,
        },
        DockItem::Tab { name, template, kind } => Item::Tab {
            name: name.clone(),
            template: template.0,
            kind: kind.0,
        },
    }
}

fn from_item(item: Item) -> DockItem {
    match item {
        Item::Splitter { axis, align, a, b } => DockItem::Splitter {
            axis: match axis {
                Axis::Horizontal => SplitterAxis::Horizontal,
                Axis::Vertical => SplitterAxis::Vertical,
            },
            align: match align {
                Align::FromA(v) => SplitterAlign::FromA(v),
                Align::FromB(v) => SplitterAlign::FromB(v),
                Align::Weighted(v) => SplitterAlign::Weighted(v),
            },
            a: LiveId(a),
            b: LiveId(b),
        },
        Item::Tabs { tabs, selected, closable, hide_tab_bar } => DockItem::Tabs {
            tabs: tabs.into_iter().map(LiveId).collect(),
            selected,
            closable,
            hide_tab_bar,
        },
        Item::Tab { name, template, kind } => DockItem::Tab {
            name,
            template: LiveId(template),
            kind: LiveId(kind),
        },
    }
}

/// `HashMap<LiveId, DockItem>`.
pub mod dock_items {
    use super::*;

    pub fn serialize<S: Serializer>(map: &HashMap<LiveId, DockItem>, s: S) -> Result<S::Ok, S::Error> {
        map.iter()
            .map(|(id, item)| (id.0, to_item(item)))
            .collect::<HashMap<u64, Item>>()
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<HashMap<LiveId, DockItem>, D::Error> {
        Ok(HashMap::<u64, Item>::deserialize(d)?
            .into_iter()
            .map(|(id, item)| (LiveId(id), from_item(item)))
            .collect())
    }
}

/// `HashMap<LiveId, T>` for a serde `T`.
pub mod live_id_map {
    use super::*;

    pub fn serialize<S: Serializer, T: Serialize>(map: &HashMap<LiveId, T>, s: S) -> Result<S::Ok, S::Error> {
        map.iter().map(|(id, v)| (id.0, v)).collect::<HashMap<u64, &T>>().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<HashMap<LiveId, T>, D::Error> {
        Ok(HashMap::<u64, T>::deserialize(d)?
            .into_iter()
            .map(|(id, v)| (LiveId(id), v))
            .collect())
    }
}
