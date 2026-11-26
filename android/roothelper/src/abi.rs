// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The ALSA kernel interface (`include/uapi/sound/asound.h`): ioctl numbers and the
//! structs they carry, for 32-bit (armeabi-v7a) and 64-bit (arm64-v8a) userspace.
//!
//! Several structs hold a C `long` or a pointer, so their size, and with it the ioctl
//! number that encodes the size, depends on the word size the helper is built for.
//! The structs that differ are generic over that word (`u32` or `u64`); the helper
//! uses the native one (`usize`). The tests pin both layouts, so a layout mistake
//! shows up as a failed test instead of `ENOTTY` on a phone. (Building one binary per
//! ABI because of this follows gsm2sip's tinymix, MIT.)
//!
//! Only ARM's layout rules are covered: a 64-bit integer is 8-byte aligned on 32-bit
//! ARM too. 32-bit x86 aligns it to 4 and is not a target.

use core::mem::size_of;

const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

/// `_IOC(dir, type, nr, size)` as on ARM and arm64.
pub const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    (dir << 30) | (((size as u32) & 0x3fff) << 16) | ((ty as u32) << 8) | nr as u32
}
pub const fn io(ty: u8, nr: u8) -> u32 {
    ioc(IOC_NONE, ty, nr, 0)
}
pub const fn ior(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(IOC_READ, ty, nr, size)
}
pub const fn iow(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(IOC_WRITE, ty, nr, size)
}
pub const fn iowr(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(IOC_READ | IOC_WRITE, ty, nr, size)
}

/// Plain-data kernel structs: every bit pattern, all zeros included, is a valid value.
///
/// # Safety
/// Only for `#[repr(C)]` structs made of integers and arrays of integers.
pub unsafe trait Pod: Sized {}

/// A zero-filled kernel struct.
pub fn zeroed<T: Pod>() -> T {
    // SAFETY: `Pod` types are integers and arrays of integers only.
    unsafe { core::mem::zeroed() }
}

/// Bytes with the 8-byte alignment of the kernel unions that hold a `long long`.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct Raw8<const N: usize>(pub [u8; N]);

// ------------------------------------------------------------------ control device

pub const CTL_NAME_LEN: usize = 44;

/// `struct snd_ctl_elem_id` (64 bytes on both).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ElemId {
    pub numid: u32,
    pub iface: i32,
    pub device: u32,
    pub subdevice: u32,
    pub name: [u8; CTL_NAME_LEN],
    pub index: u32,
}

/// `struct snd_ctl_card_info` (376 bytes on both).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CardInfo {
    pub card: i32,
    pub pad: i32,
    pub id: [u8; 16],
    pub driver: [u8; 16],
    pub name: [u8; 32],
    pub longname: [u8; 80],
    pub reserved: [u8; 16],
    pub mixername: [u8; 80],
    pub components: [u8; 128],
}

/// `struct snd_ctl_elem_list`; `W` is the pointer width (72 bytes on 32-bit, 80 on 64).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ElemList<W> {
    pub offset: u32,
    pub space: u32,
    pub used: u32,
    pub count: u32,
    pub pids: W,
    pub reserved: [u8; 50],
}

/// `struct snd_ctl_elem_info` (272 bytes on both). `value` is the union; read it with
/// the `info_*` functions below.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ElemInfo {
    pub id: ElemId,
    pub kind: i32,
    pub access: u32,
    pub count: u32,
    pub owner: i32,
    pub value: Raw8<128>,
    pub reserved: [u8; 64],
}

/// `struct snd_ctl_elem_value`; `N` is the size of the value union, `128 * sizeof(long)`
/// (712 bytes on 32-bit, 1224 on 64).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ElemValue<const N: usize> {
    pub id: ElemId,
    pub indirect: u32,
    pub value: Raw8<N>,
    pub reserved: [u8; 128],
}

pub const CTL_TYPE_NONE: i32 = 0;
pub const CTL_TYPE_BOOLEAN: i32 = 1;
pub const CTL_TYPE_INTEGER: i32 = 2;
pub const CTL_TYPE_ENUMERATED: i32 = 3;
pub const CTL_TYPE_BYTES: i32 = 4;
pub const CTL_TYPE_IEC958: i32 = 5;
pub const CTL_TYPE_INTEGER64: i32 = 6;

