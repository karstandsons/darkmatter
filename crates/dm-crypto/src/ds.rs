//! The server's side: MLS delivery service (E2EE.md §5).
//!
//! The server tracks each group's public state (the tree and the epoch, no
//! secrets) with openmls's `PublicGroup`, so it can refuse handshake messages
//! that would break a group: commits for an old epoch, from non-members,
//! adding devices without certificates or users who aren't in the channel,
//! or leaving out removals the server proposed. It is also the group's one
//! external sender, which lets it propose removals, and nothing else.

use openmls::prelude::tls_codec::Deserialize as _;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;
use openmls_traits::storage::{CURRENT_VERSION, StorageProvider};

use crate::device::{DeviceName, ServerKey, parse_group_info, server_sender};
use crate::identity::{self, Directory};
use crate::{CIPHERSUITE, Error, Provider, Result, mls};

/// Why a handshake was refused.
#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    /// Someone else's commit for this epoch got here first (HTTP 409): the
    /// sender processes it and tries again.
    #[error("the group has moved on to epoch {current}")]
    Stale { current: u64 },
    /// It's wrong, and trying again won't help (HTTP 400/403).
    #[error(transparent)]
    Invalid(#[from] Error),
}

/// A commit the server took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub epoch: u64,
    /// A device joining by itself (external commit): it needs no Welcome.
    pub external: bool,
    pub added: Vec<DeviceName>,
    pub removed: Vec<DeviceName>,
}

pub struct Server<S> {
    provider: Provider<S>,
    signer: SignatureKeyPair,
}

impl<S: StorageProvider<CURRENT_VERSION>> Server<S> {
    /// A server with a new external-sender key, kept in `storage`.
    pub fn create(storage: S) -> Result<Self> {
        let provider = Provider::new(storage)?;
        let signer = SignatureKeyPair::new(CIPHERSUITE.signature_algorithm()).map_err(mls)?;
        signer.store(provider.storage()).map_err(|e| Error::Storage(format!("{e:?}")))?;
        Ok(Server { provider, signer })
    }

    pub fn open(signature_key: &[u8], storage: S) -> Result<Self> {
        let provider = Provider::new(storage)?;
        let signer = SignatureKeyPair::read(provider.storage(), signature_key, CIPHERSUITE.signature_algorithm())
            .ok_or_else(|| Error::Storage("the server's key isn't stored".into()))?;
        Ok(Server { provider, signer })
    }

    /// Where the server keeps the groups' public state (the app saves it).
    pub fn storage(&self) -> &S {
        self.provider.storage()
    }

    /// The epoch a handshake message was made in, without applying it
    /// ([`Server::commit`] does the real checks).
    pub fn epoch_of(bytes: &[u8]) -> Result<u64> {
        Ok(parse_protocol(bytes)?.epoch().as_u64())
    }

    /// What clients put in their groups as the external sender (published in
    /// `e2ee/config`).
    pub fn key(&self) -> ServerKey {
        ServerKey { signature_key: self.signer.to_public_vec() }
    }

    fn load(&self, channel_id: &str) -> Result<PublicGroup> {
        PublicGroup::load(self.provider.storage(), &GroupId::from_slice(channel_id.as_bytes()))
            .map_err(|e| Error::Storage(format!("{e:?}")))?
            .ok_or_else(|| Error::NoGroup(channel_id.into()))
    }

    pub fn has_group(&self, channel_id: &str) -> bool {
        self.load(channel_id).is_ok()
    }

