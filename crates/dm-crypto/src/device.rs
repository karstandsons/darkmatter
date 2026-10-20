//! One device: its MLS signature key and its groups (E2EE.md §3–4).
//!
//! Every encrypted channel is one MLS group whose ID is the channel ID, and
//! whose members are devices. Handshake messages (commits, proposals) go out
//! as `PublicMessage`s so the server can check them; content is always a
//! `PrivateMessage`.
//!
//! Anything that changes membership returns a [`Handshake`] for the server.
//! It stays pending on this device until the server accepts it
//! ([`Device::confirm`]) or refuses it because someone else's commit got
//! there first ([`Device::discard`]).

use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::messages::group_info::{GroupInfo, VerifiableGroupInfo};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;
use openmls_traits::storage::{CURRENT_VERSION, StorageProvider};

use crate::identity::{self, Directory};
use crate::wire::{self, Aad, Content};
use crate::{CIPHERSUITE, Error, PADDING, PAST_EPOCHS, Provider, Result, mls};

/// A device's identity in groups: `user_id:device_id`.
pub type DeviceName = (String, String);

/// Something for the server: a commit, the `Welcome` for devices it adds,
/// and the group's new `GroupInfo` (which lets others join by external
/// commit).
#[derive(Clone, Debug)]
pub struct Handshake {
    /// The epoch the commit was made in (the server's current one).
    pub epoch: u64,
    pub commit: Vec<u8>,
    pub welcome: Option<Vec<u8>>,
    pub group_info: Vec<u8>,
}

/// What a message from the server turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    /// Someone's message. Check `aad` against where the server delivered it
    /// ([`Aad::check`]) before showing it.
    Message { from: DeviceName, content: Content, aad: Aad, epoch: u64 },
    /// The server proposed removing these devices. The next thing this
    /// device sends must be preceded by [`Device::commit_pending`].
    Proposal { removed: Vec<DeviceName> },
    /// A commit, now applied: the group is in `epoch`.
    Commit { by: Option<DeviceName>, added: Vec<DeviceName>, removed: Vec<DeviceName>, epoch: u64 },
    /// Something this device sent, fanned back by the server: nothing to do.
    Own,
}

/// The server's key as an MLS external sender. Groups name it so that it can
/// propose removals (only removals: see [`Device::process`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerKey {
    pub signature_key: Vec<u8>,
}

/// The MLS credential the server signs its proposals as.
pub(crate) const SERVER_IDENTITY: &[u8] = b"axiond";

pub struct Device<S> {
    pub user_id: String,
    pub device_id: String,
    provider: Provider<S>,
    signer: SignatureKeyPair,
}

impl<S: StorageProvider<CURRENT_VERSION>> Device<S> {
    /// A new device with a fresh signature key, kept in `storage`. Its user
    /// then certifies [`Device::signature_key`] with their identity key.
    pub fn create(user_id: &str, device_id: &str, storage: S) -> Result<Self> {
        let provider = Provider::new(storage)?;
        let signer = SignatureKeyPair::new(CIPHERSUITE.signature_algorithm()).map_err(mls)?;
        signer.store(provider.storage()).map_err(|e| Error::Storage(format!("{e:?}")))?;
        Ok(Device { user_id: user_id.into(), device_id: device_id.into(), provider, signer })
    }

    /// The device again, from `storage`, given its public signature key.
    pub fn open(user_id: &str, device_id: &str, signature_key: &[u8], storage: S) -> Result<Self> {
        let provider = Provider::new(storage)?;
        let signer = SignatureKeyPair::read(provider.storage(), signature_key, CIPHERSUITE.signature_algorithm())
            .ok_or_else(|| Error::Storage("this device's signature key isn't stored".into()))?;
        Ok(Device { user_id: user_id.into(), device_id: device_id.into(), provider, signer })
    }

    pub fn signature_key(&self) -> &[u8] {
        self.signer.public()
    }

    /// Where the device keeps its state (the app saves it).
    pub fn storage(&self) -> &S {
        self.provider.storage()
    }

    fn credential(&self) -> CredentialWithKey {
        CredentialWithKey { credential: identity::credential(&self.user_id, &self.device_id), signature_key: self.signer.to_public_vec().into() }
    }

