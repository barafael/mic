//! Where the device secret is kept (D16): the system keyring, and nowhere else. Windows'
//! Credential Manager, macOS' Keychain, the Secret Service on Linux and the BSDs (GNOME Keyring,
//! KWallet), through `keyring-core` and a store per platform.
//!
//! The secret is made on first start: 32 bytes from the OS's random generator, which is what HKDF
//! wants as its input key material. No passphrase: it identifies a device, not a person, and
//! nothing else needs to make it again. Losing it only means new identities in every room.
//!
//! Stores block (the keyring is a D-Bus call or a system call away): call them off an async
//! runtime's threads (`spawn_blocking`).

use std::sync::{Mutex, PoisonError};

use derive_more::{Display, Error};
use zeroize::Zeroize;

use crate::keys::DeviceSecret;

/// Where a device secret is kept.
pub trait SecretStore: Send + Sync {
    /// The secret kept, if any.
    fn load(&self) -> Result<Option<DeviceSecret>, SecretError>;
    fn save(&self, secret: &DeviceSecret) -> Result<(), SecretError>;
}

#[derive(Debug, Display, Error)]
pub enum SecretError {
    /// No keyring to keep it in: on Linux, no Secret Service runs.
    #[display("no system keyring to keep the device secret in: {_0}")]
    NoKeyring(#[error(not(source))] String),
    #[display("the system keyring: {_0}")]
    Keyring(#[error(not(source))] String),
    /// Something else is kept under the device secret's name.
    #[display("the keyring's device secret isn't 32 bytes")]
    Malformed,
}

/// The device's secret from `store`, made and kept there if it has none yet.
pub fn device_secret(store: &dyn SecretStore) -> Result<DeviceSecret, SecretError> {
    if let Some(secret) = store.load()? {
        return Ok(secret);
    }
    let secret = DeviceSecret::random();
    store.save(&secret)?;
    Ok(secret)
}

/// The system keyring, under a service name: `mic` for the app's own secret.
pub struct Keyring {
    service: String,
}

/// The keyring entry's user name, under the service.
const ENTRY: &str = "device-secret";

impl Keyring {
    /// The app's: service `mic`.
    pub fn system() -> Result<Self, SecretError> {
        Self::named("mic")
    }

    /// Under another service name, as tests keep theirs apart from the app's.
    pub fn named(service: &str) -> Result<Self, SecretError> {
        store_ready()?;
        Ok(Self {
            service: service.to_owned(),
        })
    }

    fn entry(&self) -> Result<keyring_core::Entry, SecretError> {
        keyring_core::Entry::new(&self.service, ENTRY).map_err(keyring_error)
    }

    /// Removes the secret kept, if any: for tests, and for a "new identity" action.
    pub fn forget(&self) -> Result<(), SecretError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(error) => Err(keyring_error(error)),
        }
    }
}

impl SecretStore for Keyring {
    fn load(&self) -> Result<Option<DeviceSecret>, SecretError> {
        match self.entry()?.get_secret() {
            Ok(mut bytes) => {
                let secret = <[u8; 32]>::try_from(bytes.as_slice()).map(DeviceSecret::from_bytes);
                bytes.zeroize();
                secret.map(Some).map_err(|_| SecretError::Malformed)
            }
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(keyring_error(error)),
        }
    }

    fn save(&self, secret: &DeviceSecret) -> Result<(), SecretError> {
        self.entry()?
            .set_secret(secret.as_bytes())
            .map_err(keyring_error)
    }
}

fn keyring_error(error: keyring_core::Error) -> SecretError {
    match error {
        keyring_core::Error::NoDefaultStore | keyring_core::Error::NoStorageAccess(_) => {
            SecretError::NoKeyring(error.to_string())
        }
        other => SecretError::Keyring(other.to_string()),
    }
}

/// The platform's store, set as keyring-core's default once per process; its failure is kept and
/// said again on every later try.
fn store_ready() -> Result<(), SecretError> {
    static READY: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    READY
        .get_or_init(|| set_store().map_err(|error| error.to_string()))
        .clone()
        .map_err(SecretError::NoKeyring)
}

fn set_store() -> keyring_core::Result<()> {
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new()?;
    #[cfg(target_os = "windows")]
    let store = windows_native_keyring_store::Store::new()?;
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    let store = zbus_secret_service_keyring_store::Store::new()?;
    keyring_core::set_default_store(store);
    Ok(())
}

/// In memory, for tests and for headless participants (`mic-crowd`), which have no keyring of
/// their own each.
#[derive(Default)]
pub struct InMemory(Mutex<Option<DeviceSecret>>);

impl SecretStore for InMemory {
    fn load(&self) -> Result<Option<DeviceSecret>, SecretError> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }

    fn save(&self, secret: &DeviceSecret) -> Result<(), SecretError> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(secret.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_made_once_and_kept() {
        let store = InMemory::default();
        let first = device_secret(&store).unwrap();
        assert_eq!(device_secret(&store).unwrap(), first);
        assert_eq!(store.load().unwrap(), Some(first));
    }

    /// Against the system keyring, under a service of its own, removed again: only where
    /// `MIC_TEST_KEYRING=1` says a keyring is there to use (CI, and the box's clients).
    #[test]
    fn the_keyring_keeps_the_secret() {
        if std::env::var("MIC_TEST_KEYRING").as_deref() != Ok("1") {
            eprintln!("skipped: MIC_TEST_KEYRING isn't 1");
            return;
        }
        let service = format!(
            "mic-test-{}",
            crate::invite::RoomId::random().to_string().to_lowercase()
        );
        let keyring = Keyring::named(&service).unwrap();
        assert_eq!(keyring.load().unwrap(), None);
        let made = device_secret(&keyring).unwrap();
        assert_eq!(device_secret(&keyring).unwrap(), made);
        keyring.forget().unwrap();
        assert_eq!(keyring.load().unwrap(), None);
    }
}
