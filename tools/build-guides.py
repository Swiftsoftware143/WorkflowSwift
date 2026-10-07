#!/usr/bin/env python3
"""Convert WorkflowSwift's TWO markdown guides to the fleet house HTML.

House shell is copied from apps/FunnelSwift/www/guide.html (dark indigo sidebar +
content-area family, the shape every fleet guide now wears) so the guides look like
one family: same head/CSS/inline-nav pattern, same sidebar-footer anchor.

ONE SOURCE per document: both documents are authored as HTML in the app repo
(www-app/guide.html, www-admin/admin-guide.html) and installed into the served roots by
scripts/publish-workflowswift-frontend.sh.  guide.html is installed into TWO roots
(app.workflowswift.com and workflowswift.com), which is why it is declared in
fleet/guide-generation-parity.py as ONE source with TWO destinations.

Refuses to write a stub: asserts every markdown H2 became a nav entry and that the
rendered body carries a table whenever the markdown did.
"""
import html
import os
import re

import markdown

TPL = "/opt/swift/apps/FunnelSwift/www/guide.html"

JOBS = [
    dict(
        md="/opt/swift/apps/WorkflowSwift/docs/user-guide.md",
        out="/opt/swift/apps/WorkflowSwift/www-app/guide.html",
        title="WorkflowSwift — User Guide",
        logo_sub="User Guide v1.0",
        badge="v1.0 &middot; Workflows, Steps &amp; Integrations",
        footer="&copy; 2026 WorkflowSwift &mdash; Workflow Automation Platform",
        icon="⚡",
    ),
    dict(
        md="/opt/swift/apps/WorkflowSwift/docs/admin-guide.md",
        out="/opt/swift/apps/WorkflowSwift/www-admin/admin-guide.html",
        title="WorkflowSwift — Admin Guide",
        logo_sub="Admin Guide v1.0",
        badge="v1.0 &middot; Tenants, Plans &amp; Operators",
        footer="&copy; 2026 WorkflowSwift &mdash; Admin Console",
        icon="🛠️",
    ),
]

# the exact template strings replaced, so a template change fails loudly instead of
# silently shipping a FunnelSwift-branded page under a WorkflowSwift URL
HEAD_SUBS = [
    ("<title>FunnelSwift User Guide</title>", "title_tag"),
    ("<h1>FunnelSwift</h1>", "appname"),
    ("<p>User Guide v3.0</p>", "logo_sub"),
    ('<div class="logo-icon">🔁</div>', "icon"),
]
MID_SUBS = [
    ("&copy; 2026 FunnelSwift &mdash; Lead Generation Platform", "footer"),
    ("<h2>FunnelSwift User Guide</h2>", "h2_title"),
    ('<span class="topbar-badge">v3.0 &middot; Lead Gen, Kinetic Cards &amp; Affiliates</span>',
     "badge"),
]
TAIL_MARKER = "\n    </div>\n  </div>\n</div>\n<script>"


def slug(txt: str) -> str:
    t = html.unescape(re.sub(r"<[^>]+>", "", txt)).lower()
    return re.sub(r"[^a-z0-9]+", "-", t).strip("-")


def indent(body: str) -> str:
    """indent the rendered body to sit inside .content-area like the house guides do"""
    return "\n".join(("      " + ln) if ln.strip() else ln for ln in body.splitlines())


def convert(md_text: str):
    """markdown -> html body, with an id on every h1..h4; returns (body, h2s)."""
    body = markdown.markdown(md_text, extensions=["tables", "sane_lists"])
    heads = []

    def repl(m):
        lvl, inner = m.group(1), m.group(2)
        sid = slug(inner)
        if lvl == "2":
            heads.append((sid, html.escape(html.unescape(re.sub(r"<[^>]+>", "", inner)))))
        return f'<h{lvl} id="{sid}">{inner}</h{lvl}>'

    body = re.sub(r"<h([1234])>(.*?)</h\1>", repl, body, flags=re.S)
    return body.strip(), heads


def build(tpl: str, job: dict) -> str:
    md_text = open(job["md"], encoding="utf-8").read()
    lines = md_text.splitlines()
    i = 0
    while i < len(lines) and not lines[i].strip():
        i += 1
    assert lines[i].startswith("# "), f"{job['md']}: first line is not an H1: {lines[i]!r}"
    dropped_h1 = lines.pop(i)[2:].strip()

    body, heads = convert("\n".join(lines))
    assert heads, f"{job['md']}: no H2 headings rendered"
    if "\n|" in md_text:
        assert "<table>" in body, f"{job['md']}: markdown had pipe tables, none rendered"

    # the first rendered paragraph is the page's lead (the house shell styles .lead)
    lead_body = body.replace("<p>", '<p class="lead">', 1)

    nav = ['<nav class="sidebar-nav">', '      <div class="section-title">Contents</div>']
    for sid, text in heads:
        nav.append(f'      <a href="#{sid}" class="nav-item" onclick="closeMobile()">'
                   f'<span class="nav-icon">📄</span> {text}</a>')
    nav.append("    </nav>")
    nav = "\n".join(nav)

    i_nav = tpl.index('<nav class="sidebar-nav">')
    i_nav_end = tpl.index("</nav>") + len("</nav>")
    i_content = tpl.index('<div class="content-area">') + len('<div class="content-area">')
    i_tail = tpl.index(TAIL_MARKER, i_content)

    head, mid, tail = tpl[:i_nav], tpl[i_nav_end:i_content], tpl[i_tail:]

    want = dict(job)
    want["appname"] = "<h1>WorkflowSwift</h1>"
    want["title_tag"] = f'<title>{job["title"]}</title>'
    want["h2_title"] = f'<h2>{job["title"]}</h2>'
    want["logo_sub"] = f'<p>{job["logo_sub"]}</p>'
    want["icon"] = f'<div class="logo-icon">{job["icon"]}</div>'
    want["badge"] = f'<span class="topbar-badge">{job["badge"]}</span>'
    for needle, key in HEAD_SUBS:
        assert needle in head, f"template head no longer holds {needle!r}"
        head = head.replace(needle, want[key], 1)
    for needle, key in MID_SUBS:
        assert needle in mid, f"template mid no longer holds {needle!r}"
        mid = mid.replace(needle, want[key], 1)

    content = f'\n      <h1>{job["title"]}</h1>\n{indent(lead_body)}\n'
    out = head + nav + mid + content + tail

    # the app guide legitimately mentions FunnelSwift (affiliate auto-sync), so test the
    # template's own brand strings, not the word
    for leaked in ("FunnelSwift User Guide", "2026 FunnelSwift", ">FunnelSwift</h1>",
                   '<div class="logo-icon">🔁</div>'):
        assert leaked not in out, f"template brand leaked into the built page: {leaked!r}"
    assert f"<title>{job['title']}</title>" in out
    assert out.count("<h2 id=") == len(heads)
    assert len(out) > 8000, f"refusing to ship a stub ({len(out)} B)"
    return out, dropped_h1


def main():
    tpl = open(TPL, encoding="utf-8").read()
    for job in JOBS:
        page, h1 = build(tpl, job)
        os.makedirs(os.path.dirname(job["out"]), exist_ok=True)
        with open(job["out"], "w", encoding="utf-8") as fh:
            fh.write(page)
        print(f"{job['out']}  {len(page)} B  src-h1={h1!r}")


if __name__ == "__main__":
    main()
