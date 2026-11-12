//! Screen capture (GDI), cursor compositing, JPEG encoding and TCP streaming.

use std::io::Write;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    HALFTONE, HGDIOBJ, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, GetSystemMetrics, CURSORINFO, DI_NORMAL, SM_CXCURSOR,
    SM_CXSCREEN, SM_CYCURSOR, SM_CYSCREEN,
};

use crate::applog::log;
use crate::protocol::{SCREEN_FPS, SCREEN_JPEG_QUALITY, SCREEN_MAX_WIDTH};

pub struct ScreenSession {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ScreenSession {
    pub fn start(
        client_ip: IpAddr,
        port: u16,
    ) -> (Self, Receiver<Result<(u32, u32), String>>) {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let (ready_tx, ready_rx) = mpsc::channel();

        let handle = thread::Builder::new()
            .name("screen".into())
            .spawn(move || stream_loop(client_ip, port, thread_stop, ready_tx))
            .ok();

        (
            Self {
                stop,
                handle,
            },
            ready_rx,
        )
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ScreenSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn stream_loop(
    client_ip: IpAddr,
    port: u16,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(u32, u32), String>>,
) {
    let address = SocketAddr::new(client_ip, port);
    let mut first_frame = true;

    while !stop.load(Ordering::SeqCst) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut stream = None;
        while !stop.load(Ordering::SeqCst) {
            match TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
                Ok(sock) => {
                    let _ = sock.set_nodelay(true);
                    stream = Some(sock);
                    break;
                }
                Err(err) => {
                    if Instant::now() > deadline {
                        let _ = ready.send(Err(format!(
                            "телефон не принял TCP-соединение: {err}"
                        )));
                        return;
                    }
                    thread::sleep(Duration::from_millis(1500));
                }
            }
        }
        let Some(mut stream) = stream else {
            return;
        };
        log(format!("Демонстрация экрана: подключение к {client_ip}:{port}"));

        let frame_time = Duration::from_millis(1000 / SCREEN_FPS);
        while !stop.load(Ordering::SeqCst) {
            let started = Instant::now();
            match capture_jpeg() {
                Ok((jpeg, width, height)) => {
                    if first_frame {
                        first_frame = false;
                        let _ = ready.send(Ok((width, height)));
                        log(format!(
                            "Демонстрация экрана: {width}x{height}, кадр {} КБ",
                            jpeg.len() / 1024
                        ));
                    }
                    let mut packet = Vec::with_capacity(4 + jpeg.len());
                    packet.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
                    packet.extend_from_slice(&jpeg);
                    if stream.write_all(&packet).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    log(format!("Демонстрация экрана недоступна: {err}"));
                    if first_frame {
                        let _ = ready.send(Err(err.to_string()));
                        return;
                    }
                    thread::sleep(Duration::from_millis(500));
                }
            }
            if let Some(delay) = frame_time.checked_sub(started.elapsed()) {
                thread::sleep(delay);
            }
        }

        if stop.load(Ordering::SeqCst) {
            return;
        }
        log("Демонстрация экрана: соединение потеряно, переподключаюсь…");
        thread::sleep(Duration::from_millis(500));
    }
}

fn capture_jpeg() -> Result<(Vec<u8>, u32, u32)> {
    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1) as u32;
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1) as u32;

    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.is_invalid() {
        return Err(anyhow!("не удалось получить контекст экрана"));
    }
    let src_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
    let dst_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };

    let result = (|| -> Result<(Vec<u8>, u32, u32)> {
        let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: screen_w as i32,
            biHeight: -(screen_h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            ..Default::default()
        };

        let mut src_bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let src_bitmap = unsafe {
            CreateDIBSection(
                Some(screen_dc),
                &info,
                DIB_RGB_COLORS,
                &mut src_bits,
                None,
                0,
            )?
        };
        let old_src = unsafe { SelectObject(src_dc, HGDIOBJ(src_bitmap.0)) };
        unsafe {
            BitBlt(
                src_dc,
                0,
                0,
                screen_w as i32,
                screen_h as i32,
                Some(screen_dc),
                0,
                0,
                SRCCOPY,
            )?;
        }

        let out_w = screen_w.min(SCREEN_MAX_WIDTH);
        let out_h = ((screen_h as u64 * out_w as u64) / screen_w as u64).max(1) as u32;

        info.bmiHeader.biWidth = out_w as i32;
        info.bmiHeader.biHeight = -(out_h as i32);
        let mut dst_bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dst_bitmap = unsafe {
            CreateDIBSection(
                Some(screen_dc),
                &info,
                DIB_RGB_COLORS,
                &mut dst_bits,
                None,
                0,
            )?
        };
        let old_dst = unsafe { SelectObject(dst_dc, HGDIOBJ(dst_bitmap.0)) };

        unsafe {
            SetStretchBltMode(dst_dc, HALFTONE);
            let _ = StretchBlt(
                dst_dc,
                0,
                0,
                out_w as i32,
                out_h as i32,
                Some(src_dc),
                0,
                0,
                screen_w as i32,
                screen_h as i32,
                SRCCOPY,
            );
        }

        draw_cursor(dst_dc, out_w, out_h, screen_w, screen_h);

        let raw = unsafe {
            std::slice::from_raw_parts(dst_bits as *const u8, (out_w * out_h * 4) as usize)
        };
        let mut rgb = Vec::with_capacity((out_w * out_h * 3) as usize);
        for pixel in raw.chunks_exact(4) {
            rgb.push(pixel[2]);
            rgb.push(pixel[1]);
            rgb.push(pixel[0]);
        }

        unsafe {
            SelectObject(dst_dc, old_dst);
            let _ = DeleteObject(HGDIOBJ(dst_bitmap.0));
            SelectObject(src_dc, old_src);
            let _ = DeleteObject(HGDIOBJ(src_bitmap.0));
        }

        let image = image::RgbImage::from_raw(out_w, out_h, rgb)
            .ok_or_else(|| anyhow!("не удалось собрать кадр"))?;
        let mut jpeg = Vec::new();
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, SCREEN_JPEG_QUALITY);
        encoder.encode_image(&image)?;
        Ok((jpeg, out_w, out_h))
    })();

    unsafe {
        let _ = DeleteDC(src_dc);
        let _ = DeleteDC(dst_dc);
        ReleaseDC(None, screen_dc);
    }
    result
}

