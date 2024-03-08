use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::time::Instant;

use crate::block_stores::postgres::LITERPC_QUERY_ROLE;
use anyhow::{anyhow, bail, Context, Result};
use itertools::Itertools;
use log::{debug, info, warn};
use solana_lite_rpc_core::structures::epoch::EpochRef;
use solana_lite_rpc_core::structures::{epoch::EpochCache, produced_block::ProducedBlock};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::slot_history::Slot;

use super::postgres_block::*;
use super::postgres_config::*;
use super::postgres_epoch::*;
use super::postgres_session::*;
use super::postgres_transaction::*;


// no clone/sync/send
pub struct PostgresQueryBlockStore {
    session: PostgresSession,
    epoch_schedule: EpochCache,
}

impl PostgresQueryBlockStore {
    pub async fn new(
        epoch_schedule: EpochCache,
        pg_session_config: BlockstorePostgresSessionConfig,
    ) -> Self {
        // let session_cache = PostgresSessionCache::new(pg_session_config.clone())
        //     .await
        //     .unwrap();
        let session = PostgresSession::new(pg_session_config.clone()).await.unwrap();

        Self::check_postgresql_version(&session).await;
        Self::check_query_role(&session).await;

        Self {
            session,
            epoch_schedule,
        }
    }

    // async fn get_session(&self) -> PostgresSession {
    //     self.session_cache
    //         .get_session()
    //         .await
    // }

    pub async fn is_block_in_range(&self, slot: Slot) -> bool {
        let epoch = self.epoch_schedule.get_epoch_at_slot(slot);
        let ranges = self.get_slot_range_by_epoch().await;
        let matching_range: Option<&RangeInclusive<Slot>> = ranges.get(&epoch.into());

        matching_range
            .map(|slot_range| slot_range.contains(&slot))
            .is_some()
    }

    pub async fn query_block(&self, slot: Slot) -> Result<ProducedBlock> {
        let started_at = Instant::now();
        let epoch: EpochRef = self.epoch_schedule.get_epoch_at_slot(slot).into();

        // TODO could we use join! here?
        let statement = PostgresBlock::build_query_statement(epoch, slot);
        let row = self.session
            .query_opt(&statement, &[]).await
            .context("query block sql")?
            .context("block not found")?;

        let statement = PostgresTransaction::build_query_statement(epoch, slot);
        let transaction_rows = self.session
            .query_list(&statement, &[])
            .await?;

        warn!(
            "transaction_rows: {} - print first 10",
            transaction_rows.len()
        );

        let tx_infos = transaction_rows
            .iter()
            // TODO check why we map to PostgresTransaction and then to TransactionInfo
            .map(|tx_row| {
                PostgresTransaction {
                    slot: slot as i64,
                    idx_in_block: tx_row.get("idx"),
                    signature: tx_row.get("signature"),
                    err: tx_row.get("err"),
                    cu_requested: tx_row.get("cu_requested"),
                    prioritization_fees: tx_row.get("prioritization_fees"),
                    cu_consumed: tx_row.get("cu_consumed"),
                    recent_blockhash: tx_row.get("recent_blockhash"),
                    message_version: tx_row.get("message_version"),
                    message: tx_row.get("message"),
                }
            })
            .sorted_by(|a, b| a.idx_in_block.cmp(&b.idx_in_block))
            .map(|pt| pt.to_transaction_info())
            .collect_vec();


        // meta data
        let _epoch: i64 = row.get("_epoch");
        let epoch_schema: String = row.get("_epoch_schema");
        let blockhash: String = row.get("blockhash");
        let block_height: i64 = row.get("block_height");
        let slot: i64 = row.get("slot");
        let parent_slot: i64 = row.get("parent_slot");
        let block_time: i64 = row.get("block_time");
        let previous_blockhash: String = row.get("previous_blockhash");
        let rewards: Option<String> = row.get("rewards");
        let leader_id: Option<String> = row.get("leader_id");

        let postgres_block = PostgresBlock {
            slot,
            blockhash,
            block_height,
            parent_slot,
            block_time,
            previous_blockhash,
            rewards,
            leader_id,
        };

        let produced_block = postgres_block.to_produced_block(
            tx_infos,
            // FIXME
            CommitmentConfig::confirmed(),
        );

        debug!(
            "Querying produced block {} from postgres in epoch schema {} took {:.2}ms: {}/{}",
            produced_block.slot,
            epoch_schema,
            started_at.elapsed().as_secs_f64() * 1000.0,
            produced_block.blockhash,
            produced_block.commitment_config.commitment
        );

        Ok(produced_block)
    }