    /// Starts tracking a new group from its creator's first `GroupInfo`.
    /// It must be for `channel_id`, at epoch 0, with only its creator in it,
    /// and name this server as its only external sender.
    pub fn register(&self, channel_id: &str, group_info: &[u8], creator: &DeviceName, directory: &impl Directory) -> Result<()> {
        if self.has_group(channel_id) {
            return Err(Error::Untrusted("that channel already has a group".into()));
        }
        let info = parse_group_info(group_info)?;
        if info.group_id().as_slice() != channel_id.as_bytes() || info.epoch().as_u64() != 0 {
            return Err(Error::Untrusted("a new group must be for its channel, at epoch 0".into()));
        }
        let tree = info.extensions().ratchet_tree().ok_or(Error::Malformed("group info without its tree"))?.ratchet_tree().clone();
        let (group, _) = PublicGroup::from_external(self.provider.crypto(), self.provider.storage(), tree, info, ProposalStore::default()).map_err(mls)?;
        let checked = (|| {
            let members: Vec<Member> = group.members().collect();
            let [member] = members.as_slice() else {
                return Err(Error::Untrusted("a new group must hold only its creator".into()));
            };
            if &identity::verify_member(&member.credential, &member.signature_key, directory)? != creator {
                return Err(Error::Untrusted("the group's creator isn't the device registering it".into()));
            }
            let senders = group.group_context().extensions().external_senders().map(|s| s.as_slice()).unwrap_or_default();
            match senders {
                [one] if *one == server_sender(&self.key()) => Ok(()),
                _ => Err(Error::Untrusted("the group must name this server as its only external sender".into())),
            }
        })();
        if checked.is_err() {
            let _ = PublicGroup::delete(self.provider.storage(), group.group_id());
        }
        checked
    }

    /// Checks a commit from `sender` (the logged-in device) and applies it.
    /// `may_join` says whether a user may be in the channel. `group_info` is
    /// the group's new `GroupInfo`, which the server hands out for external
    /// joins.
    pub fn commit(
        &self,
        channel_id: &str,
        sender: &DeviceName,
        commit: &[u8],
        group_info: &[u8],
        may_join: impl Fn(&str) -> bool,
        directory: &impl Directory,
    ) -> Result<Applied, Refusal> {
        let mut group = self.load(channel_id)?;
        let current = group.group_context().epoch().as_u64();
        let message = parse_protocol(commit)?;
        if message.group_id().as_slice() != channel_id.as_bytes() {
            return Err(Error::Untrusted("a commit for another group".into()).into());
        }
        if message.epoch().as_u64() != current {
            return Err(Refusal::Stale { current });
        }
        let processed = group.process_message(self.provider.crypto(), message).map_err(mls)?;
        let by = match processed.sender() {
            Sender::Member(_) | Sender::NewMemberCommit => identity::parse_credential(processed.credential())?,
            _ => return Err(Error::Untrusted("a commit must come from a device".into()).into()),
        };
        if &by != sender {
            return Err(Error::Untrusted("a commit signed by another device than the one sending it".into()).into());
        }
        let external_join = matches!(processed.sender(), Sender::NewMemberCommit);
        let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() else {
            return Err(Error::Untrusted("only commits go to the handshake log".into()).into());
        };

        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut removed_leaves = Vec::new();
        for queued in staged.queued_proposals() {
            let by_server = matches!(queued.sender(), Sender::External(_));
            match queued.proposal() {
                Proposal::Add(add) if !by_server => {
                    let leaf = add.key_package().leaf_node();
                    let device = identity::verify_member(leaf.credential(), leaf.signature_key().as_slice(), directory)?;
                    if !may_join(&device.0) {
                        return Err(Error::Untrusted(format!("{} isn't in this channel", device.0)).into());
                    }
                    added.push(device);
                }
                Proposal::Update(update) if !by_server => {
                    let leaf = update.leaf_node();
                    identity::verify_member(leaf.credential(), leaf.signature_key().as_slice(), directory)?;
                }
                Proposal::Remove(remove) => {
                    removed_leaves.push(remove.removed());
                    let gone = group.leaf(remove.removed()).ok_or_else(|| Error::Untrusted("removing an empty leaf".into()))?;
                    removed.push(identity::parse_credential(gone.credential())?);
                }
                Proposal::ExternalInit(_) if external_join => {}
                _ => return Err(Error::Untrusted("a commit with a proposal that isn't allowed".into()).into()),
            }
        }
        if let Some(leaf) = staged.update_path_leaf_node() {
            let device = identity::verify_member(leaf.credential(), leaf.signature_key().as_slice(), directory)?;
            if external_join {
                if !may_join(&device.0) {
                    return Err(Error::Untrusted(format!("{} isn't in this channel", device.0)).into());
                }
                added.push(device);
            }
        }
        // Removals the server proposed can't be left out.
        let pending: Vec<LeafNodeIndex> = group
            .queued_proposals(self.provider.storage())
            .map_err(|e| Error::Storage(format!("{e:?}")))?
            .into_iter()
            .filter_map(|(_, p)| match (p.sender(), p.proposal()) {
                (Sender::External(_), Proposal::Remove(r)) => Some(r.removed()),
                _ => None,
            })
            .collect();
        if let Some(missing) = pending.iter().find(|leaf| !removed_leaves.contains(leaf)) {
            return Err(Error::Untrusted(format!("the commit leaves out the server's removal of leaf {}", missing.u32())).into());
        }

        let info = parse_group_info(group_info)?;
        if info.group_id().as_slice() != channel_id.as_bytes() || info.epoch().as_u64() != current + 1 {
            return Err(Error::Untrusted("the group info isn't for the commit's new epoch".into()).into());
        }
        group.merge_commit(self.provider.storage(), *staged).map_err(mls)?;
        let epoch = group.group_context().epoch().as_u64();
        Ok(Applied { epoch, external: external_join, added, removed })
    }

