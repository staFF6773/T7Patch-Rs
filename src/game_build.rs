use std::sync::OnceLock;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    Unknown,
    February2026,
    September2026,
}

pub const fn fingerprint(timestamp: u32, size: u32) -> Build {
    match (timestamp, size) {
        (0x693D731E, 0x1D74AC00 | 0x1D74B000) => Build::February2026,
        (0x6A7B6355, 0x1D75BC00 | 0x1D75C000) => Build::September2026,
        _ => Build::Unknown,
    }
}

pub const fn translate_rva(rva: usize, build: Build) -> usize {
    if matches!(build, Build::September2026) && rva >= 0x1D29C20 && rva < 0x2EFF000 {
        rva - 0x6C0
    } else {
        rva
    }
}

pub fn image_base() -> usize {
    static BASE: OnceLock<usize> = OnceLock::new();
    *BASE.get_or_init(|| unsafe { GetModuleHandleW(std::ptr::null()) as usize })
}

pub fn current_build() -> Build {
    static BUILD: OnceLock<Build> = OnceLock::new();
    *BUILD.get_or_init(|| unsafe {
        let base = image_base();
        if !crate::memory::readable(base, 0x40) || crate::memory::read::<u16>(base) != 0x5A4D {
            return Build::Unknown;
        }
        let offset = crate::memory::read::<i32>(base + 0x3c);
        if !(0x40..=0x100000).contains(&offset) {
            return Build::Unknown;
        }
        let nt = base + offset as usize;
        if !crate::memory::readable(nt, 0x108)
            || crate::memory::read::<u32>(nt) != 0x4550
            || crate::memory::read::<u16>(nt + 4) != 0x8664
            || crate::memory::read::<u16>(nt + 24) != 0x20b
        {
            return Build::Unknown;
        }
        fingerprint(
            crate::memory::read(nt + 8),
            crate::memory::read(nt + 24 + 56),
        )
    })
}

pub fn address(rva: usize) -> usize {
    image_base() + translate_rva(rva, current_build())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mappings_and_boundaries() {
        assert_eq!(translate_rva(0x1DECFD0, Build::September2026), 0x1DEC910);
        assert_eq!(translate_rva(0x226B0A0, Build::September2026), 0x226A9E0);
        assert_eq!(translate_rva(0x1686E948, Build::September2026), 0x1686E948);
        assert_eq!(translate_rva(0x1D29C1F, Build::September2026), 0x1D29C1F);
        assert_eq!(translate_rva(0x1D29C20, Build::September2026), 0x1D29560);
        assert_eq!(translate_rva(0x2EFF000, Build::September2026), 0x2EFF000);
        assert_eq!(translate_rva(0x1DECFD0, Build::February2026), 0x1DECFD0);
        assert_eq!(fingerprint(0x6A7B6355, 0x1D75C000), Build::September2026);
        assert_eq!(fingerprint(0x6A7B6355, 0x1D74AC00), Build::Unknown);
    }
}