pub const CTL_ACCESS_READ: u32 = 1 << 0;
pub const CTL_ACCESS_WRITE: u32 = 1 << 1;
pub const CTL_ACCESS_VOLATILE: u32 = 1 << 2;
pub const CTL_ACCESS_INACTIVE: u32 = 1 << 8;

/// Most values a control may hold, per type (what the value union fits).
pub const MAX_VALUES_LONG: u32 = 128;
pub const MAX_VALUES_INTEGER64: u32 = 64;
pub const MAX_VALUES_ENUM: u32 = 128;
pub const MAX_VALUES_BYTES: u32 = 512;

// ------------------------------------------------------------------ PCM device

/// `struct snd_interval` (12 bytes). `flags`: openmin, openmax, integer, empty bits.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Interval {
    pub min: u32,
    pub max: u32,
    pub flags: u32,
}
pub const INTERVAL_INTEGER: u32 = 1 << 2;

/// `struct snd_mask` (256 bits).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Mask {
    pub bits: [u32; 8],
}

/// `struct snd_pcm_hw_params`; `W` is `unsigned long` (604 bytes on 32-bit, 608 on 64).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HwParams<W> {
    pub flags: u32,
    pub masks: [Mask; 3],
    pub mres: [Mask; 5],
    pub intervals: [Interval; 12],
    pub ires: [Interval; 9],
    pub rmask: u32,
    pub cmask: u32,
    pub info: u32,
    pub msbits: u32,
    pub rate_num: u32,
    pub rate_den: u32,
    pub fifo_size: W,
    pub reserved: [u8; 64],
}

/// `struct snd_pcm_sw_params`; `W` is `unsigned long` (104 bytes on 32-bit, 136 on 64).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SwParams<W> {
    pub tstamp_mode: i32,
    pub period_step: u32,
    pub sleep_min: u32,
    pub avail_min: W,
    pub xfer_align: W,
    pub start_threshold: W,
    pub stop_threshold: W,
    pub silence_threshold: W,
    pub silence_size: W,
    pub boundary: W,
    pub proto: u32,
    pub tstamp_type: u32,
    pub reserved: [u8; 56],
}

/// `struct snd_xferi`; result, buffer pointer and frame count, all word-sized.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Xferi<W> {
    pub result: W,
    pub buf: W,
    pub frames: W,
}

// SAFETY: all of these are `#[repr(C)]` integers and arrays of integers.
unsafe impl Pod for ElemId {}
unsafe impl Pod for CardInfo {}
unsafe impl<W: Copy> Pod for ElemList<W> {}
unsafe impl Pod for ElemInfo {}
unsafe impl<const N: usize> Pod for ElemValue<N> {}
unsafe impl<W: Copy> Pod for HwParams<W> {}
unsafe impl<W: Copy> Pod for SwParams<W> {}
unsafe impl<W: Copy> Pod for Xferi<W> {}

pub const PCM_ACCESS_RW_INTERLEAVED: u32 = 3;
pub const PCM_FORMAT_S16_LE: u32 = 2;
pub const PCM_SUBFORMAT_STD: u32 = 0;

pub const HW_PARAM_ACCESS: usize = 0;
pub const HW_PARAM_FORMAT: usize = 1;
pub const HW_PARAM_SUBFORMAT: usize = 2;
/// Interval parameters count from 8 (`SNDRV_PCM_HW_PARAM_FIRST_INTERVAL`).
pub const HW_PARAM_FIRST_INTERVAL: usize = 8;
pub const HW_PARAM_SAMPLE_BITS: usize = 8;
pub const HW_PARAM_FRAME_BITS: usize = 9;
pub const HW_PARAM_CHANNELS: usize = 10;
pub const HW_PARAM_RATE: usize = 11;
pub const HW_PARAM_PERIOD_SIZE: usize = 13;
pub const HW_PARAM_PERIODS: usize = 15;
pub const HW_PARAM_BUFFER_SIZE: usize = 17;