    /// A key package others use to add this device to a group: one-time, or
    /// the last-resort one the server hands out when the others run out.
    pub fn key_package(&self, last_resort: bool) -> Result<Vec<u8>> {
        // Every key package says the device understands last-resort ones,
        // which openmls requires before it accepts one.
        let capabilities = Capabilities::new(None, Some(&[CIPHERSUITE]), Some(&[ExtensionType::LastResort]), None, Some(&[CredentialType::Basic]));
        let mut builder = KeyPackage::builder().leaf_node_capabilities(capabilities);
        if last_resort {
            builder = builder.mark_as_last_resort();
        }
        let bundle = builder.build(CIPHERSUITE, &self.provider, &self.signer, self.credential()).map_err(mls)?;
        bundle.key_package().tls_serialize_detached().map_err(mls)
    }

    fn create_config(server: &ServerKey) -> Result<MlsGroupCreateConfig> {
        let external = Extension::ExternalSenders(vec![server_sender(server)]);
        Ok(MlsGroupCreateConfig::builder()
            .ciphersuite(CIPHERSUITE)
            .wire_format_policy(PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
            .padding_size(PADDING)
            .max_past_epochs(PAST_EPOCHS)
            .sender_ratchet_configuration(ratchet())
            .use_ratchet_tree_extension(true)
            .with_group_context_extensions(Extensions::single(external).map_err(mls)?)
            .build())
    }

    fn join_config() -> MlsGroupJoinConfig {
        MlsGroupJoinConfig::builder()
            .wire_format_policy(PURE_PLAINTEXT_WIRE_FORMAT_POLICY)
            .padding_size(PADDING)
            .max_past_epochs(PAST_EPOCHS)
            .sender_ratchet_configuration(ratchet())
            .use_ratchet_tree_extension(true)
            .build()
    }

    fn load(&self, channel_id: &str) -> Result<MlsGroup> {
        MlsGroup::load(self.provider.storage(), &GroupId::from_slice(channel_id.as_bytes()))
            .map_err(|e| Error::Storage(format!("{e:?}")))?
            .ok_or_else(|| Error::NoGroup(channel_id.into()))
    }

    pub fn has_group(&self, channel_id: &str) -> bool {
        self.load(channel_id).is_ok()
    }

    /// Starts the group for a channel with just this device in it. The
    /// returned `GroupInfo` registers it with the server.
    pub fn create_group(&self, channel_id: &str, server: &ServerKey) -> Result<Vec<u8>> {
        let group = MlsGroup::new_with_group_id(
            &self.provider,
            &self.signer,
            &Self::create_config(server)?,
            GroupId::from_slice(channel_id.as_bytes()),
            self.credential(),
        )
        .map_err(mls)?;
        self.group_info(&group)
    }

    fn group_info(&self, group: &MlsGroup) -> Result<Vec<u8>> {
        group.export_group_info(self.provider.crypto(), &self.signer, true).map_err(mls)?.to_bytes().map_err(mls)
    }

    /// Adds devices, from their key packages. Each must carry a valid
    /// certificate for its key (checked against `directory`), or nothing is
    /// added.
    pub fn add(&self, channel_id: &str, key_packages: &[Vec<u8>], directory: &impl Directory) -> Result<Handshake> {
        let mut group = self.load(channel_id)?;
        let mut packages = Vec::new();
        for bytes in key_packages {
            let kp = KeyPackageIn::tls_deserialize_exact(bytes.as_slice()).map_err(|_| Error::Malformed("key package"))?;
            let kp = kp.validate(self.provider.crypto(), ProtocolVersion::Mls10).map_err(mls)?;
            identity::verify_member(kp.leaf_node().credential(), kp.leaf_node().signature_key().as_slice(), directory)?;
            packages.push(kp);
        }
        let epoch = group.epoch().as_u64();
        let (commit, welcome, info) = group.add_members(&self.provider, &self.signer, &packages).map_err(mls)?;
        handshake(epoch, commit, Some(welcome), info)
    }

    /// Commits the proposals waiting in the group (the server's removals),
    /// if there are any. Must happen before sending.
    pub fn commit_pending(&self, channel_id: &str) -> Result<Option<Handshake>> {
        let mut group = self.load(channel_id)?;
        if group.pending_proposals().next().is_none() {
            return Ok(None);
        }
        let epoch = group.epoch().as_u64();
        let (commit, welcome, info) = group.commit_to_pending_proposals(&self.provider, &self.signer).map_err(mls)?;
        handshake(epoch, commit, welcome, info).map(Some)
    }

    /// A fresh leaf key, for post-compromise security (every week, and after
    /// a day offline).
    pub fn update(&self, channel_id: &str) -> Result<Handshake> {
        let mut group = self.load(channel_id)?;
        let epoch = group.epoch().as_u64();
        let bundle = group.self_update(&self.provider, &self.signer, LeafNodeParameters::default()).map_err(mls)?;
        let (commit, welcome, info) = bundle.into_contents();
        handshake(epoch, commit, welcome.map(wrap_welcome), info)
    }

    /// The server took our handshake: apply it.
    pub fn confirm(&self, channel_id: &str) -> Result<()> {
        let mut group = self.load(channel_id)?;
        group.merge_pending_commit(&self.provider).map_err(mls)
    }

    /// The server refused our handshake (someone else's commit won): drop
    /// it, process theirs, and try again.
    pub fn discard(&self, channel_id: &str) -> Result<()> {
        let mut group = self.load(channel_id)?;
        group.clear_pending_commit(self.provider.storage()).map_err(|e| Error::Storage(format!("{e:?}")))
    }

    /// Joins from a `Welcome` someone's commit added us with. Every member
    /// must check out against `directory`, or we don't join.
    pub fn join(&self, welcome: &[u8], server: &ServerKey, directory: &impl Directory) -> Result<String> {
        let message = MlsMessageIn::tls_deserialize_exact(welcome).map_err(|_| Error::Malformed("welcome"))?;
        let MlsMessageBodyIn::Welcome(welcome) = message.extract() else {
            return Err(Error::Malformed("welcome"));
        };
        let staged = StagedWelcome::new_from_welcome(&self.provider, &Self::join_config(), welcome, None).map_err(mls)?;
        for member in staged.members() {
            identity::verify_member(&member.credential, &member.signature_key, directory)?;
        }
        let mut group = staged.into_group(&self.provider).map_err(mls)?;
        if let Err(e) = check_external_senders(&group, server) {
            let _ = group.delete(self.provider.storage());
            return Err(e);
        }
        group_channel(&group)
    }

    /// Joins by external commit, from the group's latest `GroupInfo`: for a
    /// public encrypted channel, or this user's new device. The commit goes
    /// to the server like any other.
    pub fn join_external(&self, group_info: &[u8], server: &ServerKey, directory: &impl Directory) -> Result<(String, Handshake)> {
        let info = parse_group_info(group_info)?;
        let epoch = info.epoch().as_u64();
        let (group, bundle) = MlsGroup::external_commit_builder()
            .with_config(Self::join_config())
            .build_group(&self.provider, info, self.credential())
            .map_err(mls)?
            .load_psks(self.provider.storage())
            .map_err(mls)?
            .build(self.provider.rand(), self.provider.crypto(), &self.signer, |_| true)
            .map_err(mls)?
            .finalize(&self.provider)
            .map_err(mls)?;
        let channel = group_channel(&group)?;
        let checked = group
            .members()
            .filter(|m| m.index != group.own_leaf_index())
            .try_for_each(|m| identity::verify_member(&m.credential, &m.signature_key, directory).map(|_| ()))
            .and_then(|()| check_external_senders(&group, server));
        if let Err(e) = checked {
            let mut group = group;
            let _ = group.delete(self.provider.storage());
            return Err(e);
        }
        let (commit, welcome, info) = bundle.into_contents();
        Ok((channel, handshake(epoch, commit, welcome.map(wrap_welcome), info)?))
    }

    /// Encrypts content for the group. Refuses while the server's proposals
    /// wait: commit them first ([`Device::commit_pending`]).
    pub fn encrypt(&self, channel_id: &str, content: &Content, aad: &Aad) -> Result<wire::DmE2ee> {
        wire::check_kind(content, aad)?;
        if aad.channel_id != channel_id {
            return Err(Error::Untrusted("the authenticated data names another channel".into()));
        }
        let mut group = self.load(channel_id)?;
        let plaintext = serde_json::to_vec(content).map_err(|_| Error::Malformed("content"))?;
        group.set_aad(aad.to_bytes());
        let message = group.create_message(&self.provider, &self.signer, &plaintext).map_err(mls)?;
        Ok(wire::DmE2ee::new(group.epoch().as_u64(), &message.to_bytes().map_err(mls)?))
    }

    /// Reads a message the server delivered for this channel: someone's
    /// content, a proposal or a commit. Commits are checked (who may propose
    /// what, and that every new or changed device has a valid certificate)
    /// before they're applied.
    pub fn process(&self, channel_id: &str, message: &[u8], directory: &impl Directory) -> Result<Incoming> {
        let mut group = self.load(channel_id)?;
        let message = MlsMessageIn::tls_deserialize_exact(message).map_err(|_| Error::Malformed("mls message"))?;
        let message = message.try_into_protocol_message().map_err(|_| Error::Malformed("mls message"))?;
        if message.group_id().as_slice() != channel_id.as_bytes() {
            return Err(Error::Untrusted("a message for another group".into()));
        }
        let processed = group.process_message(&self.provider, message).map_err(mls)?;
        let epoch = processed.epoch().as_u64();
        let sender = processed.sender().clone();
        let from = match &sender {
            Sender::Member(_) | Sender::NewMemberCommit => Some(identity::parse_credential(processed.credential())?),
            _ => None,
        };
        let aad = processed.aad().to_vec();
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => {
                let from = from.ok_or_else(|| Error::Untrusted("a message from outside the group".into()))?;
                let content: Content = serde_json::from_slice(&app.into_bytes()).map_err(|_| Error::Malformed("content"))?;
                let aad = Aad::from_bytes(&aad)?;
                wire::check_kind(&content, &aad)?;
                if aad.channel_id != channel_id {
                    return Err(Error::Untrusted("a message for another channel".into()));
                }
                Ok(Incoming::Message { from, content, aad, epoch })
            }
            ProcessedMessageContent::ProposalMessage(proposal) => {
                // Members commit their changes directly; only the server
                // proposes, and only removals.
                let Proposal::Remove(remove) = proposal.proposal() else {
                    return Err(Error::Untrusted("a proposal other than a removal".into()));
                };
                if !matches!(sender, Sender::External(_)) {
                    return Err(Error::Untrusted("a standalone proposal from a member".into()));
                }
                let removed = vec![name_at(&group, remove.removed())?];
                group.store_pending_proposal(self.provider.storage(), *proposal).map_err(|e| Error::Storage(format!("{e:?}")))?;
                Ok(Incoming::Proposal { removed })
            }
            ProcessedMessageContent::ExternalJoinProposalMessage(_) => Err(Error::Untrusted("an external join proposal".into())),
            // Our own commit, fanned back before we confirmed it: same thing.
            ProcessedMessageContent::OwnPendingCommit => {
                group.merge_pending_commit(&self.provider).map_err(mls)?;
                Ok(Incoming::Own)
            }
            ProcessedMessageContent::OwnPrivateMessage => Ok(Incoming::Own),
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                let mut added = Vec::new();
                let mut removed = Vec::new();
                for queued in staged.queued_proposals() {
                    let by_server = matches!(queued.sender(), Sender::External(_));
                    match queued.proposal() {
                        Proposal::Add(add) if !by_server => {
                            let leaf = add.key_package().leaf_node();
                            added.push(identity::verify_member(leaf.credential(), leaf.signature_key().as_slice(), directory)?);
                        }
                        Proposal::Update(update) if !by_server => {
                            let leaf = update.leaf_node();
                            identity::verify_member(leaf.credential(), leaf.signature_key().as_slice(), directory)?;
                        }
                        Proposal::Remove(remove) => removed.push(name_at(&group, remove.removed())?),
                        Proposal::ExternalInit(_) if matches!(sender, Sender::NewMemberCommit) => {}
                        _ => return Err(Error::Untrusted("a commit with a proposal that isn't allowed".into())),
                    }
                }
                // A new leaf (from an update path or an external join) must be
                // vouched for too.
                if let Some(leaf) = staged.update_path_leaf_node() {
                    let joined = identity::verify_member(leaf.credential(), leaf.signature_key().as_slice(), directory)?;
                    if matches!(sender, Sender::NewMemberCommit) {
                        added.push(joined);
                    }
                }
                // Someone else's commit for this epoch beat ours, if we had
                // one: it's gone (the caller makes it again).
                if group.pending_commit().is_some() {
                    group.clear_pending_commit(self.provider.storage()).map_err(|e| Error::Storage(format!("{e:?}")))?;
                }
                group.merge_staged_commit(&self.provider, *staged).map_err(mls)?;
                Ok(Incoming::Commit { by: from, added, removed, epoch: group.epoch().as_u64() })
            }
        }
    }

    pub fn epoch(&self, channel_id: &str) -> Result<u64> {
        Ok(self.load(channel_id)?.epoch().as_u64())
    }

    /// The devices in the group, as the group itself says (not the server).
    pub fn members(&self, channel_id: &str) -> Result<Vec<DeviceName>> {
        let group = self.load(channel_id)?;
        group.members().map(|m| identity::parse_credential(&m.credential)).collect()
    }

    /// Whether this device is still in the group (false once removed).
    pub fn is_member(&self, channel_id: &str) -> bool {
        self.load(channel_id).is_ok_and(|g| g.is_active())
    }

    /// Whether the server's proposals wait to be committed.
    pub fn has_pending_proposals(&self, channel_id: &str) -> bool {
        self.load(channel_id).is_ok_and(|g| g.pending_proposals().next().is_some())
    }

    /// A secret both ends of a group can derive and no one else can (MLS
    /// exporter): for call keys later.
    pub fn export_secret(&self, channel_id: &str, label: &str, context: &[u8], len: usize) -> Result<Vec<u8>> {
        let group = self.load(channel_id)?;
        group.export_secret(self.provider.crypto(), label, context, len).map_err(mls)
    }
}

