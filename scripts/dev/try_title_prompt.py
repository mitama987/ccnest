"""日本語タブ名のプロンプトを、ccnest と同じ呼び方で試す (開発用)。

src/claude/jp_title.rs の SYSTEM_PROMPT を読み、`claude -p --model haiku` (設定・
CLAUDE.md・MCP・ツール無し) にサンプルの話題を 1 件ずつ渡して、返った名前と表示幅
(セル) を出す。プロンプトを変えるときに、変える前と後で見比べる。

    python -X utf8 scripts/dev/try_title_prompt.py            # ソースの SYSTEM_PROMPT
    python -X utf8 scripts/dev/try_title_prompt.py prompt.txt  # 別の文面を試す

1 件あたり入力 800 トークン弱の Haiku 呼び出し (プランの利用量をごく僅かに使う)。
タブは「N:」+ 名前を 16 セルで切るので、名前は 12〜13 セルに収まるのが目安。
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
import unicodedata
from pathlib import Path

SOURCE = Path(__file__).resolve().parents[2] / "src" / "claude" / "jp_title.rs"

# (会話の話題 = ai-title, 最後の依頼)。内容の無い依頼 (続き・進めて・まだだめ) を
# 混ぜてあるのは、名前がそれに引っぱられないかを見るため。
SAMPLES: list[tuple[str, str | None]] = [
    ("resume-p1-affilia-features", "続きから再開して"),
    ("resume-p1-affilia-features", None),
    ("namegear-dns-change", "ネームギアのDNSを変更して"),
    ("move-business-payments-to-gmo", "決済の移行をGMOに"),
    ("whitening-consultation-prep", "ホワイトニングの相談に行くので質問をまとめて"),
    ("verify-v097-deploy", "v097のデプロイを確認して"),
    ("fix-login-redirect-loop", "まだだめです"),
    ("ccnest-background-agent-blue-marker", "続き"),
    ("xtp6-lp-pricing-revision", "OK、進めて"),
    ("continue-work", "続きから再開"),
    ("add-stripe-payment-methods", "Stripeに決済手段を足したい"),
    ("instagram-account-link-debug", "インスタのアカウント紐づけができない"),
]


def source_prompt() -> str:
    """jp_title.rs の `const SYSTEM_PROMPT: &str = "…";` を Rust の規則で読む。"""
    text = SOURCE.read_text(encoding="utf-8")
    m = re.search(r'const SYSTEM_PROMPT: &str =\s*"((?:[^"\\]|\\.)*)";', text, re.S)
    if not m:
        raise SystemExit(f"SYSTEM_PROMPT not found in {SOURCE}")
    # 行末の `\` は、改行と次の行の先頭の空白を飛ばす。
    return re.sub(r"\\\r?\n\s*", "", m.group(1))


def cells(s: str) -> int:
    return sum(2 if unicodedata.east_asian_width(c) in "WFA" else 1 for c in s)


def should_strip_env(key: str) -> bool:
    k = key.upper()
    return k != "CLAUDE_CONFIG_DIR" and (k.startswith("CLAUDE") or k.startswith("AI_AGENT"))


def main() -> None:
    prompt = (
        Path(sys.argv[1]).read_text(encoding="utf-8").strip()
        if len(sys.argv) > 1
        else source_prompt()
    )
    env = {k: v for k, v in os.environ.items() if not should_strip_env(k)}
    workdir = tempfile.mkdtemp(prefix="ccnest-title-try-")
    for ai_title, last_user in SAMPLES:
        text = f"会話の話題: {ai_title}\n"
        if last_user:
            text += f"最後の依頼: {last_user}\n"
        r = subprocess.run(
            [
                "claude", "-p", "--model", "haiku", "--effort", "low",
                "--no-session-persistence", "--strict-mcp-config", "--tools", "",
                "--disable-slash-commands", "--setting-sources", "",
                "--system-prompt", prompt,
            ],
            input=text.encode("utf-8"),
            capture_output=True,
            env=env,
            cwd=workdir,
            timeout=60,
            check=False,
        )
        lines = [ln.strip() for ln in r.stdout.decode("utf-8", "replace").splitlines() if ln.strip()]
        title = lines[0] if lines else f"(none rc={r.returncode})"
        print(f"{cells(title):>2} cells  {title}    <= {ai_title} / {last_user}")


if __name__ == "__main__":
    main()
