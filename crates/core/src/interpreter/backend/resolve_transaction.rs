use super::resolve_block::{batch_get_blocks, get_block};
use crate::common::{
    block::BlockId,
    chain::ChainOrRpc,
    query_result::TransactionQueryRes,
    transaction::{Transaction, TransactionField},
};
use alloy::{
    primitives::FixedBytes,
    providers::{Provider, ProviderBuilder, RootProvider},
    rpc::types::{BlockTransactions, Transaction as RpcTransaction},
    transports::http::{Client, Http},
};
use anyhow::{Ok, Result};
use futures::future::try_join_all;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, thiserror::Error)]
pub enum TransactionResolverErrors {
    #[error("Mismatch between Entity and EntityId, {0} can't be resolved as a transaction id")]
    MismatchEntityAndEntityId(String),
    #[error("Query should either provide tx hash or block number/range filter")]
    MissingTransactionHashOrFilter,
}

/// Resolve the query to get transactions after receiving an transaction entity expression
/// Iterate through entity_ids and map them to a futures list. Execute all futures concurrently and collect the results.
/// The sequence of steps to fetch transactions is:
/// 1. Check if ids are provided.
/// 2. If ids are provided, fetch the transactions.
/// 3. If ids are not provided, fetch the transactions by block number.
/// 4. If ids are not provided, then block number or block range filter must be provided.
/// 5. Fetch the transactions by block number or block range.
/// 6. If both ids and block number or block range filter are provided, then fetch the transactions by ids first, and filter the result by block number or block range.
pub async fn resolve_transaction_query(
    transaction: &Transaction,
    chains: &[ChainOrRpc],
) -> Result<Vec<TransactionQueryRes>> {
    if !transaction.ids().is_some() && !transaction.has_block_filter() {
        return Err(TransactionResolverErrors::MissingTransactionHashOrFilter.into());
    }

    let mut all_results = Vec::new();

    for chain in chains {
        let provider = Arc::new(ProviderBuilder::new().on_http(chain.rpc_url()?));

        // Fetch transactions for this chain
        let rpc_transactions = match transaction.ids() {
            Some(ids) => get_transactions_by_ids(ids, &provider).await?,
            None => {
                let block_id = transaction.get_block_id_filter()?;
                get_transactions_by_block_id(block_id, &provider).await?
            }
        };

        let result_futures = rpc_transactions
            .iter()
            .map(|t| pick_transaction_fields(t, transaction.fields(), &provider, chain));
        let tx_res = try_join_all(result_futures).await?;

        // Filter and collect results for this chain
        let filtered_tx_res: Vec<TransactionQueryRes> = tx_res
            .into_iter()
            .filter(|t| transaction.filter(t))
            .collect();

        all_results.extend(filtered_tx_res);
    }

    Ok(all_results)
}

async fn get_transactions_by_ids(
    ids: &Vec<FixedBytes<32>>,
    provider: &RootProvider<Http<Client>>,
) -> Result<Vec<RpcTransaction>> {
    let mut tx_futures = Vec::new();
    for id in ids {
        let provider = provider.clone();
        let tx_future = async move { provider.get_transaction_by_hash(*id).await };

        tx_futures.push(tx_future);
    }

    let tx_res = try_join_all(tx_futures).await?;

    Ok(tx_res.into_iter().filter_map(|t| t).collect())
}

async fn get_transactions_by_block_id(
    block_id: &BlockId,
    provider: &Arc<RootProvider<Http<Client>>>,
) -> Result<Vec<RpcTransaction>> {
    match block_id {
        BlockId::Number(n) => {
            let block = get_block(n.clone(), provider.clone(), true).await?;
            match &block.transactions {
                BlockTransactions::Full(txs) => Ok(txs.clone()),
                _ => panic!("Block transactions should be full"),
            }
        }
        BlockId::Range(r) => {
            let block_numbers = r.resolve_block_numbers(provider).await?;
            let blocks = batch_get_blocks(block_numbers, provider, true).await?;
            let txs = blocks
                .iter()
                .flat_map(|b| match &b.transactions {
                    BlockTransactions::Full(txs) => txs.clone(),
                    _ => panic!("Block transactions should be full"),
                })
                .collect::<Vec<_>>();

            Ok(txs)
        }
    }
}

