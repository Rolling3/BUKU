//! Album art: decodes a player's thumbnail into a small square of premultiplied BGRA pixels.
//!
//! Runs on the media worker thread. The UI thread turns the pixels into a
//! Direct2D bitmap, so decoding never blocks drawing.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::Storage::Streams::{
    Buffer, DataReader, IInputStream, IRandomAccessStreamReference, InputStreamOptions,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeHighQualityCubic,
    WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IStream};
use windows::core::{Interface, Result, RuntimeType};
use windows_future::{
    AsyncOperationCompletedHandler, AsyncOperationWithProgressCompletedHandler, IAsyncOperation,
    IAsyncOperationWithProgress,
};

/// Side of the decoded square in pixels: 56 DIPs at 300% scaling.
pub const ART_PX: u32 = 168;
/// How long a player gets to hand over its thumbnail. A player that never
/// answers would otherwise hold the media thread, and every update behind it.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// Overall time allowed to read the thumbnail bytes once it is open.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest encoded thumbnail read into memory.
const MAX_SOURCE_BYTES: u64 = 10 * 1024 * 1024;
const CHUNK_BYTES: u32 = 64 * 1024;
/// Largest thumbnail decoded: 8192 x 8192.
const MAX_SOURCE_PIXELS: u64 = 8192 * 8192;

pub struct Art {
    /// `ART_PX` x `ART_PX` premultiplied BGRA, rows packed.
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Art {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Art")
            .field("bytes", &self.pixels.len())
            .finish()
    }
}

/// Center-crops the thumbnail to a square and scales it to `ART_PX`.
pub fn decode(reference: &IRandomAccessStreamReference) -> Result<Art> {
    let stream = wait(reference.OpenReadAsync()?)?;
    let size = source_len(stream.Size()?)?;
    // Read the bytes here under a deadline, so WIC never blocks on a player's stream.
    let input: IInputStream = stream.cast()?;
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut bytes = Vec::with_capacity(size);
    while bytes.len() < size {
        let buffer = Buffer::Create(CHUNK_BYTES)?;
        let chunk = wait_progress(
            input.ReadAsync(&buffer, CHUNK_BYTES, InputStreamOptions::Partial)?,
            deadline,
        )?;
        let len = chunk.Length()? as usize;
        if len == 0 {
            break;
        }
        // The reported size can lie; never keep more than we agreed to.
        if bytes.len() + len > size {
            return Err(windows::Win32::Foundation::E_INVALIDARG.into());
        }
        let start = bytes.len();
        bytes.resize(start + len, 0);
        DataReader::FromBuffer(&chunk)?.ReadBytes(&mut bytes[start..])?;
    }
    unsafe {
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        // `bytes` outlives the decode: the WIC stream only borrows the memory.
        let stream = wic.CreateStream()?;
        stream.InitializeFromMemory(&bytes)?;
        let stream: IStream = stream.cast()?;
        let decoder =
            wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;

        let (mut width, mut height) = (0, 0);
        frame.GetSize(&mut width, &mut height)?;
        let side = width.min(height);
        // Cover art is at most a few thousand pixels a side; a giant image would
        // cost seconds and hundreds of MB to decode for a 168 px square.
        if side == 0 || u64::from(width) * u64::from(height) > MAX_SOURCE_PIXELS {
            return Err(windows::Win32::Foundation::E_INVALIDARG.into());
        }
        // Players often send letterboxed video frames; the middle square is the art.
        let clipper = wic.CreateBitmapClipper()?;
        clipper.Initialize(
            &frame,
            &WICRect {
                X: ((width - side) / 2) as i32,
                Y: ((height - side) / 2) as i32,
                Width: side as i32,
                Height: side as i32,
            },
        )?;
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(
            &clipper,
            ART_PX,
            ART_PX,
            WICBitmapInterpolationModeHighQualityCubic,
        )?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;

        let stride = ART_PX * 4;
        let mut pixels = vec![0u8; (stride * ART_PX) as usize];
        converter.CopyPixels(std::ptr::null(), stride, &mut pixels)?;
        Ok(Art { pixels })
    }
}

/// Checks the stream's reported size against `MAX_SOURCE_BYTES`.
fn source_len(size: u64) -> Result<usize> {
    if size == 0 || size > MAX_SOURCE_BYTES {
        return Err(windows::Win32::Foundation::E_INVALIDARG.into());
    }
    Ok(size as usize)
}

/// Waits for a WinRT read, cancelling it once `deadline` passes.
fn wait_progress<T, P>(operation: IAsyncOperationWithProgress<T, P>, deadline: Instant) -> Result<T>
where
    T: RuntimeType + 'static,
    P: RuntimeType + 'static,
{
    let (done_tx, done_rx) = mpsc::channel();
    operation.SetCompleted(&AsyncOperationWithProgressCompletedHandler::new(
        move |_, _| {
            let _ = done_tx.send(());
            Ok(())
        },
    ))?;
    let left = deadline.saturating_duration_since(Instant::now());
    if done_rx.recv_timeout(left).is_err() {
        let _ = operation.Cancel();
        return Err(windows::Win32::Foundation::ERROR_TIMEOUT.into());
    }
    operation.GetResults()
}

/// Waits for a WinRT operation, cancelling it after `OPEN_TIMEOUT`.
fn wait<T: RuntimeType + 'static>(operation: IAsyncOperation<T>) -> Result<T> {
    let (done_tx, done_rx) = mpsc::channel();
    operation.SetCompleted(&AsyncOperationCompletedHandler::new(move |_, _| {
        let _ = done_tx.send(());
        Ok(())
    }))?;
    if done_rx.recv_timeout(OPEN_TIMEOUT).is_err() {
        let _ = operation.Cancel();
        return Err(windows::Win32::Foundation::ERROR_TIMEOUT.into());
    }
    operation.GetResults()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Storage::Streams::{
        DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference,
    };

    #[test]
    fn decode_should_return_square_premultiplied_pixels() -> Result<()> {
        unsafe {
            let _ = windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            );
        }
        let png = include_bytes!("../branding/icons/buku-256.png");
        let stream = InMemoryRandomAccessStream::new()?;
        let writer = DataWriter::CreateDataWriter(&stream)?;
        writer.WriteBytes(png)?;
        writer.StoreAsync()?.join()?;
        writer.DetachStream()?;
        stream.Seek(0)?;

        let reference: IRandomAccessStreamReference =
            RandomAccessStreamReference::CreateFromStream(&stream)?.cast()?;
        let art = decode(&reference)?;
        assert_eq!(art.pixels.len(), (ART_PX * ART_PX * 4) as usize);
        Ok(())
    }

    #[test]
    fn source_len_should_refuse_empty_and_oversized_streams() {
        assert!(source_len(0).is_err());
        assert!(source_len(MAX_SOURCE_BYTES + 1).is_err());
        assert_eq!(
            source_len(MAX_SOURCE_BYTES).unwrap(),
            MAX_SOURCE_BYTES as usize
        );
    }
}
