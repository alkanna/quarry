//! What the system reports about the process memory beyond its total, read only while the memory readout's tooltip
//! is open, so the readout costs nothing while it is off or not hovered.

/// What the system reports about the rest of the process memory. A figure the platform cannot tell is None.
#[derive(Clone, Copy, Default)]
pub struct SysMem {
    /// The heap in use: Quarry's own data and that of the libraries it loads.
    pub malloc: Option<usize>,
    /// Heap freed but kept for later allocations, not yet given back to the system.
    pub reuse: Option<usize>,
    /// The window's drawing surfaces, drawn images and GPU memory mapped into the process.
    pub window: Option<usize>,
    /// Pages the libraries and the binary have written to: their global data.
    pub lib_data: Option<usize>,
    /// Resident code and other mapped files, such as fonts.
    pub code: Option<usize>,
    /// Whether `window` holds egui's textures, so they can be shown apart from it.
    pub window_has_textures: bool,
}

static LAST: std::sync::Mutex<Option<(std::time::Instant, SysMem)>> = std::sync::Mutex::new(None);
static READING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The last reading, None before the first. Once it is a second old a new one is taken on a thread of its own, so the
/// walk over the process memory never holds up a frame, and the window repaints when it lands.
pub fn latest(wake: impl Fn() + Send + 'static) -> Option<SysMem> {
    use std::sync::atomic::Ordering;
    let last = *LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_none_or(|(at, _)| at.elapsed().as_secs() >= 1) && !READING.swap(true, Ordering::AcqRel) {
        std::thread::spawn(move || {
            let s = read();
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), s));
            READING.store(false, Ordering::Release);
            wake();
        });
    }
    last.map(|l| l.1)
}

