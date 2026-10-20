//! openmls needs crypto, randomness and storage in one value. Crypto and
//! randomness come from libcrux; storage is whatever the app keeps its state
//! in (an encrypted SQLite database in Darkmatter, memory in tests).

use openmls_libcrux_crypto::CryptoProvider;
use openmls_traits::OpenMlsProvider;
use openmls_traits::storage::{CURRENT_VERSION, StorageProvider};

pub struct Provider<S> {
    crypto: CryptoProvider,
    storage: S,
}

impl<S: StorageProvider<CURRENT_VERSION>> Provider<S> {
    pub fn new(storage: S) -> crate::Result<Self> {
        let crypto = CryptoProvider::new().map_err(crate::mls)?;
        Ok(Provider { crypto, storage })
    }
}

impl<S: StorageProvider<CURRENT_VERSION>> OpenMlsProvider for Provider<S> {
    type CryptoProvider = CryptoProvider;
    type RandProvider = CryptoProvider;
    type StorageProvider = S;

    fn storage(&self) -> &S {
        &self.storage
    }

    fn crypto(&self) -> &CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &CryptoProvider {
        &self.crypto
    }
}
