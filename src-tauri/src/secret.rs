//! 本地凭据加密：机器绑定密钥（HKDF-SHA256 派生 + AES-256-GCM 加密）
//!
//! 目的：让 config.json 里不落明文凭据 —— 配置文件被读取或拷走后，
//! 在另一台机器上无法解密（密钥由本机机器码派生，且不落盘）。
//! 安全边界：只防「文件泄露」，不防「本机上的恶意进程」（它们同样能读到机器码）。
//!
//! 存储格式：`enc:v1:` + base64(nonce(12) ‖ ciphertext ‖ tag(16))
//! 兼容规则：空串不加密；无 `enc:v1:` 前缀的值按历史明文原样读取。

use std::sync::OnceLock;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::hkdf::{KeyType, Salt, HKDF_SHA256};
use ring::rand::{SecureRandom, SystemRandom};

const PREFIX: &str = "enc:v1:";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// 读取本机机器码；极端情况下退回 hostname（两者都失败时用固定串兜底，保证进程内可用）
fn machine_id() -> String {
    if let Ok(id) = machine_uid::get() {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "desktools-fallback".to_string())
}

struct OkmLen;

impl KeyType for OkmLen {
    fn len(&self) -> usize {
        KEY_LEN
    }
}

/// 派生并缓存 AES-256-GCM 密钥（机器绑定，进程生命周期内不变）
fn cipher() -> Result<&'static LessSafeKey, String> {
    static CIPHER: OnceLock<Result<LessSafeKey, String>> = OnceLock::new();
    CIPHER
        .get_or_init(|| {
            // HKDF-SHA256(ikm = 机器码, salt = 应用标识, info = 用途) -> 32 字节密钥
            let salt = Salt::new(HKDF_SHA256, b"com.hyy.desktools");
            let prk = salt.extract(machine_id().as_bytes());
            let okm = prk
                .expand(&[b"minio-credentials"], OkmLen)
                .map_err(|_| "密钥派生失败".to_string())?;
            let mut key = [0u8; KEY_LEN];
            okm.fill(&mut key).map_err(|_| "密钥填充失败".to_string())?;
            let unbound =
                UnboundKey::new(&AES_256_GCM, &key).map_err(|_| "密钥初始化失败".to_string())?;
            Ok(LessSafeKey::new(unbound))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// 加密一个字段值（空串原样返回，避免给「未填写」的字段生成密文）
pub fn encrypt(plain: &str) -> Result<String, String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    let key = cipher()?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| "随机数生成失败".to_string())?;
    let mut buf = plain.as_bytes().to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce_bytes),
        Aad::empty(),
        &mut buf,
    )
    .map_err(|_| "加密失败".to_string())?;
    let mut out = Vec::with_capacity(NONCE_LEN + buf.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&buf);
    Ok(format!("{PREFIX}{}", BASE64.encode(out)))
}

/// 解密一个字段值：无前缀按历史明文透传；解密失败返回 Err
pub fn decrypt(stored: &str) -> Result<String, String> {
    if stored.is_empty() {
        return Ok(String::new());
    }
    let Some(encoded) = stored.strip_prefix(PREFIX) else {
        return Ok(stored.to_string());
    };
    let key = cipher()?;
    let raw = BASE64
        .decode(encoded)
        .map_err(|_| "密文不是有效的 Base64".to_string())?;
    // nonce(12) + ciphertext + tag(16)，AES-256-GCM 的 tag 固定 16 字节
    if raw.len() < NONCE_LEN + 16 {
        return Err("密文长度不足".to_string());
    }
    let (nonce_bytes, ciphertext) = raw.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce_bytes
        .try_into()
        .map_err(|_| "nonce 长度错误".to_string())?;
    let mut buf = ciphertext.to_vec();
    let plain = key
        .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf)
        .map_err(|_| "解密失败（配置可能来自其他机器或已被修改）".to_string())?;
    String::from_utf8(plain.to_vec()).map_err(|_| "解密结果不是有效文本".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let plain = "AKIA1234567890/+= 中文也支持";
        let enc = encrypt(plain).unwrap();
        assert!(enc.starts_with(PREFIX));
        assert_ne!(enc, plain);
        assert_eq!(decrypt(&enc).unwrap(), plain);
    }

    #[test]
    fn same_plaintext_produces_different_ciphertexts() {
        // 随机 nonce：同样的明文两次加密结果不同，读回来一致
        let a = encrypt("secret").unwrap();
        let b = encrypt("secret").unwrap();
        assert_ne!(a, b);
        assert_eq!(decrypt(&a).unwrap(), decrypt(&b).unwrap());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let enc = encrypt("secret").unwrap();
        let raw = BASE64.decode(enc.strip_prefix(PREFIX).unwrap()).unwrap();
        let mut tampered = raw.clone();
        // 翻转密文中间的 1 个 bit：GCM 校验必须失败
        let mid = tampered.len() / 2;
        tampered[mid] ^= 0x01;
        let tampered = format!("{PREFIX}{}", BASE64.encode(tampered));
        assert!(decrypt(&tampered).is_err());
    }

    #[test]
    fn legacy_plaintext_passes_through() {
        // 历史明文（或手工编辑过的配置）原样读取
        assert_eq!(decrypt("plain-old-value").unwrap(), "plain-old-value");
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(encrypt("").unwrap(), "");
        assert_eq!(decrypt("").unwrap(), "");
    }

    #[test]
    fn malformed_ciphertext_fails() {
        assert!(decrypt("enc:v1:AAAA").is_err());
        assert!(decrypt("enc:v1:not-base64!!").is_err());
    }
}
