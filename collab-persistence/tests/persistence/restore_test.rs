use std::thread;

use collab_persistence::kv::kv_sled_impl::SledKV;
use collab_persistence::{CollabDB, SledCollabDB};
use yrs::{Doc, GetString, Text, Transact};

use crate::util::db;

#[test]
fn single_thread_test() {
  let (path, db) = db();
  for i in 0..100 {
    let oid = format!("doc_{}", i);
    let doc = Doc::new();
    {
      let txn = doc.transact();
      db.doc(1).create_new_doc(&oid, &txn).unwrap();
    }
    {
      let text = doc.get_or_insert_text("text");
      let mut txn = doc.transact_mut();
      text.insert(&mut txn, 0, &format!("Hello, world! {}", i));
      let update = txn.encode_update_v1();
      db.doc(1).push_update(&oid, &update).unwrap();
    }
  }
  drop(db);

  let db = SledCollabDB::open(path).unwrap();
  for i in 0..100 {
    let oid = format!("doc_{}", i);
    let doc = Doc::new();
    {
      let mut txn = doc.transact_mut();
      db.doc(1).load_doc(&oid, &mut txn).unwrap();
    }
    let text = doc.get_or_insert_text("text");
    let txn = doc.transact();
    assert_eq!(text.get_string(&txn), format!("Hello, world! {}", i));
  }
}

#[test]
fn multiple_thread_test() {
  let (path, db) = db();
  let mut handles = vec![];
  for i in 0..100 {
    let cloned_db = db.clone();
    let handle = thread::spawn(move || {
      let oid = format!("doc_{}", i);
      let doc = Doc::new();
      {
        let txn = doc.transact();
        cloned_db.doc(1).create_new_doc(&oid, &txn).unwrap();
      }
      {
        let text = doc.get_or_insert_text("text");
        let mut txn = doc.transact_mut();
        text.insert(&mut txn, 0, &format!("Hello, world! {}", i));
        let update = txn.encode_update_v1();
        cloned_db.doc(1).push_update(&oid, &update).unwrap();
      }
    });
    handles.push(handle);
  }

  for handle in handles {
    handle.join().unwrap();
  }
  drop(db);

  let db = SledCollabDB::open(path).unwrap();
  for i in 0..100 {
    let oid = format!("doc_{}", i);
    let doc = Doc::new();
    {
      let mut txn = doc.transact_mut();
      db.doc(1).load_doc(&oid, &mut txn).unwrap();
    }
    let text = doc.get_or_insert_text("text");
    let txn = doc.transact();
    assert_eq!(text.get_string(&txn), format!("Hello, world! {}", i));
  }
}
