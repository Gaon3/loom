use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::ops::Deref;
use std::sync::Arc;

use alloy_consensus::TxEnvelope;
use alloy_primitives::{Address, BlockNumber, Bytes, TxHash, U256};
use alloy_primitives::private::alloy_rlp;
use alloy_rlp::Encodable;
use alloy_rpc_types::{Transaction, TransactionRequest};
use eyre::{eyre, Result};
use eyre::OptionExt;
use revm::InMemoryDB;

use defi_entities::{SwapLine, SwapStep, Token, TxSigner};
use defi_types::{GethStateUpdateVec, Opcodes};

use crate::Message;

#[derive(Clone, Debug)]
pub enum TxState {
    Stuffing(Transaction),
    SignatureRequired(TransactionRequest),
    ReadyForBroadcast(Bytes),
    ReadyForBroadcastStuffing(Bytes),
}

impl TxState {
    pub fn rlp(&self) -> Result<Bytes> {
        match self {
            TxState::Stuffing(t) => {
                let mut r: Vec<u8> = Vec::new();
                let tenv: TxEnvelope = t.clone().try_into()?;
                tenv.encode(&mut r);
                Ok(Bytes::from(r))
            }
            TxState::ReadyForBroadcast(t) | TxState::ReadyForBroadcastStuffing(t) => Ok(t.clone()),
            _ => Err(eyre!("NOT_READY_FOR_BROADCAST"))
        }
    }
}


#[derive(Clone, Debug)]
pub enum SwapType {
    None,
    BackrunSwapSteps((SwapStep, SwapStep)),
    BackrunSwapLine(SwapLine),
    Multiple(Vec<SwapType>),
}

impl Display for SwapType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SwapType::BackrunSwapLine(path) => write!(f, "{path}"),
            SwapType::BackrunSwapSteps((sp0, sp1)) => write!(f, "{sp0} {sp1}"),
            SwapType::Multiple(_) => write!(f, "MULTIPLE_SWAP"),
            SwapType::None => write!(f, "UNKNOWN_SWAP_TYPE")
        }
    }
}

impl SwapType {
    pub fn abs_profit(&self) -> U256 {
        match self {
            SwapType::BackrunSwapLine(path) => {
                path.abs_profit()
            }
            SwapType::BackrunSwapSteps((sp0, sp1)) => {
                SwapStep::abs_profit(sp0, sp1)
            }
            SwapType::Multiple(swap_vec) => {
                swap_vec.iter().map(|x| x.abs_profit()).sum()
            }
            SwapType::None => {
                U256::ZERO
            }
        }
    }

    pub fn pre_estimate_gas(&self) -> u64 {
        match self {
            SwapType::BackrunSwapLine(path) => {
                path.gas_used.unwrap_or_default()
            }
            SwapType::BackrunSwapSteps((sp0, sp1)) => {
                sp0.swap_line_vec().iter().map(|i| i.gas_used.unwrap_or_default()).sum::<u64>() + sp1.swap_line_vec().iter().map(|i| i.gas_used.unwrap_or_default()).sum::<u64>()
            }
            SwapType::Multiple(swap_vec) => {
                swap_vec.iter().map(|x| x.pre_estimate_gas()).sum()
            }
            SwapType::None => {
                0
            }
        }
    }

    pub fn abs_profit_eth(&self) -> U256 {
        match self {
            SwapType::BackrunSwapLine(path) => {
                path.abs_profit_eth()
            }
            SwapType::BackrunSwapSteps((sp0, sp1)) => {
                SwapStep::abs_profit_eth(sp0, sp1)
            }
            SwapType::Multiple(swap_vec) => {
                swap_vec.iter().map(|x| x.abs_profit_eth()).sum()
            }
            SwapType::None => {
                U256::ZERO
            }
        }
    }

    pub fn get_first_token(&self) -> Option<&Arc<Token>> {
        match self {
            SwapType::BackrunSwapLine(swap_path) => {
                swap_path.get_first_token()
            }
            SwapType::BackrunSwapSteps((sp0, _sp1)) => {
                sp0.get_first_token()
            }
            SwapType::Multiple(_) => None,
            SwapType::None => None
        }
    }

    pub fn get_first_tokens(&self) -> Result<Vec<&Arc<Token>>> {
        match self {
            SwapType::BackrunSwapLine(swap_path) => {
                vec![swap_path.get_first_token().ok_or_eyre("NO_FIRST_TOKEN")].into_iter().collect()
            }
            SwapType::BackrunSwapSteps((sp0, _sp1)) => {
                vec![sp0.get_first_token().ok_or_eyre("NO_FIRST_TOKEN")].into_iter().collect()
            }
            SwapType::Multiple(s) => {
                let mut seen = HashSet::new();
                s.iter().filter(|x| {
                    let t = x.get_first_token();
                    t.is_some() && seen.insert(t.unwrap())
                }).map(|x| x.get_first_token().ok_or_eyre("x")).collect()
            }
            SwapType::None => Err(eyre!("NOT_SUPPORTED_SWAP_TYPE"))
        }
    }

