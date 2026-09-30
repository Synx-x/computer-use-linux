#!/usr/bin/env python3
"""Random-site control for agent evals, to catch overfitting to fixtures.

Each run index draws one public, read-only site from SITES with a seed, so
both arms of an eval visit the same site on the same run. The task: from the
home page, use the site's own links to reach a page two clicks deep, then
report FINAL_URL and TITLE. The checker fetches the reported URL and compares
its <title> with the reported one, and a transcript check rejects any typed
or goto navigation past the start page.

    control_sites.py pick SEED            print the site for a run
    control_sites.py check SEED ANSWER_FILE TRANSCRIPT [--typed-marker TEXT]
"""
import difflib, html, json, random, re, sys, urllib.request
from urllib.parse import urlparse

SITES = ["https://www.python.org", "https://www.gnu.org", "https://www.rust-lang.org", "https://www.w3.org",
         "https://archlinux.org", "https://www.kernel.org", "https://www.debian.org", "https://www.apache.org",
         "https://nodejs.org", "https://go.dev", "https://www.postgresql.org", "https://www.sqlite.org",
         "https://www.gutenberg.org", "https://www.nasa.gov", "https://www.usa.gov", "https://www.gov.uk",
         "https://www.openstreetmap.org", "https://news.ycombinator.com", "https://lobste.rs",
         "https://www.mozilla.org", "https://www.ietf.org", "https://www.iana.org", "https://www.eff.org",
         "https://creativecommons.org", "https://www.theguardian.com", "https://www.gnome.org",
         "https://kde.org", "https://www.freebsd.org", "https://www.ruby-lang.org", "https://www.php.net"]


def pick(seed: str) -> str:
    return random.Random(f"control-{seed}").choice(SITES)


def domain(url: str) -> str:
    host = urlparse(url).netloc.lower().split(":")[0]
    return host[4:] if host.startswith("www.") else host


def fetch_title(url: str) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (X11; Linux x86_64) Firefox/140.0"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        raw = resp.read(2000000)
    # python.org sends gzip even when the request does not ask for it.
    if raw[:2] == b"\x1f\x8b":
        import gzip
        raw = gzip.decompress(raw)
    body = raw.decode("utf-8", "replace")
    found = re.search(r"<title[^>]*>(.*?)</title>", body, re.S | re.I)
    return html.unescape(re.sub(r"\s+", " ", found.group(1))).strip() if found else ""


def norm(text: str) -> str:
    return re.sub(r"\s+", " ", re.sub(r"[^\w\s]", " ", text.lower())).strip()


def check(seed: str, answer: str, transcript_cmds: list[str]) -> dict:
    site = pick(seed)
    url = re.search(r"FINAL_URL:\s*(\S+)", answer)
    title = re.search(r"TITLE:\s*(.+)", answer)
    result = {"site": site, "url": url.group(1) if url else None, "title": title.group(1).strip() if title else None}
    if not url or not title:
        return {**result, "ok": False, "reason": "no FINAL_URL or TITLE in the answer"}
    final = url.group(1).rstrip(").,")
    if not domain(final).endswith(domain(site)):
        return {**result, "ok": False, "reason": "final page is on another site"}
    if urlparse(final).path.strip("/") == urlparse(site).path.strip("/") and not urlparse(final).query:
        return {**result, "ok": False, "reason": "final page is the home page"}
    typed = [c for c in transcript_cmds if final.split("#")[0].rstrip("/") in c]
    if typed:
        return {**result, "ok": False, "reason": "the final URL was typed or passed to goto"}
    try:
        live = fetch_title(final)
    except Exception as exc:
        return {**result, "ok": None, "reason": f"could not fetch the page to check: {exc}"}
    score = difflib.SequenceMatcher(a=norm(live), b=norm(title.group(1))).ratio()
    ok = score >= 0.8 or (norm(title.group(1)) and norm(title.group(1)) in norm(live))
    return {**result, "live_title": live, "score": round(score, 2), "ok": bool(ok),
            "reason": "" if ok else "reported title does not match the page"}


if __name__ == "__main__":
    if sys.argv[1] == "pick":
        print(pick(sys.argv[2]))
    else:
        seed, answer_file, cmds_file = sys.argv[2:5]
        print(json.dumps(check(seed, open(answer_file).read(), json.load(open(cmds_file)))))
