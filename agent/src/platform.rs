use super::{Error, Result};
use std::env;
use std::ffi::CString;
use std::io::{self, Read, Write};
use std::os::fd::RawFd;

const NESTED_AGENT_ENV: &str = "SHELLO_AGENT_ACTIVE";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

pub fn default_shell() -> String {
    env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

pub fn terminal_size() -> Result<TerminalSize> {
    let mut winsize: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut winsize) } != 0 {
        return Err(Error::Io(io::Error::last_os_error()));
    }
    Ok(TerminalSize {
        cols: winsize.ws_col,
        rows: winsize.ws_row,
    })
}

pub struct Pty {
    master: RawFd,
    child: libc::pid_t,
}

impl Pty {
    pub fn spawn(shell: &str, size: TerminalSize) -> Result<Self> {
        let mut master = -1;
        let mut winsize = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let child = unsafe {
            libc::forkpty(
                &mut master,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut winsize,
            )
        };
        if child < 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        if child == 0 {
            env::set_var(NESTED_AGENT_ENV, "1");
            env::set_var("TERM", "xterm-256color");
            env::remove_var("TERM_PROGRAM");
            env::remove_var("KITTY_WINDOW_ID");
            let shell = CString::new(shell).unwrap_or_else(|_| CString::new("/bin/sh").unwrap());
            let argv = [shell.as_ptr(), std::ptr::null()];
            unsafe {
                libc::execvp(shell.as_ptr(), argv.as_ptr());
                libc::_exit(127);
            }
        }
        Ok(Self { master, child })
    }

    pub fn try_clone(&self) -> Result<PtyHandle> {
        let fd = unsafe { libc::dup(self.master) };
        if fd < 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        Ok(PtyHandle(fd))
    }

    pub fn wait(&mut self) -> Result<()> {
        let mut status = 0;
        unsafe { libc::waitpid(self.child, &mut status, 0) };
        Ok(())
    }

    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        let mut winsize = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(self.master, libc::TIOCSWINSZ, &mut winsize) } != 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe { libc::close(self.master) };
    }
}

pub struct PtyHandle(RawFd);

impl PtyHandle {
    pub fn resize(&mut self, size: TerminalSize) -> Result<()> {
        let mut winsize = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(self.0, libc::TIOCSWINSZ, &mut winsize) } != 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        Ok(())
    }
}

impl Read for PtyHandle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe { libc::read(self.0, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EIO) {
                return Ok(0);
            }
            return Err(error);
        }
        Ok(n as usize)
    }
}

impl Write for PtyHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = unsafe { libc::write(self.0, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

pub struct RawTerminal(libc::termios);

impl RawTerminal {
    pub fn enter() -> Result<Self> {
        let mut state = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(0, &mut state) } != 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        let mut raw = state;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        Ok(Self(state))
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.0) };
    }
}
