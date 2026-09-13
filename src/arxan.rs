use crate::{
    game_build,
    memory::{self, Patch},
};
use windows_sys::Win32::System::Memory::*;

const PATTERNS: [&[u8]; 3] = [
    &[0x8b, 0x0c, 0x8b, 0x33, 0x0c, 0x82],
    &[0x8b, 0x0c, 0x8b, 0xf7, 0xd9, 0x03, 0x0c, 0x82],
    &[0x8b, 0x04, 0x82, 0x8b, 0x14, 0x8b, 0x3b, 0xc2],
];
const REPLACEMENTS: [&[u8]; 3] = [
    &[0x48, 0x31, 0xc9, 0x90, 0x90, 0x90],
    &[0x48, 0x31, 0xc9, 0x90, 0x90, 0x90, 0x90, 0x90],
    &[0x48, 0x31, 0xc0, 0x48, 0x31, 0xd2],
];
const COUNTS: [usize; 3] = [259, 259, 847];

unsafe fn scan(pattern: &[u8]) -> Vec<usize> {
    let mut matches = Vec::new();
    let base = game_build::image_base();
    let nt = base + memory::read::<u32>(base + 0x3c) as usize;
    let image_size = memory::read::<u32>(nt + 24 + 56) as usize;
    let sections = nt + 24 + memory::read::<u16>(nt + 20) as usize;
    for index in 0..memory::read::<u16>(nt + 6) as usize {
        let section = sections + index * 40;
        if !memory::readable(section, 40) {
            break;
        }
        if memory::read::<u32>(section + 36) & 0x20000000 == 0 {
            continue;
        }
        let rva = memory::read::<u32>(section + 12) as usize;
        if rva >= image_size {
            continue;
        }
        let mut cursor = base + rva;
        let end = cursor + (memory::read::<u32>(section + 8) as usize).min(image_size - rva);
        while cursor < end {
            let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            if VirtualQuery(
                cursor as _,
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            ) == 0
            {
                break;
            }
            let region_end = (info.BaseAddress as usize + info.RegionSize).min(end);
            if region_end <= cursor {
                break;
            }
            if info.State == MEM_COMMIT
                && info.Protect & (PAGE_GUARD | PAGE_NOACCESS) == 0
                && info.Protect
                    & (PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY)
                    != 0
            {
                let bytes = std::slice::from_raw_parts(cursor as *const u8, region_end - cursor);
                matches.extend(
                    bytes
                        .windows(pattern.len())
                        .enumerate()
                        .filter_map(|(i, p)| (p == pattern).then_some(cursor + i)),
                );
            }
            cursor = region_end;
        }
    }
    matches
}

pub struct Integrity {
    patches: Vec<Patch>,
    waiting: bool,
}
impl Integrity {
    pub unsafe fn install() -> Result<Self, String> {
        if game_build::current_build() == game_build::Build::Unknown {
            return Err("Unsupported BO3 executable".into());
        }
        if scan(REPLACEMENTS[0]).len() >= 518
            && scan(REPLACEMENTS[1]).len() >= 259
            && scan(REPLACEMENTS[2]).len() >= 847
        {
            return Ok(Self {
                patches: Vec::new(),
                waiting: false,
            });
        }
        let mut patches = Vec::new();
        for i in 0..3 {
            let matches = scan(PATTERNS[i]);
            if matches.len() != COUNTS[i] {
                return Err(format!(
                    "Arxan pattern {}: expected {}, found {}",
                    i + 1,
                    COUNTS[i],
                    matches.len()
                ));
            }
            for address in matches {
                patches.push(Patch::prepare(address, REPLACEMENTS[i])?);
            }
        }
        for (i, patch) in patches.iter().enumerate() {
            if let Err(error) = patch.apply() {
                for old in patches[..=i].iter().rev() {
                    if let Err(e) = old.restore() {
                        memory::debug(&e);
                    }
                }
                return Err(error);
            }
        }
        Ok(Self {
            patches,
            waiting: true,
        })
    }
    pub unsafe fn maintain(&mut self) {
        let signal = game_build::address(0x1686E948);
        if !self.waiting || !memory::readable(signal, 8) || memory::read::<u64>(signal) == 0 {
            return;
        }
        for patch in &self.patches {
            if !memory::readable(patch.address, patch.replacement.len()) {
                return;
            }
            if std::slice::from_raw_parts(patch.address as *const u8, patch.replacement.len())
                != patch.replacement
            {
                if let Err(e) = patch.apply() {
                    memory::debug(&e);
                    return;
                }
            }
        }
        self.waiting = false;
    }
    pub unsafe fn restore(&self) -> Result<(), String> {
        for patch in self.patches.iter().rev() {
            patch.restore()?;
        }
        Ok(())
    }
}
