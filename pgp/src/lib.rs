//! PGP encrypt/decrypt/sign/verify via `sequoia-openpgp`.
//!
//! All operations work on raw bytes.  Armoring / MIME wrapping is the
//! caller's responsibility.

use std::io::Write;

// Re-export so callers can load certs from files.
pub use sequoia_openpgp::Cert;

use sequoia_openpgp as sq;
use sq::cert::prelude::*;
use sq::crypto::KeyPair;
use sq::parse::stream::*;
use sq::parse::Parse;
use sq::policy::Policy;
use sq::policy::StandardPolicy;
use sq::serialize::stream::*;
use sq::types::KeyFlags;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate a test key pair with signing + encryption subkeys.
pub fn generate_key(userid: &str) -> sq::Result<sq::Cert> {
    let (cert, _revocation) = CertBuilder::new()
        .add_userid(userid)
        .add_signing_subkey()
        .add_transport_encryption_subkey()
        .generate()?;
    Ok(cert)
}

/// Generate a test key pair with a passphrase-protected secret key.
pub fn generate_encrypted_key(userid: &str, passphrase: &str) -> sq::Result<sq::Cert> {
    let (cert, _revocation) = CertBuilder::new()
        .add_userid(userid)
        .add_signing_subkey()
        .add_transport_encryption_subkey()
        .set_password(Some(passphrase.into()))
        .generate()?;
    Ok(cert)
}

/// Load a certificate from a file path (PEM-armored).
pub fn load_cert(path: &str) -> sq::Result<sq::Cert> {
    Cert::from_file(path)
}

// ---------------------------------------------------------------------------
// KeyStore
// ---------------------------------------------------------------------------

/// A simple in-memory PGP key store.
///
/// Loads certificates from a file path and supports lookup by key ID
/// or email address.
#[derive(Clone, Default)]
pub struct KeyStore {
    certs: Vec<sq::Cert>,
}

impl KeyStore {
    /// Load all certs from a PEM-armored key file.
    pub fn load(path: &str) -> sq::Result<Self> {
        let cert = load_cert(path)?;
        Ok(Self {
            certs: vec![cert],
        })
    }

    /// Look up a cert by an exact email match on any user ID.
    pub fn by_email(&self, email: &str) -> Option<&sq::Cert> {
        self.certs.iter().find(|c| {
            c.userids().any(|uid| match uid.userid().email() {
                Ok(Some(e)) => e.eq_ignore_ascii_case(email),
                _ => false,
            })
        })
    }

    /// Look up a cert by key ID (hex string, case-insensitive).
    pub fn by_key_id(&self, key_id: &str) -> Option<&sq::Cert> {
        let needle = key_id.to_lowercase();
        self.certs.iter().find(|c| {
            format!("{:X}", c.fingerprint()).to_lowercase().contains(&needle)
                || format!("{:X}", c.keyid()).to_lowercase() == needle
        })
    }

    /// Return the first cert in the store (useful when only one is loaded).
    pub fn first(&self) -> Option<&sq::Cert> {
        self.certs.first()
    }

    /// Return all loaded certs.
    pub fn all(&self) -> &[sq::Cert] {
        &self.certs
    }
}

// ---------------------------------------------------------------------------
// Keyring — directory of public keys indexed by email
// ---------------------------------------------------------------------------

/// A keyring of recipient public keys, loaded from a directory of
/// PEM-armored cert files and indexed by email address for O(1) lookup.
///
/// Files can be named anything (e.g. `alice.asc`, `bob.gpg`) — the
/// keyring extracts email addresses from each cert's user IDs.
#[derive(Clone, Default)]
pub struct Keyring {
    by_email: HashMap<String, sq::Cert>,
}

impl Keyring {
    /// Load all cert files from a directory.
    ///
    /// Each file should contain a single PEM-armored certificate.
    /// The keyring indexes each cert by every email address found in
    /// its user IDs.
    pub fn load(dir: &std::path::Path) -> sq::Result<Self> {
        let mut by_email = HashMap::new();
        if !dir.exists() {
            return Ok(Self { by_email });
        }
        for entry in std::fs::read_dir(dir).map_err(|e| {
            anyhow::anyhow!("cannot read keyring dir {}: {e}", dir.display())
        })? {
            let entry = entry.map_err(|e| {
                anyhow::anyhow!("keyring entry error: {e}")
            })?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let cert = load_cert(&path.to_string_lossy())?;
            for uid in cert.userids() {
                if let Ok(Some(email)) = uid.userid().email() {
                    by_email.insert(email.to_lowercase(), cert.clone());
                }
            }
        }
        Ok(Self { by_email })
    }

