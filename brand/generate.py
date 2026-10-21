"""Regenerates the payments-ledger logo masters and README banners in this folder.

Run: python3 brand/generate.py
The mark: an equals sign whose lower bar is split in two and spans the same width as the entry bar above.
The postings always sum to the entry, which is the ledger's core rule. The wordmark is Poppins SemiBold, outlined by
brand/outline_wordmark.py (Poppins is SIL Open Font License 1.1, see POPPINS-OFL.txt), so no font is needed.
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))
INK, PAPER, BRASS, BRONZE, SMOKE = "#16181C", "#F4F1EA", "#D4A84B", "#A07A2C", "#9A958A"

# The mark in a 256 box: entry bar on top, its two postings below, stroke 40 with round ends.
ENTRY, POSTING_LEFT, POSTING_RIGHT = "M40,96 H216", "M40,160 H96", "M160,160 H216"
MARK_WEIGHT = 40
MARK_INK = (20, 76, 236, 180)  # x0, y0, x1, y1 of the bars including their round ends

# "ledger" in Poppins SemiBold, font units, y down, baseline at 0.
WORD = "M209 -740V0H69V-740ZM848 -235H443Q448 -175 485 -141Q522 -107 576 -107Q654 -107 687 -174H838Q814 -94 746 -42.5Q678 9 579 9Q499 9 435.5 -26.5Q372 -62 336.5 -127Q301 -192 301 -277Q301 -363 336 -428Q371 -493 434 -528Q497 -563 579 -563Q658 -563 720.5 -529Q783 -495 817.5 -432.5Q852 -370 852 -289Q852 -259 848 -235ZM707 -329Q706 -383 668 -415.5Q630 -448 575 -448Q523 -448 487.5 -416.5Q452 -385 444 -329ZM1162 -563Q1216 -563 1265 -539.5Q1314 -516 1343 -477V-740H1485V0H1343V-82Q1317 -41 1270 -16Q1223 9 1161 9Q1091 9 1033 -27Q975 -63 941.5 -128.5Q908 -194 908 -279Q908 -363 941.5 -428Q975 -493 1033 -528Q1091 -563 1162 -563ZM1197 -440Q1158 -440 1125 -421Q1092 -402 1071.5 -365.5Q1051 -329 1051 -279Q1051 -229 1071.5 -191.5Q1092 -154 1125.5 -134Q1159 -114 1197 -114Q1236 -114 1270 -133.5Q1304 -153 1324 -189.5Q1344 -226 1344 -277Q1344 -328 1324 -364.5Q1304 -401 1270 -420.5Q1236 -440 1197 -440ZM2012 -475V-554H2153V4Q2153 81 2122 141.5Q2091 202 2029 237.5Q1967 273 1879 273Q1761 273 1685.5 218Q1610 163 1600 68H1739Q1750 106 1786.5 128.5Q1823 151 1875 151Q1936 151 1974 114.5Q2012 78 2012 4V-82Q1985 -43 1937.5 -17Q1890 9 1829 9Q1759 9 1701 -27Q1643 -63 1609.5 -128.5Q1576 -194 1576 -279Q1576 -363 1609.5 -428Q1643 -493 1700.5 -528Q1758 -563 1829 -563Q1891 -563 1938 -538.5Q1985 -514 2012 -475ZM1865 -440Q1826 -440 1793 -421Q1760 -402 1739.5 -365.5Q1719 -329 1719 -279Q1719 -229 1739.5 -191.5Q1760 -154 1793.5 -134Q1827 -114 1865 -114Q1904 -114 1938 -133.5Q1972 -153 1992 -189.5Q2012 -226 2012 -277Q2012 -328 1992 -364.5Q1972 -401 1938 -420.5Q1904 -440 1865 -440ZM2791 -235H2386Q2391 -175 2428 -141Q2465 -107 2519 -107Q2597 -107 2630 -174H2781Q2757 -94 2689 -42.5Q2621 9 2522 9Q2442 9 2378.5 -26.5Q2315 -62 2279.5 -127Q2244 -192 2244 -277Q2244 -363 2279 -428Q2314 -493 2377 -528Q2440 -563 2522 -563Q2601 -563 2663.5 -529Q2726 -495 2760.5 -432.5Q2795 -370 2795 -289Q2795 -259 2791 -235ZM2650 -329Q2649 -383 2611 -415.5Q2573 -448 2518 -448Q2466 -448 2430.5 -416.5Q2395 -385 2387 -329ZM3197 -562V-415H3160Q3094 -415 3060.5 -384Q3027 -353 3027 -276V0H2887V-554H3027V-468Q3054 -512 3097.5 -537Q3141 -562 3197 -562Z"
WORD_BOX = (69, -740, 3197, 273)  # x0, y0, x1, y1
X_HEIGHT = 554


def mark(color, extra=("", "", "")):
    return "".join(f'<path{e} d="{d}" fill="none" stroke="{color}" stroke-width="{MARK_WEIGHT}" stroke-linecap="round"/>'
                   for d, e in zip((ENTRY, POSTING_LEFT, POSTING_RIGHT), extra))


def lockup(mark_color, word_color, height=200, extra=("", "", "")):
    """Mark beside the word, the mark's bars matching the letters' x-height.
    Returns (mark_svg, word_svg, width, word_left)."""
    bars = MARK_INK[3] - MARK_INK[1]
    x_px = height * 0.42                    # x-height in px
    sm, sw = x_px / bars, x_px / X_HEIGHT
    word_ascent = -WORD_BOX[1] * sw          # ascender above the baseline
    baseline = (height - (word_ascent + WORD_BOX[3] * sw)) / 2 + word_ascent
    mx = -MARK_INK[0] * sm
    my = baseline - MARK_INK[3] * sm
    gap = x_px * 0.45
    wx = (MARK_INK[2] - MARK_INK[0]) * sm + gap - WORD_BOX[0] * sw
    width = wx + WORD_BOX[2] * sw
    mark_svg = f'<g transform="translate({mx:.2f},{my:.2f}) scale({sm:.4f})">{mark(mark_color, extra)}</g>'
    word_svg = f'<g transform="translate({wx:.2f},{baseline:.2f}) scale({sw:.5f})"><path d="{WORD}" fill="{word_color}"/></g>'
    return mark_svg, word_svg, width, wx + WORD_BOX[0] * sw


def write(name, body, w, h):
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w:.0f} {h:.0f}" width="{w:.0f}" height="{h:.0f}">'
           f'<title>payments-ledger</title>{body}</svg>\n')
    with open(os.path.join(HERE, name), "w", encoding="utf-8") as f:
        f.write(svg)


def banner(animated):
    """1280 x 640 README header and social preview. Animated: the entry bar draws, its two postings
    grow in from the outer edges to the same total length, the word wipes in, the line fades in.
    Everything is visible by default; hidden start states live only inside the keyframes."""
    style, extra, clip = "", ("", "", ""), ""
    if animated:
        style = """<style>
