#!/bin/sh

# Refresh the desktop/icon caches so the .desktop file shows up in the
# launcher without re-login. Silently skip if the tools are absent.
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q usr/share/applications >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -t -f usr/share/icons/hicolor >/dev/null 2>&1 || true
fi

cat <<'EOF'

  Fono installed.

  To launch on login for the current user:
    systemctl --user daemon-reload
    systemctl --user enable --now fono.service

  Or just run `fono` once for the first-run wizard.

  Config:  ~/.config/fono/config.toml
  Models:  ~/.cache/fono/models/
  History: ~/.local/share/fono/history.sqlite

EOF