// ------------------------------------------------------------------ native layout

/// `sizeof(long)` of this build: 4 or 8.
pub const WORD: usize = size_of::<usize>();
pub const VALUE_UNION: usize = 128 * WORD;
pub type NativeElemList = ElemList<usize>;
pub type NativeElemValue = ElemValue<VALUE_UNION>;
pub type NativeHwParams = HwParams<usize>;
pub type NativeSwParams = SwParams<usize>;
pub type NativeXferi = Xferi<usize>;

pub const SNDRV_CTL_IOCTL_PVERSION: u32 = ior(b'U', 0x00, size_of::<i32>());
pub const SNDRV_CTL_IOCTL_CARD_INFO: u32 = ior(b'U', 0x01, size_of::<CardInfo>());
pub const SNDRV_CTL_IOCTL_ELEM_LIST: u32 = iowr(b'U', 0x10, size_of::<NativeElemList>());
pub const SNDRV_CTL_IOCTL_ELEM_INFO: u32 = iowr(b'U', 0x11, size_of::<ElemInfo>());
pub const SNDRV_CTL_IOCTL_ELEM_READ: u32 = iowr(b'U', 0x12, size_of::<NativeElemValue>());
pub const SNDRV_CTL_IOCTL_ELEM_WRITE: u32 = iowr(b'U', 0x13, size_of::<NativeElemValue>());

pub const SNDRV_PCM_IOCTL_HW_PARAMS: u32 = iowr(b'A', 0x11, size_of::<NativeHwParams>());
pub const SNDRV_PCM_IOCTL_SW_PARAMS: u32 = iowr(b'A', 0x13, size_of::<NativeSwParams>());
pub const SNDRV_PCM_IOCTL_PREPARE: u32 = io(b'A', 0x40);
pub const SNDRV_PCM_IOCTL_DROP: u32 = io(b'A', 0x43);
pub const SNDRV_PCM_IOCTL_WRITEI_FRAMES: u32 = iow(b'A', 0x50, size_of::<NativeXferi>());
pub const SNDRV_PCM_IOCTL_READI_FRAMES: u32 = ior(b'A', 0x51, size_of::<NativeXferi>());

// ------------------------------------------------------------------ union access

fn le_bytes<const K: usize>(buf: &[u8], at: usize) -> [u8; K] {
    let mut b = [0u8; K];
    b.copy_from_slice(&buf[at..at + K]);
    b
}

/// Element `i` of a `long` array (`word` = 4 or 8), sign-extended.
pub fn get_long(buf: &[u8], word: usize, i: usize) -> i64 {
    if word == 4 {
        i32::from_le_bytes(le_bytes(buf, i * 4)) as i64
    } else {
        i64::from_le_bytes(le_bytes(buf, i * 8))
    }
}

