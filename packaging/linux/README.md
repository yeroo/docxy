# Linux desktop integration

Build the desktop suite, then install its launcher and existing Docxy icon:

```sh
cargo build --manifest-path suite/Cargo.toml --release --bin suite
python3 packaging/linux/install.py
```

The installer writes only to the current user's XDG data directory (normally
`~/.local/share`). It points the launcher at the build executable; pass a path
as its argument to use an executable installed elsewhere. Re-run it after
moving that executable. The desktop entry matches the suite's Wayland app ID
and X11 window class, so GNOME can display the icon and group its windows.
