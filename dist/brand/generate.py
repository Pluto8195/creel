#!/usr/bin/env python3
"""Emit the Mast logo SVGs into dist/brand/ (mark D, thin-rim aurora lens, Cofactr palette only).

Run `python3 dist/brand/generate.py`, then `dist/brand/build-assets.sh` to rasterise and
build dist/macos/mast.icns. build-assets.sh runs this script first, so running it alone is
only needed to inspect the SVGs.

Colours are Cofactr brand colours (ui/src/assets/scss/_themes-vars.module.scss); the only
additions are white highlights and two shades of steel for the icon tile.

Outputs (all standalone SVG):
  mast-icon.svg                    full app icon, 1024 canvas, metal finish on a steel tile
  mast-icon-small.svg              heavier strokes for 32 px and below
  mast-mark-{light,dark}.svg       flat mark, transparent background (steel / moss ink)
  mast-mark-mono.svg               single-colour mark (no gradients, filters or masks)
  mast-wordmark-{light,dark}.svg   Copperplate "MAST": oversized M, small caps AST
  mast-lockup-{light,dark}.svg     mark + wordmark
  status/mast-icon-{idle,working,ready,input,error}.svg
                                   the app icon with a lens gradient per status lamp

The flat marks are transparent: the gaps that separate the rigging from the lens and the
ring are cut with SVG masks rather than painted in a background colour, so they sit on any
surface.
"""
import math
from pathlib import Path

OUT = Path(__file__).resolve().parent
FONT = "Copperplate,'Copperplate Gothic Bold',serif"

COFACTR = {'clover': '#38D059', 'juniper': '#245F31', 'fir': '#619C82', 'moss': '#D6F1DB', 'steel': '#183028',
           'silicon': '#B5B5B5', 'magnesium': '#F3F3F3', 'cobalt': '#3F69CC'}
C = COFACTR
TILE = [(0, '#24443A'), (1, '#0C1A15')]                       # steel, lightened and darkened
AURORA = [(0, C['moss']), (.28, C['clover']), (1, C['cobalt'])]
METAL = [(0, C['fir']), (.3, C['magnesium']), (.55, C['silicon']), (.8, C['moss']), (1, C['fir'])]
METAL_BALL = [(0, '#FFFFFF'), (.35, C['magnesium']), (.75, C['silicon']), (1, C['fir'])]

CX, CY, R = 100, 50, 27
RCY, RRX, RRY = 96, 50, 13
LINES = [(100, 60, 100, 172), (100, 62, 50, 150), (100, 62, 150, 150)]
NODES = [(50, 156), (150, 156), (100, 178)]
LENS, RIM_W, RIM_MID = .7 * R, .2 * R, .9 * R


def stops(items):
    return ''.join(f'<stop offset="{o}" stop-color="{c}"/>' for o, c in items)


def lens(uid, rim_paint, mono=False):
    if mono:
        return (f'<circle cx="{CX}" cy="{CY}" r="{RIM_MID:.2f}" fill="none" stroke="{rim_paint}" stroke-width="{RIM_W:.2f}"/>'
                f'<circle cx="{CX}" cy="{CY}" r="{LENS:.2f}" fill="{rim_paint}"/>')
    r = LENS
    cx_, cy_ = CX + r * .38, CY + r * .5
    return (f'<linearGradient id="{uid}a" x1="0" y1="0" x2="1" y2="1">{stops(AURORA)}</linearGradient>'
            f'<linearGradient id="{uid}r" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".8"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient>'
            f'<filter id="{uid}b" x="-100%" y="-100%" width="300%" height="300%"><feGaussianBlur stdDeviation="5"/></filter>'
            f'<clipPath id="{uid}c"><circle cx="{CX}" cy="{CY}" r="{r:.2f}"/></clipPath>'
            f'<circle cx="{CX}" cy="{CY}" r="{r + 6:.2f}" fill="url(#{uid}a)" opacity=".65" filter="url(#{uid}b)"/>'
            f'<circle cx="{CX}" cy="{CY}" r="{RIM_MID:.2f}" fill="none" stroke="{rim_paint}" stroke-width="{RIM_W:.2f}"/>'
            f'<circle cx="{CX}" cy="{CY}" r="{r:.2f}" fill="url(#{uid}a)"/>'
            f'<g clip-path="url(#{uid}c)"><ellipse cx="{CX}" cy="{CY - r * .55:.2f}" rx="{r * .95:.2f}" ry="{r * .62:.2f}" fill="url(#{uid}r)"/>'
            f'<ellipse cx="{cx_:.2f}" cy="{cy_:.2f}" rx="{r * .32:.2f}" ry="{r * .16:.2f}" fill="#fff" opacity=".55" transform="rotate(-35 {cx_:.2f} {cy_:.2f})"/></g>'
            f'<circle cx="{CX}" cy="{CY}" r="{r - .9:.2f}" fill="none" stroke="#fff" stroke-opacity=".6" stroke-width="1.2"/>')


