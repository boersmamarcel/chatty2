#!/usr/bin/env python3
"""XTest driver for the Chatty desktop app under Xvfb.

Usage: x.py <command> [args]      (DISPLAY must point at the Xvfb server)

  geom [pattern]                 window id, title and root-relative geometry
  resize W H                     move the Chatty window to 0,0 and size it
  focus                          give the Chatty window keyboard focus (no WM)
  shot FILE [X Y W H]            screenshot the screen (or a region) to PNG
  crop IN OUT X0 Y0 X1 Y1 [S]    crop a PNG (optionally scale by S) for Read
  move X Y                       warp the pointer
  click X Y                      left click (motion first — gpui needs it)
  rclick X Y                     right click (context menus)
  dblclick X Y
  ctrlclick X Y | shiftclick X Y modifier click (multi-select)
  drag X1 Y1 X2 Y2 [STEPS] [shot]  press, move in steps, release; `shot`
                                 saves shots/drag-inflight.png before release
  wheel X Y [-N|N]               scroll N notches (negative = down)
  type TEXT                      type ASCII text (shifted symbols handled)
  key KEYSYM                     press one key: Return, Escape, End, Tab, F5…
  ctrl KEYSYM | alt KEYSYM | shift KEYSYM   chord with one modifier

Needs python-xlib and Pillow (on this workstation: the pyenv python3, not
/usr/bin/python3).
"""
import sys
import time

from PIL import Image
from Xlib import X, XK, display
from Xlib.ext import xtest

d = display.Display()
root = d.screen().root

SHIFTED = set(':"()?!_#*+<>~@&{}|^%$')
KEYSYMS = {
    ' ': 'space', '\n': 'Return', '\t': 'Tab', '.': 'period', ',': 'comma',
    '/': 'slash', ':': 'colon', ';': 'semicolon', '-': 'minus', '_': 'underscore',
    '(': 'parenleft', ')': 'parenright', '"': 'quotedbl', "'": 'apostrophe',
    '=': 'equal', '?': 'question', '!': 'exclam', '#': 'numbersign',
    '*': 'asterisk', '+': 'plus', '<': 'less', '>': 'greater', '~': 'asciitilde',
    '@': 'at', '&': 'ampersand', '{': 'braceleft', '}': 'braceright', '|': 'bar',
    '^': 'asciicircum', '%': 'percent', '$': 'dollar', '[': 'bracketleft',
    ']': 'bracketright', '\\': 'backslash', '`': 'grave',
}


def find(pattern):
    out = []

    def walk(w):
        try:
            name = w.get_wm_name()
        except Exception:
            name = None
        if name and pattern.lower() in name.lower():
            out.append(w)
        try:
            for c in w.query_tree().children:
                walk(c)
        except Exception:
            pass

    walk(root)
    return out


def geom(w):
    g = w.get_geometry()
    p = w.translate_coords(root, 0, 0)
    return -p.x, -p.y, g.width, g.height


def keycode(name):
    return d.keysym_to_keycode(XK.string_to_keysym(name))


def motion(x, y):
    # gpui only registers an XTest button press if a MotionNotify preceded
    # it; warp_pointer alone does nothing. Two motions, so the hover state
    # settles before the press.
    xtest.fake_input(d, X.MotionNotify, x=x, y=y)
    d.sync()
    time.sleep(0.3)
    xtest.fake_input(d, X.MotionNotify, x=x + 1, y=y)
    d.sync()
    time.sleep(0.3)


def press(button=1):
    xtest.fake_input(d, X.ButtonPress, button)
    d.sync()
    time.sleep(0.15)
    xtest.fake_input(d, X.ButtonRelease, button)
    d.sync()


def with_modifier(name, body):
    kc = keycode(name)
    xtest.fake_input(d, X.KeyPress, kc)
    d.sync()
    time.sleep(0.1)
    try:
        body()
    finally:
        time.sleep(0.05)
        xtest.fake_input(d, X.KeyRelease, kc)
        d.sync()


def screenshot(path, region=None):
    if region is None:
        g = root.get_geometry()
        region = (0, 0, g.width, g.height)
    x, y, w, h = region
    raw = root.get_image(x, y, w, h, X.ZPixmap, 0xffffffff)
    Image.frombytes("RGB", (w, h), raw.data, "raw", "BGRX").save(path)
    print("saved", path, w, h)


