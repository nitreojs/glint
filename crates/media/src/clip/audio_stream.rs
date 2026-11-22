use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use glint_core::record::mp4::AudioView;
use windows::Win32::Foundation::{E_NOTIMPL, E_POINTER, S_FALSE, S_OK, STG_E_ACCESSDENIED, STG_E_INVALIDFUNCTION, STG_E_READFAULT};
use windows::Win32::System::Com::*;
use windows::core::{HRESULT, Ref, implement};

pub(super) fn open(view: AudioView, stop: Arc<AtomicBool>) -> IStream {
    AudioStream { view: Arc::new(view), stop, position: Mutex::new(0) }.into()
}

#[implement(IStream, IAgileObject)]
struct AudioStream {
    view: Arc<AudioView>,
    stop: Arc<AtomicBool>,
    position: Mutex<u64>,
}

impl IAgileObject_Impl for AudioStream_Impl {}

impl ISequentialStream_Impl for AudioStream_Impl {
    fn Read(&self, buf: *mut c_void, len: u32, read: *mut u32) -> HRESULT {
        if !read.is_null() {
            unsafe { *read = 0 };
        }
        if len == 0 {
            return S_OK;
        }
        if buf.is_null() {
            return E_POINTER;
        }

        let Ok(mut at) = self.position.lock() else { return STG_E_READFAULT };
        let buf = unsafe { std::slice::from_raw_parts_mut(buf.cast::<u8>(), len as usize) };
        let Ok(n) = self.view.read_at(*at, buf, || self.stop.load(Ordering::Relaxed)) else {
            return STG_E_READFAULT;
        };
        *at += n as u64;
        if !read.is_null() {
            unsafe { *read = n as u32 };
        }

        if n == len as usize { S_OK } else { S_FALSE }
    }

    fn Write(&self, _: *const c_void, _: u32, written: *mut u32) -> HRESULT {
        if !written.is_null() {
            unsafe { *written = 0 };
        }

        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for AudioStream_Impl {
    fn Seek(&self, offset: i64, origin: STREAM_SEEK, result: *mut u64) -> windows::core::Result<()> {
        let mut at = self.position.lock().map_err(|_| STG_E_READFAULT)?;
        let base = match origin {
            STREAM_SEEK_SET => 0,
            STREAM_SEEK_CUR => *at,
            STREAM_SEEK_END => self.view.len(),
            _ => return Err(STG_E_INVALIDFUNCTION.into()),
        };
        *at = base.checked_add_signed(offset).ok_or(STG_E_INVALIDFUNCTION)?;
        if !result.is_null() {
            unsafe { *result = *at };
        }

        Ok(())
    }

    fn SetSize(&self, _: u64) -> windows::core::Result<()> {
        Err(STG_E_ACCESSDENIED.into())
    }

    fn CopyTo(&self, _: Ref<IStream>, _: u64, _: *mut u64, _: *mut u64) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Commit(&self, _: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn LockRegion(&self, _: u64, _: u64, _: &LOCKTYPE) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn UnlockRegion(&self, _: u64, _: u64, _: u32) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn Stat(&self, stat: *mut STATSTG, _: &STATFLAG) -> windows::core::Result<()> {
        if stat.is_null() {
            return Err(E_POINTER.into());
        }

        unsafe {
            stat.write(STATSTG { r#type: STGTY_STREAM.0 as u32, cbSize: self.view.len(), ..Default::default() });
        }

        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        let at = *self.position.lock().map_err(|_| STG_E_READFAULT)?;

        Ok(AudioStream { view: self.view.clone(), stop: self.stop.clone(), position: Mutex::new(at) }.into())
    }
}
