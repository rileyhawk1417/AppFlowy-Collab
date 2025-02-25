use std::sync::Arc;
use std::time::Duration;

use collab::core::origin::CollabOrigin;
use collab::core::transaction::TransactionRetry;
use collab::error::CollabError;
use collab::preclude::{Collab, MapRefWrapper};
use yrs::{Map, Observable};

use crate::helper::{setup_log, Person, Position};

#[tokio::test]
async fn insert_text() {
  let mut collab = Collab::new(1, "1", "1", vec![]);
  let _sub = collab.observer_data(|txn, event| {
    event.target().iter(txn).for_each(|(a, b)| {
      println!("{}: {}", a, b);
    });
  });

  collab.insert("text", "hello world");
  let value = collab.get("text").unwrap();
  let s = value.to_string(&collab.transact());
  assert_eq!(s, "hello world".to_string());
}

#[tokio::test]
async fn insert_json_attrs() {
  let mut collab = Collab::new(1, "1", "1", vec![]);
  let object = Person {
    name: "nathan".to_string(),
    position: Position {
      title: "develop".to_string(),
      level: 3,
    },
  };
  collab.insert_json_with_path(vec![], "person", object);
  println!("{}", collab);

  let person = collab
    .get_json_with_path::<Person>(vec!["person".to_string()])
    .unwrap();

  println!("{:?}", person);

  let pos = collab
    .get_json_with_path::<Position>(vec!["person".to_string(), "position".to_string()])
    .unwrap();
  println!("{:?}", pos);
}

#[tokio::test]
async fn observer_attr_mut() {
  let mut collab = Collab::new(1, "1", "1", vec![]);
  let object = Person {
    name: "nathan".to_string(),
    position: Position {
      title: "developer".to_string(),
      level: 3,
    },
  };
  collab.insert_json_with_path(vec![], "person", object);
  let _sub = collab
    .get_map_with_path::<MapRefWrapper>(vec!["person".to_string(), "position".to_string()])
    .unwrap()
    .observe(|txn, event| {
      event.target().iter(txn).for_each(|(a, b)| {
        println!("{}: {}", a, b);
      });
    });

  let map = collab
    .get_map_with_path::<MapRefWrapper>(vec!["person".to_string(), "position".to_string()])
    .unwrap();

  map.insert("title", "manager");
}

#[tokio::test]
async fn remove_value() {
  let mut collab = Collab::new(1, "1", "1", vec![]);
  let object = Person {
    name: "nathan".to_string(),
    position: Position {
      title: "developer".to_string(),
      level: 3,
    },
  };
  collab.insert_json_with_path(vec![], "person", object);
  let map =
    collab.get_map_with_path::<MapRefWrapper>(vec!["person".to_string(), "position".to_string()]);
  assert!(map.is_some());

  collab.remove_with_path(vec!["person".to_string(), "position".to_string()]);

  let map =
    collab.get_map_with_path::<MapRefWrapper>(vec!["person".to_string(), "position".to_string()]);
  assert!(map.is_none());
}

#[tokio::test]
async fn retry_write_txn_success_test() {
  setup_log();
  let collab = Arc::new(Collab::new(1, "1", "1", vec![]));
  let doc = collab.get_doc().clone();
  let txn = TransactionRetry::new(&doc).get_write_txn_with(CollabOrigin::Empty);

  let doc = collab.get_doc().clone();
  let result = tokio::task::spawn_blocking(move || {
    let _txn = TransactionRetry::new(&doc).try_get_write_txn_with(CollabOrigin::Empty)?;
    Ok::<(), CollabError>(())
  });

  tokio::time::sleep(Duration::from_secs(1)).await;
  drop(txn);

  let result = result.await.unwrap();
  assert!(result.is_ok());

  tokio::time::sleep(Duration::from_secs(2)).await;
}

#[tokio::test]
#[should_panic]
async fn retry_write_txn_fail_test() {
  setup_log();
  let collab = Arc::new(Collab::new(1, "1", "1", vec![]));
  let doc = collab.get_doc().clone();
  let _txn = TransactionRetry::new(&doc).get_write_txn_with(CollabOrigin::Empty);

  let doc = collab.get_doc().clone();
  let result = tokio::task::spawn_blocking(move || {
    let _txn = TransactionRetry::new(&doc).try_get_write_txn_with(CollabOrigin::Empty)?;

    Ok::<(), CollabError>(())
  });

  tokio::time::sleep(Duration::from_secs(1)).await;
  let result = result.await.unwrap();
  assert!(result.is_ok());
  tokio::time::sleep(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn undo_single_insert_text() {
  let mut collab = Collab::new(1, "1", "1", vec![]);
  collab.enable_undo_redo();
  collab.insert("text", "hello world");

  assert_json_diff::assert_json_eq!(
    collab.to_json(),
    serde_json::json!({
      "text": "hello world"
    }),
  );

  // Undo the insert operation
  assert!(collab.can_undo());
  collab.undo().unwrap();

  // The text should be empty
  assert_json_diff::assert_json_eq!(collab.to_json(), serde_json::json!({}),);
}

#[tokio::test]
async fn redo_single_insert_text() {
  let mut collab = Collab::new(1, "1", "1", vec![]);
  collab.enable_undo_redo();
  collab.insert("text", "hello world");

  // Undo the insert operation
  assert!(collab.can_undo());
  assert!(!collab.can_redo());

  collab.undo().unwrap();
  assert!(collab.can_redo());
  collab.redo().unwrap();

  assert_json_diff::assert_json_eq!(
    collab.to_json(),
    serde_json::json!({
      "text": "hello world"
    }),
  );
}

#[tokio::test]
#[should_panic]
async fn undo_manager_not_enable_test() {
  let mut collab = Collab::new(1, "1", "1", vec![]);
  collab.insert("text", "hello world");
  collab.undo().unwrap();
}

#[tokio::test]
async fn undo_second_insert_text() {
  let mut collab = Collab::new(1, "1", "1", vec![]);
  collab.insert("1", "a");

  collab.enable_undo_redo();
  collab.insert("2", "b");
  collab.undo().unwrap();

  assert_json_diff::assert_json_eq!(
    collab.to_json(),
    serde_json::json!({
      "1": "a"
    }),
  );

  assert!(!collab.can_undo());
}