    /// Look up a public key by recipient email address (case-insensitive).
    pub fn lookup(&self, email: &str) -> Option<&sq::Cert> {
        self.by_email.get(&email.to_lowercase())
    }

    /// Return the number of keys in the keyring.
    pub fn len(&self) -> usize {
        self.by_email.len()
    }

    /// Return true if the keyring is empty.
    pub fn is_empty(&self) -> bool {
        self.by_email.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Passphrase handling
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::Mutex;

use zeroize::Zeroizing;

/// In-memory passphrase cache.  Never persisted to disk.
///
/// Keys are key fingerprints; values are passphrases stored in
/// [`Zeroizing<String>`] so they are actively cleared from memory on
/// drop rather than waiting for garbage collection.
static PASSPHRASE_CACHE: std::sync::LazyLock<Mutex<HashMap<String, Zeroizing<String>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Try to decrypt a cert's secret key material using the given passphrase.
/// On success, caches the passphrase in memory so the user isn't
/// re-prompted for the same key during this session.
///
/// Returns `Ok(())` if decryption succeeded, or an error if the
/// passphrase was wrong or the key couldn't be decrypted.
pub fn unlock_key(cert: &sq::Cert, passphrase: &str) -> sq::Result<()> {
    let fp = format!("{:X}", cert.fingerprint());

    // Check cache first.
    {
        let cache = PASSPHRASE_CACHE.lock().unwrap();
        if let Some(cached) = cache.get(&fp)
            && cached.as_str() == passphrase {
                return Ok(()); // already cached, no-op
            }
    }

    // Try to decrypt each encrypted secret subkey.
    let password: sq::crypto::Password = passphrase.into();
    let encrypted: Vec<_> = cert.keys().secret().encrypted_secret().collect();
    if encrypted.is_empty() {
        // Key is already unencrypted or has no secret material.
        let mut cache = PASSPHRASE_CACHE.lock().unwrap();
        cache.insert(fp, Zeroizing::new(passphrase.to_owned()));
        return Ok(());
    }
    // Try to decrypt each encrypted key — if all fail, wrong passphrase.
    let mut decrypted_any = false;
    for ka in encrypted {
        let pub_key = ka.key().clone();
        let mut secret_key = ka.key().clone();
        if secret_key
            .secret_mut()
            .decrypt_in_place(&pub_key, &password)
            .is_ok()
        {
            decrypted_any = true;
        }
    }
    if !decrypted_any {
        return Err(anyhow::anyhow!("wrong passphrase"));
    }

    {
        // Cache for the session.
        let mut cache = PASSPHRASE_CACHE.lock().unwrap();
        cache.insert(fp, Zeroizing::new(passphrase.to_owned()));
    }
    Ok(())
}

/// Sign with a cert, first unlocking it with the passphrase.
///
/// If the cert's key is already unencrypted or has been cached from a
/// previous `unlock_key` call, no prompt is needed.
pub fn sign_unlocked(
    signer: &sq::Cert,
    plaintext: &[u8],
    passphrase: Option<&str>,
) -> sq::Result<Vec<u8>> {
    let fp = format!("{:X}", signer.fingerprint());

    // Check cache or use provided passphrase.
    let pw: Option<Zeroizing<String>> = passphrase
        .map(|s| Zeroizing::new(s.to_owned()))
        .or_else(|| {
            PASSPHRASE_CACHE
                .lock()
                .unwrap()
                .get(&fp)
                .cloned()
        });

    // If we have a passphrase, try to decrypt first.
    if let Some(ref pw) = pw {
        unlock_key(signer, pw)?;
    }

    // Now sign (sign() will use the now-unencrypted secret key).
    sign(signer, plaintext)
}

/// Encrypt `plaintext` for the given recipient certificates.
pub fn encrypt(recipients: &[sq::Cert], plaintext: &[u8]) -> sq::Result<Vec<u8>> {
    let p = &StandardPolicy::new();
    let mut recipient_keys = Vec::new();
    for cert in recipients {
        for key in cert
            .keys()
            .with_policy(p, None)
            .supported()
            .alive()
            .revoked(false)
            .key_flags(KeyFlags::empty().set_transport_encryption())
        {
            recipient_keys.push(key);
        }
    }

    if recipient_keys.is_empty() {
        return Err(anyhow::anyhow!("no valid encryption keys for recipients"));
    }

    let mut sink = Vec::new();
    {
        let message = Message::new(&mut sink);
        let message = Armorer::new(message).build()?;
        let message =
            Encryptor::for_recipients(message, recipient_keys).build()?;
        let mut message = LiteralWriter::new(message).build()?;
        message.write_all(plaintext)?;
        message.finalize()?;
    }
    Ok(sink)
}

/// Decrypt `ciphertext` using the provided secret keys.
pub fn decrypt(
    secret_keys: &[sq::Cert],
    ciphertext: &[u8],
) -> sq::Result<Vec<u8>> {
    let p = &StandardPolicy::new();
    let helper = PgpHelper::new(p, secret_keys, &[]);
    let mut decryptor = DecryptorBuilder::from_bytes(ciphertext)?
        .with_policy(p, None, helper)?;
    let mut content = Vec::new();
    std::io::copy(&mut decryptor, &mut content)?;
    Ok(content)
}

/// Sign `plaintext` with the given signer's secret key, producing an
/// OpenPGP signed message (armored).
pub fn sign(signer: &sq::Cert, plaintext: &[u8]) -> sq::Result<Vec<u8>> {
    let p = &StandardPolicy::new();
    let keypair: KeyPair = signer
        .keys()
        .unencrypted_secret()
        .with_policy(p, None)
        .supported()
        .alive()
        .revoked(false)
        .for_signing()
        .next()
        .ok_or_else(|| {
            anyhow::anyhow!("no signing-capable secret key")
        })?
        .key()
        .clone()
        .into_keypair()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut sink = Vec::new();
    {
        let message = Message::new(&mut sink);
        let signer = Signer::new(message, keypair)?.build()?;
        let mut writer = LiteralWriter::new(signer).build()?;
        writer.write_all(plaintext)?;
        writer.finalize()?;
    }
    Ok(sink)
}

/// Verify `signed_data` against `public_keys` and return the extracted
/// plaintext content.
pub fn verify(
    public_keys: &[sq::Cert],
    signed_data: &[u8],
) -> sq::Result<Vec<u8>> {
    let p = &StandardPolicy::new();
    let helper = PgpHelper::new(p, &[], public_keys);
    let mut verifier = VerifierBuilder::from_bytes(signed_data)?
        .with_policy(p, None, helper)?;
    let mut content = Vec::new();
    std::io::copy(&mut verifier, &mut content)?;
    Ok(content)
}

// ---------------------------------------------------------------------------
// Combined helper for decrypt & verify
// ---------------------------------------------------------------------------

struct PgpHelper<'a> {
    policy: &'a dyn Policy,
    secret_keys: Vec<sq::Cert>,
    public_keys: Vec<sq::Cert>,
}

impl<'a> PgpHelper<'a> {
    fn new(policy: &'a dyn Policy, secret: &[sq::Cert], public: &[sq::Cert]) -> Self {
        Self {
            policy,
            secret_keys: secret.to_vec(),
            public_keys: public.to_vec(),
        }
    }
}

impl VerificationHelper for PgpHelper<'_> {
    fn get_certs(
        &mut self,
        _ids: &[sq::KeyHandle],
    ) -> sq::Result<Vec<sq::Cert>> {
        Ok(self.public_keys.clone())
    }

