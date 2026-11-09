//! fork: keep CLI coding agents (claude, codex, pi) running in agent-panel terminals
//! across restarts, and keep closed agent terminals in thread history.
//!
//! While a terminal runs one of these agents, the agent's own session files are read to
//! find its session, and a resume command is recorded per terminal in the key-value
//! store. Restoring the terminal (on startup, or from the sidebar) runs that command
//! instead of `agent.terminal_init_command`.
//!
//! In a remote (ssh) terminal the agent runs on the other host, so the agent is read
//! from the terminal title it sets instead (see `remote_resume_from_title`).
//!
//! Closing an agent terminal saves an archived thread-history row with
//! `agent_id == TERMINAL_HISTORY_AGENT_ID` and the terminal id as its session id.
//! Opening that row reopens the terminal and resumes the session.

use std::{
    collections::HashMap,
    fs,
    io::{BufRead as _, BufReader},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use agent_client_protocol::schema::v2 as acp;
use chrono::Utc;
use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Entity, Global};
use project::AgentId;
use serde::{Deserialize, Serialize};
use terminal::Terminal;
use util::ResultExt as _;

use crate::{
    agent_panel::TerminalId,
    terminal_thread_metadata_store::{TerminalThreadMetadata, TerminalThreadMetadataStore},
    thread_metadata_store::{ThreadId, ThreadMetadata, ThreadMetadataStore},
};

/// Agent id of thread-history rows that stand for a closed terminal.
pub const TERMINAL_HISTORY_AGENT_ID: &str = "ryanzed-terminal";

