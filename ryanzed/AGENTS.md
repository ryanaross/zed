# RyanZed: fork notes for agents and humans

RyanZed is Ryan Ross's personal fork of Zed, [ryanaross/zed](https://github.com/ryanaross/zed). It adds personal features and installs **next to** official Zed. Everything needed to build, deploy and maintain it is in this `ryanzed/` directory. Upstream never touches this directory, so it can't conflict on a rebase. It must be possible to run the fork from a single Linux machine, with no Mac involved.

## Branch model
- `patches` (GitHub default) is upstream's newest stable tag (`vX.Y.Z`) plus a short linear stack of `fork:` commits. `git log --oneline $(git describe --tags --abbrev=0 --match 'v[0-9]*' --exclude '*-*')..patches` lists the stack.
- Upgrade by **rebasing** onto the newer tag (`ryanzed/ci.sh --sync`). Never merge upstream. Turn `git config rerere.enabled true` on in every clone, so a conflict resolved once is replayed automatically.
- Each deploy is tagged `ryanzed/<upstream-tag>-<n>`. `main` only mirrors upstream.
- Remotes: `origin` is `git@github.com:ryanaross/zed.git`; `upstream` is `https://github.com/zed-industries/zed.git`.

## The fork stack (keep each commit small, it is replayed every release)
| Commit | Hook points in upstream files |
|---|---|
| `fork: rebrand as RyanZed` | `crates/release_channel/src/lib.rs`: Stable `display_name` "RyanZed", `app_id` `dev.ryanaross.RyanZed`, `poll_for_updates()` false (no auto-update). `crates/db/src/db.rs`: stable DB scope `ryanzed-stable`. `crates/zed/src/zed/open_listener.rs`: Linux socket `ryanzed-<channel>.sock`. `crates/zed/Cargo.toml` `[package.metadata.bundle-stable]`: identifier and name. `script/bundle-linux`: stable `APP_NAME`/`APP_ID`. |
| `fork: separate macOS single-instance port` | `crates/zed/src/zed/mac_only_instance.rs`: stable port `43737 + 5*USER_BLOCK` (official Zed uses `2*`) and its own handshake string. Without it, RyanZed exits with "zed is already running" while Zed is open. |
| `fork: RyanZed icon` | `crates/zed/resources/app-icon-ryanzed{,@2x}.png` (made by `ryanzed/make-icon.sh`), `bundle-stable` `icon`, and `icon_suffix` in `script/bundle-linux`. |
| `fork: ryanzed tooling` | This directory, plus a pointer appended to `.rules` (which `AGENTS.md`, `CLAUDE.md` and `GEMINI.md` symlink to). |

Look for `// fork:` / `# fork:` comments to find every hook. When a rebase conflicts, keep upstream's change and re-apply the fork's one-line intent.

### What is shared with official Zed and what is separate
| | Official Zed | RyanZed |
|---|---|---|
| App ID / bundle | `dev.zed.Zed` | `dev.ryanaross.RyanZed` |
| State DB (workspaces, sessions) | `<data>/db/0-stable` | `<data>/db/0-ryanzed-stable` |
| Single instance | `zed-stable.sock` (Linux), port 43937+uid (Mac) | `ryanzed-stable.sock`, port 44237+uid |
| Settings, keymap, themes, extensions, prompts, threads | shared: `~/.config/zed` and `<data>` | shared |
| `zed://` URL scheme | registered | shared on Mac; not registered on Linux |

`<data>` is `~/.local/share/zed` on Linux and `~/Library/Application Support/Zed` on the Mac. To start RyanZed from official Zed's state, quit RyanZed, then take a SQLite backup of `0-stable/db.sqlite` into `0-ryanzed-stable/db.sqlite`. This is safe while Zed runs. Without `sqlite3`, use Python's `sqlite3.Connection.backup`.

## Build and deploy: `ryanzed/ci.sh`
```
ryanzed/ci.sh                    # fmt + check + tests for touched crates -> push + tag -> build + install -> verify
ryanzed/ci.sh --sync             # first rebase onto the newest upstream stable tag
ryanzed/ci.sh --only home|mac
ryanzed/ci.sh --skip-tests | --skip-checks
ryanzed/ci.sh --rollback home|mac
```
- It requires a clean checkout on `patches`. It runs from a temp copy of itself, so `--sync` can rebase it safely. Logs go to `~/.local/state/ryanzed/logs/`.
- **On Linux** the `home` target builds in place with `ryanzed/install-linux.sh`. That script runs `script/bundle-linux`, then installs to `~/.local/ryanzed.app` (keeping the old build as `ryanzed.prev.app`), writes the `~/.local/bin/ryanzed` launcher, and writes the `dev.ryanaross.RyanZed.desktop` entry.
- **On macOS** both targets run:
  - `mac` uses `script/bundle-mac -i` to install `/Applications/RyanZed.app` (ad-hoc signed) and links the `ryanzed` CLI. `DEVELOPER_DIR` must point at full Xcode for the Metal compiler.
  - `home` is built over `ssh home`, in `/mnt/data/build/zed`, as a detached job that the Mac polls.
- `bundle-mac -i` exits non-zero after a successful install, because it tries to make a DMG of the bundle it just moved. ci.sh accepts that if the installed binary is new.

## Linux-only operation (no Mac)
The `home` desktop is `ryross-desktop`: Linux Mint 22.3, x86_64. `/` is small, so the repo and `target/` live on `/mnt/data/build/zed`, which needs 60 GB or more free. To deploy from it directly:
```
cd /mnt/data/build/zed
git checkout patches && git pull --ff-only    # a Mac deploy leaves it on a detached SHA
ryanzed/ci.sh                                 # mac target is skipped automatically on Linux
```
Bootstrapping a fresh Linux machine:
```
git clone git@github.com:ryanaross/zed.git && cd zed      # lands on patches
git remote add upstream https://github.com/zed-industries/zed.git && git fetch upstream --tags
git config rerere.enabled true
git config user.name ryanaross && git config user.email 7442411+ryanaross@users.noreply.github.com
curl https://sh.rustup.rs -sSf | sh -s -- -y              # rust-toolchain.toml pins the version
script/linux                                               # system build deps (sudo)
ryanzed/ci.sh --only home
```
The launcher sets `ZED_FILE_WATCHER_MODE=native`, which fixes file watching on the SMB mounts; official Zed uses `~/.local/bin/zed-safe` for the same thing.

## Adding a feature
- Features are listed in `ryanzed/todo.md`.
- Prefer an extension, a setting or a keymap change. Fork code only for what those can't do.
- One `fork: <feature>` commit per feature. Put new code in new files or modules, and keep the hooks into upstream files thin and marked `// fork:`. Put new behaviour behind a setting, so it stays dormant by default.
- Follow upstream's Rust guidelines in `.rules`.
- Deploy only through `ryanzed/ci.sh`.
