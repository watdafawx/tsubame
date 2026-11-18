//! Thin Direct2D / DirectWrite layer. One render target, one reusable brush,
//! text formats created lazily and cached by (size, weight, family, alignment).

use std::collections::HashMap;
use std::sync::Arc;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows_numerics::Vector2 as D2D_POINT_2F;

pub use tsubame_ui::{Align, Canvas, Face, Font, Rect, Weight, f};

fn d2(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.r(), bottom: r.b() }
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

/// Device-side image cache cap. Every device copy is also what the GPU (or
/// WARP) holds in memory, so this is a share of the process budget. Thumbnails
/// live only here once drawn (the app drops its CPU pixels after the upload),
/// so keep it above the app's thumbnail cap (12 MB) plus the icons in use.
/// See docs/icons-thumbnails.md.
pub const BITMAP_CAP_BYTES: usize = 16 << 20;

struct CachedBitmap {
    bmp: ID2D1Bitmap,
    bytes: usize,
    /// `Gfx::frame` it was last drawn in
    used: u32,
}

pub struct Gfx {
    factory: ID2D1Factory,
    dw: IDWriteFactory,
    rt: Option<ID2D1RenderTarget>,
    /// the target as a D2D device context on our own flip-model swap chain, or
    flip: Option<Flip>,
    /// as an HWND target (`TSUBAME_FLIP=0`, or the flip chain failed)
    hwnd_rt: Option<ID2D1HwndRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    formats: HashMap<Font, IDWriteTextFormat>,
    /// shaped text by (string, font, box size): `text` reuses a layout while
    /// the same string is drawn frame after frame (DrawText reshapes it on
    /// every call). Device-independent, so it survives a lost target.
    layouts: HashMap<u64, Layout>,
    /// frame counter for `layouts` eviction
    frame: u32,
    /// images (icons, thumbnails) by cache key; tied to the render target.
    /// Bounded in bytes (`BITMAP_CAP_BYTES`), least recently drawn go first.
    bitmaps: HashMap<Arc<str>, CachedBitmap>,
    bitmap_bytes: usize,
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

/// Most layouts `Gfx::text` keeps. Blink used 4096 / 30 frames (a busy frame
/// draws ~400 strings); a scrolling list draws mostly new strings each frame, so
/// that only held ~70 MB of dead layouts (spike S1, docs/spike-d2d.md).
const LAYOUT_CAP: usize = 1024;
/// Frames a layout survives without being drawn.
const LAYOUT_AGE: u32 = 4;

fn layout_age() -> u32 {
    static C: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *C.get_or_init(|| std::env::var("TSUBAME_LAYOUT_AGE").ok().and_then(|v| v.parse().ok()).unwrap_or(LAYOUT_AGE).max(1))
}

/// Layout cache cap. `TSUBAME_LAYOUT_CAP=<n>` overrides it (spike experiment;
/// 0 turns the cache off).
fn layout_cap() -> usize {
    static C: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *C.get_or_init(|| std::env::var("TSUBAME_LAYOUT_CAP").ok().and_then(|v| v.parse().ok()).unwrap_or(LAYOUT_CAP))
}

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
/// when the warm device exists makes its GPU target on it (the flip-model
/// path) or next to it (an HWND target: the driver is loaded by then, ~40 ms
/// instead of ~200, and the warm device is dropped).
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
    struct Dev(ID3D11Device);
    // SAFETY: D3D11 devices are free-threaded; this one is made on the worker,
    // then dropped or handed to the UI thread (`take`), which alone uses it.
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
            // no driver worker threads (as D2D makes its own devices): with
            // them the NVIDIA driver started ~70 threads and idle CPU rose
            let r = unsafe { D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS, None, D3D11_SDK_VERSION, Some(&mut dev), None, None) };
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
        if std::thread::Builder::new().name("gpu warm-up".into()).spawn(work).is_err()
            && let Ok(mut s) = SLOT.lock() {
                s.1 = State::Failed(E_FAIL);
            }
    }

    /// The warm device, if it is made (the slot keeps the Ready state).
    pub fn take() -> Option<ID3D11Device> {
        SLOT.lock().ok().and_then(|mut s| s.2.take()).map(|d| d.0)
    }

    /// Forget the warm device, and any being made.
    pub fn reset() {
        if let Ok(mut s) = SLOT.lock() {
            (s.0, s.1, s.2) = (s.0 + 1, State::Idle, None);
        }
    }
}

