use anyhow::bail;
use collab::preclude::{lib0Any, Array, MapRefExtension, MapRefWrapper, ReadTxn, YrsValue};
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::folder::FAVORITES_V1;
use crate::{Folder, View, ViewRelations, Workspace};

const WORKSPACES: &str = "workspaces";
const WORKSPACE_ID: &str = "id";
const WORKSPACE_NAME: &str = "name";
const WORKSPACE_CREATED_AT: &str = "created_at";
impl Folder {
  /// Retrieves historical favorite data from the key `FAVORITES_V1`.
  /// Note: `FAVORITES_V1` is deprecated. Use `FAVORITES_V2` for storing favorite data.
  ///
  /// Returns a `Vec<FavoriteId>` containing the historical favorite data.
  /// The vector will be empty if no historical favorite data exists.
  pub fn get_favorite_v1(&self) -> Vec<FavoriteId> {
    let txn = self.root.transact();
    let mut favorites = vec![];
    if let Some(favorite_array) = self.root.get_array_ref_with_txn(&txn, FAVORITES_V1) {
      for record in favorite_array.iter(&txn) {
        if let Ok(id) = FavoriteId::try_from(&record) {
          favorites.push(id);
        }
      }
    }
    favorites
  }

  pub fn migrate_workspace_to_view(&self) -> Option<()> {
    let mut workspace = {
      let txn = self.root.transact();
      let workspace_array = self.root.get_array_ref_with_txn(&txn, WORKSPACES)?;
      let map_refs = workspace_array.to_map_refs();
      map_refs
        .into_iter()
        .flat_map(|map_ref| to_workspace_with_txn(&txn, &map_ref, &self.views.view_relations))
        .collect::<Vec<_>>()
    };
    if workspace.is_empty() {
      error!("No workspace found. When migrating from v1 to v2, the workspace must be present.");
    } else {
      let workspace = workspace.pop().unwrap();
      self.root.with_transact_mut(|txn| {
        self
          .views
          .insert_view_with_txn(txn, View::from(workspace), None);
      })
    }

    Some(())
  }
}

pub fn to_workspace_with_txn<T: ReadTxn>(
  txn: &T,
  map_ref: &MapRefWrapper,
  views: &ViewRelations,
) -> Option<Workspace> {
  let id = map_ref.get_str_with_txn(txn, WORKSPACE_ID)?;
  let name = map_ref
    .get_str_with_txn(txn, WORKSPACE_NAME)
    .unwrap_or_default();
  let created_at = map_ref
    .get_i64_with_txn(txn, WORKSPACE_CREATED_AT)
    .unwrap_or_default();

  let child_views = views
    .get_children_with_txn(txn, &id)
    .map(|array| array.get_children())
    .unwrap_or_default();

  Some(Workspace {
    id,
    name,
    child_views,
    created_at,
  })
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FavoriteId {
  pub id: String,
}

impl From<lib0Any> for FavoriteId {
  fn from(any: lib0Any) -> Self {
    let mut json = String::new();
    any.to_json(&mut json);
    serde_json::from_str(&json).unwrap()
  }
}

impl From<FavoriteId> for lib0Any {
  fn from(item: FavoriteId) -> Self {
    let json = serde_json::to_string(&item).unwrap();
    lib0Any::from_json(&json).unwrap()
  }
}

impl TryFrom<&YrsValue> for FavoriteId {
  type Error = anyhow::Error;

  fn try_from(value: &YrsValue) -> Result<Self, Self::Error> {
    match value {
      YrsValue::Any(any) => Ok(FavoriteId::from(any.clone())),
      _ => bail!("Invalid favorite yrs value"),
    }
  }
}
