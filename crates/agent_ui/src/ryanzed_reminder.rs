//! fork: terminal thread reminders.
//!
//! The bell in the agent panel toolbar sets a "check back on this" reminder on the terminal
//! thread being shown. When it comes due, the terminal alerts exactly as if it had rung its
//! bell (sidebar dot, sound, pop-up). Reminders are kept in the key-value store as one JSON
//! map, so they survive restarts, and are cleared when the terminal is next activated.

use std::{
    collections::{HashMap, HashSet},
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, Local, Utc};
use db::kvp::KeyValueStore;
use gpui::{Anchor, App, AppContext as _, Context, Global, IntoElement, Window};
use ui::{ContextMenu, IconButton, IconName, IconSize, PopoverMenu, Tooltip, prelude::*};
use util::ResultExt as _;

use crate::{
    agent_panel::{AgentPanel, TerminalId},
    terminal_thread_metadata_store::TerminalThreadMetadataStore,
};

const KEY: &str = "ryanzed_terminal_remind_at";
const OPTIONS_HOURS: [i64; 4] = [2, 4, 8, 24];
const TICK: StdDuration = StdDuration::from_secs(60);

#[derive(Default)]
struct Reminders {
    loaded: bool,
    remind_at: HashMap<TerminalId, DateTime<Utc>>,
    /// Reminders already announced this session, so the ticker notifies observers once.
    fired: HashSet<TerminalId>,
    ticker_running: bool,
}

impl Global for Reminders {}

fn load(cx: &mut App) {
    if cx.default_global::<Reminders>().loaded {
        return;
    }
    let stored = KeyValueStore::global(cx)
        .read_kvp(KEY)
        .log_err()
        .flatten()
        .and_then(|json| serde_json::from_str::<HashMap<String, DateTime<Utc>>>(&json).log_err())
        .unwrap_or_default();
    let now = Utc::now();
    let reminders = cx.default_global::<Reminders>();
    reminders.loaded = true;
    reminders.remind_at = stored
        .into_iter()
        .filter_map(|(key, time)| Some((TerminalId::from_key_string(&key).ok()?, time)))
        .collect();
    // Anything that came due while closed is shown by `is_due` already.
    reminders.fired = reminders
        .remind_at
        .iter()
        .filter(|(_, time)| **time <= now)
        .map(|(id, _)| *id)
        .collect();
    ensure_ticker(cx);
}

pub(crate) fn init(cx: &mut App) {
    load(cx);
}

/// When the reminder on this terminal is set for, if any.
pub fn remind_at(id: TerminalId, cx: &App) -> Option<DateTime<Utc>> {
    cx.try_global::<Reminders>()?.remind_at.get(&id).copied()
}

/// Whether this terminal has a reminder that has come due.
pub fn is_due(id: TerminalId, cx: &App) -> bool {
    remind_at(id, cx).is_some_and(|time| time <= Utc::now())
}

fn set(id: TerminalId, hours: i64, cx: &mut App) {
    load(cx);
    cx.update_global::<Reminders, _>(|reminders, _| {
        reminders
            .remind_at
            .insert(id, Utc::now() + Duration::hours(hours));
        reminders.fired.remove(&id);
    });
    changed(cx);
}

pub(crate) fn clear(id: TerminalId, cx: &mut App) {
    if remind_at(id, cx).is_none() {
        return;
    }
    cx.update_global::<Reminders, _>(|reminders, _| {
        reminders.remind_at.remove(&id);
        reminders.fired.remove(&id);
    });
    changed(cx);
}

/// Called when the terminal is activated: a due reminder has done its job.
pub(crate) fn clear_if_due(id: TerminalId, cx: &mut App) {
    if is_due(id, cx) {
        clear(id, cx);
    }
}

fn changed(cx: &mut App) {
    persist(cx);
    ensure_ticker(cx);
    notify_sidebar(cx);
}

fn persist(cx: &mut App) {
    let Some(reminders) = cx.try_global::<Reminders>() else {
        return;
    };
    let json = serde_json::to_string(
        &reminders
            .remind_at
            .iter()
            .map(|(id, time)| (id.to_key_string(), *time))
            .collect::<HashMap<_, _>>(),
    );
    if let Some(json) = json.log_err() {
        let kvp = KeyValueStore::global(cx);
        cx.background_spawn(async move { kvp.write_kvp(KEY.to_string(), json).await.log_err() })
            .detach();
    }
}

