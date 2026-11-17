//! Thin Direct2D / DirectWrite layer. One render target, one reusable brush,
//! text formats created lazily and cached by (size, weight, family, alignment).

use std::collections::HashMap;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows_numerics::Vector2 as D2D_POINT_2F;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Font {
    pub size10: u32,
    pub bold: bool,
    pub mono: bool,
    pub icon: bool,
    pub align: Align,
}

pub const fn f(size: f32) -> Font {
    Font { size10: (size * 10.0) as u32, bold: false, mono: false, icon: false, align: Align::Left }
}
impl Font {
    pub const fn b(mut self) -> Self {
        self.bold = true;
        self
    }
    pub const fn m(mut self) -> Self {
        self.mono = true;
        self
    }
    pub const fn icon(mut self) -> Self {
        self.icon = true;
        self
    }
    pub const fn r(mut self) -> Self {
        self.align = Align::Right;
        self
    }
    pub const fn c(mut self) -> Self {
        self.align = Align::Center;
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }
    pub fn r(&self) -> f32 {
        self.x + self.w
    }
    pub fn b(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.r() && py >= self.y && py < self.b()
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    fn d2(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.r(), bottom: self.b() }
    }
}

pub fn rgb(c: u32) -> D2D1_COLOR_F {
    rgba(c, 1.0)
}
pub fn rgba(c: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((c >> 16) & 255) as f32 / 255.0,
        g: ((c >> 8) & 255) as f32 / 255.0,
        b: (c & 255) as f32 / 255.0,
        a,
    }
}

pub struct Gfx {
    factory: ID2D1Factory,
    dw: IDWriteFactory,
    rt: Option<ID2D1HwndRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    formats: HashMap<Font, IDWriteTextFormat>,
    /// shaped text by (string, font, box size): `text` reuses a layout while
    /// the same string is drawn frame after frame (DrawText reshapes it on
    /// every call). Device-independent, so it survives a lost target.
    layouts: HashMap<u64, Layout>,
    /// frame counter for `layouts` eviction
    frame: u32,
    /// images (process icons) by cache key; tied to the render target
    bitmaps: HashMap<std::sync::Arc<str>, ID2D1Bitmap>,
    hwnd: HWND,
    pub dpi: f32,
    /// last frame's EndDraw (rasterise + present) and render-target creation
    /// times, ms (for `stall: paint` lines)
    pub end_ms: f32,
    pub create_ms: f32,
    /// the current target is a hardware (GPU) one
    on_gpu: bool,
    /// when the GPU path last failed (creation error or device lost): software
    /// until `GPU_RETRY` has passed
    gpu_failed: Option<std::time::Instant>,
    /// the target is a software stand-in until `warm` has a GPU device
    warming: bool,
}

/// Most layouts `Gfx::text` keeps (a busy frame draws ~400 strings).
const LAYOUT_CAP: usize = 4096;

struct Layout {
    s: Box<str>,
    font: Font,
    w4: u32,
    h4: u32,
    l: IDWriteTextLayout,
    used: u32,
}

/// The first GPU device in a process takes ~200 ms (driver load and init;
/// 190-250 ms on an RTX 3080), and the hide → release → show cycle pays it on
/// every open. So it is made on a worker: the app paints the open with a
/// software target (~15 ms), starts the worker once that frame is shown, and
/// when the warm device exists makes its GPU target (the driver is loaded by
/// then: ~40 ms instead of ~200) and drops the warm device.
mod warm {
    use std::sync::Mutex;
    use windows::Win32::Foundation::{E_FAIL, HMODULE, HWND};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::*;

