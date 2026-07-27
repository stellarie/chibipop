# winspike — BitBlt capture -> 2x upscale -> in-memory Windows.Media.Ocr

Status: **WORKING**. Compiles clean (zero warnings) with `cargo build`, and ran
successfully end-to-end against a real live-screen BitBlt capture, twice
(horizontal sentence PNG and vertical sentence PNG, both opened in Windows
Paint and captured off the actual desktop — not decoded from disk).

Crate location: `...\scratchpad\winspike` (scratch crate, not touching chibipop).

Toolchain used: rustc/cargo 1.97.1, stable-x86_64-pc-windows-msvc (as instructed,
not reinstalled).

---

## 1. Exact Cargo.toml

```toml
[package]
name = "winspike"
version = "0.1.0"
edition = "2024"

[dependencies]
windows = { version = "0.62.2", features = ["Win32_Graphics_Gdi", "Win32_UI_HiDpi", "Win32_System_WinRT", "Media_Ocr", "Graphics_Imaging", "Globalization", "Storage_Streams", "Security_Cryptography"] }
windows-future = "0.3.2"
```

Resolved versions (`cargo tree`): `windows v0.62.2`, `windows-core v0.62.2`,
`windows-future v0.3.2`, `windows-collections v0.3.2`, `windows-numerics v0.3.1`
(pulled in transitively, unused directly), plus the usual proc-macro chain
(`syn`, `quote`, `proc-macro2`, `windows-implement`, `windows-interface`,
`windows-link`, `windows-result`, `windows-strings`, `windows-threading`).

**Every feature in that list was verified necessary by actually removing it
and re-running `cargo build`.** The first working draft also had
`Win32_Foundation`, `Win32_UI_WindowsAndMessaging`, `Win32_System_Com`, and the
WinRT `Foundation` feature turned on speculatively. All four were removed and
the crate still compiled clean:
- `Win32_Foundation` — not required as a *direct* feature; nothing in our code
  names a `Win32::Foundation` type directly (we pass `None`/`Some(hdc)` and let
  inference resolve `Option<HDC>` etc.).
- `Win32_UI_WindowsAndMessaging` — never used; `GetDC`/`ReleaseDC` for the
  whole-screen DC live in `Win32_Graphics_Gdi` itself, no need for
  `GetDesktopWindow`.
- `Win32_System_Com` — speculative `CoInitializeEx` alternative; we ended up
  using `RoInitialize` from `Win32_System_WinRT` instead (see §5).
