"""ConPTY の入力パイプに文字列を書いたとき、ccnest (crossterm) がどんなキーとして受けるかを実測する。

Windows Terminal の `sendInput "\\u001b\\r"` (Shift+Enter → 改行の定番設定) と同じ経路を、
自前の ConPTY (pywinpty) で再現する。ccnest は `CCNEST_INPUT_TRACE=1` で起動し、受けた
イベントを `%APPDATA%\\ccnest\\input-trace.log` に書くので、その追記分を表示する。

実行: uv run --with pywinpty python scripts/e2e/conpty-input-probe.py [path\\to\\ccnest.exe]
ゆうさんの ccnest・Windows Terminal には触らない (使い捨てプロセスだけ)。
"""

from __future__ import annotations

import os
import sys
import threading
import time
from pathlib import Path

from winpty import PtyProcess  # type: ignore

ROOT = Path(__file__).resolve().parents[2]
EXE = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "release" / "ccnest.exe"
TRACE = Path(os.environ["APPDATA"]) / "ccnest" / "input-trace.log"


def clean_env() -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if not (
            (k.startswith("CLAUDE") and k != "CLAUDE_CONFIG_DIR")
            or k.startswith("AI_AGENT")
            or k.startswith("CCNEST")
        )
    }
    env["CCNEST_INPUT_TRACE"] = "1"
    env["CCNEST_JP_TITLE"] = "off"
    return env


def main() -> int:
    before = TRACE.stat().st_size if TRACE.exists() else 0
    proc = PtyProcess.spawn(
        [str(EXE)], cwd=str(ROOT), env=clean_env(), dimensions=(30, 120)
    )
    out: list[str] = []

    def pump() -> None:
        try:
            while proc.isalive():
                out.append(proc.read(65536))
        except Exception:  # EOF
            pass

    threading.Thread(target=pump, daemon=True).start()
    try:
        time.sleep(4)  # raw mode + input pump が立つまで
        experiments = [
            ("A: ESC CR を 1 回の write で (WT の sendInput と同じ)", ["\x1b\r"]),
            ("B: ESC と CR を別 write (100ms 空ける)", ["\x1b", "\r"]),
            ("C: 素の CR", ["\r"]),
        ]
        marks: list[tuple[str, float]] = []
        for label, chunks in experiments:
            marks.append((label, time.time()))
            for c in chunks:
                proc.write(c)
                time.sleep(0.1)
            time.sleep(1.0)
    finally:
        try:
            proc.terminate(force=True)
        except Exception:
            pass
    time.sleep(0.5)
    text = TRACE.read_text(encoding="utf-8", errors="replace")[before:] if TRACE.exists() else ""
    lines = [l for l in text.splitlines() if "Key(" in l or "Paste(" in l]
    print(f"ccnest: {EXE}\ntrace: {TRACE} (+{len(text)} bytes)\n")
    for label, _ in marks:
        print(label)
    print("\n--- key events ccnest received (crossterm) ---")
    for l in lines:
        print("  " + l)
    if not lines:
        print("  (none — ccnest が入力を受け取れていない。起動待ちを延ばす)")
    return 0 if lines else 1


if __name__ == "__main__":
    sys.exit(main())
