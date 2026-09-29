#!/usr/bin/env python3
"""Run a fixed, loopback-only model sample through Buzz Agent ACP."""

from __future__ import annotations

import sys
import types
from pathlib import Path

PACKAGE_ROOT = Path(__file__).resolve().parents[1]
PACKAGE_SOURCE = PACKAGE_ROOT / "src" / "harbor_buzz_orchestra"
sys.path.insert(0, str(PACKAGE_ROOT / "src"))
# This command uses only stdlib runner modules. The Harbor package initializer
# eagerly imports Harbor itself, so provide its source path as a namespace
# package and avoid loading that unrelated runtime dependency.
package = types.ModuleType("harbor_buzz_orchestra")
package.__path__ = [str(PACKAGE_SOURCE)]
package.__package__ = "harbor_buzz_orchestra"
sys.modules.setdefault("harbor_buzz_orchestra", package)

from harbor_buzz_orchestra.local_route_smoke import main

if __name__ == "__main__":
    raise SystemExit(main())