/// Stores `v` as element `i` of a `long` array. On 32-bit the caller has checked the
/// range already (the control's own minimum and maximum are 32-bit there).
pub fn put_long(buf: &mut [u8], word: usize, i: usize, v: i64) {
    if word == 4 {
        buf[i * 4..i * 4 + 4].copy_from_slice(&(v as i32).to_le_bytes());
    } else {
        buf[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
}

pub fn get_u32(buf: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(le_bytes(buf, i * 4))
}
pub fn put_u32(buf: &mut [u8], i: usize, v: u32) {
    buf[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
}
pub fn get_i64(buf: &[u8], i: usize) -> i64 {
    i64::from_le_bytes(le_bytes(buf, i * 8))
}
pub fn put_i64(buf: &mut [u8], i: usize, v: i64) {
    buf[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
}

/// `value.integer` of `snd_ctl_elem_info`: min, max, step as `long`.
pub fn info_integer(raw: &[u8; 128], word: usize) -> (i64, i64, i64) {
    (get_long(raw, word, 0), get_long(raw, word, 1), get_long(raw, word, 2))
}
/// `value.integer64`: min, max, step.
pub fn info_integer64(raw: &[u8; 128]) -> (i64, i64, i64) {
    (get_i64(raw, 0), get_i64(raw, 1), get_i64(raw, 2))
}
/// `value.enumerated.items`.
pub fn info_enum_items(raw: &[u8; 128]) -> u32 {
    get_u32(raw, 0)
}
/// Sets `value.enumerated.item`, the item whose name the next ELEM_INFO returns.
pub fn info_set_enum_item(raw: &mut [u8; 128], item: u32) {
    put_u32(raw, 1, item);
}
/// `value.enumerated.name`.
pub fn info_enum_name(raw: &[u8; 128]) -> String {
    cstr(&raw[8..72])
}

/// A fixed-size, NUL-padded kernel string.
pub fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Copies `s` into a fixed-size kernel name field; false when it does not fit.
pub fn set_cstr(dst: &mut [u8], s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() >= dst.len() || b.contains(&0) {
        return false;
    }
    dst.fill(0);
    dst[..b.len()].copy_from_slice(b);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of};

    #[test]
    fn struct_sizes_both_abis() {
        assert_eq!(size_of::<ElemId>(), 64);
        assert_eq!(size_of::<CardInfo>(), 376);
        assert_eq!(size_of::<ElemInfo>(), 272);
        assert_eq!(offset_of!(ElemInfo, value), 80);
        assert_eq!(size_of::<ElemList<u32>>(), 72);
        assert_eq!(size_of::<ElemList<u64>>(), 80);
        assert_eq!(size_of::<ElemValue<512>>(), 712);
        assert_eq!(size_of::<ElemValue<1024>>(), 1224);
        assert_eq!(offset_of!(ElemValue<512>, value), 72);
        assert_eq!(offset_of!(ElemValue<1024>, value), 72);
        assert_eq!(size_of::<Interval>(), 12);
        assert_eq!(size_of::<Mask>(), 32);
        assert_eq!(size_of::<HwParams<u32>>(), 604);
        assert_eq!(size_of::<HwParams<u64>>(), 608);
        assert_eq!(offset_of!(HwParams<u64>, intervals), 260);
        assert_eq!(offset_of!(HwParams<u64>, rmask), 512);
        assert_eq!(size_of::<SwParams<u32>>(), 104);
        assert_eq!(size_of::<SwParams<u64>>(), 136);
        assert_eq!(offset_of!(SwParams<u64>, avail_min), 16);
        assert_eq!(offset_of!(SwParams<u32>, avail_min), 12);
        assert_eq!(size_of::<Xferi<u32>>(), 12);
        assert_eq!(size_of::<Xferi<u64>>(), 24);
        assert_eq!(align_of::<Raw8<4>>(), 8);
    }

    #[test]
    fn ioctl_numbers_64_bit() {
        assert_eq!(iowr(b'U', 0x10, size_of::<ElemList<u64>>()), 0xC050_5510);
        assert_eq!(iowr(b'U', 0x12, size_of::<ElemValue<1024>>()), 0xC4C8_5512);
        assert_eq!(iowr(b'U', 0x13, size_of::<ElemValue<1024>>()), 0xC4C8_5513);
        assert_eq!(iowr(b'A', 0x11, size_of::<HwParams<u64>>()), 0xC260_4111);
        assert_eq!(iowr(b'A', 0x13, size_of::<SwParams<u64>>()), 0xC088_4113);
        assert_eq!(iow(b'A', 0x50, size_of::<Xferi<u64>>()), 0x4018_4150);
        assert_eq!(ior(b'A', 0x51, size_of::<Xferi<u64>>()), 0x8018_4151);
    }

    #[test]
    fn ioctl_numbers_32_bit() {
        assert_eq!(iowr(b'U', 0x10, size_of::<ElemList<u32>>()), 0xC048_5510);
        assert_eq!(iowr(b'U', 0x12, size_of::<ElemValue<512>>()), 0xC2C8_5512);
        assert_eq!(iowr(b'U', 0x13, size_of::<ElemValue<512>>()), 0xC2C8_5513);
        assert_eq!(iowr(b'A', 0x11, size_of::<HwParams<u32>>()), 0xC25C_4111);
        assert_eq!(iowr(b'A', 0x13, size_of::<SwParams<u32>>()), 0xC068_4113);
        assert_eq!(iow(b'A', 0x50, size_of::<Xferi<u32>>()), 0x400C_4150);
        assert_eq!(ior(b'A', 0x51, size_of::<Xferi<u32>>()), 0x800C_4151);
    }

    #[test]
    fn ioctl_numbers_same_on_both() {
        assert_eq!(SNDRV_CTL_IOCTL_PVERSION, 0x8004_5500);
        assert_eq!(SNDRV_CTL_IOCTL_CARD_INFO, 0x8178_5501);
        assert_eq!(SNDRV_CTL_IOCTL_ELEM_INFO, 0xC110_5511);
        assert_eq!(SNDRV_PCM_IOCTL_PREPARE, 0x0000_4140);
        assert_eq!(SNDRV_PCM_IOCTL_DROP, 0x0000_4143);
    }

    #[test]
    fn native_numbers_match_this_build() {
        if WORD == 8 {
            assert_eq!(SNDRV_CTL_IOCTL_ELEM_READ, 0xC4C8_5512);
            assert_eq!(SNDRV_PCM_IOCTL_HW_PARAMS, 0xC260_4111);
            assert_eq!(SNDRV_PCM_IOCTL_READI_FRAMES, 0x8018_4151);
        } else {
            assert_eq!(SNDRV_CTL_IOCTL_ELEM_READ, 0xC2C8_5512);
            assert_eq!(SNDRV_PCM_IOCTL_HW_PARAMS, 0xC25C_4111);
            assert_eq!(SNDRV_PCM_IOCTL_READI_FRAMES, 0x800C_4151);
        }
        assert_eq!(SNDRV_CTL_IOCTL_ELEM_LIST & 0xffff, 0x5510);
        assert_eq!(SNDRV_CTL_IOCTL_ELEM_WRITE & 0xffff, 0x5513);
        assert_eq!(SNDRV_PCM_IOCTL_SW_PARAMS & 0xffff, 0x4113);
        assert_eq!(SNDRV_PCM_IOCTL_WRITEI_FRAMES & 0xffff, 0x4150);
    }

    #[test]
    fn long_values_both_widths() {
        let mut b = [0u8; 64];
        put_long(&mut b, 4, 1, -5);
        assert_eq!(get_long(&b, 4, 1), -5);
        assert_eq!(&b[4..8], &(-5i32).to_le_bytes());
        put_long(&mut b, 8, 2, 4_294_967_295);
        assert_eq!(get_long(&b, 8, 2), 4_294_967_295);
        assert_eq!(&b[16..24], &4_294_967_295i64.to_le_bytes());
    }

    #[test]
    fn info_union_fields() {
        let mut raw = [0u8; 128];
        put_long(&mut raw, 4, 0, 0);
        put_long(&mut raw, 4, 1, 100);
        put_long(&mut raw, 4, 2, 1);
        assert_eq!(info_integer(&raw, 4), (0, 100, 1));
        let mut raw = [0u8; 128];
        put_long(&mut raw, 8, 0, -10);
        put_long(&mut raw, 8, 1, 10);
        assert_eq!(info_integer(&raw, 8), (-10, 10, 0));
        let mut raw = [0u8; 128];
        put_u32(&mut raw, 0, 3);
        info_set_enum_item(&mut raw, 2);
        raw[8..11].copy_from_slice(b"DL\0");
        assert_eq!(info_enum_items(&raw), 3);
        assert_eq!(get_u32(&raw, 1), 2);
        assert_eq!(info_enum_name(&raw), "DL");
    }

    #[test]
    fn kernel_names() {
        let mut n = [0u8; CTL_NAME_LEN];
        assert!(set_cstr(&mut n, "Incall Mic Mute"));
        assert_eq!(cstr(&n), "Incall Mic Mute");
        assert!(!set_cstr(&mut n, &"x".repeat(CTL_NAME_LEN)));
        assert!(!set_cstr(&mut n, "a\0b"));
    }
}
