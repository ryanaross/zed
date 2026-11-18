//! fork: terminal-only agent panel.
//!
//! Ryan runs agents only in terminals, so the agent panel never offers or falls back to the
//! Zed Agent or external (ACP) agents: the toolbar plus opens a terminal, new entries are
//! terminals, and where upstream would show an empty agent draft (after a terminal closes,
//! say) the panel shows the most recently used terminal instead.

use gpui::{App, Context, FocusHandle, Global, IntoElement, Window};
use ui::{IconButton, IconName, IconSize, Tooltip, prelude::*};

use crate::{AgentThreadSource, NewThread, agent_panel::AgentPanel};

/// Overrides the default. Upstream's tests expect agent drafts, so the mode is off in test builds
/// (`cfg(test)` or `test-support`) unless a test turns it on.
pub(crate) struct TerminalOnly(pub bool);

impl Global for TerminalOnly {}

pub(crate) fn enabled(cx: &App) -> bool {
    cx.try_global::<TerminalOnly>()
        .map_or(!cfg!(any(test, feature = "test-support")), |mode| mode.0)
}

/// Replaces the agent draft with a terminal. Returns false when the panel should fall back
/// to upstream's draft (mode off, or the project can't run terminals).
pub(crate) fn activate_terminal_instead_of_draft(
    panel: &mut AgentPanel,
    focus: bool,
    window: &mut Window,
    cx: &mut Context<AgentPanel>,
) -> bool {
    if !enabled(cx) || !panel.supports_terminal(cx) {
        return false;
    }
    let most_recent = panel
        .terminals(cx)
        .into_iter()
        .max_by_key(|terminal| {
            crate::ryanzed_activity::terminal_interacted_at(terminal.id, cx)
                .map_or(terminal.created_at, |time| time.max(terminal.created_at))
        })
        .map(|terminal| terminal.id);
    match most_recent {
        Some(terminal_id) => panel.activate_terminal(terminal_id, focus, window, cx),
        None => panel.new_terminal(None, AgentThreadSource::AgentPanel, window, cx),
    }
    true
}

/// The toolbar plus: a new terminal, no agent menu.
pub(crate) fn render_new_terminal_button(focus_handle: FocusHandle) -> impl IntoElement {
    IconButton::new("ryanzed_new_terminal_btn", IconName::Plus)
        .icon_size(IconSize::Small)
        .tooltip(move |_window, cx| {
            Tooltip::for_action_in("New Terminal", &NewThread, &focus_handle, cx)
        })
        .on_click(|_, window, cx| window.dispatch_action(Box::new(NewThread), cx))
}
