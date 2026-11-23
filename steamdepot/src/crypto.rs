use aes::Aes256;
use aes::cipher::{BlockCipherDecrypt, KeyInit};
use base64::Engine;
use cbc::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};

use crate::error::{Error, Result};

type Aes256CbcDec = cbc::Decryptor<Aes256>;

/// AES-256-ECB decrypt with PKCS7 unpadding.
pub fn aes_ecb_decrypt(key: &[u8], input: &[u8]) -> Result<Vec<u8>> {
    if key.len() != 32 {
        return Err(Error::Other(format!(
            "depot key must be 32 bytes, got {}",
            key.len()
        )));
    }

    if input.len() & 15 != 0 {
        return Err(Error::Other(format!(
            "encrypted data length {} not a multiple of 16",
            input.len()
        )));
    }

    let cipher =
        Aes256::new_from_slice(key).map_err(|e| Error::Other(format!("AES key init: {}", e)))?;

    let mut output = input.to_vec();
    for i in (0..output.len()).step_by(16) {
        let block: &mut aes::Block = (&mut output[i..i + 16])
            .try_into()
            .expect("16-byte AES block");
        cipher.decrypt_block(block);
    }

    // Remove PKCS7 padding
    if let Some(&pad_len) = output.last() {
        let pad_len = pad_len as usize;
        if pad_len > 0
            && pad_len <= 16
            && output.len() >= pad_len
            && output[output.len() - pad_len..]
                .iter()
                .all(|&b| b == pad_len as u8)
        {
            output.truncate(output.len() - pad_len);
        }
    }

    Ok(output)
}

/// AES-256-CBC decrypt with PKCS7 unpadding.
pub fn aes_cbc_decrypt(key: &[u8], iv: &[u8], input: &[u8]) -> Result<Vec<u8>> {
    if key.len() != 32 {
        return Err(Error::Other(format!(
            "AES-CBC key must be 32 bytes, got {}",
            key.len()
        )));
    }
    if iv.len() != 16 {
        return Err(Error::Other(format!(
            "AES-CBC IV must be 16 bytes, got {}",
            iv.len()
        )));
    }

    let mut buf = input.to_vec();
    let decryptor = Aes256CbcDec::new_from_slices(key, iv)
        .map_err(|e| Error::Other(format!("AES-CBC init: {}", e)))?;

    let plaintext = decryptor
        .decrypt_padded::<Pkcs7>(&mut buf)
        .map_err(|e| Error::Other(format!("AES-CBC decrypt: {}", e)))?;

    Ok(plaintext.to_vec())
}

/// Decrypt an encrypted manifest filename.
///
/// SteamKit2 `DepotManifest.cs` algorithm:
/// 1. Base64 decode the filename string
/// 2. ECB-decrypt the first 16 bytes to get the IV
/// 3. CBC-decrypt the remainder using that IV + PKCS7 padding
/// 4. Strip trailing null bytes
/// 5. Interpret as UTF-8
pub fn decrypt_filename(key: &[u8], encrypted_b64: &str) -> Result<String> {
    // Filenames may contain newlines/whitespace in the base64 encoding
    let cleaned: String = encrypted_b64
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let encrypted = base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .map_err(|e| Error::Other(format!("filename base64 decode: {}", e)))?;

    if encrypted.len() < 32 {
        return Err(Error::Other(format!(
            "encrypted filename too short ({} bytes, need at least 32)",
            encrypted.len()
        )));
    }

    // First 16 bytes: ECB-decrypt to get the IV
    let iv = aes_ecb_decrypt_block(key, &encrypted[..16])?;

    // Remainder: CBC-decrypt with that IV
    let plaintext = aes_cbc_decrypt(key, &iv, &encrypted[16..])?;

    // Strip trailing null bytes
    let end = plaintext
        .iter()
        .rposition(|&b| b != 0)
        .map(|i| i + 1)
        .unwrap_or(0);

    String::from_utf8(plaintext[..end].to_vec())
        .map_err(|e| Error::Other(format!("filename UTF-8: {}", e)))
}

/// SteamKit2-compatible symmetric decrypt.
///
/// Input format: first 16 bytes are the AES-ECB-encrypted IV, remainder is
/// AES-256-CBC encrypted with PKCS7 padding. This is used for chunk data
/// and encrypted filenames.
pub fn symmetric_decrypt(key: &[u8], input: &[u8]) -> Result<Vec<u8>> {
    if input.len() < 32 {
        return Err(Error::Other(format!(
            "encrypted data too short ({} bytes, need at least 32)",
            input.len()
        )));
    }

    let iv = aes_ecb_decrypt_block(key, &input[..16])?;
    aes_cbc_decrypt(key, &iv, &input[16..])
}

/// ECB-decrypt a single 16-byte block (no padding removal).
fn aes_ecb_decrypt_block(key: &[u8], block: &[u8]) -> Result<Vec<u8>> {
    let cipher =
        Aes256::new_from_slice(key).map_err(|e| Error::Other(format!("AES key init: {}", e)))?;

    let mut output = [0u8; 16];
    output.copy_from_slice(block);
    let block: &mut aes::Block = (&mut output[..]).try_into().expect("16-byte AES block");
    cipher.decrypt_block(block);
    Ok(output.to_vec())
}

