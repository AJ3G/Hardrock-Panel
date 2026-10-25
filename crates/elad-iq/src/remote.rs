//! Taking an FDM-DUO out of stand-alone, which is what makes it stream wider
//! than 192 kHz.
//!
//! # What stand-alone is
//!
//! The DUO boots its own FPGA, and the image it boots is the stand-alone one:
//! the radio's own receiver and front panel, with a 192 kHz I/Q channel on the
//! side. Every wider rate is a *remote* image that the host uploads over USB,
//! after which the radio stays in remote mode for as long as the host keeps
//! sending it `IQ1;`. Nothing in the radio's menus selects a rate, which is why
//! a DUO left alone reads 192 kHz whatever SPAN asks for.
//!
//! # Where this comes from
//!
//! Two USB captures of SDR Console opening the DUO at 384, 768, 3072 and
//! 6144 kHz, on the owner's own radio. Every request below
//! is one that capture contains, in its order:
//!
//! ```text
//! E1 idx F600 in 1        → F6
//! E1 idx F900 in 2        → F9 00
//! E1 val 1 idx E300 in 1  → E3            begin the load
//! E1 val n idx E400 out n                 the image, 64 bytes a request,
//!                                         then its last byte on its own
//! E1 idx E500 in 2        → E5 10         the load is done
//! E1 idx FD00 in 3        → FD 32 31
//! E1 F2 tune, data [word >> 24, 0x02]     0x02 rather than stand-alone's 0x00
//! CAT CF…; IQ1;  tune again;  E1 E9 val 1 start
//! CAT IQ1; about every eight seconds while streaming, IQ0; on the way out
//! ```
//!
//! # The images are ELAD's
//!
//! They are not in this crate and must not be. They are read at run time out
//! of `ExtIOmc_ELAD_FDMDUO.dll`, the ELAD driver that SDR Console and FDM-SW2
//! install, found by scanning it for the Xilinx sync word and checked against
//! the SHA-256 of the exact bytes SDR Console uploaded. Two builds of that DLL
//! have the images at different offsets, which is why it is the hash that is
//! trusted and never a position.
//!
//! # Not while transmitting
//!
//! Reloading the FPGA reconfigures the radio, and its transmitter goes through
//! that FPGA as well. SAFETY INTERLOCK (docs/safety-interlocks.md, #10): no
//! load starts while the owner of this crate says the radio is transmitting,
//! and a load that sees a transmission begin stops between two requests. See
//! [`set_transmitting`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The rate the DUO streams at without a host-loaded image.
pub const STANDALONE_RATE_HZ: u32 = 192_000;

/// How often `IQ1;` is sent while a remote image streams. SDR Console sends it
/// every four to eight seconds; nothing says how long the radio waits for it,
/// so this sits inside the shortest gap seen.
pub const KEEPALIVE: Duration = Duration::from_secs(4);

/// Bytes per upload request.
pub const CHUNK: usize = 64;

/// The name of ELAD's driver DLL, which carries the images.
pub const DLL_NAME: &str = "ExtIOmc_ELAD_FDMDUO.dll";

/// An environment variable naming the DLL, for an install somewhere else.
pub const DLL_ENV: &str = "HRP_ELAD_DLL";

/// Where the DLL is looked for after [`DLL_ENV`]: SDR Console's folder, then
/// FDM-SW2's under both program folders.
const DLL_DIRS: &[&str] = &[
    r"C:\Program Files\SDR-Radio.com (V3)",
    r"C:\Program Files (x86)\ELAD\ELAD FDM-SW2",
    r"C:\Program Files\ELAD\ELAD FDM-SW2",
];

/// The Xilinx sync word as it sits in ELAD's images (bit-reversed bytes).
const SYNC: [u8; 4] = [0x55, 0x99, 0xAA, 0x66];

/// One remote image this crate can recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    pub rate_hz: u32,
    /// Bytes uploaded: sixteen `0xFF`, the image from its sync word, and one
    /// more `0xFF`.
    pub upload_len: usize,
    /// SHA-256 of those bytes, as SDR Console sent them.
    pub sha256: [u8; 32],
}

