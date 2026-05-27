use std::error::Error;
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;

use bypass_io::UringBackend;

fn main() -> Result<(), Box<dyn Error>> {
    let backend = match UringBackend::new(8) {
        Ok(backend) => backend,
        Err(err) => {
            eprintln!("io_uring unavailable on this host: {err}");
            return Ok(());
        }
    };

    let path = std::env::temp_dir().join(format!(
        "bypass-io-uring-example-{}.bin",
        std::process::id()
    ));
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)?;

    let message = b"bypass-io example\n";
    let written = backend.write_at(file.as_raw_fd(), message, 0)?;
    backend.fsync(file.as_raw_fd())?;

    let mut read_back = vec![0; message.len()];
    let read = backend.read_at(file.as_raw_fd(), &mut read_back, 0)?;
    fs::remove_file(&path).ok();

    println!(
        "uring_example path={} written={} read={} payload={:?}",
        path.display(),
        written,
        read,
        String::from_utf8_lossy(&read_back)
    );
    Ok(())
}