- WinRT `Foundation` — turns out to be transitively implied by `Globalization`
  (`Globalization = ["Foundation"]` in the windows crate's own Cargo.toml), so
  listing it explicitly was redundant.

`windows-future` as a **second, explicit, direct dependency** was the one
genuine surprise — see the "biggest gotcha" section (§7) for why it's needed
at all; short version: `windows` doesn't re-export the async types you need to
even *name*, and it was already an identical-version transitive dependency of
`windows` so adding it directly costs nothing new (no new download, no version
drift, same 0.3.2 either way).

`windows-collections` was **not** added as a direct dependency even though
`OcrResult::Lines()` returns `windows_collections::IVectorView<OcrLine>` —
unlike the async path, none of our code ever needs to *spell* that type name
(`for line in &lines`, `lines.Size()?` etc. all work through inference /
inherent methods), so it was left as a pure transitive dependency.

---

## 2. Complete main.rs (verbatim)

```rust
use std::ffi::c_void;
use std::mem::size_of;
use std::time::Duration;

use windows::core::{Result, HSTRING};
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Globalization::Language;
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// BitBlt-capture a rectangular region of the virtual desktop into a tightly
/// packed top-down 32bpp BGRA buffer (alpha byte undefined coming out of GDI).
fn capture_region(x: i32, y: i32, width: i32, height: i32) -> Result<Vec<u8>> {
    unsafe {
        let screen_dc = GetDC(None); // NULL hwnd => DC for the whole screen
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
        let old_obj = SelectObject(mem_dc, bitmap.into());

        BitBlt(mem_dc, 0, 0, width, height, Some(screen_dc), x, y, SRCCOPY)?;

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width;
        bmi.bmiHeader.biHeight = -height; // negative => top-down DIB
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;

        let mut buf = vec![0u8; (width as usize) * (height as usize) * 4];
        let lines_copied = GetDIBits(
            mem_dc,
            bitmap,
            0,
            height as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        SelectObject(mem_dc, old_obj);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);

        if lines_copied == 0 {
            eprintln!("warning: GetDIBits copied 0 scanlines (capture may be blank)");
        }

        Ok(buf)
    }
}

/// Nearest-neighbour 2x upscale of a tightly packed BGRA buffer. Forces the
/// alpha byte to 0xFF on the way out since GDI/BitBlt does not populate it.
fn upscale_2x(src: &[u8], w: i32, h: i32) -> (Vec<u8>, i32, i32) {
    let w2 = w * 2;
    let h2 = h * 2;
    let mut dst = vec![0u8; (w2 as usize) * (h2 as usize) * 4];
    for y in 0..(h2 as usize) {
        let sy = y / 2;
        for x in 0..(w2 as usize) {
            let sx = x / 2;
            let si = (sy * (w as usize) + sx) * 4;
            let di = (y * (w2 as usize) + x) * 4;
            dst[di] = src[si];
            dst[di + 1] = src[si + 1];
            dst[di + 2] = src[si + 2];
            dst[di + 3] = 0xFF;
        }
    }
    (dst, w2, h2)
}

/// Synchronously block on a WinRT `IAsyncOperation<T>` by polling `Status`.
///
/// `windows-future::Async::join()` (the spiritual successor of the old
/// windows-rs `.get()`) exists but is NOT publicly reachable: it lives in a
/// private `mod r#async` inside the `windows-future` crate that is only
/// glob-`use`d (not `pub use`d) at that crate's root, so no downstream crate
/// can name or call it. This hand-rolled poll loop is the workaround.
fn wait_blocking<T>(op: windows_future::IAsyncOperation<T>) -> Result<T>
where
    T: windows::core::RuntimeType + 'static,
{
    loop {
        let status = op.Status()?;
        if status != windows_future::AsyncStatus::Started {
            return op.GetResults();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: winspike <x> <y> <width> <height>");
        std::process::exit(1);
    }
    let x: i32 = args[1].parse().expect("x must be an integer");
    let y: i32 = args[2].parse().expect("y must be an integer");
    let width: i32 = args[3].parse().expect("width must be an integer");
    let height: i32 = args[4].parse().expect("height must be an integer");

    unsafe {
        // Must happen before any GDI/USER32 calls, or BitBlt captures a
        // DPI-virtualized (blurry/rescaled) image on scaled displays instead
        // of real physical pixels.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
        // WinRT activation (OcrEngine::TryCreateFromLanguage etc.) needs the
        // apartment initialized or it fails with CO_E_NOTINITIALIZED.
        RoInitialize(RO_INIT_MULTITHREADED)?;
    }

    eprintln!("capturing region x={x} y={y} w={width} h={height}");
    let raw = capture_region(x, y, width, height)?;
    let (up, up_w, up_h) = upscale_2x(&raw, width, height);
    eprintln!("upscaled to {up_w}x{up_h}");

    let ibuffer = CryptographicBuffer::CreateFromByteArray(&up)?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &ibuffer,
        BitmapPixelFormat::Bgra8,
        up_w,
        up_h,
        BitmapAlphaMode::Ignore,
    )?;

    let lang = Language::CreateLanguage(&HSTRING::from("ja"))?;
    let engine = OcrEngine::TryCreateFromLanguage(&lang)?;
    let op = engine.RecognizeAsync(&bitmap)?;
    let result = wait_blocking(op)?;

    let lines = result.Lines()?;
    println!("lines: {}", lines.Size()?);
    for line in &lines {
        println!("LINE: {}", line.Text()?);
        let words = line.Words()?;
        for word in &words {
            let r = word.BoundingRect()?;
            println!(
                "  word {:<12} x={:>7.1} y={:>7.1} w={:>6.1} h={:>6.1}",
                word.Text()?.to_string(),
                r.X,
                r.Y,
                r.Width,
                r.Height
            );
        }
    }

    Ok(())
}
```

No `unsafe` outside the two blocks that are inherently unsafe (raw GDI calls,
and the two process-global init calls). Everything else is safe, checked
`windows-rs` projected API.

---

## 3. The exact async pattern

**Used: a hand-rolled blocking poll loop on `IAsyncOperation<T>::Status()` /
`::GetResults()`.** Not `.get()` (doesn't exist in this version), not `.await`
(would need an executor — see below), and not `windows-future`'s own `Async`
trait `.join()`/`.when()` helpers (exist but are unreachable, see §7).

`IAsyncOperation<TResult>` (from `windows-future`, referenced through the
`windows` crate's `Media::Ocr::OcrEngine::RecognizeAsync` return type) has
public **inherent** methods `Status() -> Result<AsyncStatus>` and
`GetResults() -> Result<TResult>` generated directly from the WinRT
`IAsyncOperation`/`IAsyncInfo` vtables. No trait import needed for those two —
they're just methods on the struct. The loop:

```rust
loop {
    let status = op.Status()?;
    if status != windows_future::AsyncStatus::Started {
        return op.GetResults();
    }
    std::thread::sleep(Duration::from_millis(5));
}
```

This needed **one extra direct dependency**: `windows-future = "0.3.2"` in
`Cargo.toml`, purely so the code can *name* `IAsyncOperation<T>` (as a function
parameter type) and `AsyncStatus` (to compare against `::Started`). See §7 —
`windows` itself doesn't re-export either of these under its own namespace.
This is not a "real" new dependency in the sense of pulling in new code: it
was already resolved transitively at the exact same version 0.3.2; the `cargo
add` just gave our own crate permission to name types that were already being
compiled in.

The alternative idiomatic-looking path — `.await` — **is** available:
`IAsyncOperation<T>` implements `std::future::IntoFuture` (in
`windows-future`'s `future.rs`, gated behind the `std` feature, which is on by
default). But that only gets you a `Future`; nothing in `windows` or
`windows-future` ships an executor to drive it. You'd need `#[tokio::main]`,
`pollster::block_on`, `futures::executor::block_on`, or a hand-rolled
`Waker`/`thread::park` executor to actually run it — i.e. a real extra
dependency (or a nontrivial amount of extra hand-written code) for no benefit
in a single-shot synchronous CLI tool. The poll loop above needed zero extra
crates beyond the visibility fix, so that's what shipped.

---

## 4. Getting pixels into a SoftwareBitmap

**Route used: `SoftwareBitmap::CreateCopyWithAlphaFromBuffer` fed by
`CryptographicBuffer::CreateFromByteArray`.** This is the file-free route the
task flagged as likely — confirmed correct and it's genuinely the shortest
path: two calls, no stream, no decoder, no disk.

```rust
let ibuffer = CryptographicBuffer::CreateFromByteArray(&up)?;               // &[u8] -> IBuffer
let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
    &ibuffer, BitmapPixelFormat::Bgra8, up_w, up_h, BitmapAlphaMode::Ignore,
)?;
```

(`CreateCopyFromBuffer`, the 4-arg overload without an explicit alpha mode,
also exists and defaults to `Premultiplied` — I used the 5-arg
`CreateCopyWithAlphaFromBuffer` overload instead to be explicit, since our
buffer's alpha byte isn't meaningfully premultiplied data.)

**Pixel format:** `BitmapPixelFormat::Bgra8` (enum value 87) is what BitBlt
already gives you via a 32bpp DIB — `Rgba8` would require a manual
channel-swap, so `Bgra8` is strictly the easier choice, not just a permitted
one.

**Alpha:** GDI's `BitBlt`/`GetDIBits` **does not populate the alpha channel**
of a plain 32bpp DIB from a desktop capture — the 4th byte of every pixel
comes back as whatever garbage (usually `0x00`) happened to be in the
destination bitmap's memory, not a real coverage value. Two required
consequences, both done in `upscale_2x`:
1. Force the byte to `0xFF` yourself before handing the buffer to WinRT — I do
   this during the upscale pass (a value of `0x00` alpha, if actually honored,
   would make the whole capture read as fully transparent).
2. Tell `SoftwareBitmap` to **ignore** whatever is in that byte anyway via
   `BitmapAlphaMode::Ignore` rather than `Premultiplied` or `Straight` — those
   two modes tell WinRT the alpha byte is *meaningful* data, which for a raw
   GDI capture it never is. I did not test what OCR does if you leave this on
   `Premultiplied` with garbage alpha, since `Ignore` is obviously correct for
   this data and worked first try — flagging that untested corner rather than
   guessing.

**Stride/padding:** never an issue here, and worth being explicit about why —
`CreateCopyFromBuffer`/`CreateCopyWithAlphaFromBuffer` require the input
`IBuffer.Length` to equal exactly `width * height * bytesPerPixel` (no
per-row padding parameter exists on this API at all — unlike `GetDIBits`
itself, or a raw BMP header, which do support padded/strided rows). This is
automatically satisfied for 32bpp: DIB rows are always a multiple of 4 bytes
already at 4 bytes/pixel, so `GetDIBits` never pads a 32bpp capture, and our
own hand-written `upscale_2x` loop only ever produces a tightly packed buffer.
**This would not be true at 24bpp** (rows padded to 4-byte boundaries) —
anyone adapting this to a 24bpp path needs to explicitly de-pad before calling
`CreateCopyFromBuffer`, or it will reject the buffer length outright (this
was reasoned from the API's exact-length contract, not independently observed
first-hand, since the spike only exercises the 32bpp path).

**Format rejection:** none observed — `Bgra8` + `Ignore` was accepted by both
`CreateCopyWithAlphaFromBuffer` and by `OcrEngine::RecognizeAsync` on the
first attempt, no fallback/conversion needed.

---

## 5. COM/WinRT initialization

**Yes, required**, confirmed by needing it in practice (first draft without it
would have hit `CO_E_NOTINITIALIZED` — every windows-rs sample that calls into
WinRT activation from a plain `fn main()` needs this, since a console binary
gets no automatic COM apartment).

Exact call used:

```rust
RoInitialize(RO_INIT_MULTITHREADED)?;
```

from `windows::Win32::System::WinRT` (feature `Win32_System_WinRT`), called
once at the top of `main()`, before touching `Language::CreateLanguage`,
`OcrEngine::TryCreateFromLanguage`, or anything else WinRT. Threading model:
**multithreaded apartment (MTA)** via `RO_INIT_MULTITHREADED`. This was a
choice, not a forced one — `RO_INIT_SINGLETHREADED` (STA) would also work for
a single-threaded CLI tool with no UI message loop; MTA was picked because it
needs no message pump and this program has no window/UI thread to pump
messages on. `CoInitializeEx(None, COINIT_MULTITHREADED)` from
`Win32_System_Com` is the equivalent classic-COM call and would also have
worked (`RoInitialize` is effectively a superset that also spins up the WinRT
activation machinery) — I used `RoInitialize` specifically because it's the
more "native" call for a program that's *only* doing WinRT (no classic COM
objects), and it meant `Win32_System_Com` could be dropped from the feature
list entirely (confirmed compiles without it).

