use anyhow::{bail, Error};
use futures::future::join_all;
use futures::TryFutureExt;
use itertools::Itertools;
use log::{debug, info, trace, warn};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::client_error::ErrorKind;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::clock::Slot;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::Signature;
use solana_sdk::transaction::Transaction;
use solana_transaction_status::TransactionConfirmationStatus;
use std::collections::{HashMap, HashSet};
use std::iter::zip;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use url::Url;

pub fn create_rpc_client(rpc_url: &Url) -> RpcClient {
    RpcClient::new_with_commitment(rpc_url.to_string(), CommitmentConfig::confirmed())
}

#[derive(Clone)]
pub enum ConfirmationResponseFromRpc {
    SendError(Arc<ErrorKind>),
    // (sent slot at confirmed commitment, confirmed slot, ..., ...)
    Success(Slot, Slot, TransactionConfirmationStatus, Duration),
    Timeout(Duration),
}

pub async fn send_and_confirm_bulk_transactions(
    rpc_client: &RpcClient,
    txs: &[Transaction],
) -> anyhow::Result<Vec<(Signature, ConfirmationResponseFromRpc)>> {
    let send_slot = poll_next_slot_start(rpc_client).await?;

    let send_config = RpcSendTransactionConfig {
        skip_preflight: true,
        preflight_commitment: None,
        encoding: None,
        max_retries: Some(3),
        min_context_slot: None,
    };

    let started_at = Instant::now();
    let batch_sigs_or_fails = join_all(txs.iter().map(|tx| {
        rpc_client
            .send_transaction_with_config(tx, send_config)
            .map_err(|e| e.kind)
    }))
    .await;

    let after_send_slot = rpc_client
        .get_slot_with_commitment(CommitmentConfig::confirmed())
        .await?;

    // optimal value is "0"
    info!(
        "slots passed while sending: {}",
        after_send_slot - send_slot
    );

    let num_sent_ok = batch_sigs_or_fails
        .iter()
        .filter(|sig_or_fail| sig_or_fail.is_ok())
        .count();
    let num_sent_failed = batch_sigs_or_fails
        .iter()
        .filter(|sig_or_fail| sig_or_fail.is_err())
        .count();

    for (i, tx_sig) in txs.iter().enumerate() {
        let tx_sent = batch_sigs_or_fails[i].is_ok();
        if tx_sent {
            info!("- tx_sent {}", tx_sig.get_signature());
        } else {
            info!("- tx_fail {}", tx_sig.get_signature());
        }
    }
    info!(
        "{} transactions sent successfully in {:.02}ms",
        num_sent_ok,
        started_at.elapsed().as_secs_f32() * 1000.0
    );
    info!(
        "{} transactions failed to send in {:.02}ms",
        num_sent_failed,
        started_at.elapsed().as_secs_f32() * 1000.0
    );

    if num_sent_failed > 0 {
        warn!(
            "Some transactions failed to send: {} out of {}",
            num_sent_failed,
            txs.len()
        );
        bail!("Failed to send all transactions");
    }

    let mut pending_status_set: HashSet<Signature> = HashSet::new();
    batch_sigs_or_fails
        .iter()
        .filter(|sig_or_fail| sig_or_fail.is_ok())
        .for_each(|sig_or_fail| {
            pending_status_set.insert(sig_or_fail.as_ref().unwrap().to_owned());
        });
    let mut result_status_map: HashMap<Signature, ConfirmationResponseFromRpc> = HashMap::new();

    // items get moved from pending_status_set to result_status_map

    let started_at = Instant::now();
    let mut iteration = 1;
    'pooling_loop: loop {
        let iteration_ends_at = started_at + Duration::from_millis(iteration * 200);
        assert_eq!(
            pending_status_set.len() + result_status_map.len(),
            num_sent_ok,
            "Items must move between pending+result"
        );
        let tx_batch = pending_status_set.iter().cloned().collect_vec();
        debug!(
            "Request status for batch of remaining {} transactions in iteration {}",
            tx_batch.len(),
            iteration
        );
        // TODO warn if get_status api calles are slow
        let batch_responses = rpc_client.get_signature_statuses(&tx_batch).await?;
        let elapsed = started_at.elapsed();

        for (tx_sig, status_response) in zip(tx_batch, batch_responses.value) {
            match status_response {
                Some(tx_status) => {
                    trace!(
                        "Some signature status {:?} received for {} after {:.02}ms",
                        tx_status.confirmation_status,
                        tx_sig,
                        elapsed.as_secs_f32() * 1000.0
                    );
                    if !tx_status.satisfies_commitment(CommitmentConfig::confirmed()) {
                        continue 'pooling_loop;
                    }
                    // status is confirmed or finalized
                    pending_status_set.remove(&tx_sig);
                    let prev_value = result_status_map.insert(
                        tx_sig,
                        ConfirmationResponseFromRpc::Success(
                            send_slot,
                            tx_status.slot,
                            tx_status.confirmation_status(),
                            elapsed,
                        ),
                    );
                    assert!(prev_value.is_none(), "Must not override existing value");
                }
                None => {
                    // None: not yet processed by the cluster
                    trace!(
                        "No signature status was received for {} after {:.02}ms - continue waiting",
                        tx_sig,
                        elapsed.as_secs_f32() * 1000.0
                    );
                }
            }
        }

        if pending_status_set.is_empty() {
            info!("All transactions confirmed after {} iterations", iteration);
            break 'pooling_loop;
        }

        if iteration == 100 {
            info!("Timeout waiting for transactions to confirmed after {} iterations - giving up on {}", iteration, pending_status_set.len());
            break 'pooling_loop;
        }
        iteration += 1;

        // avg 2 samples per slot
        tokio::time::sleep_until(iteration_ends_at).await;
    } // -- END polling loop

    let total_time_elapsed_polling = started_at.elapsed();

    // all transactions which remain in pending list are considered timed out
    for tx_sig in pending_status_set.clone() {
        pending_status_set.remove(&tx_sig);
        result_status_map.insert(
            tx_sig,
            ConfirmationResponseFromRpc::Timeout(total_time_elapsed_polling),
        );
    }

    let result_as_vec = batch_sigs_or_fails
        .into_iter()
        .enumerate()
        .map(|(i, sig_or_fail)| match sig_or_fail {
            Ok(tx_sig) => {
                let confirmation = result_status_map
                    .get(&tx_sig)
                    .expect("consistent map with all tx")
                    .clone()
                    .to_owned();
                (tx_sig, confirmation)
            }
            Err(send_error) => {
                let tx_sig = txs[i].get_signature();
                let confirmation = ConfirmationResponseFromRpc::SendError(Arc::new(send_error));
                (*tx_sig, confirmation)
            }
        })
        .collect_vec();

    Ok(result_as_vec)
}

pub async fn poll_next_slot_start(rpc_client: &RpcClient) -> Result<Slot, Error> {
    let started_at = Instant::now();
    let mut last_slot: Option<Slot> = None;
    let mut i = 1;
    // try to catch slot start
    let send_slot = loop {
        if i > 500 {
            bail!("Timeout waiting for slot change");
        }

        let iteration_ends_at = started_at + Duration::from_millis(i * 30);
        let slot = rpc_client
            .get_slot_with_commitment(CommitmentConfig::confirmed())
            .await?;
        trace!("polling slot {}", slot);
        if let Some(last_slot) = last_slot {
            if last_slot + 1 == slot {
                break slot;
            }
        }
        last_slot = Some(slot);
        tokio::time::sleep_until(iteration_ends_at).await;
        i += 1;
    };
    Ok(send_slot)
}