const KEY_PREFIX: &str = "ryanzed_terminal_resume/";
const CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// Slack when matching session files against the time the agent was first seen.
const SESSION_FILE_SLACK: Duration = Duration::from_secs(10);
const AGENTS: &[&str] = &["claude", "codex", "pi"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Record {
    program: String,
    command: String,
    /// The agent was running the last time we looked, so a restore should resume it.
    running: bool,
    working_directory: Option<PathBuf>,
}

struct Live {
    program: Option<String>,
    first_seen: SystemTime,
    last_check: Option<Instant>,
    in_flight: bool,
}

#[derive(Default)]
struct Tracker {
    /// Cache of the key-value store; `None` means "known to have no record".
    records: HashMap<TerminalId, Option<Record>>,
    live: HashMap<TerminalId, Live>,
}

impl Global for Tracker {}

fn key(id: TerminalId) -> String {
    format!("{KEY_PREFIX}{}", id.to_key_string())
}

fn record(id: TerminalId, cx: &mut App) -> Option<Record> {
    if let Some(cached) = cx.default_global::<Tracker>().records.get(&id) {
        return cached.clone();
    }
    let loaded = KeyValueStore::global(cx)
        .read_kvp(&key(id))
        .log_err()
        .flatten()
        .and_then(|json| serde_json::from_str::<Record>(&json).log_err());
    cx.default_global::<Tracker>()
        .records
        .insert(id, loaded.clone());
    loaded
}

fn set_record(id: TerminalId, record: Option<Record>, cx: &mut App) {
    cx.default_global::<Tracker>()
        .records
        .insert(id, record.clone());
    let kvp = KeyValueStore::global(cx);
    cx.background_spawn(async move {
        match record.and_then(|record| serde_json::to_string(&record).log_err()) {
            Some(json) => kvp.write_kvp(key(id), json).await.log_err(),
            None => kvp.delete_kvp(key(id)).await.log_err(),
        };
    })
    .detach();
}

/// The command to run when this terminal is restored, if it was running an agent.
pub(crate) fn resume_command(id: TerminalId, cx: &mut App) -> Option<String> {
    record(id, cx)
        .filter(|record| record.running)
        .map(|record| record.command)
}

/// Called whenever the terminal reports activity. Cheap: does real work at most every
/// `CHECK_INTERVAL` per terminal, and file lookups run in the background.
pub(crate) fn observe(id: TerminalId, terminal: &Entity<Terminal>, cx: &mut App) {
    let (program, pid, cwd, title) = {
        let terminal = terminal.read(cx);
        let Some(program) = terminal.foreground_process_command_name() else {
            return;
        };
        (
            program,
            terminal.pid().map(|pid| pid.as_u32()),
            terminal.working_directory(),
            terminal.breadcrumb_text.clone(),
        )
    };

    let now = Instant::now();
    let tracker = cx.default_global::<Tracker>();
    let live = tracker.live.entry(id).or_insert_with(|| Live {
        program: None,
        first_seen: SystemTime::now(),
        last_check: None,
        in_flight: false,
    });
    if live.program.as_deref() != Some(program.as_str()) {
        live.program = Some(program.clone());
        live.first_seen = SystemTime::now();
        live.last_check = None;
    }
    if live.in_flight
        || live
            .last_check
            .is_some_and(|last| now.duration_since(last) < CHECK_INTERVAL)
    {
        return;
    }
    live.last_check = Some(now);

    // A remote (ssh) terminal: the agent runs on the other host, so its process and
    // session files are out of reach. Go by the terminal title the agent sets instead.
    if program == "ssh" {
        match remote_resume_from_title(&title) {
            RemoteTitle::Agent { program, command } => {
                let updated = Some(Record {
                    program,
                    command,
                    running: true,
                    working_directory: cwd,
                });
                if updated != record(id, cx) {
                    set_record(id, updated, cx);
                }
            }
            RemoteTitle::Shell => {
                if let Some(mut record) = record(id, cx).filter(|record| record.running) {
                    record.running = false;
                    set_record(id, Some(record), cx);
                }
            }
            RemoteTitle::Unknown => {}
        }
        return;
    }

    if !AGENTS.contains(&program.as_str()) {
        if let Some(mut record) = record(id, cx).filter(|record| record.running) {
            record.running = false;
            set_record(id, Some(record), cx);
        }
        return;
    }

    live.in_flight = true;
    let since = live
        .first_seen
        .checked_sub(SESSION_FILE_SLACK)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    cx.spawn(async move |cx| {
        let command = cx
            .background_spawn({
                let program = program.clone();
                let cwd = cwd.clone();
                async move { find_resume_command(&program, pid, cwd.as_deref(), since) }
            })
            .await;
        cx.update(|cx| {
            if let Some(live) = cx.default_global::<Tracker>().live.get_mut(&id) {
                live.in_flight = false;
            }
            let existing = record(id, cx);
            let updated = match command {
                Some(command) => Some(Record {
                    program,
                    command,
                    running: true,
                    working_directory: cwd,
                }),
                // Session file not found (yet), e.g. a resumed codex/pi session that has
                // not written anything since it started: keep what we had.
                None => existing
                    .clone()
                    .filter(|record| record.program == program)
                    .map(|record| Record {
                        running: true,
                        ..record
                    }),
            };
            if updated.is_some() && updated != existing {
                set_record(id, updated, cx);
            }
        });
    })
    .detach();
}

/// Called when a terminal is closed. Agent terminals become thread-history rows.
pub(crate) fn on_terminal_closed(
    id: TerminalId,
    metadata: Option<TerminalThreadMetadata>,
    cx: &mut App,
) {
    cx.default_global::<Tracker>().live.remove(&id);
    let Some(record) = record(id, cx) else {
        return;
    };
    let Some(metadata) = metadata else {
        set_record(id, None, cx);
        return;
    };

    let title = metadata.editable_title();
    let title = if title.trim().is_empty() {
        record.program.into()
    } else {
        title
    };
    let now = Utc::now();
    let thread = ThreadMetadata {
        thread_id: ThreadId::new(),
        session_id: Some(acp::SessionId::new(id.to_key_string())),
        agent_id: AgentId::new(TERMINAL_HISTORY_AGENT_ID),
        title: Some(title),
        title_override: None,
        updated_at: now,
        created_at: Some(metadata.created_at),
        interacted_at: Some(now),
        worktree_paths: metadata.worktree_paths,
        remote_connection: metadata.remote_connection,
        archived: true,
    };
    ThreadMetadataStore::global(cx).update(cx, |store, cx| store.save(thread, cx));
}

pub fn is_terminal_history(agent_id: &AgentId) -> bool {
    agent_id.as_ref() == TERMINAL_HISTORY_AGENT_ID
}

fn history_terminal_id(thread: &ThreadMetadata) -> Option<TerminalId> {
    if !is_terminal_history(&thread.agent_id) {
        return None;
    }
    TerminalId::from_key_string(thread.session_id.as_ref()?.0.as_ref()).log_err()
}

/// Turns a terminal thread-history row back into a terminal entry (it is removed from
/// history and the agent session will be resumed). `None` for any other thread.
pub fn reopen_from_history(
    thread: &ThreadMetadata,
    cx: &mut App,
) -> Option<TerminalThreadMetadata> {
    let id = history_terminal_id(thread)?;
    let record = record(id, cx).map(|record| Record {
        running: true,
        ..record
    });
    let working_directory = record
        .as_ref()
        .and_then(|record| record.working_directory.clone());
    set_record(id, record, cx);

    let thread_id = thread.thread_id;
    ThreadMetadataStore::global(cx).update(cx, |store, cx| store.delete(thread_id, cx));
    let metadata = TerminalThreadMetadata {
        terminal_id: id,
        title: thread.title.clone().unwrap_or_default(),
        custom_title: thread.title_override.clone(),
        created_at: thread.created_at.unwrap_or(thread.updated_at),
        worktree_paths: thread.worktree_paths.clone(),
        remote_connection: thread.remote_connection.clone(),
        working_directory,
    };
    TerminalThreadMetadataStore::global(cx).update(cx, |store, cx| {
        store.save(metadata.clone(), cx);
    });
    Some(metadata)
}

/// Deletes a terminal thread-history row and its resume record. Returns false (and does
/// nothing) for any other thread.
pub(crate) fn delete_history(
    thread_id: ThreadId,
    session_id: Option<&acp::SessionId>,
    agent_id: &AgentId,
    cx: &mut App,
) -> bool {
    if !is_terminal_history(agent_id) {
        return false;
    }
    ThreadMetadataStore::global(cx).update(cx, |store, cx| store.delete(thread_id, cx));
    if let Some(id) =
        session_id.and_then(|session_id| TerminalId::from_key_string(session_id.0.as_ref()).ok())
    {
        set_record(id, None, cx);
    }
    true
}

fn find_resume_command(
    program: &str,
    pid: Option<u32>,
    cwd: Option<&Path>,
    since: SystemTime,
) -> Option<String> {
    match program {
        "claude" => claude_session(pid?).map(|id| format!("claude --resume {id}")),
        "codex" => codex_session(cwd?, since).map(|id| format!("codex resume {id}")),
        "pi" => pi_session(cwd?, since)
            .map(|path| format!("pi --session {}", shell_quote(&path.to_string_lossy()))),
        _ => None,
    }
}

#[derive(Debug, PartialEq)]
enum RemoteTitle {
    Agent {
        program: String,
        command: String,
    },
    /// The default bash prompt title, `user@host: dir`: no agent running.
    Shell,
    Unknown,
}

/// Reads which agent a remote terminal runs from the title the agent sets.
/// - codex: `<thread name> | <dir>`, resumed by thread name.
/// - claude: `✳ <topic>` (or a braille spinner while working); the title carries no
///   session id, so it resumes the latest session in the directory.
fn remote_resume_from_title(title: &str) -> RemoteTitle {
    let title = title.trim();
    let first = title.chars().next();
    let status_glyph = first.is_some_and(|c| c == '✳' || ('\u{2800}'..='\u{28ff}').contains(&c));
    let rest = if status_glyph {
        title[first.map_or(0, char::len_utf8)..].trim_start()
    } else {
        title
    };
    if let Some((name, _dir)) = rest.rsplit_once(" | ")
        && !name.trim().is_empty()
    {
        return RemoteTitle::Agent {
            program: "codex".into(),
            command: format!("codex resume {}", shell_quote(name.trim())),
        };
    }
    if status_glyph {
        return RemoteTitle::Agent {
            program: "claude".into(),
            command: "claude --continue".into(),
        };
    }
    let is_prompt = title.split_once(": ").is_some_and(|(user_host, _)| {
        user_host.contains('@') && !user_host.contains(char::is_whitespace)
    });
    if is_prompt {
        RemoteTitle::Shell
    } else {
        RemoteTitle::Unknown
    }
}

fn dir_from_env(var: &str, default: &[&str]) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| {
        default
            .iter()
            .fold(util::paths::home_dir().clone(), |dir, part| dir.join(part))
    })
}