    #[derive(Clone, Copy)]
    pub enum State {
        Idle,
        Running,
        /// made, in this many ms
        Ready(f32),
        Failed(windows::core::HRESULT),
    }
    struct Dev(#[allow(dead_code)] ID3D11Device);
    // SAFETY: D3D11 devices are free-threaded; this one is only held and dropped.
    unsafe impl Send for Dev {}
    /// (generation, state, the device): `reset` bumps the generation so a
    /// worker that finishes after it drops its device instead of keeping it
    static SLOT: Mutex<(u64, State, Option<Dev>)> = Mutex::new((0, State::Idle, None));

    pub fn status() -> State {
        SLOT.lock().map(|s| s.1).unwrap_or(State::Idle)
    }

    /// Start making the device unless it is made or being made. `hwnd` is
    /// repainted when it is done, which swaps in the GPU target.
    pub fn start(hwnd: isize) {
        let generation = {
            let Ok(mut s) = SLOT.lock() else { return };
            if !matches!(s.1, State::Idle) {
                return;
            }
            s.1 = State::Running;
            s.0
        };
        let work = move || {
            let t0 = std::time::Instant::now();
            let mut dev = None;
            let r = unsafe { D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, None) };
            let ms = t0.elapsed().as_secs_f32() * 1000.0;
            {
                let Ok(mut s) = SLOT.lock() else { return };
                if s.0 != generation {
                    return; // released meanwhile (window hidden): drop it
                }
                match (r, dev) {
                    (Ok(()), Some(d)) => (s.1, s.2) = (State::Ready(ms), Some(Dev(d))),
                    (Err(e), _) => s.1 = State::Failed(e.code()),
                    (Ok(()), None) => s.1 = State::Failed(E_FAIL),
                }
            }
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(HWND(hwnd as *mut _)), None, false);
            }
        };
        if std::thread::Builder::new().name("gpu warm-up".into()).spawn(work).is_err() {
            if let Ok(mut s) = SLOT.lock() {
                s.1 = State::Failed(E_FAIL);
            }
        }
    }

    /// Forget the warm device, and any being made.
    pub fn reset() {
        if let Ok(mut s) = SLOT.lock() {
            (s.0, s.1, s.2) = (s.0 + 1, State::Idle, None);
        }
    }
}

/// After a GPU failure, how long the app stays on the software path before a new
/// target (next open, or next loss) tries the GPU again.
const GPU_RETRY: std::time::Duration = std::time::Duration::from_secs(300);

/// `TSUBAME_GPURENDER=0` forces the software (WARP) path; anything else, or
/// unset, renders on the GPU with software as the fallback.
fn gpu_allowed_by_env() -> bool {
    static A: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *A.get_or_init(|| std::env::var_os("TSUBAME_GPURENDER").is_none_or(|v| v != "0"))
}

/// Policy: try the GPU unless forced off or it failed less than `GPU_RETRY` ago.
fn want_gpu(env_allows: bool, failed: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    env_allows && failed.is_none_or(|t| now.duration_since(t) >= GPU_RETRY)
}

