//! axiond as the MLS delivery service, end to end: devices made with
//! `dm-crypto`, talking to a real axiond through `mm-api`, the way
//! Darkmatter will.

use std::collections::HashMap;

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_crypto::device::ServerKey;
use dm_crypto::wire::{Aad, Content};
use dm_crypto::{Device, DeviceCertificate, Directory, IdentityKey, Incoming, UserIdentity};
use mm_api::Client;
use mm_api::models::NewHandshake;
use openmls_memory_storage::MemoryStorage;

const PASSWORD: &str = "correct horse battery";

struct Server {
    url: String,
    app: App,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(Config { data_dir: dir.path().into(), ..Config::default() }).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_user(&app, user("bob"), false).await.unwrap();
    admin::add_user(&app, user("carol"), false).await.unwrap();
    admin::add_channel(&app, "home", "secret", "Secret").await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.clone().serve(listener));
    Server { url, app, _dir: dir }
}

/// One installation of Darkmatter: signed in as a device, with its keys.
struct Phone {
    api: Client,
    device: Device<MemoryStorage>,
    user_id: String,
    server_key: ServerKey,
}

impl Phone {
    /// Signs in as `device_id`, publishes the identity key (the first time)
    /// and this device's certificate, and uploads key packages.
    async fn new(s: &Server, name: &str, device_id: &str, identity: &UserIdentity) -> Phone {
        let mut api = Client::new(&s.url).unwrap().device_id(device_id);
        let me = api.login(name, PASSWORD, None).await.unwrap();
        let config = api.axiond().await.unwrap().expect("axiond");
        assert!(config.enabled);
        let server_key = ServerKey { signature_key: B64.decode(config.external_sender).unwrap() };
        api.put_identity(&B64.encode(identity.public().0)).await.unwrap();
        let device = Device::create(&me.id, device_id, MemoryStorage::default()).unwrap();
        let cert = identity.certify(&me.id, device_id, device.signature_key(), 1);
        api.publish_device(&serde_json::to_value(&cert).unwrap()).await.unwrap();
        let mut packages: Vec<String> = (0..3).map(|_| B64.encode(device.key_package(false).unwrap())).collect();
        packages.push(B64.encode(device.key_package(true).unwrap()));
        api.upload_key_packages(&packages).await.unwrap();
        Phone { api, device, user_id: me.id, server_key }
    }

    /// The keys of everyone in a channel, as the server publishes them.
    async fn directory(&self, users: &[&str]) -> Keys {
        let mut keys = Keys::default();
        for user in users {
            let id = self.api.identity(user).await.unwrap();
            if let Ok(key) = B64.decode(&id.identity_key) {
                keys.identities.insert(user.to_string(), IdentityKey::from_slice(&key).unwrap());
            }
            for cert in id.devices {
                let cert: DeviceCertificate = serde_json::from_value(cert).unwrap();
                keys.certificates.insert((cert.user_id.clone(), cert.device_id.clone()), cert);
            }
        }
        keys
    }

    /// Fetches and applies the handshakes after `seq`; returns the new seq.
    async fn catch_up(&self, channel: &str, seq: i64, keys: &Keys) -> (i64, Vec<Incoming>) {
        let mut out = Vec::new();
        let mut last = seq;
        for h in self.api.handshakes(channel, seq).await.unwrap() {
            out.push(self.device.process(channel, &B64.decode(&h.data).unwrap(), keys).unwrap());
            last = h.seq;
        }
        (last, out)
    }

    async fn send(&self, channel: &str, h: &dm_crypto::Handshake) -> Result<i64, mm_api::Error> {
        let body = NewHandshake {
            commit: B64.encode(&h.commit),
            group_info: B64.encode(&h.group_info),
            welcome: h.welcome.as_ref().map(|w| B64.encode(w)).unwrap_or_default(),
        };
        let entry = self.api.post_handshake(channel, &body).await?;
        self.device.confirm(channel).unwrap();
        Ok(entry.seq)
    }

    /// Takes in the Welcomes waiting for this device.
    async fn take_welcomes(&self, keys: &Keys) -> Vec<(String, i64)> {
        let mut joined = Vec::new();
        for w in self.api.welcomes().await.unwrap() {
            let channel = self.device.join(&B64.decode(&w.welcome).unwrap(), &self.server_key, keys).unwrap();
            assert_eq!(channel, w.channel_id);
            self.api.ack_welcome(w.id).await.unwrap();
            joined.push((channel, w.seq));
        }
        joined
    }
}

#[derive(Default)]
struct Keys {
    identities: HashMap<String, IdentityKey>,
    certificates: HashMap<(String, String), DeviceCertificate>,
}

impl Directory for Keys {
    fn identity_key(&self, user_id: &str) -> Option<IdentityKey> {
        self.identities.get(user_id).copied()
    }

    fn certificate(&self, user_id: &str, device_id: &str) -> Option<DeviceCertificate> {
        self.certificates.get(&(user_id.into(), device_id.into())).cloned()
    }
}

/// The HTTP status of a refusal.
trait Status {
    fn status(&self) -> Option<u16>;
}