`SetProcessDpiAwarenessContext` is called immediately before `RoInitialize` —
unrelated API, but same "must happen before you use the thing it affects"
constraint; see the DPI gotcha in §7.

---

## 6. Real printed output

Both runs captured a **live BitBlt of the actual desktop**, not a decode of
the PNG from disk: the target PNG (900x400, containing only the rendered
Japanese sentence) was opened in the *default* PNG viewer via
`Start-Process <path>.png`, which on this machine resolved to **`mspaint.exe`**
(not Photos) — confirmed via `Get-Process | Where MainWindowTitle`. Paint
opened maximized on the secondary (portrait, 1080x1920) monitor at screen
offset `(2560, 0)`. Rather than guess Paint's canvas/scroll offset, the
binary was pointed at the *entire* monitor rectangle
(`x=2560 y=0 width=1080 height=1920`, obtained via `GetWindowRect` /
`Screen.AllScreens` from PowerShell) — a legitimate, argv-driven rectangular
region per the spec, and it sidesteps needing to know exactly where inside
Paint's window chrome the image landed. OCR correctly finds the target
sentence regardless of the surrounding Paint ribbon/toolbar text also being
in frame (that ribbon text — "View", "Copy", "Paste", "Select", "Resize",
"Rotate", "Shapes", "Clipboard" — shows up correctly recognized too in the
output below, which is itself independent confirmation that this is a real
screen capture and not a fluke).