/// The first stand-in's WARP device, made on a worker from process start so
/// its creation overlaps window and folder setup (`prewarm_software`).
mod soft {
    use std::sync::Mutex;
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
    use windows::Win32::Graphics::Direct3D11::*;

    struct Dev(ID3D11Device);
    // SAFETY: D3D11 devices are free-threaded; made on the worker, then only
    // the UI thread uses it.
    unsafe impl Send for Dev {}
    static JOB: Mutex<Option<JoinHandle<Option<Dev>>>> = Mutex::new(None);

    pub fn start() {
        let Ok(mut job) = JOB.lock() else { return };
        if job.is_some() {
            return;
        }
        *job = std::thread::Builder::new()
            .name("warp warm-up".into())
            .spawn(|| {
                let mut dev = None;
                unsafe { D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, None) }.ok()?;
                dev.map(Dev)
            })
            .ok();
    }

    /// The device, waiting for the worker if it is still making it; None
    /// when none was started (or it failed).
    pub fn take() -> Option<ID3D11Device> {
        let job = JOB.lock().ok()?.take()?;
        job.join().ok().flatten().map(|d| d.0)
    }
}

/// Start making the software stand-in's device now (call once, early in
/// `main`): the first frame then only adds its swap chain (~5-10 ms saved).
pub fn prewarm_software() {
    if flip_enabled() {
        soft::start();
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

/// A flip-model swap chain on our own D3D11 device, drawn with a D2D device
/// context. An HWND render target makes a GDI-compatible blt-model swap chain
/// instead: the driver keeps a system-memory copy of the window (~32 MB
/// private at 4K), and its WARP stand-in left one more (~30 MB) behind after
/// the GPU swap (docs/memory.md).
struct Flip {
    d3d: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    swap: windows::Win32::Graphics::Dxgi::IDXGISwapChain1,
    ctx: ID2D1DeviceContext,
}

/// `TSUBAME_FLIP=0` uses an HWND render target instead (A/B measurements).
fn flip_enabled() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("TSUBAME_FLIP").is_none_or(|v| v != "0"))
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
                flip: None,
                hwnd_rt: None,
                brush: None,
                formats: HashMap::new(),
                layouts: HashMap::new(),
                frame: 0,
                bitmaps: HashMap::new(),
                bitmap_bytes: 0,
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

    /// Cache sizes for reports: (text layouts, text formats, bitmaps).
    pub fn cache_counts(&self) -> (usize, usize, usize) {
        (self.layouts.len(), self.formats.len(), self.bitmaps.len())
    }

    /// Bytes the device-side image cache holds (BGRA, 4 per pixel).
    pub fn bitmap_bytes(&self) -> usize {
        self.bitmap_bytes
    }

    /// Makes room for `need` more bytes: drops the least recently drawn
    /// images down to 90% of the cap (so this runs once per several uploads,
    /// not once per upload).
    fn evict_bitmaps(&mut self, need: usize) {
        if self.bitmap_bytes + need <= BITMAP_CAP_BYTES {
            return;
        }
        let target = (BITMAP_CAP_BYTES / 10 * 9).saturating_sub(need);
        let now = self.frame;
        let mut order: Vec<(u32, Arc<str>)> = self.bitmaps.iter().map(|(k, v)| (now.wrapping_sub(v.used), k.clone())).collect();
        // oldest (largest age) first
        order.sort_unstable_by_key(|a| std::cmp::Reverse(a.0));
        for (_, k) in order {
            if self.bitmap_bytes <= target {
                break;
            }
            if let Some(v) = self.bitmaps.remove(&k) {
                self.bitmap_bytes -= v.bytes;
            }
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

    fn create_target(&mut self, gpu: bool) -> Result<ID2D1RenderTarget> {
        if flip_enabled() {
            match self.create_flip(gpu) {
                Ok(rt) => return Ok(rt),
                Err(e) => crate::log(&format!("render: flip-model swap chain failed {:?}, using an HWND target", e.code())),
            }
        }
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
            let h = self.factory.CreateHwndRenderTarget(&props, &hprops)?;
            let rt: ID2D1RenderTarget = h.cast()?;
            self.hwnd_rt = Some(h);
            Ok(rt)
        }
    }

    /// A D3D11 device (the warm one when it is made; WARP for software), a
    /// flip-model swap chain on the window and a D2D device context on it.
    fn create_flip(&mut self, gpu: bool) -> Result<ID2D1RenderTarget> {
        use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
        use windows::Win32::Graphics::Direct3D11::*;
        use windows::Win32::Graphics::Dxgi::*;
        unsafe {
            let d3d = match if gpu { warm::take() } else { soft::take() } {
                Some(d) => d,
                None => {
                    let mut dev = None;
                    let kind = if gpu { D3D_DRIVER_TYPE_HARDWARE } else { D3D_DRIVER_TYPE_WARP };
                    // WARP rasterises on its worker threads (the stand-in's
                    // first frame: ~16 ms of drawing instead of ~24); a GPU
                    // driver's threads only cost idle wake-ups and memory
                    let flags = if gpu { D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS } else { D3D11_CREATE_DEVICE_BGRA_SUPPORT };
                    D3D11CreateDevice(None, kind, HMODULE::default(), flags, None, D3D11_SDK_VERSION, Some(&mut dev), None, None)?;
                    dev.ok_or_else(|| Error::from(E_FAIL))?
                }
            };
            let dxgi: IDXGIDevice = d3d.cast()?;
            let f1: ID2D1Factory1 = self.factory.cast()?;
            let ctx = f1.CreateDevice(&dxgi)?.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let dxf: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
            let mut rc = RECT::default();
            windows::Win32::UI::WindowsAndMessaging::GetClientRect(self.hwnd, &mut rc)?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: rc.right.max(1) as u32,
                Height: rc.bottom.max(1) as u32,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let swap = dxf.CreateSwapChainForHwnd(&d3d, self.hwnd, &desc, None, None)?;
            let _ = dxf.MakeWindowAssociation(self.hwnd, DXGI_MWA_NO_ALT_ENTER);
            self.flip_target(&ctx, &swap)?;
            let rt: ID2D1RenderTarget = ctx.cast()?;
            self.flip = Some(Flip { d3d, swap, ctx });
            Ok(rt)
        }
    }

    /// Points the device context at the swap chain's back buffer.
    fn flip_target(&self, ctx: &ID2D1DeviceContext, swap: &windows::Win32::Graphics::Dxgi::IDXGISwapChain1) -> Result<()> {
        unsafe {
            let surf: windows::Win32::Graphics::Dxgi::IDXGISurface = swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: self.dpi,
                dpiY: self.dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bmp = ctx.CreateBitmapFromDxgiSurface(&surf, Some(&props))?;
            ctx.SetTarget(&bmp);
        }
        Ok(())
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
            // an HWND target's factory caches its WARP device (~40 MB
            // private with its buffers): a fresh one lets it go
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
            // the target holds a device now, which keeps the driver loaded
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
        if let (Ok(mut last), false) = (LAST.lock(), self.warming)
            && *last != path {
                *last = path;
                // the software target rasterises with WARP (d3d10warp.dll) and its worker threads
                let warp = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(w!("d3d10warp.dll")).is_ok() };
                let model = if self.flip.is_some() { "flip-model swap chain" } else { "HWND target" };
                crate::log(&format!("render: {path} Direct2D target ({model}), WARP loaded={warp}, created in {:.1} ms", self.create_ms));
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
        self.bitmap_bytes = 0;
        self.brush = None;
        self.rt = None;
        self.hwnd_rt = None;
        if let Some(f) = self.flip.take() {
            // the swap chain must be gone before the next one is made on the
            // window: release the back buffer and flush deferred destruction
            unsafe { f.ctx.SetTarget(None) };
            let Flip { d3d, swap, ctx } = f;
            drop((ctx, swap));
            if let Ok(c) = unsafe { d3d.GetImmediateContext() } {
                unsafe {
                    c.ClearState();
                    c.Flush();
                }
            }
        }
        self.on_gpu = false;
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if let Some(f) = &self.flip {
            use windows::Win32::Graphics::Dxgi::DXGI_SWAP_CHAIN_FLAG;
            unsafe { f.ctx.SetTarget(None) };
            let ok = unsafe { f.swap.ResizeBuffers(0, w.max(1), h.max(1), DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0)) }.is_ok() && self.flip_target(&f.ctx, &f.swap).is_ok();
            if !ok {
                // remade on the next paint
                self.drop_target();
            }
            return;
        }
        if let Some(rt) = &self.hwnd_rt {
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

    fn br(&self, c: D2D1_COLOR_F) -> &ID2D1SolidColorBrush {
        let b = self.brush.as_ref().unwrap();
        unsafe { b.SetColor(&c) };
        b
    }
    fn rt(&self) -> &ID2D1RenderTarget {
        self.rt.as_ref().unwrap()
    }

    fn format(&mut self, font: Font) -> IDWriteTextFormat {
        if let Some(f) = self.formats.get(&font) {
            return f.clone();
        }
        let fam = match font.face {
            Face::Icon => w!("Segoe Fluent Icons"),
            Face::Mono => w!("Cascadia Mono"),
            Face::Ui => w!("Segoe UI Variable Text"),
        };
        let weight = match font.weight {
            Weight::Semibold => DWRITE_FONT_WEIGHT_SEMI_BOLD,
            Weight::Normal => DWRITE_FONT_WEIGHT_NORMAL,
        };
        unsafe {
            let fmt = self
                .dw
                .CreateTextFormat(fam, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, font.size10 as f32 / 10.0, w!("en-us"))
                .or_else(|_| {
                    let fb = if font.face == Face::Icon { w!("Segoe MDL2 Assets") } else { w!("Segoe UI") };
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

    /// Shaped layout of `s` (cached like `text`, box `w` x `h`).
    fn layout(&mut self, s: &str, font: Font, w: f32, h: f32) -> Option<IDWriteTextLayout> {
        use std::hash::{Hash, Hasher};
        // box size in quarter DIPs: the layout's max width / height
        let (w4, h4) = ((w * 4.0).round() as u32, (h * 4.0).round() as u32);
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        (s, font, w4, h4).hash(&mut hs);
        let key = hs.finish();
        let frame = self.frame;
        if let Some(l) = self.layouts.get_mut(&key).filter(|l| &*l.s == s && l.font == font && l.w4 == w4 && l.h4 == h4) {
            l.used = frame;
            return Some(l.l.clone());
        }
        let fmt = self.format(font);
        let wide: Vec<u16> = s.encode_utf16().collect();
        let l = unsafe { self.dw.CreateTextLayout(&wide, &fmt, w4 as f32 / 4.0, h4 as f32 / 4.0) }.ok()?;
        if layout_cap() > 0 {
            self.layouts.insert(key, Layout { s: s.into(), font, w4, h4, l: l.clone(), used: frame });
        }
        Some(l)
    }
}

impl Canvas for Gfx {
    /// Logical (DIP) size of the client area.
    fn size(&self) -> (f32, f32) {
        let mut rc = RECT::default();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(self.hwnd, &mut rc);
        }
        let s = 96.0 / self.dpi;
        (rc.right as f32 * s, rc.bottom as f32 * s)
    }

    fn begin(&mut self, bg: u32) -> bool {
        if self.ensure_target().is_err() {
            return false;
        }
        self.frame = self.frame.wrapping_add(1);
        // drop layouts no frame drew in the last `layout_age()` frames (values that changed)
        if self.frame.is_multiple_of(layout_age()) || self.layouts.len() > layout_cap() {
            let (f, age) = (self.frame, layout_age());
            self.layouts.retain(|_, l| f.wrapping_sub(l.used) < age);
            if self.layouts.len() > layout_cap() {
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

    fn end(&mut self) {
        let Some(rt) = &self.rt else { return };
        let t0 = std::time::Instant::now();
        let mut r = unsafe { rt.EndDraw(None, None) };
        if let (Ok(()), Some(f)) = (&r, &self.flip) {
            use windows::Win32::Graphics::Dxgi::*;
            let hr = unsafe { f.swap.Present(0, DXGI_PRESENT(0)) };
            if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
                r = Err(Error::from(D2DERR_RECREATE_TARGET));
            } else {
                r = hr.ok();
            }
        }
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

    fn fill_a(&mut self, r: Rect, c: u32, a: f32) {
        unsafe { self.rt().FillRectangle(&d2(r), self.br(rgba(c, a))) }
    }
    fn round_a(&mut self, r: Rect, rad: f32, c: u32, a: f32) {
        let rr = D2D1_ROUNDED_RECT { rect: d2(r), radiusX: rad, radiusY: rad };
        unsafe { self.rt().FillRoundedRectangle(&rr, self.br(rgba(c, a))) }
    }
    fn round_stroke(&mut self, r: Rect, rad: f32, c: u32, w: f32) {
        let r = r.inset(w / 2.0, w / 2.0);
        let rr = D2D1_ROUNDED_RECT { rect: d2(r), radiusX: rad, radiusY: rad };
        unsafe { self.rt().DrawRoundedRectangle(&rr, self.br(rgb(c)), w, None) }
    }
    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: u32, w: f32) {
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
    fn circle(&mut self, x: f32, y: f32, r: f32, c: u32) {
        let e = D2D1_ELLIPSE { point: D2D_POINT_2F { X: x, Y: y }, radiusX: r, radiusY: r };
        unsafe { self.rt().FillEllipse(&e, self.br(rgb(c))) }
    }

    /// Draws single-line text vertically centred in `r`, clipped and ellipsised.
    fn text(&mut self, s: &str, r: Rect, font: Font, c: u32) {
        if s.is_empty() || r.w <= 1.0 {
            return;
        }
        let Some(layout) = self.layout(s, font, r.w, r.h) else { return };
        unsafe { self.rt().DrawTextLayout(D2D_POINT_2F { X: r.x, Y: r.y }, &layout, self.br(rgb(c)), D2D1_DRAW_TEXT_OPTIONS_CLIP) }
    }

    /// Width of `s` in DIPs.
    fn measure(&mut self, s: &str, font: Font) -> f32 {
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

    /// First line of `s` wrapped to `w`, from the shaper's own line breaking.
    fn first_line_len(&mut self, s: &str, font: Font, w: f32) -> usize {
        if s.is_empty() || w <= 1.0 {
            return s.len();
        }
        let fmt = self.format(font.left());
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(l) = self.dw.CreateTextLayout(&wide, &fmt, w, 1000.0) else { return s.len() };
            let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            // the first call only counts the lines (it reports "buffer too small")
            let mut n = 0u32;
            let _ = l.GetLineMetrics(None, &mut n);
            if n < 2 {
                return s.len();
            }
            let mut lines = vec![DWRITE_LINE_METRICS::default(); n as usize];
            if l.GetLineMetrics(Some(&mut lines), &mut n).is_err() {
                return s.len();
            }
            // UTF-16 units of line 1 -> bytes
            let mut u = 0u32;
            for (i, ch) in s.char_indices() {
                if u >= lines[0].length {
                    return i;
                }
                u += ch.len_utf16() as u32;
            }
            s.len()
        }
    }

    /// Caret position from the shaped run (kerning and ligatures included).
    fn caret_x(&mut self, s: &str, font: Font, idx: usize) -> f32 {
        let idx = tsubame_ui::canvas::floor_char(s, idx);
        if idx == 0 || s.is_empty() {
            return 0.0;
        }
        let Some(l) = self.layout(s, font.left(), 100_000.0, 100.0) else { return 0.0 };
        let i16 = s[..idx].encode_utf16().count() as u32;
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        // the leading edge of char idx, or the trailing edge of the last one
        let r = if idx >= s.len() {
            let last = s[..idx].chars().next_back().map(|c| c.len_utf16() as u32).unwrap_or(1);
            unsafe { l.HitTestTextPosition(i16 - last, true, &mut x, &mut y, &mut m) }
        } else {
            unsafe { l.HitTestTextPosition(i16, false, &mut x, &mut y, &mut m) }
        };
        if r.is_err() { self.measure(&s[..idx], font) } else { x }
    }

    fn hit_index(&mut self, s: &str, font: Font, x: f32) -> usize {
        if s.is_empty() || x <= 0.0 {
            return 0;
        }
        let Some(l) = self.layout(s, font.left(), 100_000.0, 100.0) else { return 0 };
        let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        if unsafe { l.HitTestPoint(x, 50.0, &mut trailing, &mut inside, &mut m) }.is_err() {
            return 0;
        }
        let pos16 = m.textPosition as usize + if trailing.as_bool() { m.length as usize } else { 0 };
        // UTF-16 position back to a byte index
        let mut n16 = 0usize;
        for (b, c) in s.char_indices() {
            if n16 >= pos16 {
                return b;
            }
            n16 += c.len_utf16();
        }
        s.len()
    }

    /// Draws premultiplied BGRA pixels scaled into `r`. The D2D bitmap is
    /// made once per `key` and kept until the render target goes.
    fn image(&mut self, key: &str, w: u32, h: u32, px: &[u32], r: Rect) {
        self.image_a(key, w, h, px, r, 1.0)
    }

    fn image_a(&mut self, key: &str, w: u32, h: u32, px: &[u32], r: Rect, a: f32) {
        if !self.bitmaps.contains_key(key) {
            if w == 0 || h == 0 || px.len() < (w as usize) * (h as usize) {
                return;
            }
            let bytes = w as usize * h as usize * 4;
            self.evict_bitmaps(bytes);
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let Ok(bmp) = (unsafe { self.rt().CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(px.as_ptr() as *const _), w * 4, &props) }) else { return };
            self.bitmap_bytes += bytes;
            self.bitmaps.insert(Arc::from(key), CachedBitmap { bmp, bytes, used: self.frame });
        }
        let frame = self.frame;
        if let Some(b) = self.bitmaps.get_mut(key) {
            b.used = frame;
        }
        if let Some(b) = self.bitmaps.get(key) {
            unsafe { self.rt().DrawBitmap(&b.bmp, Some(&d2(r)), a.clamp(0.0, 1.0), D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None) };
        }
    }

    fn has_image(&self, key: &str) -> bool {
        self.bitmaps.contains_key(key)
    }

    fn clip(&mut self, r: Rect) {
        unsafe { self.rt().PushAxisAlignedClip(&d2(r), D2D1_ANTIALIAS_MODE_ALIASED) }
    }
    fn unclip(&mut self) {
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
