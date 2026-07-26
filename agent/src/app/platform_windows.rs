use super::{Error, Result, NESTED_AGENT_ENV};
use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, GetConsoleMode, GetConsoleScreenBufferInfo,
    GetStdHandle, ResizePseudoConsole, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
    CONSOLE_SCREEN_BUFFER_INFO, COORD, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
    ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, HPCON,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Environment::{GetEnvironmentVariableW, SetEnvironmentVariableW};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    STARTUPINFOEXW,
};

const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;
const UTF8_CODE_PAGE: u32 = 65001;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

pub fn default_shell() -> String {
    for candidate in [
        r"C:\Program Files\PowerShell\7\pwsh.exe",
        r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        r"C:\Windows\System32\cmd.exe",
    ] {
        if std::path::Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    "cmd.exe".to_string()
}

pub fn terminal_size() -> Result<TerminalSize> {
    unsafe {
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        if output == INVALID_HANDLE_VALUE || output == 0 {
            return Err(last_error("GetStdHandle stdout failed"));
        }
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = zeroed();
        if GetConsoleScreenBufferInfo(output, &mut info) == 0 {
            return Err(last_error("GetConsoleScreenBufferInfo failed"));
        }
        Ok(TerminalSize {
            cols: (info.srWindow.Right - info.srWindow.Left + 1) as u16,
            rows: (info.srWindow.Bottom - info.srWindow.Top + 1) as u16,
        })
    }
}

pub struct Pty {
    input: HANDLE,
    output: HANDLE,
    process: HANDLE,
    thread: HANDLE,
    console: HPCON,
}

impl Pty {
    pub fn spawn(shell: &str) -> Result<Self> {
        unsafe {
            let input = PipePair::new()?;
            let output = PipePair::new()?;
            let mut console: HPCON = 0;
            let size = COORD { X: 120, Y: 30 };
            if CreatePseudoConsole(size, input.read, output.write, 0, &mut console) != 0 {
                return Err(last_error("CreatePseudoConsole failed"));
            }

            let mut attrs = AttributeList::new(console)?;
            let mut startup: STARTUPINFOEXW = zeroed();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.lpAttributeList = attrs.list;

            let mut command = quote_command(shell);
            let _env = ChildEnvMarker::set()?;
            let mut process_info: PROCESS_INFORMATION = zeroed();
            if CreateProcessW(
                null(),
                command.as_mut_ptr(),
                null(),
                null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT,
                null(),
                null(),
                &startup.StartupInfo,
                &mut process_info,
            ) == 0
            {
                return Err(last_error("CreateProcessW failed"));
            }

            CloseHandle(input.read);
            CloseHandle(output.write);
            attrs.close();

            Ok(Self {
                input: input.write,
                output: output.read,
                process: process_info.hProcess,
                thread: process_info.hThread,
                console,
            })
        }
    }

    pub fn try_clone(&self) -> Result<PtyHandle> {
        Ok(PtyHandle {
            input: self.input,
            output: self.output,
            console: self.console,
        })
    }

    pub fn wait(&mut self) -> Result<()> {
        unsafe {
            WaitForSingleObject(self.process, INFINITE);
            let mut exit_code = 0;
            if GetExitCodeProcess(self.process, &mut exit_code) == 0 {
                return Err(last_error("GetExitCodeProcess failed"));
            }
        }
        Ok(())
    }

    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        unsafe {
            let coord = COORD {
                X: size.cols as i16,
                Y: size.rows as i16,
            };
            if ResizePseudoConsole(self.console, coord) != 0 {
                return Err(last_error("ResizePseudoConsole failed"));
            }
        }
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.input);
            CloseHandle(self.output);
            CloseHandle(self.thread);
            CloseHandle(self.process);
            ClosePseudoConsole(self.console);
        }
    }
}

pub struct PtyHandle {
    input: HANDLE,
    output: HANDLE,
    console: HPCON,
}

unsafe impl Send for PtyHandle {}

impl PtyHandle {
    pub fn resize(&mut self, size: TerminalSize) -> Result<()> {
        unsafe {
            let coord = COORD {
                X: size.cols as i16,
                Y: size.rows as i16,
            };
            if ResizePseudoConsole(self.console, coord) != 0 {
                return Err(last_error("ResizePseudoConsole failed"));
            }
        }
        Ok(())
    }
}

impl Read for PtyHandle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        unsafe {
            let mut read = 0;
            if ReadFile(
                self.output,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut read,
                null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(read as usize)
        }
    }
}

