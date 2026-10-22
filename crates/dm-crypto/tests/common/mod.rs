//! The group test harness: several devices, the server's checks, and a
//! directory of identity keys and device certificates, in one process.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;

use dm_crypto::device::{DeviceName, ServerKey};
use dm_crypto::ds::{Refusal, Server};
use dm_crypto::wire::Content;
use dm_crypto::{Device, DeviceCertificate, Directory, Error, Handshake, IdentityKey, Incoming, UserIdentity};
use openmls_memory_storage::MemoryStorage;

pub type Dev = Device<MemoryStorage>;

/// What the server publishes: identity keys and certificates.
#[derive(Default)]
pub struct Keys {
    pub identities: RefCell<HashMap<String, IdentityKey>>,
    pub certificates: RefCell<HashMap<(String, String), DeviceCertificate>>,
}

impl Directory for Keys {
    fn identity_key(&self, user_id: &str) -> Option<IdentityKey> {
        self.identities.borrow().get(user_id).copied()
    }

    fn certificate(&self, user_id: &str, device_id: &str) -> Option<DeviceCertificate> {
        self.certificates.borrow().get(&(user_id.into(), device_id.into())).cloned()
    }
}

pub struct World {
    pub keys: Keys,
    pub server: Server<MemoryStorage>,
    pub users: HashMap<String, UserIdentity>,
}

impl World {
    pub fn new() -> Self {
        World { keys: Keys::default(), server: Server::create(MemoryStorage::default()).unwrap(), users: HashMap::new() }
    }

    pub fn server_key(&self) -> ServerKey {
        self.server.key()
    }

    /// A device for `user`, certified by the user's identity key.
    pub fn device(&mut self, user: &str, device: &str) -> Dev {
        let identity = self.users.entry(user.into()).or_insert_with(UserIdentity::generate);
        self.keys.identities.borrow_mut().insert(user.into(), identity.public());
        let d = Device::create(user, device, MemoryStorage::default()).unwrap();
        let cert = identity.certify(user, device, d.signature_key(), 1);
        self.keys.certificates.borrow_mut().insert((user.into(), device.into()), cert);
        d
    }

    pub fn create(&self, owner: &Dev, channel: &str) {
        let info = owner.create_group(channel, &self.server_key()).unwrap();
        self.server.register(channel, &info, &name(owner), &self.keys).unwrap();
    }

    /// Sends a handshake to the server; on success, the sender applies it.
    pub fn send(&self, from: &Dev, channel: &str, h: &Handshake) -> Result<dm_crypto::ds::Applied, Refusal> {
        let applied = self.server.commit(channel, &name(from), &h.commit, &h.group_info, |_| true, &self.keys)?;
        from.confirm(channel).unwrap();
        Ok(applied)
    }

    /// Adds devices (from fresh key packages) and has them join.
    pub fn add(&self, by: &Dev, channel: &str, new: &[&Dev], others: &[&Dev]) {
        let packages: Vec<Vec<u8>> = new.iter().map(|d| d.key_package(false).unwrap()).collect();
        let h = by.add(channel, &packages, &self.keys).unwrap();
        self.send(by, channel, &h).unwrap();
        for d in new {
            assert_eq!(d.join(h.welcome.as_ref().unwrap(), &self.server_key(), &self.keys).unwrap(), channel);
        }
        for d in others {
            assert!(matches!(d.process(channel, &h.commit, &self.keys).unwrap(), Incoming::Commit { .. }));
        }
    }
}

pub fn name(d: &Dev) -> DeviceName {
    (d.user_id.clone(), d.device_id.clone())
}

pub fn post(text: &str) -> Content {
    Content::Post { message: text.into(), files: vec![], preview: None }
}

pub fn read(d: &Dev, channel: &str, ct: &dm_crypto::wire::DmE2ee, keys: &Keys) -> Result<(DeviceName, String), Error> {
    match d.process(channel, &ct.ciphertext()?, keys)? {
        Incoming::Message { from, content: Content::Post { message, .. }, .. } => Ok((from, message)),
        other => panic!("expected a message, got {other:?}"),
    }
}