def main(argv):
    cmd = argv[1]
    a = argv[2:]
    if cmd == "geom":
        for w in find(a[0] if a else "chatty"):
            print(w.id, w.get_wm_name(), geom(w))
    elif cmd == "resize":
        for w in find("chatty"):
            w.configure(x=0, y=0, width=int(a[0]), height=int(a[1]))
            d.sync()
            print("resized", w.id)
            break
    elif cmd == "focus":
        for w in find("chatty"):
            d.set_input_focus(w, X.RevertToParent, X.CurrentTime)
            d.sync()
            print("focused", w.id)
            break
    elif cmd == "shot":
        region = tuple(int(v) for v in a[1:5]) if len(a) >= 5 else None
        screenshot(a[0], region)
    elif cmd == "crop":
        src, out = a[0], a[1]
        x0, y0, x1, y1 = (int(v) for v in a[2:6])
        im = Image.open(src).crop((x0, y0, x1, y1))
        if len(a) > 6:
            s = float(a[6])
            im = im.resize((int(im.width * s), int(im.height * s)), Image.LANCZOS)
        im.save(out)
        print("saved", out, im.size)
    elif cmd == "move":
        d.warp_pointer(int(a[0]), int(a[1]))
        d.sync()
    elif cmd in ("click", "rclick", "dblclick"):
        motion(int(a[0]), int(a[1]))
        if cmd == "rclick":
            press(3)
        elif cmd == "dblclick":
            press(1)
            time.sleep(0.08)
            press(1)
        else:
            press(1)
    elif cmd in ("ctrlclick", "shiftclick"):
        motion(int(a[0]), int(a[1]))
        with_modifier("Control_L" if cmd == "ctrlclick" else "Shift_L", press)
    elif cmd == "drag":
        x1, y1, x2, y2 = (int(v) for v in a[0:4])
        steps = int(a[4]) if len(a) > 4 else 10
        motion(x1, y1)
        xtest.fake_input(d, X.ButtonPress, 1)
        d.sync()
        time.sleep(0.4)
        for i in range(1, steps + 1):
            xtest.fake_input(
                d, X.MotionNotify,
                x=x1 + (x2 - x1) * i // steps, y=y1 + (y2 - y1) * i // steps,
            )
            d.sync()
            time.sleep(0.35)
        time.sleep(0.6)
        if len(a) > 5 and a[5] == "shot":
            screenshot("shots/drag-inflight.png")
        xtest.fake_input(d, X.ButtonRelease, 1)
        d.sync()
    elif cmd == "wheel":
        motion(int(a[0]), int(a[1]))
        n = int(a[2]) if len(a) > 2 else -3
        button = 5 if n < 0 else 4
        for _ in range(abs(n)):
            xtest.fake_input(d, X.ButtonPress, button)
            xtest.fake_input(d, X.ButtonRelease, button)
            d.sync()
            time.sleep(0.05)
    elif cmd == "type":
        for ch in a[0]:
            kc = keycode(KEYSYMS.get(ch, ch))
            shift = ch.isupper() or ch in SHIFTED
            if shift:
                xtest.fake_input(d, X.KeyPress, keycode("Shift_L"))
            xtest.fake_input(d, X.KeyPress, kc)
            xtest.fake_input(d, X.KeyRelease, kc)
            if shift:
                xtest.fake_input(d, X.KeyRelease, keycode("Shift_L"))
            d.sync()
            time.sleep(0.02)
    elif cmd == "key":
        kc = keycode(a[0])
        xtest.fake_input(d, X.KeyPress, kc)
        xtest.fake_input(d, X.KeyRelease, kc)
        d.sync()
    elif cmd in ("ctrl", "alt", "shift"):
        mod = {"ctrl": "Control_L", "alt": "Alt_L", "shift": "Shift_L"}[cmd]
        kc = keycode(a[0])

        def tap():
            xtest.fake_input(d, X.KeyPress, kc)
            xtest.fake_input(d, X.KeyRelease, kc)
            d.sync()

        with_modifier(mod, tap)
    else:
        print(__doc__)
        sys.exit(2)


if __name__ == "__main__":
    main(sys.argv)