#[cfg(target_os = "macos")]
pub fn read() -> SysMem {
    const TASK_VM_INFO: libc::task_flavor_t = 22;
    // libc deprecates its mach_task_self for the mach2 crate and has no region walk, so both are declared here.
    unsafe extern "C" {
        static mach_task_self_: libc::mach_port_t;
        fn mach_vm_region_recurse(task: libc::mach_port_t, addr: *mut u64, size: *mut u64, depth: *mut u32, info: *mut u32, count: *mut u32) -> libc::kern_return_t;
    }
    let task = unsafe { mach_task_self_ };
    let mut s: libc::malloc_statistics_t = unsafe { std::mem::zeroed() };
    // A null zone sums every malloc zone.
    unsafe { libc::malloc_zone_statistics(std::ptr::null_mut(), &mut s) };
    // task_vm_info_data_t as its 93 32-bit words; the byte offsets are from the macOS SDK header.
    let mut w = [0u32; 93];
    let mut n = w.len() as libc::mach_msg_type_number_t;
    let ok = unsafe { libc::task_info(task, TASK_VM_INFO, w.as_mut_ptr().cast(), &mut n) } == 0;
    let at = |byte: usize| (w[byte / 4] as u64 | (w[byte / 4 + 1] as u64) << 32) as usize;
    // ledger_tag_graphics_footprint and its compressed part, which older kernels leave out of a shorter answer.
    let graphics = if ok && n as usize * 4 >= 288 { at(272) + at(280) } else { 0 };
    // Dirty pages by the tag the system gives each region, as footprint(1) counts them. Submaps are the shared library
    // cache, whose dirty pages are the libraries' written data.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let (mut heap, mut window, mut lib_data) = (0, graphics, 0);
    let (mut addr, mut size, mut depth) = (0u64, 0u64, 0u32);
    loop {
        // vm_region_submap_info_64 as its 21 words: user_tag, pages_swapped_out, pages_dirtied and is_submap at words 5, 8, 9 and 12.
        let mut i = [0u32; 21];
        let mut n = i.len() as u32;
        if unsafe { mach_vm_region_recurse(task, &mut addr, &mut size, &mut depth, i.as_mut_ptr(), &mut n) } != 0 {
            break;
        }
        if i[12] != 0 {
            // The same address again, one level down.
            depth += 1;
            continue;
        }
        let dirty = (i[8] + i[9]) as usize * page;
        match i[5] {
            _ if depth > 0 => lib_data += dirty,
            // The malloc tags, VM_MEMORY_MALLOC to VM_MEMORY_MALLOC_PROB_GUARD.
            1..=13 => heap += dirty,
            // IOSurface, CoreAnimation, CGImage, CoreGraphics and CoreUI. IOAccelerator is in the graphics ledger.
            88 | 51 | 52 | 42 | 54..=58 | 76 | 77 | 103 => window += dirty,
            _ => {}
        }
        addr += size;
    }
    // Clean code pages are not part of the macOS footprint, so there is no code line.
    SysMem { malloc: Some(s.size_in_use), reuse: Some(heap.saturating_sub(s.size_in_use)), window: Some(window), lib_data: Some(lib_data), code: None, window_has_textures: true }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn read() -> SysMem {
    let m = unsafe { libc::mallinfo2() };
    let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap_or_default();
    let (window, lib_data, code) = smaps_parts(&smaps);
    SysMem { malloc: Some(m.uordblks + m.hblkhd), reuse: Some(m.fordblks), window: Some(window), lib_data: Some(lib_data), code: Some(code), window_has_textures: false }
}

/// Resident bytes of GPU and window buffers, of written library data, and of code and other mapped files, from smaps.
#[cfg(any(test, all(target_os = "linux", target_env = "gnu")))]
fn smaps_parts(smaps: &str) -> (usize, usize, usize) {
    enum Kind {
        Gpu,
        File,
        Other,
    }
    let (mut kind, mut window, mut lib_data, mut code) = (Kind::Other, 0, 0, 0);
    for l in smaps.lines() {
        let mut t = l.split_whitespace();
        let Some(first) = t.next() else { continue };
        let Some(key) = first.strip_suffix(':') else {
            // A mapping's header: range, permissions, offset, device, inode and the path, which can hold spaces.
            let path = l.split_whitespace().skip(5).collect::<Vec<_>>().join(" ");
            // DRM devices and the buffers drivers map through anonymous inodes, and the shared memory Wayland and X11
            // hand window contents through. Matched by prefix, so driver libraries such as libdrm stay files.
            kind = if ["/dev/dri", "anon_inode:", "/memfd:", "/SYSV", "/dmabuf"].iter().any(|k| path.starts_with(k)) {
                Kind::Gpu
            } else if path.starts_with('/') {
                Kind::File
            } else {
                Kind::Other
            };
            continue;
        };
        let Some(bytes) = t.next().and_then(|v| v.parse::<usize>().ok()).map(|v| v << 10) else { continue };
        match (key, &kind) {
            ("Rss", Kind::Gpu) => window += bytes,
            ("Rss", Kind::File) => code += bytes,
            // Written pages of a file mapping are its private copy: a library's global data.
            ("Private_Dirty", Kind::File) => {
                code -= bytes.min(code);
                lib_data += bytes;
            }
            _ => {}
        }
    }
    (window, lib_data, code)
}

#[cfg(windows)]
pub fn read() -> SysMem {
    use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapSummary, VirtualQuery, HEAP_SUMMARY, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_IMAGE, MEM_PRIVATE};
    use windows_sys::Win32::System::ProcessStatus::K32QueryWorkingSet;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut h = HEAP_SUMMARY { cb: size_of::<HEAP_SUMMARY>() as u32, ..Default::default() };
    // The process heap, which Rust and the C runtime both allocate from.
    let heap = unsafe { HeapSummary(GetProcessHeap(), 0, &mut h) } != 0;
    // Committed regions that are not private memory, ascending: images (the binary and DLLs) and mapped files.
    let mut regions = Vec::new();
    let mut addr = 0usize;
    loop {
        let mut m: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { VirtualQuery(addr as *const _, &mut m, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
            break;
        }
        let (start, end) = (m.BaseAddress as usize, m.BaseAddress as usize + m.RegionSize);
        if m.State == MEM_COMMIT && m.Type != MEM_PRIVATE {
            regions.push((start, end, m.Type == MEM_IMAGE));
        }
        addr = end;
    }
    // PSAPI_WORKING_SET_INFORMATION as words: the entry count, then one word per resident page holding the page address
    // above bit 12 and, in bit 8, whether the page is shared.
    let mut buf: Vec<usize> = vec![0; 1 << 16];
    loop {
        let ok = unsafe { K32QueryWorkingSet(GetCurrentProcess(), buf.as_mut_ptr().cast(), (buf.len() * size_of::<usize>()) as u32) } != 0;
        if ok || buf[0] < buf.len() {
            break;
        }
        buf = vec![0; buf[0] + 4096];
    }
    let (mut lib_data, mut code) = (0, 0);
    for &e in &buf[1..=buf[0].min(buf.len() - 1)] {
        let page = e & !0xfff;
        let i = regions.partition_point(|r| r.1 <= page);
        let Some(&(_, _, image)) = regions.get(i).filter(|r| r.0 <= page) else { continue };
        // A private page of an image is one the module wrote to: its global data. Shared ones are code and constants.
        if image && e & 0x100 == 0 {
            lib_data += 4096;
        } else {
            code += 4096;
        }
    }
    // ponytail: GPU memory sits outside the working set on Windows; DXGI QueryVideoMemoryInfo could show it beside the total.
    SysMem { malloc: heap.then_some(h.cbAllocated), reuse: heap.then(|| h.cbCommitted.saturating_sub(h.cbAllocated)), window: None, lib_data: Some(lib_data), code: Some(code), window_has_textures: false }
}

#[cfg(not(any(target_os = "macos", windows, all(target_os = "linux", target_env = "gnu"))))]
pub fn read() -> SysMem {
    SysMem::default()
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(any(target_os = "macos", windows, all(target_os = "linux", target_env = "gnu")))]
    fn a_large_allocation_shows_in_the_heap_figure() {
        // The figure covers the whole process, so a test running alongside can free a large block between the two
        // reads and hide this one. That is momentary, so a few tries tell a heap figure that misses allocations from noise.
        let grew = (0..5).any(|_| {
            let before = super::read().malloc.unwrap();
            let v = std::hint::black_box(vec![0u8; 256 << 20]);
            let after = super::read().malloc.unwrap();
            drop(v);
            after > before + (128 << 20)
        });
        assert!(grew, "a 256 MB allocation never showed in the heap figure");
    }

    #[test]
    fn smaps_sorts_mappings_into_gpu_library_data_and_code() {
        let smaps = "\
7f0000000000-7f0000100000 r-xp 00000000 08:01 42 /usr/lib/libfoo.so
Rss:                 400 kB
Private_Dirty:         0 kB
7f0000100000-7f0000200000 rw-p 00100000 08:01 42 /usr/lib/libfoo.so
Rss:                  64 kB
Private_Dirty:        48 kB
7f0000200000-7f0000300000 rw-s 00000000 00:05 7 /dev/dri/renderD128
Rss:                1024 kB
Private_Dirty:         0 kB
7f0000300000-7f0000400000 rw-p 00000000 00:00 0
Rss:                2048 kB
Private_Dirty:      2048 kB
";
        assert_eq!(super::smaps_parts(smaps), (1024 << 10, 48 << 10, 416 << 10));
    }
}