/// The images seen in the captures, the rates named by the bandwidth chosen in
/// SDR Console as each was loaded. 1536 kHz is in the same DLL but has not been
/// seen on the wire yet.
pub const IMAGES: [Image; 4] = [
    Image {
        rate_hz: 384_000,
        upload_len: 802_571,
        sha256: hex32("1d5efa829738e7a1f96c6e2cb1cb405403bbde6358cd98c86c2b973cf837f2c6"),
    },
    Image {
        rate_hz: 768_000,
        upload_len: 802_295,
        sha256: hex32("d9a7e0913cc6216e3b1877d3373ae8897204ab6c5498fdb645fbe68564b3525f"),
    },
    Image {
        rate_hz: 3_072_000,
        upload_len: 801_463,
        sha256: hex32("3a06429c44b6769ee25f29ab48656dccb25b669e16821eb8a4a4c644fa8918db"),
    },
    Image {
        rate_hz: 6_144_000,
        upload_len: 801_463,
        sha256: hex32("d37cdae628d361b85e1a954ca14d194490e797439931463041580746646972ed"),
    },
];

/// The image for `rate_hz`, if one is known.
pub fn image(rate_hz: u32) -> Option<Image> {
    IMAGES.iter().copied().find(|i| i.rate_hz == rate_hz)
}

/// Whether the DUO needs a remote image for `rate_hz`.
pub fn wanted(rate_hz: u32) -> bool {
    rate_hz > STANDALONE_RATE_HZ
}

static TRANSMITTING: AtomicBool = AtomicBool::new(false);

/// Tell this crate whether the radio is transmitting, as often as that is
/// known. A load does not start while it is, and stops if it starts.
pub fn set_transmitting(on: bool) {
    TRANSMITTING.store(on, Ordering::Relaxed);
}

pub fn transmitting() -> bool {
    TRANSMITTING.load(Ordering::Relaxed)
}

/// Whether this process has put the radio in remote mode, so a later open at
/// 192 kHz knows to send `IQ0;` first.
static WENT_REMOTE: AtomicBool = AtomicBool::new(false);

pub(crate) fn note_remote() {
    WENT_REMOTE.store(true, Ordering::Relaxed);
}

pub(crate) fn went_remote() -> bool {
    WENT_REMOTE.load(Ordering::Relaxed)
}

/// Images already pulled out of the DLL in this process.
static CACHE: Mutex<Vec<(u32, Arc<Vec<u8>>)>> = Mutex::new(Vec::new());

/// The upload for `rate_hz`, read from the installed DLL, or the sentence the
/// operator needs.
pub fn upload_for(rate_hz: u32) -> Result<Arc<Vec<u8>>, String> {
    let Some(img) = image(rate_hz) else {
        return Err(format!(
            "The FDM-DUO's {} kHz image is not known yet, so it stays at {} kHz. A USB capture \
             of SDR Console at that bandwidth would identify it.",
            rate_hz / 1000,
            STANDALONE_RATE_HZ / 1000
        ));
    };
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, u)) = cache.iter().find(|(r, _)| *r == rate_hz) {
        return Ok(Arc::clone(u));
    }
    let dlls = dll_paths();
    for path in &dlls {
        let Ok(bytes) = std::fs::read(path) else { continue };
        if let Some(u) = find_upload(&bytes, &img) {
            let u = Arc::new(u);
            cache.push((rate_hz, Arc::clone(&u)));
            return Ok(u);
        }
    }
    Err(if dlls.is_empty() {
        format!(
            "To stream wider than {} kHz the FDM-DUO needs ELAD's {DLL_NAME}, which SDR Console \
             or FDM-SW2 installs; neither was found, so it stays at {} kHz.",
            STANDALONE_RATE_HZ / 1000,
            STANDALONE_RATE_HZ / 1000
        )
    } else {
        format!(
            "{DLL_NAME} was found but has no {} kHz image this program recognises, so the \
             FDM-DUO stays at {} kHz.",
            rate_hz / 1000,
            STANDALONE_RATE_HZ / 1000
        )
    })
}

/// The DLLs that exist, in the order they are tried.
fn dll_paths() -> Vec<PathBuf> {
    let env = std::env::var_os(DLL_ENV).map(PathBuf::from);
    env.into_iter()
        .chain(DLL_DIRS.iter().map(|d| PathBuf::from(d).join(DLL_NAME)))
        .filter(|p| p.is_file())
        .collect()
}

