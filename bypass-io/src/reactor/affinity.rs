use std::ffi::c_void;
use std::io;
use std::mem::size_of;

const CPU_SETSIZE: usize = 1024;
const WORD_BITS: usize = usize::BITS as usize;
const CPU_WORDS: usize = CPU_SETSIZE / WORD_BITS;

unsafe extern "C" {
    fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const c_void) -> i32;
}

/// Pin the calling thread to `cpu`.
///
/// # Errors
///
/// Returns an OS error if Linux rejects the affinity mask or if `cpu` is outside
/// the supported mask size.
pub fn set_cpu_affinity(cpu: usize) -> io::Result<()> {
    if cpu >= CPU_SETSIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cpu index exceeds CPU_SETSIZE",
        ));
    }

    let mut mask = [0usize; CPU_WORDS];
    mask[cpu / WORD_BITS] |= 1usize << (cpu % WORD_BITS);

    // Safety: `mask` points to a valid CPU bitset for the duration of the call.
    let rc = unsafe { sched_setaffinity(0, size_of::<[usize; CPU_WORDS]>(), mask.as_ptr().cast()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
