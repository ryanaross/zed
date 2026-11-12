//! fork: terminal threads sort by last message.
//!
//! Upstream only stores when a terminal thread was created, so the sidebar shows and
//! sorts terminals by that. Here every Enter pressed in an agent-panel terminal stamps the
//! terminal's "last messaged" time, kept in the key-value store as one JSON map, and the
//! sidebar uses it instead of the creation time.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Entity, Global};
use terminal::Terminal;
use util::ResultExt as _;

use crate::{agent_panel::TerminalId, terminal_thread_metadata_store::TerminalThreadMetadataStore};

const KEY: &str = "ryanzed_terminal_interacted_at";
/// Stamps older than this are dropped when the map is saved.
const KEEP_DAYS: i64 = 90;

#[derive(Default)]
struct Activity {
    loaded: bool,
    interacted_at: HashMap<TerminalId, DateTime<Utc>>,
    seen_submits: HashMap<TerminalId, u64>,
}

impl Global for Activity {}

fn load(cx: &mut App) -> &mut Activity {
    if !cx.default_global::<Activity>().loaded {
        let stored = KeyValueStore::global(cx)
            .read_kvp(KEY)
            .log_err()
            .flatten()
            .and_then(|json| {
                serde_json::from_str::<HashMap<String, DateTime<Utc>>>(&json).log_err()
            })
            .unwrap_or_default();
        let activity = cx.default_global::<Activity>();
        activity.loaded = true;
        activity.interacted_at = stored
            .into_iter()
            .filter_map(|(key, time)| Some((TerminalId::from_key_string(&key).ok()?, time)))
            .collect();
    }
    cx.default_global::<Activity>()
}

pub(crate) fn init(cx: &mut App) {
    load(cx);
}

/// When the user last pressed Enter in this terminal, if ever.
pub fn terminal_interacted_at(id: TerminalId, cx: &App) -> Option<DateTime<Utc>> {
    cx.try_global::<Activity>()?.interacted_at.get(&id).copied()
}

/// Called whenever the terminal reports activity; stamps the terminal when the user has
/// pressed Enter since the last call.
pub(crate) fn observe(id: TerminalId, terminal: &Entity<Terminal>, cx: &mut App) {
    let submits = terminal.read(cx).submit_count();
    let activity = load(cx);
    let seen = activity.seen_submits.entry(id).or_insert(submits);
    if *seen == submits {
        return;
    }
    *seen = submits;

    let now = Utc::now();
    activity.interacted_at.insert(id, now);
    let cutoff = now - Duration::days(KEEP_DAYS);
    activity.interacted_at.retain(|_, time| *time > cutoff);
    let json = serde_json::to_string(
        &activity
            .interacted_at
            .iter()
            .map(|(id, time)| (id.to_key_string(), *time))
            .collect::<HashMap<_, _>>(),
    );

    if let Some(json) = json.log_err() {
        let kvp = KeyValueStore::global(cx);
        cx.background_spawn(async move { kvp.write_kvp(KEY.to_string(), json).await.log_err() })
            .detach();
    }
    if let Some(store) = TerminalThreadMetadataStore::try_global(cx) {
        store.update(cx, |_, cx| cx.notify());
    }
}
