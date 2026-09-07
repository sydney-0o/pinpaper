"""Render the code-owned leaf outline at high resolution, then antialias.
Requires Pillow only for icon development; no runtime dependency.
Outline matches assets/leaf.svg, adapted from Lucide's ISC-licensed leaf.
"""
from pathlib import Path
from PIL import Image, ImageDraw
ROOT = Path(__file__).resolve().parents[1]
curves = [
    [(11,20), (7,20),(4,17),(4,13), (4,9.5),(6.5,6.7),(9.8,6.1), (15.5,5),(17,4.48),(19,2), (20,4),(21,6.18),(21,10), (21,15.5),(16.22,20),(11,20)],
    [(2,21), (2,18),(3.85,15.64),(7.08,15), (9.5,14.52),(12,13),(13,12)],
]
for name, color in [('tray.png',(0,0,0,255)),('tray-color.png',(66,139,82,255))]:
    image=Image.new('RGBA',(192,192))
    draw=ImageDraw.Draw(image)
    for curve in curves:
        points=[curve[0]]
        for i in range(1,len(curve),3):
            a,b,c,d=curve[i-1:i+3]
            for k in range(1,41):
                t=k/40; u=1-t
                points.append(tuple(u*u*u*a[j]+3*u*u*t*b[j]+3*u*t*t*c[j]+t*t*t*d[j] for j in (0,1)))
        points=[(x*8,y*8) for x,y in points]
        draw.line(points,fill=color,width=16,joint='curve')
        for x,y in [points[0],points[-1]]: draw.ellipse((x-8,y-8,x+8,y+8),fill=color)
    image.resize((32,32),Image.Resampling.LANCZOS).save(ROOT/'src-tauri/icons'/name)
