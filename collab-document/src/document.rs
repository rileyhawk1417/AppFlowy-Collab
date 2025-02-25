use std::collections::HashMap;
use std::sync::Arc;
use std::vec;

use collab::core::collab::MutexCollab;
use collab::core::collab_state::SyncState;
use collab::core::origin::CollabOrigin;
use collab::preclude::*;
use serde_json::Value;
use tokio_stream::wrappers::WatchStream;

use crate::blocks::{
  deserialize_text_delta, Block, BlockAction, BlockActionPayload, BlockActionType, BlockEvent,
  BlockOperation, ChildrenOperation, DocumentData, DocumentMeta, RootDeepSubscription,
  TextOperation,
};
use crate::error::DocumentError;

const ROOT: &str = "document";

/// The page_id is a reference that points to the block’s id.
/// The block that is referenced by this page_id is the first block of the document.
/// Crossing this block, we can build the whole document tree.
const PAGE_ID: &str = "page_id";
/// Document's all [Block] Map.
const BLOCKS: &str = "blocks";
/// Document's meta data.
const META: &str = "meta";
/// [Block]'s relation map. And it's also in [META].
/// The key is the parent block's children_id, and the value is the children block's id.
const CHILDREN_MAP: &str = "children_map";
/// [Block]'s yText map. And it's also in [META].
/// The key is the text block's external_id, and the value is the text block's yText.
const TEXT_MAP: &str = "text_map";

pub struct Document {
  inner: Arc<MutexCollab>,
  root: MapRefWrapper,
  subscription: RootDeepSubscription,
  children_operation: ChildrenOperation,
  block_operation: BlockOperation,
  text_operation: TextOperation,
}

impl Document {
  /// Create or get a document.
  pub fn open(collab: Arc<MutexCollab>) -> Result<Document, DocumentError> {
    Document::open_document_with_collab(collab)
  }

  pub fn get_collab(&self) -> &Arc<MutexCollab> {
    &self.inner
  }

  /// Create a new document with the given data.
  pub fn create_with_data(
    collab: Arc<MutexCollab>,
    data: DocumentData,
  ) -> Result<Document, DocumentError> {
    Document::create_document(collab, Some(data))
  }

  pub fn from_updates(
    origin: CollabOrigin,
    updates: Vec<Vec<u8>>,
    document_id: &str,
    plugins: Vec<Arc<dyn CollabPlugin>>,
  ) -> Result<Self, DocumentError> {
    let collab = MutexCollab::new_with_raw_data(origin, document_id, updates, plugins)?;
    Document::open(Arc::new(collab))
  }

  /// open a document and subscribe to the document changes.
  pub fn subscribe_block_changed<F>(&mut self, callback: F)
  where
    F: Fn(&Vec<BlockEvent>, bool) + 'static,
  {
    self
      .subscription
      .subscribe(&mut self.root, move |block_events, origin| {
        let is_remote = origin.is_some();
        callback(block_events, is_remote);
      });
  }

  pub fn subscribe_sync_state(&self) -> WatchStream<SyncState> {
    self.inner.lock().subscribe_sync_state()
  }

  pub fn with_transact_mut<F, T>(&self, f: F) -> T
  where
    F: FnOnce(&mut TransactionMut) -> T,
  {
    self.root.with_transact_mut(f)
  }

  /// Get document data.
  pub fn get_document_data(&self) -> Result<DocumentData, DocumentError> {
    let collab_guard = self.inner.lock();
    let txn = collab_guard.transact();
    let page_id = self
      .root
      .get_str_with_txn(&txn, PAGE_ID)
      .ok_or(DocumentError::PageIdIsEmpty)?;

    drop(txn);
    drop(collab_guard);

    let blocks = self.block_operation.get_all_blocks();
    let children_map = self.children_operation.get_all_children();
    let text_map = self.text_operation.serialize_all_text_delta();
    let document_data = DocumentData {
      page_id,
      blocks,
      meta: DocumentMeta {
        children_map,
        text_map: Some(text_map),
      },
    };
    Ok(document_data)
  }

