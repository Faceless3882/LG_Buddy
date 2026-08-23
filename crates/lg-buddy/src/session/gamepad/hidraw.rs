use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const HID_REPORT_BUFFER_SIZE: usize = 64;

#[derive(Debug)]
pub(crate) struct RawHidReportReader {
    file: File,
}

impl RawHidReportReader {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;

        Ok(Self { file })
    }

    pub(crate) fn read_available(&mut self) -> io::Result<Vec<Vec<u8>>> {
        let mut reports = Vec::new();
        let mut buffer = [0_u8; HID_REPORT_BUFFER_SIZE];

        loop {
            match self.file.read(&mut buffer) {
                Ok(0) => return Ok(reports),
                Ok(length) => reports.push(buffer[..length].to_vec()),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(reports),
                Err(err) => return Err(err),
            }
        }
    }
}
