//! Darwin link-state checks matching .NET 10's `GetNativeIPInterfaceStatistics`.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

// Darwin net/if.h and net/if_media.h ABI. libc does not expose ifmediareq.
#[repr(C)]
struct FlagsRequest {
    name: [libc::c_char; libc::IFNAMSIZ],
    flags: libc::c_short,
    padding: [u8; 14],
}

#[repr(C)]
struct MediaRequest {
    name: [libc::c_char; libc::IFNAMSIZ],
    current: libc::c_int,
    mask: libc::c_int,
    status: libc::c_int,
    active: libc::c_int,
    count: libc::c_int,
    list: *mut libc::c_int,
}

const _: () = assert!(std::mem::size_of::<FlagsRequest>() == 32);
const _: () = assert!(std::mem::size_of::<MediaRequest>() == 48);

// _IOWR('i', 17, struct ifreq) and _IOWR('i', 56, struct ifmediareq).
const GET_FLAGS: libc::c_ulong = 0xc020_6911;
const GET_MEDIA: libc::c_ulong = 0xc030_6938;
const MEDIA_VALID: libc::c_int = 1;
const MEDIA_ACTIVE: libc::c_int = 2;

pub(super) fn is_up(name: &str) -> io::Result<bool> {
    if name.len() >= libc::IFNAMSIZ {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "interface name exceeds IFNAMSIZ",
        ));
    }
    let mut interface_name = [0; libc::IFNAMSIZ];
    for (target, source) in interface_name.iter_mut().zip(name.bytes()) {
        *target = libc::c_char::from_ne_bytes([source]);
    }
    // SAFETY: socket has no pointer arguments; the returned descriptor is checked.
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if socket < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this function exclusively owns the successfully created descriptor.
    let socket = unsafe { OwnedFd::from_raw_fd(socket) };
    let mut flags = FlagsRequest {
        name: interface_name,
        flags: 0,
        padding: [0; 14],
    };
    // SAFETY: FlagsRequest matches Darwin's 32-byte ifreq; ioctl writes that buffer.
    if unsafe { libc::ioctl(socket.as_raw_fd(), GET_FLAGS, &raw mut flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if i32::from(flags.flags) & libc::IFF_UP == 0 {
        return Ok(false);
    }
    let mut media = MediaRequest {
        name: interface_name,
        current: 0,
        mask: 0,
        status: 0,
        active: 0,
        count: 0,
        list: std::ptr::null_mut(),
    };
    // SAFETY: MediaRequest matches Darwin's 48-byte ifmediareq; count=0 means
    // the kernel never dereferences the null media-list pointer.
    if unsafe { libc::ioctl(socket.as_raw_fd(), GET_MEDIA, &raw mut media) } < 0 {
        let error = io::Error::last_os_error();
        // Virtual devices have no media information; .NET accepts administrative UP.
        if matches!(error.raw_os_error(), Some(libc::EOPNOTSUPP | libc::EINVAL)) {
            return Ok(true);
        }
        return Err(error);
    }
    Ok(media.status & (MEDIA_VALID | MEDIA_ACTIVE) == (MEDIA_VALID | MEDIA_ACTIVE))
}