  /// Create a yText for incremental synchronization.
  /// - @param text_id: The text block's external_id.
  /// - @param delta: The text block's delta. "\[{"insert": "Hello", "attributes": { "bold": true, "italic": true } }, {"insert": " World!"}]".
  pub fn create_text(&self, text_id: &str, delta: String) {
    self.inner.lock().with_origin_transact_mut(|txn| {
      self.create_text_with_txn(txn, text_id, delta);
    })
  }

  pub fn create_text_with_txn(&self, txn: &mut TransactionMut, text_id: &str, delta: String) {
    let delta = deserialize_text_delta(&delta).ok();
    self.text_operation.create_text_with_txn(txn, text_id);
    if let Some(delta) = delta {
      self
        .text_operation
        .apply_delta_with_txn(txn, text_id, delta);
    }
  }

  /// Apply a delta to the yText.
  /// - @param text_id: The text block's external_id.
  /// - @param delta: The text block's delta. "\[{"insert": "Hello", "attributes": { "bold": true, "italic": true } }, {"insert": " World!"}]".
  pub fn apply_text_delta(&self, text_id: &str, delta: String) {
    self
      .inner
      .lock()
      .with_origin_transact_mut(|txn| self.apply_text_delta_with_txn(txn, text_id, delta))
  }

  pub fn apply_text_delta_with_txn(&self, txn: &mut TransactionMut, text_id: &str, delta: String) {
    let delta = deserialize_text_delta(&delta).ok();
    if let Some(delta) = delta {
      self
        .text_operation
        .apply_delta_with_txn(txn, text_id, delta);
    } else {
      self
        .text_operation
        .apply_delta_with_txn(txn, text_id, vec![]);
    }
  }

  /// Apply actions to the document.
  pub fn apply_action(&self, actions: Vec<BlockAction>) {
    self.inner.lock().with_origin_transact_mut(|txn| {
      for action in actions {
        let result = match action.action {
          BlockActionType::Insert => self.handle_insert_action(txn, action.payload),
          BlockActionType::Update => self.handle_update_action(txn, action.payload),
          BlockActionType::Delete => self.handle_delete_action(txn, action.payload),
          BlockActionType::Move => self.handle_move_action(txn, action.payload),
          BlockActionType::InsertText => self.handle_insert_text_action(txn, action.payload),
          BlockActionType::ApplyTextDelta => {
            self.handle_apply_text_delta_action(txn, action.payload)
          },
        };

        if let Err(err) = result {
          // Handle the error
          tracing::error!("[Document] apply_action error: {:?}", err);
          return;
        }
      }
    })
  }

  /// Get block with the given id.
  pub fn get_block(&self, block_id: &str) -> Option<Block> {
    let collab_guard = self.inner.lock();
    let txn = collab_guard.transact();
    self.block_operation.get_block_with_txn(&txn, block_id)
  }

  /// Insert block to the document.
  pub fn insert_block(
    &self,
    txn: &mut TransactionMut,
    block: Block,
    prev_id: Option<String>,
  ) -> Result<Block, DocumentError> {
    let block = self.block_operation.create_block_with_txn(txn, block)?;
    self.insert_block_to_parent(txn, &block, prev_id)
  }

  /// Insert block with the given parent id and prev id.
  pub fn insert_block_to_parent(
    &self,
    txn: &mut TransactionMut,
    block: &Block,
    prev_id: Option<String>,
  ) -> Result<Block, DocumentError> {
    let parent_id = &block.parent;
    // If the parent is not found, return an error.
    if parent_id.is_empty() {
      return Err(DocumentError::ParentIsNotFound);
    }

    let parent = match self.block_operation.get_block_with_txn(txn, parent_id) {
      Some(parent) => parent,
      None => return Err(DocumentError::ParentIsNotFound),
    };

    let parent_children_id = &parent.children;
    // If the prev_id is not found, insert the block to the first position.
    // so the default index is 0.
    let index = prev_id
      .and_then(|prev_id| {
        self
          .children_operation
          .get_child_index_with_txn(txn, parent_children_id, &prev_id)
      })
      .map(|prev_index| prev_index + 1)
      .unwrap_or(0);
    self
      .children_operation
      .insert_child_with_txn(txn, parent_children_id, &block.id, index);
    let block = self
      .block_operation
      .get_block_with_txn(txn, &block.id)
      .unwrap();
    Ok(block)
  }