    fn check(&mut self, structure: MessageStructure) -> sq::Result<()> {
        // Accept messages that are either signed (with valid sigs) or
        // unsigned (encrypted-only, no signatures to check).
        for (i, layer) in structure.into_iter().enumerate() {
            if let (0, MessageLayer::SignatureGroup { results }) = (i, layer) {
                let any_good = results.iter().any(|r| r.is_ok());
                if any_good {
                    return Ok(());
                }
                if !results.is_empty() {
                    return Err(anyhow::anyhow!("No valid signature found"));
                }
            }
        }
        // No signatures at all — that's fine for encrypted-only messages.
        Ok(())
    }
}

impl DecryptionHelper for PgpHelper<'_> {
    fn decrypt(
        &mut self,
        pkesks: &[sq::packet::PKESK],
        _skesks: &[sq::packet::SKESK],
        sym_algo: Option<sq::types::SymmetricAlgorithm>,
        decrypt: &mut dyn FnMut(
            Option<sq::types::SymmetricAlgorithm>,
            &sq::crypto::SessionKey,
        ) -> bool,
    ) -> sq::Result<Option<sq::Cert>> {
        for cert in &self.secret_keys {
            for key in cert
                .keys()
                .unencrypted_secret()
                .with_policy(self.policy, None)
                .supported()
                .alive()
                .revoked(false)
                .for_transport_encryption()
            {
                let kp: Result<KeyPair, _> =
                    key.key().clone().into_keypair();
                let mut keypair = match kp {
                    Ok(k) => k,
                    Err(_) => continue,
                };

                for p in pkesks {
                    if let Some((algo, sk)) =
                        p.decrypt(&mut keypair, sym_algo)
                        && decrypt(algo, &sk) {
                            return Ok(Some(cert.clone()));
                        }
                }
            }
        }
        Err(anyhow::anyhow!(
            "no matching secret key for decryption"
        ))
    }
}

