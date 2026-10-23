#!/usr/bin/env python3
"""Emit the Creel logo SVGs into dist/brand/.

Geometry and colours come from the logo exploration (Y1 mark, Cofactr
"Steel · greens" palette). Run `python3 dist/brand/generate.py`, then
`dist/brand/build-assets.sh` to rasterise and build creel.icns.

Outputs (all standalone SVG):
  creel-icon.svg            full app icon, 1024 canvas, macOS tile
  creel-icon-small.svg      simplified icon for 32 px and below (solid orb)
  creel-mark-{light,dark}.svg      flat mark, transparent background
  creel-wordmark-{light,dark}.svg  oversized C + small capitals REEL
  creel-lockup-{light,dark}.svg    mark + wordmark

The flat marks are transparent: the gaps that separate threads from the
halo and the ball are cut with SVG masks rather than painted in a
background colour, so they sit on any surface.
"""
import math
from pathlib import Path

OUT = Path(__file__).resolve().parent

CF = {"clover": "#38D059", "juniper": "#245F31", "fir": "#619C82", "moss": "#D6F1DB", "steel": "#183028"}
STRANDS = [CF["clover"], CF["fir"], CF["juniper"], CF["fir"]]
# On the dark theme the juniper strand would vanish into moss ink only if
# it were dark; against moss ink it still reads, so both variants share it.

RING = [(0, "#5E6B78"), (.3, "#FFFFFF"), (.55, "#9AA7B4"), (.8, "#F1F5F9"), (1, "#4B5560")]
BOB = [(0, "#FFFFFF"), (.35, "#C9D2DB"), (1, "#3D4650")]
MOSSBALL = [(0, "#FFFFFF"), (.5, "#E8F7EB"), (1, "#9CC9A7")]
TILE = [(0, "#24443A"), (1, "#0C1A15")]

BOBS = [(50, 36), (100, 36), (150, 36)]
CX, CY, R = 100, 160, 29
FAMS = [(-32, [11, 22]), (48, [9, 19])]
# the back halo is drawn as the full ellipse so the front arc hides the seam
BACK = 'M32 112 A68 18 0 0 1 168 112 A68 18 0 0 1 32 112 Z'
FRONT = 'M32 112 A68 18 0 0 0 168 112'
# Tight crop of the 200-unit mark (ring stroke, bobbins, ball) with a little air.
MARK_VIEWBOX = (24, 18, 152, 174)
WM_X0, WM_Y0, WM_W, WM_H = 178, 72, 372, 76


def stops(items):
    return ''.join(f'<stop offset="{o}" stop-color="{c}"/>' for o, c in items)


def lin(i, items, x2=0, y2=1):
    return f'<linearGradient id="{i}" x1="0" y1="0" x2="{x2}" y2="{y2}">{stops(items)}</linearGradient>'


def rad(i, items):
    return f'<radialGradient id="{i}" cx=".35" cy=".3" r=".8">{stops(items)}</radialGradient>'


def threads_lines(colors, sw=8):
    return ''.join(
        f'<line x1="{x}" y1="{y + 8}" x2="100" y2="152" stroke="{c}"/>' for (x, y), c in zip(BOBS, colors)
    ).join([f'<g stroke-linecap="round" stroke-width="{sw}">', '</g>'])


def winding(strands, sw=3.4):
    parts = []
    k = 0
    for ang, rys in FAMS:
        for ry in rys:
            parts.append(
                f'<ellipse cx="{CX}" cy="{CY}" rx="{R + 2}" ry="{ry}" transform="rotate({ang} {CX} {CY})" '
                f'fill="none" stroke="{strands[k % len(strands)]}" stroke-width="{sw}"/>'
            )
            k += 1
    return ''.join(parts)