def mark(uid, ink, ball=None, small=False, mono=False):
    """ink paints the rigging and rings; ball (default ink) paints the three agents."""
    k = 1.35 if small else 1.0
    lw, rw, nr = 8 * k, 8 * k, min(14 * k, 17)
    back = f'M{CX - RRX} {RCY} A{RRX} {RRY} 0 0 1 {CX + RRX} {RCY}'
    front = f'M{CX - RRX} {RCY} A{RRX} {RRY} 0 0 0 {CX + RRX} {RCY}'
    lines = lambda col, w: ''.join(f'<line x1="{a}" y1="{b}" x2="{c}" y2="{d}" stroke="{col}" stroke-width="{w:.1f}" stroke-linecap="round"/>' for a, b, c, d in LINES)
    hole = f'<circle cx="{CX}" cy="{CY}" r="{R + 4}" fill="#000"/>'
    m = lambda i, cut: (f'<mask id="{uid}{i}" maskUnits="userSpaceOnUse" x="0" y="0" width="200" height="210">'
                        f'<rect width="200" height="210" fill="#fff"/>{cut}</mask>')
    defs = m('b', lines('#000', lw + 7) + hole) + m('l', f'<path d="{front}" fill="none" stroke="#000" stroke-width="{rw + 8:.1f}"/>' + hole)
    return (f'<defs>{defs}</defs>'
            f'<path d="{back}" fill="none" stroke="{ink}" stroke-width="{rw:.1f}" mask="url(#{uid}b)"/>'
            f'<g mask="url(#{uid}l)">{lines(ink, lw)}</g>'
            f'<path d="{front}" fill="none" stroke="{ink}" stroke-width="{rw:.1f}"/>'
            f'<g fill="{ball or ink}">' + ''.join(f'<circle cx="{x}" cy="{y}" r="{nr:.1f}"/>' for x, y in NODES) + '</g>'
            + lens(uid + 'z', ink, mono))


def squircle(cx, cy, half, n=5.0, steps=360):
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        c, s = math.cos(t), math.sin(t)
        pts.append(f'{cx + half * math.copysign(abs(c) ** (2 / n), c):.2f} {cy + half * math.copysign(abs(s) ** (2 / n), s):.2f}')
    return 'M' + ' L'.join(pts) + ' Z'


def svg(vb, body, w=None, h=None):
    size = f' width="{w}" height="{h}"' if w else ''
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{" ".join(map(str, vb))}"{size}>{body}</svg>\n'


def icon(small=False):
    u = 'is' if small else 'il'
    s = 4.1 if small else 3.75
    tx, ty = 512 - s * CX, 512 - s * 104
    defs = (f'<linearGradient id="{u}bg" x1="0" y1="0" x2="0" y2="1">{stops(TILE)}</linearGradient>'
            f'<linearGradient id="{u}M" gradientUnits="userSpaceOnUse" x1="36" y1="0" x2="164" y2="0">{stops(METAL)}</linearGradient>'
            f'<radialGradient id="{u}B" cx=".35" cy=".3" r=".8">{stops(METAL_BALL)}</radialGradient>'
            '<filter id="drop" x="-10%" y="-10%" width="120%" height="125%"><feDropShadow dx="0" dy="10" stdDeviation="14" flood-color="#000" flood-opacity=".32"/></filter>')
    return svg((0, 0, 1024, 1024),
               f'<defs>{defs}</defs><path d="{squircle(512, 512, 412)}" fill="url(#{u}bg)" filter="url(#drop)"/>'
               f'<path d="{squircle(512, 512, 410)}" fill="none" stroke="#fff" stroke-opacity=".10" stroke-width="3"/>'
               f'<g transform="translate({tx:.1f} {ty:.1f}) scale({s})">{mark(u + "m", f"url(#{u}M)", f"url(#{u}B)", small)}</g>', 1024, 1024)


def wordmark(ink, x=145, y=134):
    return (f'<text x="{x}" y="{y}" fill="{ink}" font-family="{FONT}" font-weight="700" font-size="92" '
            f'transform="skewX(-12) scale(1.44 1)">M<tspan font-size="68">AST</tspan></text>')


# Status lamp gradients (the lens colour is the only thing that changes).
CADMIUM, BROMINE = '#FE4B25', '#DFBE1B'
STATUS_LENS = {
    'idle': [(0, C['magnesium']), (.5, C['silicon']), (1, '#7F8A85')],
    'working': [(0, C['moss']), (.28, C['clover']), (1, C['fir'])],
    'ready': [(0, C['moss']), (.3, '#6F92E0'), (1, C['cobalt'])],
    'input': [(0, C['magnesium']), (.35, BROMINE), (1, CADMIUM)],
    'error': [(0, BROMINE), (.3, CADMIUM), (1, '#B8321A')],
}


def main():
    global AURORA
    (OUT / 'status').mkdir(exist_ok=True)
    (OUT / 'mast-icon.svg').write_text(icon())
    (OUT / 'mast-icon-small.svg').write_text(icon(small=True))
    MARK_VB, LOCK_VB, WM_VB = (30, 4, 140, 202), (20, 0, 500, 210), (172, 70, 340, 78)
    for theme, ink in (('light', C['steel']), ('dark', C['moss'])):
        (OUT / f'mast-mark-{theme}.svg').write_text(svg(MARK_VB, mark(f'm{theme[0]}', ink)))
        (OUT / f'mast-wordmark-{theme}.svg').write_text(svg(WM_VB, wordmark(ink)))
        (OUT / f'mast-lockup-{theme}.svg').write_text(svg(LOCK_VB, f'<g transform="translate(0 6) scale(.94)">{mark(f"l{theme[0]}", ink)}</g>{wordmark(ink)}'))
    (OUT / 'mast-mark-mono.svg').write_text(svg(MARK_VB, mark('mm', '#000', small=True, mono=True)))
    default = AURORA
    for state, stops_ in STATUS_LENS.items():
        AURORA = stops_
        (OUT / 'status' / f'mast-icon-{state}.svg').write_text(icon())
    AURORA = default


if __name__ == '__main__':
    main()
