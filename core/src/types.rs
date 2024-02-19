use std::rc::Rc;
use std::sync::Arc;

use solana_rpc_client_api::response::{RpcContactInfo, RpcVoteAccountStatus};
use tokio::sync::broadcast::Receiver;

use crate::{
    structures::{produced_block::ProducedBlock, slot_notification::SlotNotification},
    traits::subscription_sink::SubscriptionSink,
};
use crate::structures::produced_block::ProducedBlockShared;

pub type BlockStream = Receiver<ProducedBlockShared>;
pub type SlotStream = Receiver<SlotNotification>;
pub type VoteAccountStream = Receiver<RpcVoteAccountStatus>;
pub type ClusterInfoStream = Receiver<Vec<RpcContactInfo>>;
pub type SubscptionHanderSink = Arc<dyn SubscriptionSink>;
