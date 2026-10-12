//! 备份加密：Argon2id 从口令派生密钥，AES-256-GCM 加密。
//!
//! 文件格式：`THBK1` (5) | salt (16) | nonce (12) | ciphertext+tag。口令丢失则无法恢复。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};

const MAGIC: &[u8; 5] = b"THBK1";

fn derive(passphrase: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let mut key = [0u8; 32];
    argon2::Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| format!("derive key: {e}"))?;
    Ok(key)
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom(&mut b);
    b
}

/// 系统随机数（/dev/urandom）。
fn getrandom(buf: &mut [u8]) {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buf))
        .expect("read /dev/urandom");
}

pub fn encrypt(passphrase: &str, plain: &[u8]) -> Result<Vec<u8>, String> {
    if passphrase.chars().count() < 12 {
        return Err("backup passphrase must be at least 12 characters".into());
    }
    let salt: [u8; 16] = random();
    let nonce: [u8; 12] = random();
    let key = derive(passphrase, &salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let ct = cipher.encrypt(Nonce::from_slice(&nonce), plain).map_err(|_| "encrypt failed".to_owned())?;
    let mut out = Vec::with_capacity(MAGIC.len() + 28 + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn decrypt(passphrase: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 33 || &data[..5] != MAGIC {
        return Err("not a thought backup (bad header)".into());
    }
    let (salt, nonce, ct) = (&data[5..21], &data[21..33], &data[33..]);
    let key = derive(passphrase, salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| "wrong passphrase or corrupted backup".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_rejects_wrong_passphrase() {
        let data = b"\xe6\x80\x9d\xe6\x83\xb3 memory".to_vec();
        let enc = encrypt("correct horse battery", &data).unwrap();
        assert_ne!(&enc[33..], &data[..]);
        assert_eq!(decrypt("correct horse battery", &enc).unwrap(), data);
        assert!(decrypt("wrong horse battery", &enc).is_err());
        let mut tampered = enc.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(decrypt("correct horse battery", &tampered).is_err());
        assert!(encrypt("short", &data).is_err());
        // 同样内容每次密文不同（随机 salt / nonce）
        assert_ne!(encrypt("correct horse battery", &data).unwrap(), enc);
    }
}