impl Status for mm_api::Error {
    fn status(&self) -> Option<u16> {
        match self {
            mm_api::Error::Api(e) => Some(e.status_code),
            _ => None,
        }
    }
}

fn post(text: &str) -> Content {
    Content::Post { message: text.into(), files: vec![] }
}

async fn channel_id(api: &Client, name: &str) -> String {
    let team = api.my_teams().await.unwrap()[0].id.clone();
    api.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == name).unwrap().id
}

async fn encrypt_channel(s: &Server, channel: &str) {
    sqlx::query("UPDATE channels SET encrypted = 1 WHERE id = ?").bind(channel).execute(&s.app.db).await.unwrap();
}

#[tokio::test]
async fn a_group_through_axiond_from_creation_to_removal() {
    let s = server().await;
    let alice_id = UserIdentity::generate();
    let bob_id = UserIdentity::generate();
    let alice = Phone::new(&s, "alice", "alice-laptop", &alice_id).await;
    let alice_phone = Phone::new(&s, "alice", "alice-phone", &alice_id).await;
    let bob = Phone::new(&s, "bob", "bob-desktop", &bob_id).await;
    let channel = channel_id(&alice.api, "secret").await;

    // Only encrypted channels get a group.
    let info = B64.encode(alice.device.create_group(&channel, &alice.server_key).unwrap());
    assert_eq!(alice.api.create_group(&channel, &info).await.unwrap_err().status(), Some(400));
    encrypt_channel(&s, &channel).await;
    alice.api.create_group(&channel, &info).await.unwrap();
    assert_eq!(alice.api.create_group(&channel, &info).await.unwrap_err().status(), Some(409), "one group per channel");

    // Alice adds everyone's devices: one key package each (not her own).
    let users = [alice.user_id.as_str(), bob.user_id.as_str()];
    let keys = alice.directory(&users).await;
    let claimed = alice.api.claim_key_packages(&[alice.user_id.clone(), bob.user_id.clone()]).await.unwrap();
    let mut claimed_devices: Vec<&str> = claimed.iter().map(|c| c.device_id.as_str()).collect();
    claimed_devices.sort();
    assert_eq!(claimed_devices, ["alice-phone", "bob-desktop"]);
    let packages: Vec<Vec<u8>> = claimed.iter().map(|c| B64.decode(&c.key_package).unwrap()).collect();
    let h = alice.device.add(&channel, &packages, &keys).unwrap();
    let seq = alice.send(&channel, &h).await.unwrap();

    for d in [&bob, &alice_phone] {
        assert_eq!(d.take_welcomes(&keys).await, vec![(channel.clone(), seq)]);
        assert!(d.api.welcomes().await.unwrap().is_empty(), "taken in once");
    }
    assert_eq!(alice.api.group_state(&channel).await.unwrap().epoch, 1);

    // A message reaches both.
    let ct = alice.device.encrypt(&channel, &post("only us"), &Aad::post(&channel, "", "p1")).unwrap();
    for d in [&bob, &alice_phone] {
        let Incoming::Message { content, .. } = d.device.process(&channel, &ct.ciphertext().unwrap(), &keys).unwrap() else { panic!() };
        assert_eq!(content, post("only us"));
    }

    // Two commits at once: the second gets 409, catches up, tries again.
    let a = alice.device.update(&channel).unwrap();
    let b = bob.device.update(&channel).unwrap();
    let seq = alice.send(&channel, &a).await.unwrap();
    assert_eq!(bob.send(&channel, &b).await.unwrap_err().status(), Some(409));
    let (_, got) = bob.catch_up(&channel, seq - 1, &keys).await;
    assert!(matches!(got.as_slice(), [Incoming::Commit { .. }]));
    let b = bob.device.update(&channel).unwrap();
    let seq = bob.send(&channel, &b).await.unwrap();
    // Each device carries on from the last handshake it has: Alice from her
    // own commit, her phone from before both.
    alice.catch_up(&channel, seq - 1, &keys).await;
    alice_phone.catch_up(&channel, seq - 2, &keys).await;
    assert_eq!([alice.device.epoch(&channel).unwrap(), bob.device.epoch(&channel).unwrap(), alice_phone.device.epoch(&channel).unwrap()], [3, 3, 3]);

    // Bob leaves: the server proposes removing his device; Alice must
    // commit that before she can send.
    bob.api.leave_channel(&channel, &bob.user_id).await.unwrap();
    let (seq, got) = alice.catch_up(&channel, seq, &keys).await;
    assert!(matches!(got.as_slice(), [Incoming::Proposal { removed }] if removed == &vec![(bob.user_id.clone(), "bob-desktop".to_string())]));
    assert!(alice.device.encrypt(&channel, &post("x"), &Aad::post(&channel, "", "p2")).is_err());
    let commit = alice.device.commit_pending(&channel).unwrap().unwrap();
    let seq_removed = alice.send(&channel, &commit).await.unwrap();
    assert_eq!(seq_removed, seq + 1);
    // Bob is no longer in the channel, so he can't read the log; his copy
    // of the group can't read what comes next either.
    assert_eq!(bob.api.handshakes(&channel, 0).await.unwrap_err().status(), Some(403));
    let ct = alice.device.encrypt(&channel, &post("bob's gone"), &Aad::post(&channel, "", "p3")).unwrap();
    assert!(bob.device.process(&channel, &ct.ciphertext().unwrap(), &keys).is_err());
    alice_phone.catch_up(&channel, seq - 1, &keys).await;
    let Incoming::Message { content, .. } = alice_phone.device.process(&channel, &ct.ciphertext().unwrap(), &keys).unwrap() else { panic!() };
    assert_eq!(content, post("bob's gone"));

    // Signing out takes a device out too.
    let mut phone_api = alice_phone.api;
    phone_api.logout().await.unwrap();
    let (_, got) = alice.catch_up(&channel, seq_removed, &keys).await;
    assert!(matches!(got.as_slice(), [Incoming::Proposal { removed }] if removed == &vec![(alice.user_id.clone(), "alice-phone".to_string())]));
    let identity = alice.api.identity(&alice.user_id).await.unwrap();
    assert_eq!(identity.revoked, vec!["alice-phone"]);
    assert_eq!(identity.devices.len(), 1);
}

