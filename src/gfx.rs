//! Direct3D / DirectComposition / Direct2D plumbing.
//!
//! The window has no redirection bitmap; its pixels come from a premultiplied-alpha
//! composition swap chain that Direct2D draws into. That gives real per-pixel
//! transparency and lets the window cost nothing while nothing is drawn.

use crate::art::{ART_PX, Art};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext,
    ID2D1Factory1, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, Result, w};

/// Text styles used by the notch.
pub struct Fonts {
    pub title: IDWriteTextFormat,
    pub body: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
    pub lyric_center: IDWriteTextFormat,
    /// Large numbers on the card: clock, timer, water count.
    pub big: IDWriteTextFormat,
    pub icon: IDWriteTextFormat,
    /// Small icons drawn inside other controls (check mark, delete cross).
    pub icon_small: IDWriteTextFormat,
    pub icon_large: IDWriteTextFormat,
}

pub struct Gfx {
    pub ctx: ID2D1DeviceContext,
    pub dwrite: IDWriteFactory,
    pub brush: ID2D1SolidColorBrush,
    pub fonts: Fonts,
    pub factory: ID2D1Factory1,
    swap: IDXGISwapChain1,
    dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    _d3d: ID3D11Device,
    dpi: f32,
}

impl Gfx {
    /// Creates every device-dependent object for `hwnd`, sized to `width` x `height` pixels.
    pub fn new(hwnd: HWND, width: u32, height: u32, dpi: f32) -> Result<Self> {
        let d3d = create_d3d_device()?;
        let dxgi_device: IDXGIDevice = d3d.cast()?;

        let factory: ID2D1Factory1 = unsafe {
            D2D1CreateFactory(
                D2D1_FACTORY_TYPE_SINGLE_THREADED,
                Some(&D2D1_FACTORY_OPTIONS::default()),
            )?
        };
        let device = unsafe { factory.CreateDevice(&dxgi_device)? };
        let ctx = unsafe { device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };

        let dxgi_factory: IDXGIFactory2 =
            unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))? };
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            ..Default::default()
        };
        let swap =
            unsafe { dxgi_factory.CreateSwapChainForComposition(&dxgi_device, &desc, None)? };

        let dcomp: IDCompositionDevice = unsafe { DCompositionCreateDevice(&dxgi_device)? };
        let target = unsafe { dcomp.CreateTargetForHwnd(hwnd, true)? };
        let visual = unsafe { dcomp.CreateVisual()? };
        unsafe {
            visual.SetContent(&swap)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;
        }

        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let fonts = Fonts::new(&dwrite)?;
        let brush = unsafe { ctx.CreateSolidColorBrush(&rgba(1.0, 1.0, 1.0, 1.0), None)? };

        let gfx = Self {
            ctx,
            dwrite,
            brush,
            fonts,
            factory,
            swap,
            dcomp,
            _target: target,
            _visual: visual,
            _d3d: d3d,
            dpi,
        };
        gfx.bind_back_buffer()?;
        Ok(gfx)
    }

    /// Resizes the swap chain after a DPI or monitor change.
    pub fn resize(&mut self, width: u32, height: u32, dpi: f32) -> Result<()> {
        self.dpi = dpi;
        unsafe {
            // The back buffer can't be resized while Direct2D still holds it.
            self.ctx.SetTarget(None);
            self.swap.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }
        self.bind_back_buffer()
    }

    fn bind_back_buffer(&self) -> Result<()> {
        unsafe {
            let surface: IDXGISurface = self.swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: self.dpi,
                dpiY: self.dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bitmap = self
                .ctx
                .CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.ctx.SetTarget(&bitmap);
            self.ctx.SetDpi(self.dpi, self.dpi);
        }
        Ok(())
    }

    pub fn begin(&self) {
        unsafe {
            self.ctx.BeginDraw();
            self.ctx.Clear(Some(&rgba(0.0, 0.0, 0.0, 0.0)));
        }
    }

    /// Finishes the frame. An error means the device was lost and `Gfx` must be rebuilt.
    pub fn end(&self) -> Result<()> {
        unsafe {
            self.ctx.EndDraw(None, None)?;
            self.swap.Present(1, DXGI_PRESENT(0)).ok()?;
            self.dcomp.Commit()?;
        }
        Ok(())
    }

    /// Uploads decoded album art. It is drawn scaled, so its own DPI is left at 96.
    pub fn create_art_bitmap(&self, art: &Art) -> Result<ID2D1Bitmap1> {
        self.create_bitmap(ART_PX, &art.pixels)
    }

    /// Uploads a square of premultiplied BGRA pixels, `side` pixels wide.
    pub fn create_bitmap(&self, side: u32, pixels: &[u8]) -> Result<ID2D1Bitmap1> {
        if pixels.len() < (side * side * 4) as usize {
            return Err(windows::Win32::Foundation::E_INVALIDARG.into());
        }
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        unsafe {
            self.ctx.CreateBitmap(
                D2D_SIZE_U {
                    width: side,
                    height: side,
                },
                Some(pixels.as_ptr().cast()),
                side * 4,
                &props,
            )
        }
    }

    /// Width and height of `text` laid out in `format`, in DIPs.
    pub fn measure(&self, text: &str, format: &IDWriteTextFormat) -> (f32, f32) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let layout = unsafe {
            self.dwrite
                .CreateTextLayout(&wide, format, 10_000.0, 1_000.0)
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        match layout {
            Ok(layout) if unsafe { layout.GetMetrics(&mut metrics) }.is_ok() => {
                (metrics.widthIncludingTrailingWhitespace, metrics.height)
            }
            _ => (0.0, 0.0),
        }
    }
}