    pub fn get_pool_address_vec(&self) -> Vec<Address> {
        match self {
            SwapType::BackrunSwapSteps((sp0, _sp1)) => {
                sp0.swap_line_vec().iter().flat_map(|item| item.pools().iter().map(|p| p.get_address()).collect::<Vec<_>>()).collect()
            }
            SwapType::BackrunSwapLine(swap_line) => {
                swap_line.pools().iter().map(|item| item.get_address()).collect()
            }
            SwapType::Multiple(swap_vec) => {
                swap_vec.iter().flat_map(|x| x.get_pool_address_vec()).collect()
            }
            SwapType::None => Vec::new()
        }
    }
}

#[derive(Clone, Debug)]
pub enum TxCompose {
    Encode(TxComposeData),
    Estimate(TxComposeData),
    Sign(TxComposeData),
    Broadcast(TxComposeData),
}

impl Deref for TxCompose {
    type Target = TxComposeData;

    fn deref(&self) -> &Self::Target {
        self.data()
    }
}

impl TxCompose {
    pub fn data(&self) -> &TxComposeData {
        match self {
            TxCompose::Encode(x) | TxCompose::Broadcast(x) | TxCompose::Sign(x) | TxCompose::Estimate(x) => { x }
        }
    }
}

#[derive(Debug, Clone)]
pub enum RlpState {
    Stuffing(Bytes),
    Backrun(Bytes),
    None,
}

impl RlpState {
    pub fn is_none(&self) -> bool {
        match self {
            RlpState::None => true,
            _ => false
        }
    }

    pub fn unwrap(&self) -> Bytes {
        match self.clone() {
            RlpState::Backrun(val) | RlpState::Stuffing(val) => { val }
            RlpState::None => Bytes::new()
        }
    }
}

#[derive(Clone, Debug)]
pub struct TxComposeData {
    pub signer: Option<TxSigner>,
    pub nonce: u64,
    pub eth_balance: U256,
    pub value: U256,
    pub gas: u128,
    pub gas_fee: u128,
    pub priority_gas_fee: u128,
    pub stuffing_txs_hashes: Vec<TxHash>,
    pub stuffing_txs: Vec<Transaction>,
    pub block: BlockNumber,
    pub block_timestamp: u64,
    pub swap: SwapType,
    pub opcodes: Option<Opcodes>,
    pub tx_bundle: Option<Vec<TxState>>,
    pub rlp_bundle: Option<Vec<RlpState>>,
    pub prestate: Option<Arc<InMemoryDB>>,
    pub poststate: Option<Arc<InMemoryDB>>,
    pub poststate_update: Option<GethStateUpdateVec>,
    pub origin: Option<String>,
    pub tips_pct: Option<u32>,
    pub tips: Option<U256>,
}


impl TxComposeData {
    pub fn same_stuffing(&self, others_stuffing_txs_hashes: &Vec<TxHash>) -> bool {
        let tx_len = self.stuffing_txs_hashes.len();

        if tx_len != others_stuffing_txs_hashes.len() {
            false
        } else {
            if tx_len == 0 {
                true
            } else {
                others_stuffing_txs_hashes.iter().all(|x| self.stuffing_txs_hashes.contains(x))
            }
        }
    }

    pub fn cross_pools(&self, others_pools: &Vec<Address>) -> bool {
        self.swap.get_pool_address_vec().iter().any(|x| others_pools.contains(x))
    }


    pub fn first_stuffing_hash(&self) -> TxHash {
        self.stuffing_txs_hashes.first().map_or(TxHash::default(), |x| *x)
    }

    pub fn tips_gas_ratio(&self) -> U256 {
        if self.gas == 0 {
            U256::ZERO
        } else {
            self.tips.unwrap_or_default() / U256::from(self.gas)
        }
    }

    pub fn profit_eth_gas_ratio(&self) -> U256 {
        if self.gas == 0 {
            U256::ZERO
        } else {
            self.swap.abs_profit_eth() / U256::from(self.gas)
        }
    }
}

impl Default for TxComposeData {
    fn default() -> Self {
        Self {
            signer: None,
            nonce: Default::default(),
            eth_balance: Default::default(),
            gas_fee: Default::default(),
            value: Default::default(),
            gas: Default::default(),
            priority_gas_fee: Default::default(),
            stuffing_txs_hashes: Vec::new(),
            stuffing_txs: Vec::new(),
            block: Default::default(),
            block_timestamp: Default::default(),
            swap: SwapType::None,
            opcodes: None,
            tx_bundle: None,
            rlp_bundle: None,
            prestate: None,
            poststate: None,
            poststate_update: None,
            origin: None,
            tips_pct: None,
            tips: None,
        }
    }
}


