# Xara's render engine (CDraw/GDraw): reverse engineering and a Rust reimplementation plan

> **Clean-room notice.** This document describes the *behaviour* and the *data
> formats* of the Xara Xtreme render engine (a GPL-2.0-only application; the
> CDraw rasteriser is a proprietary binary library) for the purposes of
> interoperability and independent reimplementation. It reproduces neither
> source code nor headers from the original; the `file:line` references point
> into the reference tree in `xara-xtreme/` and serve only to locate the logic
> described. Xarast is implemented from this specification, not by translating
> the original.

> **Status:** RESEARCH — the basis for Xarast's "render engine" phase.
> **Document:** `docs/research/03-render-engine.md`
> **Date:** 2026-09-19
> **Primary sources:** `/home/user/xara-xtreme/GDraw/{gdraw.h,gdraw2.h,gconsts.h,GVersion.h}`,
> `/home/user/xara-xtreme/libs/x86_64/libCDraw.a` (binary, 1,037,494 bytes, 64 objects),
> `/home/user/xara-xtreme/Kernel/*`, `/home/user/xara-xtreme/wxOil/*`.
> **Depends on:** `docs/00-vision-and-scope.md`, `research/02-document-model.md`.

## Executive summary

`libCDraw.a` is Xara's closed vector rasteriser (API version **4.000**,
`GDraw/GVersion.h:100-102`). It is the one real reason Xara LX cannot be ported: the whole
application draws through it and no source code exists.

Figures extracted from the binary:

| Metric | Value |
|---|---|
| Total `T` symbols (text, global) | **926** |
| Public `extern "C"` API (`GDraw_`, `GDraw2_`, `GColour_`, `GBitmap_`) | **126** |
| — of those, `GDraw_` / `GColour_` / `GBitmap_` / `GDraw2_` | 55 / 50 / 15 / 6 |
| Internal `GDraw::` methods (C++ mangled) | **640** |
| Internal global C++ functions (not `GDraw::`) | 65 |
| Header declarations with no `extern "C"` symbol in the library | 28 |
| Entry points used by the app and **not** declared in `gdraw.h` | 4 (`ClipPathToPath`, `GenerateWallShadow`, `GenerateFloorShadow`, `ContourBitmap`) |
| Blend modes (`TransparencyEnum`) | **36 enumerated values** = 12 families × 3 variants (flat/gradient/generic) |

**Recommendation (developed in Task C):** our own hybrid engine, with
`vello`/`vello_cpu` (a *sparse strips* architecture) as the base coverage rasteriser on top of
`wgpu`, plus **our own compositor layer** implementing Xara's exotic blend modes via 2D LUTs
(256×256) in WGSL, and a `vello_cpu`/`tiny-skia` *fallback* for CPU and deterministic export.

---

## Contents