def icon_mark(ring, bob, ball, ids, strands=STRANDS, wound=True, heavy=False):
    """The coloured mark drawn on the icon tile (no cutouts needed)."""
    rw, tw, br = (14, 13, 18) if heavy else (9, 8, 15)
    o = [f'<path d="{BACK}" fill="none" stroke="{ring}" stroke-width="{rw}"/>']
    thread = [CF["clover"], CF["fir"], CF["moss"]]
    o.append(threads_lines(thread, tw))
    o.append(f'<path d="{FRONT}" fill="none" stroke="{ring}" stroke-width="{rw}"/>')
    o.append(f'<g fill="{bob}">' + ''.join(f'<circle cx="{x}" cy="{y}" r="{br}"/>' for x, y in BOBS) + '</g>')
    o.append(f'<circle cx="{CX}" cy="{CY}" r="{R}" fill="{ball}"/>')
    if wound:
        o.append(f'<clipPath id="{ids}clip"><circle cx="{CX}" cy="{CY}" r="{R}"/></clipPath>')
        o.append(f'<g clip-path="url(#{ids}clip)">{winding(strands)}</g>')
        o.append(f'<ellipse cx="{CX - 10}" cy="{CY - 14}" rx="8" ry="4.5" fill="#fff" opacity=".75"/>')
    else:
        o.append(f'<ellipse cx="{CX - 9}" cy="{CY - 11}" rx="9" ry="5.5" fill="#fff" opacity=".7"/>')
    return ''.join(o)


def squircle(cx, cy, half, n=5.0, steps=360):
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        c, s = math.cos(t), math.sin(t)
        x = cx + half * math.copysign(abs(c) ** (2 / n), c)
        y = cy + half * math.copysign(abs(s) ** (2 / n), s)
        pts.append(f'{x:.2f} {y:.2f}')
    return 'M' + ' L'.join(pts) + ' Z'


def app_icon(small=False):
    """1024 canvas; 824 tile centred (Apple's macOS icon grid, 100 px margins)."""
    pid = 'sm' if small else 'lg'
    defs = (lin(f'{pid}bg', TILE) + lin(f'{pid}ring', RING, 1, 0) + rad(f'{pid}bob', BOB) + rad(f'{pid}ball', MOSSBALL)
            + '<filter id="drop" x="-10%" y="-10%" width="120%" height="125%">'
              '<feDropShadow dx="0" dy="10" stdDeviation="14" flood-color="#000" flood-opacity=".32"/></filter>')
    s = 3.937 if not small else 4.1
    # centre the mark's bbox (x 27.5..172.5, y 21..189 → centre 100,105) on the tile
    tx, ty = 512 - s * 100, 512 - s * 105
    body = icon_mark(f'url(#{pid}ring)', f'url(#{pid}bob)', f'url(#{pid}ball)', pid,
                     wound=not small, heavy=small)
    highlight = ('<path d="' + squircle(512, 512, 412 - 2) + '" fill="none" stroke="#fff" stroke-opacity=".10" stroke-width="3"/>')
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">'
            f'<defs>{defs}</defs>'
            f'<path d="{squircle(512, 512, 412)}" fill="url(#{pid}bg)" filter="url(#drop)"/>{highlight}'
            f'<g transform="translate({tx:.2f} {ty:.2f}) scale({s})">{body}</g></svg>\n')


