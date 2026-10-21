# payments-ledger brand

## The idea

An equals sign whose lower bar is split in two and spans the same width as the bar above. The top bar is the journal entry, the two below are its postings, and they always sum to it. It is the rule the database enforces, drawn.

- **Mark**: on its own wherever space is square (avatar, favicon).
- **Lockup**: the mark beside the word `ledger`, set in Poppins SemiBold and outlined, so no font is needed. The mark's bars match the letters' x-height.

## Colour

| Name | HEX | Use |
|---|---|---|
| Brass | `#D4A84B` | the mark on dark (8.0:1 on Ink) |
| Bronze | `#A07A2C` | the mark on light (3.5:1 on Paper) |
| Ink | `#16181C` | banner background, the word on light |
| Paper | `#F4F1EA` | the word on dark, light backgrounds |
| Smoke | `#9A958A` | secondary text on Ink |

Ink and brass, like the gold leaf on a bound ledger book. It stays clear of fintech's usual blue and green.

## Files

| File | Use |
|---|---|
| `ledger-mark-*.svg` | the mark in black, white, Brass, Bronze, Ink |
| `ledger-lockup-light.svg`, `ledger-lockup-dark.svg` | mark and word for light and dark backgrounds |
| `ledger-avatar.svg` | square avatar: Brass mark on an Ink tile |
| `banner.svg` | animated README header: the entry bar draws, its postings grow in to span the same width, the word wipes in. Still under reduced motion. |
| `banner-static.svg`, `banner.png` | still banner; the PNG (exported at 1280x640) is the GitHub social preview |
| `generate.py` | regenerates every SVG above |
| `outline_wordmark.py`, `POPPINS-OFL.txt` | outlines the word from a Poppins font file; Poppins is SIL Open Font License 1.1 |

## Use

- Clear space: at least the height of one bar around the mark or lockup.
- Minimum size: mark 16 px; lockup 96 px wide.
- Keep the gap in the lower bar, both lower bars the same length, and the lower bar the full width of the upper. Do not retype the word in another font.