### Run 1 — `ocr_horizontal-sentence.png` open in Paint, command: `winspike.exe 2560 0 1080 1920`

Full raw output was 33 recognized lines (mostly Paint ribbon-icon noise, which
is expected/correct OCR behavior on non-text UI graphics, not a pipeline bug).
The line that matters:

```
capturing region x=2560 y=0 w=1080 h=1920
upscaled to 2160x3840
lines: 33
...
LINE: 昨 日 は 友 達 と 映 画 を 見 に 行 き ま し た 。
  word 昨            x=   70.0 y=  346.0 w=  64.0 h=  68.0
  word 日            x=  154.0 y=  350.0 w=  46.0 h=  60.0
  word は            x=  222.0 y=  350.0 w=  54.0 h=  60.0
  word 友            x=  286.0 y=  346.0 w=  66.0 h=  66.0
  word 達            x=  364.0 y=  346.0 w=  68.0 h=  66.0
  word と            x=  442.0 y=  350.0 w=  40.0 h=  60.0
  word 映            x=  496.0 y=  346.0 w=  66.0 h=  68.0
  word 画            x=  572.0 y=  348.0 w=  66.0 h=  64.0
  word を            x=  648.0 y=  348.0 w=  48.0 h=  62.0
  word 見            x=  708.0 y=  350.0 w=  64.0 h=  64.0
  word に            x=  786.0 y=  352.0 w=  48.0 h=  58.0
  word 行            x=  844.0 y=  346.0 w=  68.0 h=  68.0
  word き            x=  922.0 y=  348.0 w=  44.0 h=  62.0
  word ま            x=  978.0 y=  348.0 w=  46.0 h=  62.0
  word し            x= 1036.0 y=  350.0 w=  42.0 h=  62.0
  word た            x= 1084.0 y=  348.0 w=  50.0 h=  62.0
  word 。            x= 1142.0 y=  394.0 w=  22.0 h=  20.0
```