pub struct TxComposeBest {
    validity_pct: Option<U256>,
    best_profit_swap: Option<TxComposeData>,
    best_profit_gas_ratio_swap: Option<TxComposeData>,
    best_tips_swap: Option<TxComposeData>,
    best_tips_gas_ratio_swap: Option<TxComposeData>,

}

impl Default for TxComposeBest {
    fn default() -> Self {
        Self {
            validity_pct: None,
            best_profit_swap: None,
            best_profit_gas_ratio_swap: None,
            best_tips_swap: None,
            best_tips_gas_ratio_swap: None,
        }
    }
}

impl TxComposeBest {
    pub fn new_with_pct<T: Into<U256>>(validity_pct: T) -> Self {
        TxComposeBest {
            validity_pct: Some(validity_pct.into()),
            ..Default::default()
        }
    }

    pub fn check(&mut self, request: &TxComposeData) -> bool {
        let mut is_ok = false;

        match &self.best_profit_swap {
            None => {
                self.best_profit_swap = Some(request.clone());
                is_ok = true;
            }
            Some(best_swap) => {
                if best_swap.swap.abs_profit_eth() < request.swap.abs_profit_eth() {
                    self.best_profit_swap = Some(request.clone());
                    is_ok = true;
                } else {
                    match self.validity_pct {
                        Some(pct) => {
                            if (best_swap.swap.abs_profit_eth() * pct) / U256::from(10000) < request.swap.abs_profit_eth() {
                                is_ok = true
                            }
                        }
                        None => {}
                    }
                }
            }
        }

        if request.tips.is_some() {
            match &self.best_tips_swap {
                Some(best_swap) => {
                    if best_swap.tips.unwrap_or_default() < request.tips.unwrap_or_default() {
                        self.best_tips_swap = Some(request.clone());
                        is_ok = true;
                    } else {
                        match self.validity_pct {
                            Some(pct) => {
                                if (best_swap.tips.unwrap_or_default() * pct) / U256::from(10000) < request.tips.unwrap_or_default() {
                                    is_ok = true
                                }
                            }
                            None => {}
                        }
                    }
                }
                None => {
                    self.best_tips_swap = Some(request.clone());
                    is_ok = true;
                }
            }
        }


        if request.gas != 0 {
            match &self.best_tips_gas_ratio_swap {
                Some(best_swap) => {
                    if best_swap.tips_gas_ratio() < request.tips_gas_ratio() {
                        self.best_tips_gas_ratio_swap = Some(request.clone());
                        is_ok = true;
                    } else {
                        match self.validity_pct {
                            Some(pct) => {
                                if (best_swap.tips_gas_ratio() * pct) / U256::from(10000) < request.tips_gas_ratio() {
                                    is_ok = true
                                }
                            }
                            None => {}
                        }
                    }
                }
                None => {
                    self.best_tips_gas_ratio_swap = Some(request.clone());
                    is_ok = true;
                }
            }

            match &self.best_profit_gas_ratio_swap {
                Some(best_swap) => {
                    if best_swap.profit_eth_gas_ratio() < request.profit_eth_gas_ratio() {
                        self.best_profit_gas_ratio_swap = Some(request.clone());
                        is_ok = true;
                    } else {
                        match self.validity_pct {
                            Some(pct) => {
                                if (best_swap.profit_eth_gas_ratio() * pct) / U256::from(10000) < request.profit_eth_gas_ratio() {
                                    is_ok = true
                                }
                            }
                            None => {}
                        }
                    }
                }
                None => {
                    self.best_profit_gas_ratio_swap = Some(request.clone());
                    is_ok = true;
                }
            }
        }
        is_ok
    }
}


pub type MessageTxCompose = Message<TxCompose>;

impl MessageTxCompose {
    pub fn encode(data: TxComposeData) -> Self {
        Message::new(TxCompose::Encode(data))
    }

    pub fn sign(data: TxComposeData) -> Self {
        Message::new(TxCompose::Sign(data))
    }

    pub fn estimate(data: TxComposeData) -> Self {
        Message::new(TxCompose::Estimate(data))
    }

    pub fn broadcast(data: TxComposeData) -> Self {
        Message::new(TxCompose::Broadcast(data))
    }
}


#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test() {
        let v = vec![
            RlpState::Stuffing(Bytes::from(vec![1])),
            RlpState::Backrun(Bytes::from(vec![2])),
        ];

        let b: Vec<Bytes> = v.iter().filter(|i| matches!(i, RlpState::Backrun(_))).map(|i| i.unwrap()).collect();

        for c in b {
            println!("{c:?}");
        }
    }
}