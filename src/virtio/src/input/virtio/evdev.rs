//! The Linux side of the virtio input device: the uinput sink the device creates in the
//! backend VM, and the evdev node through which it reads what is written into the sink.

use std::fs::{read_dir, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::thread::sleep;
use std::time::Duration;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_MAX: u16 = 0x1f;
pub const KEY_MAX: u16 = 0x2ff;
pub const REL_MAX: u16 = 0x0f;
pub const BTN_MOUSE: u16 = 0x110;
pub const BTN_TASK: u16 = 0x117;
/// The keyboard key codes end here; the button ranges start at 0x100
pub const KEY_KEYBOARD_MAX: u16 = 0xff;
pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_HWHEEL: u16 = 0x06;
pub const REL_WHEEL: u16 = 0x08;
pub const BUS_VIRTUAL: u16 = 0x06;

/// ioctl encoding for arm64 and x86-64: dir[31:30] size[29:16] type[15:8] nr[7:0]
const IOC_READ: u64 = 2;
const IOC_WRITE: u64 = 1;

const fn ioc(dir: u64, ty: u8, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | (ty as u64) << 8 | nr
}

const EVIOCGRAB: u64 = ioc(IOC_WRITE, b'E', 0x90, 4);

/// struct uinput_setup: struct input_id (four u16), char name[80], u32 ff_effects_max
const UINPUT_SETUP_SIZE: u64 = 8 + 80 + 4;
const UI_DEV_CREATE: u64 = ioc(0, b'U', 1, 0);
const UI_DEV_SETUP: u64 = ioc(IOC_WRITE, b'U', 3, UINPUT_SETUP_SIZE);
const UI_SET_EVBIT: u64 = ioc(IOC_WRITE, b'U', 100, 4);
const UI_SET_KEYBIT: u64 = ioc(IOC_WRITE, b'U', 101, 4);
const UI_SET_RELBIT: u64 = ioc(IOC_WRITE, b'U', 102, 4);

const fn ui_get_sysname(len: u64) -> u64 {
    ioc(IOC_READ, b'U', 44, len)
}

/// struct input_event as read from the device: timeval (16 bytes on 64-bit), type, code, value
pub const INPUT_EVENT_SIZE: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    pub ev_type: u16,
    pub code: u16,
    pub value: i32,
}

/// Bitmap of supported codes for one event type, 1024 bits
pub type Bitmap = [u8; 128];

pub fn bit(map: &Bitmap, n: u16) -> bool {
    let n = n as usize;
    n / 8 < map.len() && map[n / 8] & (1 << (n % 8)) != 0
}

fn check(ret: libc::c_int) -> io::Result<()> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The uinput device the backend writes events into. Its evdev node is the device's source;
/// the uinput descriptor is kept open, closing it removes the device.
pub struct Sink {
    _uinput: File,
    pub path: String,
}

impl Sink {
    /// Creates the device with the given keys, buttons and relative axes and returns it once
    /// its evdev node exists
    pub fn create(name: &str, ids: [u16; 4], keys: &Bitmap, rels: &Bitmap) -> io::Result<Sink> {
        let uinput = OpenOptions::new().write(true).open("/dev/uinput")?;
        let fd = uinput.as_raw_fd();
        // SAFETY: the UI_SET_* ioctls take an int by value
        unsafe {
            for ev in [EV_SYN, EV_KEY, EV_REL] {
                check(libc::ioctl(fd, UI_SET_EVBIT as _, ev as libc::c_int))?;
            }
            for code in 0..=KEY_MAX {
                if bit(keys, code) {
                    check(libc::ioctl(fd, UI_SET_KEYBIT as _, code as libc::c_int))?;
                }
            }
            for code in 0..=REL_MAX {
                if bit(rels, code) {
                    check(libc::ioctl(fd, UI_SET_RELBIT as _, code as libc::c_int))?;
                }
            }
        }

        let mut setup = [0u8; UINPUT_SETUP_SIZE as usize];
        for (i, id) in ids.iter().enumerate() {
            setup[i * 2..i * 2 + 2].copy_from_slice(&id.to_ne_bytes());
        }
        let name = name.as_bytes();
        let n = name.len().min(79);
        setup[8..8 + n].copy_from_slice(&name[..n]);
        // SAFETY: UI_DEV_SETUP reads a struct uinput_setup from the buffer handed to it
        unsafe {
            check(libc::ioctl(fd, UI_DEV_SETUP as _, setup.as_ptr()))?;
            check(libc::ioctl(fd, UI_DEV_CREATE as _))?;
        }

        let mut sysname = [0u8; 64];
        // SAFETY: UI_GET_SYSNAME writes at most the length given into the buffer handed to it
        unsafe {
            check(libc::ioctl(
                fd,
                ui_get_sysname(sysname.len() as u64) as _,
                sysname.as_mut_ptr(),
            ))?;
        }
        let end = sysname.iter().position(|&c| c == 0).unwrap_or(sysname.len());
        let sysname = String::from_utf8_lossy(&sysname[..end]).into_owned();

        // The event node follows the device by a moment
        let sysdir = format!("/sys/class/input/{}", sysname);
        for _ in 0..50 {
            if let Ok(dir) = read_dir(&sysdir) {
                for entry in dir.flatten() {
                    let entry = entry.file_name().to_string_lossy().into_owned();
                    if entry.starts_with("event") {
                        let path = format!("/dev/input/{}", entry);
                        if std::path::Path::new(&path).exists() {
                            return Ok(Sink {
                                _uinput: uinput,
                                path,
                            });
                        }
                    }
                }
            }
            sleep(Duration::from_millis(20));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no event node for {}", sysname),
        ))
    }
}

/// The evdev node of the sink, read by the device
pub struct Source {
    pub path: String,
    pub file: File,
    buf: Vec<u8>,
}

impl Source {
    pub fn open(path: &str) -> io::Result<Source> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Ok(Source {
            path: path.to_string(),
            file,
            buf: Vec::new(),
        })
    }

    /// Exclusive access: nothing else in this VM acts on what is written into the sink
    pub fn grab(&mut self) -> io::Result<()> {
        let arg: libc::c_int = 1;
        // SAFETY: EVIOCGRAB takes an int by value
        check(unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB as _, arg) })
    }

    /// Reads what the device has queued and returns the complete events in it
    pub fn read_events(&mut self) -> io::Result<Vec<Event>> {
        let mut chunk = [0u8; INPUT_EVENT_SIZE * 64];
        loop {
            match self.file.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }

        let mut events = Vec::new();
        let whole = self.buf.len() / INPUT_EVENT_SIZE * INPUT_EVENT_SIZE;
        for rec in self.buf[..whole].chunks(INPUT_EVENT_SIZE) {
            events.push(Event {
                ev_type: u16::from_ne_bytes([rec[16], rec[17]]),
                code: u16::from_ne_bytes([rec[18], rec[19]]),
                value: i32::from_ne_bytes([rec[20], rec[21], rec[22], rec[23]]),
            });
        }
        self.buf.drain(..whole);
        Ok(events)
    }
}
