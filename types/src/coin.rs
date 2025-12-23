//! Coin Object - Transferable Asset
//! 
//! Design Philosophy:
//! - Coin is an independent object that can be owned by SBT
//! - Supports operations like split, merge, transfer
//! - Balance is a value type, not an object

use serde::{Deserialize, Serialize};
use crate::object::{Object, ObjectId, Address, generate_object_id};

/// Balance is a value type that encapsulates token amount
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Balance {
    value: u64,
}

impl Balance {
    /// Create a new Balance
    pub fn new(value: u64) -> Self {
        Self { value }
    }
    
    /// Get the balance value
    pub fn value(&self) -> u64 {
        self.value
    }
    
    /// Withdraw a specified amount, returns the withdrawn Balance
    pub fn withdraw(&mut self, amount: u64) -> Result<Balance, String> {
        if self.value < amount {
            return Err(format!(
                "Insufficient balance: have {}, need {}",
                self.value, amount
            ));
        }
        self.value -= amount;
        Ok(Balance::new(amount))
    }
    
    /// Deposit Balance
    pub fn deposit(&mut self, balance: Balance) -> Result<(), String> {
        self.value = self.value.checked_add(balance.value)
            .ok_or("Balance overflow")?;
        Ok(())
    }
    
    /// Destroy Balance (used for merging)
    pub fn destroy(self) -> u64 {
        self.value
    }
}

/// Coin object data - represents transferable tokens
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinData {
    pub balance: Balance,
}

/// Coin type alias
pub type Coin = Object<CoinData>;

impl Coin {
    /// Create a new Coin object
    /// 
    /// # Parameters
    /// - `owner`: Owner of the Coin (usually an SBT's ObjectId)
    /// - `value`: Initial balance
    pub fn new(owner: Address, value: u64) -> Self {
        let id = generate_object_id(format!("coin:{}:{}", owner, value).as_bytes());
        let data = CoinData {
            balance: Balance::new(value),
        };
        Object::new_owned(id, &owner, data)
    }
    
    /// Get balance
    pub fn value(&self) -> u64 {
        self.data.balance.value()
    }
    
    /// Split a specified amount into a new Coin
    /// 
    /// # Parameters
    /// - `amount`: Amount to split
    /// - `new_owner`: Owner of the new Coin
    /// 
    /// # Returns
    /// Returns the newly created Coin object
    pub fn split(&mut self, amount: u64, new_owner: Address) -> Result<Coin, String> {
        let withdrawn = self.data.balance.withdraw(amount)?;
        let new_coin = Coin::new(new_owner, withdrawn.value());
        self.increment_version();
        Ok(new_coin)
    }
    
    /// Merge another Coin into the current Coin
    /// 
    /// # Parameters
    /// - `other`: The Coin object to merge
    pub fn merge(&mut self, other: Coin) -> Result<(), String> {
        self.data.balance.deposit(other.data.balance)?;
        self.increment_version();
        Ok(())
    }
    
    /// Transfer ownership of the Coin
    /// 
    /// # Parameters
    /// - `new_owner`: New owner
    pub fn transfer(&mut self, new_owner: Address) {
        self.metadata.owner = Some(new_owner);
        self.increment_version();
    }
}

/// Helper function: create Coin
pub fn create_coin(owner: Address, value: u64) -> Coin {
    Coin::new(owner, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_balance_operations() {
        let mut balance = Balance::new(1000);
        
        // Withdraw
        let withdrawn = balance.withdraw(300).unwrap();
        assert_eq!(balance.value(), 700);
        assert_eq!(withdrawn.value(), 300);
        
        // Deposit
        balance.deposit(withdrawn).unwrap();
        assert_eq!(balance.value(), 1000);
        
        // Insufficient balance
        assert!(balance.withdraw(2000).is_err());
    }
    
    #[test]
    fn test_balance_overflow() {
        let mut balance = Balance::new(u64::MAX - 100);
        let to_add = Balance::new(200);
        assert!(balance.deposit(to_add).is_err());
    }
    
    #[test]
    fn test_coin_creation() {
        let owner = Address::from("sbt_alice");
        let coin = Coin::new(owner.clone(), 1000);
        
        assert_eq!(coin.value(), 1000);
        assert_eq!(coin.metadata.owner.as_ref().unwrap(), &owner);
        assert_eq!(coin.metadata.version, 1); // Initial version is 1
    }
    
    #[test]
    fn test_coin_split() {
        let owner = Address::from("sbt_alice");
        let mut coin = Coin::new(owner.clone(), 1000);
        
        let new_owner = Address::from("sbt_bob");
        let new_coin = coin.split(300, new_owner.clone()).unwrap();
        
        assert_eq!(coin.value(), 700);
        assert_eq!(new_coin.value(), 300);
        assert_eq!(coin.metadata.version, 2); // Version incremented after operation
        assert_eq!(new_coin.metadata.version, 1); // Newly created object has version 1
        assert_eq!(new_coin.metadata.owner.as_ref().unwrap(), &new_owner);
    }
    
    #[test]
    fn test_coin_split_insufficient() {
        let owner = Address::from("sbt_alice");
        let mut coin = Coin::new(owner.clone(), 100);
        
        let result = coin.split(200, Address::from("sbt_bob"));
        assert!(result.is_err());
    }
    
    #[test]
    fn test_coin_merge() {
        let owner = Address::from("sbt_alice");
        let mut coin1 = Coin::new(owner.clone(), 1000);
        let coin2 = Coin::new(owner.clone(), 500);
        
        coin1.merge(coin2).unwrap();
        
        assert_eq!(coin1.value(), 1500);
        assert_eq!(coin1.metadata.version, 2); // Version incremented after operation
    }
    
    #[test]
    fn test_coin_transfer() {
        let owner = Address::from("sbt_alice");
        let mut coin = Coin::new(owner.clone(), 1000);
        
        let new_owner = Address::from("sbt_bob");
        coin.transfer(new_owner.clone());
        
        assert_eq!(coin.metadata.owner.as_ref().unwrap(), &new_owner);
        assert_eq!(coin.metadata.version, 2); // Version incremented after operation
    }
}