This is `昨日は友達と映画を見に行きました。` ("Yesterday I went to see a
movie with a friend"), split into one `word` per character (expected — no
inter-character whitespace in Japanese for the OCR engine to segment on),
each with a plausible, monotonically-increasing-in-x bounding box. Sanity
check on the upscale: font was rendered at size 28 in the source PNG;
post-2x-upscale character boxes are ~44-68px, consistent with a ~22-34px glyph
at 1x scale, i.e. the upscale is really being applied and isn't a no-op.

The rest of the 33 lines (omitted here, full transcript was captured live and
is reproducible by rerunning) is genuine Paint ribbon UI text/icon-noise, e.g.:

```
LINE: View
  word View         x=  258.0 y=   62.0 w=  50.0 h=  16.0
LINE: [ Copy
  word [            x=  102.0 y=  152.0 w=  12.0 h=  24.0
  word Copy         x=  142.0 y=  160.0 w=  50.0 h=  20.0
LINE: Paste
  word Paste        x=   28.0 y=  184.0 w=  52.0 h=  16.0
LINE: Shapes
  word Shapes       x=  918.0 y=  250.0 w=  70.0 h=  20.0
LINE: Clipboard
  word Clipboard    x=   56.0 y=  250.0 w=  98.0 h=  20.0
```
This is Paint's actual ribbon text, correctly recognized — independent proof
the capture is really pulling live desktop pixels via BitBlt, not e.g.
silently falling back to a blank/garbage buffer that happens to OCR into a
suspiciously-perfect result.

### Run 2 — `ocr_vertical.png` open in Paint, same command/region

```
LINE: 昨 日 は 友 達 と 映 画 を 目
  word 昨            x= 1438.0 y=  344.0 w=  64.0 h=   70.0
  word 日            x= 1446.0 y=  428.0 w=  46.0 h=   60.0
  word は            x= 1442.0 y=  504.0 w=  56.0 h=   58.0
  word 友            x= 1434.0 y=  576.0 w=  68.0 h=   66.0
  word 達            x= 1434.0 y=  654.0 w=  68.0 h=   66.0
  word と            x= 1446.0 y=  732.0 w=  44.0 h=   60.0
  word 映            x= 1438.0 y=  806.0 w=  66.0 h=   68.0
  word 画            x= 1436.0 y=  888.0 w=  66.0 h=   64.0
  word を            x= 1440.0 y=  962.0 w=  56.0 h=   64.0
  word 目            x= 1446.0 y= 1040.0 w=  44.0 h=   48.0
```
Near-constant x (~1434-1446), monotonically increasing y per character — a
correctly read top-to-bottom vertical column, exposed as a single `OcrLine`
with per-character words in reading order. Last character (見, "see") was
misread as 目 ("eye") — a real, minor OCR accuracy artifact of the engine
itself (same class of error the original `ocr_probe.ps1` PowerShell script
run would be equally subject to), not a bug in the capture/upscale/feed
pipeline.

---

## 7. Gotchas a doc-only plan would have missed