fn notify_sidebar(cx: &mut App) {
    if let Some(store) = TerminalThreadMetadataStore::try_global(cx) {
        store.update(cx, |_, cx| cx.notify());
    }
}

/// Runs a once-a-minute wall-clock check while any reminder is pending. A single long
/// timer would fire late after the machine sleeps.
fn ensure_ticker(cx: &mut App) {
    let reminders = cx.default_global::<Reminders>();
    let pending = reminders
        .remind_at
        .keys()
        .any(|id| !reminders.fired.contains(id));
    if !pending || reminders.ticker_running {
        return;
    }
    reminders.ticker_running = true;
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(TICK).await;
            if !cx.update(tick) {
                break;
            }
        }
    })
    .detach();
}

/// Announces newly due reminders. Returns whether any reminder is still pending.
fn tick(cx: &mut App) -> bool {
    let now = Utc::now();
    let reminders = cx.default_global::<Reminders>();
    let newly_due: Vec<TerminalId> = reminders
        .remind_at
        .iter()
        .filter(|(id, time)| **time <= now && !reminders.fired.contains(id))
        .map(|(id, _)| *id)
        .collect();
    reminders.fired.extend(newly_due.iter().copied());
    let pending = reminders
        .remind_at
        .keys()
        .any(|id| !reminders.fired.contains(id));
    reminders.ticker_running = pending;
    if !newly_due.is_empty() {
        // Runs every panel's `observe_in_panel` callback.
        cx.update_global::<Reminders, _>(|_, _| {});
        notify_sidebar(cx);
    }
    pending
}

/// Makes the panel ring its terminals whose reminders are due.
pub(crate) fn observe_in_panel(window: &Window, cx: &mut Context<AgentPanel>) {
    cx.observe_global_in::<Reminders>(window, ring_due).detach();
}

fn ring_due(panel: &mut AgentPanel, window: &mut Window, cx: &mut Context<AgentPanel>) {
    let Some(reminders) = cx.try_global::<Reminders>() else {
        return;
    };
    let now = Utc::now();
    let due: Vec<TerminalId> = reminders
        .remind_at
        .iter()
        .filter(|(id, time)| **time <= now && panel.has_terminal(**id))
        .map(|(id, _)| *id)
        .collect();
    for id in due {
        if panel.active_terminal_visible(id, window, cx) {
            clear(id, cx);
        } else {
            panel.mark_terminal_notification(id, window, cx);
        }
    }
}

/// The toolbar bell for the terminal the panel is showing.
pub(crate) fn render_button(terminal_id: Option<TerminalId>, cx: &App) -> Option<impl IntoElement> {
    let terminal_id = terminal_id?;
    let remind_at = remind_at(terminal_id, cx);
    let tooltip: SharedString = match remind_at {
        Some(time) => format!(
            "Reminder at {}",
            time.with_timezone(&Local).format("%a %-I:%M %p")
        )
        .into(),
        None => "Remind Me Later\u{2026}".into(),
    };
    Some(
        PopoverMenu::new("ryanzed_reminder_menu")
            .trigger_with_tooltip(
                IconButton::new(
                    "ryanzed_reminder_btn",
                    if remind_at.is_some() {
                        IconName::BellRing
                    } else {
                        IconName::Bell
                    },
                )
                .icon_size(IconSize::Small)
                .toggle_state(remind_at.is_some()),
                Tooltip::text(tooltip),
            )
            .anchor(Anchor::TopRight)
            .menu(move |window, cx| {
                Some(ContextMenu::build(window, cx, |mut menu, _window, _cx| {
                    for hours in OPTIONS_HOURS {
                        menu = menu.entry(format!("Remind me in {hours} hours"), None, {
                            move |_, cx| set(terminal_id, hours, cx)
                        });
                    }
                    if remind_at.is_some() {
                        menu = menu
                            .separator()
                            .entry("Cancel reminder", None, move |_, cx| clear(terminal_id, cx));
                    }
                    menu
                }))
            }),
    )
}

/// Sets a reminder for an exact time, for tests that need one already due.
#[cfg(test)]
pub(crate) fn set_at(id: TerminalId, time: DateTime<Utc>, cx: &mut App) {
    load(cx);
    cx.update_global::<Reminders, _>(|reminders, _| {
        reminders.remind_at.insert(id, time);
        reminders.fired.remove(&id);
    });
    changed(cx);
}