fn is_session_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Claude Code writes `~/.claude/sessions/<pid>.json` with the live session id.
fn claude_session(pid: u32) -> Option<String> {
    let path = dir_from_env("CLAUDE_CONFIG_DIR", &[".claude"])
        .join("sessions")
        .join(format!("{pid}.json"));
    let json: serde_json::Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let id = json.get("sessionId")?.as_str()?;
    is_session_id(id).then(|| id.to_string())
}

/// Codex writes `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`; the first line is
/// `session_meta` with the session `id` and `cwd`. Takes the newest one for `cwd`.
fn codex_session(cwd: &Path, since: SystemTime) -> Option<String> {
    let sessions = dir_from_env("CODEX_HOME", &[".codex"]).join("sessions");
    let mut files = Vec::new();
    let mut day_dirs = 0;
    'days: for year in sorted_dirs_desc(&sessions) {
        for month in sorted_dirs_desc(&year) {
            for day in sorted_dirs_desc(&month) {
                files.extend(recent_files(&day, "rollout-", since));
                day_dirs += 1;
                if day_dirs >= 31 {
                    break 'days;
                }
            }
        }
    }
    files.sort_by_key(|file| std::cmp::Reverse(file.0));
    files.into_iter().find_map(|(_, path)| {
        let mut line = String::new();
        BufReader::new(fs::File::open(path).ok()?)
            .read_line(&mut line)
            .ok()?;
        let json: serde_json::Value = serde_json::from_str(&line).ok()?;
        let payload = json.get("payload")?;
        if Path::new(payload.get("cwd")?.as_str()?) != cwd {
            return None;
        }
        let id = payload.get("id")?.as_str()?;
        is_session_id(id).then(|| id.to_string())
    })
}