.entry{transform-box:fill-box;transform-origin:left center;animation:grow .7s cubic-bezier(.65,0,.35,1) .1s both}
.pl{transform-box:fill-box;transform-origin:left center;animation:grow .5s cubic-bezier(.22,1,.36,1) .7s both}
.pr{transform-box:fill-box;transform-origin:right center;animation:grow .5s cubic-bezier(.22,1,.36,1) .7s both}
.wipe{transform-box:fill-box;transform-origin:left center;animation:grow 1.1s cubic-bezier(.65,0,.35,1) 1.1s both}
.line{animation:fade .8s ease-out 2s both}
@keyframes grow{from{transform:scaleX(0)}to{transform:scaleX(1)}}
@keyframes fade{from{opacity:0}to{opacity:1}}
@media (prefers-reduced-motion:reduce){.entry,.pl,.pr,.wipe,.line{animation:none}}
</style>"""
        extra = (' class="entry"', ' class="pl"', ' class="pr"')
    mark_svg, word_svg, width, word_left = lockup(BRASS, PAPER, height=220, extra=extra)
    left, top = 140, 150
    if animated:
        clip = ' clip-path="url(#reveal)"'
        # The clip lives in the lockup group's coordinates, so it starts where the word starts.
        style += f'<defs><clipPath id="reveal"><rect class="wipe" x="{word_left - 10:.0f}" y="-50" width="{width - word_left + 40:.0f}" height="320"/></clipPath></defs>'
    font = 'font-family="-apple-system,BlinkMacSystemFont,Helvetica Neue,Arial,sans-serif"'
    cls = ' class="line"' if animated else ""
    text = (f'<g{cls}><text x="{left}" y="{top + 300}" {font} font-size="38" fill="{PAPER}">'
            'A double-entry payments ledger in Rust.</text>'
            f'<text x="{left}" y="{top + 350}" font-family="Menlo,monospace" font-size="24" fill="{SMOKE}">'
            'Axum · SQLx · Postgres · Paystack webhooks</text></g>')
    return f'<rect width="1280" height="640" fill="{INK}"/>{style}<g transform="translate({left},{top})">{mark_svg}<g{clip}>{word_svg}</g></g>{text}'


def main():
    for suffix, color in (("", "#000"), ("-white", "#FFFFFF"), ("-brass", BRASS), ("-bronze", BRONZE), ("-ink", INK)):
        write(f"ledger-mark{suffix}.svg", mark(color), 256, 256)
    mark_svg, word_svg, width, _ = lockup(BRONZE, INK)
    write("ledger-lockup-light.svg", mark_svg + word_svg, width, 200)
    mark_svg, word_svg, width, _ = lockup(BRASS, PAPER)
    write("ledger-lockup-dark.svg", mark_svg + word_svg, width, 200)
    # Square avatar (org or package icon): brass mark on an ink tile.
    write("ledger-avatar.svg", f'<rect width="512" height="512" rx="112" fill="{INK}"/>'
          f'<g transform="translate(64,64) scale(1.5)">{mark(BRASS)}</g>', 512, 512)
    write("banner.svg", banner(animated=True), 1280, 640)
    write("banner-static.svg", banner(animated=False), 1280, 640)


if __name__ == "__main__":
    main()
