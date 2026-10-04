#!/usr/bin/env python3
"""Install the suite's desktop launcher and icon for the current Linux user."""

import argparse
import os
from pathlib import Path
import shutil
import subprocess


def main():
    root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", type=Path,
                        default=root / "suite/target/release/suite")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"not an executable: {binary}")

    data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
    app_id = "io.github.yeroo.docxy"
    icons = data / "icons"
    applications = data / "applications"
    icons.mkdir(parents=True, exist_ok=True)
    applications.mkdir(parents=True, exist_ok=True)
    icon = icons / f"{app_id}.png"
    shutil.copy2(root / "packaging/macos/docxy-1024.png", icon)

    # Desktop Exec fields have their own quoting rules, including literal %.
    executable = str(binary).replace("%", "%%")
    for character in ('\\', '"', '`', '$'):
        executable = executable.replace(character, '\\' + character)
    executable = executable.replace('\\', '\\\\')
    desktop = applications / f"{app_id}.desktop"
    desktop.write_text(
        "[Desktop Entry]\n"
        "Type=Application\n"
        "Name=Docxy\n"
        "Comment=Open and edit documents, spreadsheets and projects\n"
        f'Exec="{executable}" %F\n'
        f"Icon={icon}\n"
        f"StartupWMClass={app_id}\n"
        "Terminal=false\n"
        "Categories=Office;WordProcessor;Spreadsheet;\n",
        encoding="utf-8",
    )
    for command in (("gtk-update-icon-cache", "-f", "-t", str(data / "icons/hicolor")),
                    ("update-desktop-database", str(applications))):
        if shutil.which(command[0]):
            subprocess.run(command, check=False)
    print(f"Installed {desktop}")


if __name__ == "__main__":
    main()
