#!/bin/sh
# Install the mirrord Agent Skills for OpenCode, or any agent that reads a
# skills directory (Codex, Devin, Cursor, Gemini CLI, ...).
#
#   curl -fsSL https://raw.githubusercontent.com/metalbear-co/skills/main/install.sh | sh
#   sh install.sh --agent codex
#   sh install.sh --dest ~/somewhere/skills
#
# Re-running replaces previously installed mirrord-* skills and leaves any
# other skill in the destination untouched.

set -eu

REPO_URL="https://github.com/metalbear-co/skills.git"
AGENT="opencode"
DEST=""

usage() {
	cat <<'EOF'
Usage: install.sh [--agent <name>] [--dest <dir>]

  --agent opencode   ~/.config/opencode/skills   (default; honours XDG_CONFIG_HOME)
  --agent codex      ~/.agents/skills
  --agent devin      ~/.agents/skills
  --agent claude     ~/.claude/skills
  --dest <dir>       install into <dir> instead of an agent default
EOF
}

while [ $# -gt 0 ]; do
	case "$1" in
	--agent)
		[ $# -ge 2 ] || { echo "install.sh: --agent needs a value" >&2; exit 2; }
		AGENT="$2"
		shift 2
		;;
	--dest)
		[ $# -ge 2 ] || { echo "install.sh: --dest needs a value" >&2; exit 2; }
		DEST="$2"
		shift 2
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "install.sh: unknown argument '$1'" >&2
		usage >&2
		exit 2
		;;
	esac
done

if [ -z "$DEST" ]; then
	case "$AGENT" in
	opencode) DEST="${XDG_CONFIG_HOME:-$HOME/.config}/opencode/skills" ;;
	codex | devin) DEST="$HOME/.agents/skills" ;;
	claude) DEST="$HOME/.claude/skills" ;;
	*)
		echo "install.sh: unknown agent '$AGENT' (expected opencode, codex, devin, or claude)" >&2
		exit 2
		;;
	esac
fi

# Use the checkout we're running from when there is one, otherwise fetch a copy.
# Piped through `curl | sh`, $0 is "sh" and SCRIPT_DIR lands on the caller's cwd,
# so require markers that only a checkout of this repo has before trusting it.
SRC=""
CLONE=""
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd 2>/dev/null) || SCRIPT_DIR=""
if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/install.sh" ] && [ -d "$SCRIPT_DIR/skills/mirrord-config" ]; then
	SRC="$SCRIPT_DIR/skills"
else
	command -v git >/dev/null 2>&1 || {
		echo "install.sh: git is required to fetch the skills" >&2
		exit 1
	}
	CLONE=$(mktemp -d)
	trap 'rm -rf "$CLONE"' EXIT INT TERM
	git clone --depth 1 --quiet "$REPO_URL" "$CLONE"
	SRC="$CLONE/skills"
fi

mkdir -p "$DEST"

count=0
for skill in "$SRC"/*/; do
	[ -f "$skill/SKILL.md" ] || continue
	name=$(basename "$skill")
	rm -rf "$DEST/$name"
	cp -R "$skill" "$DEST/$name"
	echo "  installed $name"
	count=$((count + 1))
done

if [ "$count" -eq 0 ]; then
	echo "install.sh: no skills found in $SRC" >&2
	exit 1
fi

echo ""
echo "$count mirrord skills installed into $DEST"
echo "Restart $AGENT (or your agent) to pick them up."
