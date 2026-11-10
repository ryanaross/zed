# Sourced by ci.sh (Mac) and install-linux.sh (home) before bundling.
# RyanZed is a personal build: trade a little runtime speed for much faster deploys.

# RyanZed downloads the official remote server for its version, so ours is never used.
export RYANZED_SKIP_REMOTE_SERVER=1

# Upstream's release profile is tuned for shipping (codegen-units = 1, thin LTO, debuginfo),
# which makes every rebuilt crate single-threaded and the final link slow.
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_DEBUG=false
export CARGO_PROFILE_RELEASE_INCREMENTAL=true
