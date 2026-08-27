use std::collections::HashMap;

use bytes::Bytes;
use ring::{
    aead::{self, Aad, LessSafeKey, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};

const MAGIC: &[u8; 8] = b"DSDOCENC";
const VERSION: u8 = 1;
const NONCE_LEN: usize = 12;
const MAX_KEY_ID_LEN: usize = 64;

pub struct DocumentCipher {
    primary_key_id: String,
    keys: HashMap<String, LessSafeKey>,
    allow_legacy_plaintext_reads: bool,
    rng: SystemRandom,
}

impl DocumentCipher {
    pub fn from_hex_keyring(
        primary_key_id: &str,
        keyring: &str,
        allow_legacy_plaintext_reads: bool,
    ) -> Result<Self, String> {
        validate_key_id(primary_key_id)?;
        let mut keys = HashMap::new();
        for entry in keyring
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let (key_id, encoded_key) = entry
                .split_once('=')
                .ok_or_else(|| "document encryption keyring entry is malformed".to_string())?;
            validate_key_id(key_id)?;
            let key_bytes = hex::decode(encoded_key)
                .map_err(|_| "document encryption key must be hexadecimal".to_string())?;
            if key_bytes.len() != 32 {
                return Err("document encryption key must be 32 bytes".to_string());
            }
            let key = UnboundKey::new(&aead::AES_256_GCM, &key_bytes)
                .map_err(|_| "document encryption key is invalid".to_string())?;
            if keys
                .insert(key_id.to_string(), LessSafeKey::new(key))
                .is_some()
            {
                return Err("document encryption key id is duplicated".to_string());
            }
        }
        if !keys.contains_key(primary_key_id) {
            return Err("document encryption primary key is missing from keyring".to_string());
        }
        Ok(Self {
            primary_key_id: primary_key_id.to_string(),
            keys,
            allow_legacy_plaintext_reads,
            rng: SystemRandom::new(),
        })
    }

    pub fn encrypt(&self, object_key: &str, plaintext: &[u8]) -> Result<Bytes, String> {
        let key = self
            .keys
            .get(&self.primary_key_id)
            .ok_or_else(|| "document encryption primary key unavailable".to_string())?;
        let mut nonce = [0_u8; NONCE_LEN];
        self.rng
            .fill(&mut nonce)
            .map_err(|_| "document encryption nonce generation failed".to_string())?;
        let header = header(&self.primary_key_id, nonce)?;
        let aad = aad(&header, object_key);
        let mut ciphertext = plaintext.to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad.as_slice()),
            &mut ciphertext,
        )
        .map_err(|_| "document encryption failed".to_string())?;
        let mut envelope = header;
        envelope.extend_from_slice(&ciphertext);
        Ok(Bytes::from(envelope))
    }

    pub fn decrypt(&self, object_key: &str, stored: &[u8]) -> Result<Bytes, String> {
        if !stored.starts_with(MAGIC) {
            return if self.allow_legacy_plaintext_reads {
                Ok(Bytes::copy_from_slice(stored))
            } else {
                Err("legacy unencrypted document read is disabled".to_string())
            };
        }
        let parsed = parse_header(stored)?;
        let key = self
            .keys
            .get(parsed.key_id)
            .ok_or_else(|| "document encryption key id is unavailable".to_string())?;
        let context = aad(&stored[..parsed.header_len], object_key);
        let mut ciphertext = stored[parsed.header_len..].to_vec();
        let plaintext = key
            .open_in_place(
                Nonce::assume_unique_for_key(parsed.nonce),
                Aad::from(context.as_slice()),
                &mut ciphertext,
            )
            .map_err(|_| "document authentication failed".to_string())?;
        Ok(Bytes::copy_from_slice(plaintext))
    }

    pub fn is_encrypted(stored: &[u8]) -> bool {
        stored.starts_with(MAGIC)
    }
}

struct ParsedHeader<'a> {
    key_id: &'a str,
    nonce: [u8; NONCE_LEN],
    header_len: usize,
}

fn validate_key_id(key_id: &str) -> Result<(), String> {
    if key_id.is_empty()
        || key_id.len() > MAX_KEY_ID_LEN
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("document encryption key id is invalid".to_string());
    }
    Ok(())
}

fn header(key_id: &str, nonce: [u8; NONCE_LEN]) -> Result<Vec<u8>, String> {
    validate_key_id(key_id)?;
    let mut header = Vec::with_capacity(MAGIC.len() + 2 + key_id.len() + NONCE_LEN);
    header.extend_from_slice(MAGIC);
    header.push(VERSION);
    header.push(key_id.len() as u8);
    header.extend_from_slice(key_id.as_bytes());
    header.extend_from_slice(&nonce);
    Ok(header)
}

fn parse_header(stored: &[u8]) -> Result<ParsedHeader<'_>, String> {
    if stored.len() < MAGIC.len() + 2 + NONCE_LEN || !stored.starts_with(MAGIC) {
        return Err("document encryption envelope is invalid".to_string());
    }
    if stored[MAGIC.len()] != VERSION {
        return Err("document encryption envelope version is unsupported".to_string());
    }
    let key_id_len = stored[MAGIC.len() + 1] as usize;
    if key_id_len == 0 || key_id_len > MAX_KEY_ID_LEN {
        return Err("document encryption envelope key id is invalid".to_string());
    }
    let key_id_start = MAGIC.len() + 2;
    let nonce_start = key_id_start + key_id_len;
    let header_len = nonce_start + NONCE_LEN;
    if stored.len() < header_len + aead::AES_256_GCM.tag_len() {
        return Err("document encryption envelope is truncated".to_string());
    }
    let key_id = std::str::from_utf8(&stored[key_id_start..nonce_start])
        .map_err(|_| "document encryption envelope key id is invalid".to_string())?;
    validate_key_id(key_id)?;
    let nonce = stored[nonce_start..header_len]
        .try_into()
        .map_err(|_| "document encryption envelope nonce is invalid".to_string())?;
    Ok(ParsedHeader {
        key_id,
        nonce,
        header_len,
    })
}

fn aad(header: &[u8], object_key: &str) -> Vec<u8> {
    let mut context = Vec::with_capacity(header.len() + object_key.len() + 1);
    context.extend_from_slice(header);
    context.push(0);
    context.extend_from_slice(object_key.as_bytes());
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn payment_evidence_is_encrypted_and_bound_to_its_object_key() {
        let cipher =
            DocumentCipher::from_hex_keyring("key-2026", &format!("key-2026={KEY}"), false)
                .unwrap();
        let encrypted = cipher
            .encrypt("payments/PAY-1/proof", b"bank proof")
            .unwrap();
        assert!(!encrypted.windows(10).any(|window| window == b"bank proof"));
        assert_eq!(
            cipher.decrypt("payments/PAY-1/proof", &encrypted).unwrap(),
            Bytes::from_static(b"bank proof")
        );
        assert!(cipher.decrypt("payments/PAY-2/proof", &encrypted).is_err());
    }

    #[test]
    fn legacy_plaintext_requires_explicit_migration_flag() {
        let strict =
            DocumentCipher::from_hex_keyring("key-2026", &format!("key-2026={KEY}"), false)
                .unwrap();
        assert!(strict.decrypt("proof", b"legacy").is_err());
        let migration =
            DocumentCipher::from_hex_keyring("key-2026", &format!("key-2026={KEY}"), true).unwrap();
        assert_eq!(
            migration.decrypt("proof", b"legacy").unwrap(),
            b"legacy"[..]
        );
    }
}