def flat_mark(ink, strands=STRANDS, uid='m'):
    """Flat mark with transparent gaps (masks), in `ink` with coloured winding."""
    m = uid
    w = 200
    ring = lambda d, sw: f'<path d="{d}" fill="none" stroke="{ink}" stroke-width="{sw}"/>'
    gap_lines = ''.join(f'<line x1="{x}" y1="{y + 8}" x2="100" y2="152"/>' for x, y in BOBS)
    defs = (
        # cut around the threads where they cross the back halo
        f'<mask id="{m}a" maskUnits="userSpaceOnUse" x="0" y="0" width="{w}" height="{w}">'
        f'<rect width="{w}" height="{w}" fill="#fff"/>'
        f'<g stroke="#000" stroke-width="15" stroke-linecap="round">{gap_lines}</g></mask>'
        # cut the back halo and threads around the front halo
        f'<mask id="{m}b" maskUnits="userSpaceOnUse" x="0" y="0" width="{w}" height="{w}">'
        f'<rect width="{w}" height="{w}" fill="#fff"/>'
        f'<path d="{BACK}" fill="none" stroke="#000" stroke-width="17" clip-path="url(#{m}g)"/></mask>'
        # cut everything but the ball around the ball
        f'<mask id="{m}c" maskUnits="userSpaceOnUse" x="0" y="0" width="{w}" height="{w}">'
        f'<rect width="{w}" height="{w}" fill="#fff"/>'
        f'<circle cx="{CX}" cy="{CY}" r="{R + 3.5}" fill="#000"/></mask>'
        # front halo = lower half of the ellipse; the cut starts 1.5 units below the
        # ring so the two never meet on the same edge (that leaves a hairline seam)
        f'<clipPath id="{m}g"><rect x="0" y="112.5" width="{w}" height="{w}"/></clipPath>'
        f'<clipPath id="{m}f"><rect x="0" y="111" width="{w}" height="{w}"/></clipPath>'
        f'<clipPath id="{m}k"><circle cx="{CX}" cy="{CY}" r="{R}"/></clipPath>'
    )
    thread_g = threads_lines([ink] * 3)
    back = f'<g mask="url(#{m}a)">{ring(BACK, 9)}</g>'
    behind = f'<g mask="url(#{m}b)">{back}{thread_g}</g>'
    front = f'<g clip-path="url(#{m}f)">{ring(BACK, 9)}</g>'
    bobs = f'<g fill="{ink}">' + ''.join(f'<circle cx="{x}" cy="{y}" r="15"/>' for x, y in BOBS) + '</g>'
    upper = f'<g mask="url(#{m}c)">{behind}{front}{bobs}</g>'
    ball = (f'<circle cx="{CX}" cy="{CY}" r="{R}" fill="{ink}"/>'
            f'<g clip-path="url(#{m}k)">{winding(strands)}</g>')
    return f'<defs>{defs}</defs>{upper}{ball}'


def svg(viewbox, body, extra=''):
    vb = ' '.join(str(v) for v in viewbox)
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb}"{extra}>{body}</svg>\n'


# Wordmark: Copperplate Bold (ships with macOS; Apple's font, not redistributed
# here: the SVG names it as a font-family and falls back to serif), an
# oversized C then small capitals REEL, stretched 1.44x (about 250 -> 360
# units) and slanted -12 degrees. The stretch is an explicit scale() rather
# than textLength so rsvg, WebKit and Chrome all draw the same shape.
WORDMARK_STRETCH = 1.44
WORDMARK_BOX = (WM_X0, WM_Y0, WM_W, WM_H)


def wordmark_text(color, x=145, y=134):
    return (f'<text x="{x}" y="{y}" fill="{color}" font-family="Copperplate,\'Copperplate Gothic Bold\',serif" '
            f'font-weight="700" font-size="92" '
            f'transform="skewX(-12) scale({WORDMARK_STRETCH} 1)">C<tspan font-size="68">REEL</tspan></text>')


def main():
    (OUT / 'creel-icon.svg').write_text(app_icon())
    (OUT / 'creel-icon-small.svg').write_text(app_icon(small=True))
    for name, ink, bg in (('light', CF['steel'], None), ('dark', CF['moss'], CF['steel'])):
        (OUT / f'creel-mark-{name}.svg').write_text(svg(MARK_VIEWBOX, flat_mark(ink, uid=f'{name[0]}m')))
        (OUT / f'creel-wordmark-{name}.svg').write_text(svg(WORDMARK_BOX, wordmark_text(ink)))
        mark = flat_mark(ink, uid=f'{name[0]}l')
        (OUT / f'creel-lockup-{name}.svg').write_text(
            svg((10, 10, 540, 186),
                f'<g transform="translate(0 6) scale(.94)">{mark}</g>{wordmark_text(ink)}'))


if __name__ == '__main__':
    main()