    async fn check_query_role(session: &PostgresSession) {
        let role = LITERPC_QUERY_ROLE;
        let statement = format!("SELECT 1 FROM pg_roles WHERE rolname='{role}'");
        let count = session
            .execute(&statement, &[])
            .await
            .expect("must execute query to check for role");

        if count == 0 {
            panic!(
                "Missing mandatory postgres query role '{}' for Lite RPC - see permissions.sql",
                role
            );
        } else {
            info!("Self check - found postgres role '{}'", role);
        }
    }

    async fn check_postgresql_version(session: &PostgresSession) {
        let statement = r#"SELECT version(), current_setting('server_version_num')::integer >= 150000 AS is_v15_or_higher"#;
        let row = session
            .query_one(statement, &[])
            .await
            .expect("must execute query to check for role");
        let is_v15_or_higher = row.get::<&str, bool>("is_v15_or_higher");
        let version_string = row.get::<&str, &str>("version");
        assert!(
            is_v15_or_higher,
            "Postgres version must be 15 or higher, found: {}",
            version_string
        );
        info!("Self check - found postgres version: {}", version_string);
    }
}

impl PostgresQueryBlockStore {
    pub async fn get_slot_range(&self) -> RangeInclusive<Slot> {
        let map_epoch_to_slot_range = self.get_slot_range_by_epoch().await;

        let rows_minmax: Vec<&RangeInclusive<Slot>> =
            map_epoch_to_slot_range.values().collect_vec();

        let slot_min = rows_minmax
            .iter()
            .map(|range| range.start())
            .min()
            // TODO decide what todo
            .expect("non-empty result - TODO");
        let slot_max = rows_minmax
            .iter()
            .map(|range| range.end())
            .max()
            .expect("non-empty result - TODO");

        RangeInclusive::new(*slot_min, *slot_max)
    }

    pub async fn get_slot_range_by_epoch(&self) -> HashMap<EpochRef, RangeInclusive<Slot>> {
        let started = Instant::now();
        // let session = self.get_session().await;
        // e.g. "rpc2a_epoch_552"
        let query = format!(
            r#"
                SELECT
                 schema_name
                FROM information_schema.schemata
                WHERE schema_name ~ '^{schema_prefix}[0-9]+$'
            "#,
            schema_prefix = EPOCH_SCHEMA_PREFIX
        );
        let result = self.session.query_list(&query, &[]).await.unwrap();

        let epoch_schemas = result
            .iter()
            .map(|row| row.get::<&str, &str>("schema_name"))
            .map(|schema_name| {
                (
                    schema_name,
                    PostgresEpoch::parse_epoch_from_schema_name(schema_name),
                )
            })
            .collect_vec();

        if epoch_schemas.is_empty() {
            return HashMap::new();
        }

        let inner = epoch_schemas
            .iter()
            .map(|(schema, epoch)| {
                format!(
                    "SELECT slot,{epoch}::bigint as epoch FROM {schema}.blocks",
                    schema = schema,
                    epoch = epoch
                )
            })
            .join(" UNION ALL ");

        let query = format!(
            r#"
                SELECT epoch, min(slot) as slot_min, max(slot) as slot_max FROM (
                    {inner}
                ) AS all_slots
                GROUP BY epoch
            "#,
            inner = inner
        );

        let rows_minmax = self.session.query_list(&query, &[]).await;

        match rows_minmax {
            Ok(ref rows_minmax) => {
                if rows_minmax.is_empty() {
                    return HashMap::new();
                }
            }
            Err(err) => {
                warn!("Skipping query slot range by epoch: {:?}", err);
                return HashMap::new();
            }
        };

        let rows_minmax = rows_minmax.unwrap();

        let mut map_epoch_to_slot_range = rows_minmax
            .iter()
            .map(|row| {
                (
                    row.get::<&str, i64>("epoch"),
                    RangeInclusive::new(
                        row.get::<&str, i64>("slot_min") as Slot,
                        row.get::<&str, i64>("slot_max") as Slot,
                    ),
                )
            })
            .into_grouping_map()
            .fold(None, |acc, _key, val| {
                assert!(acc.is_none(), "epoch must be unique");
                Some(val)
            });

        let final_range: HashMap<EpochRef, RangeInclusive<Slot>> = map_epoch_to_slot_range
            .iter_mut()
            .map(|(epoch, range)| {
                let epoch = EpochRef::new(*epoch as u64);
                (
                    epoch,
                    range.clone().expect("range must be returned from SQL"),
                )
            })
            .collect();

        debug!(
            "Slot range check in postgres found {} ranges, took {:2}sec: {:?}",
            rows_minmax.len(),
            started.elapsed().as_secs_f64(),
            final_range
        );

        final_range
    }
}
