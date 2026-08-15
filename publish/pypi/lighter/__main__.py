from __future__ import annotations

import os
import subprocess
import sys

from lighter import find_lighter_bin


def main() -> None:
    """Execute the lighter binary passing along CLI arguments."""
    lighter_bin = find_lighter_bin()
    args = [lighter_bin, *sys.argv[1:]]

    if os.name == "posix":
        os.execv(lighter_bin, args)
    else:
        result = subprocess.run(args)
        sys.exit(result.returncode)


if __name__ == "__main__":
    main()