/// pi writes `~/.pi/agent/sessions/--<cwd with / as ->--/<timestamp>_<id>.jsonl`.
fn pi_session(cwd: &Path, since: SystemTime) -> Option<PathBuf> {
    let name = format!(
        "--{}--",
        cwd.to_string_lossy()
            .trim_start_matches('/')
            .replace('/', "-")
    );
    let dir = dir_from_env("PI_CODING_AGENT_DIR", &[".pi", "agent"])
        .join("sessions")
        .join(name);
    recent_files(&dir, "", since)
        .into_iter()
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path)
}

fn sorted_dirs_desc(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort_by(|a, b| b.cmp(a));
    dirs
}

/// `*.jsonl` files in `dir` whose name starts with `prefix`, modified at or after `since`.
fn recent_files(dir: &Path, prefix: &str, since: SystemTime) -> Vec<(SystemTime, PathBuf)> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !name.starts_with(prefix) || !name.ends_with(".jsonl") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            (modified >= since).then(|| (modified, entry.path()))
        })
        .collect()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_quote() {
        assert_eq!(shell_quote("/a b/c.jsonl"), "'/a b/c.jsonl'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn test_remote_resume_from_title() {
        let agent = |program: &str, command: &str| RemoteTitle::Agent {
            program: program.into(),
            command: command.into(),
        };
        assert_eq!(
            remote_resume_from_title("AubergeRP | ryross"),
            agent("codex", "codex resume 'AubergeRP'")
        );
        assert_eq!(
            remote_resume_from_title("⠋ Confirm living room guardian | ryross"),
            agent("codex", "codex resume 'Confirm living room guardian'")
        );
        assert_eq!(
            remote_resume_from_title("✳ zed-fork-setup"),
            agent("claude", "claude --continue")
        );
        assert_eq!(
            remote_resume_from_title("ryross@flux: ~/thoughtd"),
            RemoteTitle::Shell
        );
        assert_eq!(remote_resume_from_title(""), RemoteTitle::Unknown);
        assert_eq!(
            remote_resume_from_title("vim notes.md"),
            RemoteTitle::Unknown
        );
    }

    #[test]
    fn test_session_files() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = Path::new("/work/proj");

        let pi_dir = dir.path().join("sessions").join("--work-proj--");
        fs::create_dir_all(&pi_dir).unwrap();
        fs::write(pi_dir.join("2026-01-01_old.jsonl"), "{}").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        fs::write(pi_dir.join("2026-01-02_new.jsonl"), "{}").unwrap();
        // SAFETY: test-only, single-threaded use of these variables.
        unsafe { std::env::set_var("PI_CODING_AGENT_DIR", dir.path()) };
        assert_eq!(
            pi_session(cwd, SystemTime::UNIX_EPOCH),
            Some(pi_dir.join("2026-01-02_new.jsonl"))
        );
        assert_eq!(
            pi_session(cwd, SystemTime::now() + Duration::from_secs(60)),
            None
        );

        let day = dir.path().join("codex/sessions/2026/01/02");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-a.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"abc-1","cwd":"/work/proj"}}"#,
        )
        .unwrap();
        fs::write(
            day.join("rollout-b.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"abc-2","cwd":"/elsewhere"}}"#,
        )
        .unwrap();
        unsafe { std::env::set_var("CODEX_HOME", dir.path().join("codex")) };
        assert_eq!(
            codex_session(cwd, SystemTime::UNIX_EPOCH).as_deref(),
            Some("abc-1")
        );

        let claude = dir.path().join("claude/sessions");
        fs::create_dir_all(&claude).unwrap();
        fs::write(
            claude.join("42.json"),
            r#"{"pid":42,"sessionId":"f7ce-288d"}"#,
        )
        .unwrap();
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", dir.path().join("claude")) };
        assert_eq!(claude_session(42).as_deref(), Some("f7ce-288d"));
        assert_eq!(claude_session(43), None);
    }
}
