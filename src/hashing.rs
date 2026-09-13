//! Hashes used by the original GSCU and configuration code. Arithmetic wraps in all profiles.
pub const fn canon_hash(input: &[u8]) -> u32 {
    let mut hash = 0x4B9ACE2Fu32;
    let mut i = 0;
    while i < input.len() && input[i] != 0 {
        let c = input[i].to_ascii_lowercase() as i8 as i32 as u32;
        let sum = hash.wrapping_add(c);
        let mixed = sum ^ sum.wrapping_shl(10);
        hash = mixed.wrapping_add(mixed >> 6);
        i += 1;
    }
    let mixed = hash.wrapping_mul(9);
    0x8001u32.wrapping_mul(mixed ^ (mixed >> 11))
}

pub const fn canon_hash64(input: &[u8]) -> u64 {
    let mut hash = 14695981039346656037u64;
    let mut i = 0;
    while i < input.len() && input[i] != 0 {
        // MSVC's default char is signed. Preserve the sign extension before the XOR,
        // including high bytes in UTF-8 passwords (verified against the C++ reference).
        hash =
            (hash ^ input[i].to_ascii_lowercase() as i8 as i64 as u64).wrapping_mul(1099511628211);
        i += 1;
    }
    hash & 0x7FFF_FFFF_FFFF_FFFF
}

pub const fn fnv1a(input: &[u8]) -> u32 {
    let mut hash = 0x4B9ACE2Fu32;
    let mut i = 0;
    while i < input.len() && input[i] != 0 {
        hash = (hash ^ input[i] as i8 as i32 as u32).wrapping_mul(0x1000193);
        i += 1;
    }
    hash.wrapping_mul(0x1000193)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpp_reference_vectors() {
        // Captured from the original C++ before legacy-source cleanup; see docs/VALIDATION.md.
        for (text, canon, canon64, fnv) in [
            ("", 0xC1243180, 0x4BF29CE484222325, 0x33B293FD),
            (
                "serious_anticrash_2023",
                0x272868BE,
                0x74716513BB6E9ADB,
                0xC8358E7B,
            ),
            ("AbC", 0x64B235EB, 0x671FA2190541574B, 0x496E9F13),
            ("playername", 0x0206812E, 0x33FB5C90491BF903, 0xB3D32C97),
            (
                "networkpassword",
                0x097EC228,
                0x6BE42D5008679978,
                0x115819CA,
            ),
            ("Password123!", 0x7C00F0B3, 0x6FE627F97D2B951B, 0x32623DBB),
        ] {
            assert_eq!(canon_hash(text.as_bytes()), canon, "{text}");
            assert_eq!(canon_hash64(text.as_bytes()), canon64, "{text}");
            assert_eq!(fnv1a(text.as_bytes()), fnv, "{text}");
        }
        assert_eq!(canon_hash(b"ABC\0ignored"), canon_hash(b"abc"));
        assert_eq!(canon_hash64(b"ABC\0ignored"), canon_hash64(b"abc"));
        assert_ne!(fnv1a(b"ABC"), fnv1a(b"abc"));
        assert_eq!(canon_hash(b"\xc3\x91"), 0xCB81ADF7);
        assert_eq!(canon_hash64(b"\xc3\x91"), 0x080D3B07B4CBD0D9);
        assert_eq!(canon_hash64(b"\xff"), 0x509C41B379FE466E);
    }
}
