use crate::common::{
    chain::Chain, 
    ens::NameOrAddress, 
    entity_id::EntityId, 
    query_result::AccountQueryRes, 
    types::AccountField
};
use std::error::Error;
use alloy::{
    primitives::Address, 
    providers::{Provider, ProviderBuilder, RootProvider}, 
    transports::http::{Client, Http}
};
use futures::future::try_join_all;
use serde::{Deserialize, Serialize};


#[derive(Debug, Serialize, Deserialize, thiserror::Error)]
pub enum AccountResolverErrors {
    // #[error("Invalid address")]
    // InvalidAddress,
    #[error("Mismatch between Entity and EntityId")]
    MismatchEntityAndEntityId,
    #[error("Unable resolve ENS name")]
    EnsResolution,
}

pub async fn resolve_account_query(
    entity_ids: Vec<EntityId>, 
    fields: Vec<AccountField>,
    provider: &RootProvider<Http<Client>>,
) -> Result<Vec<AccountQueryRes>, Box<dyn Error>> {
    // Create a vector to store individual futures, for each request.
    let mut account_futures = Vec::new();
    // Iterate through entity_ids and map them to futures.
    for entity_id in entity_ids {
        let fields = fields.clone(); // Clone fields for each async block.
        let provider = provider.clone(); // Clone the provider if necessary, ensure it's Send + Sync.
        let account_future = async move {
        
            match entity_id {
                EntityId::Account(name_or_address) => { 
                    let address = to_address(name_or_address).await?;
                    get_account(address, fields, &provider).await
                },
                // Ensure all entity IDs are of the variant EntityId::Account
                _ => Err(Box::new(AccountResolverErrors::MismatchEntityAndEntityId).into()),
            }
        };

    // Add the future to the list.
    account_futures.push(account_future);
    }

    // Execute all futures concurrently and collect the results.
    let account_res = try_join_all(account_futures).await?;
    Ok(account_res)
}

async fn to_address(name_or_address: NameOrAddress) -> Result<Address, AccountResolverErrors> {
    match &name_or_address {
        NameOrAddress::Address(address) => Ok(*address),
        NameOrAddress::Name(_) => {
            let rpc_url = Chain::Ethereum
                .rpc_url()
                .parse()
                .map_err(|_| AccountResolverErrors::EnsResolution)?;

            let provider = ProviderBuilder::new().on_http(rpc_url);

            let address = name_or_address
                .resolve(&provider)
                .await
                .map_err(|_| AccountResolverErrors::EnsResolution)?;

            Ok(address)
        }
    }
}

async fn get_account(
    address: Address,
    fields: Vec<AccountField>,
    provider: &RootProvider<Http<Client>>,
) -> Result<AccountQueryRes, Box<dyn Error>> {
    let mut account = AccountQueryRes::default();

    for field in &fields {
        match field {
            AccountField::Balance => {
                account.balance = Some(provider.get_balance(address).await?);
            }
            AccountField::Nonce => {
                account.nonce = Some(provider.get_transaction_count(address).await?);
            }
            AccountField::Address => {
                account.address = Some(address);
            }
            AccountField::Code => {
                account.code = Some(provider.get_code_at(address).await?);
            }
        }
    }

    Ok(account)
}