fn draw_cursor(
    dst_dc: windows::Win32::Graphics::Gdi::HDC,
    out_w: u32,
    out_h: u32,
    screen_w: u32,
    screen_h: u32,
) {
    unsafe {
        let mut cursor = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if GetCursorInfo(&mut cursor).is_err() || cursor.flags.0 & 1 == 0 {
            return;
        }

        let mut icon = Default::default();
        if GetIconInfo(cursor.hCursor.into(), &mut icon).is_err() {
            return;
        }

        let cursor_w = GetSystemMetrics(SM_CXCURSOR).max(1) as f32;
        let cursor_h = GetSystemMetrics(SM_CYCURSOR).max(1) as f32;
        let scale_x = out_w as f32 / screen_w as f32;
        let scale_y = out_h as f32 / screen_h as f32;
        let draw_w = (cursor_w * scale_x).max(1.0) as i32;
        let draw_h = (cursor_h * scale_y).max(1.0) as i32;
        let x = (cursor.ptScreenPos.x as f32 * scale_x - icon.xHotspot as f32 * scale_x) as i32;
        let y = (cursor.ptScreenPos.y as f32 * scale_y - icon.yHotspot as f32 * scale_y) as i32;

        if x < 0 || y < 0 || x + draw_w > out_w as i32 || y + draw_h > out_h as i32 {
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(icon.hbmMask.0));
            if !icon.hbmColor.is_invalid() {
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ(icon.hbmColor.0));
            }
            return;
        }

        let cursor_dc = CreateCompatibleDC(Some(dst_dc));
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: draw_w,
            biHeight: -draw_h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        if let Ok(bitmap) = CreateDIBSection(
            Some(dst_dc),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        ) {
            let old = SelectObject(cursor_dc, HGDIOBJ(bitmap.0));
            let _ = BitBlt(
                cursor_dc,
                0,
                0,
                draw_w,
                draw_h,
                Some(dst_dc),
                x,
                y,
                SRCCOPY,
            );
            let _ = DrawIconEx(
                cursor_dc,
                0,
                0,
                cursor.hCursor.into(),
                draw_w,
                draw_h,
                0,
                None,
                DI_NORMAL,
            );
            let _ = BitBlt(
                dst_dc,
                x,
                y,
                draw_w,
                draw_h,
                Some(cursor_dc),
                0,
                0,
                SRCCOPY,
            );
            SelectObject(cursor_dc, old);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
        let _ = DeleteDC(cursor_dc);
        let _ = DeleteObject(HGDIOBJ(icon.hbmMask.0));
        if !icon.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(icon.hbmColor.0));
        }
    }
}