  /// delete the block from the document
  ///
  /// 1. delete all the children of this block
  /// 2. delete the block from its parent
  /// 3. delete the block from the block map
  pub fn delete_block(
    &self,
    txn: &mut TransactionMut,
    block_id: &str,
  ) -> Result<(), DocumentError> {
    let block = match self.block_operation.get_block_with_txn(txn, block_id) {
      Some(block) => block,
      None => return Err(DocumentError::BlockIsNotFound),
    };

    let external_id = &block.external_id;

    // Delete all the children of this block.
    let children = self
      .children_operation
      .get_children_with_txn(txn, &block.children);
    children
      .iter(txn)
      .map(|child| child.to_string(txn))
      .collect::<Vec<String>>()
      .iter()
      .for_each(|child| self.delete_block(txn, child).unwrap_or_default());

    // Delete the block from its parent.
    let parent_id = &block.parent;
    self.delete_block_from_parent(txn, block_id, parent_id);

    // Delete the text
    if let Some(external_id) = external_id {
      self.text_operation.delete_text_with_txn(txn, external_id);
    }
    // Delete the block
    self
      .block_operation
      .delete_block_with_txn(txn, block_id)
      .map(|_| ())
  }

  /// remove the reference of the block from its parent.
  pub fn delete_block_from_parent(
    &self,
    txn: &mut TransactionMut,
    block_id: &str,
    parent_id: &str,
  ) {
    let parent = self.block_operation.get_block_with_txn(txn, parent_id);
    if let Some(parent) = parent {
      let parent_children_id = &parent.children;
      self
        .children_operation
        .delete_child_with_txn(txn, parent_children_id, block_id);
    }
  }

  /// update the block data.
  pub fn update_block_data(
    &self,
    txn: &mut TransactionMut,
    block_id: &str,
    data: HashMap<String, Value>,
  ) -> Result<(), DocumentError> {
    let block = match self.block_operation.get_block_with_txn(txn, block_id) {
      Some(block) => block,
      None => return Err(DocumentError::BlockIsNotFound),
    };
    self.block_operation.set_block_with_txn(
      txn,
      &block.id,
      Some(data),
      None,
      block.external_id,
      block.external_type,
    )
  }

  /// move the block to the new parent.
  pub fn move_block(
    &self,
    txn: &mut TransactionMut,
    block_id: &str,
    parent_id: Option<String>,
    prev_id: Option<String>,
  ) -> Result<(), DocumentError> {
    // If the parent is not found, return an error.
    let new_parent = match parent_id {
      Some(parent_id) => match self.block_operation.get_block_with_txn(txn, &parent_id) {
        Some(parent) => parent,
        None => return Err(DocumentError::ParentIsNotFound),
      },
      None => return Err(DocumentError::ParentIsNotFound),
    };

    let block = match self.block_operation.get_block_with_txn(txn, block_id) {
      Some(block) => block,
      None => return Err(DocumentError::BlockIsNotFound),
    };

    // If the old parent is not found, return an error.
    let old_parent = match self.block_operation.get_block_with_txn(txn, &block.parent) {
      Some(parent) => parent,
      None => return Err(DocumentError::ParentIsNotFound),
    };

    let new_parent_children_id = new_parent.children;
    let old_parent_children_id = old_parent.children;

    // It must delete the block from the old parent.
    // And this operation must be done before insert the block to the new parent,
    // because the block may be moved to the same parent.
    self
      .children_operation
      .delete_child_with_txn(txn, &old_parent_children_id, block_id);

    // If the prev_id is not found, insert the block to the first position.
    // so the default index is 0.
    let index = prev_id
      .and_then(|prev_id| {
        self
          .children_operation
          .get_child_index_with_txn(txn, &new_parent_children_id, &prev_id)
      })
      .map(|prev_index| prev_index + 1)
      .unwrap_or(0);

    // Insert the block to the new parent.
    self
      .children_operation
      .insert_child_with_txn(txn, &new_parent_children_id, block_id, index);

    // Update the parent of the block.
    self.block_operation.set_block_with_txn(
      txn,
      block_id,
      Some(block.data),
      Some(&new_parent.id),
      None,
      None,
    )
  }