async fn pick_transaction_fields(
    tx: &RpcTransaction,
    fields: &Vec<TransactionField>,
    provider: &Arc<RootProvider<Http<Client>>>,
    chain: &ChainOrRpc,
) -> Result<TransactionQueryRes> {
    let mut result = TransactionQueryRes::default();
    let chain = chain.to_chain().await?;

    for field in fields {
        match field {
            TransactionField::TransactionType => {
                result.transaction_type = tx.transaction_type;
            }
            TransactionField::Hash => {
                result.hash = Some(tx.hash);
            }
            TransactionField::From => {
                result.from = Some(tx.from);
            }
            TransactionField::To => {
                result.to = tx.to;
            }
            TransactionField::Data => {
                result.data = Some(tx.input.clone());
            }
            TransactionField::Value => {
                result.value = Some(tx.value);
            }
            TransactionField::GasPrice => {
                result.gas_price = tx.gas_price;
            }
            TransactionField::Gas => {
                result.gas = Some(tx.gas);
            }
            TransactionField::Status => match provider.get_transaction_receipt(tx.hash).await? {
                Some(receipt) => {
                    result.status = Some(receipt.status());
                }
                None => {
                    result.status = None;
                }
            },
            TransactionField::ChainId => {
                result.chain_id = tx.chain_id;
            }
            TransactionField::V => {
                result.v = tx.signature.map_or(None, |s| Some(s.v));
            }
            TransactionField::R => {
                result.r = tx.signature.map_or(None, |s| Some(s.r));
            }
            TransactionField::S => {
                result.s = tx.signature.map_or(None, |s| Some(s.s));
            }
            TransactionField::MaxFeePerBlobGas => {
                result.max_fee_per_blob_gas = tx.max_fee_per_blob_gas;
            }
            TransactionField::MaxFeePerGas => {
                result.max_fee_per_gas = tx.max_fee_per_gas;
            }
            TransactionField::MaxPriorityFeePerGas => {
                result.max_priority_fee_per_gas = tx.max_priority_fee_per_gas;
            }
            TransactionField::YParity => {
                result.y_parity = tx
                    .signature
                    .map_or(None, |s| s.y_parity)
                    .map_or(None, |y| Some(y.0));
            }
            TransactionField::Chain => {
                result.chain = Some(chain.clone());
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{
        block::BlockRange,
        chain::Chain,
        filters::{ComparisonFilter, EqualityFilter, FilterType},
        transaction::TransactionFilter,
    };
    use alloy::{
        eips::BlockNumberOrTag,
        primitives::{address, U256},
        providers::ProviderBuilder,
    };

    #[tokio::test]
    async fn test_get_transactions_by_block_range() {
        let rpc = Chain::Ethereum.rpc_url().unwrap();
        let provider = Arc::new(ProviderBuilder::new().on_http(rpc));
        let block_id = BlockId::Range(BlockRange::new(10000000.into(), Some(10000015.into())));
        let transactions = get_transactions_by_block_id(&block_id, &provider)
            .await
            .unwrap();

        assert_eq!(transactions.len(), 2394);
    }

    #[tokio::test]
    async fn test_get_transactions_by_block_number() {
        let rpc = Chain::Ethereum.rpc_url().unwrap();
        let provider = Arc::new(ProviderBuilder::new().on_http(rpc));
        let block_id = BlockId::Number(BlockNumberOrTag::Number(21036202));
        let transactions = get_transactions_by_block_id(&block_id, &provider)
            .await
            .unwrap();

        assert_eq!(transactions.len(), 177);
    }

    #[tokio::test]
    async fn test_resolve_query_using_block_range_filter() {
        let rpc = Chain::Ethereum.rpc_url().unwrap();
        let chain = ChainOrRpc::Rpc(rpc);
        let block_id = BlockId::Range(BlockRange::new(10000000.into(), Some(10000001.into())));
        let transaction = Transaction::new(
            None,
            Some(vec![TransactionFilter::BlockId(block_id)]),
            TransactionField::all_variants().to_vec(),
        );

        let transactions = resolve_transaction_query(&transaction, &[chain])
            .await
            .unwrap();

        assert_eq!(transactions.len(), 211);
    }

    #[tokio::test]
    async fn test_resolve_query_using_filters() {
        let value = "1000000000000000".parse::<U256>().unwrap();
        let from = address!("BF2EFaA8715d75AfC562Cde29f56B55aA0Fb219F");
        let to = address!("3fE873889008521bf335E07CEAfdfd0D9a6864A8");
        let gas = 22000;
        let gas_price = 5000000000;
        let status = true;

        let chain = ChainOrRpc::Chain(Chain::Ethereum);
        let block_id = BlockId::Range(BlockRange::new(10000004.into(), None));
        let transaction = Transaction::new(
            None,
            Some(vec![
                TransactionFilter::BlockId(block_id),
                TransactionFilter::Value(FilterType::Comparison(ComparisonFilter::Lte(value))),
                TransactionFilter::From(EqualityFilter::Eq(from)),
                TransactionFilter::To(EqualityFilter::Eq(to)),
                TransactionFilter::Gas(FilterType::Comparison(ComparisonFilter::Lte(gas))),
                TransactionFilter::GasPrice(FilterType::Comparison(ComparisonFilter::Lte(
                    gas_price,
                ))),
                TransactionFilter::Status(EqualityFilter::Eq(status)),
            ]),
            TransactionField::all_variants().to_vec(),
        );

        let transactions = resolve_transaction_query(&transaction, &[chain])
            .await
            .unwrap();

        let tx = transactions.first().unwrap();
        let expected_tx: TransactionQueryRes = TransactionQueryRes {
            value: Some(value),
            from: Some(from),
            to: Some(to),
            gas: Some(gas),
            gas_price: Some(gas_price),
            status: Some(status),
            ..Default::default()
        };

        assert_eq!(transactions.len(), 1);
        assert_eq!(tx.value, expected_tx.value);
        assert_eq!(tx.from, expected_tx.from);
        assert_eq!(tx.to, expected_tx.to);
        assert_eq!(tx.gas, expected_tx.gas);
        assert_eq!(tx.gas_price, expected_tx.gas_price);
        assert_eq!(tx.status, expected_tx.status);
    }
}
