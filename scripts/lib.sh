#!/usr/bin/env bash
# Shared helpers for Commonspace's local developer/release scripts.
#
# This file is meant to be *sourced*, not executed:
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   source "${SCRIPT_DIR}/lib.sh"
#
# It intentionally avoids bashisms newer than what ships in Git Bash on
# Windows and the default /bin/bash on macOS, so every script in this
# directory works unmodified on Windows, macOS, and Linux.

set -euo pipefail

# --- repo root detection ----------------------------------------------------
# Resolve the directory this file lives in, then treat its parent as the
# repo root and cd into it. This makes every script behave the same no
# matter what directory it was invoked from.
_commonspace_lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${_commonspace_lib_dir}/.." && pwd)"
cd "${REPO_ROOT}"

# --- output helpers ----------------------------------------------------------
# Deliberately no color codes: color escapes are unreliable across Windows
# terminals, CI-less log files, and piped output, and they add nothing a
# clear text prefix doesn't already give you.

# step_header <name> -- a clearly delimited section header.
step_header() {
  printf '\n==> %s\n' "$1"
}

# info <message> -- a normal, indented status line.
info() {
  printf '    %s\n' "$1"
}

# warn <message> -- a non-fatal warning, printed to stderr.
warn() {
  printf 'WARNING: %s\n' "$1" >&2
}

# fail <message> -- print a loud failure message and exit non-zero.
fail() {
  printf '\nFAILED: %s\n' "$1" >&2
  exit 1
}

# run_step "description" cmd [args...]
# Prints a header, runs the command, and fails loudly (naming both the step
# and the exact command that failed) if it exits non-zero.
run_step() {
  local name="$1"
  shift
  step_header "$name"
  if ! "$@"; then
    fail "${name} (command: $*)"
  fi
}

# have <cmd> -- true if a command exists on PATH.
have() {
  command -v "$1" >/dev/null 2>&1
}

# has_frontend -- true only once the desktop app has been scaffolded.
has_frontend() {
  [ -f "${REPO_ROOT}/apps/desktop/package.json" ]
}

# has_tauri -- true only once the Tauri shell has been scaffolded.
has_tauri() {
  [ -f "${REPO_ROOT}/apps/desktop/src-tauri/tauri.conf.json" ]
}

# os_name -- prints one of: linux, macos, windows, unknown.
os_name() {
  case "$(uname -s)" in
    Linux*) printf 'linux\n' ;;
    Darwin*) printf 'macos\n' ;;
    MINGW* | MSYS* | CYGWIN*) printf 'windows\n' ;;
    *) printf 'unknown\n' ;;
  esac
}

# _pkg_has_script <package.json path> <script name>
# Internal helper: true if the given package.json defines that npm script.
# Uses node (already a hard requirement whenever a frontend exists) to parse
# JSON properly, rather than scraping `npm run` text output, whose format
# varies across npm versions and locales.
_pkg_has_script() {
  node -e '
    var fs = require("fs");
    var pkg;
    try {
      pkg = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    } catch (e) {
      process.exit(1);
    }
    process.exit(pkg.scripts && pkg.scripts[process.argv[2]] ? 0 : 1);
  ' "$1" "$2" >/dev/null 2>&1
}

# npm_script_owner <script-name>
# Echoes "root" if the repo-root package.json defines the script, "desktop"
# if apps/desktop/package.json defines it (checked in that order), or
# nothing (with a non-zero exit) if neither does yet.
npm_script_owner() {
  local script_name="$1"
  if [ -f "${REPO_ROOT}/package.json" ] && _pkg_has_script "${REPO_ROOT}/package.json" "${script_name}"; then
    printf 'root\n'
    return 0
  fi
  if [ -f "${REPO_ROOT}/apps/desktop/package.json" ] && _pkg_has_script "${REPO_ROOT}/apps/desktop/package.json" "${script_name}"; then
    printf 'desktop\n'
    return 0
  fi
  return 1
}

# npm_script_dir <owner> -- maps "root"/"desktop" (as returned by
# npm_script_owner) to the absolute directory that owns that package.json.
npm_script_dir() {
  case "$1" in
    root) printf '%s\n' "${REPO_ROOT}" ;;
    desktop) printf '%s\n' "${REPO_ROOT}/apps/desktop" ;;
    *) fail "npm_script_dir: unknown owner '$1'" ;;
  esac
}