1. [Task A — Surface of the CDraw API](#1-task-a--surface-of-the-cdraw-api)
2. [Task B — Render semantics to replicate](#2-task-b--render-semantics-to-replicate)
3. [Task C — Rust reimplementation plan](#3-task-c--rust-reimplementation-plan)
4. [Appendices](#4-appendices)

---

# 1. Task A — Surface of the CDraw API

## 1.1 Methodology

```bash
nm -g --defined-only libs/x86_64/libCDraw.a | grep ' T ' | sort    # 926 symbols
nm -g --defined-only ... | c++filt                                  # demangle
ar x libCDraw.a && objdump -d -r -C libCDraw_la-GScanT-D.o          # actual semantics
```

The symbol list was cross-checked against the declarations in `GDraw/gdraw.h` and
`GDraw/gdraw2.h`, and then every call site in the application was located
(`Kernel/GDrawIntf.cpp`, `wxOil/grndrgn.cpp`, `Kernel/beveler.cpp`, `Kernel/bshadow.cpp`,
`Kernel/paths.cpp`).

**Access architecture from the app** (important for the design of the replacement):

```
Kernel/*  and  wxOil/grndrgn.cpp
   └── GDrawContext / GDrawAsm      (Kernel/GDrawIntf.h:172, GDrawIntf.cpp)   ← C++ facade with a global lock
         └── XaDraw_* / XaColour_*  (Kernel/XaDraw.h:110 — tracing layer, compiles to nothing)
               └── GDraw_* / GColour_* / GBitmap_*   (GDraw/gdraw.h)          ← libCDraw.a
```

In other words: **a single choke point already exists** (`GDrawContext`, ~180 virtual methods,
`Kernel/GDrawIntf.h:180-433`). Any reimplementation can replace that class and nothing else — a
key fact for an incremental migration.

## 1.2 Context model

CDraw is not object-oriented on the outside: every call receives an opaque `pGCONTEXT`.

The `GCONTEXT` type (`GDraw/gconsts.h:237`) is opaque: a 32-bit validation word followed by a
block of data whose size is indeterminate from the client's point of view. The validation word
holds `0xC90FDAA2` when the context is initialised.

The size is requested at run time with `GDraw_ContextLength()` and the app allocates it with its
own `malloc` (`Kernel/GDrawIntf.cpp:258-272`). From the disassembly one deduces that the context
is **enormous** (the accesses to the grey tables are at `ctx+0x610f0`, `ctx+0x614f0`,
`ctx+0x618f0`, that is ≥ 400 KiB): it holds all the dither LUTs, gradient tables, style state and
the pointers to the selected scanline routines. It is a **per-thread** context
(`GDrawAsm::GetContextForCurrentThread`, `Kernel/GDrawIntf.cpp:183`), and the whole API is
additionally serialised by a global `CriticalSection` in the facade.

## 1.3 Public API grouped by functionality

### (a) Initialisation, context, memory, errors — 16 functions

| Function | Signature (abridged) | Notes |
|---|---|---|
| `GDraw_GetVersion` | `DWORD()` | HIWORD=major (4), LOWORD=minor (0). The app aborts if it does not match (`GDrawIntf.cpp:236`). |
| `GDraw_GetSvnVersion` | `const char*()` | Non-MSW only. |
| `GDraw_ContextLength` | `INT32()` | Size to allocate for a `GCONTEXT`. |
| `GDraw_Initialise` | `(pGCONTEXT, pcGCONTEXT pOld=NULL)` | |
| `GDraw_CopyContext` | `(pcGCONTEXT, pGCONTEXT)` | State cloning (used for per-thread contexts). |
| `GDraw_Terminate`, `GDraw_Clean` | `(pGCONTEXT)` | |
| `GDraw_SetMemoryHandlers` | `(pGCONTEXT, BYTE*(*alloc)(UINT32), void(*free)(BYTE*))` | The app injects `CCMalloc/CCFree`. |
| `GDraw_SetStackLimit` / `GDraw_SetStackSize` | `(pGCONTEXT, ...)` | The rasteriser recurses (Bézier subdivision) and needs to know the stack limit; the app gives it 100 KiB (`grndrgn.cpp:660`). |
| `GDraw_ClearLastError` / `GDraw_GetLastError` / `GDraw_GetLastErrorText` | | 21 `eError` codes (`gconsts.h:119`). |
| `GDraw_ComparePalettes` | `(pcGCONTEXT, pcLOGPALETTE, bool)` | |
| `GDraw_GetMaxBitmapWidth` / `GDraw_GetMaxBitmapDepth` | `INT32()` | 0x800 × 0x800 (`gconsts.h:355`). |

### (b) Draw target, matrix, transformation — 8 functions

| Function | Signature | Notes |
|---|---|---|
| `GDraw_SetDIBitmap` | `(ctx, pcBITMAPINFOHEADER, pBYTE, UINT32 Format16BPP=FORMAT16BPP_555)` | `gdraw.h:443`. Bottom-up DIB bitmap; `biCompression=0x80000001` marks 32bpp with a transparency channel (`grndrgn.cpp:7654`, `capturemanager.cpp:285`). |
| `GDraw_SetInvertedDIBitmap` | idem | For top-down DIBs. |
| `GDraw_SetMatrix` | `(ctx, pcGMATRIX)` | See §2.1. |
| `GDraw_MaxScale` | `(ctx, pcGMATRIX, pcRECT)` | Maximum scale of the matrix over a bbox (to choose flatness/DPI). |
| `GDraw_TransformPath` | `(ctx, pcPOINT in, pPOINT out, UINT32 len, pcGMATRIX)` | |
| `GDraw_ScrollBitmap` | `(ctx, INT32 x, INT32 y)` | Displacement of the destination bitmap: an incremental *scroll* optimisation. |
| `GDraw_SetFlatness` | `(ctx, UINT32)` | Bézier flattening tolerance, in document units (millipoints). |
| `GDraw_SetMiterLimit` | `(ctx, UINT32)` | |

### (c) Path filling and stroking — 12 functions

| Function | Signature | Notes |
|---|---|---|
| `GDraw_FillPath` | `(ctx, pcPOINT, pcBYTE types, UINT32 len, UINT32 Winding)` | `Winding`: 0 = even/odd (alternate), 1 = nonzero. The app: `(RR_WINDINGRULE()==EvenOddWinding)?0:1` (`grndrgn.cpp:2769`). Bit 1 (`<<1`) selects the **inverse** polygon (used by the beveller: `beveler.cpp:790`). |
| `GDraw_FillRectangle` | `(ctx, pcRECT)` | |
| `GDraw_FillPoint` | `(ctx, pcPOINT)` | A single pixel (editing blobs). |
| `GDraw_FillRegion` | `(ctx, pcREGION, pcPOINT offset)` | |
| `GDraw_StrokePath` | `(ctx, pts, types, len, bool Close, UINT32 LineWidth, DWORD Caps, DWORD Join, pcDashType)` | `gdraw.h:492`. Caps: BUTT/ROUND/SQUARE; Join: MITER/ROUND/BEVEL (`gconsts.h:166,172`). |
| `GDraw_StrokePathToPath` | `(ctx, in…, out…, …)` | **Converts the stroke into a filled outline.** The basis of "convert line to shape" and of contouring (`Kernel/paths.cpp:5646`). |
| `GDraw_CalcStrokeBBox` | `(ctx, …, pRECT, …)` | Exact bbox of the stroke (`Kernel/combshps.cpp:1461`, `nodetext.cpp:2891`). |
| `GDraw_HintPath` | `(ctx, pPOINT, pcBYTE, UINT32, bool, UINT32 LineWidth)` | Grid *hinting*: snaps horizontal/vertical runs to the pixel. |
| `GDraw_SetHintingFlag`, `GDraw_SetDashAdjustmentFlag` | `(ctx, bool)` | |
| `GDraw_IsOverlap`, `GDraw_IsStrokeOverlap` | | Hit-testing without drawing. |
| `GDraw_CalcBBox`, `GDraw_CalcSimpleBBox` | | Bbox with/without curve flattening. |
| `GDraw_GetStatistics` | `(ctx, path…, pSTATISTICS)` | Order 0/1/2 moments of colour and position over the filled area (`STATISTICS`, `gconsts.h:321`): sums R, R², RX, RY, … Used for the mean colour of a region. |

### (d) Regions and clipping — 7 functions

| Function | Notes |
|---|---|
| `GDraw_MakeRegion`, `GDraw_MakeUnclippedRegion` | Rasterise a path into a `REGION` (`gconsts.h:296`): `Type` 0 = no AA, 1 = 5-line AA, 2 = 9-line AA; plus a `RECT` and RLE data. |
| `GDraw_ClipRegion`, `GDraw_GetClipRegion` | Clipping by an arbitrary region (clip paths, ClipView). |
| `GDraw_ClipRectangle`, `GDraw_DeviceClipRectangle`, `GDraw_GetDeviceClipRectangle` | Rectangular clipping in document and in device coordinates. |

### (e) Solid colour, dither, palette — 12 functions

`GColour_SetColour` (COLORREF with dither), `GColour_SetSolidColour` (no dither, with a screen
format *hint*), `GColour_SetWordColour` (raw 32-bit value), `GColour_SetInvert` (XOR),
`GColour_SetDitherStyle` (8 styles, `gconsts.h:224`: diffusion, ordered, ordered grey,
Floyd-Steinberg, none, simple grey, grey diffusion, exact), `GColour_SelectPalette`,
`GColour_InitialiseWithPalette`, `GColour_SetConversionPalette`, `GColour_SetHalftoneOrigin`
(origin of the halftone pattern, readjusted on capture: `grndrgn.cpp:8739`),
`GColour_ReturnBrush`, `GColour_ReturnBrushRGB`, `GColour_SetGreyConversionValues`
(R, G, B luminance weights — **a key parameter of every blend mode**, §2.6).

### (f) Gradients — 16 functions

| Function | Description |
|---|---|
| `GColour_BuildGraduationTable` / `…32` | Builds the 256- (or 2048-) entry LUT between two colours; `HSVFlag` picks the interpolation space. The `…32` variant produces pure `COLORREF` (32 bpp); the plain one produces a `DitherBlock` of 4 DWORDs per entry (4×4 dither for 8/16 bpp screens). |
| `GColour_AddToGraduationTable` | Fills in **one** entry — this is how the app injects arbitrary multi-stop ramps and bias/gain ramp profiles (`Kernel/gradtbl.cpp:549`). |
| `GColour_SetGraduation` | `(ctx, Style, pcGraduationTable, A, B, C)` — 3 control points. |
| `GColour_SetGraduation4` | + `D` → a **perspective** gradient (quadrilateral). |
| `GColour_Set3WayGraduation(4)` | 3-colour mesh (barycentric interpolation). |
| `GColour_Set4WayGraduation(4)` | 4-colour mesh (bilinear). |
| `GColour_SetGourand` | Gouraud triangle. |
| `GColour_SetTransparentGraduation(4)`, `…3Way…`, `…4Way…` | The same five types but on the transparency channel. |
| `GColour_GetGraduationTableSize/Length` and the `Long`/`Transparent` variants | Table sizes (256 vs 2048 entries; see `Kernel/gradtbl.cpp:208`). |

`Style` encodes (as seen in `grndrgn.cpp:2480-2520` and `4150-4170`):
`bits 0-7` = shape (0 linear, 1 radial/elliptical, 2 conical, 3 square/diamond; for meshes
0/1/2 = simple 3-colour / simple 4-colour / tiled) `| 0x80` HQ repetition;
`bits 8-15` = transparency type (`TransparencyEnum`);
`bit 16` = repetition for transparencies.

### (g) Transparency — 5 functions + 36 styles

`GColour_SetTransparency(ctx, COLORREF rgbt, DWORD Style)` — the high byte of `rgbt` is the
transparency value (0 = opaque, 255 = transparent) and `Style` is a `TransparencyEnum`
(`gconsts.h:193`). `GColour_SetTransparencyLookupTable(ctx, pcBYTE)` installs the "type 12" LUT
(transparency by table). The graduated variants are configured with the functions in section
(f). Full table in §2.6.

### (h) Bitmap (tile) fills — 6 functions

`GColour_SetTilePattern(4)`, `GColour_SetTransparentTilePattern(4)`, `GBitmap_PlotTile(4)`.
Representative signature (`gdraw.h:351`):

`GColour_SetTilePattern` (`gdraw.h:351`) receives, besides the context, the following
parameters (and returns an integer status):

| Parameter | Type | Role |
|---|---|---|
| bitmap header | pointer to `BITMAPINFOHEADER` | dimensions and depth of the tile |
| pixels | pointer to bytes | tile data |
| style | 32-bit integer | repetition/sampling mode |
| A, B, C | points | mapping parallelogram (the "4" variant adds a point D) |
| default colour | `COLORREF` | colour outside the tile when it does not repeat |
| translation table | BGR table | global colour remapping |
| R, G, B tables | 3 tables of 256 bytes | per-channel correction (contone, separation) |
| transparency table | table of 256 bytes | transparency by index/value |
| tile offset | integer | phase offset of the tiling |

The four translation tables allow *contone*, colour correction and separation inside the
sampling itself. `GColour_SetTileSmoothingFlag` (bilinear interpolation) and
`GColour_SetTileFilteringFlag` (high-quality filtering for print/export) choose the resampling
(`grndrgn.cpp:3432`).

### (i) Bitmap processing / image effects — 15 `GBitmap_` functions

`SetBrightness(double)`, `SetContrast(double)`, `SetGamma(double)`, `SetPostGamma(double)`,
`SetSaturation(double)`, `SetContone(UINT32 style, COLORREF start, COLORREF end)`,
`SetBias(UINT32 channel, double)`, `SetGain(UINT32 channel, double)`,
`SetInputRange(channel, BYTE, BYTE)`, `SetOutputRange(channel, BYTE, BYTE)`,
`SetMaxFilterSize(UINT32)`, `Sharpen(INT32)`, `Blur(INT32)`, `PlotTile(4)`.

Channel `3` is the transparency channel: the app applies the graduated fill's bias/gain ramp
profile right there (`grndrgn.cpp:3730-3732` and `4313-4315`).

### (j) Colour conversion and separation — 8 functions

`GColour_ConvertBitmap(ctx, srcInfo, src, dstInfo, dst, Dither)` — the universal
depth/palette converter (used by `wxOil/dibconv.cpp` in 6 places and by the captures at
`grndrgn.cpp:8055, 8270, 8461, 8961`). `GColour_SetSeparationTables(cyan, magenta, yellow,
black, UCR, blackGeneration)` — CMYK separation with *under-colour removal* and *black
generation* as external LUTs. `GColour_SetBitmapConversionTable(pcBGR)` — colour correction
applied to bitmaps. `GColour_ConvertHSVtoRGB` / `GColour_ConvertRGBtoHSV` (contextless).
`GColour_SetMaxDiffusionError`, `GColour_ScaleBitmap` (see §1.5: **declared but not exported as
C**).

### (k) Bevel — 4 functions in `gdraw.h` + 6 in `gdraw2.h`

`GDraw_SetBevelContrast/Lightness/Darkness(ctx, UINT32)` and
`GDraw_TranslateBevelValue(ctx, BYTE index, BYTE colour)`: they control how the bevel index map
(8 bits) is turned into lighting. The second API (`GDraw/gdraw2.h`, **contextless**, global
state) generates that map:

| Function (`GDraw/gdraw2.h`) | Inputs | Role |
|---|---|---|
| `GDraw2_SetDIBitmap` | header + pixels of the destination bitmap, bevel style, two light angles (`float`) | sets the destination and the lighting parameters |
| `GDraw2_FillTriangle` | 3 points + normal (x, y) as `double` | fills a triangle of the normal map |
| `GDraw2_FillTrapezium` | 4 points + normal (x, y) as `double` | likewise for a trapezium |

They all return an integer status and operate on global state (they take no context).

15 bevel styles (`gdraw2.h:104`): FLAT, ROUND, HALFROUND, FRAME, MESA_1/2, SMOOTH_1/2,
POINT_1/2a/2b, RUFFLE_2a/2b/3a/3b. Real usage in `Kernel/beveler.cpp:756, 764, 844`.

### (l) Changed bbox (incremental render) — 3 functions

`GDraw_ClearChangedBBox`, `GDraw_GetChangedBBox`, `GDraw_SetChangedBBox`. CDraw **accumulates
the rectangle of pixels actually touched**; the app uses it to know what to blit
(`grndrgn.cpp:5958, 6533, 7703`) and to size the captures.

## 1.4 Library entry points not declared in `gdraw.h`

These four functions are global C++ (mangled) that the application declares **in its own
headers** and which nevertheless live inside `libCDraw.a`:

| Symbol (demangled) | Declared in | Used in | Function |
|---|---|---|---|
| `ClipPathToPath(POINT*,BYTE*,uint,double,POINT*,BYTE*,uint,double,uint,uint,POINT*,BYTE*,uint)` | `Kernel/gclip.h:108` and `:155` | `Kernel/pbecomea.cpp:285,305,314,411`, `Kernel/swfrndr.cpp:232` | **Path booleans** (AND, OR, EOR, NOT, intersection) with a tolerance; 8 styles via `CLIPPING_STYLE` (`Kernel/gclips.h:106`). |
| `GenerateWallShadow(...)` | `Kernel/bshadow2.h:113` | `Kernel/bshadow.cpp:526`, `Kernel/fthrconv.cpp:313` | **Blur of an 8 bpp mask** by circular convolution. The basis of shadows and *feathering*. |
| `GenerateFloorShadow(...)` | `Kernel/bshadow2.h:128` | `Kernel/bshadow.cpp:749` | Cast shadow with tilt/projection. |
| `ContourBitmap(BYTE*,uint,BYTE*,BITMAPINFO*,double,uint)` | `Kernel/bshadow2.h:141` | `Kernel/bshadow.cpp:1158,1517`, `nodeliveeffect.cpp` | Contouring/dilation of an 8 bpp mask. |

In addition, the library contains `G3D_*` (the 3D extrusion engine: `G3D_DefineView`,
`G3D_AddTriangleToView`, `G3D_PlotView`, `G3D_GetViewBBox`, `G3D_SetTruePerspectiveFlag`,
`G3D_AddFlatTriangleToView`) **only as mangled C++ symbols**, while `gdraw.h` declares them
`extern "C"` inside a commented-out block: they are unreachable from the app as it stands.

## 1.5 Header ↔ binary discrepancies

The 126 exported `extern "C"` functions are **all** documented in the headers (there is no
hidden C API). Conversely, 28 header declarations have no C symbol:

* 14 `G3D_*` — a commented-out block in `gdraw.h:638-720` (e.g. `gdraw.h:649`), present mangled.
* `GDraw_FillTriangle`, `GDraw_InitialiseFillPath`, `GDraw_FillPathLine`, `GDraw_DoFillPath`,
  `GDraw_SaveContext/RestoreContext/SwapContext`, `GColour_Initialise`,
  `GColour_SetMultiRadial`, `GColour_SetSupersampleRate`, `GDraw2_SetMemoryHandlers`,
  `GDraw2_Terminate` — commented out in the header; some exist mangled
  (`GDraw_FillTriangle(GDraw*,const POINT*,const POINT*,const POINT*,uint)`).
* **`GColour_ScaleBitmap` and `GColour_SetMaxDiffusionError` are declared live in `gdraw.h`
  but only exist mangled** (`_Z19GColour_ScaleBitmapP5GDraw…`): using them would produce a link
  error. They are, in fact, dead functionality in Xara LX.
* `GColour_SetMultiRadial` (multi-radial fill with "blobs") was never published: there is a
  trace of the `GBLOB` structure commented out in `gconsts.h:340`.

## 1.6 What the 640 internal methods reveal (a map of the implementation)

Demangling the internal symbols describes the rasteriser's complete architecture and is the
best available specification of the engine. Grouped:

| Group | Representative symbols | What it implies |
|---|---|---|
| Edge table | `GenEdgeTable`, `GenEdge`, `GenFastEdge`, `GenCurvedEdge(s)`, `ExtendEdgeTable`, `SortYEdges`, `SortOpenEdges`, `SortClosedEdges`, `TidyEdgeTable`, `ProcessEdges` | **Classic scanline rasterisation with an active edge table** (not *tile-based*). |
| Fill by rule | `FillWindingPolygon`, `FillAlternatePolygon`, `FillInverseWindingPolygon`, `FillInverseAlternatePolygon`, `FillAntialiasedPolygon`, `FillBevelPolygon` | 4 rules (nonzero / evenodd × normal / inverse) + a separate AA path + a bevel path. |
| Antialiasing | `AddScanline5x5`, `AddScanline17x5`, `AddScanline11x11`, `AddScanline12x11`, `ClipLineAA5/11/12/17`, `ClearScanlineAA`, `GenAntialiasedRegion` | *Scanline* supersampling (§2.3). |
| Curves | `FlattenCurve`, `DoFlattenCurve`, `Flatten`, `FlattenSplit`, `FlattenCCurve`, `CalcCurveLength`, `CBezier::Length` | Adaptive flattening by subdivision. |
| Stroke | `DoStrokePath`, `StrokeLine`, `StrokeLineAndJoin`, `GenRoundCap`, `BBoxRoundCap`, `GenDashSection`, `MakeDashAdjustment`, `StrokeCurve_cbrt` | Its own geometric stroking (offsetting with a cube root for the curve error). |
| Pixel/blit per format | `Pixel1/4/8/16/24/32`, `PixelCMYK`, `PixelT32`, and the `…C` (dither), `…_F` (filtered), `…T` (transparency), `…_X` (XOR) variants | ~120 pixel-access routines: 1, 4, 8, 16 (555/565/655/664), 24, 32 bpp and **native CMYK**. |
| Gradients | `LinearGradBlitLine`, `RLinearGrad…` (repeated), `RadialGradBlitLine(Start/End)`, `RRadialGrad…`, `SquareGrad…`, `RSquareGrad…`, `ConicalGradBlitLine{Start,End,Left,PosMid,NegMid}`, `…Grad4…` (perspective), `MGradX3/X34`, `DGradX3`, `GradX4/X44` | One specialised *blitter* per (shape × repetition × perspective × with/without transparency). |
| Transparency/blending | `CalcTransparency{A,B,C,D,H,L,Lu,S,Sn}[_T]`, `MergeTransparency…`, `Merge{,S,X,A,Bvl,Bvl2}Transparent[Grad]32[T]`, `MergeTransparencyMask` | 12 blend families × (flat, variable, graduated, bitmap) × (with/without a destination alpha channel). |
| Bevel | `FlatBevelBlitLine32N/P`, `RoundBevel…`, `MesaBevel…`, `SmoothBevel…`, `Ruffle2Bevel…`, `Ruffle3Bevel…`, `SimpleBevel…`, `GenBevel`, `TranslateBevelValue` | One blitter per bevel profile, with an N/P branch according to the sign of the lighting. |
| Static tables | `aContrastTable`, `aSaturationTable1/2`, `aChopTable`, `aDiv3Table`, `aSquareTable`, `aArcTable`, `aArcCosTable`, `a{Round,Mesa1,Mesa2,Smooth1,Smooth2,HalfRound,Ruffle2,Ruffle3}BevelTable`, `apMulTable`, `apIMulTable`, dither tables for 4/8/16 bpp | **Everything is resolved with precomputed LUTs**, not with per-pixel arithmetic. |
| Conversion | `ConvertBitmap32to{1,4,8,16,24,32}_{NoDither,Dither,Diffuse,Floyd,GreyDither}`, `Floyd_LR/RL`, `ConvertLineRGBto16_5xx` | A complete matrix of converters. |
| Auxiliary geometry | `GenBevelFaces::BevelPath`, `GenPathContour::ContourPath`, `ClipPathToPath`, `GenClipPath`, `GenClipLines`, `Intersect` | Path contouring and booleans. |
| Shadows | `GenerateWallShadow`, `GenerateFloorShadow`, `ContourBitmap`, `Blur`, `Sharpen`, `BlurBitmapSetup`, `BlurTransparencyBitmapSetup`, `CalcFilter`, `CalcFilterD` | Live effects. |

---

# 2. Task B — Render semantics to replicate

## 2.1 Coordinates, matrix and precision

The document lives in **millipoints** (`MILLIPOINT` = 1/1000 pt;
`MILLIPOINTS_PER_INCH = 72000`), signed 32-bit integers. A path is a pair of Win32-style
arrays: `POINT[]` (x, y as `INT32`) and a `BYTE[]` of verbs `PT_MOVETO=6`, `PT_LINETO=2`,
`PT_BEZIERTO=4`, `| PT_CLOSEFIGURE=1` (`gconsts.h:106`).

The document→device matrix is:

| Field | Width | Meaning |
|---|---|---|
| `AX`, `AY`, `BX`, `BY` | signed 32-bit integer | linear part (a, b, c, d) |
| `CX`, `CY` | signed 64-bit integer | translation (e, f) |

Declared in `gconsts.h:310`. The fractional scale constant is `FX = 14` (`gconsts.h:354`).

Actual construction (`wxOil/grndrgn.cpp:5297-5340`):

The procedure, described step by step:

1. Compute an integer multiplier `Mult = round(dpi · 2^FX)`, where `dpi` is the device's
   pixels per inch and `FX = 14`.
2. For each of the four linear terms (a, b, c, d), taken as integers in 16.16 fixed point:
   multiply by `Mult` in 64-bit arithmetic and divide by 72,000 (millipoints per inch).
   Result: **2.30** fixed point (16 bits of the original fraction + 14 from `FX`).
3. For the translation (e, f): multiply the displacement in pixels by `2^(FX+16)` in 64 bits
   and **change the sign**. Result: 64 bits with **30 fractional bits**.

→ **a, b, c, d in 2.30 fixed point** (16 bits of `FIXED16` + `FX`=14) and **e, f in 64 bits with
30 fractional bits**. At 96 dpi, `AX = 2^30 · 96/72000 = 1,431,655`. The code comment
(`grndrgn.cpp:5290`) warns that CDraw **truncates** rather than rounding, and the app
compensates by adding half a pixel. This subtlety is the cause of 1-pixel misalignments when
comparing outputs: the new engine's rounding rule must be chosen deliberately.

Consequences for the design: the scene is defined in 32-bit integers (millipoints), and device
space uses 2.30 fixed point. **An `f32` (24 bits of mantissa) does not cover the millipoint
range of a large document** (±2^31 mp ≈ ±29,800 m): see §3.7.

## 2.2 Rasterisation: the scan model

Deduced from the internal symbols and from `Kernel/gclips.h` (which documents the shared
structures `Edge`, `Curve`, `Strip`):

1. **Flattening**: curves are recursively subdivided down to the `Flatness` tolerance (in
   document units). The app sets `Flatness = (72000/dpi)/2 / scale`, and **divides it
   by 5 when antialiasing is on** (`grndrgn.cpp:5626-5631`): half a pixel without AA, one tenth
   with it.
2. **Edge table**: `GenEdgeTable` produces edges with `XS,YS,XE,YE`, a slope `DX` as a `double`,
   flags and, for curves, a pointer to the `Curve` plus start/end `t` parameters
   (`Kernel/gclips.h:127`). In other words: **curves are not always flattened; the clipper keeps
   the parametric curve** so that it can return exact Béziers from the boolean operations.
3. **Scan**: `SortYEdges` → `ProcessEdges` → `GenFilledStrips`/`GenLineStrips`. Lines are
   emitted to a *blitter* (`SolidBlitLine32`, `LinearGradBlitLine`, `TransparencyBlitLine32`, …)
   selected by the active style (`InitialiseStyle`, `STYLE`, `BlitSetup`).
4. **Fill rule**: `FillWindingPolygon` (nonzero) and `FillAlternatePolygon` (even/odd), plus the
   *inverse* variants (which fill the complement). The `Winding` parameter of `GDraw_FillPath`
   is `bit0` = nonzero, `bit1` = inverse.

## 2.3 Antialiasing

CDraw does **supersampling by sub-scanlines with quantised horizontal coverage**, not
*analytic coverage*:

| Routine | Sub-scanlines (V) | Horizontal sub-positions (H) | Weight per sub-scanline |
|---|---|---|---|
| `AddScanline5x5` | 5 | 5 | 5 |
| `AddScanline17x5` | 5 | 17 | 51 (= 255/5) |
| `AddScanline11x11` | 11 | 11 | 23 (≈ 255/11) |
| `AddScanline12x11` | 11 | 12 | 24 |

(the weights are `…::Word` constants in the `.rodata` of `libCDraw_la-GScanAA.o`; that object's
main LUT starts with `0x33,0x30,0x2d,…` = 51, 48, 45 …, that is 17 steps of 3 = 51, confirming
the 17×5 scheme).

* `GDraw_SetAntialiasFlag(bool)` turns AA on and off.
* `GDraw_SetAntialiasQualityFlag(bool)` switches between the normal mode (5 lines) and the
  high-quality one (11 lines); the app exposes it as the `HighQualityAA` preference
  (`grndrgn.cpp:313, 5622`).
* Regions have AA too: `REGION::Type` 0 = no AA, 1 = 5 lines, 2 = 9 lines (`gconsts.h:296`),
  with `ClipLineAA5/11/12/17` for clipping with coverage.
* The final coverage is a byte 0..255, applied as alpha in the *merge*.

**Implication for Rust:** 5×17 = 85 effective levels (normal) and 11×12 = 132 (high quality).
A modern analytic coverage rasteriser (vello, tiny-skia) is **strictly better**, so the goal is
not to replicate the artefact but to match or exceed the quality; we must accept that the output
will not be bit-exact (see §3.8, validation strategy).

## 2.4 Render quality levels

`Kernel/quality.h:100` defines a 0..110 scalar (`QUALITY_GUIDELAYER = -1`, default 100) from
which five axes are derived (`Kernel/quality.cpp:157-255`):

| Axis | Thresholds |
|---|---|
| Line | `>50` FullLine · `>30` ThinLine · otherwise BlackLine |
| Fill | `>=60` Graduated · `>30` Solid · `>10` Bitmaps · otherwise NoFill |
| Blend | `<=20` endpoints only · otherwise complete |
| **Antialiasing** | `<=100` no · `>100` yes |
| Transparency | always "NoTransparency" (axis unused) |

AA is only enabled above 100, and the default quality is exactly 100: in Xara LX
**antialiasing is off by default** and the tools raise the quality to 110.

## 2.5 Banding, captures and bitmap cache

**Banding** (`wxOil/grnddib.cpp:409-600`): if the "tuned" memory available is not enough for the
whole bitmap, the region is split into horizontal bands of
`MaxScanLines = RAM / ScanlineSize` (16 lines being the useful minimum);
`SetFirstBand`/`GetNextBand` walk the bands, re-rendering the scene with a different
`CurrentClipRect`.

**Captures** (`Kernel/capturemanager.h:131-215`): the *render to an intermediate bitmap*
mechanism used for transparent groups, live effects, shadows and bevels. Types: `ctNESTABLE`,
`ctREUSE`, `ctRESTART`; 13 flags, among them `cfGRABRENDERED` (reuses the parent bitmap),
`cfDIRECT`/`cfALLOWDIRECT` (a child node **hands over** its already-computed bitmap — this is
the per-node render cache), `cfFULLCOVERAGE`, `cfLOCKEDTRANSPARENT`, `cfQUALITYNORMAL`.

**Changed bbox**: CDraw accumulates the rectangle touched (§1.3.l) and the app uses it to blit
only what changed and to crop the capture to the real area.

**Scroll**: `GDraw_ScrollBitmap(ctx, dx, dy)` moves the contents of the destination bitmap so
that valid pixels can be reused when *panning*, drawing only the new strip.

## 2.6 Gradients: the exact mathematics

### 2.6.1 Shapes

| `GradEnum` | Points | Geometry |
|---|---|---|
| `GRAD_LINEAR` (0) | A(start), B, C | Parameter `s = ((P−A)·u)/‖u‖²` with `u = C−A`; B is the perpendicular axis that makes "skewed" gradients possible. The app reorders: `A=Start, B=EndPoint2, C=End` (`grndrgn.cpp:2524`). |
| `GRAD_RADIAL` (1) | A(centre), B, C | Elliptical: two independent radii ⇒ `s = ‖M⁻¹(P−A)‖`. |
| `GRAD_CONICAL` (2) | A(centre), B, C | Angular: `s = atan2` normalised. The app mirrors B about A (`grndrgn.cpp:2532`). Implemented with `CalcConicalGrad`, `aArcTable` and per-quadrant blitters (`ConicalGradBlitLine{Left,PosMid,NegMid,Start,End}`). |
| `GRAD_SQUARE` (3) | A, B, C | Diamond: `s = max(‖u‖∞, ‖v‖∞)` (the L∞ metric in the A,B,C frame). |
| `GRAD_3COLOUR` | A, B, D + 3 colours | Barycentric mesh. |
| `GRAD_4COLOUR` | A, B, C, D + 4 colours | Bilinear mesh. |

Each shape additionally has a **repeated** variant (`RLinearGrad…`, `RRadialGrad…`,
`RSquareGrad…`: `s` taken modulo, with or without mirroring) and a **perspective** variant
(`…Grad4…`: a fourth point D is passed and the interpolation is projective, validated by
`MouldPerspective::WillBeValid`).

### 2.6.2 The gradient table

| Structure | Fields | Use |
|---|---|---|
| `GraduationTable` (`gconsts.h:248`) | length (`u32`), start colour, end colour, 256 entries of type `DitherBlock` | destinations below 32 bpp |
| `GraduationTable32` | length (`u32`), start colour, end colour, 256 colours | 32 bpp destinations |
| `TransparentGradTable` (`gconsts.h:262`) | 256 bytes | transparency ramp |
| `DitherBlock` | 4 words of 32 bits | precomputed 4×4 dither pattern for one colour |

`Length` may be 256 or **2048** ("long" tables, `LargeGradTables`, `Kernel/gradtbl.cpp:262`) to
avoid banding in large gradients. For destinations below 32 bpp each entry is not a colour but a
`DitherBlock` of 4 DWORDs = the precomputed 4×4 dither pattern for that colour.

The ramp is built **in the application** (`Kernel/gradtbl.cpp`), not in CDraw, except for the
simple interpolation:

* `EFFECT_RGB` → linear interpolation in RGB.
* `EFFECT_HSV_SHORT` / `EFFECT_HSV_LONG` → interpolation in HSV along the short or the long way
  round the hue (`gradtype.h:105`); the app forces RGB if spot-ink colours are involved
  (`gradtbl.cpp:453`).
* Multi-stop ramps (`ColourRamp`) → filled in stretch by stretch with `AddToGraduationTable`.
* With separation/colour correction active, **every** entry goes through the `ColourContext`
  (`gradtbl.cpp:549`) — slow but exact.

### 2.6.3 Ramp profile (bias/gain) — exact formulas

`CProfileBiasGain` (`Kernel/biasgain.cpp`) is Xara's "profile" (the distribution curve of the
gradient, the blend, the contour and the shadow). It is the classic Schlick bias/gain:

User parameters `B, G ∈ [−1, +1]`, converted to `b, g ∈ (0,1)` with (`biasgain.cpp:516`,
`ε = 1e-5`):

```
b = (B + 1)·(0.5 − ε) + ε
```

Kernel (`biasgain.cpp:592` and `616`):

```
bias(b, x) = x·b / ( (1 − 2b)·(1 − x) + b )

           ⎧ x·g / (C + g)                 if x < 0.5
gain(g, x) =⎨                                              with C = (1 − 2g)·(1 − 2x)
           ⎩ (C − x·g) / (C − g)           if x ≥ 0.5

profile(x) = gain( g, bias(b, x) )          (biasgain.cpp:572)
```

With `B = G = 0` ⇒ `b = g = 0.5` ⇒ the identity (the code even short-circuits that case,
`biasgain.cpp:341`). Application to the ramp (`gradtbl.cpp:1206`, transparency):

```text
# Building the ramp with a profile (equivalent to gradtbl.cpp:1206)
domain of the profile := [0, Length]
for i in 0 .. Length-1:
    f        := profile(i / Length)          # bias/gain ramp profile normalised to 0..1
    Table[i] := lerp(Start, End, f)          # linear interpolation of the colour/value
```

Transparency interpolation without a profile, in fixed point (`gradtbl.cpp:1562`):

```text
# Linear interpolation in fixed point with 22 fractional bits (gradtbl.cpp:1562)
t   := (start << 22) + 2^21          # the added 2^21 is equivalent to +0.5: round to nearest
inc := ((end - start) << 22) / (endIdx - startIdx)
for each i of the stretch:
    Table[i] := (t >> 22) AND 0xFF
    t        := t + inc
```

## 2.7 Transparency and blend modes

### 2.7.1 The complete enumeration

`TransparencyEnum` (`gconsts.h:193`) — 36 values. The three variants of each family indicate how
the transparency value arrives: **generic**, **flat** (`T_FLAT_*`, a constant value) and
**graduated** (`T_GRAD_*`, from the gradient/bitmap blitter):

| # | Value | Family | Xara name (UI) |
|---|---|---|---|
| 0 | `T_NONE` | — | None |
| 1,4,7 | `T_REFLECTIVE`, `T_FLAT_REFLECTIVE`, `T_GRAD_REFLECTIVE` | Mix | **Mix** (normal alpha) |
| 2,5,8 | `T_SUBTRACTIVE`, `T_FLAT_SUBTRACTIVE`, `T_GRAD_SUBTRACTIVE` | Multiply | **Stained Glass** |
| 3,6,9 | `T_ADDITIVE`, `T_FLAT_ADDITIVE`, `T_GRAD_ADDITIVE` | Screen | **Bleach** |
| 10,11,12 | `T_SPECIAL_1/2/3` | — | Reserved (12 uses `SetTransparencyLookupTable`) |
| 13,14,15 | `T_CONTRAST`, `T_FLAT_CONTRAST`, `T_GRAD_CONTRAST` | Contrast | **Contrast** |
| 16,17,18 | `T_SATURATION`, … | Saturation | **Saturation** |
| 19,20,21 | `T_DARKEN`, … | Darken | **Darken** |
| 22,23,24 | `T_LIGHTEN`, … | Lighten | **Lighten** |
| 25,26,27 | `T_BRIGHTNESS`, … | Brightness | **Brightness** |
| 28,29,30 | `T_LUMINOSITY`, … | Luminosity | **Luminosity** |
| 31,32,33 | `T_HUE`, … | Hue | **Hue** |
| 34,35,36 | `T_BEVEL`, `T_FLAT_BEVEL`, `T_GRAD_BEVEL` | Bevel | Internal (bevel lighting) |

The document model uses its own `TranspType` (`Kernel/fillval.h:144`) with
`TT_Mix=1, TT_StainGlass=2, TT_Bleach=3` and then the GDraw values from 13 onwards; the
translation is in `GRenderRegion::MapTranspTypeToGDraw` (`wxOil/grndrgn.cpp:8844`):

The translation is a simple change of base between two contiguous numberings, in two stretches:

* **Classic stretch** (`TT_Mix`, `TT_StainGlass`, `TT_Bleach`): the value of `TT_Mix` is
  subtracted to obtain the index 0, 1 or 2 within the family, and the corresponding GDraw base
  is added — `T_FLAT_REFLECTIVE` if the transparency is flat, `T_GRAD_REFLECTIVE` if it is
  graduated.
* **Blend-mode stretch** (from `TT_CONTRAST` to `TT_BEVEL`): the same, subtracting the base
  `TT_CONTRAST` and adding `T_FLAT_CONTRAST` or `T_GRAD_CONTRAST`.

Any other value falls outside both stretches and has no equivalent.

Value convention: **0 = opaque, 255 = fully transparent** (the inverse of the usual alpha).

### 2.7.2 Blending formulas (derived from the disassembly)

Notation: `s` = source colour (BGR), `d` = destination colour, `t` = transparency 0..255,
`Wr,Wg,Wb` = the context's luminance weight tables (`GColour_SetGreyConversionValues`, at
`ctx+0x610f0/0x614f0/0x618f0`, 256 × u32 each), and

```
Y(c)      = (Wr[c.r] + Wg[c.g] + Wb[c.b]) >> 24        // luminance 0..255
Mul[a][b] = a·b / 255                                   // GDraw::apMulTable
IMul[a][b]= (255−a)·b / 255                             // GDraw::apIMulTable
```

Each family is a function `(Y(s), t) → level L`, followed by a 256-entry LUT applied to every
channel of the destination. Verified instruction by instruction:

**Darken** (`libCDraw_la-GScanT-D.o`, `GDraw::CalcTransparencyD`):

```
L        = Y(s) + IMul[Y(s)][t]  =  Y(s) + (255 − Y(s))·t/255
out_c    = Mul[L][d_c]           =  d_c · L / 255
```
→ multiplies the destination by the luminance of the source, faded towards 1 by `t`.

**Lighten** (`GScanT-L.o`, `CalcTransparencyL`):

```
L        = IMul[t][Y(s)]         =  Y(s)·(255 − t)/255
out_c    = L + IMul[L][d_c]      =  L + d_c·(255 − L)/255        // screen
```

**Brightness** (`GScanT-B.o`, `CalcTransparencyB`): the same as Lighten/Darken but the level is
computed over the **signed** value `v = (ΣW)>>23` (≈ `2·Y − 255`), choosing the `screen` branch
if `v ≥ 0` and `multiply` if `v < 0`; `L = IMul[t][|v|]`.

**Contrast** (`GScanT-C.o`): `v = ±(2Y−255)`, `L = t + IMul[t][v]`, and the output is taken from
the static LUT **`aContrastTable`** indexed by `L` and by the **centred** destination
(`d_c − 128`), adding 128 to the result: it is a parametric S-curve, not a closed formula.

**Saturation** (`GScanT-Sn.o`, `CalcTransparencySn`):

```
k      = t·aSaturationTable2[Y(s)] + aSaturationTable1[Y(s)]      // u32
g      = Y(d)                                                      // luminance of the destination
out_c  = aChopTable[ g + (((d_c − g)·k) >> 21) ]                   // aChopTable clamps −1024..1023 → 0..255
```
→ scales the destination's chrominance around its own luminance.

**Luminosity** (`GScanT-Lu.o`, `CalcTransparencyLu`):

```
target = IMul[t][Y(s)]
m      = max(d.b, d.g, d.r)
if m == 0:  out = (target, target, target)
otherwise:  k     = target·Recip[m] + Off[t]          // Recip[] ≈ 2^24/m, Off[] a table indexed by t
            out_c = (d_c·k + 0x800000) >> 24
```
→ rescales the destination so that its maximum channel reaches the source's luminance
(preserving hue and saturation).

**Hue** (`GScanT-H.o`, `CalcTransparencyH`): converts source and destination with
`GDraw::ConvertRGBtoHSV`, interpolates the H channel with `Mul`/`IMul` according to `t`, and
converts back with `ConvertHSVtoRGB` (the same functions that the public API
`GColour_ConvertHSVtoRGB/RGBtoHSV` exports).

**Mix / Stained Glass / Bleach** (`GScanT-R.o`, `GScanT-S.o`, `GScanT-A.o`): they use only
`apMulTable`/`apIMulTable` and `aChopTable`/`aDiv3Table`; they correspond to

```
Mix            out_c = IMul[t][s_c] + Mul[t][d_c]        = s_c(255−t)/255 + d_c·t/255
Stained Glass  out_c = d_c · (255 − IMul[t][255 − s_c]) / 255      ≈ multiply faded by t
Bleach         out_c = 255 − (255 − d_c)·(255 − IMul[t][s_c])/255  ≈ screen faded by t
```

(in the `_T` variant with a destination alpha channel, Stained Glass adds a term with
`aDiv3Table[r+g+b]`, the channel mean, to preserve the accumulated alpha).

**Bevel** (`GScanT-Bvl.o`): the "source colour" is really an 8-bit **bevel index** produced by
GDraw2; `SetBevelContrast/Lightness/Darkness` parameterise two LUTs (`.rodata` +
`.rodata+0x100`) that are combined with `Mul`/`IMul` to lighten or darken the destination.

> **Design conclusion**: the 12 families are all "*(a scalar derived from the source) → a tone
> LUT applied to the destination*". That maps **exactly** onto a 2D 256×256 R8 LUT texture per
> family on the GPU (§3.4), with no branches and no transcendental functions.

### 2.7.3 Graduated and bitmap transparency

The same 12 families are combined with the five gradient shapes
(`GColour_SetTransparentGraduation`, `…4`, `…3Way…`, `…4Way…`) and with bitmaps
(`GColour_SetTransparentTilePattern`), which supply the per-pixel `t` value. The table is
`TransparentGradTable` (256 bytes) and it accepts the same bias/gain ramp profile
(`gradtbl.cpp:1206`) and multi-stop ramps (`TransparencyRamp`, `gradtbl.cpp:1251`).

## 2.8 Bitmap and fractal fills

**Bitmap/tile** (`wxOil/grndrgn.cpp:3453-3800`): a parallelogram A,B,C (or a quadrilateral A..D
in perspective) is defined, and the repetition type comes from `FillMappingAttribute::Repeat`
(`RepeatType`, `Kernel/fillval.h:132`): 1 simple, 2 repeated, 3 repeated inverted (mirrored),
4 repeated high quality.

**Orientation.** The fill's `StartPoint`, `EndPoint`, `EndPoint2` are passed as the
parallelogram's first three corners (`grndrgn.cpp:3501-3521`; with perspective the
four are Start, End, `EndPoint3`, `EndPoint2`), and the plotter maps the first corner
to the first stored row of the bottom-up DIB: a plain bitmap plot passes a rectangle's
low (bottom) corner first (`grndrgn.cpp:4862-4865`). So `StartPoint` is the image's
**bottom-left** corner, `EndPoint` its bottom-right and `EndPoint2` its top-left,
which is also how a bitmap object becomes a fill (`Kernel/nodebmp.cpp:1210-1212`:
parallelogram corner 3 → start, 2 → end, 0 → second end). They are combined with:

* `SetTileSmoothingFlag` (bilinear smoothing when there is rotation/scaling) and
  `SetTileFilteringFlag` (maximum-quality filter when printing/exporting), decided by
  `NeedToSmooth()` and by whether printing is under way (`grndrgn.cpp:3400-3433`).
* *Contone* (`GBitmap_SetContone(style, rgbStart, rgbEnd)`, `grndrgn.cpp:3725`): remaps a bitmap
  onto a two-colour ramp, with the `FillEffect` (RGB / HSV short / HSV long) as the style.
* Bias/gain ramp profile on channel 3 (transparency) and `SetOutputRange(3, start, end)` to
  limit the transparency range (`grndrgn.cpp:3730-3732`, `4313-4315`).

**Fractal (plasma/clouds)**: `FILLSHAPE_CLOUDS = 9`, `FILLSHAPE_PLASMA = 10`
(`Kernel/fillval.h:129`). **CDraw does not generate it**: `Kernel/fracfill.cpp` implements a
recursive *midpoint displacement* (diamond-square), `PlasmaFractalFill::SubDivide`
(`fracfill.cpp:290`), with the parameters `Seed`, `Tileable`, `Squash`, `Graininess` (0..32) and
`Gravity` (0..255):

```text
# Adjusting the displacement at each subdivision level (fracfill.cpp:213-262)
# 'potential' is the pseudorandom value of the midpoint before scaling.
noise       := (graininess · (potential >> 17)) >> level       # amplitude halved per level
attraction  := gravity >> (2 · level)                          # bias towards the centre
displacement := noise - attraction
```

The result is a bitmap painted with the normal tiling machinery. The Perlin noise
(`Kernel/noise1.cpp`, `noisef.cpp`) is application code too.

### 2.8.1 Clouds (`FILLSHAPE_CLOUDS`): the complete generator

Everything below is readable kernel source (XARA-T-0336); nothing of it is in
CDraw. The facts are restated as a specification; Xarast's `xarast-image`
`procedural` module is written from this section.

**Lattice and bitmap.** The fractal lives on a virtual square lattice of side
L = 2²⁸ (`MAX_FRACTAL_COORD`, `fracfill.cpp:127`). The bitmap is a square of
side D, a power of two, 8 bits per pixel, an ascending grey palette; pixel
(px, py) is lattice point (px · 2ᴱ, py · 2ᴱ) with E = 28 − log₂ D
(`DoFill`, `fracfill.cpp:582-600`). Pixel row 0 is the bitmap's first stored
row, which the plotter puts at the fill's `StartPoint` (§2.8 "Orientation"):
x runs towards `EndPoint`, y towards `EndPoint2`.

**Size D** (`FillGeometryAttribute::GenerateFractalBitmap`,
`fillval.cpp:389-505`). `dpi` 0 means 96 (`AttrFillGeometry::FractalDPI`,
`fillattr.cpp:287`); above 72000 it is 72000. A pixel is p = ⌊72000 / dpi⌋
millipoints. w = ⌊|Start − End|⌋ / p and h = ⌊|Start − End2|⌋ / p in integer
division (distances truncated to whole millipoints), each 0 becoming 2.
D is the smallest power of two ≥ 4 that is ≥ both w and h
(`GetDimension`, `fracfill.cpp:533-539`), then capped at **256**
(`AttrFillGeometry::MaxFractalSize`, `fillattr.cpp:288`). A fill whose
`Start` equals `End` or `End2` is not generated (`RecalcFractal`,
`fillval.cpp:3358-3362`). The bitmap's own resolution is recomputed from D
as D · 72000 / max(w, h in millipoints) (`fillval.cpp:490`); the corpus'
stored `dpi` values (20 to 2487, varying with the fill's size) look like
that value, so reloading gives D back, but the path from the bitmap to the
attribute was not traced.

**Integer parameters** (constructor, `fracfill.cpp:441-490`). The maximum
potential M = 65535. Graininess and gravity arrive as 16.16 fixed point:

| Name | Value |
|---|---|
| G (both axes) | ⌊graininess · 8 + 0.5⌋, clamped to 0..0xffff |
| Γ (both axes) | ⌊gravity · M⌋ |
| squash > 0 | x axis only: Γₓ = ⌊gravity · M / squash⌋, Gₓ = ⌊(graininess · 8 + 0.5) / squash⌋ (16.16 arithmetic, floored) |
| squash < 0 | the same on the y axis with −squash |
| corners forced to zero | gravity ≠ 0 |
| coordinate mask K | 2²⁸ − 1 when tileable, else all ones |

**PRNG** (`DoRand`, `fracfill.cpp:180-184`; described in the source as the
generator of the ARM data manual, p. 2-51). A 33-bit state: a 32-bit word s
and a carry bit c. One step, in 32-bit unsigned arithmetic:
t = (s ≫ 1) ⊕ (c ≪ 31); c = s & 1; t = t ⊕ (s ≪ 12); s = t ⊕ (t ≫ 20).
The output is s. Seeding sets s = seed, c = 1 (`SetSeed`, `fracfill.cpp:147-151`).

**Corners** (`DoFill`, `fracfill.cpp:616-660`). Four outputs give
rᵢ = (((out ≫ 16) · M) + 2¹⁵) ≫ 16. If tileable and gravity = 0, r₀ = 0.
Otherwise a contrast stretch: d₁ = |r₀ − r₂|, d₂ = |r₁ − r₃|; if d₁ < d₂ the
pair is (0, 2), else (1, 3); a is the index of the pair's lower value, b the
other; e = (M − d₁) − d₂; if e > 0, r_a −= e / 2 and r_b += e / 2 (truncating).
The root square (0, 0)–(L, L) then starts with corner potentials, in the
order bottom-left (0,0), top-left (0,L), top-right (L,L), bottom-right (L,0):
all 0 when gravity ≠ 0; all r₀ when tileable; else r₀, r₁, r₂, r₃.

**Subdivision** (`SubDivide`, `fracfill.cpp:290-404`) of the square
(x₁,y₁)–(x₂,y₂) with corner potentials p₁₁ (x₁,y₁), p₁₂ (x₁,y₂),
p₂₂ (x₂,y₂), p₂₁ (x₂,y₁) and a "plot" flag:

1. With pixel coordinates v ≫ E: if the flag is set and (x₁, y₁) is inside
   the bitmap, that pixel becomes 255 − (p₁₁ ≫ 8).
2. If the square is under two pixels on both sides, stop.
3. The level ℓ goes up by one; xₕ = (x₁ + x₂) ≫ 1, yₕ likewise.
4. Edge midpoints, in this order, with coordinates masked by K: bottom
   A(p₁₁, p₂₁, xₕ, y₁; Gₓ, Γₓ); right A(p₂₁, p₂₂, x₂, yₕ; G_y, Γ_y);
   top A(p₁₂, p₂₂, xₕ, y₂; Gₓ, Γₓ); left A(p₁₁, p₁₂, x₁, yₕ; G_y, Γ_y).
5. Centre = (sum of the four + 2) ≫ 2. At ℓ = 1 only: M when gravity ≠ 0,
   else M − p₁₁ when tileable.
6. Recurse into bottom-left (flag clear: its corner is already drawn),
   bottom-right, top-right, top-left (flag set), each with its own corners;
   then ℓ goes down by one.

Every pixel is written exactly once, as the bottom-left corner of a
one-pixel square.

**Midpoint A(pa, pb, x, y; G, Γ)** at level ℓ (`Adjust`,
`fracfill.cpp:213-262`), in 32-bit arithmetic that wraps:

1. h = pa ⊕ (pb ≪ 16) ⊕ rotl(x, 16) ⊕ y ⊕ seed ⊕ 0xABCD1234.
2. One PRNG step on (s = h, c = 1).
3. r = (x ⊕ x≫5 ⊕ x≫10 ⊕ x≫15 ⊕ x≫20 ⊕ x≫25 ⊕ x≫30 ⊕ y ⊕ y≫5 ⊕ … ⊕ y≫30) & 31;
   h = rotl(h, r).
4. A second PRNG step, carrying c over from the first.
5. If squash ≠ 0 and x = y = 0, h = 0.
6. v = ((G · (h ≫ₛ 17)) ≫ₛ ℓ) − (Γ ≫ₛ 2ℓ) + ((pa + pb + 1) ≫ 1), with ≫ₛ
   an arithmetic shift of the signed word; clamp v to 0..M.

Edge cases the source leaves to the compiler: a rotation by 0 is the
identity; Γ ≫ 2ℓ for 2ℓ ≥ 32 cannot occur (ℓ ≤ log₂ 256 = 8). An assembler
variant of `Adjust` (`FASTFRACTALADJUST`, `fracfill.cpp:215`) is behind a
macro that nothing in the tree defines.

### 2.8.2 Plasma (`FILLSHAPE_PLASMA`): the noise generator

`NoiseFractalFill::DoFill(scale, seed, bitmap)` (`noisef.cpp:272-319`) over
`NoiseGen1` (`noise1.cpp`):

* The bitmap is always **256 × 256**, 8 bits, grey (`GenerateNoiseBitmap`,
  `fillval.cpp:572`); neither the fill's `dpi` nor its `tileable` flag
  reaches the generator. `scale` is the fill's graininess.
* **The table**: 50 × 50 floats (`TABLEWIDTH`, `noise1.cpp:115`), filled in
  order with `rand() / RAND_MAX` after `srand(seed)` (`SeedTable`,
  `noise1.cpp:176-187`). This is the **C library's** generator, so the
  original's plasma differs between platforms.
* **Pixel (i, j)** samples u = (i + ½)/256, v = (j + ½)/256 (accumulated in
  steps of 1/256 from ½/256) and is written as ⌊n · 255 + 0.5⌋.
* **Sample n(u, v)** (`VNoise1D`, `noise1.cpp:227-318`), in 32-bit floats:
  S = ⌊scale⌋, and n = 0 when S < 1. xf = u · S, a = ⌊xf⌋, f = xf − a; the
  quadratic B-spline weights are w₀ = 0.5 − f + f²/2, w₁ = 0.5 + f − f²,
  w₂ = f²/2; likewise in y. The three lattice indices a, a+1, a+2 are taken
  modulo S (so the tile wraps), with the third forced to 1 when S = 1, and
  then modulo 50 when S > 50. n = Σⱼ w_yj · (Σᵢ w_xi · T[xᵢ + 50 · yⱼ]),
  rows summed in order j = 0, 1, 2.
* Row j = 0 is the first stored row, at the fill's `StartPoint`.

### 2.8.3 Cache, randomise, persistence

The generated bitmap is cached by the full parameter set and D
(`CacheFractalData` / `IsSameAsCachedFractal`, `fillval.h:317-318`;
`GetGlobalFractalList()->CheckFractalBitmap`, `fillval.cpp:470`), is never
saved (`SetAsFractal`, `bitmap.h:573`), and `Randomise()` only draws a new
seed (`fillval.h:274`). The corpus (`docs/memory/xar-import.md` §25)
holds no fill with gravity or squash other than 0.

## 2.9 Live effects: which part CDraw did

| Effect | Geometry / control (Kernel) | The part CDraw does |
|---|---|---|
| **Shadow** (`nodeshad.cpp`, `bshadow.cpp`) | Chooses radius, colour, bias/gain ramp profile (`nodeshad.cpp:410`); builds the circular convolution mask and the normalisation table | `GenerateWallShadow` (blur), `GenerateFloorShadow` (cast shadow with tilt) |
| **Feather** (`fthrconv.cpp:313`) | As for the shadow but applied to the object's alpha | `GenerateWallShadow` |
| **Bevel** (`beveler.cpp`) | `GenBevelFaces::BevelPath` generates the faces (triangles/trapezia) with their 2D normals; `CBeveler` decides style, light angle and *tilt* | `GDraw2_SetDIBitmap` + `GDraw2_FillTriangle/FillTrapezium` rasterise the lighting map into the high channel of a 32 bpp bitmap; `T_BEVEL` then applies it |
| **Contour** (`nodecntr.cpp`, `paths.cpp:5725`) | `Path::GetContourForStep` with a ramp profile | `GDraw_StrokePathToPath` (path offsetting) and `GenPathContour::ContourPath` |
| **Booleans** (`pbecomea.cpp`) | Chooses the clipping style | `ClipPathToPath` |
| **Blend** (`nodebldr.cpp`) | Interpolation of paths and attributes with a ramp profile | nothing (it only draws the steps) |

The actual blur (`CBitmapShadow::Blur8BppBitmap`, `Kernel/bshadow.cpp:458-536`) is a
**convolution with a disc**: for every row `r` of the disc of radius `fBlur` the left/right
(`aLeft/aRight`) and top/bottom (`aLow/aHigh`) offsets are computed, and a normalisation table
of `uArea·255` entries is passed, compressed to ≤ 0x800 (`TABLE_SIZE`) by a shift `uShift`.
CDraw performs the accumulated sum. Maximum radius 100 px (`MAX_SHADOW_BLUR`), minimum diameter
`sqrt(0.5)`.

## 2.10 Colour management

* **Spaces** (`Kernel/colmodel.h:199`): `CIET` (XYZ+T), `RGBT`, `CMYK`, `HSVT`, `GREYT`,
  `WEBRGBT`, plus indexed colours and spot inks.
* **CMYK**: a naive conversion in the kernel (`colcontx.cpp:1755`, `2224`):
  `R = 1 − C`, `G = 1 − M`, `B = 1 − Y`, applying `K` afterwards; the **real UCR/GCR** is
  CDraw's responsibility via `GColour_SetSeparationTables(cyan, magenta, yellow, black, UCR,
  blackGeneration)`, six LUTs that the app installs when printing/separating and removes
  afterwards (`grndrgn.cpp:3843, 7591`). CDraw also has native CMYK pixel paths
  (`PixelCMYK`, `FPixelCMYK_C_F`).
* **Gamma**: `ColourContextRGBT(View*, double GammaValue)` (`colcontx.cpp:1160`) in the kernel,
  and `GBitmap_SetGamma` / `GBitmap_SetPostGamma` in CDraw (bitmap pre- and post-processing).
* **Luminance**: `GColour_SetGreyConversionValues(R,G,B)` defines the weights used by *all* the
  blend modes and by the conversion to grey. The disassembly of
  `GDraw::SetGreyConversionValues` (`libCDraw_la-GColour.o+0x1790`) shows that it builds three
  cumulative tables of 256 × u32 at `ctx+0x610f0/0x614f0/0x618f0`, with
  `W_c[i] = i · (c · 0x010101) / (R+G+B)`, normalised so that the sum with all three channels at
  255 gives `0xFF000000`. **Xara LX never calls this function**, so CDraw's internal default
  weights are what apply: they must be recovered empirically (by rendering a Darken over a known
  gradient) before the formulas in §2.7.2 can be taken as correct. The starting hypothesis is
  ITU-R BT.601 (0.299 / 0.587 / 0.114).
* **Bitmap correction/separation**: `GColour_SetBitmapConversionTable(pcBGR)`.
* All of CDraw's internal compositing is in **non-linear 8-bit-per-channel sRGB** (the LUTs have
  256 entries): replicating the blends in linear space would give different results.

---

# 3. Task C — Rust reimplementation plan

## 3.1 Requirements derived from Tasks A and B

| # | Requirement | Origin |
|---|---|---|
| R1 | Path filling with cubic Béziers, nonzero and evenodd rules (+ inverse) | §2.2 |
| R2 | AA of quality ≥ 85 levels, switchable (a draft mode with no AA) | §2.3 |
| R3 | Stroking with caps/joins/miter/dash **and** an exact `stroke→path` (for contours and booleans) | §1.3.c |
| R4 | 5 gradient shapes × (simple/repeated/mirrored) × (affine/perspective) × (2 colours with an arbitrary ramp / 3-mesh / 4-mesh) | §2.6 |
| R5 | 12 blend families with *LUT over destination* semantics, not the PDF/CSS ones | §2.7 |
| R6 | Graduated and bitmap transparency with the same 12 families | §2.7.3 |
| R7 | Bitmap fills with parallelogram/perspective, 4 repetition modes, smoothing and HQ filtering, contone | §2.8 |
| R8 | Disc blur (shadow/feather), mask contouring, lit bevel map | §2.9 |
| R9 | Path booleans with Bézier output | §1.4 |
| R10 | Nested intermediate-bitmap rendering (captures) with reuse | §2.5 |
| R11 | Changed bbox and incremental scroll | §2.5 |
| R12 | Document coordinates as 32-bit integers (millipoints) with no loss | §2.1 |
| R13 | Output at 1/4/8/16/24/32 bpp, CMYK, dither (8 styles), separation with UCR/GCR | §1.3.e,j |
| R14 | Full determinism on the export path (PNG/PDF) | new, Xarast |

## 3.2 Comparison of the Rust ecosystem (state as of 2026-09)

| Criterion | **vello** (GPU, wgpu) | **vello_cpu** (CPU sparse strips) | **tiny-skia** | **lyon + our own wgpu** | **resvg** | **femtovg** | **skia-safe** |
|---|---|---|---|---|---|---|---|
| Nature | *Compute-centric* rasteriser, declarative scene | Same model, CPU SIMD backend | A port of a subset of Skia (CPU) | Tessellator to triangles + our own pipeline | SVG renderer on top of tiny-skia | NanoVG-style canvas (GPU) | *Bindings* to Skia C++ |
| AA quality | Exact analytic coverage (conflation-free through *strips*) | Identical to vello | Skia-style *supersampling*/analytic, very good | MSAA (4–8×) or manual analytic AA | tiny-skia's | Stencil+cover, mediocre on thin edges | Excellent |
| Blend modes | The 12+16 of PDF/CSS, in the compositing *shader* | Likewise | Skia's `BlendMode` (16) | Whichever you program | SVG's only | Few | All of Skia's |
| **Custom** blends | Requires touching the compositing shader (a fork) or a post-pass | A fork of the Rust compositor (easier) | Requires a CPU post-pass | **Trivial** (it is your shader) | No | No | Very hard (C++) |
| Gradients with an arbitrary ramp | Yes (`ColorStops`, up to N stops) | Yes | Yes (`GradientStop`) | Bespoke (LUT texture) | Yes | Limited | Yes |
| Conical / diamond gradient | Sweep yes; diamond no | Likewise | Sweep no (linear/radial/two-point only) | Bespoke | No | No | Sweep yes |
| **Perspective** gradient | Via a projective transform, not supported out of the box | No | No (affine only) | **Yes** (projective interpolation in the shader) | No | No | Partial |
| Performance (large scene) | Very high on discrete/integrated GPU | 2nd place in the 2025 *benchmarks*, ahead of Skia and Cairo in many cases | Adequate; worse on ARM | Depends on the tessellation; poor with many small paths | = tiny-skia | High on simple 2D | Very high |
| Maturity | 0.5+/sparse-strips still evolving; API still changing | **alpha** (so declared by the authors) | Stable, quiet maintenance | Stable (lyon 1.x) | Stable | Stable | Stable (but it is C++) |
| Licence | Apache-2.0 / MIT | Apache-2.0 / MIT | BSD-3 (Skia heritage) | MIT/Apache | MPL-2.0 | MIT | BSD-3 + a Skia build |
| Linux/Wayland | Yes (wgpu: Vulkan/GL) | N/A (CPU) | N/A | Yes | N/A | Yes (GL/GLES) | Yes |
| Windows / macOS | Yes (DX12 / Metal) | Yes | Yes | Yes | Yes | Yes | Yes |
| Binary size | ~ 3–6 MB with wgpu | ~ 0.6 MB | **~ 0.4 MB** | ~ 3–6 MB | ~ 1.5 MB | ~ 2 MB | **~ 30–60 MB** + C++ toolchain |
| Build cost | Medium | Low | Low | Medium | Low | Low | **Very high** (C++, ~1 h) |
| Risk for Xarast | A moving API | alpha | No conical/perspective/blends | Everything by hand | Not an engine, a consumer | Insufficient | Contradicts the "100 % Rust" goal, the licence and the weight |

Context notes: Vello CPU and Vello GPU share the *sparse strips* architecture and common
infrastructure in `vello_common`, and Vello CPU is declared **alpha**; in the 2025 *benchmarks*
`vello_cpu` comes second after Blend2D, ahead of Skia and Cairo in many cases. `tiny-skia` is
explicitly "a small subset of Skia" with all its logic ported from Skia.

## 3.3 Firm recommendation: our own `xarast-render` engine with two backends

```
                 ┌──────────────────────────── xarast-render (our own crate) ────────┐
   Document ───▶ │ Scene (retained)  →  Display list  →  Tile/band scheduler          │
   (model)       │            ↑ per-node cache         ↓                              │
                 │            └──────────────  GPU backend  ─┬── CPU backend          │
                 └───────────────────────────────────────────┼────────────────────────┘
                                                             │
        vello (sparse strips, wgpu)  ◀── geometry + paints ──┤
        + our own WGSL compositing passes (Xara blends)      │
                                                             └─▶ vello_cpu (same strips)
                                                                 + our own CPU compositor
```

**Decisions and why:**

1. **Adopt no library as "the engine"**, only as a *coverage rasteriser*. The reason is R5+R4:
   none of them brings Xara's blend modes or the conical/diamond gradient in perspective. What
   *is* reusable — and expensive to write — is the high-quality AA coverage rasteriser. There
   `vello`/`vello_cpu` wins: its intermediate representation (*sparse strips*: strips of pixels
   with per-column coverage) is **exactly** the same concept as CDraw's `Strip`
   (`Kernel/gclips.h:163`), and it lends itself to the compositor being ours.
2. **One frontend, two backends.** The scene and the display list are our own and
   backend-neutral; GPU and CPU consume the same structure. This gives us: (a) a *fallback*
   with no GPU (render servers, CI, Wayland with poor drivers), (b) **determinism** for export
   (R14: always the CPU backend), (c) regression tests comparing both backends pixel by pixel.
3. **`lyon` only for `stroke→path` and geometric utilities** (R3), not for tessellating to
   triangles: `lyon_algorithms` + `kurbo` cover offsetting, adaptive flattening and arc
   lengths. The booleans (R9) go through `kurbo` + our own clipper or a `path-bool`
   equivalent; the rasteriser plays no part.
4. **`tiny-skia` remains a validation reference**, not a production dependency: it is useful for
   generating control images in tests because its AA is well known.
5. **`skia-safe` rejected**: 30–60 MB, a C++ toolchain and a third-party dependency — it would
   recreate the very `libCDraw.a` problem we are solving.

### Facade contract (a 1:1 replacement for `GDrawContext`)

```rust
pub trait Rasterizer {
    fn begin_frame(&mut self, target: &mut Surface, clip: DeviceRect);
    fn fill_path(&mut self, path: &PathRef, rule: FillRule, paint: &Paint, xf: &Transform2D);
    fn stroke_path(&mut self, path: &PathRef, style: &StrokeStyle, paint: &Paint, xf: &Transform2D);
    fn draw_image(&mut self, img: &ImageRef, mapping: &Mapping, paint: &ImagePaint);
    fn push_layer(&mut self, kind: LayerKind, bounds: DeviceRect) -> LayerId; // ≈ Capture
    fn pop_layer(&mut self, id: LayerId, blend: Blend, opacity: Transparency);
    fn end_frame(&mut self) -> DirtyRect;                                     // ≈ GetChangedBBox
}
```

## 3.4 How to cover Xara's exotic blend modes

Task B shows that **the 12 families are the same machine**:

```
level L = f_family( Y(src), t )             // scalar 0..255
out_c   = LUT_family[L][dst_c]              // 256×256 table
```

That is literally an `R8Unorm` texture of 256×256 per family (64 KiB), or a `TEXTURE_2D_ARRAY`
of 12 layers (768 KiB) resident on the GPU. Two families need a little more: **Saturation** and
**Luminosity** operate on the luminance of the **destination** (not just per channel), and
**Hue** requires RGB↔HSV; they are implemented analytically in the shader.

Generation of the tables (CPU, once, in `build.rs` or at start-up):

```rust
/// Generates the 2D LUT of a blend family in the CDraw style.
/// X axis = destination component (0..=255); Y axis = level L (0..=255).
pub fn build_blend_lut(family: BlendFamily) -> [[u8; 256]; 256] {
    let mut lut = [[0u8; 256]; 256];
    for l in 0..=255usize {
        for d in 0..=255usize {
            lut[l][d] = match family {
                // out = d · L / 255
                BlendFamily::Darken   => mul(l as u8, d as u8),
                // out = L + d·(255−L)/255
                BlendFamily::Lighten  => l as u8 + imul(l as u8, d as u8),
                BlendFamily::Contrast => contrast_curve(l as u8, d as u8),
                _ => d as u8,
            };
        }
    }
    lut
}

#[inline] fn mul (a: u8, b: u8) -> u8 { ((a as u16 * b as u16 + 127) / 255) as u8 }
#[inline] fn imul(a: u8, b: u8) -> u8 { (((255 - a) as u16 * b as u16 + 127) / 255) as u8 }
```

And the compositing pass in WGSL:

```wgsl
// Compositing a Xarast layer over the destination.
// src_col : the already-resolved source colour (fill/gradient/bitmap), non-linear sRGB.
// t       : Xara transparency, 0 = opaque … 1 = transparent.
// cov     : antialiasing coverage 0..1.
// blend_luts: texture_2d_array<f32>, layer = family; x = destination, y = level.

const W: vec3<f32> = vec3<f32>(0.299, 0.587, 0.114);   // GColour_SetGreyConversionValues

fn xara_level(family: u32, y_src: f32, t: f32) -> f32 {
    switch family {
        case FAM_DARKEN:  { return y_src + (1.0 - y_src) * t; }
        case FAM_LIGHTEN: { return y_src * (1.0 - t); }
        case FAM_BRIGHT:  { return abs(2.0 * y_src - 1.0) * (1.0 - t); }
        default:          { return t; }
    }
}

@fragment
fn fs_composite(in: VsOut) -> @location(0) vec4<f32> {
    let dst = textureLoad(dst_tex, vec2<i32>(in.pos.xy), 0);
    let src = in.src_col;
    let t   = in.transparency;
    let y   = dot(src.rgb, W);
    var out: vec3<f32>;
    switch in.family {
        // "LUT" families: a single sample per channel.
        case FAM_DARKEN, FAM_LIGHTEN, FAM_BRIGHT, FAM_CONTRAST, FAM_BEVEL: {
            let l = xara_level(in.family, y, t);
            out = vec3<f32>(
                lut(in.family, dst.r, l),
                lut(in.family, dst.g, l),
                lut(in.family, dst.b, l));
        }
        // Saturation: scales the destination's chrominance around its luminance.
        case FAM_SATURATION: {
            let g = dot(dst.rgb, W);
            let k = sat_gain(y, t);                    // ≈ (t·Sat2[y] + Sat1[y]) / 2^21
            out = clamp(vec3<f32>(g) + (dst.rgb - vec3<f32>(g)) * k, vec3(0.0), vec3(1.0));
        }
        // Luminosity: rescales the destination up to the source's luminance.
        case FAM_LUMINOSITY: {
            let target = y * (1.0 - t);
            let m = max(dst.r, max(dst.g, dst.b));
            out = select(dst.rgb * (target / m), vec3<f32>(target), m <= 0.0);
        }
        case FAM_HUE: {
            var h = rgb_to_hsv(dst.rgb);
            h.x = mix(h.x, rgb_to_hsv(src.rgb).x, 1.0 - t);
            out = hsv_to_rgb(h);
        }
        // Mix / Stained glass / Bleach: closed forms.
        case FAM_MIX:     { out = mix(dst.rgb, src.rgb, 1.0 - t); }
        case FAM_STAINED: { out = dst.rgb * (vec3(1.0) - (vec3(1.0) - src.rgb) * (1.0 - t)); }
        case FAM_BLEACH:  { out = vec3(1.0) - (vec3(1.0) - dst.rgb) * (vec3(1.0) - src.rgb * (1.0 - t)); }
        default:          { out = src.rgb; }
    }
    return vec4<f32>(mix(dst.rgb, out, cov(in)), max(dst.a, in.alpha));
}

fn lut(family: u32, d: f32, l: f32) -> f32 {
    return textureSampleLevel(blend_luts, lut_sampler,
                              vec2<f32>(d, l), i32(family), 0.0).r;
}
```

**Critical points:**

* **Colour space**: these formulas are in **encoded sRGB**, just as in CDraw. The compositing
  *target* must be `Rgba8Unorm` (not `…Srgb`) so that the GPU does **not** linearise, or else
  the conversion has to be undone explicitly. Blending in linear space visibly changes Stained
  Glass and Bleach.
* **These blends read the destination** ⇒ they cannot be expressed with the GPU's fixed *blend
  state*. They are resolved with `textureLoad` over an input *attachment* (subpass input /
  `TEXTURE_BINDING` with a per-tile copy) and a **strict ordering** of the display list by
  layer. Since Xara already renders to intermediate bitmaps (captures, §2.5), this fits: every
  layer with an exotic blend is resolved with a *ping-pong* between two tile textures.
* **CPU fallback**: the same code in scalar/SIMD Rust over the same LUTs, shared with the GPU
  backend by construction (a single source of truth for the tables).

## 3.5 Pipeline design

```
 (1) Retained scene             (2) Display list           (3) Scheduling
 ─────────────────────         ──────────────────         ──────────────────
 Document node tree       ──▶  Flat ordered list     ──▶  Partition into 256×256 tiles
 + inherited attributes        of DrawCmd (opaque:        (GPU) or bands of N lines
 + per-node bbox               geometry+paint+blend       (CPU / printing, ≈ grnddib)
 + content hash                +layer push/pop)           + culling by bbox and by clip

 (4) Rasterisation            (5) Compositing            (6) Presentation
 ──────────────────           ─────────────────          ─────────────────
 vello / vello_cpu       ──▶  Our own passes:      ──▶   Blit of the dirty rect to the
 → sparse strips               • paint (gradient/            window surface
   (coverage per span)           bitmap/contone)             (wgpu surface or wl_buffer)
                                • Xara blend (LUT)
                                • layers (captures)
```

**(1) Retained scene and per-node cache.** Every node carries a `ContentHash` (geometry +
attributes + relative matrix) and a `RenderCacheSlot`:

```rust
pub struct NodeCache {
    key:      CacheKey,        // content hash + quantised scale + quality
    bounds:   DeviceRect,
    surface:  Option<CachedSurface>,   // GPU texture or premultiplied CPU buffer
    cost:     u32,             // µs measured while generating it → eviction policy
    epoch:    u64,
}
```

Policy: **only** the expensive nodes are cached — transparent groups, live effects (shadow,
bevel, feather, contour), fractal fills and groups with >N primitives — which is exactly the
criterion behind `cfALLOWDIRECT`/`cfDIRECT` in Xara's captures (§2.5). LRU eviction weighted by
`cost`. The scale is quantised to powers of √2 so that the cache is not invalidated at every
zoom step: the cached texture is rescaled while the zoom stays within ±41 % and is regenerated
in the background.

**(2) Display list.** Immutable per frame, with explicit *layer push/pop*. Commands with an
exotic blend are marked `needs_dst_read = true`, which forces the scheduler to close the tile
and do a ping-pong.

**(3) Tiles and bands.** GPU: 256×256 tiles with a per-tile command list (*binning* by bbox) —
this keeps the destination reads local and able to fit in workgroup memory. CPU: horizontal
bands with the same memory criterion as `GRenderDIB::SetFirstBand` (`grnddib.cpp:451-500`),
reusing its heuristic (a minimum of 16 lines).

**(4) Incremental rendering for pan/zoom.**

* *Pan*: the valid content is reprojected (the equivalent of `GDraw_ScrollBitmap`) and only the
  new bands are rasterised. On the GPU this is a blit of a texture onto itself with an offset.
* *Zoom*: the scaled cached version is presented immediately (a response under 16 ms) and the
  exact re-render is queued.
* *Editing*: the `dirty rect` is recomputed as the union of the modified node's old and new
  bboxes (the equivalent of `GetChangedBBox`), and only the intersected tiles are
  re-rasterised, reusing the cache of the untouched nodes.

**(5) Deferred high-quality render.** Two quality levels, as in Xara but explicit:

| Level | When | Differences |
|---|---|---|
| `Draft` | during a drag/zoom/scroll | AA on but *flatness* ×5, bitmaps sampled *nearest*, shadows with a reduced radius and not recomputed, gradients with a 256-entry LUT |
| `Final` | 120 ms with no interaction (a timer) or on export | full *flatness*, HQ bitmap filtering, 2048-entry LUT, effects recomputed |

The move to `Final` is done tile by tile, prioritising the visible ones, and is cancellable.

## 3.6 Paint and layer model

```rust
pub enum Paint {
    Solid(ColorU8),
    Gradient {
        shape:   GradShape,          // Linear | Radial | Conical | Diamond | Mesh3 | Mesh4
        mapping: GradMapping,        // Affine{a,b,c} | Perspective{a,b,c,d}
        repeat:  Repeat,             // Simple | Repeat | Mirror | RepeatHQ
        ramp:    RampId,             // 256- or 2048-entry LUT, generated with the bias/gain ramp profile
    },
    Image {
        image:   ImageId,
        mapping: GradMapping,        // the same parallelogram/quadrilateral as the gradients
        repeat:  Repeat,
        filter:  Filter,             // Nearest | Bilinear | HighQuality
        contone: Option<(ColorU8, ColorU8, EffectSpace)>,
        adjust:  BitmapAdjust,       // brightness, contrast, gamma, saturation, bias/gain, ranges
    },
    Fractal(FractalParams),          // materialised into an Image; the generator is CPU
}

pub struct Transparency {
    pub family: BlendFamily,         // 12 families (§2.7)
    pub source: TranspSource,        // Flat(u8) | Gradient{…} | Image{…}
}
```

The ramp is generated with the exact profile from §2.6.3:

```rust
pub fn build_ramp(stops: &[Stop], profile: Profile, space: RampSpace, len: usize) -> Vec<ColorU8> {
    (0..len).map(|i| {
        let x = i as f64 / (len - 1) as f64;
        let f = profile.map(x);                      // gain(g, bias(b, x))
        sample_stops(stops, f, space)                // Rgb | HsvShort | HsvLong
    }).collect()
}

impl Profile {
    /// Schlick bias/gain, identical to CProfileBiasGain (Kernel/biasgain.cpp:572-640).
    pub fn map(&self, x: f64) -> f64 {
        if self.bias == 0.0 && self.gain == 0.0 { return x; }
        let b = (self.bias + 1.0) * (0.5 - 1e-5) + 1e-5;
        let g = (self.gain + 1.0) * (0.5 - 1e-5) + 1e-5;
        let biased = x * b / ((1.0 - 2.0 * b) * (1.0 - x) + b);
        let c = (1.0 - 2.0 * g) * (1.0 - 2.0 * biased);
        if biased < 0.5 { biased * g / (c + g) } else { (c - biased * g) / (c - g) }
    }
}
```

## 3.7 Precision strategy

| Stage | Type | Rationale |
|---|---|---|
| Document model / files | **`i32` millipoints** | Compatibility with `.xar`/`.xarast` and with `DocCoord`; range ±2,147,483 mp ≈ ±29.8 m; no accumulation errors while editing (R12). |
| Transformation algebra | **`f64`** | A 3×3 matrix in `f64` has 52 bits of mantissa: it covers exact millipoints (31 bits) with room to spare for scale/rotation. `kurbo` is already `f64`. It is what Xara itself does with the `double DX` in its edges (`Kernel/gclips.h:130`). |
| Intermediate geometry (flattening, offsetting, booleans) | **`f64` (`kurbo::BezPath`)** | Booleans and `stroke→path` are numerically delicate; CDraw uses `double` too. |
| Device coordinates handed to the rasteriser | **`f32` in tile space** | After subtracting the tile origin, the range is ≤ 4096 px ⇒ `f32` gives sub-micropixel precision. It is `vello`'s requirement. **The `f64 → f32` conversion is always done relative to the tile, never in absolute document coordinates.** |
| AA coverage | `u8` (0..255) on CPU, `f32` on GPU | Compatibility with the 8-bit model and with the blend LUTs. |
| Compositing colour | **`u8` non-linear sRGB** | Mandated by §2.10: the blend LUTs have 256 entries and the formulas are defined in encoded sRGB. |
| Colour in the effects that need it (blur, HQ scaling) | `u16` or linear `f32` internally, returning to `u8` | Avoids *banding* in large blurs without changing the semantics of the blend. |

**Golden rule of the port:** *never* put absolute millipoints into an `f32`. A document 5 m wide
has 3.6·10⁸ mp; `f32` has a relative resolution of 2⁻²³ ⇒ an error of ~43 mp ≈ 0.04 pt, visible
at high zoom. Every conversion to `f32` is preceded by a translation to the tile origin.

The **rounding rule** must also be fixed explicitly: CDraw truncates and the app adds half a
pixel to it (`grndrgn.cpp:5290-5300`). In Xarast *round-half-away-from-zero* will be documented
for the conversion to device space and the inherited compensation will be removed.

## 3.8 Validation: how to know the new engine is "the same one"

1. **Reference corpus**: render the set of `.xar` files in `/home/user/xara-xtreme/testfiles`
   and `Designs/` with the original Xara LX (using `libCDraw.a` in an x86-64 VM) to 32 bpp PNG,
   at several zoom levels, with and without AA.
2. **Metric**: do not demand bit-for-bit equality (the AA differs by construction, §2.3).
   Proposed threshold: mean ΔE₀₀ < 1.0 and 99th percentile < 3.0; in flat areas (the interior of
   fills) exact equality **is** required, because only the blend formula and the ramp are
   involved there.
3. **Blend unit tests**: for each of the 12 families, a 256×256×(t) table generated by the new
   engine compared against the one extracted from the original binary via a harness that calls
   `GDraw::CalcTransparencyX` directly (it is possible: they are exported functions, signature
   `(GDraw*, BGR*, BGR, u8)`).
4. **GPU/CPU parity**: the two backends must agree bit for bit on the `Final` path; any
   divergence is a bug (achieved by keeping the LUTs and the order of operations identical).

## 3.9 Phase plan

| Phase | Deliverable | Dependencies | Risk |
|---|---|---|---|
| **M0** | The `Rasterizer` facade + a minimal CPU backend (nonzero/evenodd fill, stroke, solid colour) on top of `vello_cpu`; a `.xarast` viewer | `kurbo`, `vello_cpu` | Low |
| **M1** | Complete gradients (5 shapes × repetition × perspective) + ramps with a bias/gain ramp profile | M0 | Medium (conical and perspective are our own code) |
| **M2** | The 12 blend families with LUTs + nested layers/captures | M1, extraction of the LUTs from the binary | **High** (exact semantics) |
| **M3** | Bitmap fills: parallelogram/perspective, repetition, HQ filtering, contone, adjustments | M1 | Medium |
| **M4** | GPU backend (`vello` + our own WGSL passes) with parity against CPU | M2, M3 | High (destination reads, ordering) |
| **M5** | Tiling/bands, dirty rect, scroll and per-node cache; Draft/Final | M4 | Medium |
| **M6** | Live effects: disc blur, mask contouring, lit bevel, feather | M2 | Medium |
| **M7** | Path booleans and `stroke→path` (replacing `ClipPathToPath`/`GDraw_StrokePathToPath`) | M0 | **High** (numerical robustness) |
| **M8** | Output: dither (8 styles), depth reduction, CMYK with UCR/GCR, separations | M2 | Medium (only relevant for printing) |

A note on M8: the 8 dither styles and the conversions to 1/4/8/16 bpp (§1.6) are of little
relevance to modern displays today. **Proposal**: implement only `DITHER_NONE` and
Floyd-Steinberg in M8 and relegate the rest to an optional legacy-export module.

---

# 4. Appendices

## 4.1 Appendix A — The 126 functions exported by `libCDraw.a`

```
GBitmap_  (15): Blur, PlotTile, PlotTile4, SetBias, SetBrightness, SetContone, SetContrast,
                SetGain, SetGamma, SetInputRange, SetMaxFilterSize, SetOutputRange,
                SetPostGamma, SetSaturation, Sharpen

GColour_  (50): AddToGraduationTable, BuildGraduationTable, BuildGraduationTable32,
                BuildTransparencyTable, ConvertBitmap, ConvertHSVtoRGB, ConvertRGBtoHSV,
                GetGraduationTableLength, GetGraduationTableSize,
                GetLongGraduationTableLength, GetLongGraduationTableSize,
                GetLongTransparentGraduationTableLength, GetLongTransparentGraduationTableSize,
                GetTransparentGraduationTableLength, GetTransparentGraduationTableSize,
                InitialiseWithPalette, ReturnBrush, ReturnBrushRGB, SelectPalette,
                Set3WayGraduation, Set3WayGraduation4, Set4WayGraduation, Set4WayGraduation4,
                SetBitmapConversionTable, SetColour, SetConversionPalette, SetDitherStyle,
                SetGourand, SetGraduation, SetGraduation4, SetGreyConversionValues,
                SetHalftoneOrigin, SetInvert, SetSeparationTables, SetSolidColour,
                SetTileFilteringFlag, SetTilePattern, SetTilePattern4, SetTileSmoothingFlag,
                SetTransparency, SetTransparencyLookupTable, SetTransparent3WayGraduation,
                SetTransparent3WayGraduation4, SetTransparent4WayGraduation,
                SetTransparent4WayGraduation4, SetTransparentGraduation,
                SetTransparentGraduation4, SetTransparentTilePattern,
                SetTransparentTilePattern4, SetWordColour

GDraw_    (55): CalcBBox, CalcSimpleBBox, CalcStrokeBBox, Clean, ClearChangedBBox,
                ClearLastError, ClipRectangle, ClipRegion, ComparePalettes, ContextLength,
                CopyContext, DeviceClipRectangle, FillPath, FillPoint, FillRectangle,
                FillRegion, GetChangedBBox, GetClipRegion, GetDeviceClipRectangle,
                GetLastError, GetLastErrorText, GetMaxBitmapDepth, GetMaxBitmapWidth,
                GetStatistics, GetSvnVersion, GetVersion, HintPath, Initialise, IsOverlap,
                IsStrokeOverlap, MakeRegion, MakeUnclippedRegion, MaxScale, ScrollBitmap,
                SetAntialiasFlag, SetAntialiasQualityFlag, SetBevelContrast, SetBevelDarkness,
                SetBevelLightness, SetChangedBBox, SetDIBitmap, SetDashAdjustmentFlag,
                SetFlatness, SetHintingFlag, SetInvertedDIBitmap, SetMatrix,
                SetMemoryHandlers, SetMiterLimit, SetStackLimit, SetStackSize, StrokePath,
                StrokePathToPath, Terminate, TransformPath, TranslateBevelValue

GDraw2_   (6):  ClearLastError, FillTrapezium, FillTriangle, GetLastError, GetVersion,
                SetDIBitmap
```

Plus four C++ entry points used by the application and declared outside `gdraw.h`:
`ClipPathToPath`, `GenerateWallShadow`, `GenerateFloorShadow`, `ContourBitmap` (§1.4).

## 4.2 Appendix B — CDraw → Xarast module replacement map

| CDraw area | Proposed Rust module | External base |
|---|---|---|
| Context, state, errors | `xarast-render::context` | — |
| Matrix, transformation, bbox | `xarast-render::geom` | `kurbo` |
| Flattening, edge table, AA | `xarast-render::raster` | `vello` / `vello_cpu` |
| Stroking, caps/joins/dash, `StrokePathToPath` | `xarast-geom::stroke` | `kurbo`, `lyon_algorithms` |
| `ClipPathToPath` (booleans) | `xarast-geom::boolops` | our own, on top of `kurbo` |
| Gradients (5 shapes, perspective, meshes) | `xarast-render::paint::gradient` | our own (WGSL + CPU) |
| Gradient tables, bias/gain ramp profile | `xarast-render::ramp` | our own |
| The 12 blend families | `xarast-render::blend` (+ generated LUTs) | our own (WGSL + SIMD) |
| Bitmap fills, contone, adjustments | `xarast-render::paint::image` | `fast_image_resize` |
| Plasma/cloud fractals | `xarast-fx::fractal` | our own (a port of `fracfill.cpp`) |
| Disc blur, contouring, feather | `xarast-fx::blur`, `::contour` | our own (SAT/SIMD + compute) |
| Bevel (`GDraw2_*`) | `xarast-fx::bevel` | our own |
| Captures / layers | `xarast-render::layer` | — |
| Bands, tiles, dirty rect, scroll | `xarast-render::schedule` | — |
| Depth conversion, dither, CMYK, separation | `xarast-color` | `qcms` or our own ICC profiles |
| Regions / clipping | `xarast-render::clip` | `vello` clip stack + our own |

## 4.3 Appendix C — Reproducing the analysis

```bash
XARA=/home/user/xara-xtreme
nm -g --defined-only $XARA/libs/x86_64/libCDraw.a | grep ' T ' | sort > syms.txt      # 926
awk '{print $3}' syms.txt | grep -E '^(GDraw_|GDraw2_|GColour_|GBitmap_)' | sort -u   # 126
awk '{print $3}' syms.txt | c++filt | grep '^GDraw::' | sed 's/GDraw:://;s/(.*//' | sort -u  # 640
mkdir objs && cd objs && ar x $XARA/libs/x86_64/libCDraw.a
objdump -d -r --no-show-raw-insn -C libCDraw_la-GScanT-D.o   # Darken   (§2.7.2)
objdump -d -r --no-show-raw-insn -C libCDraw_la-GScanT-L.o   # Lighten
objdump -d -r --no-show-raw-insn -C libCDraw_la-GScanT-Lu.o  # Luminosity
objdump -d -r --no-show-raw-insn -C libCDraw_la-GScanT-Sn.o  # Saturation
objdump -s -j .rodata -C libCDraw_la-GScanAA.o               # antialiasing weights (§2.3)
```

Objects in the archive, by functional family: `GDraw/GMain/GContext/GStyle/GError/GMemory/GMaths`
(core), `GPath/GStroke/cstroke` (paths), `GScan*/GScanAA` (scanning and AA),
`GScanL/L4/R/R4/RR/S/S4/Sq/Sq4/C/C4/X3/X34/X4/X44` (gradient blitters),
`GScanT-{A,B,Bvl,C,D,H,L,Lu,R,S,Sn}` (blends), `GGrad`, `GColour/GConvert/GTable*`
(colour and LUTs), `GBevel/GTableBevel` (bevel), `GRegion/gclip*` (regions and booleans),
`bshadow2` (shadows), `GSprite/GScroll`.

---

## External sources consulted

- [Releases · linebender/vello](https://github.com/linebender/vello/releases)
- [Vello CPU (README, sparse_strips)](https://skia.googlesource.com/external/github.com/linebender/vello/+/refs/heads/main/sparse_strips/vello_cpu/README.md)
- [linebender/vello](https://github.com/linebender/vello)
- [vello_cpu — crates.io](https://crates.io/crates/vello_cpu)
- [Linebender in July 2025](https://linebender.org/blog/tmil-19/)
- [linebender/tiny-skia](https://github.com/linebender/tiny-skia)
- [tiny-skia — lib.rs](https://lib.rs/crates/tiny-skia)
- [Vello — lib.rs](https://lib.rs/crates/vello)
