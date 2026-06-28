#!/usr/bin/env bash
#
# scripts.sh — everything needed to build, deploy, and manage benny-rs.
#
# Usage:  ./scripts.sh <command>
#         ./scripts.sh help     # list all commands
#
# Production runs on the VPS reachable via `ssh benny`:
#   ~/benny-rs   git clone of main; config.json lives here (untracked), run from this dir
#   ~/lavalink   Lavalink.jar (v4.2.2) + application.yml (127.0.0.1:2333)
# Two tmux sessions: `lavalink` (start FIRST) then `benny` (the bot).
# tmux does NOT survive a reboot — after the box restarts, run `./scripts.sh start`.
#
set -euo pipefail

# ---- config -----------------------------------------------------------------
SSH_HOST="benny"            # ssh alias for the VPS
BOT_DIR="~/benny-rs"        # bot checkout on the box (config.json lives here)
LAVA_DIR="~/lavalink"       # Lavalink dir on the box
BOT_TMUX="benny"            # tmux session running the bot
LAVA_TMUX="lavalink"        # tmux session running Lavalink
BIN="benny-rs"             # release binary name (target/release/$BIN)

# The box's gcc 9.4 miscompiles aws-lc-sys (gcc bug 95189) — MUST build with clang.
BUILD_ENV="CC=clang CXX=clang++"

remote() { ssh "$SSH_HOST" "$@"; }

# =============================================================================
# Local (run on your Mac)
# =============================================================================
cmd_build()   { cargo build; }                 # dev compile
cmd_release() { cargo build --release; }        # optimized compile
cmd_check()   { cargo check; }                  # fast type-check, no linking
cmd_clippy()  { cargo clippy; }                 # lint
cmd_run()     { cargo run; }                    # run locally (uses config.dev_token in debug)

# Copy the local config.json (secrets) up to the box. config.json is gitignored,
# so this is the only way it gets there.
cmd_push-config() {
  scp config.json "$SSH_HOST:$BOT_DIR/config.json"
  echo "config.json pushed. Run './scripts.sh restart' to pick it up."
}

# =============================================================================
# Remote (drive the VPS over ssh)
# =============================================================================

# Open an interactive shell on the box.
cmd_ssh() { ssh "$SSH_HOST"; }

# Pull latest main, rebuild (clang), and restart the bot tmux session.
cmd_deploy() {
  remote "cd $BOT_DIR && git pull --ff-only && $BUILD_ENV cargo build --release"
  cmd_restart
  echo "Deployed."
}

# --- start / stop ------------------------------------------------------------

# Start Lavalink first, then the bot (the bot needs Lavalink up to play music).
cmd_start() {
  cmd_start-lava
  sleep 3
  cmd_start-bot
  cmd_status
}

cmd_start-lava() {
  remote "tmux has-session -t $LAVA_TMUX 2>/dev/null && echo 'lavalink already running' \
    || tmux new-session -d -s $LAVA_TMUX 'cd $LAVA_DIR && java -jar Lavalink.jar'"
}

cmd_start-bot() {
  remote "tmux has-session -t $BOT_TMUX 2>/dev/null && echo 'bot already running' \
    || tmux new-session -d -s $BOT_TMUX 'cd $BOT_DIR && ./target/release/$BIN'"
}

# Kill both tmux sessions (bot, then lavalink).
cmd_stop() {
  remote "tmux kill-session -t $BOT_TMUX 2>/dev/null || true; \
          tmux kill-session -t $LAVA_TMUX 2>/dev/null || true"
  echo "Stopped bot + lavalink."
}

# Restart only the bot (leaves Lavalink running).
cmd_restart() {
  remote "tmux kill-session -t $BOT_TMUX 2>/dev/null || true"
  cmd_start-bot
  echo "Bot restarted."
}

# --- observe -----------------------------------------------------------------

# Attach to the bot's tmux session (Ctrl-b then d to detach without killing it).
cmd_logs() { ssh -t "$SSH_HOST" "tmux attach -t $BOT_TMUX"; }

# Attach to the Lavalink tmux session.
cmd_logs-lava() { ssh -t "$SSH_HOST" "tmux attach -t $LAVA_TMUX"; }

# Tail the bot's log file without attaching to tmux (Ctrl-c to stop).
cmd_tail() { ssh -t "$SSH_HOST" "tail -f $BOT_DIR/logs/benny.log"; }

# Show which tmux sessions are alive.
cmd_status() { remote "tmux ls 2>/dev/null || echo 'no tmux sessions'"; }

# Hit the bot's local HTTP health endpoint on the box.
cmd_health() { remote "curl -s 127.0.0.1:8080/health || echo 'no response'"; echo; }

# =============================================================================
# dispatch
# =============================================================================
cmd_help() {
  cat <<'EOF'
benny-rs management scripts — ./scripts.sh <command>

Local (Mac):
  build         cargo build (dev)
  release       cargo build --release
  check         cargo check (fast type-check)
  clippy        cargo clippy (lint)
  run           cargo run locally (uses dev_token)
  push-config   scp config.json up to the box

Remote (VPS via ssh benny):
  ssh           open a shell on the box
  deploy        git pull + clang release build + restart bot
  start         start lavalink, then the bot (use after a reboot)
  start-lava    start only the lavalink tmux session
  start-bot     start only the bot tmux session
  stop          kill both tmux sessions
  restart       restart just the bot (lavalink stays up)
  logs          attach to the bot tmux  (Ctrl-b d to detach)
  logs-lava     attach to the lavalink tmux
  tail          tail logs/benny.log without attaching
  status        list running tmux sessions
  health        curl the bot's /health endpoint
EOF
}

main() {
  local sub="${1:-help}"
  if declare -f "cmd_$sub" >/dev/null; then
    shift
    "cmd_$sub" "$@"
  else
    echo "unknown command: $sub" >&2
    echo >&2
    cmd_help >&2
    exit 1
  fi
}

main "$@"