#[cfg(test)]
mod tests {
    use aes::Aes256;
    use aes::cipher::{BlockCipherEncrypt, BlockModeEncrypt, KeyIvInit};

    use super::*;

    const KEY: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];
    const IV: [u8; 16] = [0x42; 16];

    fn ecb_encrypt_padded(plaintext: &[u8]) -> Vec<u8> {
        let cipher = Aes256::new_from_slice(&KEY).unwrap();
        let mut buf = plaintext.to_vec();
        let pad = 16 - (buf.len() % 16);
        buf.extend(std::iter::repeat_n(pad as u8, pad));
        for i in (0..buf.len()).step_by(16) {
            let block: &mut aes::Block =
                (&mut buf[i..i + 16]).try_into().expect("16-byte AES block");
            cipher.encrypt_block(block);
        }
        buf
    }

    fn ecb_encrypt_block(block: &[u8; 16]) -> [u8; 16] {
        let cipher = Aes256::new_from_slice(&KEY).unwrap();
        let mut out = *block;
        let block: &mut aes::Block = (&mut out[..]).try_into().expect("16-byte AES block");
        cipher.encrypt_block(block);
        out
    }

    fn cbc_encrypt(plaintext: &[u8], iv: &[u8; 16]) -> Vec<u8> {
        let enc = cbc::Encryptor::<Aes256>::new_from_slices(&KEY, iv).unwrap();
        let mut buf = vec![0u8; plaintext.len() + 16];
        buf[..plaintext.len()].copy_from_slice(plaintext);
        let len = enc
            .encrypt_padded::<aes::cipher::block_padding::Pkcs7>(&mut buf, plaintext.len())
            .unwrap()
            .len();
        buf.truncate(len);
        buf
    }

    fn steam_symmetric_encrypt(plaintext: &[u8]) -> Vec<u8> {
        let iv_ct = ecb_encrypt_block(&IV);
        let body = cbc_encrypt(plaintext, &IV);
        [iv_ct.as_slice(), body.as_slice()].concat()
    }

    #[test]
    fn aes256_ecb_decrypts_nist_vector() {
        // NIST FIPS-197 AES-256 single-block known answer.
        let ciphertext = hex("8ea2b7ca516745bfeafc49904b496089");
        let expected = hex("00112233445566778899aabbccddeeff");
        assert_eq!(aes_ecb_decrypt(&KEY, &ciphertext).unwrap(), expected);
    }

    #[test]
    fn aes256_ecb_strips_pkcs7_padding() {
        let ciphertext = ecb_encrypt_padded(b"hello steam");
        assert_eq!(aes_ecb_decrypt(&KEY, &ciphertext).unwrap(), b"hello steam");
    }

    #[test]
    fn aes256_cbc_roundtrip() {
        let plaintext = b"depot content";
        let ciphertext = cbc_encrypt(plaintext, &IV);
        assert_eq!(aes_cbc_decrypt(&KEY, &IV, &ciphertext).unwrap(), plaintext);
    }

    #[test]
    fn symmetric_decrypt_roundtrip() {
        let plaintext = b"chunk bytes that are not aligned";
        let encrypted = steam_symmetric_encrypt(plaintext);
        assert_eq!(symmetric_decrypt(&KEY, &encrypted).unwrap(), plaintext);
    }

    #[test]
    fn decrypt_filename_roundtrip() {
        let plaintext = b"materials/models/foo.vmt\0\0";
        let encrypted = steam_symmetric_encrypt(plaintext);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&encrypted);

        assert_eq!(
            decrypt_filename(&KEY, &b64).unwrap(),
            "materials/models/foo.vmt"
        );
    }

    #[test]
    fn decrypt_filename_ignores_whitespace_in_base64() {
        let plaintext = b"a/b.txt";
        let encrypted = steam_symmetric_encrypt(plaintext);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&encrypted);
        let wrapped = format!("{}\n  {}\n", &b64[..8], &b64[8..]);

        assert_eq!(decrypt_filename(&KEY, &wrapped).unwrap(), "a/b.txt");
    }

    #[test]
    fn rejects_wrong_key_or_iv_sizes() {
        assert!(aes_ecb_decrypt(&[0u8; 16], &[0u8; 16]).is_err());
        assert!(aes_cbc_decrypt(&KEY, &[0u8; 8], &[0u8; 16]).is_err());
        assert!(aes_cbc_decrypt(&KEY, &IV, &[0u8; 15]).is_err());
    }

    #[test]
    fn rejects_unpadded_or_truncated_data() {
        assert!(aes_ecb_decrypt(&KEY, &[0u8; 15]).is_err());
        assert!(symmetric_decrypt(&KEY, &[0u8; 31]).is_err());
        assert!(decrypt_filename(&KEY, "not base64!!").is_err());
    }

    fn hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }
}
