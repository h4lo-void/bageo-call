// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! ALSA through its kernel ioctls: the control device (`/dev/snd/controlC<card>`) for
//! the mixer, and PCM devices (`/dev/snd/pcmC<card>D<device><c|p>`) for call audio.
//! (Talking to the kernel directly instead of through tinyalsa follows gsm2sip's
//! tinymix and tinypcm, MIT; the code is this project's own.)

use std::collections::HashMap;
use std::ffi::{c_int, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::abi::{self, zeroed, CardInfo, ElemId, ElemInfo, NativeElemList, NativeElemValue, Pod, WORD};
use crate::control::{Direction, MixerBackend, PcmRequest};
use crate::mixer::{CtlInfo, CtlKey, CtlType, Selector};

/// Most controls listed per card, and most enumerated items read per control.
const MAX_CONTROLS: u32 = 16_384;
const MAX_ENUM_ITEMS: u32 = 1_024;

fn ioctl<T: Pod>(fd: RawFd, req: u32, arg: &mut T) -> io::Result<()> {
    loop {
        // SAFETY: `arg` is a live, correctly sized kernel struct for `req` (the request
        // numbers are built from these very types, see `abi`).
        let r = unsafe { libc::ioctl(fd, req as _, arg as *mut T) };
        if r >= 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

fn ioctl_none(fd: RawFd, req: u32) -> io::Result<()> {
    loop {
        // SAFETY: requests without an argument.
        let r = unsafe { libc::ioctl(fd, req as _, 0usize) };
        if r >= 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

fn open_dev(path: &str, flags: c_int) -> io::Result<OwnedFd> {
    let c = CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: a NUL-terminated path; the returned descriptor is owned below.
    let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and is owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

// ------------------------------------------------------------------ mixer

pub struct Control {
    fd: OwnedFd,
    card: u32,
}

fn elem_id_of(info: &CtlInfo) -> ElemId {
    let mut id: ElemId = zeroed();
    id.numid = info.numid;
    id.iface = info.key.iface;
    id.device = info.key.device;
    id.subdevice = info.key.subdevice;
    id.index = info.key.index;
    abi::set_cstr(&mut id.name, &info.key.name);
    id
}

impl Control {
    pub fn open(card: u32) -> io::Result<Control> {
        let fd = open_dev(&format!("/dev/snd/controlC{card}"), libc::O_RDWR)?;
        Ok(Control { fd, card })
    }

    pub fn card_info(&self) -> io::Result<CardInfo> {
        let mut ci: CardInfo = zeroed();
        ioctl(self.fd.as_raw_fd(), abi::SNDRV_CTL_IOCTL_CARD_INFO, &mut ci)?;
        Ok(ci)
    }

    pub fn ids(&self) -> io::Result<Vec<ElemId>> {
        let mut list: NativeElemList = zeroed();
        ioctl(self.fd.as_raw_fd(), abi::SNDRV_CTL_IOCTL_ELEM_LIST, &mut list)?;
        let count = list.count.min(MAX_CONTROLS);
        let mut ids: Vec<ElemId> = (0..count).map(|_| zeroed()).collect();
        let mut list: NativeElemList = zeroed();
        list.space = count;
        list.pids = ids.as_mut_ptr() as usize;
        ioctl(self.fd.as_raw_fd(), abi::SNDRV_CTL_IOCTL_ELEM_LIST, &mut list)?;
        ids.truncate(list.used.min(count) as usize);
        Ok(ids)
    }

    /// A control's description; `with_items`: also the names of an enumerated
    /// control's items (one ioctl per item).
    pub fn info(&self, id: &ElemId, with_items: bool) -> io::Result<CtlInfo> {
        let fd = self.fd.as_raw_fd();
        let mut info: ElemInfo = zeroed();
        info.id = *id;
        ioctl(fd, abi::SNDRV_CTL_IOCTL_ELEM_INFO, &mut info)?;
        let ty = CtlType::from_raw(info.kind);
        let (min, max, step) = match ty {
            CtlType::Boolean => (0, 1, 1),
            CtlType::Integer => abi::info_integer(&info.value.0, WORD),
            CtlType::Integer64 => abi::info_integer64(&info.value.0),
            _ => (0, 0, 0),
        };
        let mut items = Vec::new();
        if ty == CtlType::Enumerated && with_items {
            let n = abi::info_enum_items(&info.value.0).min(MAX_ENUM_ITEMS);
            for i in 0..n {
                let mut q: ElemInfo = zeroed();
                q.id = info.id;
                abi::info_set_enum_item(&mut q.value.0, i);
                ioctl(fd, abi::SNDRV_CTL_IOCTL_ELEM_INFO, &mut q)?;
                items.push(abi::info_enum_name(&q.value.0));
            }
        }
        Ok(CtlInfo {
            numid: info.id.numid,
            key: CtlKey {
                card: self.card,
                iface: info.id.iface,
                device: info.id.device,
                subdevice: info.id.subdevice,
                index: info.id.index,
                name: abi::cstr(&info.id.name),
            },
            ty,
            access: info.access,
            count: info.count,
            min,
            max,
            step,
            items,
        })
    }

    pub fn read(&self, info: &CtlInfo) -> io::Result<Vec<i64>> {
        if info.count > info.ty.max_count() {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "this kind of control cannot be read"));
        }
        let mut v: NativeElemValue = zeroed();
        v.id = elem_id_of(info);
        ioctl(self.fd.as_raw_fd(), abi::SNDRV_CTL_IOCTL_ELEM_READ, &mut v)?;
        let raw = &v.value.0;
        let n = info.count as usize;
        Ok(match info.ty {
            CtlType::Boolean | CtlType::Integer => (0..n).map(|i| abi::get_long(raw, WORD, i)).collect(),
            CtlType::Integer64 => (0..n).map(|i| abi::get_i64(raw, i)).collect(),
            CtlType::Enumerated => (0..n).map(|i| abi::get_u32(raw, i) as i64).collect(),
            CtlType::Bytes => raw[..n].iter().map(|&b| b as i64).collect(),
            _ => return Err(io::Error::new(io::ErrorKind::Unsupported, "this kind of control cannot be read")),
        })
    }

    pub fn write(&self, info: &CtlInfo, values: &[i64]) -> io::Result<()> {
        if values.len() != info.count as usize || info.count > info.ty.max_count() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "wrong number of values"));
        }
        let mut v: NativeElemValue = zeroed();
        v.id = elem_id_of(info);
        let raw = &mut v.value.0;
        for (i, &x) in values.iter().enumerate() {
            match info.ty {
                CtlType::Boolean | CtlType::Integer => abi::put_long(raw, WORD, i, x),
                CtlType::Integer64 => abi::put_i64(raw, i, x),
                CtlType::Enumerated => abi::put_u32(raw, i, x as u32),
                _ => return Err(io::Error::new(io::ErrorKind::Unsupported, "this kind of control cannot be set")),
            }
        }
        ioctl(self.fd.as_raw_fd(), abi::SNDRV_CTL_IOCTL_ELEM_WRITE, &mut v)
    }
}

/// The mixer of every card, controls opened as needed.
#[derive(Default)]
pub struct AlsaMixer {
    cards: HashMap<u32, Control>,
}

fn err(e: io::Error) -> String {
    e.to_string()
}

impl AlsaMixer {
    pub fn new() -> AlsaMixer {
        AlsaMixer::default()
    }

    fn ctl(&mut self, card: u32) -> Result<&Control, String> {
        if let std::collections::hash_map::Entry::Vacant(slot) = self.cards.entry(card) {
            slot.insert(Control::open(card).map_err(|e| format!("sound card {card}: {e}"))?);
        }
        Ok(&self.cards[&card])
    }
}

impl MixerBackend for AlsaMixer {
    fn list(&mut self, card: u32) -> Result<Vec<CtlInfo>, String> {
        let c = self.ctl(card)?;
        let ids = c.ids().map_err(err)?;
        Ok(ids.iter().filter_map(|id| c.info(id, true).ok()).collect())
    }

    fn find(&mut self, card: u32, sel: &Selector) -> Result<CtlInfo, String> {
        let c = self.ctl(card)?;
        let id = match sel {
            Selector::Numid(n) => {
                let mut id: ElemId = zeroed();
                id.numid = *n;
                id
            }
            Selector::Name { .. } => *c
                .ids()
                .map_err(err)?
                .iter()
                .find(|id| sel.matches(id.numid, &abi::cstr(&id.name), id.index))
                .ok_or("no such control")?,
        };
        c.info(&id, true).map_err(|e| format!("no such control ({e})"))
    }

    fn find_key(&mut self, key: &CtlKey) -> Result<CtlInfo, String> {
        let c = self.ctl(key.card)?;
        let id = *c
            .ids()
            .map_err(err)?
            .iter()
            .find(|id| {
                id.iface == key.iface
                    && id.device == key.device
                    && id.subdevice == key.subdevice
                    && id.index == key.index
                    && abi::cstr(&id.name) == key.name
            })
            .ok_or("the control is gone")?;
        c.info(&id, true).map_err(err)
    }

    fn read(&mut self, info: &CtlInfo) -> Result<Vec<i64>, String> {
        self.ctl(info.key.card)?.read(info).map_err(err)
    }

    fn write(&mut self, info: &CtlInfo, values: &[i64]) -> Result<(), String> {
        self.ctl(info.key.card)?.write(info, values).map_err(err)
    }
}

// ------------------------------------------------------------------ PCM

/// An open PCM stream, s16le interleaved, set up and prepared.
pub struct Pcm {
    fd: OwnedFd,
    pub dir: Direction,
    pub rate: u32,
    pub channels: u32,
    pub period_frames: usize,
    pub periods: usize,
    pub buffer_frames: usize,
    frame_bytes: usize,
    pub xruns: AtomicU64,
}

fn set_mask(hw: &mut abi::NativeHwParams, param: usize, bit: u32) {
    let m = &mut hw.masks[param];
    m.bits = [0; 8];
    m.bits[(bit / 32) as usize] = 1 << (bit % 32);
}

fn set_int(hw: &mut abi::NativeHwParams, param: usize, v: u32) {
    let i = &mut hw.intervals[param - abi::HW_PARAM_FIRST_INTERVAL];
    i.min = v;
    i.max = v;
    i.flags = abi::INTERVAL_INTEGER;
}

fn set_min(hw: &mut abi::NativeHwParams, param: usize, v: u32) {
    hw.intervals[param - abi::HW_PARAM_FIRST_INTERVAL].min = v;
}

fn interval(hw: &abi::NativeHwParams, param: usize) -> u32 {
    hw.intervals[param - abi::HW_PARAM_FIRST_INTERVAL].min
}

/// Consecutive under- or overruns after which a stream gives up.
const MAX_XRUNS_IN_A_ROW: u32 = 50;

impl Pcm {
    pub fn open(req: &PcmRequest) -> io::Result<Pcm> {
        let suffix = if req.dir == Direction::Capture { 'c' } else { 'p' };
        let path = format!("/dev/snd/pcmC{}D{}{}", req.card, req.device, suffix);
        // Opened without blocking (a busy device would otherwise wait for its owner),
        // then switched to blocking reads and writes.
        let fd = open_dev(&path, libc::O_RDWR | libc::O_NONBLOCK)?;
        let raw = fd.as_raw_fd();
        // SAFETY: plain fcntl calls on a descriptor we own.
        unsafe {
            let fl = libc::fcntl(raw, libc::F_GETFL);
            if fl < 0 || libc::fcntl(raw, libc::F_SETFL, fl & !libc::O_NONBLOCK) < 0 {
                return Err(io::Error::last_os_error());
            }
        }

        let mut hw: abi::NativeHwParams = zeroed();
        for m in hw.masks.iter_mut() {
            m.bits = [u32::MAX; 8];
        }
        for i in hw.intervals.iter_mut() {
            i.min = 0;
            i.max = u32::MAX;
        }
        hw.rmask = u32::MAX;
        hw.info = u32::MAX;
        set_mask(&mut hw, abi::HW_PARAM_ACCESS, abi::PCM_ACCESS_RW_INTERLEAVED);
        set_mask(&mut hw, abi::HW_PARAM_FORMAT, abi::PCM_FORMAT_S16_LE);
        set_mask(&mut hw, abi::HW_PARAM_SUBFORMAT, abi::PCM_SUBFORMAT_STD);
        set_int(&mut hw, abi::HW_PARAM_SAMPLE_BITS, 16);
        set_int(&mut hw, abi::HW_PARAM_FRAME_BITS, 16 * req.channels);
        set_int(&mut hw, abi::HW_PARAM_CHANNELS, req.channels);
        set_int(&mut hw, abi::HW_PARAM_RATE, req.rate);
        set_min(&mut hw, abi::HW_PARAM_PERIOD_SIZE, req.period_frames);
        set_int(&mut hw, abi::HW_PARAM_PERIODS, req.periods);
        ioctl(raw, abi::SNDRV_PCM_IOCTL_HW_PARAMS, &mut hw)?;

        let period = interval(&hw, abi::HW_PARAM_PERIOD_SIZE) as usize;
        let periods = interval(&hw, abi::HW_PARAM_PERIODS) as usize;
        let mut buffer = interval(&hw, abi::HW_PARAM_BUFFER_SIZE) as usize;
        if period == 0 || periods == 0 || period > 65_536 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the device chose no usable period"));
        }
        if buffer < period {
            buffer = period * periods;
        }

        let mut sw: abi::NativeSwParams = zeroed();
        sw.period_step = 1;
        sw.avail_min = period;
        // Capture starts with the first read; playback once two periods are queued.
        sw.start_threshold = if req.dir == Direction::Capture { 1 } else { (period * 2).min(buffer) };
        sw.stop_threshold = buffer;
        ioctl(raw, abi::SNDRV_PCM_IOCTL_SW_PARAMS, &mut sw)?;
        ioctl_none(raw, abi::SNDRV_PCM_IOCTL_PREPARE)?;

        Ok(Pcm {
            fd,
            dir: req.dir,
            rate: req.rate,
            channels: req.channels,
            period_frames: period,
            periods,
            buffer_frames: buffer,
            frame_bytes: 2 * req.channels as usize,
            xruns: AtomicU64::new(0),
        })
    }

    pub fn period_bytes(&self) -> usize {
        self.period_frames * self.frame_bytes
    }

    fn transfer(&self, ptr: *mut u8, len: usize) -> io::Result<()> {
        let frames = len / self.frame_bytes;
        let req = if self.dir == Direction::Capture {
            abi::SNDRV_PCM_IOCTL_READI_FRAMES
        } else {
            abi::SNDRV_PCM_IOCTL_WRITEI_FRAMES
        };
        let mut done = 0usize;
        let mut xruns_in_a_row = 0;
        while done < frames {
            let mut x: abi::NativeXferi = zeroed();
            x.buf = ptr as usize + done * self.frame_bytes;
            x.frames = frames - done;
            match ioctl(self.fd.as_raw_fd(), req, &mut x) {
                Ok(()) => {
                    let n = x.result as isize;
                    if n <= 0 {
                        return Err(io::Error::other("the device moved no frames"));
                    }
                    done += n as usize;
                    xruns_in_a_row = 0;
                }
                Err(e) if matches!(e.raw_os_error(), Some(libc::EPIPE | libc::ESTRPIPE | libc::EBADFD)) => {
                    // An under- or overrun (or a suspend): set the stream up again.
                    self.xruns.fetch_add(1, Ordering::Relaxed);
                    xruns_in_a_row += 1;
                    if xruns_in_a_row > MAX_XRUNS_IN_A_ROW {
                        return Err(e);
                    }
                    ioctl_none(self.fd.as_raw_fd(), abi::SNDRV_PCM_IOCTL_PREPARE)?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Reads whole frames into `buf` (capture streams).
    pub fn read_frames(&self, buf: &mut [u8]) -> io::Result<()> {
        self.transfer(buf.as_mut_ptr(), buf.len())
    }

    /// Writes whole frames from `buf` (playback streams).
    pub fn write_frames(&self, buf: &[u8]) -> io::Result<()> {
        // The kernel only reads from the buffer for a playback stream.
        self.transfer(buf.as_ptr() as *mut u8, buf.len())
    }
}

impl Drop for Pcm {
    fn drop(&mut self) {
        let _ = ioctl_none(self.fd.as_raw_fd(), abi::SNDRV_PCM_IOCTL_DROP);
    }
}
