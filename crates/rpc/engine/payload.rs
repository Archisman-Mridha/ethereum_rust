use ethereum_rust_chain::error::ChainError;
use ethereum_rust_chain::{add_block, latest_valid_hash};
use ethereum_rust_core::types::ForkId;
use ethereum_rust_core::H256;
use ethereum_rust_storage::Store;
use serde_json::Value;
use tracing::{info, warn};

use crate::{
    types::payload::{ExecutionPayloadV3, PayloadStatus},
    RpcErr,
};

pub struct NewPayloadV3Request {
    pub payload: ExecutionPayloadV3,
    pub expected_blob_versioned_hashes: Vec<H256>,
    pub parent_beacon_block_root: H256,
}

impl NewPayloadV3Request {
    pub fn parse(params: &Option<Vec<Value>>) -> Result<NewPayloadV3Request, RpcErr> {
        let params = params.as_ref().ok_or(RpcErr::BadParams)?;
        if params.len() != 3 {
            return Err(RpcErr::BadParams);
        }
        Ok(NewPayloadV3Request {
            payload: serde_json::from_value(params[0].clone())?,
            expected_blob_versioned_hashes: serde_json::from_value(params[1].clone())?,
            parent_beacon_block_root: serde_json::from_value(params[2].clone())?,
        })
    }
}

pub fn new_payload_v3(
    request: NewPayloadV3Request,
    storage: Store,
) -> Result<PayloadStatus, RpcErr> {
    let block_hash = request.payload.block_hash;
    info!("Received new payload with block hash: {block_hash}");

    let block = match request.payload.into_block(request.parent_beacon_block_root) {
        Ok(block) => block,
        Err(error) => return Ok(PayloadStatus::invalid_with_err(&error.to_string())),
    };

    // Payload Validation

    // Check timestamp does not fall within the time frame of the Cancun fork
    let chain_config = storage.get_chain_config()?;
    if chain_config.get_fork(block.header.timestamp) < ForkId::Cancun {
        return Err(RpcErr::UnsuportedFork);
    }

    // Check that block_hash is valid
    let actual_block_hash = block.header.compute_block_hash();
    if block_hash != actual_block_hash {
        return Ok(PayloadStatus::invalid_with_err("Invalid block hash"));
    }
    info!("Block hash {block_hash} is valid");
    // Concatenate blob versioned hashes lists (tx.blob_versioned_hashes) of each blob transaction included in the payload, respecting the order of inclusion
    // and check that the resulting array matches expected_blob_versioned_hashes
    let blob_versioned_hashes: Vec<H256> = block
        .body
        .transactions
        .iter()
        .flat_map(|tx| tx.blob_versioned_hashes())
        .collect();
    if request.expected_blob_versioned_hashes != blob_versioned_hashes {
        return Ok(PayloadStatus::invalid_with_err(
            "Invalid blob_versioned_hashes",
        ));
    }
    // Check that the incoming block extends the current chain
    let last_block_number = storage.get_latest_block_number()?.ok_or(RpcErr::Internal)?;
    if block.header.number <= last_block_number {
        // Check if we already have this block stored
        if storage
            .get_block_number(block_hash)
            .map_err(|_| RpcErr::Internal)?
            .is_some_and(|num| num == block.header.number)
        {
            return Ok(PayloadStatus::valid_with_hash(block_hash));
        }
        warn!("Should start reorg but it is not supported yet");
        return Err(RpcErr::Internal);
    } else if block.header.number != last_block_number + 1 {
        return Ok(PayloadStatus::syncing());
    }

    let latest_valid_hash = latest_valid_hash(&storage).map_err(|_| RpcErr::Internal)?;

    // Execute and store the block
    info!("Executing payload with block hash: {block_hash}");
    match add_block(&block, &storage) {
        Err(ChainError::NonCanonicalBlock) => Ok(PayloadStatus::syncing()),
        Err(ChainError::ParentNotFound) => Ok(PayloadStatus::invalid_with_err(
            "Could not reference parent block with parent_hash",
        )),
        Err(ChainError::InvalidBlock(_)) => Ok(PayloadStatus::invalid_with_hash(latest_valid_hash)),
        Err(ChainError::EvmError(error)) => Ok(PayloadStatus::invalid_with_err(&error.to_string())),
        Err(ChainError::StoreError(_)) => Err(RpcErr::Internal),
        Ok(()) => {
            info!("Block with hash {block_hash} executed succesfully");
            info!("Block with hash {block_hash} added to storage");

            Ok(PayloadStatus::valid_with_hash(block_hash))
        }
    }
}