  pub fn redo(&self) -> bool {
    if !self.can_redo() {
      return false;
    }
    if let Some(mut collab_guard) = self.inner.try_lock() {
      collab_guard.redo().unwrap_or_default()
    } else {
      false
    }
  }

  pub fn undo(&self) -> bool {
    if !self.can_undo() {
      return false;
    }
    if let Some(mut collab_guard) = self.inner.try_lock() {
      collab_guard.undo().unwrap_or_default()
    } else {
      false
    }
  }

  pub fn can_redo(&self) -> bool {
    if let Some(collab_guard) = self.inner.try_lock() {
      collab_guard.can_redo()
    } else {
      false
    }
  }

  pub fn can_undo(&self) -> bool {
    if let Some(collab_guard) = self.inner.try_lock() {
      collab_guard.can_undo()
    } else {
      false
    }
  }

  fn create_document(
    collab: Arc<MutexCollab>,
    data: Option<DocumentData>,
  ) -> Result<Self, DocumentError> {
    let mut collab_guard = collab.lock();
    let (root, block_operation, children_operation, text_operation) = collab_guard
      .with_origin_transact_mut(|txn| {
        // { document: {:} }
        let root = collab_guard.insert_map_with_txn(txn, ROOT);
        // { document: { blocks: {:} } }
        let blocks = root.create_map_with_txn(txn, BLOCKS);
        // { document: { blocks: {:}, meta: {:} } }
        let meta = root.create_map_with_txn(txn, META);
        // {document: { blocks: {:}, meta: { children_map: {:} } }
        let children_map = meta.create_map_with_txn(txn, CHILDREN_MAP);
        // { document: { blocks: {:}, meta: { text_map: {:} } }
        let text_map = meta.create_map_with_txn(txn, TEXT_MAP);

        let children_operation = ChildrenOperation::new(children_map);
        let text_operation = TextOperation::new(text_map);
        let block_operation = BlockOperation::new(blocks, children_operation.clone());

        // If the data is not None, insert the data to the document.
        if let Some(data) = data {
          root.insert_with_txn(txn, PAGE_ID, data.page_id);

          for (_, block) in data.blocks {
            block_operation.create_block_with_txn(txn, block)?;
          }

          for (id, child_ids) in data.meta.children_map {
            let map = children_operation.get_children_with_txn(txn, &id);
            child_ids.iter().for_each(|child_id| {
              map.push_back(txn, child_id.to_string());
            });
          }
          if let Some(text_map) = data.meta.text_map {
            for (id, delta) in text_map {
              let delta = serde_json::from_str(&delta).unwrap_or_else(|_| vec![]);
              text_operation.apply_delta_with_txn(txn, &id, delta)
            }
          }
        }

        Ok::<_, DocumentError>((root, block_operation, children_operation, text_operation))
      })?;

    collab_guard.enable_undo_redo();
    let subscription = RootDeepSubscription::default();

    drop(collab_guard);

    let document = Self {
      inner: collab,
      root,
      block_operation,
      children_operation,
      text_operation,
      subscription,
    };
    Ok(document)
  }

