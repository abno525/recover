#!/usr/bin/env sh
# Install recover: the binary (via cargo), the man page, and the `r` shell prefix.
set -e

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

# 1. Require the Rust toolchain.
if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust (cargo) is not installed."
    echo
    echo "Install it with rustup, then re-run this script:"
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    echo "  (or see https://rustup.rs)"
    echo
    echo 'After installing, restart your shell or run:  . "$HOME/.cargo/env"'
    exit 1
fi

# 2. Build and install the binary onto the PATH (no manual copying).
echo "==> Installing the recover binary with cargo"
cargo install --path "$SCRIPT_DIR" --force

# 3. Install the man page.
if [ -f "$SCRIPT_DIR/man/recover.1" ]; then
    MAN_DIR="$HOME/.local/share/man/man1"
    mkdir -p "$MAN_DIR"
    cp "$SCRIPT_DIR/man/recover.1" "$MAN_DIR/"
    echo "==> Installed man page to $MAN_DIR/recover.1  (man recover)"
fi

# 4. Add the `r` shell prefix.
R_LINE='r() { recover run "$@"; }'
RC=""
case "${SHELL:-}" in
    *zsh) RC="$HOME/.zshrc" ;;
    *bash) RC="$HOME/.bashrc" ;;
esac
if [ -n "$RC" ]; then
    if [ -f "$RC" ] && grep -qF 'recover run "$@"' "$RC"; then
        echo "==> The 'r' function is already in $RC"
    else
        printf '\n# recover: run and record a command\n%s\n' "$R_LINE" >> "$RC"
        echo "==> Added the 'r' function to $RC  (reload with: . $RC)"
    fi
else
    echo "==> Add this line to your shell startup file to enable the 'r' prefix:"
    echo "    $R_LINE"
fi

# 5. PATH reminder.
case ":$PATH:" in
    *":$HOME/.cargo/bin:"*) : ;;
    *) echo "==> Note: add \$HOME/.cargo/bin to your PATH to run recover." ;;
esac

echo
echo "Done. Try:  r echo hello   then   recover"
