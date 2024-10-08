mod db;
mod nibble;
mod node;
mod node_hash;
mod rlp;
mod state;

#[cfg(test)]
mod test_utils;

use ethereum_rust_core::rlp::constants::RLP_NULL;
use ethereum_types::H256;
use node::Node;
use node_hash::NodeHash;
use sha3::{Digest, Keccak256};

use self::{db::TrieDB, nibble::NibbleSlice, node::LeafNode, state::TrieState};
use crate::error::StoreError;

use lazy_static::lazy_static;

lazy_static! {
    // Hash value for an empty trie, equal to keccak(RLP_NULL)
    static ref EMPTY_TRIE_HASH: H256 = H256::from_slice(
        Keccak256::new()
            .chain_update([RLP_NULL])
            .finalize()
            .as_slice(),
    );
}

/// RLP-encoded trie path
pub type PathRLP = Vec<u8>;
// RLP-encoded trie value
pub type ValueRLP = Vec<u8>;

/// Libmdx-based Ethereum Compatible Merkle Patricia Trie
/// Adapted from https://github.com/lambdaclass/merkle_patricia_tree
pub struct Trie<DB: TrieDB> {
    /// Hash of the current node
    root: Option<NodeHash>,
    /// Contains the trie's nodes
    pub(crate) state: TrieState<DB>,
}

impl<DB: TrieDB> Trie<DB> {
    /// Creates a new Trie from a clean DB
    pub fn new(db: DB) -> Self {
        Self {
            state: TrieState::new(db),
            root: None,
        }
    }

    /// Creates a trie from an already-initialized DB and sets root as the root node of the trie
    pub fn open(db: DB, root: H256) -> Self {
        let root = (root != *EMPTY_TRIE_HASH).then_some(root.into());
        Self {
            state: TrieState::new(db),
            root,
        }
    }

    /// Retrieve an RLP-encoded value from the trie given its RLP-encoded path.
    pub fn get(&self, path: &PathRLP) -> Result<Option<ValueRLP>, StoreError> {
        if let Some(root) = &self.root {
            let root_node = self
                .state
                .get_node(root.clone())?
                .expect("inconsistent internal tree structure");
            root_node.get(&self.state, NibbleSlice::new(path))
        } else {
            Ok(None)
        }
    }

    /// Insert an RLP-encoded value into the trie.
    pub fn insert(&mut self, path: PathRLP, value: ValueRLP) -> Result<(), StoreError> {
        let root = self.root.take();
        if let Some(root_node) = root
            .map(|root| self.state.get_node(root))
            .transpose()?
            .flatten()
        {
            // If the trie is not empty, call the root node's insertion logic
            let root_node =
                root_node.insert(&mut self.state, NibbleSlice::new(&path), value.clone())?;
            self.root = Some(root_node.insert_self(0, &mut self.state)?)
        } else {
            // If the trie is empty, just add a leaf.
            let new_leaf = Node::from(LeafNode::new(path.clone(), value));
            self.root = Some(new_leaf.insert_self(0, &mut self.state)?)
        }
        Ok(())
    }

    /// Remove a value from the trie given its RLP-encoded path.
    /// Returns the value if it was succesfully removed or None if it wasn't part of the trie
    pub fn remove(&mut self, path: PathRLP) -> Result<Option<ValueRLP>, StoreError> {
        let root = self.root.take();
        if let Some(root) = root {
            let root_node = self
                .state
                .get_node(root)?
                .expect("inconsistent internal tree structure");
            let (root_node, old_value) =
                root_node.remove(&mut self.state, NibbleSlice::new(&path))?;
            self.root = root_node
                .map(|root| root.insert_self(0, &mut self.state))
                .transpose()?;
            Ok(old_value)
        } else {
            Ok(None)
        }
    }

    /// Return the hash of the trie's root node.
    /// Returns keccak(RLP_NULL) if the trie is empty
    /// Also commits changes to the DB
    pub fn hash(&mut self) -> Result<H256, StoreError> {
        if let Some(ref root) = self.root {
            self.state.commit(root)?;
        }
        Ok(self
            .root
            .as_ref()
            .map(|root| root.clone().finalize())
            .unwrap_or(*EMPTY_TRIE_HASH))
    }

