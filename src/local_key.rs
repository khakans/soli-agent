//! Local Ed25519 custody for the isolated signer. Transaction policy stays in live.rs.
use ed25519_dalek::{Signer, SigningKey};
use zeroize::Zeroizing;

pub struct LocalKey(SigningKey);
impl LocalKey {
    pub fn from_env() -> Result<Self, String> {
        let text = Zeroizing::new(
            std::env::var("SOLI_PRIVATE_KEY")
                .map_err(|_| "SOLI_PRIVATE_KEY is required in the signer environment")?,
        );
        let expected = std::env::var("SOLI_EXPECTED_WALLET")
            .map_err(|_| "SOLI_EXPECTED_WALLET is required")?;
        Self::decode(&text, &expected)
    }
    pub(crate) fn decode(text: &str, expected: &str) -> Result<Self, String> {
        let bytes = super::vault_admin::decode_solana_key(text).map_err(
            |_| "invalid Solana private key; use hex, base58 or JSON keypair, never a seed phrase",
        )?;
        let seed: &[u8; 32] = bytes[..32].try_into().map_err(|_| "invalid seed length")?;
        let key = SigningKey::from_bytes(seed);
        if bytes.len() == 64 && bytes[32..] != key.verifying_key().to_bytes() {
            return Err("keypair public half does not match seed".into());
        }
        let result = Self(key);
        if expected != result.wallet() {
            return Err("private key does not match SOLI_EXPECTED_WALLET".into());
        }
        Ok(result)
    }
    pub fn wallet(&self) -> String {
        bs58::encode(self.0.verifying_key().to_bytes()).into_string()
    }
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.0.sign(message).to_bytes().to_vec()
    }
    pub fn verify(&self) -> Result<(), String> {
        let challenge = format!("Soli local Ed25519 compatibility {}", uuid::Uuid::new_v4());
        let sig = solana_signature::Signature::try_from(self.sign(challenge.as_bytes()).as_slice())
            .map_err(|_| "invalid local signature")?;
        if !sig.verify(&self.0.verifying_key().to_bytes(), challenge.as_bytes()) {
            return Err("local Ed25519 compatibility failed".into());
        }
        Ok(())
    }
}
pub fn check() -> Result<(), String> {
    let key = LocalKey::from_env()?;
    key.verify()?;
    println!(
        "Verified local wallet {}; no transaction signed or sent",
        key.wallet()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_key_formats_wallet_binding_and_signature() {
        let seed = [19; 32];
        let key = SigningKey::from_bytes(&seed);
        let expected = bs58::encode(key.verifying_key().to_bytes()).into_string();
        let local = LocalKey::decode(&bs58::encode(seed).into_string(), &expected).unwrap();
        local.verify().unwrap();
        let sig = solana_signature::Signature::try_from(local.sign(b"message").as_slice()).unwrap();
        assert!(sig.verify(&key.verifying_key().to_bytes(), b"message"));
        assert!(!sig.verify(&key.verifying_key().to_bytes(), b"changed"));
        assert!(
            LocalKey::decode(
                &serde_json::to_string(&key.to_keypair_bytes().to_vec()).unwrap(),
                &expected
            )
            .is_ok()
        );
        assert!(LocalKey::decode(&bs58::encode(seed).into_string(), "wrong wallet").is_err());
        assert!(LocalKey::decode(&hex::encode(seed), &expected).is_ok());
        assert!(
            LocalKey::decode(
                &format!("0x{}", hex::encode(key.to_keypair_bytes())),
                &expected
            )
            .is_ok()
        );
        assert!(LocalKey::decode(&hex::encode(seed), "wrong wallet").is_err());
        assert!(
            LocalKey::decode(&serde_json::to_string(&vec![19; 64]).unwrap(), &expected).is_err()
        );
        assert!(LocalKey::decode("not a private key", &expected).is_err());
    }
}
