//! Authenticated encryption for tokens and PKCE verifiers. The envelope format remains
//! compatible with existing databases and binds ciphertext to account generation and use.

use super::{Error, Result};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};

pub(super) struct TokenCipher(Aes256Gcm);

impl TokenCipher {
    pub(super) fn new(key: &[u8; 32]) -> anyhow::Result<Self> {
        Aes256Gcm::new_from_slice(key)
            .map(Self)
            .map_err(|_| anyhow::anyhow!("invalid encryption key"))
    }

    pub(super) fn seal(
        &self,
        user_id: i64,
        generation: &str,
        kind: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).map_err(|_| Error::Internal)?;
        let aad = format!("napominalochka:yandex:v1:{user_id}:{generation}:{kind}");
        let encrypted = self
            .0
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| Error::Internal)?;
        let mut envelope = vec![1];
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&encrypted);
        Ok(envelope)
    }

    pub(super) fn open(
        &self,
        user_id: i64,
        generation: &str,
        kind: &str,
        envelope: &[u8],
    ) -> Result<Vec<u8>> {
        if envelope.len() < 29 || envelope[0] != 1 {
            return Err(Error::ReconnectRequired);
        }
        let aad = format!("napominalochka:yandex:v1:{user_id}:{generation}:{kind}");
        self.0
            .decrypt(
                Nonce::from_slice(&envelope[1..13]),
                Payload {
                    msg: &envelope[13..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| Error::ReconnectRequired)
    }
}