    /// Retrieve a value from the trie given its path from the subtrie originating from the given root
    /// Please use a root_hash calculated using `compute_hash`
    /// This function is used to access historical data
    // WARNING: Won't work if the root is too small (as it wont be stored by its hash)
    // (This should only be a problem for testing, as the values used in this project exceed the minimum hashable)
    pub fn get_from_root(
        &self,
        root_hash: H256,
        path: &PathRLP,
    ) -> Result<Option<ValueRLP>, StoreError> {
        if let Some(root_node) = self.state.get_node(root_hash.into())? {
            root_node.get(&self.state, NibbleSlice::new(path))
        } else {
            Ok(None)
        }
    }

    /// Sets the root of the trie to the one which's hash corresponds to the one received
    /// Doesn't check that the root_hash is valid within the trie
    /// Please use a root hash that has been calculated using `compute_hash`
    // WARNING: Won't work if the root is too small (as it wont be stored by its hash)
    // (This should only be a problem for testing, as the values used in this project exceed the minimum hashable)
    pub fn set_root(&mut self, root_hash: H256) {
        self.root = (root_hash != *EMPTY_TRIE_HASH).then_some(root_hash.into());
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use crate::trie::test_utils::new_temp_trie;

    use super::*;
    // Rename imports to avoid potential name clashes
    use super::test_utils;
    use cita_trie::{MemoryDB as CitaMemoryDB, PatriciaTrie as CitaTrie, Trie as CitaTrieTrait};
    use db::libmdbx::Libmdbx;
    use hasher::HasherKeccak;
    use hex_literal::hex;
    use proptest::{
        collection::{btree_set, vec},
        prelude::*,
        proptest,
    };
    use tempdir::TempDir;

    #[test]
    fn compute_hash() {
        let mut trie = new_temp_trie();
        trie.insert(b"first".to_vec(), b"value".to_vec()).unwrap();
        trie.insert(b"second".to_vec(), b"value".to_vec()).unwrap();

        assert_eq!(
            trie.hash().unwrap().as_ref(),
            hex!("f7537e7f4b313c426440b7fface6bff76f51b3eb0d127356efbe6f2b3c891501")
        );
    }

    #[test]
    fn compute_hash_long() {
        let mut trie = new_temp_trie();
        trie.insert(b"first".to_vec(), b"value".to_vec()).unwrap();
        trie.insert(b"second".to_vec(), b"value".to_vec()).unwrap();
        trie.insert(b"third".to_vec(), b"value".to_vec()).unwrap();
        trie.insert(b"fourth".to_vec(), b"value".to_vec()).unwrap();

        assert_eq!(
            trie.hash().unwrap().0.to_vec(),
            hex!("e2ff76eca34a96b68e6871c74f2a5d9db58e59f82073276866fdd25e560cedea")
        );
    }

    #[test]
    fn get_insert_words() {
        let mut trie = new_temp_trie();
        let first_path = b"first".to_vec();
        let first_value = b"value_a".to_vec();
        let second_path = b"second".to_vec();
        let second_value = b"value_b".to_vec();
        // Check that the values dont exist before inserting
        assert!(trie.get(&first_path).unwrap().is_none());
        assert!(trie.get(&second_path).unwrap().is_none());
        // Insert values
        trie.insert(first_path.clone(), first_value.clone())
            .unwrap();
        trie.insert(second_path.clone(), second_value.clone())
            .unwrap();
        // Check values
        assert_eq!(trie.get(&first_path).unwrap(), Some(first_value));
        assert_eq!(trie.get(&second_path).unwrap(), Some(second_value));
    }

    #[test]
    fn get_insert_zero() {
        let mut trie = new_temp_trie();
        trie.insert(vec![0x0], b"value".to_vec()).unwrap();
        let first = trie.get(&[0x0][..].to_vec()).unwrap();
        assert_eq!(first, Some(b"value".to_vec()));
    }

    #[test]
    fn get_insert_a() {
        let mut trie = new_temp_trie();
        trie.insert(vec![16], vec![0]).unwrap();
        trie.insert(vec![16, 0], vec![0]).unwrap();

        let item = trie.get(&vec![16]).unwrap();
        assert_eq!(item, Some(vec![0]));

        let item = trie.get(&vec![16, 0]).unwrap();
        assert_eq!(item, Some(vec![0]));
    }

    #[test]
    fn get_insert_b() {
        let mut trie = new_temp_trie();
        trie.insert(vec![0, 0], vec![0, 0]).unwrap();
        trie.insert(vec![1, 0], vec![1, 0]).unwrap();

        let item = trie.get(&vec![1, 0]).unwrap();
        assert_eq!(item, Some(vec![1, 0]));

        let item = trie.get(&vec![0, 0]).unwrap();
        assert_eq!(item, Some(vec![0, 0]));
    }

    #[test]
    fn get_insert_c() {
        let mut trie = new_temp_trie();
        let vecs = vec![
            vec![26, 192, 44, 251],
            vec![195, 132, 220, 124, 112, 201, 70, 128, 235],
            vec![126, 138, 25, 245, 146],
            vec![129, 176, 66, 2, 150, 151, 180, 60, 124],
            vec![138, 101, 157],
        ];
        for x in &vecs {
            trie.insert(x.clone(), x.clone()).unwrap();
        }
        for x in &vecs {
            let item = trie.get(x).unwrap();
            assert_eq!(item, Some(x.clone()));
        }
    }

    #[test]
    fn get_insert_d() {
        let mut trie = new_temp_trie();
        let vecs = vec![
            vec![52, 53, 143, 52, 206, 112],
            vec![14, 183, 34, 39, 113],
            vec![55, 5],
            vec![134, 123, 19],
            vec![0, 59, 240, 89, 83, 167],
            vec![22, 41],
            vec![13, 166, 159, 101, 90, 234, 91],
            vec![31, 180, 161, 122, 115, 51, 37, 61, 101],
            vec![208, 192, 4, 12, 163, 254, 129, 206, 109],
        ];
        for x in &vecs {
            trie.insert(x.clone(), x.clone()).unwrap();
        }
        for x in &vecs {
            let item = trie.get(x).unwrap();
            assert_eq!(item, Some(x.clone()));
        }
    }

    #[test]
    fn get_insert_e() {
        let mut trie = new_temp_trie();
        trie.insert(vec![0x00], vec![0x00]).unwrap();
        trie.insert(vec![0xC8], vec![0xC8]).unwrap();
        trie.insert(vec![0xC8, 0x00], vec![0xC8, 0x00]).unwrap();

        assert_eq!(trie.get(&vec![0x00]).unwrap(), Some(vec![0x00]));
        assert_eq!(trie.get(&vec![0xC8]).unwrap(), Some(vec![0xC8]));
        assert_eq!(trie.get(&vec![0xC8, 0x00]).unwrap(), Some(vec![0xC8, 0x00]));
    }

    #[test]
    fn get_insert_f() {
        let mut trie = new_temp_trie();
        trie.insert(vec![0x00], vec![0x00]).unwrap();
        trie.insert(vec![0x01], vec![0x01]).unwrap();
        trie.insert(vec![0x10], vec![0x10]).unwrap();
        trie.insert(vec![0x19], vec![0x19]).unwrap();
        trie.insert(vec![0x19, 0x00], vec![0x19, 0x00]).unwrap();
        trie.insert(vec![0x1A], vec![0x1A]).unwrap();

        assert_eq!(trie.get(&vec![0x00]).unwrap(), Some(vec![0x00]));
        assert_eq!(trie.get(&vec![0x01]).unwrap(), Some(vec![0x01]));
        assert_eq!(trie.get(&vec![0x10]).unwrap(), Some(vec![0x10]));
        assert_eq!(trie.get(&vec![0x19]).unwrap(), Some(vec![0x19]));
        assert_eq!(trie.get(&vec![0x19, 0x00]).unwrap(), Some(vec![0x19, 0x00]));
        assert_eq!(trie.get(&vec![0x1A]).unwrap(), Some(vec![0x1A]));
    }

    #[test]
    fn get_insert_remove_a() {
        let mut trie = new_temp_trie();
        trie.insert(b"do".to_vec(), b"verb".to_vec()).unwrap();
        trie.insert(b"horse".to_vec(), b"stallion".to_vec())
            .unwrap();
        trie.insert(b"doge".to_vec(), b"coin".to_vec()).unwrap();
        trie.remove(b"horse".to_vec()).unwrap();
        assert_eq!(trie.get(&b"do".to_vec()).unwrap(), Some(b"verb".to_vec()));
        assert_eq!(trie.get(&b"doge".to_vec()).unwrap(), Some(b"coin".to_vec()));
    }

    #[test]
    fn get_insert_remove_b() {
        let mut trie = new_temp_trie();
        trie.insert(vec![185], vec![185]).unwrap();
        trie.insert(vec![185, 0], vec![185, 0]).unwrap();
        trie.insert(vec![185, 1], vec![185, 1]).unwrap();
        trie.remove(vec![185, 1]).unwrap();
        assert_eq!(trie.get(&vec![185, 0]).unwrap(), Some(vec![185, 0]));
        assert_eq!(trie.get(&vec![185]).unwrap(), Some(vec![185]));
        assert!(trie.get(&vec![185, 1]).unwrap().is_none());
    }

    #[test]
    fn compute_hash_a() {
        let mut trie = new_temp_trie();
        trie.insert(b"do".to_vec(), b"verb".to_vec()).unwrap();
        trie.insert(b"horse".to_vec(), b"stallion".to_vec())
            .unwrap();
        trie.insert(b"doge".to_vec(), b"coin".to_vec()).unwrap();
        trie.insert(b"dog".to_vec(), b"puppy".to_vec()).unwrap();

        assert_eq!(
            trie.hash().unwrap().0.as_slice(),
            hex!("5991bb8c6514148a29db676a14ac506cd2cd5775ace63c30a4fe457715e9ac84").as_slice()
        );
    }

    #[test]
    fn compute_hash_b() {
        let mut trie = new_temp_trie();
        assert_eq!(
            trie.hash().unwrap().0.as_slice(),
            hex!("56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421").as_slice(),
        );
    }

    #[test]
    fn compute_hash_c() {
        let mut trie = new_temp_trie();
        let data = [
            (
                hex!("0000000000000000000000000000000000000000000000000000000000000045").to_vec(),
                hex!("22b224a1420a802ab51d326e29fa98e34c4f24ea").to_vec(),
            ),
            (
                hex!("0000000000000000000000000000000000000000000000000000000000000046").to_vec(),
                hex!("67706c2076330000000000000000000000000000000000000000000000000000").to_vec(),
            ),
            (
                hex!("000000000000000000000000697c7b8c961b56f675d570498424ac8de1a918f6").to_vec(),
                hex!("1234567890").to_vec(),
            ),
            (
                hex!("0000000000000000000000007ef9e639e2733cb34e4dfc576d4b23f72db776b2").to_vec(),
                hex!("4655474156000000000000000000000000000000000000000000000000000000").to_vec(),
            ),
            (
                hex!("000000000000000000000000ec4f34c97e43fbb2816cfd95e388353c7181dab1").to_vec(),
                hex!("4e616d6552656700000000000000000000000000000000000000000000000000").to_vec(),
            ),
            (
                hex!("4655474156000000000000000000000000000000000000000000000000000000").to_vec(),
                hex!("7ef9e639e2733cb34e4dfc576d4b23f72db776b2").to_vec(),
            ),
            (
                hex!("4e616d6552656700000000000000000000000000000000000000000000000000").to_vec(),
                hex!("ec4f34c97e43fbb2816cfd95e388353c7181dab1").to_vec(),
            ),
            (
                hex!("000000000000000000000000697c7b8c961b56f675d570498424ac8de1a918f6").to_vec(),
                hex!("6f6f6f6820736f2067726561742c207265616c6c6c793f000000000000000000").to_vec(),
            ),
            (
                hex!("6f6f6f6820736f2067726561742c207265616c6c6c793f000000000000000000").to_vec(),
                hex!("697c7b8c961b56f675d570498424ac8de1a918f6").to_vec(),
            ),
        ];

        for (path, value) in data {
            trie.insert(path, value).unwrap();
        }

        assert_eq!(
            trie.hash().unwrap().0.as_slice(),
            hex!("9f6221ebb8efe7cff60a716ecb886e67dd042014be444669f0159d8e68b42100").as_slice(),
        );
    }

    #[test]
    fn compute_hash_d() {
        let mut trie = new_temp_trie();

        let data = [
            (
                b"key1aa".to_vec(),
                b"0123456789012345678901234567890123456789xxx".to_vec(),
            ),
            (
                b"key1".to_vec(),
                b"0123456789012345678901234567890123456789Very_Long".to_vec(),
            ),
            (b"key2bb".to_vec(), b"aval3".to_vec()),
            (b"key2".to_vec(), b"short".to_vec()),
            (b"key3cc".to_vec(), b"aval3".to_vec()),
            (
                b"key3".to_vec(),
                b"1234567890123456789012345678901".to_vec(),
            ),
        ];

        for (path, value) in data {
            trie.insert(path, value).unwrap();
        }

        assert_eq!(
            trie.hash().unwrap().0.as_slice(),
            hex!("cb65032e2f76c48b82b5c24b3db8f670ce73982869d38cd39a624f23d62a9e89").as_slice(),
        );
    }

    #[test]
    fn compute_hash_e() {
        let mut trie = new_temp_trie();
        trie.insert(b"abc".to_vec(), b"123".to_vec()).unwrap();
        trie.insert(b"abcd".to_vec(), b"abcd".to_vec()).unwrap();
        trie.insert(b"abc".to_vec(), b"abc".to_vec()).unwrap();

        assert_eq!(
            trie.hash().unwrap().0.as_slice(),
            hex!("7a320748f780ad9ad5b0837302075ce0eeba6c26e3d8562c67ccc0f1b273298a").as_slice(),
        );
    }

    #[test]
    fn get_old_state() {
        let mut trie = new_temp_trie();
        trie.insert([0; 32].to_vec(), [0; 32].to_vec()).unwrap();
        trie.insert([1; 32].to_vec(), [1; 32].to_vec()).unwrap();

        let root = trie.hash().unwrap();

        trie.insert([0; 32].to_vec(), [2; 32].to_vec()).unwrap();
        trie.insert([1; 32].to_vec(), [3; 32].to_vec()).unwrap();

        assert_eq!(trie.get(&[0; 32].to_vec()).unwrap(), Some([2; 32].to_vec()));
        assert_eq!(trie.get(&[1; 32].to_vec()).unwrap(), Some([3; 32].to_vec()));

        assert_eq!(
            trie.get_from_root(root, &[0; 32].to_vec()).unwrap(),
            Some([0; 32].to_vec())
        );
        assert_eq!(
            trie.get_from_root(root, &[1; 32].to_vec()).unwrap(),
            Some([1; 32].to_vec())
        );
    }

    #[test]
    fn get_old_state_with_removals() {
        let mut trie = new_temp_trie();
        trie.insert([0; 32].to_vec(), [0; 32].to_vec()).unwrap();
        trie.insert([1; 32].to_vec(), [1; 32].to_vec()).unwrap();
        trie.insert([2; 32].to_vec(), [2; 32].to_vec()).unwrap();

        let root = trie.hash().unwrap();

        trie.insert([0; 32].to_vec(), vec![0x04]).unwrap();
        trie.remove([1; 32].to_vec()).unwrap();
        trie.insert([2; 32].to_vec(), vec![0x05]).unwrap();
        trie.remove([0; 32].to_vec()).unwrap();

        assert_eq!(trie.get(&[0; 32].to_vec()).unwrap(), None);
        assert_eq!(trie.get(&[1; 32].to_vec()).unwrap(), None);
        assert_eq!(trie.get(&[2; 32].to_vec()).unwrap(), Some(vec![0x05]));

        assert_eq!(
            trie.get_from_root(root, &[0; 32].to_vec()).unwrap(),
            Some([0; 32].to_vec())
        );
        assert_eq!(
            trie.get_from_root(root, &[1; 32].to_vec()).unwrap(),
            Some([1; 32].to_vec())
        );
        assert_eq!(
            trie.get_from_root(root, &[2; 32].to_vec()).unwrap(),
            Some([2; 32].to_vec())
        );
    }

    #[test]
    fn revert() {
        let mut trie = new_temp_trie();
        trie.insert([0; 32].to_vec(), [0; 32].to_vec()).unwrap();
        trie.insert([1; 32].to_vec(), [1; 32].to_vec()).unwrap();

        let root = trie.hash().unwrap();

        trie.insert([0; 32].to_vec(), [2; 32].to_vec()).unwrap();
        trie.insert([1; 32].to_vec(), [3; 32].to_vec()).unwrap();

        trie.set_root(root);

        trie.insert([2; 32].to_vec(), [4; 32].to_vec()).unwrap();

        assert_eq!(trie.get(&[0; 32].to_vec()).unwrap(), Some([0; 32].to_vec()));
        assert_eq!(trie.get(&[1; 32].to_vec()).unwrap(), Some([1; 32].to_vec()));
        assert_eq!(trie.get(&[2; 32].to_vec()).unwrap(), Some([4; 32].to_vec()));
    }

    #[test]
    fn revert_with_removals() {
        let mut trie = new_temp_trie();
        trie.insert([0; 32].to_vec(), [0; 32].to_vec()).unwrap();
        trie.insert([1; 32].to_vec(), [1; 32].to_vec()).unwrap();
        trie.insert([2; 32].to_vec(), [2; 32].to_vec()).unwrap();

        let root = trie.hash().unwrap();

        trie.insert([0; 32].to_vec(), [4; 32].to_vec()).unwrap();
        trie.remove([1; 32].to_vec()).unwrap();
        trie.insert([2; 32].to_vec(), [5; 32].to_vec()).unwrap();
        trie.remove([0; 32].to_vec()).unwrap();

        trie.set_root(root);

        trie.remove([2; 32].to_vec()).unwrap();

        assert_eq!(trie.get(&[0; 32].to_vec()).unwrap(), Some([0; 32].to_vec()));
        assert_eq!(trie.get(&[1; 32].to_vec()).unwrap(), Some([1; 32].to_vec()));
        assert_eq!(trie.get(&vec![0x02]).unwrap(), None);
    }

    #[test]
    fn resume_trie() {
        const TRIE_DIR: &str = "trie-db-resume-trie-test";
        let trie_dir = TempDir::new(TRIE_DIR).expect("Failed to create temp dir");
        let trie_dir = trie_dir.path().to_str().unwrap();

        // Create new trie from clean DB
        let db = Libmdbx::create(trie_dir).unwrap();
        let mut trie = Trie::new(db);

        trie.insert([0; 32].to_vec(), [1; 32].to_vec()).unwrap();
        trie.insert([1; 32].to_vec(), [2; 32].to_vec()).unwrap();
        trie.insert([2; 32].to_vec(), [4; 32].to_vec()).unwrap();

        // Save current root
        let root = trie.hash().unwrap();

        drop(trie); // Release DB

        let mut db2 = Libmdbx::open(trie_dir).unwrap();
        // Create a new trie based on the previous trie's DB
        let trie = Trie::open(db2, root);

        assert_eq!(trie.get(&[0; 32].to_vec()).unwrap(), Some([1; 32].to_vec()));
        assert_eq!(trie.get(&[1; 32].to_vec()).unwrap(), Some([2; 32].to_vec()));
        assert_eq!(trie.get(&[2; 32].to_vec()).unwrap(), Some([4; 32].to_vec()));
    }

    // Proptests
    proptest! {
        #[test]
        fn proptest_get_insert(data in btree_set(vec(any::<u8>(), 1..100), 1..100)) {
            let mut trie = new_temp_trie();

            for val in data.iter(){
                trie.insert(val.clone(), val.clone()).unwrap();
            }

            for val in data.iter() {
                let item = trie.get(val).unwrap();
                prop_assert!(item.is_some());
                prop_assert_eq!(&item.unwrap(), val);
            }
        }

        #[test]
        fn proptest_get_insert_with_removals(mut data in vec((vec(any::<u8>(), 5..100), any::<bool>()), 1..100)) {
            let mut trie = new_temp_trie();
            // Remove duplicate values with different expected status
            data.sort_by_key(|(val, _)| val.clone());
            data.dedup_by_key(|(val, _)| val.clone());
            // Insertions
            for (val, _) in data.iter() {
                trie.insert(val.clone(), val.clone()).unwrap();
            }
            // Removals
            for (val, should_remove) in data.iter() {
                if *should_remove {
                    let removed = trie.remove(val.clone()).unwrap();
                    prop_assert_eq!(removed, Some(val.clone()));
                }
            }
            // Check trie values
            for (val, removed) in data.iter() {
                let item = trie.get(val).unwrap();
                if !removed {
                    prop_assert_eq!(item, Some(val.clone()));
                } else {
                    prop_assert!(item.is_none());
                }
            }
        }

        #[test]
        // The previous test needs to sort the input values in order to get rid of duplicate entries, leading to ordered insertions
        // This check has a fixed way of determining wether a value should be removed but doesn't require ordered insertions
        fn proptest_get_insert_with_removals_unsorted(data in btree_set(vec(any::<u8>(), 5..100), 1..100)) {
            let mut trie = new_temp_trie();
            // Remove all values that have an odd first value
            let remove = |value: &Vec<u8>| -> bool {
                value.first().is_some_and(|v| v % 2 != 0)
            };
            // Insertions
            for val in data.iter() {
                trie.insert(val.clone(), val.clone()).unwrap();
            }
            // Removals
            for val in data.iter() {
                if remove(val) {
                    let removed = trie.remove(val.clone()).unwrap();
                    prop_assert_eq!(removed, Some(val.clone()));
                }
            }
            // Check trie values
            for val in data.iter() {
                let item = trie.get(val).unwrap();
                if !remove(val) {
                    prop_assert_eq!(item, Some(val.clone()));
                } else {
                    prop_assert!(item.is_none());
                }
            }
        }


        #[test]
        fn proptest_compare_hash(data in btree_set(vec(any::<u8>(), 1..100), 1..100)) {
            let mut trie = new_temp_trie();
            let mut cita_trie = cita_trie();

            for val in data.iter(){
                trie.insert(val.clone(), val.clone()).unwrap();
                cita_trie.insert(val.clone(), val.clone()).unwrap();
            }

            let hash = trie.hash().unwrap().0.to_vec();
            let cita_hash = cita_trie.root().unwrap();
            prop_assert_eq!(hash, cita_hash);
        }

        #[test]
        fn proptest_compare_hash_with_removals(mut data in vec((vec(any::<u8>(), 5..100), any::<bool>()), 1..100)) {
            let mut trie = new_temp_trie();
            let mut cita_trie = cita_trie();
            // Remove duplicate values with different expected status
            data.sort_by_key(|(val, _)| val.clone());
            data.dedup_by_key(|(val, _)| val.clone());
            // Insertions
            for (val, _) in data.iter() {
                trie.insert(val.clone(), val.clone()).unwrap();
                cita_trie.insert(val.clone(), val.clone()).unwrap();
            }
            // Removals
            for (val, should_remove) in data.iter() {
                if *should_remove {
                    trie.remove(val.clone()).unwrap();
                    cita_trie.remove(val).unwrap();
                }
            }
            // Compare hashes
            let hash = trie.hash().unwrap().0.to_vec();
            let cita_hash = cita_trie.root().unwrap();
            prop_assert_eq!(hash, cita_hash);
        }

        #[test]
        // The previous test needs to sort the input values in order to get rid of duplicate entries, leading to ordered insertions
        // This check has a fixed way of determining wether a value should be removed but doesn't require ordered insertions
        fn proptest_compare_hash_with_removals_unsorted(data in btree_set(vec(any::<u8>(), 5..100), 1..100)) {
            let mut trie = new_temp_trie();
            let mut cita_trie = cita_trie();
            // Remove all values that have an odd first value
            let remove = |value: &Vec<u8>| -> bool {
                value.first().is_some_and(|v| v % 2 != 0)
            };
            // Insertions
            for val in data.iter() {
                trie.insert(val.clone(), val.clone()).unwrap();
                cita_trie.insert(val.clone(), val.clone()).unwrap();
            }
            // Removals
            for val in data.iter() {
                if remove(val) {
                    trie.remove(val.clone()).unwrap();
                    cita_trie.remove(val).unwrap();
                }
            }
            // Compare hashes
            let hash = trie.hash().unwrap().0.to_vec();
            let cita_hash = cita_trie.root().unwrap();
            prop_assert_eq!(hash, cita_hash);
        }

        #[test]
        fn proptest_compare_hash_between_inserts(data in btree_set(vec(any::<u8>(), 1..100), 1..100)) {
            let mut trie = new_temp_trie();
            let mut cita_trie = cita_trie();

            for val in data.iter(){
                trie.insert(val.clone(), val.clone()).unwrap();
                cita_trie.insert(val.clone(), val.clone()).unwrap();
                let hash = trie.hash().unwrap().0.to_vec();
                let cita_hash = cita_trie.root().unwrap();
                prop_assert_eq!(hash, cita_hash);
            }

        }

    }

    fn cita_trie() -> CitaTrie<CitaMemoryDB, HasherKeccak> {
        let memdb = Arc::new(CitaMemoryDB::new(true));
        let hasher = Arc::new(HasherKeccak::new());

        CitaTrie::new(Arc::clone(&memdb), Arc::clone(&hasher))
    }
}