  fn open_document_with_collab(collab: Arc<MutexCollab>) -> Result<Self, DocumentError> {
    let mut collab_guard = collab.lock();
    let (root, block_operation, children_operation, text_operation, subscription) = collab_guard
      .with_origin_transact_mut(|txn| {
        let root = collab_guard.get_map_with_txn(txn, vec![ROOT]);

        if root.is_none() {
          return (None, None, None, None, None);
        }
        let root = root.unwrap();
        let blocks = root.create_map_with_txn_if_not_exist(txn, BLOCKS);
        let meta = root.create_map_with_txn_if_not_exist(txn, META);

        let children_map = meta.create_map_with_txn_if_not_exist(txn, CHILDREN_MAP);
        let text_map = meta.create_map_with_txn_if_not_exist(txn, TEXT_MAP);
        let children_operation = ChildrenOperation::new(children_map);
        let text_operation = TextOperation::new(text_map);
        let block_operation = BlockOperation::new(blocks, children_operation.clone());
        let subscription = RootDeepSubscription::default();
        (
          Some(root),
          Some(block_operation),
          Some(children_operation),
          Some(text_operation),
          Some(subscription),
        )
      });

    collab_guard.enable_undo_redo();
    drop(collab_guard);

    if root.is_none() {
      return Err(DocumentError::Internal(anyhow::anyhow!(
        "Unexpected empty document value"
      )));
    }

    if block_operation.is_none() {
      return Err(DocumentError::BlockIsNotFound);
    }

    if children_operation.is_none() {
      return Err(DocumentError::Internal(anyhow::anyhow!(
        "Unexpected empty child map"
      )));
    }

    if text_operation.is_none() {
      return Err(DocumentError::Internal(anyhow::anyhow!(
        "Unexpected empty text map"
      )));
    }

    if subscription.is_none() {
      return Err(DocumentError::Internal(anyhow::anyhow!(
        "Unexpected empty subscription"
      )));
    }
    Ok(Self {
      inner: collab,
      root: root.unwrap(),
      block_operation: block_operation.unwrap(),
      children_operation: children_operation.unwrap(),
      text_operation: text_operation.unwrap(),
      subscription: subscription.unwrap(),
    })
  }

  fn handle_insert_action(
    &self,
    txn: &mut TransactionMut,
    payload: BlockActionPayload,
  ) -> Result<(), DocumentError> {
    if let Some(mut block) = payload.block {
      // Check if the block's parent_id is empty, if it is empty, assign the parent_id to the block
      if block.parent.is_empty() && payload.parent_id.is_some() {
        block.parent = payload.parent_id.unwrap();
      }
      self.insert_block(txn, block, payload.prev_id).map(|_| ())
    } else {
      Err(DocumentError::BlockIsNotFound)
    }
  }

  fn handle_update_action(
    &self,
    txn: &mut TransactionMut,
    payload: BlockActionPayload,
  ) -> Result<(), DocumentError> {
    if let Some(block) = payload.block {
      let data = &block.data;
      self.update_block_data(txn, &block.id, data.to_owned())
    } else {
      Err(DocumentError::BlockIsNotFound)
    }
  }

  fn handle_delete_action(
    &self,
    txn: &mut TransactionMut,
    payload: BlockActionPayload,
  ) -> Result<(), DocumentError> {
    if let Some(block) = payload.block {
      self.delete_block(txn, &block.id)
    } else {
      Err(DocumentError::BlockIsNotFound)
    }
  }

  fn handle_move_action(
    &self,
    txn: &mut TransactionMut,
    payload: BlockActionPayload,
  ) -> Result<(), DocumentError> {
    if let Some(block) = payload.block {
      self.move_block(txn, &block.id, payload.parent_id, payload.prev_id)
    } else {
      Err(DocumentError::BlockIsNotFound)
    }
  }

  fn handle_insert_text_action(
    &self,
    txn: &mut TransactionMut,
    payload: BlockActionPayload,
  ) -> Result<(), DocumentError> {
    if let Some(text_id) = payload.text_id {
      if let Some(delta) = payload.delta {
        self.create_text_with_txn(txn, &text_id, delta);
        Ok(())
      } else {
        Err(DocumentError::TextActionParamsError)
      }
    } else {
      Err(DocumentError::TextActionParamsError)
    }
  }

  fn handle_apply_text_delta_action(
    &self,
    txn: &mut TransactionMut,
    payload: BlockActionPayload,
  ) -> Result<(), DocumentError> {
    if let Some(text_id) = payload.text_id {
      if let Some(delta) = payload.delta {
        self.apply_text_delta_with_txn(txn, &text_id, delta);
        Ok(())
      } else {
        Err(DocumentError::TextActionParamsError)
      }
    } else {
      Err(DocumentError::TextActionParamsError)
    }
  }
}