/// Out-of-order and skipped messages a device tolerates within an epoch.
fn ratchet() -> SenderRatchetConfiguration {
    SenderRatchetConfiguration::new(20, 1000)
}

fn handshake(epoch: u64, commit: MlsMessageOut, welcome: Option<MlsMessageOut>, info: Option<GroupInfo>) -> Result<Handshake> {
    let info = info.ok_or_else(|| Error::Mls("the commit came without a group info".into()))?;
    Ok(Handshake {
        epoch,
        commit: commit.to_bytes().map_err(mls)?,
        welcome: welcome.map(|w| w.to_bytes()).transpose().map_err(mls)?,
        group_info: MlsMessageOut::from(info).to_bytes().map_err(mls)?,
    })
}

fn wrap_welcome(welcome: Welcome) -> MlsMessageOut {
    MlsMessageOut::from_welcome(welcome, ProtocolVersion::Mls10)
}

/// A `GroupInfo` as it travels: wrapped as an MLS message.
pub(crate) fn parse_group_info(bytes: &[u8]) -> Result<VerifiableGroupInfo> {
    match MlsMessageIn::tls_deserialize_exact(bytes).map(MlsMessageIn::extract) {
        Ok(MlsMessageBodyIn::GroupInfo(info)) => Ok(info),
        _ => Err(Error::Malformed("group info")),
    }
}

/// The external sender our groups name: the server.
pub(crate) fn server_sender(server: &ServerKey) -> ExternalSender {
    ExternalSender::new(server.signature_key.clone().into(), BasicCredential::new(SERVER_IDENTITY.to_vec()).into())
}

fn group_channel(group: &MlsGroup) -> Result<String> {
    String::from_utf8(group.group_id().as_slice().to_vec()).map_err(|_| Error::Malformed("group id"))
}

fn name_at(group: &MlsGroup, index: LeafNodeIndex) -> Result<DeviceName> {
    let credential = group.member(index).ok_or_else(|| Error::Untrusted("a removal of someone not in the group".into()))?;
    identity::parse_credential(credential)
}

/// A group we join must name exactly one external sender, our server, as
/// the groups we create do, so no one else can propose anything in it.
fn check_external_senders(group: &MlsGroup, server: &ServerKey) -> Result<()> {
    let senders = group.extensions().external_senders().map(|s| s.as_slice()).unwrap_or_default();
    match senders {
        [one] if *one == server_sender(server) => Ok(()),
        _ => Err(Error::Untrusted("the group doesn't have this server as its only external sender".into())),
    }
}
