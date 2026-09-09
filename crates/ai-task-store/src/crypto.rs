//! 凭据的静态加密。
//!
//! SSH 私钥落库前必须加密。**「先明文存着，以后再加」是不可接受的**——
//! 数据库备份、慢查询日志、误配的只读账号，任何一个都会把它漏出去，
//! 而私钥泄漏等于目标机被拿下。
//!
//! KEK 从 `AI_TASK_ENCRYPTION_KEY` 读（64 位十六进制 = 32 字节）。没配时进程
//! 启动会**拒绝服务**，而不是退化成明文——静默降级正是这类问题的根源。
//! 接外部 KMS 是后续的事，接口形状不变。

use std::sync::OnceLock;

use chacha20poly1305::aead::{Aead, Generate, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

/// 当前 KEK 的版本号。轮换密钥时递增，老密文靠它找回对应的密钥。
const KEY_VERSION: i32 = 1;

static KEK: OnceLock<[u8; 32]> = OnceLock::new();

/// 装载 KEK。进程启动时调一次。
///
/// 返回 `Err` 时调用方应当**直接退出**，不要带着"凭据无法加密"的状态跑起来。
pub fn init(hex_key: &str) -> Result<(), String> {
    let bytes = decode_hex(hex_key)
        .ok_or_else(|| "AI_TASK_ENCRYPTION_KEY 必须是 64 位十六进制（32 字节）".to_string())?;
    KEK.set(bytes)
        .map_err(|_| "加密密钥已经装载过了".to_string())
}

/// 生成一个可用的密钥，给部署文档和首次启动提示用。
#[must_use]
pub fn generate_key_hex() -> String {
    // 直接取系统熵源，不经任何用户态 PRNG
    let key = Key::generate();
    key.iter().map(|b| format!("{b:02x}")).collect()
}

pub struct Sealed {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub key_version: i32,
}

/// 加密。KEK 没装载时 panic —— 那是启动流程的 bug，不该在运行时静默变成明文。
#[must_use]
pub fn seal(plaintext: &[u8]) -> Sealed {
    let key = KEK
        .get()
        .expect("加密密钥未装载：init() 必须在服务启动时调用");
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));

    // 每条密文一个随机 nonce。复用 nonce 会让 ChaCha20 的密钥流重复，
    // 两条密文异或就能还原明文。
    let nonce = Nonce::generate();

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .expect("ChaCha20Poly1305 加密不会失败");
    Sealed {
        ciphertext,
        nonce: nonce.to_vec(),
        key_version: KEY_VERSION,
    }
}

/// 解密。返回 `None` 表示密钥不对或密文被改过。
#[must_use]
pub fn open(ciphertext: &[u8], nonce: &[u8], key_version: i32) -> Option<Vec<u8>> {
    if key_version != KEY_VERSION {
        return None;
    }
    let key = KEK.get()?;
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    cipher
        .decrypt(&Nonce::try_from(nonce).ok()?, ciphertext)
        .ok()
}

fn decode_hex(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_key_is_the_right_shape() {
        let key = generate_key_hex();
        assert_eq!(key.len(), 64);
        assert!(decode_hex(&key).is_some());
        // 两次生成不能一样
        assert_ne!(key, generate_key_hex());
    }

    #[test]
    fn malformed_keys_are_rejected() {
        assert!(decode_hex("").is_none());
        assert!(decode_hex("abc").is_none());
        assert!(decode_hex(&"z".repeat(64)).is_none());
        assert!(decode_hex(&"a".repeat(63)).is_none());
    }

    #[test]
    fn sealing_round_trips_and_uses_a_fresh_nonce_each_time() {
        let _ = init(&generate_key_hex());
        let secret = b"-----BEGIN OPENSSH PRIVATE KEY-----";

        let a = seal(secret);
        let b = seal(secret);
        // nonce 复用会让密钥流重复，两条密文异或就能还原明文
        assert_ne!(a.nonce, b.nonce, "每条密文必须用新的 nonce");
        assert_ne!(a.ciphertext, b.ciphertext);

        assert_eq!(
            open(&a.ciphertext, &a.nonce, a.key_version).as_deref(),
            Some(secret.as_slice())
        );
    }

    #[test]
    fn tampering_with_the_ciphertext_fails_to_open() {
        let _ = init(&generate_key_hex());
        let sealed = seal(b"secret");
        let mut tampered = sealed.ciphertext.clone();
        tampered[0] ^= 0xff;
        // AEAD 的意义就在这里：改一个字节就解不开，而不是解出垃圾
        assert!(open(&tampered, &sealed.nonce, sealed.key_version).is_none());
    }

    #[test]
    fn an_unknown_key_version_refuses_to_decrypt() {
        let _ = init(&generate_key_hex());
        let sealed = seal(b"secret");
        assert!(open(&sealed.ciphertext, &sealed.nonce, 999).is_none());
    }
}