#[tokio::test]
async fn the_server_refuses_what_isnt_yours_or_isnt_right() {
    let s = server().await;
    let alice_id = UserIdentity::generate();
    let alice = Phone::new(&s, "alice", "alice-laptop", &alice_id).await;
    let carol = Phone::new(&s, "carol", "carol-desktop", &UserIdentity::generate()).await;

    // A certificate for another device, or signed by another key.
    let other = Device::create(&alice.user_id, "elsewhere", MemoryStorage::default()).unwrap();
    let cert = alice_id.certify(&alice.user_id, "elsewhere", other.signature_key(), 1);
    assert_eq!(alice.api.publish_device(&serde_json::to_value(&cert).unwrap()).await.unwrap_err().status(), Some(400));
    let mut api = Client::new(&s.url).unwrap().device_id("fresh");
    api.login("alice", PASSWORD, None).await.unwrap();
    let fresh = Device::create(&alice.user_id, "fresh", MemoryStorage::default()).unwrap();
    let forged = UserIdentity::generate().certify(&alice.user_id, "fresh", fresh.signature_key(), 1);
    assert_eq!(api.publish_device(&serde_json::to_value(&forged).unwrap()).await.unwrap_err().status(), Some(400));
    // Key packages for another device, or before a certificate.
    assert_eq!(api.upload_key_packages(&[B64.encode(fresh.key_package(false).unwrap())]).await.unwrap_err().status(), Some(400));
    assert_eq!(alice.api.upload_key_packages(&[B64.encode(other.key_package(false).unwrap())]).await.unwrap_err().status(), Some(400));
    assert_eq!(alice.api.upload_key_packages(&["not base64!".into()]).await.unwrap_err().status(), Some(400));
    // A second, different identity key.
    assert_eq!(alice.api.put_identity(&B64.encode(UserIdentity::generate().public().0)).await.unwrap_err().status(), Some(409));

    // One-time key packages are used once; then the last-resort one.
    let claims: Vec<usize> = {
        let mut n = Vec::new();
        for _ in 0..5 {
            n.push(carol.api.claim_key_packages(std::slice::from_ref(&alice.user_id)).await.unwrap().len());
        }
        n
    };
    assert_eq!(claims, [1, 1, 1, 1, 1]);

    // Groups: only for members, only for the creator's own device.
    let channel = channel_id(&alice.api, "secret").await;
    encrypt_channel(&s, &channel).await;
    carol.api.leave_channel(&channel, &carol.user_id).await.unwrap();
    let info = B64.encode(carol.device.create_group(&channel, &carol.server_key).unwrap());
    assert_eq!(carol.api.create_group(&channel, &info).await.unwrap_err().status(), Some(403));
    let foreign = B64.encode(alice.device.create_group(&channel, &ServerKey { signature_key: vec![1; 32] }).unwrap());
    assert_eq!(alice.api.create_group(&channel, &foreign).await.unwrap_err().status(), Some(400), "must name this server");

    // Adding someone who isn't in the channel is refused by the server.
    admin::add_channel(&s.app, "home", "other", "Other").await.unwrap();
    let other_channel = channel_id(&alice.api, "other").await;
    encrypt_channel(&s, &other_channel).await;
    carol.api.leave_channel(&other_channel, &carol.user_id).await.unwrap();
    let info = B64.encode(alice.device.create_group(&other_channel, &alice.server_key).unwrap());
    alice.api.create_group(&other_channel, &info).await.unwrap();
    let keys = alice.directory(&[alice.user_id.as_str(), carol.user_id.as_str()]).await;
    let claimed = alice.api.claim_key_packages(std::slice::from_ref(&carol.user_id)).await.unwrap();
    let h = alice.device.add(&other_channel, &[B64.decode(&claimed[0].key_package).unwrap()], &keys).unwrap();
    assert_eq!(alice.send(&other_channel, &h).await.unwrap_err().status(), Some(400));
    assert!(carol.api.welcomes().await.unwrap().is_empty());
}