impl Fonts {
    fn new(dwrite: &IDWriteFactory) -> Result<Self> {
        let text = w!("Segoe UI Variable Text");
        let display = w!("Segoe UI Variable Display");
        let icons = icon_font_family(dwrite);
        let icons = PCWSTR(icons.as_ptr());

        let title = make_format(dwrite, display, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let body = make_format(dwrite, text, 12.5, DWRITE_FONT_WEIGHT_NORMAL)?;
        let small = make_format(dwrite, text, 11.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let lyric_center = make_format(dwrite, display, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let big = make_format(dwrite, display, 34.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        let icon_small = make_format(dwrite, icons, 10.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let icon = make_format(dwrite, icons, 15.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let icon_large = make_format(dwrite, icons, 20.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        unsafe {
            for format in [&icon, &icon_small, &icon_large, &lyric_center] {
                format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            }
            for format in [&title, &body, &small, &big] {
                format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            }
        }
        Ok(Self {
            title,
            body,
            small,
            lyric_center,
            big,
            icon,
            icon_small,
            icon_large,
        })
    }
}

fn make_format(
    dwrite: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = dwrite.CreateTextFormat(
            family,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-us"),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        let sign = dwrite.CreateEllipsisTrimmingSign(&format)?;
        format.SetTrimming(&trimming, &sign)?;
        Ok(format)
    }
}

/// Windows 11 ships Segoe Fluent Icons; Windows 10 only has Segoe MDL2 Assets.
/// Both use the same code points for the glyphs BUKU draws.
fn icon_font_family(dwrite: &IDWriteFactory) -> HSTRING {
    let fluent = HSTRING::from("Segoe Fluent Icons");
    let found = unsafe {
        let mut collection = None;
        if dwrite
            .GetSystemFontCollection(&mut collection, false)
            .is_err()
        {
            None
        } else {
            collection.and_then(|collection| {
                let mut index = 0;
                let mut exists = BOOL(0);
                collection
                    .FindFamilyName(&fluent, &mut index, &mut exists)
                    .ok()?;
                exists.as_bool().then_some(())
            })
        }
    };
    if found.is_some() {
        fluent
    } else {
        HSTRING::from("Segoe MDL2 Assets")
    }
}

fn create_d3d_device() -> Result<ID3D11Device> {
    // Fall back to the software rasterizer on machines without a usable GPU driver.
    try_d3d_device(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| try_d3d_device(D3D_DRIVER_TYPE_WARP))
}

fn try_d3d_device(driver: D3D_DRIVER_TYPE) -> Result<ID3D11Device> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
}

pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}
