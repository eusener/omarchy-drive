//! Hashes de conteúdo compatíveis com os provedores.

use std::io::Read;
use std::path::Path;

use base64::Engine as _;
use md5::Digest as _;

use crate::model::HashKind;

/// Implementação do QuickXorHash do OneDrive (largura 160 bits, shift 11).
/// Referência: documentação do Microsoft Graph ("QuickXorHash").
#[derive(Clone, Default)]
pub struct QuickXor {
    data: [u64; 3],
    shift_so_far: usize,
    length: u64,
}

impl QuickXor {
    const WIDTH: usize = 160;
    const SHIFT: usize = 11;
    const BITS_IN_LAST_CELL: usize = 32;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, buf: &[u8]) {
        let mut idx = self.shift_so_far / 64;
        let mut off = self.shift_so_far % 64;
        let iterations = buf.len().min(Self::WIDTH);
        for i in 0..iterations {
            let is_last = idx == self.data.len() - 1;
            let bits = if is_last { Self::BITS_IN_LAST_CELL } else { 64 };
            if off <= bits - 8 {
                let mut j = i;
                while j < buf.len() {
                    self.data[idx] ^= (buf[j] as u64) << off;
                    j += Self::WIDTH;
                }
            } else {
                let idx2 = if is_last { 0 } else { idx + 1 };
                let low = bits - off;
                let mut x: u8 = 0;
                let mut j = i;
                while j < buf.len() {
                    x ^= buf[j];
                    j += Self::WIDTH;
                }
                self.data[idx] ^= (x as u64) << off;
                self.data[idx2] ^= (x as u64) >> low;
            }
            off += Self::SHIFT;
            while off >= bits {
                idx = if is_last { 0 } else { idx + 1 };
                off -= bits;
            }
        }
        self.shift_so_far =
            (self.shift_so_far + Self::SHIFT * (buf.len() % Self::WIDTH)) % Self::WIDTH;
        self.length += buf.len() as u64;
    }

    pub fn finalize(&self) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[0..8].copy_from_slice(&self.data[0].to_le_bytes());
        out[8..16].copy_from_slice(&self.data[1].to_le_bytes());
        out[16..20].copy_from_slice(&self.data[2].to_le_bytes()[0..4]);
        let len = self.length.to_le_bytes();
        for (i, b) in len.iter().enumerate() {
            out[20 - 8 + i] ^= b;
        }
        out
    }

    pub fn finalize_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.finalize())
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

enum Hasher {
    Md5(md5::Md5),
    Sha256(sha2::Sha256),
    QuickXor(QuickXor),
}

impl Hasher {
    fn new(kind: HashKind) -> Self {
        match kind {
            HashKind::Md5 => Hasher::Md5(md5::Md5::new()),
            HashKind::Sha256 => Hasher::Sha256(sha2::Sha256::new()),
            HashKind::QuickXor => Hasher::QuickXor(QuickXor::new()),
        }
    }
    fn update(&mut self, b: &[u8]) {
        match self {
            Hasher::Md5(h) => h.update(b),
            Hasher::Sha256(h) => h.update(b),
            Hasher::QuickXor(h) => h.update(b),
        }
    }
    fn finish(self) -> String {
        match self {
            Hasher::Md5(h) => hex(&h.finalize()),
            Hasher::Sha256(h) => hex(&h.finalize()),
            Hasher::QuickXor(h) => h.finalize_base64(),
        }
    }
}

pub fn hash_bytes(kind: HashKind, data: &[u8]) -> String {
    let mut h = Hasher::new(kind);
    h.update(data);
    h.finish()
}

/// Calcula o hash de um arquivo em streaming (bloqueante: use em
/// `spawn_blocking`).
pub fn hash_file(kind: HashKind, path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Hasher::new(kind);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quickxor_empty() {
        assert_eq!(QuickXor::new().finalize_base64(), "AAAAAAAAAAAAAAAAAAAAAAAAAAA=");
    }

    #[test]
    fn quickxor_streaming_equals_one_shot() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i * 31 % 251) as u8).collect();
        let one = hash_bytes(HashKind::QuickXor, &data);
        for chunk in [1usize, 7, 159, 160, 161, 333, 4096] {
            let mut q = QuickXor::new();
            for c in data.chunks(chunk) {
                q.update(c);
            }
            assert_eq!(q.finalize_base64(), one, "chunk {chunk}");
        }
    }

    #[test]
    fn md5_known() {
        assert_eq!(hash_bytes(HashKind::Md5, b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }
}
