#!/usr/bin/env bash
# RyanZed CI/CD: check, publish, build and install the fork. Runs on macOS or Linux.
#
#   ryanzed/ci.sh                       check -> push+tag -> build+install every target
#   ryanzed/ci.sh --sync                first rebase `patches` onto the newest upstream stable tag
#   ryanzed/ci.sh --only mac|home       one target
#   ryanzed/ci.sh --skip-tests          skip cargo test for touched crates
#   ryanzed/ci.sh --skip-checks         skip fmt/check/tests (commit already checked)
#   ryanzed/ci.sh --rollback mac|home   swap the previous build back in
#
# Targets:
#   mac   this Mac, /Applications/RyanZed.app. Only when run on macOS.
#   home  the Linux desktop. Built in place when run on Linux; from macOS, built over
#         `ssh home` in $REMOTE_REPO as a detached job.
set -euo pipefail

# Run from a temp copy: --sync rebases this very file under a running bash.
if [[ -z "${RYANZED_CI_COPY:-}" ]]; then
  repo="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
  copy="$(mktemp)"; cp "$0" "$copy"
  RYANZED_CI_COPY=1 RYANZED_REPO="${RYANZED_REPO:-$repo}" exec bash "$copy" "$@"
fi

REPO="$RYANZED_REPO"
OS="$(uname -s)"
REMOTE_REPO=/mnt/data/build/zed      # build checkout on home (its / is too small)
REMOTE_LOGS=/mnt/data/build/logs
MIN_FREE_GB=60
TEST_TIMEOUT=15m           # whole cargo test step
HOME_BUILD_TIMEOUT_MIN=90  # home build from the Mac
# Stable local code-signing identity: an ad-hoc signature changes every build, and
# macOS then treats each build as a new app and asks for keychain access again.
MAC_SIGN_IDENTITY="${RYANZED_SIGN_IDENTITY:-Meeting Recorder Local}"
LOG_DIR="${RYANZED_LOG_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/ryanzed/logs}"

export PATH="$HOME/.cargo/bin:$PATH"
if [[ "$OS" == Darwin ]]; then
  # xcode-select may point at CommandLineTools, which lacks the Metal compiler.
  export DEVELOPER_DIR="${DEVELOPER_DIR:-$(ls -d /Applications/Xcode*.app 2>/dev/null | sort -V | tail -1)/Contents/Developer}"
fi

SYNC=false; REBASED=false; ONLY=""; SKIP_TESTS=false; SKIP_CHECKS=false; ROLLBACK=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --sync) SYNC=true ;;
    --only) ONLY="$2"; shift ;;
    --skip-tests) SKIP_TESTS=true ;;
    --skip-checks) SKIP_CHECKS=true ;;
    --rollback) ROLLBACK="$2"; shift ;;
    -h|--help) sed -n 2,14p "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

die() { echo "FAIL: $*" >&2; exit 1; }
[[ -z "$ONLY" || "$ONLY" == mac || "$ONLY" == home ]] || die "--only mac|home"
[[ "$ONLY" != mac || "$OS" == Darwin ]] || die "the mac target needs macOS"
want() {
  [[ -n "$ONLY" && "$ONLY" != "$1" ]] && return 1
  [[ "$1" == mac && "$OS" != Darwin ]] && return 1
  return 0
}
# Run a command on home: in place on Linux, over ssh from the Mac.
on_home() { if [[ "$OS" == Linux ]]; then bash -c "$1"; else ssh home "$1"; fi; }

mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/$(date +%Y%m%d-%H%M%S).log"
exec > >(tee -a "$LOG") 2>&1
stage() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

# ---------------------------------------------------------------- rollback
if [[ -n "$ROLLBACK" ]]; then
  case "$ROLLBACK" in
    mac)
      [[ "$OS" == Darwin ]] || die "the mac target needs macOS"
      [[ -d /Applications/RyanZed.prev.app ]] || die "no /Applications/RyanZed.prev.app"
      rm -rf /Applications/RyanZed.app && mv /Applications/RyanZed.prev.app /Applications/RyanZed.app ;;
    home)
      on_home 'set -e; test -d ~/.local/ryanzed.prev.app; rm -rf ~/.local/ryanzed.app; mv ~/.local/ryanzed.prev.app ~/.local/ryanzed.app' ;;
    *) die "--rollback mac|home" ;;
  esac
  echo "rolled back $ROLLBACK"; exit 0
