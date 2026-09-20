#!/usr/bin/env python3
"""把 assets/icon.svg 渲染成 macOS 需要的各档位图，并打包成 assets/rustmd.icns。

为什么是逐档渲染而不是「渲一张 1024 再缩」：SVG 是矢量的，16px 直接由曲线采样
出来比从 1024 缩下去干净得多 —— 小尺寸下 M 的两根竖笔只有不到一个像素宽，
重采样会把它糊成一团灰。

为什么要 re-exec 一次：cairosvg 走 cairo，而 cairo 只存在于 Homebrew 的
/opt/homebrew/lib，不在 dyld 的默认搜索路径里，必须靠 DYLD_FALLBACK_LIBRARY_PATH
告诉它。ctypes 加载动态库发生在 import 之后的第一次调用，所以在这里补好环境
变量再重开自己，比让调用方记住这个前缀可靠。

用法：
    scripts/make-icon.py            # 出 assets/rustmd.icns
    scripts/make-icon.py --preview  # 额外出一张自检用的拼版图
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SVG = ROOT / "assets" / "icon.svg"
ICNS = ROOT / "assets" / "rustmd.icns"
WORK = ROOT / "target" / "icon"
ICONSET = WORK / "rustmd.iconset"
HOMEBREW_LIB = "/opt/homebrew/lib"

# iconutil 要的文件名 → 该文件的像素边长。@2x 是同一张图的另一档，不能省，
# 少了 Retina 下就会被放大糊掉。
ENTRIES = {
    "icon_16x16.png": 16,
    "icon_16x16@2x.png": 32,
    "icon_32x32.png": 32,
    "icon_32x32@2x.png": 64,
    "icon_128x128.png": 128,
    "icon_128x128@2x.png": 256,
    "icon_256x256.png": 256,
    "icon_256x256@2x.png": 512,
    "icon_512x512.png": 512,
    "icon_512x512@2x.png": 1024,
}


def ensure_cairo() -> None:
    """让 dyld 能找到 Homebrew 的 libcairo，必要时带着环境变量重开自己。"""
    paths = os.environ.get("DYLD_FALLBACK_LIBRARY_PATH", "").split(":")
    if HOMEBREW_LIB in paths or not Path(f"{HOMEBREW_LIB}/libcairo.2.dylib").exists():
        return
    env = dict(os.environ)
    env["DYLD_FALLBACK_LIBRARY_PATH"] = ":".join(
        [p for p in paths if p] + [HOMEBREW_LIB]
    )
    os.execve(sys.executable, [sys.executable, *sys.argv], env)


def render(size: int) -> "Image.Image":
    import cairosvg
    from PIL import Image
    import io

    png = cairosvg.svg2png(
        url=str(SVG),
        output_width=size,
        output_height=size,
    )
    return Image.open(io.BytesIO(png)).convert("RGBA")


def preview(sheet: Path) -> None:
    """拼一张自检图：各档原尺寸 + 16px 放大 12 倍，好判断小尺寸下还认不认得出。"""
    from PIL import Image

    sizes = [16, 32, 64, 128, 256]
    cell, gap = 300, 24
    width = gap + len(sizes) * (cell + gap)
    height = gap + cell + gap + 16 * 12 + gap
    img = Image.new("RGBA", (width, height), (238, 238, 242, 255))

    x = gap
    for s in sizes:
        tile = render(s)
        img.alpha_composite(tile, (x + (cell - s) // 2, gap + (cell - s) // 2))
        x += cell + gap

    big = render(16).resize((16 * 12, 16 * 12), Image.NEAREST)
    img.alpha_composite(big, (gap, gap + cell + gap))

    img.convert("RGB").save(sheet)


def main() -> int:
    ensure_cairo()

    if not SVG.exists():
        print(f"找不到 {SVG}", file=sys.stderr)
        return 1

    if shutil.which("iconutil") is None:
        print("找不到 iconutil，只能在 macOS 上打包 icns", file=sys.stderr)
        return 1

    shutil.rmtree(ICONSET, ignore_errors=True)
    ICONSET.mkdir(parents=True)

    cache: dict[int, "Image.Image"] = {}
    for name, size in ENTRIES.items():
        if size not in cache:
            cache[size] = render(size)
        cache[size].save(ICONSET / name)
        print(f"  {name:<24} {size}×{size}")

    ICNS.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["iconutil", "-c", "icns", str(ICONSET), "-o", str(ICNS)],
        check=True,
    )
    print(f"==> {ICNS.relative_to(ROOT)}  ({ICNS.stat().st_size / 1024:.1f} KB)")

    if "--preview" in sys.argv:
        sheet = WORK / "preview.png"
        preview(sheet)
        print(f"==> {sheet}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