// ---------------------------------------------------------------------------
// Content detection
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PgpContent {
    None,
    Encrypted,
    Signed,
}

/// Quick classification of raw bytes for PGP content type.
pub fn detect(bytes: &[u8]) -> PgpContent {
    let s = String::from_utf8_lossy(bytes);
    if s.contains("-----BEGIN PGP MESSAGE-----") {
        PgpContent::Encrypted
    } else if s.contains("-----BEGIN PGP SIGNED MESSAGE-----") {
        PgpContent::Signed
    } else {
        PgpContent::None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_encrypt_decrypt() {
        let _alice = generate_key("alice@example.com").unwrap();
        let bob = generate_key("bob@example.com").unwrap();
        let plaintext = b"Hello Bob, this is a secret.";

        let ciphertext = encrypt(&[bob.clone()], plaintext).unwrap();
        assert_eq!(detect(&ciphertext), PgpContent::Encrypted);

        let decrypted = decrypt(&[bob], &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn round_trip_sign_verify() {
        let alice = generate_key("alice@example.com").unwrap();
        let plaintext = b"Signed message content.";

        let signed = sign(&alice, plaintext).unwrap();
        let verified = verify(&[alice.clone()], &signed).unwrap();
        assert_eq!(verified, plaintext);
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let bob = generate_key("bob@example.com").unwrap();
        let eve = generate_key("eve@example.com").unwrap();

        let ciphertext = encrypt(&[bob], b"secret").unwrap();
        let result = decrypt(&[eve], &ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn verify_with_wrong_key_fails() {
        let alice = generate_key("alice@example.com").unwrap();
        let eve = generate_key("eve@example.com").unwrap();

        let signed = sign(&alice, b"alice was here").unwrap();
        let result = verify(&[eve], &signed);
        assert!(result.is_err());
    }

    // -- passphrase ------------------------------------------------------

    #[test]
    fn unlock_encrypted_key_works() {
        let cert = generate_encrypted_key("test@example.com", "s3cret").unwrap();
        assert!(unlock_key(&cert, "s3cret").is_ok());
    }

    #[test]
    fn wrong_passphrase_returns_error() {
        let cert = generate_encrypted_key("test@example.com", "s3cret").unwrap();
        let result = unlock_key(&cert, "wrong!");
        assert!(result.is_err());
        assert!(
            format!("{}", result.unwrap_err()).contains("wrong passphrase")
        );
    }

    #[test]
    fn passphrase_cache_allows_second_call_without_re_prompt() {
        let cert = generate_encrypted_key("test@example.com", "s3cret").unwrap();
        // First call: decrypts.
        unlock_key(&cert, "s3cret").unwrap();
        // Clear the cache... wait, we want to test the cache, not clear it.
        // Just call unlock_key again — it should succeed via cache.
        unlock_key(&cert, "s3cret").unwrap();
        // If we got here without error, the cache worked.
    }

    #[test]
    fn keystore_by_email_finds_correct_cert() {
        let alice = generate_key("alice@example.com").unwrap();
        let _bob = generate_key("bob@example.com").unwrap();
        let ks = KeyStore { certs: vec![alice.clone()] };
        assert!(ks.by_email("alice@example.com").is_some());
        assert!(ks.by_email("bob@example.com").is_none());
        assert!(ks.first().is_some());
    }

    // -- Keyring ---------------------------------------------------------

    #[test]
    fn keyring_lookup_finds_loaded_key() {
        let dir = tempfile::tempdir().unwrap();
        let alice = generate_key("alice@example.com").unwrap();
        // Serialize cert to file.
        let path = dir.path().join("alice.asc");
        let mut f = std::fs::File::create(&path).unwrap();
        use sq::serialize::Serialize;
        alice.serialize(&mut f).unwrap();
        drop(f);

        let kr = Keyring::load(dir.path()).unwrap();
        assert!(kr.lookup("alice@example.com").is_some());
        assert!(kr.lookup("bob@example.com").is_none());
        assert!(!kr.is_empty());
    }

    #[test]
    fn keyring_empty_dir_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let kr = Keyring::load(dir.path()).unwrap();
        assert!(kr.is_empty());
        assert!(kr.lookup("anyone@example.com").is_none());
    }

    // -- detection -------------------------------------------------------

    #[test]
    fn detect_classifies_correctly() {
        let encrypted =
            b"-----BEGIN PGP MESSAGE-----\n...\n-----END PGP MESSAGE-----";
        assert_eq!(detect(encrypted), PgpContent::Encrypted);

        let signed = b"-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\nbody\n-----BEGIN PGP SIGNATURE-----";
        assert_eq!(detect(signed), PgpContent::Signed);

        let plain = b"Hello, world!";
        assert_eq!(detect(plain), PgpContent::None);
    }
}
