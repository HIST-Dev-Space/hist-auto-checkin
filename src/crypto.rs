use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::{RngCore, rngs::OsRng};

#[derive(Clone)]
pub struct Secrets(Aes256Gcm);

impl Secrets {
    pub fn new(key: &str) -> anyhow::Result<Self> {
        let bytes = STANDARD.decode(key)?;
        anyhow::ensure!(
            bytes.len() == 32,
            "CREDENTIAL_KEY_BASE64 must encode exactly 32 bytes"
        );
        Ok(Self(
            Aes256Gcm::new_from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid encryption key"))?,
        ))
    }

    // AAD binds the ciphertext to its owner, record ID, and purpose.
    pub fn encrypt(&self, plaintext: &str, scope: &str) -> anyhow::Result<String> {
        let mut nonce = [0; 12];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .0
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: scope.as_bytes(),
                },
            )
            .map_err(|_| anyhow::anyhow!("credential encryption failed"))?;
        let mut payload = nonce.to_vec();
        payload.extend(ciphertext);
        Ok(format!("v1:{}", STANDARD.encode(payload)))
    }

    pub fn decrypt(&self, ciphertext: &str, scope: &str) -> anyhow::Result<String> {
        let payload = STANDARD.decode(
            ciphertext
                .strip_prefix("v1:")
                .ok_or_else(|| anyhow::anyhow!("unknown ciphertext version"))?,
        )?;
        anyhow::ensure!(payload.len() >= 28, "invalid ciphertext");
        let plaintext = self
            .0
            .decrypt(
                Nonce::from_slice(&payload[..12]),
                Payload {
                    msg: &payload[12..],
                    aad: scope.as_bytes(),
                },
            )
            .map_err(|_| anyhow::anyhow!("credential decryption failed"))?;
        Ok(String::from_utf8(plaintext)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_cannot_be_swapped_or_tampered() {
        let secrets = Secrets::new(&STANDARD.encode([42; 32])).unwrap();
        let ciphertext = secrets.encrypt("cookie-secret", "user:1:school:2").unwrap();
        assert_eq!(
            secrets.decrypt(&ciphertext, "user:1:school:2").unwrap(),
            "cookie-secret"
        );
        assert!(secrets.decrypt(&ciphertext, "user:2:school:2").is_err());
        let mut bytes = STANDARD.decode(&ciphertext[3..]).unwrap();
        bytes[20] ^= 1;
        assert!(
            secrets
                .decrypt(&format!("v1:{}", STANDARD.encode(bytes)), "user:1:school:2")
                .is_err()
        );
        assert_ne!(
            ciphertext,
            secrets.encrypt("cookie-secret", "user:1:school:2").unwrap()
        );
    }
}