fi

# ---------------------------------------------------------------- 0. preflight
stage "0 preflight"
cd "$REPO"
[[ "$(git branch --show-current)" == patches ]] || die "$REPO is not on 'patches'"
[[ -z "$(git status --porcelain)" ]] || die "$REPO has uncommitted changes"
git fetch -q origin --tags
# The upstream stable tag `patches` sits on: nearest vX.Y.Z below HEAD.
BASE="$(git describe --tags --abbrev=0 --match 'v[0-9]*' --exclude '*-*' HEAD)"
if want mac; then
  [[ -x "$DEVELOPER_DIR/usr/bin/xcodebuild" ]] || die "no Xcode at DEVELOPER_DIR=$DEVELOPER_DIR"
fi
if want home; then
  if [[ "$OS" == Darwin ]]; then
    ssh -O check home 2>/dev/null || die "no ssh master to home; run: ! ssh home"
    [[ -z "$(ssh home "git -C $REMOTE_REPO status --porcelain")" ]] || die "home:$REMOTE_REPO has uncommitted changes"
    free_dir="$REMOTE_REPO"
  else
    free_dir="$REPO"
  fi
  free_gb=$(on_home "df -BG --output=avail $free_dir | tail -1 | tr -dc 0-9")
  (( free_gb >= MIN_FREE_GB )) || die "home has ${free_gb}G free at $free_dir (< ${MIN_FREE_GB}G)"
fi
echo "os=$OS base=$BASE head=$(git rev-parse --short HEAD) log=$LOG"

# ---------------------------------------------------------------- 1. sync
if $SYNC; then
  stage "1 sync with upstream"
  git fetch -q upstream --tags
  latest="$(git tag -l 'v[0-9]*' | grep -vE -- '-' | sort -V | tail -1)"
  if [[ "$latest" == "$BASE" ]]; then
    echo "already on newest stable tag $BASE"
  else
    echo "rebasing patches: $BASE -> $latest"
    git rebase --onto "$latest" "$BASE" patches \
      || die "rebase conflict. Resolve, 'git rebase --continue', then re-run ci.sh"
    BASE="$latest"
    REBASED=true
  fi
fi

