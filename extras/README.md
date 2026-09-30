# extras

Session-integration snippets for setups the settings window's Autostart
checkbox does not cover. The checkbox writes an XDG autostart entry
(`~/.config/autostart/chibipop.desktop`), which GNOME, KDE, and
uwsm-managed sessions all honour — prefer it where it works, and pick
**one** mechanism, not several.

Full Linux setup — trigger key, per-compositor support, troubleshooting —
is in [`docs/LINUX.md`](../docs/LINUX.md).

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

`exec-once` starts the daemon for bare Hyprland sessions.
Configure lookup and action Binds in **Settings > Shortcuts**.
Copy the generated command for each stable Bind ID.

Copy the file to `~/.config/hypr/chibipop.conf` and include it from your
main config:

    source = ~/.config/hypr/chibipop.conf

For Hold mode, add both commands from the settings window:

    bind = ALT, F, exec, chibipop ctl bind-down <id>
    bindr = ALT, F, exec, chibipop ctl bind-up <id>

For Press, Toggle, and one-shot actions, add only `bind-down`.
The fixed verbs remain supported for compatibility.
