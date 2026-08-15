"""Python wrapper for the lighter CLI tool."""

from __future__ import annotations

import os
import shutil
import sys
import sysconfig
from pathlib import Path

__all__ = ["find_lighter_bin"]


def _get_executable_name() -> str:
    """Return the platform-specific executable name for lighter."""
    exe_suffix = sysconfig.get_config_var("EXE")
    if exe_suffix is None:
        exe_suffix = ".exe" if os.name == "nt" else ""
    return f"lighter{exe_suffix}"


def find_lighter_bin() -> str:
    """Locate the lighter executable.

    Checks:
    1. sysconfig 'scripts' scheme path
    2. User scripts path
    3. sys.prefix and sys.base_prefix bin/Scripts directories
    4. System PATH via shutil.which

    Returns:
        The absolute path to the lighter binary.

    Raises:
        FileNotFoundError: If the lighter binary cannot be found.
    """
    exe_name = _get_executable_name()

    # 1. Check sysconfig scripts directory
    scripts_dir = sysconfig.get_path("scripts")
    if scripts_dir:
        candidate = Path(scripts_dir) / exe_name
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)

    # 2. Check user scripts directory
    try:
        user_scheme = f"{os.name}_user"
        if user_scheme in sysconfig.get_scheme_names():
            user_scripts = sysconfig.get_path("scripts", user_scheme)
            if user_scripts:
                candidate = Path(user_scripts) / exe_name
                if candidate.is_file() and os.access(candidate, os.X_OK):
                    return str(candidate)
    except (KeyError, AttributeError):
        pass

    # 3. Check sys.prefix and sys.base_prefix bin/Scripts directories
    for prefix in (sys.prefix, sys.base_prefix):
        for bin_dir in ("bin", "Scripts"):
            candidate = Path(prefix) / bin_dir / exe_name
            if candidate.is_file() and os.access(candidate, os.X_OK):
                return str(candidate)

    # 4. Check PATH fallback
    which_path = shutil.which(exe_name) or shutil.which("lighter")
    if which_path is not None:
        return which_path

    raise FileNotFoundError(
        f"Unable to find the '{exe_name}' binary. "
        "Please ensure 'alecghost-lighter' is properly installed."
    )