# ---------------------------------------------------------------- 2. check
stage "2 check"
if $SKIP_CHECKS; then echo "skipped"; else
  cargo fmt --all -- --check
  cargo check -q -p zed
  if ! $SKIP_TESTS; then
    # Test only what changed since the last deploy that is still in HEAD's history. After a
    # --sync rebase (or with no such deploy) that is everything the fork touches since BASE.
    since="$BASE"
    if ! $REBASED; then
      for t in $(git tag -l "ryanzed/$BASE-*" | sort -V -r); do
        if git merge-base --is-ancestor "$t" HEAD; then since="$t"; break; fi
      done
    fi
    pkgs=()
    for c in $(git diff --name-only "$since"..HEAD | sed -nE 's#^crates/([^/]+)/.*#\1#p' | sort -u); do
      [[ -f "crates/$c/Cargo.toml" ]] || continue
      pkgs+=(-p "$(sed -nE 's/^name *= *"([^"]+)".*/\1/p' "crates/$c/Cargo.toml" | head -1)")
    done
    if (( ${#pkgs[@]} == 0 )); then
      echo "no crates changed since $since; no tests"
    else
      # One cargo invocation builds the test binaries together. The timeout catches a hung
      # test (upstream tests can wait forever on behaviour the fork removed).
      echo "-- cargo test ${pkgs[*]} (changes since $since)"
      tmo=(); command -v timeout >/dev/null && tmo=(timeout "$TEST_TIMEOUT")
      ${tmo[@]+"${tmo[@]}"} cargo test -q "${pkgs[@]}" \
        || die "tests failed or hit the ${TEST_TIMEOUT} timeout (see the log for the last test running)"
    fi
  fi
fi

# ---------------------------------------------------------------- 3. publish
stage "3 publish"
SHA="$(git rev-parse HEAD)"
TAG="$(git tag --points-at "$SHA" -l 'ryanzed/*' | head -1)"
if [[ -z "$TAG" ]]; then
  n=$(( $(git tag -l "ryanzed/$BASE-*" | wc -l) + 1 ))
  TAG="ryanzed/$BASE-$n"
  git tag "$TAG" "$SHA"
fi
git push -q --force-with-lease origin patches
git push -q origin "$TAG"
echo "published $TAG ($SHA)"

# ---------------------------------------------------------------- 4. build
stage "4 build ($TAG)"
if want home && [[ "$OS" == Darwin ]]; then
  # Detached so a dropped ssh doesn't kill a long build; the Mac polls the status file.
  ssh home "set -e; mkdir -p $REMOTE_LOGS; rm -f $REMOTE_LOGS/$SHA.status
    cd $REMOTE_REPO && git fetch -q origin --tags && git checkout -q --detach $SHA
    setsid nohup bash -c 'ryanzed/install-linux.sh > $REMOTE_LOGS/$SHA.log 2>&1; echo \$? > $REMOTE_LOGS/$SHA.status' >/dev/null 2>&1 < /dev/null &"
  echo "home build started: home:$REMOTE_LOGS/$SHA.log"
fi

if want home && [[ "$OS" == Linux ]]; then
  ryanzed/install-linux.sh
fi

if want mac; then
  if [[ -d /Applications/RyanZed.app ]]; then
    rm -rf /Applications/RyanZed.prev.app; ditto /Applications/RyanZed.app /Applications/RyanZed.prev.app
  fi
  marker="$(mktemp)"
  # bundle-mac -i installs the app, then (upstream bug) tries to DMG the moved bundle and fails.
  # shellcheck source=build-env.sh
  source ryanzed/build-env.sh
  script/bundle-mac -i || [[ /Applications/RyanZed.app/Contents/MacOS/zed -nt "$marker" ]] \
    || die "bundle-mac failed before installing"
  rm -f "$marker"
  if security find-identity -v -p codesigning | grep -qF "\"$MAC_SIGN_IDENTITY\""; then
    codesign --force --deep --preserve-metadata=entitlements -s "$MAC_SIGN_IDENTITY" /Applications/RyanZed.app \
      || die "re-signing with $MAC_SIGN_IDENTITY failed"
  else
    echo "note: no '$MAC_SIGN_IDENTITY' signing identity; keeping the ad-hoc signature (keychain will re-prompt)"
  fi
  ln -sf /Applications/RyanZed.app/Contents/MacOS/cli /opt/homebrew/bin/ryanzed
fi

if want home && [[ "$OS" == Darwin ]]; then
  echo "waiting for home build..."
  deadline=$(( $(date +%s) + HOME_BUILD_TIMEOUT_MIN * 60 ))
  until ssh home "test -f $REMOTE_LOGS/$SHA.status"; do
    (( $(date +%s) < deadline )) || die "home build still running after ${HOME_BUILD_TIMEOUT_MIN} min, log: home:$REMOTE_LOGS/$SHA.log"
    # [r] keeps pgrep from matching the remote shell running this very command.
    ssh home "pgrep -f '[r]yanzed/install-linux.sh' >/dev/null || test -f $REMOTE_LOGS/$SHA.status" \
      || die "home build exited without a status, log: home:$REMOTE_LOGS/$SHA.log"
    sleep 30
  done
  status="$(ssh home "cat $REMOTE_LOGS/$SHA.status")"
  if [[ "$status" != 0 ]]; then
    ssh home "tail -40 $REMOTE_LOGS/$SHA.log"
    die "home build failed (exit $status), log: home:$REMOTE_LOGS/$SHA.log"
  fi
fi

# ---------------------------------------------------------------- 5. verify
stage "5 verify"
short="${SHA:0:10}"
if want mac; then
  v="$(/Applications/RyanZed.app/Contents/MacOS/cli --version)"; echo "mac:  $v"
  codesign -dv /Applications/RyanZed.app 2>&1 | grep -E '^(Identifier|Signature)'
fi
if want home; then
  v="$(on_home '~/.local/bin/ryanzed --version')"; echo "home: $v"
  grep -q "$short" <<<"$v" || echo "WARN: home --version does not show $short"
fi
echo "deployed $TAG"