impl Write for PtyHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        unsafe {
            let mut written = 0;
            if WriteFile(
                self.input,
                buf.as_ptr().cast(),
                buf.len() as u32,
                &mut written,
                null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(written as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct RawTerminal {
    input: HANDLE,
    input_mode: u32,
    output: HANDLE,
    output_mode: u32,
    output_active: bool,
}

impl RawTerminal {
    pub fn enter() -> Result<Self> {
        unsafe {
            SetConsoleCP(UTF8_CODE_PAGE);
            SetConsoleOutputCP(UTF8_CODE_PAGE);
            let input = GetStdHandle(STD_INPUT_HANDLE);
            if input == INVALID_HANDLE_VALUE || input == 0 {
                return Err(last_error("GetStdHandle stdin failed"));
            }
            let mut input_mode = 0;
            if GetConsoleMode(input, &mut input_mode) == 0 {
                return Err(last_error("GetConsoleMode stdin failed"));
            }
            let raw =
                input_mode & !ENABLE_ECHO_INPUT & !ENABLE_LINE_INPUT & !ENABLE_PROCESSED_INPUT;
            if SetConsoleMode(input, raw) == 0 {
                return Err(last_error("SetConsoleMode stdin failed"));
            }

            let output = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut output_mode = 0;
            let mut output_active = false;
            if output != INVALID_HANDLE_VALUE
                && output != 0
                && GetConsoleMode(output, &mut output_mode) != 0
            {
                let vt = output_mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
                output_active = SetConsoleMode(output, vt) != 0;
            }

            Ok(Self {
                input,
                input_mode,
                output,
                output_mode,
                output_active,
            })
        }
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        unsafe {
            SetConsoleMode(self.input, self.input_mode);
            if self.output_active {
                SetConsoleMode(self.output, self.output_mode);
            }
        }
    }
}

struct PipePair {
    read: HANDLE,
    write: HANDLE,
}

impl PipePair {
    unsafe fn new() -> Result<Self> {
        let mut read = 0;
        let mut write = 0;
        if CreatePipe(&mut read, &mut write, null(), 0) == 0 {
            return Err(last_error("CreatePipe failed"));
        }
        Ok(Self { read, write })
    }
}

struct AttributeList {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    buffer: Vec<u8>,
}

impl AttributeList {
    unsafe fn new(console: HPCON) -> Result<Self> {
        let mut size = 0;
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
        let mut buffer = vec![0_u8; size];
        let list = buffer.as_mut_ptr().cast();
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            return Err(last_error("InitializeProcThreadAttributeList failed"));
        }
        if UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            console as *mut c_void,
            size_of::<HPCON>(),
            null_mut(),
            null_mut(),
        ) == 0
        {
            return Err(last_error("UpdateProcThreadAttribute failed"));
        }
        Ok(Self { list, buffer })
    }

    unsafe fn close(&mut self) {
        if !self.list.is_null() {
            DeleteProcThreadAttributeList(self.list);
            self.list = null_mut();
        }
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { self.close() }
    }
}

struct ChildEnvMarker {
    previous: Option<Vec<u16>>,
}

impl ChildEnvMarker {
    unsafe fn set() -> Result<Self> {
        let name = wide(NESTED_AGENT_ENV);
        let previous = read_env(&name)?;
        let value = wide("1");
        if SetEnvironmentVariableW(name.as_ptr(), value.as_ptr()) == 0 {
            return Err(last_error("SetEnvironmentVariableW failed"));
        }
        Ok(Self { previous })
    }
}

impl Drop for ChildEnvMarker {
    fn drop(&mut self) {
        unsafe {
            let name = wide(NESTED_AGENT_ENV);
            if let Some(previous) = &self.previous {
                SetEnvironmentVariableW(name.as_ptr(), previous.as_ptr());
            } else {
                SetEnvironmentVariableW(name.as_ptr(), null());
            }
        }
    }
}

unsafe fn read_env(name: &[u16]) -> Result<Option<Vec<u16>>> {
    let needed = GetEnvironmentVariableW(name.as_ptr(), null_mut(), 0);
    if needed == 0 {
        return Ok(None);
    }
    let mut value = vec![0_u16; needed as usize];
    let written = GetEnvironmentVariableW(name.as_ptr(), value.as_mut_ptr(), needed);
    if written == 0 || written >= needed {
        return Err(last_error("GetEnvironmentVariableW failed"));
    }
    Ok(Some(value))
}

fn quote_command(value: &str) -> Vec<u16> {
    wide(&format!("\"{value}\""))
}

fn wide(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

fn last_error(context: &str) -> Error {
    let code = unsafe { GetLastError() };
    Error::Message(format!("{context}: Windows error {code}"))
}