    /// Proposes removing a device (it left the channel, or its session was
    /// revoked). Members commit it before they next send.
    pub fn propose_remove(&self, channel_id: &str, device: &DeviceName) -> Result<Vec<u8>> {
        let mut group = self.load(channel_id)?;
        let leaf = group
            .members()
            .find(|m| identity::parse_credential(&m.credential).is_ok_and(|d| &d == device))
            .ok_or_else(|| Error::Untrusted(format!("{}:{} isn't in the group", device.0, device.1)))?
            .index;
        let message = ExternalProposal::new_remove::<Provider<S>>(
            leaf,
            group.group_id().clone(),
            group.group_context().epoch(),
            &self.signer,
            SenderExtensionIndex::new(0),
        )
        .map_err(mls)?;
        let bytes = message.to_bytes().map_err(mls)?;
        // The server's own copy, so the commit that includes it checks out.
        let processed = group.process_message(self.provider.crypto(), parse_protocol(&bytes)?).map_err(mls)?;
        let ProcessedMessageContent::ProposalMessage(queued) = processed.into_content() else {
            return Err(Error::Mls("the removal didn't read back as a proposal".into()));
        };
        group.add_proposal(self.provider.storage(), *queued).map_err(|e| Error::Storage(format!("{e:?}")))?;
        Ok(bytes)
    }

    pub fn epoch(&self, channel_id: &str) -> Result<u64> {
        Ok(self.load(channel_id)?.group_context().epoch().as_u64())
    }

    pub fn members(&self, channel_id: &str) -> Result<Vec<DeviceName>> {
        self.load(channel_id)?.members().map(|m| identity::parse_credential(&m.credential)).collect()
    }
}

fn parse_protocol(bytes: &[u8]) -> Result<ProtocolMessage> {
    MlsMessageIn::tls_deserialize_exact(bytes)
        .ok()
        .and_then(|m| m.try_into_protocol_message().ok())
        .ok_or(Error::Malformed("mls message"))
}

/// Checks a key package a device uploads: well-formed, self-signed, for our
/// ciphersuite. Returns the device it's for, the signature key it uses (which
/// its certificate must vouch for) and whether it's the last-resort one.
pub fn check_key_package(bytes: &[u8]) -> Result<(DeviceName, Vec<u8>, bool)> {
    let crypto = openmls_libcrux_crypto::CryptoProvider::new().map_err(mls)?;
    let kp = KeyPackageIn::tls_deserialize_exact(bytes).map_err(|_| Error::Malformed("key package"))?;
    let kp = kp.validate(&crypto, ProtocolVersion::Mls10).map_err(mls)?;
    if kp.ciphersuite() != CIPHERSUITE {
        return Err(Error::Untrusted("a key package for another ciphersuite".into()));
    }
    let leaf = kp.leaf_node();
    Ok((identity::parse_credential(leaf.credential())?, leaf.signature_key().as_slice().to_vec(), kp.last_resort()))
}