impl Gfx {
    pub fn new(hwnd: HWND) -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Gfx {
                factory,
                dw,
                rt: None,
                brush: None,
                formats: HashMap::new(),
                layouts: HashMap::new(),
                frame: 0,
                bitmaps: HashMap::new(),
                hwnd,
                dpi: 96.0,
                end_ms: 0.0,
                create_ms: 0.0,
                on_gpu: false,
                gpu_failed: None,
                warming: false,
            })
        }
    }

    /// Which render path the current target uses, for log lines.
    pub fn path(&self) -> &'static str {
        if self.warming {
            return "software (GPU starting)";
        }
        match (self.on_gpu, gpu_allowed_by_env(), self.gpu_failed.is_some()) {
            (true, _, _) => "hardware",
            (false, false, _) => "software (TSUBAME_GPURENDER=0)",
            (false, true, true) => "software (GPU fallback)",
            (false, true, false) => "software",
        }
    }

    fn create_target(&self, gpu: bool) -> Result<ID2D1HwndRenderTarget> {
        unsafe {
            let mut rc = RECT::default();
            windows::Win32::UI::WindowsAndMessaging::GetClientRect(self.hwnd, &mut rc)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                // HARDWARE, not DEFAULT: DEFAULT quietly falls back to
                // software by itself, and we want to know which one we got.
                r#type: if gpu { D2D1_RENDER_TARGET_TYPE_HARDWARE } else { D2D1_RENDER_TARGET_TYPE_SOFTWARE },
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: self.dpi,
                dpiY: self.dpi,
                ..Default::default()
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: self.hwnd,
                pixelSize: D2D_SIZE_U { width: rc.right.max(1) as u32, height: rc.bottom.max(1) as u32 },
                presentOptions: D2D1_PRESENT_OPTIONS_IMMEDIATELY,
            };
            self.factory.CreateHwndRenderTarget(&props, &hprops)
        }
    }

    /// GPU by default. Software (WARP) when `TSUBAME_GPURENDER=0`, when the GPU
    /// target cannot be made, and for `GPU_RETRY` after the device was lost:
    /// a hung or resetting GPU driver is when a task manager is needed most.
    /// Also software while the GPU device warms up on a worker (`warm`), so
    /// an open never waits for the driver.
    fn ensure_target(&mut self) -> Result<()> {
        self.create_ms = 0.0;
        if self.rt.is_some() {
            // a software stand-in while the GPU warms up: swap once it is done
            if !self.warming || matches!(warm::status(), warm::State::Idle | warm::State::Running) {
                return Ok(());
            }
            self.drop_target();
            // the factory caches the stand-in's WARP device (~40 MB private
            // with its buffers): a fresh one lets it go
            self.fresh_factory();
        }
        let t0 = std::time::Instant::now();
        let mut gpu = want_gpu(gpu_allowed_by_env(), self.gpu_failed, t0);
        self.warming = false;
        let mut warmed_ms = None;
        if gpu {
            match warm::status() {
                // started by `end` once the stand-in has shown a frame:
                // started now, it holds up the software target too (both
                // wait on the same D3D device creation, ~250 ms)
                warm::State::Idle | warm::State::Running => (gpu, self.warming) = (false, true),
                warm::State::Failed(code) => {
                    warm::reset();
                    self.gpu_failed = Some(t0);
                    crate::log(&format!("render: GPU device failed {code:?}, using software"));
                    gpu = false;
                }
                warm::State::Ready(ms) => warmed_ms = Some(ms),
            }
        }
        let rt = match self.create_target(gpu) {
            Ok(rt) => rt,
            Err(e) if gpu => {
                warm::reset();
                self.gpu_failed = Some(t0);
                crate::log(&format!("render: GPU target failed {:?}, using software", e.code()));
                gpu = false;
                self.create_target(false)?
            }
            Err(e) => return Err(e),
        };
        self.on_gpu = gpu;
        if gpu {
            self.gpu_failed = None;
            // the target has its own device now, which keeps the driver loaded
            warm::reset();
        }
        unsafe {
            rt.SetDpi(self.dpi, self.dpi);
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            self.brush = Some(rt.CreateSolidColorBrush(&rgb(0xffffff), None)?);
        }
        self.rt = Some(rt);
        self.create_ms = t0.elapsed().as_secs_f32() * 1000.0;
        if let Some(w) = warmed_ms {
            crate::log(&format!("render: GPU target made in {:.1} ms (device warmed on a worker in {w:.0} ms)", self.create_ms));
        }
        // the first target, then each change of path (not every open, and
        // not the stand-in)
        static LAST: std::sync::Mutex<&str> = std::sync::Mutex::new("");
        let path = self.path();
        if let (Ok(mut last), false) = (LAST.lock(), self.warming) {
            if *last != path {
                *last = path;
                // the software target rasterises with WARP (d3d10warp.dll) and its worker threads
                let warp = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(w!("d3d10warp.dll")).is_ok() };
                crate::log(&format!("render: {path} Direct2D target, WARP loaded={warp}, created in {:.1} ms", self.create_ms));
            }
        }
        Ok(())
    }

    /// Drop the render target and everything made from it. The GPU driver
    /// keeps tens of MB committed per device; a hidden resident app should
    /// not. Recreated lazily on the next paint.
    pub fn release(&mut self) {
        self.drop_target();
        self.warming = false;
        warm::reset();
        self.formats.clear();
        self.fresh_factory();
    }

    /// The factory caches its D3D device; only a fresh factory lets it go.
    fn fresh_factory(&mut self) {
        if let Ok(f) = unsafe { D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) } {
            self.factory = f;
        }
    }

    fn drop_target(&mut self) {
        self.bitmaps.clear();
        self.brush = None;
        self.rt = None;
        self.on_gpu = false;
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if let Some(rt) = &self.rt {
            unsafe {
                let _ = rt.Resize(&D2D_SIZE_U { width: w.max(1), height: h.max(1) });
            }
        }
    }

    pub fn set_dpi(&mut self, dpi: f32) {
        self.dpi = dpi;
        if let Some(rt) = &self.rt {
            unsafe { rt.SetDpi(dpi, dpi) };
        }
    }

    /// Logical (DIP) size of the client area.
    pub fn size(&self) -> (f32, f32) {
        let mut rc = RECT::default();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(self.hwnd, &mut rc);
        }
        let s = 96.0 / self.dpi;
        (rc.right as f32 * s, rc.bottom as f32 * s)
    }

    pub fn begin(&mut self, bg: u32) -> bool {
        if self.ensure_target().is_err() {
            return false;
        }
        self.frame = self.frame.wrapping_add(1);
        // drop layouts no frame drew in the last 30 (values that changed)
        if self.frame.is_multiple_of(30) || self.layouts.len() > LAYOUT_CAP {
            let f = self.frame;
            self.layouts.retain(|_, l| f.wrapping_sub(l.used) < 30);
            if self.layouts.len() > LAYOUT_CAP {
                self.layouts.clear();
            }
        }
        let rt = self.rt.as_ref().unwrap();
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&rgb(bg)));
        }
        true
    }

    pub fn end(&mut self) {
        let Some(rt) = &self.rt else { return };
        let t0 = std::time::Instant::now();
        let r = unsafe { rt.EndDraw(None, None) };
        self.end_ms = t0.elapsed().as_secs_f32() * 1000.0;
        // debug builds: TSUBAME_TEST_DEVICELOST=<n> reports frame n as a lost device
        #[cfg(debug_assertions)]
        let r = {
            static FRAME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = FRAME.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if std::env::var("TSUBAME_TEST_DEVICELOST").ok().and_then(|v| v.parse::<u32>().ok()) == Some(n) { Err(Error::from(D2DERR_RECREATE_TARGET)) } else { r }
        };
        let Err(e) = r else {
            if self.warming {
                warm::start(self.hwnd.0 as isize);
            }
            return;
        };
        let lost = e.code() == D2DERR_RECREATE_TARGET;
        let path = self.path();
        if lost {
            // device lost (driver reset or update, adapter removed, TDR): a GPU
            // target falls back to software for a while; either way a new
            // target is made on the paint asked for here
            if self.on_gpu {
                self.gpu_failed = Some(t0);
            }
            self.drop_target();
            // let the dead device go (the factory caches it)
            self.fresh_factory();
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.hwnd), None, false);
            }
        }
        let next = if !lost {
            ""
        } else if want_gpu(gpu_allowed_by_env(), self.gpu_failed, t0) {
            "hardware"
        } else {
            "software"
        };
        crate::log(&format!(
            "device: EndDraw failed {:?} ({path} target){}",
            e.code(),
            if lost { format!(", target lost: a {next} one is made on the next paint") } else { String::new() }
        ));
    }

    fn br(&self, c: D2D1_COLOR_F) -> &ID2D1SolidColorBrush {
        let b = self.brush.as_ref().unwrap();
        unsafe { b.SetColor(&c) };
        b
    }
    fn rt(&self) -> &ID2D1HwndRenderTarget {
        self.rt.as_ref().unwrap()
    }

    pub fn fill(&self, r: Rect, c: u32) {
        self.fill_a(r, c, 1.0)
    }
    pub fn fill_a(&self, r: Rect, c: u32, a: f32) {
        unsafe { self.rt().FillRectangle(&r.d2(), self.br(rgba(c, a))) }
    }
    pub fn round(&self, r: Rect, rad: f32, c: u32) {
        self.round_a(r, rad, c, 1.0)
    }
    pub fn round_a(&self, r: Rect, rad: f32, c: u32, a: f32) {
        let rr = D2D1_ROUNDED_RECT { rect: r.d2(), radiusX: rad, radiusY: rad };
        unsafe { self.rt().FillRoundedRectangle(&rr, self.br(rgba(c, a))) }
    }
    pub fn round_stroke(&self, r: Rect, rad: f32, c: u32, w: f32) {
        let r = r.inset(w / 2.0, w / 2.0);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2(), radiusX: rad, radiusY: rad };
        unsafe { self.rt().DrawRoundedRectangle(&rr, self.br(rgb(c)), w, None) }
    }
    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: u32, w: f32) {
        unsafe {
            self.rt().DrawLine(
                D2D_POINT_2F { X: x0, Y: y0 },
                D2D_POINT_2F { X: x1, Y: y1 },
                self.br(rgb(c)),
                w,
                None,
            )
        }
    }
    pub fn circle(&self, x: f32, y: f32, r: f32, c: u32) {
        let e = D2D1_ELLIPSE { point: D2D_POINT_2F { X: x, Y: y }, radiusX: r, radiusY: r };
        unsafe { self.rt().FillEllipse(&e, self.br(rgb(c))) }
    }

    fn format(&mut self, font: Font) -> IDWriteTextFormat {
        if let Some(f) = self.formats.get(&font) {
            return f.clone();
        }
        let fam = if font.icon {
            w!("Segoe Fluent Icons")
        } else if font.mono {
            w!("Cascadia Mono")
        } else {
            w!("Segoe UI Variable Text")
        };
        let weight = if font.bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        unsafe {
            let fmt = self
                .dw
                .CreateTextFormat(fam, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, font.size10 as f32 / 10.0, w!("en-us"))
                .or_else(|_| {
                    let fb = if font.icon { w!("Segoe MDL2 Assets") } else { w!("Segoe UI") };
                    self.dw.CreateTextFormat(fb, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, font.size10 as f32 / 10.0, w!("en-us"))
                })
                .unwrap();
            let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = fmt.SetTextAlignment(match font.align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            });
            if let Ok(sign) = self.dw.CreateEllipsisTrimmingSign(&fmt) {
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                let _ = fmt.SetTrimming(&trim, &sign);
            }
            self.formats.insert(font, fmt.clone());
            fmt
        }
    }

    /// Draws single-line text vertically centred in `r`, clipped and ellipsised.
    pub fn text(&mut self, s: &str, r: Rect, font: Font, c: u32) {
        if s.is_empty() || r.w <= 1.0 {
            return;
        }
        use std::hash::{Hash, Hasher};
        // box size in quarter DIPs: the layout's max width / height
        let (w4, h4) = ((r.w * 4.0).round() as u32, (r.h * 4.0).round() as u32);
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        (s, font, w4, h4).hash(&mut hs);
        let key = hs.finish();
        let frame = self.frame;
        let hit = self.layouts.get_mut(&key).filter(|l| &*l.s == s && l.font == font && l.w4 == w4 && l.h4 == h4).map(|l| {
            l.used = frame;
            l.l.clone()
        });
        let layout = match hit {
            Some(l) => l,
            None => {
                let fmt = self.format(font);
                let wide: Vec<u16> = s.encode_utf16().collect();
                let Ok(l) = (unsafe { self.dw.CreateTextLayout(&wide, &fmt, w4 as f32 / 4.0, h4 as f32 / 4.0) }) else { return };
                self.layouts.insert(key, Layout { s: s.into(), font, w4, h4, l: l.clone(), used: frame });
                l
            }
        };
        unsafe { self.rt().DrawTextLayout(D2D_POINT_2F { X: r.x, Y: r.y }, &layout, self.br(rgb(c)), D2D1_DRAW_TEXT_OPTIONS_CLIP) }
    }

    /// Width of `s` in DIPs.
    pub fn measure(&mut self, s: &str, font: Font) -> f32 {
        let fmt = self.format(font);
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            match self.dw.CreateTextLayout(&wide, &fmt, 10000.0, 100.0) {
                Ok(l) => {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    let _ = l.GetMetrics(&mut m);
                    m.widthIncludingTrailingWhitespace
                }
                Err(_) => 0.0,
            }
        }
    }

    /// Draws premultiplied BGRA pixels scaled into `r`. The D2D bitmap is
    /// made once per `key` and kept until the render target goes.
    pub fn image(&mut self, key: &std::sync::Arc<str>, w: u32, h: u32, px: &[u32], r: Rect) {
        if !self.bitmaps.contains_key(key) {
            if self.bitmaps.len() >= 320 {
                self.bitmaps.clear();
            }
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let Ok(b) = (unsafe { self.rt().CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(px.as_ptr() as *const _), w * 4, &props) }) else { return };
            self.bitmaps.insert(key.clone(), b);
        }
        if let Some(b) = self.bitmaps.get(key) {
            unsafe { self.rt().DrawBitmap(b, Some(&r.d2()), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None) };
        }
    }

    pub fn clip(&self, r: Rect) {
        unsafe { self.rt().PushAxisAlignedClip(&r.d2(), D2D1_ANTIALIAS_MODE_ALIASED) }
    }
    pub fn unclip(&self) {
        unsafe { self.rt().PopAxisAlignedClip() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn gpu_unless_forced_off_or_recently_failed() {
        let now = Instant::now() + Duration::from_secs(3600);
        assert!(want_gpu(true, None, now));
        assert!(!want_gpu(false, None, now), "TSUBAME_GPURENDER=0 forces software");
        assert!(!want_gpu(true, Some(now - Duration::from_secs(10)), now), "software right after a GPU failure");
        assert!(want_gpu(true, Some(now - GPU_RETRY), now), "GPU again once GPU_RETRY has passed");
        assert!(!want_gpu(false, Some(now - GPU_RETRY), now));
    }
}
