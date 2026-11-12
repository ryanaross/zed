//! fork: handle `zed://ryanzed/agent-terminal?cmd=…&path=…` (parsed in `open_listener.rs`).
//! Opens the local or ssh project at `path`, then runs `cmd` in a new agent-panel terminal.
//! Session-search tools use it to resume a claude, codex or pi session in its project.

use std::{path::PathBuf, sync::Arc};

use agent_ui::AgentPanel;
use gpui::{App, TaskExt as _};
use recent_projects::open_remote_project;
use remote::RemoteConnectionOptions;
use util::ResultExt as _;
use workspace::{AppState, SerializedWorkspaceLocation};

use crate::zed::{self, derive_paths_with_position, open_paths_with_positions};

pub(crate) fn open(
    command: String,
    open_paths: Vec<String>,
    remote_connection: Option<RemoteConnectionOptions>,
    open_behavior: Option<cli::OpenBehavior>,
    app_state: Arc<AppState>,
    cx: &mut App,
) {
    let location = match &remote_connection {
        Some(connection) => SerializedWorkspaceLocation::Remote(connection.clone()),
        None => SerializedWorkspaceLocation::Local,
    };
    let open_options = zed::open_options_for_request(open_behavior, &location, cx);
    cx.spawn(async move |cx| {
        let window = match remote_connection {
            Some(connection) => {
                let paths = open_paths.into_iter().map(PathBuf::from).collect();
                open_remote_project(connection, paths, app_state, open_options, cx).await?
            }
            None => {
                let paths = derive_paths_with_position(app_state.fs.as_ref(), open_paths).await;
                open_paths_with_positions(&paths, &[], false, app_state, open_options, cx)
                    .await?
                    .0
            }
        };

        let panels_task = window.update(cx, |multi_workspace, _, cx| {
            multi_workspace
                .workspace()
                .update(cx, |workspace, _| workspace.take_panels_task())
        })?;
        if let Some(task) = panels_task {
            task.await.log_err();
        }

        window.update(cx, |multi_workspace, window, cx| {
            multi_workspace.workspace().update(cx, |workspace, cx| {
                match workspace.focus_panel::<AgentPanel>(window, cx) {
                    Some(panel) => panel.update(cx, |panel, cx| {
                        panel.new_terminal_running(command, window, cx)
                    }),
                    None => log::warn!(
                        "agent-terminal url received but the AgentPanel is not registered \
                         (is `disable_ai` enabled?)"
                    ),
                }
            })
        })
    })
    .detach_and_log_err(cx);
}