/// The upload for `img`, if `dll` holds it.
///
/// Every sync word is a candidate; the one whose bytes hash right is the image.
pub fn find_upload(dll: &[u8], img: &Image) -> Option<Vec<u8>> {
    let body_len = img.upload_len.checked_sub(17)?;
    let mut from = 0;
    while let Some(at) = dll[from..].windows(4).position(|w| w == SYNC).map(|p| p + from) {
        from = at + 1;
        let Some(body) = dll.get(at..at + body_len) else { break };
        let mut u = Vec::with_capacity(img.upload_len);
        u.extend_from_slice(&[0xFF; 16]);
        u.extend_from_slice(body);
        u.push(0xFF);
        if sha256(&u) == img.sha256 {
            return Some(u);
        }
    }
    None
}

/// The upload as SDR Console splits it: 64-byte requests, a short one for the
/// rest, and the last byte alone.
pub fn chunks(upload: &[u8]) -> impl Iterator<Item = &[u8]> {
    let (body, last) = upload.split_at(upload.len().saturating_sub(1));
    body.chunks(CHUNK).chain(std::iter::once(last).filter(|l| !l.is_empty()))
}

const fn hex32(s: &str) -> [u8; 32] {
    const fn nib(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("not lower-case hex"),
        }
    }
    let b = s.as_bytes();
    assert!(b.len() == 64);
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = nib(b[2 * i]) << 4 | nib(b[2 * i + 1]);
        i += 1;
    }
    out
}

/// SHA-256, so checking an image needs no crate for one hash per open.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_sha_256() {
        assert_eq!(
            sha256(b""),
            hex32("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(
            sha256(b"abc"),
            hex32("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        // Two blocks, so the length lands in the second.
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            hex32("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
        );
    }

    /// A made-up image behind a decoy sync word: the decoy hashes wrong and is
    /// passed over, and the real one comes back framed the way it is sent.
    #[test]
    fn the_image_is_the_sync_word_whose_bytes_hash_right() {
        let body: Vec<u8> = SYNC.iter().copied().chain((0..300u32).map(|i| i as u8)).collect();
        let mut want = vec![0xFF; 16];
        want.extend_from_slice(&body);
        want.push(0xFF);
        let img = Image { rate_hz: 1, upload_len: want.len(), sha256: sha256(&want) };

        let mut dll = vec![0u8; 50];
        dll.extend_from_slice(&SYNC);
        dll.extend_from_slice(&[7u8; 400]);
        dll.extend_from_slice(&body);
        dll.extend_from_slice(&[9u8; 20]);
        assert_eq!(find_upload(&dll, &img), Some(want));

        // Cut short before the image ends: nothing, rather than a slice past
        // the end.
        let short = &dll[..dll.len() - 25];
        assert_eq!(find_upload(short, &img), None);
    }

    /// SDR Console's split of the 384 kHz image: 12,540 full requests, ten
    /// bytes, then the closing `0xFF` alone.
    #[test]
    fn the_upload_goes_in_sixty_four_byte_requests_and_its_last_byte_alone() {
        let u = vec![0u8; 802_571];
        let lens: Vec<usize> = chunks(&u).map(<[u8]>::len).collect();
        assert_eq!(lens.len(), 12_542);
        assert!(lens[..12_540].iter().all(|&l| l == 64));
        assert_eq!(&lens[12_540..], &[10, 1]);
        // The 768 kHz one ends 54 then 1.
        let u = vec![0u8; 802_295];
        let lens: Vec<usize> = chunks(&u).map(<[u8]>::len).collect();
        assert_eq!(lens.len(), 12_537);
        assert_eq!(&lens[12_535..], &[54, 1]);
    }

    #[test]
    fn only_rates_above_stand_alone_need_an_image_and_four_are_known() {
        assert!(!wanted(192_000));
        assert!(wanted(384_000));
        assert_eq!(image(384_000).map(|i| i.upload_len), Some(802_571));
        assert_eq!(image(768_000).map(|i| i.upload_len), Some(802_295));
        assert_eq!(image(3_072_000).map(|i| i.upload_len), Some(801_463));
        assert_eq!(image(6_144_000).map(|i| i.upload_len), Some(801_463));
        assert_eq!(image(1_536_000), None);
        let e = upload_for(1_536_000).unwrap_err();
        assert!(e.contains("1536 kHz") && e.contains("192 kHz"), "{e}");
    }
}