**#1 — biggest one: `windows` does not re-export `windows-future`'s types
under its own namespace, even though its own generated APIs return them.**
`OcrEngine::RecognizeAsync()` returns `windows_future::IAsyncOperation<OcrResult>`
— that's the literal type in the generated source — but there is no
`windows::Foundation::IAsyncOperation` you can write instead. A plan written
purely from the `windows::Media::Ocr::OcrEngine` docs/signature would assume
you can name and store that return type using only the one dependency you
already have; you cannot. You either (a) never bind it to a named type and
thread it through inference only (works for simple call chains, breaks the
moment you want a small reusable helper function, a struct field, or to
`Box<dyn Fn>` it), or (b) add `windows-future` as a second, explicit direct
dependency just to get naming rights to a type that's already being compiled
into your binary regardless. Compounding this: the "obvious" sync-wait method
you'd reach for from older tutorials, `.get()`, doesn't exist any more in
0.62.2 — its replacement, `Async::join()`, is defined `pub` but inside a
private (non-`pub`) module and only glob-`use`d (not `pub use`d) at the
`windows-future` crate root, so it is **permanently unreachable from any
downstream crate**, not just gated behind a feature flag. This is not
documented anywhere obvious; it was only found by downloading the crate
source into the Cargo registry cache and reading `windows-future-0.3.2/src/
lib.rs` / `async.rs` directly. A plan that says "call `.get()`" or "use
`Async::join()`" will fail to compile, full stop, and the fix (manual
`Status()`/`GetResults()` poll loop) isn't something the compiler error
points you toward cleanly either — the error is just "no method named `get`
found" with a long, unhelpful list of candidate trait imports.

**#2 — the `windows` crate's WinRT-namespace Cargo features do NOT have the
`Windows_` prefix you'd guess from the WinRT namespace name.** It's
`Media_Ocr`, not `Windows_Media_Ocr`; `Graphics_Imaging`, not
`Windows_Graphics_Imaging`; `Storage_Streams`, not `Windows_Storage_Streams`.
(The `Win32_*` Win32-namespace features *do* carry a prefix, matching their
own namespace — `Win32_Graphics_Gdi` etc. — so the inconsistency is genuinely
easy to trip on if you pattern-match from one to the other.) `cargo add
windows --features Windows_Media_Ocr` fails outright with "unrecognized
feature." This is the single most likely thing to be silently wrong in a plan
written from memory/docs rather than checked against the installed crate's
own `Cargo.toml`.

**#3 — `IVectorView<T>` exposes `.Size()`, not `.Count()`.** The .NET/COM
projection (what you get scripting this same API from PowerShell, e.g. the
project's own `ocr_probe.ps1`, which calls `$result.Lines.Count`) exposes
`Count` via `IReadOnlyList<T>`. The raw `windows-rs` projection exposes the
underlying WinRT ABI method name directly: `Size()`. A plan cross-referencing
a C#/PowerShell OCR example (very likely, since almost all `Windows.Media.Ocr`
sample code in the wild is C#) will write `.Count()` and get a compile error.

**#4 — DPI virtualization silently corrupts BitBlt captures on scaled
displays**, and there's no error/exception to catch — the capture just quietly
comes back blurry/rescaled relative to real pixel coordinates instead of
failing loudly. `SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)`
must be called (successfully, i.e. before any other GDI/USER32 call touches a
DC or window) to get BitBlt to return true 1:1 physical pixels on any display
running above 100% scale. This machine's actual monitor layout (a 2560x1080
primary next to a 1080x1920 portrait secondary, confirmed via
`Screen.AllScreens`) made this concrete and worth testing against rather than
assuming — multi-monitor + mixed-orientation setups are exactly where DPI
virtualization bugs hide.

**#5 — alpha is not free.** `BitBlt`/`GetDIBits` off the desktop hands back a
32bpp buffer where the 4th byte is not a real alpha channel — nothing
documents "the alpha byte will be garbage," you find out by comparing it
against `BitmapAlphaMode::Premultiplied`'s contract (which assumes the RGB
channels were pre-scaled by that alpha) and realizing a screen capture was
never premultiplied by anything. Getting this wrong (e.g. defaulting to
`CreateCopyFromBuffer`'s implicit `Premultiplied` with a garbage/zero alpha
byte) would very plausibly make `SoftwareBitmap` treat the whole image as
transparent/black rather than raising a clean error — a silent-corruption
failure mode, not a compile or runtime error, which is the worst kind to plan
around blind.

**#6 — the default PNG viewer on this machine is `mspaint.exe`, not Photos.**
Not a `windows-rs` issue at all, but relevant to anyone else's plan that
assumes "open in the default viewer" means a specific app: it's
machine/user-configuration-dependent, discovered here only by actually
launching it and checking `Get-Process | Where MainWindowTitle` rather than
assumed in advance.
