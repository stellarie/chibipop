# extras

Session-integration snippets for setups the settings window's Autostart
checkbox does not cover. The checkbox writes an XDG autostart entry
(`~/.config/autostart/chibipop.desktop`), which GNOME, KDE, and
uwsm-managed sessions all honour — prefer it where it works, and pick
**one** mechanism, not several.

## chibipop.desktop

An application-launcher entry, so chibipop shows up in app grids and
launchers (`Exec=chibipop run`). Install to `~/.local/share/applications/`
(or `/usr/share/applications/` when packaging):

    cp chibipop.desktop ~/.local/share/applications/

## chibipop.service

A systemd **user** unit for bare compositors without XDG autostart, tied
to `graphical-session.target` so the daemon lives and dies with the
session:

    cp chibipop.service ~/.config/systemd/user/
    systemctl --user enable --now chibipop.service

Adjust `ExecStart` if the binary is not at `/usr/bin/chibipop`.

## hyprland.conf

`exec-once` plus example native trigger binds for bare Hyprland. Copy it
to `~/.config/hypr/chibipop.conf` and include it from your main config:

    source = ~/.config/hypr/chibipop.conf

The bind lines mirror the default `ALT+F` chord; the settings window
shows the exact snippet for whatever chord you configure.
